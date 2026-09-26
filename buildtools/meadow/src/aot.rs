//! **Native executables**, compiled ahead of time.
//!
//! The bytecode image a build makes is compiled to machine code
//! (`meadow_glade::codegen`), written with the image into an object file, and
//! linked by the system's C compiler with a small `main` and the runtime as a
//! static library. What comes out is an ordinary executable, under the
//! package's `target/<profile>/native/` (see [`crate::artifacts`]) -- or
//! `native/<triple>/`, for a target other than this machine.
//!
//! # The runtime to link
//!
//! `libmeadow_glade.a` (`meadow_glade.lib` on Windows) -- or Silo's,
//! `libmeadow_silo.a` -- built for the target: where `MEADOW_RUNTIME` names
//! it; else, for the host, the one built into this `meadow` (see `build.rs`);
//! else beside this `meadow` binary, as `lib/<triple>/<library>` or plain
//! `<library>` for the host; else, in a checkout, where `cargo build` in
//! `glade` or `silo` leaves it.
//!
//! It must be built from the same runtime sources as this `meadow`, whose code
//! generator assumes that runtime's layout: a library from other sources lacks
//! the symbol the program's `main` refers to (see `glade/build.rs`), so it does
//! not link, and the next place is tried.
//!
//! # The linker
//!
//! The system's C compiler: `cc`, or on Windows the Visual Studio `cl.exe` for
//! the target, found as Rust finds it. `MEADOW_CC` names another.

use crate::artifacts;
use crate::profile::Profile;
use meadow_glade::codegen::{self, Arch, object::Format};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which of Meadow's two runtime systems a program runs on.
///
/// **Glade** (`meadow_glade`) is the one the interpreter, the JIT and the
/// debugger share: bytecode, compiled to machine code block by block as it
/// runs or ahead of time into an executable (`--aot`), with a garbage
/// collector tending the heap. **Silo** (`meadow_silo`) is compiled all the way
/// down by LLVM (`meadow-llvm`) before it runs, counts references instead of
/// collecting, and carries no interpreter: always an executable. See
/// `docs/SILO.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Runtime {
    #[default]
    Glade,
    Silo,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Runtime::Glade => "glade",
            Runtime::Silo => "silo",
        }
    }

    pub fn named(name: &str) -> Option<Runtime> {
        match name {
            "glade" => Some(Runtime::Glade),
            "silo" => Some(Runtime::Silo),
            _ => None,
        }
    }

    /// The symbol a library of this runtime built from the sources this
    /// `meadow` was defines.
    pub fn symbol(self) -> String {
        match self {
            Runtime::Glade => codegen::object::runtime_symbol(),
            Runtime::Silo => meadow_llvm::runtime_symbol(),
        }
    }

    /// The static library's file name, for `format`.
    pub fn library(self, format: Format) -> &'static str {
        match (self, format) {
            (Runtime::Glade, Format::Coff) => "meadow_glade.lib",
            (Runtime::Glade, Format::MachO | Format::Elf) => "libmeadow_glade.a",
            (Runtime::Silo, Format::Coff) => "meadow_silo.lib",
            (Runtime::Silo, Format::MachO | Format::Elf) => "libmeadow_silo.a",
        }
    }
}

/// What to compile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub format: Format,
    /// Which runtime library the program links: see [`Runtime`].
    pub runtime: Runtime,
}

impl Target {
    /// The machine this is running on.
    pub fn host() -> Result<Target, String> {
        let arch = Arch::host().ok_or("native code is for aarch64 and x86-64 only")?;
        Ok(Target {
            arch,
            format: Format::host(),
            runtime: Runtime::Glade,
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
            runtime: Runtime::Glade,
        })
    }

    /// This, linked against `runtime`.
    pub fn with_runtime(self, runtime: Runtime) -> Target {
        Target { runtime, ..self }
    }

    /// Is this the machine this is running on, whichever runtime it links?
    pub fn is_host(self) -> bool {
        Target::host().is_ok_and(|h| (h.arch, h.format) == (self.arch, self.format))
    }

