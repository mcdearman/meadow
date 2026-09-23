//! Programs compiled by `meadow-llvm`, linked with the `aot` runtime, run --
//! and checked against the AxCut abstract machine, which is the meaning they
//! must keep, and for leaks: a program with no mutable state must end with
//! every block it acquired given back.
//!
//! Needs clang, and builds the runtime library (`aot/`) the first time.

use meadow_compiler::{compile_str, core};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn program(src: &str) -> core::Program {
    let (pkg, diags) = compile_str("native", src);
    let hard: Vec<_> = diags.iter().map(|d| d.msg.clone()).collect();
    assert!(hard.is_empty(), "compile errors:\n{}", hard.join("\n"));
    let entry = pkg
        .exports
        .iter()
        .find(|e| &*e.name == "main")
        .map(|e| e.var);
    assert!(entry.is_some(), "the case has no `main`");
    core::Program {
        defs: pkg.defs.clone(),
        entry,
        ctor_fields: pkg.ctor_fields.clone(),
        variants: pkg.variants.clone(),
        origins: Default::default(),
    }
}

/// The runtime library, built once.
fn runtime() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let aot = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../aot");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--quiet", "--manifest-path"])
            .arg(aot.join("Cargo.toml"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the aot runtime failed");
        let lib = if cfg!(windows) {
            "meadow_aot.lib"
        } else {
            "libmeadow_aot.a"
        };
        aot.join("target").join("release").join(lib)
    })
}

fn system_libs() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "-lkernel32",
            "-lntdll",
            "-luserenv",
            "-lws2_32",
            "-ldbghelp",
            "-lbcrypt",
            "-ladvapi32",
        ]
    } else if cfg!(target_os = "macos") {
        &["-liconv", "-lSystem"]
    } else {
        &["-lpthread", "-ldl", "-lm"]
    }
}

/// Compile `src` natively, run it, and answer its output and whether it
/// succeeded; and check it against the AxCut machine and for leaks.
#[track_caller]
fn run(name: &str, src: &str) -> String {
    run_checked(name, src, true)
}

#[track_caller]
fn run_checked(name: &str, src: &str, check: bool) -> String {
    run_with(name, src, check, false)
}

/// [`run`], for a program that abandons a continuation: what the abandoned
/// segment's frames held is not erased (see `aot/src/segments.rs`), so it
/// is not checked for leaks.
#[track_caller]
fn run_abandoning(name: &str, src: &str) -> String {
    run_with(name, src, true, true)
}

#[track_caller]
fn run_with(name: &str, src: &str, check: bool, abandons: bool) -> String {
    let prog = program(src);
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::O2);
    assert!(lowered.unsupported.is_empty(), "{:?}", lowered.unsupported);
    let want = check.then(|| {
        meadow_seq::machine::Machine::run(&lowered.program, 50_000_000)
            .unwrap_or_else(|e| panic!("the AxCut machine failed: {}", e.msg))
            .to_string()
    });
    let ll = meadow_llvm::compile(&lowered.program).unwrap_or_else(|e| panic!("{}", e.msg));
    let dir = std::env::temp_dir().join("meadow-llvm-tests").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let ll_path = dir.join("prog.ll");
    std::fs::write(&ll_path, &ll).unwrap();
    let exe = dir.join(if cfg!(windows) { "prog.exe" } else { "prog" });
    let out = Command::new("clang")
        .args(["-O2", "-Wno-override-module", "-o"])
        .arg(&exe)
        .arg(&ll_path)
        .arg(runtime())
        .args(system_libs())
        .output()
        .expect("clang runs");
    assert!(
        out.status.success(),
        "clang failed on {}:\n{}",
        ll_path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let ran = Command::new(&exe)
        .env("MEADOW_AOT_LEAKS", "1")
        .output()
        .expect("the program runs");
    let stdout = String::from_utf8_lossy(&ran.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&ran.stderr).to_string();
    assert!(
        ran.status.success(),
        "{name} failed: {stderr}\n(see {})",
        ll_path.display()
    );
    let got = if stdout.is_empty() {
        "()".to_string()
    } else {
        stdout
    };
    if let Some(want) = want {
        assert_eq!(got, want, "native vs the AxCut machine, {name}");
    }
    assert!(
        abandons || stderr.contains("aot: 0 blocks live at exit"),
        "{name} leaked: {stderr}"
    );
    got
}

#[test]
fn arithmetic_and_comparison() {
    assert_eq!(
        run(
            "arith",
            "def main = (1 + 2 * 3, 10 - 4, 7 / 2, 7 % 3, 2.5 +. 1.0, 3 < 4, 'a' == 'b')"
        ),
        "(7, 6, 3, 1, 3.5, True, False)"
    );
}

#[test]
fn recursion_on_the_native_stack() {
    assert_eq!(
        run(
            "fib",
            "fun fib (n : Int) : Int = if n < 2 then n else fib (n - 1) + fib (n - 2)
             def main = fib 25"
        ),
        "75025"
    );
}

#[test]
fn data_and_matching() {
    assert_eq!(
        run(
            "data",
            "use L.*
             data L = Nil | Cons Int L
             fun build (n : Int) : L = if n == 0 then Nil else Cons n (build (n - 1))
             fun total (xs : L) : Int = match xs with | Nil -> 0 | Cons x r -> x + total r
             fun len (xs : L) : Int = match xs with | Nil -> 0 | Cons _ r -> 1 + len r
             def main = let xs = build 1000 in (total xs, len xs)"
        ),
        "(500500, 1000)"
    );
}

