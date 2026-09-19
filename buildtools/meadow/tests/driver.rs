//! What a program run by `meadow` gets from the command line and gives back to
//! it: its arguments, bytes written as they are, a bytecode image run or linked
//! on its own -- and nothing at all when it does not compile.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-driver-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A package at `dir/name` whose `main` is `main`. The directory is called
/// what this test calls it; the package is called what a package is called.
fn package(dir: &Path, name: &str, main: &str) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Meadow.toml"),
        format!(
            "[package]\nname = \"{}\"\n",
            meadow::package::as_package_name(name)
        ),
    )
    .unwrap();
    std::fs::write(root.join("src/Main.mw"), main).unwrap();
    root
}

fn meadow(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the meadow binary runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The line a run ends with.
fn answer(stdout: &str) -> &str {
    stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("=> "))
        .unwrap_or("")
}

const ARGS: &str =
    "use Std.Process\nuse Std.Collections.Vector as V\n\ndef main = V.toList (argv ())\n";

#[test]
fn a_program_gets_what_follows_the_double_dash() {
    let dir = scratch("args");
    let root = package(&dir, "args", ARGS);
    for engine in [&[][..], &["--backend", "vm"], &["--cek"]] {
        let mut args = vec!["run"];
        args.extend_from_slice(engine);
        args.extend([".", "--", "one", "two words", "--three"]);
        let (ok, out, err) = meadow(&root, &args);
        assert!(ok, "{err}");
        assert_eq!(
            answer(&out),
            r#"=> ["one"; "two words"; "--three"]"#,
            "{engine:?}"
        );
    }
    let (ok, out, err) = meadow(&root, &["run", "."]);
    assert!(ok, "{err}");
    assert_eq!(answer(&out), "=> []", "and nothing, given nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bytes_are_written_as_they_are() {
    let dir = scratch("bytes");
    let root = package(
        &dir,
        "bytes",
        "use Std.Fs\n\ndef main =\n  let w = writeBytes (\"out.bin\", #[0xff, 0x00, 0x41, 0x80]) in\n  (w, readBytes \"out.bin\")\n",
    );
    for engine in [&[][..], &["--backend", "vm"], &["--cek"]] {
        let mut args = vec!["run"];
        args.extend_from_slice(engine);
        args.push(".");
        let (ok, out, err) = meadow(&root, &args);
        assert!(ok, "{err}");
        assert_eq!(answer(&out), "=> (Ok(()), Ok(#[255, 0, 65, 128]))");
        assert_eq!(
            std::fs::read(root.join("out.bin")).unwrap(),
            [0xff, 0, 0x41, 0x80]
        );
        std::fs::remove_file(root.join("out.bin")).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_image_runs_on_its_own() {
    let dir = scratch("exec");
    let root = package(&dir, "img", ARGS);
    let (ok, _, err) = meadow(&root, &["build", "."]);
    assert!(ok, "{err}");
    let image = root.join("target/debug/bytecode/Img.mbc");
    assert!(image.is_file());
    let image = image.to_string_lossy().into_owned();
    for backend in ["jit", "vm"] {
        let (ok, out, err) = meadow(&dir, &["exec", &image, "--backend", backend, "--", "x"]);
        assert!(ok, "{err}");
        assert_eq!(answer(&out), r#"=> ["x"]"#);
    }
    let (ok, _, err) = meadow(&dir, &["exec", &root.join("Meadow.toml").to_string_lossy()]);
    assert!(!ok);
    assert!(err.contains("not a Meadow image"), "{err}");

    // An executable from it, where there is a runtime library to link.
    let exe = dir
        .join("out")
        .join(format!("img{}", std::env::consts::EXE_SUFFIX));
    let (ok, _, err) = meadow(&dir, &["link", &image, "-o", &exe.to_string_lossy()]);
    if !ok {
        assert!(
            err.contains("runtime"),
            "a link failure other than a missing runtime: {err}"
        );
        eprintln!("skipping the executable: {err}");
    } else {
        let out = Command::new(&exe).args(["a", "b"]).output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains(r#"["a"; "b"]"#));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_that_does_not_compile_is_not_run() {
    let dir = scratch("errors");
    let root = package(&dir, "broken", "def main = 1 + \"one\"\n");
    let (ok, out, err) = meadow(&root, &["run", "."]);
    assert!(!ok);
    assert!(err.contains("mismatch") || err.contains("integer"), "{err}");
    assert!(
        !out.contains("=>") && !err.contains("ill-formed"),
        "{out}{err}"
    );
    let (ok, _, _) = meadow(&root, &["build", "."]);
    assert!(!ok);
    assert!(
        !root.join("target/debug/bytecode/Broken.mbc").exists(),
        "and no image is written for it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
