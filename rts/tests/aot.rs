//! Programs compiled ahead of time: to native code, into an object file, linked
//! with the system's C compiler against this runtime, and run as a process.
//! Each must answer what the bytecode VM answers.

use meadow_compiler::{compile_str, core};
use meadow_rts::codegen::{self, Arch, object};
use std::path::{Path, PathBuf};
use std::process::Command;

fn program(src: &str) -> core::Program {
    let (pkg, diags) = compile_str("aot", src);
    let hard: Vec<_> = diags.iter().map(|d| d.msg.clone()).collect();
    assert!(hard.is_empty(), "compile errors:\n{}", hard.join("\n"));
    let entry = pkg
        .exports
        .iter()
        .find(|e| &*e.name == "main")
        .map(|e| e.var);
    core::Program {
        defs: pkg.defs.clone(),
        entry,
        ctor_fields: pkg.ctor_fields.clone(),
        variants: pkg.variants.clone(),
        origins: Default::default(),
    }
}

fn image(src: &str, opt: meadow_core::OptLevel) -> meadow_bytecode::Program {
    let lowered = meadow_seq::lower_program(&program(src), opt);
    assert!(lowered.unsupported.is_empty(), "{:?}", lowered.unsupported);
    meadow_codegen::compile(&lowered.program).expect("codegen")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-aot-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The runtime as a static library for `arch`, as this build of it made it:
/// the host's from `cargo build`, another from `cargo build --target`.
fn runtime(arch: Arch) -> Option<PathBuf> {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = if Some(arch) == Arch::host() {
        target.join(profile)
    } else {
        target.join(triple(arch)?).join(profile)
    };
    fresh(arch, &target);
    Some(dir.join("libmeadow_rts.a")).filter(|lib| lib.exists())
}

/// Bring the static library for `arch` up to date, once per run: `cargo test`
/// builds the crate to link the tests against, not the library a program
/// links. For another architecture, only where one was built before -- that
/// needs its Rust target installed.
fn fresh(arch: Arch, target: &Path) {
    static DONE: std::sync::Mutex<Vec<Arch>> = std::sync::Mutex::new(Vec::new());
    let mut done = DONE.lock().unwrap_or_else(|p| p.into_inner());
    if done.contains(&arch) {
        return;
    }
    done.push(arch);
    let mut cargo = Command::new(env!("CARGO"));
    cargo.args(["build", "--lib", "--manifest-path"]);
    cargo.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    if !cfg!(debug_assertions) {
        cargo.arg("--release");
    }
    if Some(arch) != Arch::host() {
        match triple(arch) {
            Some(t) if target.join(t).is_dir() => {
                cargo.args(["--target", t]);
            }
            _ => return,
        }
    }
    let out = cargo.output().expect("cargo runs");
    assert!(
        out.status.success(),
        "building the runtime library failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The target a macOS runtime for `arch` is built for, where this machine can
/// also run it -- under Rosetta, on Apple silicon.
fn triple(arch: Arch) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(match arch {
        Arch::Aarch64 => "aarch64-apple-darwin",
        Arch::X86_64 => "x86_64-apple-darwin",
    })
}

/// Compile `src` for `arch`, link it, run it, and answer what it printed and
/// whether it succeeded -- or `None` if there is no runtime for `arch` to link.
fn native(name: &str, src: &str, opt: meadow_core::OptLevel, arch: Arch) -> Option<(String, bool)> {
    let runtime = runtime(arch)?;
    let image = image(src, opt);
    let compiled = codegen::compile(&image, arch);
    let bytes = meadow_bytecode::image::encode(&image);
    let dir = scratch(name);
    let obj = dir.join("program.o");
    std::fs::write(
        &obj,
        object::write(&compiled, &bytes, object::Format::host()),
    )
    .unwrap();
    let main = dir.join("main.c");
    std::fs::write(&main, object::main_c()).unwrap();
    let exe = dir.join("program");
    let mut cc = Command::new("cc");
    cc.arg(&main).arg(&obj).arg(runtime).arg("-o").arg(&exe);
    if cfg!(target_os = "macos") {
        let arch = match arch {
            Arch::Aarch64 => "arm64",
            Arch::X86_64 => "x86_64",
        };
        cc.args(["-arch", arch, "-liconv", "-lSystem", "-lc", "-lm"]);
    } else {
        cc.args([
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]);
    }
    let linked = cc.output().expect("a C compiler");
    assert!(
        linked.status.success(),
        "linking failed:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let run = Command::new(&exe).output().expect("the program runs");
    let _ = std::fs::remove_dir_all(&dir);
    let out = if run.status.success() {
        String::from_utf8_lossy(&run.stdout).trim_end().to_string()
    } else {
        String::from_utf8_lossy(&run.stderr).trim_end().to_string()
    };
    Some((out, run.status.success()))
}

/// The native executable answers what the VM does, at every level, for each
/// architecture there is a runtime to link -- the host's always, and x86-64 on
/// Apple silicon once `cargo build --target x86_64-apple-darwin` has made one.
fn agrees(name: &str, src: &str) -> String {
    let mut want = None;
    for arch in [Arch::Aarch64, Arch::X86_64] {
        for opt in [meadow_core::OptLevel::O0, meadow_core::OptLevel::O2] {
            let vm = meadow_rts::run(&image(src, opt), u64::MAX).map_err(|e| e.msg);
            let at = format!("{name}-{arch:?}-{}", opt.name());
            let Some((got, ok)) = native(&at, src, opt, arch) else {
                assert_ne!(Some(arch), Arch::host(), "no runtime for the host to link");
                eprintln!("skipping {at}: no runtime built for it");
                continue;
            };
            let got = if ok { Ok(got) } else { Err(got) };
            // The executable prints nothing for `()`.
            let vm = vm.map(|v| if v == "()" { String::new() } else { v });
            assert_eq!(vm, got, "VM vs native, {arch:?} at {}\n{src}", opt.name());
            want = Some(got.unwrap_or_else(|e| e));
        }
    }
    want.expect("run at least once")
}

#[test]
fn typed_arithmetic_and_branches_run_natively() {
    let src = "fun fib (n : Int) = if n < 2 then n else fib (n - 1) + fib (n - 2)
               fun loop (n : Int) (acc : Float) = if n == 0 then acc else loop (n - 1) (acc +. 1.5)
               fun g (a : Int) (b : Int) = (a / b, a % b, a * b - 7, a >= b, a != b)
               def main = (fib 20, loop 1000 0.0, g 17 5, g (0 - 9223372036854775807 - 1) (0 - 1))";
    assert_eq!(
        agrees("typed", src),
        "(6765, 1500.0, (3, 2, 78, True, True), (-9223372036854775808, 0, 9223372036854775801, False, True))"
    );
}

#[test]
fn everything_else_goes_through_the_interpreter() {
    let src = "use L.*\ndata L = Nil | Cons Int L
               fun build (n : Int) = if n == 0 then Nil else Cons n (build (n - 1))
               fun total xs = match xs with | Nil -> 0 | Cons x r -> x + total r
               fun twice f x = f (f x)
               def main = (total (build 1000), twice (\\x -> x * 3) 7, \"hi\", 1.5 <. 2.5)";
    assert_eq!(agrees("mixed", src), "(500500, 63, \"hi\", True)");
}

#[test]
fn a_failure_is_the_same_failure() {
    let src = "fun f (a : Int) (b : Int) = a / b\ndef main = f 1 0";
    assert_eq!(agrees("fails", src), "division by zero");
}

#[test]
fn threads_run_natively_too() {
    let src = "fun fib (n : Int) = if n < 2 then n else fib (n - 1) + fib (n - 2)
               def main = let t = threadSpawn (\\() -> fib 18) in (fib 17, threadAwait t)";
    assert_eq!(agrees("threads", src), "(1597, 2584)");
}
