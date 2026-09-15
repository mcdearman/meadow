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
    let out = pipeline::build(
        Path::new(&format!("{WORKSPACE}/app")),
        meadow::Options::debug(),
    );
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
    // `util` exports `double` / `scale` (both `@pub`) but not `secret`.
    let out = pipeline::build(
        Path::new(&format!("{WORKSPACE}/util")),
        meadow::Options::debug(),
    );
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
    // A unit that never mentions visibility exports every top-level binding.
    let (cp, _) = pipeline::compile_str("m", "fun a x = x\nfun b y = y\ndef c = 1\n");
    let mut names: Vec<_> = cp.exports.iter().map(|e| e.name.to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["a", "b", "c"]);
}

// --- module ordering within a package ----------------------------------------

#[test]
fn modules_are_compiled_in_dependency_order() {
    // `layers` has `Alpha.mw` (needs `Zeta`), `Zeta.mw` and `Main.mw`. Modules are
    // discovered in filename order, so `Alpha` comes first and everything it uses
    // comes later — inference and evaluation both have to sort that out.
    let out = pipeline::build(
        Path::new(&format!("{WORKSPACE}/layers")),
        meadow::Options::debug(),
    );
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = out.linked.expect("linked program");
    // describe 20 == (20 / 2) + 1 == 11; twentyOne == 42 / 2 == 21
    assert_eq!(eval::run(&linked.program).unwrap().to_string(), "32");
}

/// `[profile.<name>]` sections, and the layering that puts them between the
/// profile's built-in meaning and a command-line flag.
#[test]
fn a_manifest_configures_the_build_profiles() {
    use meadow::package::ProfileConfig;
    use meadow::{OptLevel, Profile, Resolved, Strictness};

    let dir = std::env::temp_dir().join("meadow-profile-manifest");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.mw"), "def main = 1\n").unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\n\
         name = \"tuned\"\n\
         \n\
         [profile.debug]\n\
         opt-level = 2      # fast builds are not what this package wants\n\
         \n\
         [profile.release]\n\
         strictness = \"lenient\"\n",
    )
    .unwrap();

    let m = Manifest::load(&dir).unwrap().expect("a manifest");
    assert_eq!(m.profile("debug").opt, Some(OptLevel::O2));
    assert_eq!(m.profile("debug").strictness, None);
    assert_eq!(m.profile("release").strictness, Some(Strictness::Lenient));
    // A profile the manifest says nothing about keeps its built-in meaning.
    assert_eq!(m.profile("bench"), ProfileConfig::default());

    // Debug is `-O1` by default; this package's manifest makes it `-O2`, and
    // says nothing about strictness, so that stays lenient.
    let debug = Resolved::resolve(Profile::Debug, &dir, ProfileConfig::default());
    assert_eq!(debug.opt(), OptLevel::O2);
    assert_eq!(debug.strictness(), Strictness::Lenient);

    // Release is strict by default; the manifest turns that off.
    let release = Resolved::resolve(Profile::Release, &dir, ProfileConfig::default());
    assert_eq!(release.opt(), OptLevel::O2);
    assert_eq!(release.strictness(), Strictness::Lenient);

    // A flag beats both.
    let flagged = Resolved::resolve(
        Profile::Debug,
        &dir,
        ProfileConfig {
            opt: Some(OptLevel::O0),
            strictness: Some(Strictness::Strict),
            backend: None,
            prune: None,
        },
    );
    assert_eq!(flagged.opt(), OptLevel::O0);
    assert_eq!(flagged.strictness(), Strictness::Strict);

    // A package with no manifest at all is just the profile.
    let bare = Resolved::resolve(
        Profile::Release,
        Path::new("no/such/place"),
        ProfileConfig::default(),
    );
    assert_eq!(bare.options, Profile::Release.options());

    std::fs::remove_dir_all(&dir).ok();
}

/// Debug runs on the JIT and release as an executable, unless `meadow.toml` or
/// a flag says otherwise -- and a release backend nobody named gives way to the
/// JIT when there is no executable to be had.
#[test]
fn a_manifest_chooses_the_backend() {
    use meadow::package::ProfileConfig;
    use meadow::{Backend, Profile, Resolved};

    let bare = Path::new("no/such/place");
    let debug = Resolved::resolve(Profile::Debug, bare, ProfileConfig::default());
    assert_eq!(debug.backend, Backend::Jit);
    assert_eq!(debug.fallback(), None);
    let release = Resolved::resolve(Profile::Release, bare, ProfileConfig::default());
    assert_eq!(release.backend, Backend::Aot);
    assert_eq!(release.fallback().map(|r| r.backend), Some(Backend::Jit));

    let dir = std::env::temp_dir().join("meadow-backend-manifest");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.mw"), "def main = 1\n").unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\n\
         name = \"chosen\"\n\
         \n\
         [profile.debug]\n\
         backend = \"vm\"\n\
         \n\
         [profile.release]\n\
         backend = \"aot\"\n",
    )
    .unwrap();

    let m = Manifest::load(&dir).unwrap().expect("a manifest");
    assert_eq!(m.profile("debug").backend, Some(Backend::Vm));
    assert_eq!(m.profile("release").backend, Some(Backend::Aot));

    let debug = Resolved::resolve(Profile::Debug, &dir, ProfileConfig::default());
    assert_eq!(debug.backend, Backend::Vm);
    // Named in the manifest, so it is what the package gets or an error.
    let release = Resolved::resolve(Profile::Release, &dir, ProfileConfig::default());
    assert_eq!(release.backend, Backend::Aot);
    assert_eq!(release.fallback(), None);

    // A flag beats the manifest.
    let flagged = Resolved::resolve(
        Profile::Release,
        &dir,
        ProfileConfig {
            backend: Some(Backend::Jit),
            ..ProfileConfig::default()
        },
    );
    assert_eq!(flagged.backend, Backend::Jit);

    std::fs::remove_dir_all(&dir).ok();
}

/// Pruning is on in every profile; a manifest can turn it off for one, and a
/// flag beats the manifest.
#[test]
fn prune_is_on_unless_a_manifest_or_a_flag_says_otherwise() {
    use meadow::package::ProfileConfig;
    use meadow::profile::{Profile, Resolved};

    let dir = std::env::temp_dir().join("meadow-prune-manifest");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Main.mw"), "def main = 1\n").unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\n\
         name = \"pruned\"\n\
         \n\
         [profile.release]\n\
         prune = false\n",
    )
    .unwrap();

    let debug = Resolved::resolve(Profile::Debug, &dir, ProfileConfig::default());
    assert!(debug.prune, "on when nothing says otherwise");
    let release = Resolved::resolve(Profile::Release, &dir, ProfileConfig::default());
    assert!(!release.prune, "off where the manifest says so");
    let flagged = Resolved::resolve(
        Profile::Debug,
        &dir,
        ProfileConfig {
            prune: Some(false),
            ..ProfileConfig::default()
        },
    );
    assert!(!flagged.prune, "off where a flag says so");

    std::fs::remove_dir_all(&dir).ok();
}