#[test]
fn closures_and_higher_order() {
    assert_eq!(
        run(
            "closures",
            "fun twice f x = f (f x)
             fun compose f g x = f (g x)
             def main =
               let k = 10 in
               let add = \\x -> x + k in
               (twice add 1, compose (\\x -> x * 2) add 5, twice (twice (\\x -> x + 1)) 0)"
        ),
        "(21, 30, 4)"
    );
}

#[test]
fn deep_non_tail_recursion() {
    assert_eq!(
        run(
            "deep",
            "use L.*
             data L = Nil | Cons Int L
             fun build (n : Int) (acc : L) : L = if n == 0 then acc else build (n - 1) (Cons n acc)
             fun total (xs : L) : Int = match xs with | Nil -> 0 | Cons x r -> x + total r
             def main = total (build 1000000 Nil)"
        ),
        "500000500000"
    );
}

/// Not a test: how long `fib 32` takes natively, for comparing with the other
/// backends. `cargo test --release -p meadow-llvm --test native -- --ignored`.
#[test]
#[ignore]
fn time_fib() {
    let started = std::time::Instant::now();
    run_checked(
        "timefib",
        "fun fib (n : Int) : Int = if n < 2 then n else fib (n - 1) + fib (n - 2)
         def main = fib 32",
        false,
    );
    eprintln!("fib 32, compiled and run: {:?}", started.elapsed());
}

// --- effects: stack segments ---------------------------------------------

#[test]
fn a_handler_that_never_resumes_aborts_the_body() {
    assert_eq!(
        run_abandoning(
            "abort",
            "effect Abort { bail : () -> Int }
             def main = handle 1 + bail () with { bail u k -> 99 }"
        ),
        "99"
    );
}

#[test]
fn a_handler_that_resumes_continues_the_body_in_place() {
    assert_eq!(
        run(
            "resume",
            "effect Ask { ask : () -> Int }
             def main = handle ask () + 1 with { ask u k -> k 5 + 100 }"
        ),
        "106"
    );
}

#[test]
fn handlers_are_deep() {
    assert_eq!(
        run(
            "deep_handler",
            "effect Ask { ask : () -> Int }
             fun twice u = ask () + ask ()
             def main = (handle ask () + ask () with { ask u k -> k 5 },
                         handle twice () with { ask u k -> k 3 })"
        ),
        "(10, 6)"
    );
}

#[test]
fn state_by_hand_is_a_handler_returning_a_function() {
    assert_eq!(
        run(
            "state",
            "effect St { get : () -> Int, put : Int -> () }
             def main =
               let f =
                 handle
                   let a = get () in
                   let u = put (a + 1) in
                   let b = get () in
                   a + b
                 with {
                   get u k -> \\s -> (k s) s,
                   put v k -> \\s -> (k ()) v,
                   return x -> \\s -> x
                 }
               in f 10"
        ),
        "21"
    );
}

#[test]
fn nested_handlers_and_performing_outwards() {
    assert_eq!(
        run(
            "nested",
            "effect Ask { ask : () -> Int }
             def main = handle (handle ask () with { ask u k -> k 1 }) + ask () with { ask u k -> k 100 }"
        ),
        "101"
    );
    assert_eq!(
        run(
            "outwards",
            "effect Yield { yield : Int -> () }
             data L = Nil | Cons Int L
             use L.*
             fun range (lo : Int) (hi : Int) = if lo >= hi then () else let _ = yield lo in range (lo + 1) hi
             fun map f producer =
               handle producer () with { yield x k -> let _ = yield (f x) in k (), return r -> () }
             fun toList producer =
               handle producer () with { yield x k -> Cons x (k ()), return r -> Nil }
             def main = toList (\\() -> map (\\x -> x * 2) (\\() -> range 0 4))"
        ),
        "Cons(0, Cons(2, Cons(4, Cons(6, Nil))))"
    );
}

// --- threads -----------------------------------------------------------------
//
// The AxCut machine has no scheduler, so these are checked by their answers.

#[test]
fn threads_answer_through_await() {
    assert_eq!(
        run_checked(
            "spawn",
            "fun fib (n : Int) : Int = if n < 2 then n else fib (n - 1) + fib (n - 2)
             def main =
               let a = threadSpawn (\\() -> fib 15) in
               let b = threadSpawn (\\() -> (fib 16, 1)) in
               (threadAwait a, threadAwait b, threadAwait a)",
            false
        ),
        "(610, (987, 1), 610)"
    );
}

#[test]
fn a_receiver_waits_for_a_sender() {
    assert_eq!(
        run_checked(
            "channel",
            "def main =
               let ch = channelNew () in
               let reader = threadSpawn (\\() -> channelReceive ch + channelReceive ch) in
               let _ = threadYield () in
               let _ = channelSend ch (toInt 20) in
               let _ = channelSend ch 22 in
               threadAwait reader",
            false
        ),
        "42"
    );
}

#[test]
fn a_thread_waits_inside_a_handler() {
    assert_eq!(
        run_checked(
            "wait_in_handler",
            "effect Ask { ask : () -> Int }
             def main =
               let ch = channelNew () in
               let t = threadSpawn (\\() ->
                 handle (let x = channelReceive ch in x + ask ()) with { ask u k -> k 1 + 100 }) in
               let _ = threadYield () in
               let _ = channelSend ch (toInt 41) in
               threadAwait t",
            false
        ),
        "142"
    );
}
