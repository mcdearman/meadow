//! **Native executables**, compiled ahead of time.
//!
//! The bytecode image a build makes is compiled to machine code
//! (`meadow_rts::codegen`), written with the image into an object file, and
//! linked by the system's C compiler with a small `main` and the runtime as a
//! static library. What comes out is an ordinary executable, under the
//! package's `target/<profile>/native/` (see [`crate::artifacts`]) -- or
//! `native/<triple>/`, for a target other than this machine.
//!
//! # The runtime to link
//!
//! `libmeadow_rts.a`, built for the target: where `MEADOW_RUNTIME` names it;
//! else beside this `meadow` binary, as `lib/<triple>/libmeadow_rts.a` or plain
//! `libmeadow_rts.a` for the host; else, in a checkout, where `cargo build` in
//! `rts` leaves it.
//!
//! It must be built from the same runtime sources as this `meadow`, whose code
//! generator assumes that runtime's layout: a library from other sources lacks
//! the symbol the program's `main` refers to (see `rts/build.rs`), so it does
//! not link, and the next place is tried.

use crate::artifacts;
use crate::profile::Profile;
use meadow_rts::codegen::{self, Arch, object::Format};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What to compile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub format: Format,
}

impl Target {
    /// The machine this is running on.
    pub fn host() -> Result<Target, String> {
        let arch = Arch::host().ok_or("native code is for aarch64 and x86-64 only")?;
        Ok(Target {
            arch,
            format: Format::host(),
        })
    }

    /// An architecture by name, for this platform's object format:
    /// `aarch64` (or `arm64`), `x86_64` (or `x86-64`).
    pub fn named(name: &str) -> Result<Target, String> {
        let arch = match name {
            "aarch64" | "arm64" => Arch::Aarch64,
            "x86_64" | "x86-64" | "amd64" => Arch::X86_64,
            other => {
                return Err(format!(
                    "no native target `{other}` -- there is `aarch64` and `x86_64`"
                ));
            }
        };
        Ok(Target {
            arch,
            format: Format::host(),
        })
    }

    /// The Rust target the runtime library for this is built for.
    pub fn triple(self) -> &'static str {
        match (self.arch, self.format) {
            (Arch::Aarch64, Format::MachO) => "aarch64-apple-darwin",
            (Arch::X86_64, Format::MachO) => "x86_64-apple-darwin",
            (Arch::Aarch64, Format::Elf) => "aarch64-unknown-linux-gnu",
            (Arch::X86_64, Format::Elf) => "x86_64-unknown-linux-gnu",
        }
    }
}

/// Compile `image` for `target` and link it into package `name`'s executable,
/// answering where it went.
pub fn build(
    root: &Path,
    profile: Profile,
    name: &str,
    image: &meadow_bytecode::Program,
    target: Target,
) -> Result<PathBuf, String> {
    let runtimes = runtimes(target)?;
    // The host's executable where `run` looks; another target's beside it,
    // under its triple.
    let mut dir = artifacts::native_dir(root, profile);
    if Target::host().ok() != Some(target) {
        dir = dir.join(target.triple());
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    let compiled = codegen::compile(image, target.arch);
    let bytes = meadow_bytecode::image::encode(image);
    let object = dir.join(format!("{name}.o"));
    write(
        &object,
        &codegen::object::write(&compiled, &bytes, target.format),
    )?;
    let main = dir.join(format!("{name}-main.c"));
    write(&main, codegen::object::main_c().as_bytes())?;
    let exe = dir.join(name);

    let mut stale = Vec::new();
    for runtime in &runtimes {
        match link(&main, &object, runtime, &exe, target) {
            Ok(()) => return Ok(exe),
            Err(e) if e.contains(&codegen::object::runtime_symbol()) => stale.push(runtime),
            Err(e) => return Err(e),
        }
    }
    let mut msg = String::from("no runtime library built from this compiler's sources:");
    for runtime in stale {
        msg.push_str(&format!("\n  {} is from another build", runtime.display()));
    }
    msg.push_str(&format!(
        "\nrebuild it with `cargo build --release --target {}` in `rts`",
        target.triple()
    ));
    Err(msg)
}

/// Link `main`, `object` and `runtime` into `exe`, or say why the linker would
/// not.
fn link(
    main: &Path,
    object: &Path,
    runtime: &Path,
    exe: &Path,
    target: Target,
) -> Result<(), String> {
    let cc = std::env::var("MEADOW_CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.arg(&main).arg(&object).arg(runtime).arg("-o").arg(exe);
    match target.format {
        Format::MachO => {
            let arch = match target.arch {
                Arch::Aarch64 => "arm64",
                Arch::X86_64 => "x86_64",
            };
            cmd.args(["-arch", arch, "-liconv", "-lSystem", "-lc", "-lm"]);
        }
        Format::Elf => {
            cmd.args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ]);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run `{cc}` to link: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "linking {} failed:\n{}",
            exe.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Where a runtime library for `target` might be, most wanted first -- see the
/// module docs.
pub fn runtimes(target: Target) -> Result<Vec<PathBuf>, String> {
    const LIB: &str = "libmeadow_rts.a";
    if let Some(path) = std::env::var_os("MEADOW_RUNTIME") {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(vec![path])
        } else {
            Err(format!(
                "MEADOW_RUNTIME names {}, which is not there",
                path.display()
            ))
        };
    }
    let host = Target::host().ok() == Some(target);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("lib").join(target.triple()).join(LIB));
        if host {
            candidates.push(dir.join(LIB));
        }
    }
    // A checkout: the `rts` crate's own target directory.
    let rts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rts/target");
    for profile in ["release", "debug"] {
        candidates.push(rts.join(target.triple()).join(profile).join(LIB));
        if host {
            candidates.push(rts.join(profile).join(LIB));
        }
    }
    let found: Vec<PathBuf> = candidates.into_iter().filter(|p| p.is_file()).collect();
    if !found.is_empty() {
        return Ok(found);
    }
    Err(format!(
        "no runtime library for {} -- build one with `cargo build --release --target {}` in `rts`, \
             or name it with MEADOW_RUNTIME",
        target.triple(),
        target.triple()
    ))
}
