//! What the language server needs from the build system.
//!
//! `meadow-lsp` answers an editor; this crate knows how packages are laid out
//! on disk. The arrow between them points one way — this crate depends on the
//! server for its `lsp` subcommand — so the server asks for a package through a
//! function pointer, and this module is what it is given.

use std::path::PathBuf;

/// Find the package a file belongs to, for the language server.
///
/// The server cannot do this itself: manifest parsing and module discovery live
/// here, and `meadow-lsp` is a dependency of this crate rather than the other
/// way round. So it asks through a function pointer, and this is the function.
///
/// Only the package's own modules are handed over. Its *dependencies* are not
/// built here — that is a full compile per keystroke — so a document is still
/// analysed against the standard library alone, plus the siblings that make its
/// own `use` lines resolve.
pub fn load_package(file: &std::path::Path) -> Option<meadow_lsp::analysis::PackageSources> {
    find_package(file)?.ok()
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
    Some(Ok(meadow_lsp::analysis::PackageSources {
        name: pkg.name,
        modules,
    }))
}
