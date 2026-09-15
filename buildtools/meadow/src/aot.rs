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
//! `libmeadow_rts.a` (`meadow_rts.lib` on Windows), built for the target:
//! where `MEADOW_RUNTIME` names it; else, for the host, the one built into this
//! `meadow` (see `build.rs`); else beside this `meadow` binary, as
//! `lib/<triple>/libmeadow_rts.a` or plain `libmeadow_rts.a` for the host;
//! else, in a checkout, where `cargo build` in `rts` leaves it.
//!
//! It must be built from the same runtime sources as this `meadow`, whose code
//! generator assumes that runtime's layout: a library from other sources lacks
//! the symbol the program's `main` refers to (see `rts/build.rs`), so it does
//! not link, and the next place is tried.
//!
//! # The linker
//!
//! The system's C compiler: `cc`, or on Windows the Visual Studio `cl.exe` for
//! the target, found as Rust finds it. `MEADOW_CC` names another.

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
            (Arch::Aarch64, Format::Coff) => "aarch64-pc-windows-msvc",
            (Arch::X86_64, Format::Coff) => "x86_64-pc-windows-msvc",
        }
    }
}

/// Compile `image` for `target` at `opt`, and link it into package `name`'s executable,
/// answering where it went.
pub fn build(
    root: &Path,
    profile: Profile,
    opt: meadow_compiler::OptLevel,
    name: &str,
    image: &meadow_bytecode::Program,
    target: Target,
) -> Result<PathBuf, String> {
    let exe =
        native_dir(root, profile, target).join(format!("{name}{}", target.format.exe_suffix()));
    link_image(image, opt, target, &exe)?;
    Ok(exe)
}

/// Where what is compiled for `target` goes: the host's where `run` looks;
/// another target's beside it, under its triple -- and without `\\?\`, which
/// `cl` takes for the start of a file name.
pub fn native_dir(root: &Path, profile: Profile, target: Target) -> PathBuf {
    let native = artifacts::native_dir(root, profile);
    let mut dir = PathBuf::from(crate::dap::session::plain_path(&native.to_string_lossy()));
    if Target::host().ok() != Some(target) {
        dir = dir.join(target.triple());
    }
    dir
}

/// Write the native code `image` compiles to for `target` at `opt` as text,
/// under package `name`'s native directory, answering where it went -- what
/// `--emit asm` makes in place of an executable.
pub fn write_asm(
    root: &Path,
    profile: Profile,
    opt: meadow_compiler::OptLevel,
    name: &str,
    image: &meadow_bytecode::Program,
    target: Target,
) -> Result<PathBuf, String> {
    let path = native_dir(root, profile, target).join(format!("{name}.s"));
    artifacts::write_text(&path, &crate::listing::asm(image, target.arch, opt))?;
    Ok(path)
}

