//! `meadow init` — what it writes, and what it refuses to.
//!
//! The interesting assertion is not that two files appear. It is that what
//! appears is a package the *rest* of the tools accept: the manifest parses
//! back through `package::Manifest`, the source builds, and the name is one
//! that could appear in a `use` path. A template that produced something the
//! compiler then rejected would be worse than no template.

use meadow::{init, package::Manifest, pipeline, Options};
use std::path::{Path, PathBuf};

/// A directory of our own, named after the test so concurrent ones do not
/// collide. Removed first, so a previous failed run cannot make this one pass.
fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-init-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn init_at(path: &Path, name: Option<&str>) -> Result<init::Created, String> {
    init::run(&init::Options {
        path: path.to_path_buf(),
        name: name.map(|s| s.to_string()),
    })
}

/// The whole point: what is written is a package that builds and runs.
#[test]
fn what_init_writes_is_a_package_that_builds() {
    let dir = scratch("builds");
    let made = init_at(&dir.join("demo"), None).expect("init");
    assert_eq!(made.name, "demo", "the name comes from the directory");

    // It parses back as a manifest, with the name written down rather than
    // inferred — which is the thing a bare directory of sources would not have.
    let m = Manifest::load(&made.root)
        .expect("readable")
        .expect("a manifest");
    assert_eq!(m.name, "demo");
    assert_eq!(m.version, "0.1.0");
    assert!(m.deps.is_empty());

    // And the source compiles clean against the real pipeline.
    let out = pipeline::build(&made.root, Options::debug());
    assert!(
        out.diagnostics.is_empty(),
        "the generated package should build clean: {:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    assert!(out.linked.is_some(), "and it should have an entry point");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_directory_is_created_if_it_is_missing() {
    let dir = scratch("mkdir");
    let target = dir.join("deep").join("nested");
    let made = init_at(&target, None).expect("init");
    assert_eq!(made.name, "nested");
    assert!(target.join("meadow.toml").is_file());
    assert!(target.join("src").join("main.mw").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_name_can_be_given_when_the_directory_is_not_one() {
    let dir = scratch("named");
    let made = init_at(&dir.join("my-app"), Some("myApp")).expect("init");
    assert_eq!(made.name, "myApp");
    assert_eq!(Manifest::load(&made.root).unwrap().unwrap().name, "myApp");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A manifest is what says "this is already a package", so overwriting one to
/// put a template there is the one thing this must not do by accident.
#[test]
fn an_existing_package_is_refused_rather_than_overwritten() {
    let dir = scratch("refuse");
    let target = dir.join("twice");
    init_at(&target, None).expect("the first one");

    // Something worth losing, in the file that would be overwritten.
    let manifest = target.join("meadow.toml");
    let before = std::fs::read_to_string(&manifest).unwrap() + "\n[dependencies]\nutil = \"../u\"\n";
    std::fs::write(&manifest, &before).unwrap();

    let err = init_at(&target, None).expect_err("the second one");
    assert!(err.contains("already"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        before,
        "the manifest must be left exactly as it was"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run in a directory that already has sources, `init` supplies the manifest
/// and leaves the code alone.
#[test]
fn existing_sources_are_not_replaced() {
    let dir = scratch("existing");
    let target = dir.join("has-code");
    std::fs::create_dir_all(target.join("src")).unwrap();
    let main = target.join("src").join("main.mw");
    std::fs::write(&main, "def main = 7\n").unwrap();

    init_at(&target, Some("hasCode")).expect("init");
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "def main = 7\n",
        "the existing `main` should survive"
    );
    assert!(target.join("meadow.toml").is_file(), "but the manifest arrives");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The name has to lex as one identifier, because it is the first segment of a
/// `use` path. A hyphen is what every other ecosystem spells a multi-word
/// package with, so it is what someone will type — and it cannot work here.
#[test]
fn a_name_the_language_could_not_refer_to_is_refused() {
    let dir = scratch("badname");
    let err = init_at(&dir.join("my-pkg"), None).expect_err("hyphen");
    assert!(err.contains("my-pkg"), "{err}");
    assert!(err.contains("try `_`"), "the message should say what to do: {err}");
    assert!(
        !dir.join("my-pkg").join("meadow.toml").exists(),
        "nothing should be written when the name is refused"
    );

    for bad in ["2fast", "a b", "My_App"] {
        assert!(
            init_at(&dir.join("z"), Some(bad)).is_err(),
            "{bad} should be refused"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A generated package can be depended on *by the name `init` wrote* — which
/// is what the name rule exists to guarantee.
#[test]
fn a_generated_package_can_be_used_as_a_dependency() {
    let dir = scratch("dep");
    let util = dir.join("util");
    let app = dir.join("app");
    init_at(&util, None).expect("util");
    init_at(&app, None).expect("app");

    std::fs::write(
        util.join("src").join("main.mw"),
        "@pub(pack) fun double x = x * 2\n",
    )
    .unwrap();
    std::fs::write(
        app.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nutil = { path = \"../util\" }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src").join("main.mw"),
        "use util (double)\n\ndef main = double 21\n",
    )
    .unwrap();

    let out = pipeline::build(&app, Options::debug());
    assert!(
        out.diagnostics.is_empty(),
        "{:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = out.linked.expect("linked");
    assert_eq!(meadow_eval::run(&linked.program).unwrap().to_string(), "42");
    let _ = std::fs::remove_dir_all(&dir);
}
