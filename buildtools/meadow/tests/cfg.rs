//! `@cfg` from the driver's side: where a build's conditions come from -- the
//! profile, the backend, the manifest, `--cfg` -- and that they reach the
//! program.

use meadow::package::ProfileConfig;
use meadow::pipeline;
use meadow::profile::{Profile, Resolved};
use meadow_compiler::intern::InternedString;

/// A package whose `main` reports what its `@cfg`s kept.
fn package(name: &str, manifest_profiles: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-cfg-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n{manifest_profiles}"),
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.mw"),
        r#"
@cfg(fast) def speed = "fast"
@cfg(not(fast)) def speed = "normal"
@cfg(feature = "gpu") def gpu = True
@cfg(not(feature = "gpu")) def gpu = False
@cfg(release) def profile = "release"
@cfg(debug) def profile = "debug"
@cfg(backend = "aot") def backend = "aot"
@cfg(not(backend = "aot")) def backend = "not aot"
def main = (speed, gpu, profile, backend)
"#,
    )
    .unwrap();
    dir
}

/// What `main` answers when `dir` is built as `resolved` says.
fn answer(dir: &std::path::Path, resolved: Resolved) -> String {
    let out = pipeline::build(dir, resolved.options);
    assert!(
        out.diagnostics.is_empty(),
        "{:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = out.linked.expect("linked");
    meadow::runtime::run(&linked.program, meadow::Engine::Cek, resolved.opt()).unwrap()
}

#[test]
fn the_profile_and_backend_are_conditions() {
    let dir = package("profiles", "");
    let debug = Resolved::resolve(Profile::Debug, &dir, ProfileConfig::default());
    assert_eq!(
        answer(&dir, debug),
        r#"("normal", False, "debug", "not aot")"#
    );
    let release = Resolved::resolve(Profile::Release, &dir, ProfileConfig::default());
    assert_eq!(
        answer(&dir, release),
        r#"("normal", False, "release", "aot")"#
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn flags_come_from_the_manifest_and_the_command_line_together() {
    let dir = package("flags", "[profile.debug]\ncfg = \"fast\"\n");
    let manifest_only = Resolved::resolve(Profile::Debug, &dir, ProfileConfig::default());
    assert_eq!(
        answer(&dir, manifest_only),
        r#"("fast", False, "debug", "not aot")"#
    );
    let both = Resolved::resolve(
        Profile::Debug,
        &dir,
        ProfileConfig {
            cfg: Some(InternedString::from("feature=gpu")),
            ..ProfileConfig::default()
        },
    );
    assert_eq!(answer(&dir, both), r#"("fast", True, "debug", "not aot")"#);
    // The manifest's flags are the debug profile's, not the release one's.
    let release = Resolved::resolve(Profile::Release, &dir, ProfileConfig::default());
    assert_eq!(
        answer(&dir, release),
        r#"("normal", False, "release", "aot")"#
    );
    std::fs::remove_dir_all(&dir).ok();
}
