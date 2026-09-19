//! What the language server needs from the build system.
//!
//! `meadow-lsp` answers an editor; this crate knows how packages are laid out
//! on disk. The arrow between them points one way — this crate depends on the
//! server for its `lsp` subcommand — so the server asks for a package through a
//! function pointer, and this module is what it is given.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Find the package a file belongs to, for the language server.
///
/// The server cannot do this itself: manifest parsing and module discovery live
/// here, and `meadow-lsp` is a dependency of this crate rather than the other
/// way round. So it asks through a function pointer, and this is the function.
///
/// Its **dependencies are built too**, and that is the part worth explaining.
/// Doing it on every keystroke would be a full build per keystroke, so what a
/// package's dependencies compiled to is kept, and reused until one of their
/// sources changes. Without them, every name a dependency provides is
/// undefined -- which is what an editor used to show for any package that had
/// one.
pub fn load_package(file: &std::path::Path) -> Option<meadow_lsp::analysis::PackageSources> {
    find_package(file)?.ok()
}

/// What a package's dependencies came to, and what can run a macro one of them
/// exports.
struct Built {
    deps: Vec<(
        meadow_compiler::intern::InternedString,
        Rc<meadow_compiler::CompiledPackage>,
    )>,
    procs: Rc<dyn meadow_compiler::expand::proc::Runner>,
}

thread_local! {
    /// Per package root: what its dependencies were last built to, and the
    /// digest of the sources that produced them. Not a `static`, because a
    /// compiled package is not something to share between threads -- and the
    /// server asks from one.
    static BUILT: RefCell<HashMap<PathBuf, (u64, Rc<Built>)>> = RefCell::new(HashMap::new());
}

/// Everything `graph`'s root depends on, compiled -- from memory when nothing
/// they are made of has changed since last time.
fn dependencies(root: &std::path::Path, graph: &crate::package::PackageGraph) -> Rc<Built> {
    let digest = sources_digest(graph);
    if let Some(had) = BUILT.with(|b| {
        b.borrow()
            .get(root)
            .filter(|(d, _)| *d == digest)
            .map(|(_, built)| built.clone())
    }) {
        return had;
    }
    let opts = crate::Options::debug();
    let cache = crate::incremental::Cache::new(root, opts);
    let compiled = crate::pipeline::compile_graph(graph, opts, cache.as_ref());
    let me = graph.root();
    let deps: Vec<_> = graph.packages[me]
        .deps
        .iter()
        .zip(&graph.packages[me].dep_names)
        .filter_map(|(id, alias)| {
            compiled.packages[*id]
                .as_ref()
                .map(|p| (*alias, Rc::new(p.clone())))
        })
        .collect();
    // A macro could live in any of them, or in the standard library they were
    // compiled against.
    let mut held: Vec<meadow_compiler::CompiledPackage> = compiled.std.clone();
    held.extend(compiled.packages.iter().flatten().cloned());
    let built = Rc::new(Built {
        deps,
        procs: Rc::new(crate::proc::Macros::owning(held)),
    });
    BUILT.with(|b| {
        b.borrow_mut()
            .insert(root.to_path_buf(), (digest, built.clone()))
    });
    built
}

/// What every package in `graph` but the root is made of, as one number: the
/// name, length and last change of each of their files. Enough to notice a
/// dependency being edited, and cheap enough to ask on every keystroke.
fn sources_digest(graph: &crate::package::PackageGraph) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let me = graph.root();
    for (id, pkg) in graph.packages.iter().enumerate() {
        if id == me {
            continue;
        }
        for m in &pkg.modules {
            if let meadow_compiler::source::SourceKind::File(name) = m.source.kind {
                let path = name.to_string();
                feed(path.as_bytes());
                if let Ok(meta) = std::fs::metadata(&path) {
                    feed(&meta.len().to_le_bytes());
                    if let Ok(t) = meta.modified()
                        && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
                    {
                        feed(&d.as_nanos().to_le_bytes());
                    }
                }
            }
        }
    }
    h
}

/// [`load_package`], saying why when the file is in a package that cannot be
/// loaded. This is the one the server is given, so the reason reaches the editor.
pub fn find_package(
    file: &std::path::Path,
) -> Option<Result<meadow_lsp::analysis::PackageSources, String>> {
    use meadow_compiler::source::SourceKind;
    let root = crate::package::enclosing_root(file)?;
    let graph = match crate::package::PackageGraph::build(&root) {
        Ok(g) => g,
        Err(d) => return Some(Err(d.msg)),
    };
    let pkg = &graph.packages[graph.root()];
    let modules = pkg
        .modules
        .iter()
        .map(|m| meadow_lsp::analysis::ModuleFile {
            path: m.path.clone(),
            name: m.name,
            // Canonical, because the other side of the comparison is a path
            // an editor chose the spelling of.
            file: match m.source.kind {
                SourceKind::File(name) => {
                    crate::package::canonical(&PathBuf::from(name.to_string()))
                }
                SourceKind::Interactive => PathBuf::new(),
            },
            text: m.source.content.to_string(),
        })
        .collect();
    let built = dependencies(&root, &graph);
    Some(Ok(meadow_lsp::analysis::PackageSources {
        name: pkg.name,
        modules,
        deps: built.deps.clone(),
        procs: Some(built.procs.clone()),
    }))
}