    /// The Rust target the runtime library for this is built for.
    pub fn triple(self) -> &'static str {
        match (self.arch, self.format) {
            // An ELF target on Android is Android: a native executable is for
            // the machine this runs on, and on a phone that is not glibc Linux.
            (Arch::Aarch64, Format::Elf) if cfg!(target_os = "android") => "aarch64-linux-android",
            (Arch::X86_64, Format::Elf) if cfg!(target_os = "android") => "x86_64-linux-android",
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
    calls: &codegen::Calls,
) -> Result<PathBuf, String> {
    let exe =
        native_dir(root, profile, target).join(format!("{name}{}", target.format.exe_suffix()));
    // An executable that is already the one this image makes is left alone --
    // and that matters more than it sounds. Linking is the slow part of an
    // `aot` build, and on macOS the *first* run of a newly written binary pays
    // for its signature to be checked, which is slower still. Relinking an
    // unchanged program therefore cost about a fifth of a second every time it
    // was run, against twenty milliseconds for the same program on the JIT.
    let stamp = exe.with_extension("stamp");
    let want = made_from(image, opt, target, calls);
    if exe.exists() && std::fs::read_to_string(&stamp).is_ok_and(|had| had == want) {
        return Ok(exe);
    }
    link_image(image, opt, target, calls, &exe)?;
    // After linking, so that a link that failed half-way is not taken for a
    // finished one.
    let _ = std::fs::write(&stamp, &want);
    Ok(exe)
}

/// Compile `program` with the native backend -- AxCut to LLVM IR, compiled and
/// linked with the `meadow_silo` runtime by clang -- into package `name`'s
/// executable, answering where it went. See `docs/SILO.md`.
pub fn build_native(
    root: &Path,
    profile: Profile,
    opt: meadow_compiler::OptLevel,
    name: &str,
    program: &meadow_compiler::core::Program,
    target: Target,
) -> Result<PathBuf, String> {
    let lowered = meadow_seq::lower_program(program, opt);
    if !lowered.unsupported.is_empty() {
        return Err(format!(
            "the back end cannot translate {:?} yet",
            lowered.unsupported
        ));
    }
    let units =
        meadow_llvm::compile_split(&lowered.program, meadow_llvm::UNIT).map_err(|e| e.msg)?;
    let dir = native_dir(root, profile, target);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let exe = dir.join(format!("{name}{}", target.format.exe_suffix()));
    let runtime = runtimes(target)?
        .into_iter()
        .next()
        .ok_or("no Silo runtime library")?;
    // Unchanged module and runtime: the executable there is this one.
    let stamp = exe.with_extension("stamp");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let meta = std::fs::metadata(&runtime).ok();
    for b in units
        .iter()
        .flat_map(|u| u.bytes())
        .chain(opt.name().bytes())
        .chain(meta.iter().flat_map(|m| m.len().to_le_bytes()))
        .chain(
            meta.iter()
                .filter_map(|m| m.modified().ok())
                .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .flat_map(|d| d.as_nanos().to_le_bytes()),
        )
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let want = format!("{h:016x}");
    if exe.exists() && std::fs::read_to_string(&stamp).is_ok_and(|had| had == want) {
        return Ok(exe);
    }
    let modules = write_units(&dir, name, &units)?;
    clang_link(&modules, &runtime, &exe, opt, target)?;
    let _ = std::fs::write(&stamp, &want);
    Ok(exe)
}

/// Write the LLVM modules `units` into `dir`, as `name.ll`, `name.1.ll`, ...,
/// and answer their paths.
pub fn write_units(dir: &Path, name: &str, units: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        let path = if i == 0 {
            dir.join(format!("{name}.ll"))
        } else {
            dir.join(format!("{name}.{i}.ll"))
        };
        write(&path, u.as_bytes())?;
        paths.push(path);
    }
    Ok(paths)
}

/// Compile the LLVM modules at `modules` -- in parallel, when there are
/// several -- and link them with the runtime library `runtime` into `exe`,
/// with clang (or `MEADOW_CLANG`).
pub fn clang_link(
    modules: &[PathBuf],
    runtime: &Path,
    exe: &Path,
    opt: meadow_compiler::OptLevel,
    target: Target,
) -> Result<(), String> {
    let level = match opt {
        meadow_compiler::OptLevel::O0 => "-O0",
        meadow_compiler::OptLevel::O1 => "-O1",
        meadow_compiler::OptLevel::O2 => "-O2",
    };
    let clang = std::env::var("MEADOW_CLANG").unwrap_or_else(|_| "clang".into());
    let run = |cmd: &mut Command, what: &Path| -> Result<(), String> {
        let out = cmd.output().map_err(|e| {
            format!("could not run {clang}: {e} -- the native backend needs clang, or MEADOW_CLANG")
        })?;
        if !out.status.success() {
            // MSVC's linker says what went wrong on stdout.
            return Err(format!(
                "clang could not compile {}:\n{}{}",
                what.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    };
    let triple = format!("--target={}", target.triple());
    // One module compiles and links in one step; several are compiled apart
    // first, as many at once as there are cores.
    let inputs: Vec<PathBuf> = if modules.len() == 1 {
        modules.to_vec()
    } else {
        let objects: Vec<PathBuf> = modules.iter().map(|m| m.with_extension("o")).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let failed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        std::thread::scope(|scope| {
            for _ in 0..cores.min(modules.len()) {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= modules.len() {
                            return;
                        }
                        let mut cmd = Command::new(&clang);
                        cmd.arg(level)
                            .arg("-c")
                            .arg("-Wno-override-module")
                            .arg(&triple)
                            .arg("-o")
                            .arg(&objects[i])
                            .arg(&modules[i]);
                        if let Err(e) = run(&mut cmd, &modules[i]) {
                            failed
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .get_or_insert(e);
                            return;
                        }
                    }
                });
            }
        });
        if let Some(e) = failed.into_inner().unwrap_or_else(|p| p.into_inner()) {
            return Err(e);
        }
        objects
    };
    let mut cmd = Command::new(&clang);
    cmd.arg(level)
        .arg("-Wno-override-module")
        .arg(&triple)
        .arg("-o")
        .arg(exe)
        .args(&inputs)
        .arg(runtime);
    for lib in native_system_libs(target.format) {
        cmd.arg(lib);
    }
    run(&mut cmd, &modules[0])
}

