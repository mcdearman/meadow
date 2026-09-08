//! Multi-package builds: `meadow.toml` manifests, a local path dependency, and
//! `@pub` gating between packages.

use meadow::{package::Manifest, pipeline};
use meadow_eval as eval;
use std::path::Path;

const WORKSPACE: &str = "tests/fixtures/workspace";

#[test]
fn manifest_parses_cargo_style() {
    let m = Manifest::load(Path::new(&format!("{WORKSPACE}/app")))
        .unwrap()
        .expect("app has a manifest");
    assert_eq!(m.name, "app");
    assert_eq!(m.version, "0.1.0");
    assert_eq!(m.deps.len(), 1);
    assert_eq!(m.deps[0].0, "util");
    assert_eq!(m.deps[0].1, Path::new("../util"));
}

#[test]
fn builds_app_against_a_path_dependency() {
    let out = pipeline::build(Path::new(&format!("{WORKSPACE}/app")), meadow::Options::debug());
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = out.linked.expect("linked program");
    // [1..6] -> double -> *3 -> sum  ==  (2+4+6+8+10)*3 == 90
    assert_eq!(eval::run(&linked.program).unwrap().to_string(), "90");
}

#[test]
fn private_names_do_not_cross_package_boundaries() {
    // `util` exports `double` / `scale` (both `@pub`) but not the plain `secret`.
    let out = pipeline::build(Path::new(&format!("{WORKSPACE}/util")), meadow::Options::debug());
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let linked = out.linked.unwrap();
    let mut names: Vec<_> = linked
        .symbols
        .iter()
        .filter(|s| &*s.package == "util")
        .map(|s| s.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["double", "scale"]);
}

#[test]
fn without_pub_everything_is_still_exported() {
    // Back-compat: a unit with no `@pub` anywhere exports every top-level binding.
    let (cp, _) = pipeline::compile_str("m", "fun a x = x\nfun b y = y\ndef c = 1\n");
    let mut names: Vec<_> = cp.exports.iter().map(|e| e.name.to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["a", "b", "c"]);
}
