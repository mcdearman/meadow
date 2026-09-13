//! The unit of compilation is a **package**: a directory of modules plus a list of
//! dependency packages. Modules inside one package may refer to each other freely
//! (mutual recursion is fine); packages form a DAG and may not — a dependency cycle
//! is a hard error, exactly like cargo crates.
//!
//! Discovery is filesystem-based. A package root optionally carries a
//! `meadow.toml` manifest, in a Cargo-like format:
//!
//! ```toml
//! [package]
//! name = "demo"
//! version = "0.1.0"
//!
//! [dependencies]
//! util = "../util"                 # bare string = path
//! shared = { path = "../shared" }  # inline table with `path`
//! ```
//!
//! `[package]` name/version may also be given at the top level, and the legacy
//! file name `meadow.pkg` is still accepted. Without a manifest the directory (or
//! single `.mw` file) is a standalone package named after its stem. The embedded
//! `Std` package (see [`crate::stdlib`]) is always an implicit dependency and
//! never needs to be listed.
//!
//! A manifest may also configure the build profiles, which is how a package
//! changes what `--debug` and `--release` mean for it:
//!
//! ```toml
//! [profile.debug]
//! opt-level = 1            # 0, 1, 2 — or "O1"
//!
//! [profile.release]
//! opt-level = 2
//! strictness = "strict"    # "lenient" | "strict"
//! ```
//!
//! Only the keys that are present are overridden; the rest keep the profile's
//! built-in meaning (see [`crate::Profile`]). A command-line flag wins over
//! both.

use meadow_compiler::{
    diagnostics::Diagnostic,
    intern::InternedString,
    source::{Source, SourceKind},
    OptLevel, Options, Strictness,
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
    /// `[profile.<name>]` sections, keyed by profile name.
    pub profiles: HashMap<String, ProfileConfig>,
}

/// What one `[profile.<name>]` section says.
///
/// Every field is optional, and absent means "whatever that profile already
/// meant" — a manifest that only wants a different optimization level in debug
/// builds says exactly that and inherits the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileConfig {
    pub opt: Option<OptLevel>,
    pub strictness: Option<Strictness>,
}

impl ProfileConfig {
    /// This section applied on top of `base`.
    pub fn apply(self, base: Options) -> Options {
        Options {
            opt: self.opt.unwrap_or(base.opt),
            strictness: self.strictness.unwrap_or(base.strictness),
            debug_info: base.debug_info,
        }
    }
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

        let modules = discover_modules(&canon, name)?;
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

/// Manifest file names, in precedence order.
const MANIFEST_NAMES: &[&str] = &["meadow.toml", "meadow.pkg"];

impl Manifest {
    pub fn load(dir: &Path) -> std::io::Result<Option<Manifest>> {
        let Some(file) = MANIFEST_NAMES
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
        else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&file)?;
        Ok(Some(parse_manifest(&text, dir)))
    }

    /// The manifest governing `path`, which may be a package directory or a
    /// single `.mw` file inside one.
    ///
    /// Unreadable or missing is not an error here: a package need not have a
    /// manifest at all, and a build should not be stopped by one it could not
    /// read when everything it says is optional anyway.
    pub fn find(path: &Path) -> Option<Manifest> {
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()?.to_path_buf()
        };
        Manifest::load(&dir).ok().flatten()
    }

    /// The `[profile.<name>]` section, or an empty one.
    pub fn profile(&self, name: &str) -> ProfileConfig {
        self.profiles.get(name).copied().unwrap_or_default()
    }
}

/// The package directory a file belongs to: the nearest ancestor holding a
/// `meadow.toml`, or — for a package that has no manifest — the parent of the
/// `src` directory it sits under.
///
/// `None` when the file is not in a package at all, which is an ordinary thing
/// for an editor to be shown: a scratch file, or a `.mw` opened on its own.
pub fn enclosing_root(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join("meadow.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        // No manifest anywhere above: `src/` is the other thing that marks a
        // package root, and it is what `discover_modules` looks for.
        if dir.file_name().and_then(|n| n.to_str()) == Some("src")
            && dir.parent().is_some_and(|p| p.join("src").is_dir())
        {
            return dir.parent().map(Path::to_path_buf);
        }
        dir = dir.parent()?;
    }
}

