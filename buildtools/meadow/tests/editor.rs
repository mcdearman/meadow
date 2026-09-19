//! What the language server is given: a package, its dependencies, and what can
//! run a macro one of them exports.
//!
//! The server cannot build a package itself -- manifests and module discovery
//! live in this crate -- so it asks through `editor::load_package`. What that
//! hands over is what an editor knows about, and for a long time it left the
//! dependencies out: every name one provided was reported undefined.

use meadow::Options;
use std::path::{Path, PathBuf};

/// A package written out in a directory of its own.
fn package(what: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-editor-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("a scratch package");
    std::fs::write(dir.join("Meadow.toml"), manifest).expect("a manifest");
    for (name, text) in files {
        std::fs::write(dir.join("src").join(name), text).expect("a module");
    }
    dir
}

/// The standard library, as the server holds it.
fn std_lib() -> meadow_lsp::analysis::Std {
    let opts = Options::debug();
    let modules = meadow::stdlib::std_modules(opts)
        .0
        .into_iter()
        .map(|(dotted, pkg)| (dotted.to_string(), pkg))
        .collect();
    meadow_lsp::analysis::Std::new(
        meadow::stdlib::std_packages(opts).0,
        modules,
        None,
        meadow::stdlib::MODULES,
    )
}

/// What the server would report for `file`, loaded the way it loads one.
fn diagnostics(file: &Path) -> Vec<String> {
    let sources = meadow::editor::load_package(file).expect("the file is in a package");
    let text = std::fs::read_to_string(file).expect("the file reads");
    let analysis = std_lib()
        .analyse_package(&sources, file, &text)
        .expect("the file is one of the package's modules");
    analysis.diagnostics.iter().map(|d| d.msg.clone()).collect()
}

#[test]
fn a_packages_dependencies_are_loaded_with_it() {
    let lib = package(
        "dep",
        "[package]\nname = \"Util\"\nversion = \"0.1.0\"\n",
        &[("Lib.mw", "@pub fun double x = x + x\n")],
    );
    let app = package(
        "dependent",
        &format!(
            "[package]\nname = \"App\"\nversion = \"0.1.0\"\n\n[dependencies]\nutil = {{ path = \"{}\" }}\n",
            lib.display()
        ),
        &[("Lib.mw", "use util (double)\n\ndef main = double 21\n")],
    );
    let file = app
        .join("src/Lib.mw")
        .canonicalize()
        .expect("the file is there");
    let sources = meadow::editor::load_package(&file).expect("the file is in a package");
    assert_eq!(
        sources
            .deps
            .iter()
            .map(|(n, _)| n.to_string())
            .collect::<Vec<_>>(),
        vec!["util".to_string()],
        "the dependency comes with the package"
    );
    assert!(diagnostics(&file).is_empty(), "{:?}", diagnostics(&file));
}

#[test]
fn a_name_a_dependency_does_not_have_is_still_undefined() {
    // The point is not that errors go away: it is that the right ones are left.
    let lib = package(
        "dep-partial",
        "[package]\nname = \"Util\"\nversion = \"0.1.0\"\n",
        &[("Lib.mw", "@pub fun double x = x + x\n")],
    );
    let app = package(
        "dependent-partial",
        &format!(
            "[package]\nname = \"App\"\nversion = \"0.1.0\"\n\n[dependencies]\nutil = {{ path = \"{}\" }}\n",
            lib.display()
        ),
        &[("Lib.mw", "use util (double)\n\ndef main = treble 21\n")],
    );
    let file = app
        .join("src/Lib.mw")
        .canonicalize()
        .expect("the file is there");
    let errs = diagnostics(&file);
    assert!(errs.iter().any(|e| e.contains("treble")), "{errs:?}");
}

#[test]
fn a_procedural_macro_runs_for_the_editor() {
    // The server is given something that can run one, so a document using a
    // macro is analysed as what the macro made of it -- not as a file full of
    // undefined names.
    let lib = package(
        "macro-dep",
        "[package]\nname = \"Maker\"\nversion = \"0.1.0\"\n",
        &[(
            "Lib.mw",
            "use Std.Macro.TokenTree.*\n\n@macro\n@pub fun defineOne ts = [Code \"def one = 1\"]\n",
        )],
    );
    let app = package(
        "macro-user",
        &format!(
            "[package]\nname = \"App\"\nversion = \"0.1.0\"\n\n[dependencies]\nmaker = {{ path = \"{}\" }}\n",
            lib.display()
        ),
        &[(
            "Lib.mw",
            "use maker (defineOne!)\n\ndefineOne!()\n\ndef main = one\n",
        )],
    );
    let file = app
        .join("src/Lib.mw")
        .canonicalize()
        .expect("the file is there");
    assert!(diagnostics(&file).is_empty(), "{:?}", diagnostics(&file));
}
