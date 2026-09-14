//! The runtime library, built for the target this `meadow` is, and put where
//! `src/aot.rs` embeds it -- so that a release build makes native executables
//! with nothing installed beside the binary.
//!
//! Only for a release build of `meadow`, or when `MEADOW_EMBED_RUNTIME=1`
//! asks: the library is a release build of `rts` with its link-time
//! optimization, which is minutes, not seconds, and a debug `meadow` in a
//! checkout finds the one `cargo build --release` in `rts` leaves instead.
//! `MEADOW_EMBED_RUNTIME=0` leaves it out of a release build too.
//!
//! It is built by a `cargo` of its own, in a target directory of its own, from
//! the same sources as the `meadow_rts` this binary links -- so its fingerprint
//! (see `rts/build.rs`) is the one the code generator writes. A failure to
//! build it is a warning, not an error: `meadow` still works, and says where
//! else a runtime library can come from when it wants one.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let embedded = out.join("runtime");
    println!("cargo:rerun-if-env-changed=MEADOW_EMBED_RUNTIME");
    println!("cargo:rerun-if-changed=build.rs");

    let bytes = match wanted() {
        true => build(&out).unwrap_or_else(|e| {
            println!("cargo:warning=no runtime library built into meadow: {e}");
            Vec::new()
        }),
        false => Vec::new(),
    };
    // Unchanged contents leave the file alone, so nothing recompiles for it.
    if std::fs::read(&embedded).ok().as_deref() != Some(&bytes[..]) {
        std::fs::write(&embedded, &bytes).expect("OUT_DIR is writable");
    }
}

fn wanted() -> bool {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch != "x86_64" && arch != "aarch64" {
        return false;
    }
    match std::env::var("MEADOW_EMBED_RUNTIME").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => std::env::var("PROFILE").as_deref() == Ok("release"),
    }
}

/// Build the library and answer its bytes.
fn build(out: &Path) -> Result<Vec<u8>, String> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rts = here.join("../../rts");
    let compiler = here.join("../../compiler");
    for input in [
        rts.join("src"),
        rts.join("build.rs"),
        rts.join("Cargo.toml"),
        compiler.join("meadow-bytecode"),
        compiler.join("meadow-core"),
        compiler.join("meadow-intern"),
    ] {
        println!("cargo:rerun-if-changed={}", input.display());
    }

    let target = std::env::var("TARGET").map_err(|e| e.to_string())?;
    let dir = target_dir(out);
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "--release", "--lib", "--target", &target])
        .arg("--manifest-path")
        .arg(rts.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&dir);
    // What the outer build was told about itself is not for this one.
    for var in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
        "MFLAGS",
    ] {
        cmd.env_remove(var);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("`cargo build` in rts failed ({status})"));
    }
    let lib = if target.contains("windows-msvc") {
        "meadow_rts.lib"
    } else {
        "libmeadow_rts.a"
    };
    let path = dir.join(&target).join("release").join(lib);
    std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// A target directory shared by every build of `meadow` in this workspace --
/// debug and release, `build` and `check` -- beside theirs: `OUT_DIR` is
/// `<target>/<profile>/build/<package>/out`. Failing that, `OUT_DIR` itself.
fn target_dir(out: &Path) -> PathBuf {
    let build = out.ancestors().nth(2);
    match (build, out.ancestors().nth(4)) {
        (Some(b), Some(target)) if b.file_name().is_some_and(|n| n == "build") => {
            target.join("meadow-runtime")
        }
        _ => out.join("rts"),
    }
}