/// Compile `image` for `target` at `opt` and link it into the executable
/// `exe`, with the object file and `main` it is linked from beside it --
/// what `meadow link` does with an image from anywhere.
pub fn link_image(
    image: &meadow_bytecode::Program,
    opt: meadow_compiler::OptLevel,
    target: Target,
    exe: &Path,
) -> Result<(), String> {
    let runtimes = runtimes(target)?;
    let dir = match exe.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} is not a file name", exe.display()))?;

    let compiled = codegen::compile(image, target.arch, opt);
    let bytes = meadow_bytecode::image::encode(image);
    let object = dir.join(format!("{name}.{}", target.format.object_extension()));
    write(
        &object,
        &codegen::object::write(&compiled, &bytes, target.format),
    )?;
    let main = dir.join(format!("{name}-main.c"));
    write(&main, codegen::object::main_c().as_bytes())?;

    let mut stale = Vec::new();
    for runtime in &runtimes {
        match link(&main, &object, runtime, &exe, target) {
            Ok(()) => return Ok(()),
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
    let (mut cmd, cc) = compiler(target)?;
    match target.format {
        Format::MachO | Format::Elf => {
            cmd.arg(main).arg(object).arg(runtime).arg("-o").arg(exe);
            if target.format == Format::MachO {
                let arch = match target.arch {
                    Arch::Aarch64 => "arm64",
                    Arch::X86_64 => "x86_64",
                };
                cmd.args(["-arch", arch]);
            }
        }
        Format::Coff => {
            // `main`'s own object goes beside the rest rather than wherever
            // this was run from.
            let mut fo = std::ffi::OsString::from("/Fo");
            fo.push(main.with_extension("obj"));
            let mut fe = std::ffi::OsString::from("/Fe");
            fe.push(exe);
            cmd.args(["/nologo", "/MD"])
                .arg(fo)
                .arg(fe)
                .arg(main)
                .arg(object)
                .arg(runtime);
        }
    }
    cmd.args(target.format.system_libs());
    // On Windows the executable a run just made can stay open for a moment
    // after it exits -- a virus scanner looking at it -- and the linker cannot
    // replace it (LNK1104). That goes away by itself: wait for it, a little.
    let mut out;
    let mut tries = 0;
    loop {
        out = cmd
            .output()
            .map_err(|e| format!("could not run `{cc}` to link: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout);
        let locked = target.format == Format::Coff
            && !out.status.success()
            && said
                .lines()
                .any(|l| l.contains("LNK1104") && l.contains(&*exe.to_string_lossy()));
        if !locked || tries == 20 {
            break;
        }
        tries += 1;
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if !out.status.success() {
        // The Microsoft tools say what went wrong on standard output.
        return Err(format!(
            "linking {} failed:\n{}{}",
            exe.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// The C compiler that links for `target`, and what to call it in a message:
/// `MEADOW_CC` if it is set -- on Windows, something that takes `cl`'s
/// arguments, like `clang-cl` -- else `cc`, or on Windows the Visual Studio
/// `cl.exe` for the target, with the environment it needs to find its headers
/// and libraries.
fn compiler(target: Target) -> Result<(Command, String), String> {
    if let Ok(cc) = std::env::var("MEADOW_CC") {
        return Ok((Command::new(&cc), cc));
    }
    if target.format != Format::Coff {
        return Ok((Command::new("cc"), "cc".to_string()));
    }
    #[cfg(windows)]
    if let Some(cmd) = find_msvc_tools::find(target.triple(), "cl.exe") {
        return Ok((cmd, "cl.exe".to_string()));
    }
    Err(format!(
        "no C compiler to link for {}: install Visual Studio's C++ build tools, \
         or name a `cl`-compatible compiler with MEADOW_CC",
        target.triple()
    ))
}

/// What the runtime library is called, as its format's linker wants it.
fn library_name(format: Format) -> &'static str {
    match format {
        Format::Coff => "meadow_rts.lib",
        Format::MachO | Format::Elf => "libmeadow_rts.a",
    }
}

/// The runtime library built into this `meadow`, for the machine it runs on --
/// empty if it was built without one (see `build.rs`).
static EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime"));

/// The embedded runtime library, as a file a linker can be handed: under
/// `~/.meadow/lib/<its fingerprint>/`, written the first time it is wanted.
/// Its fingerprint names the directory, so a `meadow` from other sources
/// writes its own beside it rather than over it.
fn embedded(lib: &str) -> Option<PathBuf> {
    if EMBEDDED.is_empty() {
        return None;
    }
    let dir = crate::stdlib::home()?
        .join("lib")
        .join(codegen::object::runtime_symbol());
    let path = dir.join(lib);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == EMBEDDED.len() as u64) {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    // Whole or not at all: another `meadow` may be linking against it.
    let partial = dir.join(format!("{lib}.{}", std::process::id()));
    std::fs::write(&partial, EMBEDDED).ok()?;
    if std::fs::rename(&partial, &path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    Some(path).filter(|p| p.is_file())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Where a runtime library for `target` might be, most wanted first -- see the
/// module docs.
pub fn runtimes(target: Target) -> Result<Vec<PathBuf>, String> {
    let lib = library_name(target.format);
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
    if host && let Some(path) = embedded(lib) {
        candidates.push(path);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("lib").join(target.triple()).join(lib));
        if host {
            candidates.push(dir.join(lib));
        }
    }
    // A checkout: the `rts` crate's own target directory.
    let rts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rts/target");
    for profile in ["release", "debug"] {
        candidates.push(rts.join(target.triple()).join(profile).join(lib));
        if host {
            candidates.push(rts.join(profile).join(lib));
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