/// What clang links a program against besides the runtime library.
fn native_system_libs(format: Format) -> &'static [&'static str] {
    match format {
        Format::Coff => &[
            "-lkernel32",
            "-lntdll",
            "-luserenv",
            "-lws2_32",
            "-ldbghelp",
            "-lbcrypt",
            "-ladvapi32",
        ],
        Format::MachO => &["-liconv"],
        Format::Elf => &["-lpthread", "-ldl", "-lm"],
    }
}

/// What an executable was made from, as one line: the image, how it was
/// compiled, what for, and which runtime it was linked against. Anything that
/// would change the executable changes this.
fn made_from(
    image: &meadow_bytecode::Program,
    opt: meadow_compiler::OptLevel,
    target: Target,
    calls: &codegen::Calls,
) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(&meadow_bytecode::image::encode(image));
    feed(opt.name().as_bytes());
    feed(target.triple().as_bytes());
    feed(target.runtime.name().as_bytes());
    // And what a profile said the calls enter, which the code is guarded on.
    let mut sites: Vec<_> = calls.iter().collect();
    sites.sort_by_key(|(pc, _)| **pc);
    for (pc, k) in sites {
        feed(&pc.to_le_bytes());
        feed(&[k.frame as u8]);
        feed(&k.meta.to_le_bytes());
    }
    // The runtime is linked in, so a new one makes a new executable. Its name
    // carries this compiler's identity (see `link_image`), and its file says
    // whether it has been rebuilt since.
    feed(target.runtime.symbol().as_bytes());
    for runtime in runtimes(target).unwrap_or_default() {
        if let Ok(meta) = std::fs::metadata(&runtime) {
            feed(&meta.len().to_le_bytes());
            if let Ok(t) = meta.modified()
                && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
            {
                feed(&d.as_nanos().to_le_bytes());
            }
        }
    }
    format!("{h:016x}")
}

/// Where what is compiled for `target` goes: the host's where `run` looks;
/// another target's beside it, under its triple -- and without `\\?\`, which
/// `cl` takes for the start of a file name.
pub fn native_dir(root: &Path, profile: Profile, target: Target) -> PathBuf {
    let native = artifacts::native_dir(root, profile);
    let mut dir = PathBuf::from(crate::dap::session::plain_path(&native.to_string_lossy()));
    if !target.is_host() {
        dir = dir.join(target.triple());
    }
    // Beside the default runtime's, so the two can be run against each other.
    if target.runtime != Runtime::Glade {
        dir = dir.join(target.runtime.name());
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
    calls: &codegen::Calls,
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

    let compiled = codegen::compile_guided(image, target.arch, opt, calls);
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
        "\nrebuild it with `cargo build --release --target {}` in `glade`",
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

/// The runtime libraries built into this `meadow`, for the machine it runs on
/// -- each empty if it was built without it (see `build.rs`).
static EMBEDDED_GLADE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime"));
static EMBEDDED_SILO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime-silo"));

/// `runtime`'s embedded library, as a file a linker can be handed: under
/// `~/.meadow/lib/<its fingerprint>/`, written the first time it is wanted.
/// Its fingerprint names the directory, so a `meadow` from other sources
/// writes its own beside it rather than over it.
fn embedded(runtime: Runtime, lib: &str) -> Option<PathBuf> {
    let bytes = match runtime {
        Runtime::Glade => EMBEDDED_GLADE,
        Runtime::Silo => EMBEDDED_SILO,
    };
    if bytes.is_empty() {
        return None;
    }
    let dir = crate::stdlib::home()?.join("lib").join(runtime.symbol());
    let path = dir.join(lib);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64) {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    // Whole or not at all: another `meadow` may be linking against it.
    let partial = dir.join(format!("{lib}.{}", std::process::id()));
    std::fs::write(&partial, bytes).ok()?;
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
    let lib = target.runtime.library(target.format);
    // Where a checkout builds it: the runtime's crate, `glade` or `silo`.
    let crate_dir = target.runtime.name();
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
    let host = target.is_host();
    let mut candidates = Vec::new();
    if host && let Some(path) = embedded(target.runtime, lib) {
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
    // A checkout: the runtime crate's own target directory.
    let built = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(crate_dir)
        .join("target");
    for profile in ["release", "debug"] {
        candidates.push(built.join(target.triple()).join(profile).join(lib));
        if host {
            candidates.push(built.join(profile).join(lib));
        }
    }
    let found: Vec<PathBuf> = candidates.into_iter().filter(|p| p.is_file()).collect();
    if !found.is_empty() {
        return Ok(found);
    }
    Err(format!(
        "no `{crate_dir}` runtime library for {} -- build one with \
         `cargo build --release --target {}` in `{crate_dir}`, or name it with MEADOW_RUNTIME",
        target.triple(),
        target.triple()
    ))
}