/// A deliberately small line-based TOML reader — enough for `[package]` /
/// `[dependencies]` with string or `{ path = "…" }` values.
fn parse_manifest(text: &str, dir: &Path) -> Manifest {
    let mut name = package_name(dir).to_string();
    let mut version = "0.0.0".to_string();
    let mut deps = Vec::new();
    let mut profiles: HashMap<String, ProfileConfig> = HashMap::new();
    let mut section = String::new();

    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = inner.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match section.as_str() {
            "dependencies" => {
                if let Some(path) = dep_path(value) {
                    deps.push((key.to_string(), PathBuf::from(path)));
                }
            }
            "package" | "" => match key {
                "name" => name = unquote(value).to_string(),
                "version" => version = unquote(value).to_string(),
                _ => {}
            },
            // `[profile.release]`, `[profile.debug]`, or any other name a
            // driver might come to know. An unknown key is ignored rather than
            // rejected: a manifest written for a later version of the compiler
            // should still build.
            _ if section.starts_with("profile.") => {
                let p = profiles.entry(section["profile.".len()..].to_string()).or_default();
                match key {
                    "opt-level" | "opt_level" => p.opt = OptLevel::parse(unquote(value)),
                    "strictness" => p.strictness = Strictness::parse(unquote(value)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Manifest {
        name,
        version,
        deps,
        profiles,
    }
}

fn strip_comment(line: &str) -> &str {
    // `#` outside of a quoted string starts a comment.
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(v: &str) -> &str {
    v.trim().trim_matches('"')
}

/// A dependency value is either `"path"` or `{ path = "path" }`.
fn dep_path(value: &str) -> Option<&str> {
    let v = value.trim();
    if let Some(inner) = v.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        inner
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| k.trim() == "path")
            .map(|(_, p)| unquote(p))
    } else {
        Some(unquote(v))
    }
}

fn discover_modules(root: &Path, pkg_name: InternedString) -> Result<Vec<ModuleSource>, Diagnostic> {
    discover_modules_io(root, pkg_name).map_err(|e| match e {
        Discovery::Io(e) => io_diag(root, e),
        Discovery::Misnamed(d) => d,
    })
}

enum Discovery {
    Io(std::io::Error),
    Misnamed(Diagnostic),
}

impl From<std::io::Error> for Discovery {
    fn from(e: std::io::Error) -> Self {
        Discovery::Io(e)
    }
}

fn discover_modules_io(root: &Path, pkg_name: InternedString) -> Result<Vec<ModuleSource>, Discovery> {
    // A single `.mw` file is a one-module package. Its name is exempt from the
    // PascalCase rule below: it is the *package's* name, and a package may be
    // lower-case (`meadow init` makes `app`), whereas a module in a package is a
    // name a `use` has to write.
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
    check_module_names(&src_root, &files).map_err(Discovery::Misnamed)?;

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
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("Mod");
        if !ROOT_STEMS.contains(&stem) {
            segs.push(InternedString::from(stem));
        }
        let name = segs
            .last()
            .copied()
            .unwrap_or(pkg_name);
        modules.push(module_from_file(&file, &segs, name)?);
    }

    if modules.is_empty() {
        return Err(Discovery::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no .mw modules under {}", src_root.display()),
        )));
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

/// A path as the filesystem itself would name it — symlinks and `..` resolved —
/// so that two ways of naming one file compare equal. An editor's URI and a
/// discovered module have to meet somewhere, and this is where.
pub(crate) fn canonical(path: &Path) -> PathBuf {
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

/// File stems that make a module the package's *root* rather than a module of
/// its own name.
const ROOT_STEMS: &[&str] = &["Main", "Lib", "Mod"];

/// Is `s` a module name — PascalCase, as a `use` path segment must be?
///
/// The rule is the lexer's for an upper-case identifier, less the `'` it also
/// allows, which has no business in a file name.
pub fn is_module_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| c.is_ascii_alphanumeric())
}

/// The PascalCase spelling of a name, for suggesting a rename: `main` -> `Main`,
/// `p5` -> `P5`, `my-mod` and `my_mod` -> `MyMod`.
fn pascal_case(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Every module file, and every directory between it and the source root, has to
/// be PascalCase.
///
/// Both are the same thing to the language: `src/Collections/Vector.mw` is the
/// module `Collections.Vector`, and each segment is a name someone writes in a
/// `use`. A lower-case file used to be accepted and even meant something —
/// `main.mw` was the root — so the message names every offender and what to
/// call it, rather than the first one found.
fn check_module_names(src_root: &Path, files: &[PathBuf]) -> Result<(), Diagnostic> {
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    for file in files {
        let rel = file.strip_prefix(src_root).unwrap_or(file);
        let mut fixed = PathBuf::new();
        let mut bad = false;
        let parts: Vec<_> = rel.components().filter_map(|c| c.as_os_str().to_str()).collect();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let name = if last { part.strip_suffix(".mw").unwrap_or(part) } else { part };
            let good = if is_module_name(name) { name.to_string() } else {
                bad = true;
                pascal_case(name)
            };
            fixed.push(if last { format!("{good}.mw") } else { good });
        }
        if bad {
            renames.push((rel.to_path_buf(), fixed));
        }
    }
    if renames.is_empty() {
        return Ok(());
    }

    // A directory shared by several files would otherwise be reported once per
    // file; the files are what get renamed, so that is fine, but say it once.
    renames.dedup();
    let list = renames
        .iter()
        .map(|(from, to)| format!("`{}` to `{}`", from.display(), to.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let (first, _) = &renames[0];
    Err(Diagnostic {
        msg: format!(
            "module files and directories must be PascalCase, since each is a \
             name in a `use` path — rename {list}"
        ),
        filename: src_root.join(first).display().to_string(),
        label: ("not PascalCase".to_string(), Default::default()),
        extra_labels: vec![],
    })
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    #[test]
    fn a_module_name_is_pascal_case() {
        for good in ["Main", "Lib", "Vector", "P5", "HttpServer"] {
            assert!(is_module_name(good), "{good}");
        }
        for bad in ["main", "prelude", "p5", "my-mod", "My_Mod", "", "5p", "Don't"] {
            assert!(!is_module_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_suggested_name_is_what_someone_would_have_meant() {
        assert_eq!(pascal_case("main"), "Main");
        assert_eq!(pascal_case("p5"), "P5");
        assert_eq!(pascal_case("my-mod"), "MyMod");
        assert_eq!(pascal_case("my_mod"), "MyMod");
        assert_eq!(pascal_case("httpServer"), "HttpServer");
    }
}
