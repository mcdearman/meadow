//! `@cfg(…)`: what a build keeps, and what it says about a condition it cannot
//! read.

use meadow_compiler::{Cfg, Options, compile_str_with};

/// The names `src` exports under `opts`, and its diagnostics.
fn exports(src: &str, opts: Options) -> (Vec<String>, Vec<String>) {
    let (pkg, diags) = compile_str_with("cfg", src, opts);
    let mut names: Vec<String> = pkg.exports.iter().map(|e| e.name.to_string()).collect();
    names.sort();
    (names, diags.iter().map(|d| d.msg.clone()).collect())
}

fn on(os: &'static str, profile: &'static str, backend: &'static str, flags: &str) -> Options {
    Options {
        cfg: Cfg::host(profile, backend)
            .on(os, "x86_64")
            .with_flags(flags),
        ..Options::debug()
    }
}

const PLATFORM: &str = r#"
@cfg(windows)
def onWindows = 1

@cfg(unix)
def onUnix = 1

@cfg(os = "macos")
def onMac = 1

@cfg(all(unix, not(os = "macos")))
def onOtherUnix = 1
"#;

#[test]
fn the_platform_decides() {
    assert_eq!(
        exports(PLATFORM, on("windows", "debug", "jit", "")).0,
        ["onWindows"]
    );
    assert_eq!(
        exports(PLATFORM, on("linux", "debug", "jit", "")).0,
        ["onOtherUnix", "onUnix"]
    );
    assert_eq!(
        exports(PLATFORM, on("macos", "debug", "jit", "")).0,
        ["onMac", "onUnix"]
    );
}

#[test]
fn the_profile_backend_and_test_decide() {
    let src = r#"
        @cfg(debug) def a = 1
        @cfg(release) def b = 1
        @cfg(backend = "aot") def c = 1
        @cfg(any(backend = "cek", test)) def d = 1
        @cfg(opt_level = "2") def e = 1
    "#;
    assert_eq!(exports(src, on("linux", "debug", "jit", "")).0, ["a"]);
    let release = Options {
        opt: meadow_compiler::OptLevel::O2,
        ..on("linux", "release", "aot", "")
    };
    assert_eq!(exports(src, release).0, ["b", "c", "e"]);
    let mut testing = on("linux", "debug", "jit", "");
    testing.cfg.test = true;
    assert_eq!(exports(src, testing).0, ["a", "d"]);
}

#[test]
fn flags_are_whatever_the_build_turned_on() {
    let src = r#"
        @cfg(fast) def a = 1
        @cfg(feature = "gpu") def b = 1
        @cfg(feature = "cpu") def c = 1
        @cfg(not(fast)) def d = 1
    "#;
    assert_eq!(exports(src, on("linux", "debug", "jit", "")).0, ["d"]);
    assert_eq!(
        exports(src, on("linux", "debug", "jit", "fast, feature = gpu")).0,
        ["a", "b"]
    );
}

#[test]
fn a_name_defined_once_per_build_is_not_a_duplicate() {
    let src = r#"
        @cfg(windows) def sep = "\\"
        @cfg(not(windows)) def sep = "/"
        def main = sep
    "#;
    let (names, diags) = exports(src, on("linux", "debug", "jit", ""));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(names, ["main", "sep"]);
}

#[test]
fn what_is_left_out_is_not_checked() {
    // Nonsense that never reaches the type checker on this build.
    let src = "@cfg(windows)\ndef broken = 1 + \"one\"\ndef main = 2\n";
    let (names, diags) = exports(src, on("linux", "debug", "jit", ""));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(names, ["main"]);
}

#[test]
fn fields_and_operations_take_cfg_too() {
    let src = r#"
        record Config = {
          name : String,
          @cfg(windows)
          drive : String,
        }
        @cfg(windows)
        def config = Config { name = "w", drive = "C:" }
        @cfg(not(windows))
        def config = Config { name = "u" }
    "#;
    for os in ["windows", "linux"] {
        let (_, diags) = exports(src, on(os, "debug", "jit", ""));
        assert!(diags.is_empty(), "{os}: {diags:?}");
    }
}

#[test]
fn a_condition_that_cannot_be_read_is_an_error() {
    for (src, want) in [
        ("@cfg(os = \"linx\")\ndef x = 1\n", "is never"),
        ("@cfg(not(unix, windows))\ndef x = 1\n", "exactly one"),
        ("@cfg(some(unix))\ndef x = 1\n", "there is no `some"),
        ("@cfg\ndef x = 1\n", "one condition"),
        ("@cfg(unix, windows)\ndef x = 1\n", "one condition"),
    ] {
        let (_, diags) = exports(src, on("linux", "debug", "jit", ""));
        assert!(diags.iter().any(|d| d.contains(want)), "{src}: {diags:?}");
    }
}
