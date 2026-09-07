//! The unit of compilation is a **package**: a directory of modules plus a list of
//! dependency packages. Modules inside one package may refer to each other freely
//! (mutual recursion is fine); packages form a DAG and may not — a dependency cycle
//! is a hard error, exactly like cargo crates.
//!
//! Discovery is filesystem-based. A package root optionally carries a `meadow.pkg`
//! manifest:
//!
//! ```text
//! name = "demo"
//! version = "0.1.0"
//!
//! [dependencies]
//! std = "../../lib/std"
//! ```
//!
//! Without a manifest the directory (or single `.mw` file) is treated as a
//! standalone package named after its stem.

use crate::{
    diagnostics::Diagnostic,
    intern::InternedString,
    source::{Source, SourceKind},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type PackageId = usize;

#[derive(Debug, Clone)]
pub struct ModuleSource {
    /// Dotted path from the package's source root; empty for the root module.
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub deps: Vec<(String, PathBuf)>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub id: PackageId,
    pub name: InternedString,
    pub root: PathBuf,
    pub modules: Vec<ModuleSource>,
    pub deps: Vec<PackageId>,
}

#[derive(Debug, Clone)]
pub struct PackageGraph {
    pub packages: Vec<Package>,
    /// Topological order: every package appears after all of its dependencies.
    order: Vec<PackageId>,
}

impl PackageGraph {
    pub fn order(&self) -> &[PackageId] {
        &self.order
    }

    pub fn root(&self) -> PackageId {
        *self.order.last().expect("at least one package")
    }

    /// Discover the package rooted at `entry` and everything it depends on.
    pub fn build(entry: &Path) -> Result<PackageGraph, Diagnostic> {
        let mut builder = Builder {
            packages: Vec::new(),
            by_root: HashMap::new(),
            order: Vec::new(),
            stack: Vec::new(),
        };
        builder.visit(entry)?;
        Ok(PackageGraph {
            packages: builder.packages,
            order: builder.order,
        })
    }
}

struct Builder {
    packages: Vec<Package>,
    by_root: HashMap<PathBuf, PackageId>,
    order: Vec<PackageId>,
    /// Roots currently being visited, for cycle reporting.
    stack: Vec<(PathBuf, InternedString)>,
}

impl Builder {
    fn visit(&mut self, path: &Path) -> Result<PackageId, Diagnostic> {
        let canon = canonical(path);
        if let Some(id) = self.by_root.get(&canon) {
            return Ok(*id);
        }
        if let Some(pos) = self.stack.iter().position(|(p, _)| *p == canon) {
            let chain = self
                .stack
                .iter()
                .skip(pos)
                .map(|(_, n)| n.to_string())
                .chain(std::iter::once(package_name(&canon).to_string()))
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(Diagnostic {
                msg: format!("packages cannot be mutually recursive: {chain}"),
                filename: canon.display().to_string(),
                label: ("dependency cycle starts here".to_string(), Default::default()),
                extra_labels: vec![],
            });
        }

        let manifest = Manifest::load(&canon).map_err(|e| io_diag(&canon, e))?;
        let name = manifest
            .as_ref()
            .map(|m| InternedString::from(m.name.as_str()))
            .unwrap_or_else(|| package_name(&canon));

        self.stack.push((canon.clone(), name));

        // resolve dependencies first so `order` ends up topologically sorted
        let mut dep_ids = Vec::new();
        if let Some(m) = &manifest {
            for (dep_name, rel) in &m.deps {
                let dep_path = canon.join(rel);
                if !dep_path.exists() {
                    self.stack.pop();
                    return Err(Diagnostic {
                        msg: format!(
                            "dependency `{dep_name}` of `{name}` not found at {}",
                            dep_path.display()
                        ),
                        filename: canon.display().to_string(),
                        label: ("declared here".to_string(), Default::default()),
                        extra_labels: vec![],
                    });
                }
                dep_ids.push(self.visit(&dep_path)?);
            }
        }

        self.stack.pop();

        let modules = discover_modules(&canon, name).map_err(|e| io_diag(&canon, e))?;
        let id = self.packages.len();
        self.packages.push(Package {
            id,
            name,
            root: canon.clone(),
            modules,
            deps: dep_ids,
        });
        self.by_root.insert(canon, id);
        self.order.push(id);
        Ok(id)
    }
}

impl Manifest {
    pub fn load(dir: &Path) -> std::io::Result<Option<Manifest>> {
        let file = dir.join("meadow.pkg");
        if !file.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&file)?;
        let mut name = package_name(dir).to_string();
        let mut version = "0.0.0".to_string();
        let mut deps = Vec::new();
        let mut in_deps = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_deps = line[1..line.len() - 1].trim() == "dependencies";
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            if in_deps {
                deps.push((key.to_string(), PathBuf::from(value)));
            } else if key == "name" {
                name = value.to_string();
            } else if key == "version" {
                version = value.to_string();
            }
        }
        Ok(Some(Manifest {
            name,
            version,
            deps,
        }))
    }
}

fn discover_modules(root: &Path, pkg_name: InternedString) -> std::io::Result<Vec<ModuleSource>> {
    // A single `.mw` file is a one-module package.
    if root.is_file() {
        return Ok(vec![module_from_file(root, &[], pkg_name)?]);
    }
    let src_root = {
        let s = root.join("src");
        if s.is_dir() { s } else { root.to_path_buf() }
    };
    let mut files = Vec::new();
    collect_mw(&src_root, &mut files)?;
    files.sort();

    let mut modules = Vec::new();
    for file in files {
        let rel = file.strip_prefix(&src_root).unwrap_or(&file);
        let mut segs: Vec<InternedString> = rel
            .parent()
            .map(|p| {
                p.components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .map(InternedString::from)
                    .collect()
            })
            .unwrap_or_default();
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("mod");
        if !matches!(stem, "main" | "lib" | "mod") {
            segs.push(InternedString::from(stem));
        }
        let name = segs
            .last()
            .copied()
            .unwrap_or(pkg_name);
        modules.push(module_from_file(&file, &segs, name)?);
    }

    if modules.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .mw modules under {}", src_root.display()),
        ));
    }
    Ok(modules)
}

fn module_from_file(
    file: &Path,
    segs: &[InternedString],
    name: InternedString,
) -> std::io::Result<ModuleSource> {
    let content = std::fs::read_to_string(file)?;
    let source = Source::new(
        SourceKind::File(InternedString::from(file.display().to_string())),
        InternedString::from(content),
    );
    Ok(ModuleSource {
        path: segs.to_vec(),
        name,
        source,
    })
}

fn collect_mw(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_mw(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("mw") {
            out.push(path);
        }
    }
    Ok(())
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn package_name(root: &Path) -> InternedString {
    let stem = root
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("package");
    InternedString::from(stem)
}

fn io_diag(root: &Path, e: std::io::Error) -> Diagnostic {
    Diagnostic {
        msg: format!("could not read package at {}: {e}", root.display()),
        filename: root.display().to_string(),
        label: ("here".to_string(), Default::default()),
        extra_labels: vec![],
    }
}
