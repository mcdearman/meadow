//! Programs compiled by `meadow-llvm`, linked with Silo, run --
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
        let aot = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../silo");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--quiet", "--manifest-path"])
            .arg(aot.join("Cargo.toml"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building Silo failed");
        let lib = if cfg!(windows) {
            "meadow_silo.lib"
        } else {
            "libmeadow_silo.a"
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
/// segment's frames held is not erased (see `silo/src/segments.rs`), so it
/// is not checked for leaks.
#[track_caller]
fn run_abandoning(name: &str, src: &str) -> String {
    run_with(name, src, true, true)
}

#[track_caller]
fn run_with(name: &str, src: &str, check: bool, abandons: bool) -> String {
    run_full(name, src, check, abandons).0
}

/// [`run`], and how many blocks the program acquired: what reuse saves.
#[track_caller]
fn run_counting(name: &str, src: &str) -> (String, u64) {
    let (got, stderr) = run_full(name, src, true, false);
    let acquired = stderr
        .lines()
        .find_map(|l| {
            l.strip_prefix("aot: ")?
                .strip_suffix(" blocks acquired")?
                .parse()
                .ok()
        })
        .expect("the run says how many blocks it acquired");
    (got, acquired)
}

#[track_caller]
fn run_full(name: &str, src: &str, check: bool, abandons: bool) -> (String, String) {
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
        .env("MEADOW_SILO_LEAKS", "1")
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
    (got, stderr)
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

// --- cycles ------------------------------------------------------------------
//
// Counting alone leaves a cycle behind, so the runtime collects them by trial
// deletion (`silo/src/cycles.rs`). The leak check in `run` is what these are
// really testing: a program that ties knots and drops them must end with
// nothing live.

#[test]
fn a_cycle_through_a_ref_is_collected() {
    assert_eq!(
        run_checked(
            "cycle_ref",
            "data L = Nil | Node Int (Ref L)
             use L.*
             fun knot (n : Int) =
               let r = newRef Nil in
               let _ = setRef r (Node n r) in
               ()
             fun many (n : Int) : Int ! Mut = if n == 0 then 0 else let _ = knot n in many (n - 1)
             def main = many 2000",
            false
        ),
        "0"
    );
}

#[test]
fn a_longer_cycle_is_collected() {
    // Two cells and two nodes to a knot, so the collector has a subgraph to
    // walk rather than a self-reference.
    assert_eq!(
        run_checked(
            "cycle_long",
            "data L = Nil | Node Int (Ref L)
             use L.*
             fun knot (n : Int) =
               let a = newRef Nil in
               let b = newRef Nil in
               let _ = setRef a (Node n b) in
               let _ = setRef b (Node n a) in
               ()
             fun many (n : Int) : Int ! Mut = if n == 0 then 0 else let _ = knot n in many (n - 1)
             def main = many 2000",
            false
        ),
        "0"
    );
}

#[test]
fn a_cycle_still_in_use_is_kept() {
    // The knot is held across thousands of allocations, which is long enough
    // for the collector to have run over it several times. If it collected
    // what is still reachable, this reads freed memory or answers wrongly.
    assert_eq!(
        run_checked(
            "cycle_live",
            "data L = Nil | Node Int (Ref L)
             use L.*
             fun churn (n : Int) (acc : Int) : Int ! Mut =
               if n == 0 then acc
               else
                 let r = newRef Nil in
                 let _ = setRef r (Node n r) in
                 churn (n - 1) (acc + 1)
             fun step (r : Ref L) : Int ! Mut =
               match getRef r with
               | Nil -> 0
               | Node v back -> match getRef back with | Nil -> v | Node w _ -> v + w
             def main =
               let keep = newRef Nil in
               let _ = setRef keep (Node 7 keep) in
               let n = churn 5000 0 in
               n + step keep",
            false
        ),
        "5014"
    );
}

// --- reuse ---------------------------------------------------------------------

const LIST: &str = "use L.*
     data L = Nil | Cons Int L
     fun build (n : Int) (acc : L) : L = if n == 0 then acc else build (n - 1) (Cons n acc)
     fun inc (xs : L) : L = match xs with | Nil -> Nil | Cons x rest -> Cons (x + 1) (inc rest)
     fun total (xs : L) : Int = match xs with | Nil -> 0 | Cons x rest -> x + total rest
";

#[test]
fn a_list_nobody_else_holds_is_rebuilt_in_place() {
    // Each `inc` takes a cell apart and builds one of the same size: with the
    // list its only reference, it builds in the cell it took apart. Two
    // passes over a thousand cells acquire none beyond the thousand built.
    let (got, acquired) = run_counting(
        "reuse-map",
        &format!("{LIST} def main = total (inc (inc (build 1000 Nil)))"),
    );
    assert_eq!(got, "502500");
    assert!(
        acquired < 1100,
        "{acquired} blocks acquired: the passes did not reuse"
    );
}

#[test]
fn a_list_still_held_elsewhere_is_copied() {
    // Shared, the cells are not `inc`'s to reuse: it copies, and the list it
    // was given is still whole for the second `total`.
    let (got, acquired) = run_counting(
        "reuse-shared",
        &format!("{LIST} def main = let xs = build 1000 Nil in total (inc xs) + total xs"),
    );
    assert_eq!(got, "1002000");
    assert!(
        acquired >= 2000,
        "{acquired}: a shared list cannot have been reused"
    );
}

#[test]
fn a_path_that_builds_nothing_gives_the_block_back() {
    // `keep` builds a cell on one path and none on the other: the cells it
    // drops go back to the runtime, and nothing leaks.
    let (got, _) = run_counting(
        "reuse-filter",
        &format!(
            "{LIST} fun keep (xs : L) : L = match xs with | Nil -> Nil | Cons x rest -> if x % 2 == 0 then Cons x (keep rest) else keep rest
             def main = total (keep (build 1000 Nil))"
        ),
    );
    assert_eq!(got, "250500");
}

#[test]
fn a_block_is_rebuilt_after_the_calls_it_waits_on() {
    // `bump` takes a node apart, recurses into both sides -- two calls that
    // are not tail calls -- and only then builds the node again. The block
    // it took apart waits in the frames of those calls, and the node is built
    // in it when they return.
    let (got, acquired) = run_counting(
        "reuse-across-calls",
        "use T.*
         data T = Tip | Node T Int T
         fun build (d : Int) : T = if d == 0 then Tip else Node (build (d - 1)) d (build (d - 1))
         fun bump (t : T) : T = match t with | Tip -> Tip | Node l x r -> let l2 = bump l in let r2 = bump r in Node l2 (x + 1) r2
         fun sum (t : T) : Int = match t with | Tip -> 0 | Node l x r -> sum l + x + sum r
         def main = sum (bump (bump (build 10)))",
    );
    assert_eq!(got, "4082");
    assert!(
        acquired < 1100,
        "{acquired} blocks acquired for a tree of 1023: the rebuilds did not reuse"
    );
}

#[test]
fn nested_patterns_rebuild_in_what_they_took_apart() {
    // Koka's red-black insertion (its `rbtree` benchmark), with the nested
    // patterns it is written with. The matches are decision trees, so each
    // `switch` consumes what it takes apart and the nodes a rotation builds
    // are the nodes it matched; a chain of failure objects would have held
    // every scrutinee while the arm ran, and nothing could have been reused.
    // `balanceLeft` is inlined into `ins`, its one caller, so a rotation
    // builds in the node `ins` took apart as well as the two it matched; and
    // where `ins` keeps `t` for the path that returns it, the other paths drop
    // it into a token. Two thousand insertions copy a path of a dozen nodes
    // each without reuse -- over forty thousand blocks -- and with it build
    // nothing but the two thousand leaves.
    let (got, acquired) = run_counting(
        "reuse-rbtree",
        "use Color.*
         use Tree.*
         data Color = Red | Black
         data Tree = Leaf | Node Color Tree Int Tree
         fun isRed (t : Tree) : Bool = match t with | Node Red _ _ _ -> True | _ -> False
         fun balanceLeft (l : Tree) (k : Int) (r : Tree) : Tree = match l with | Node _ (Node Red a x b) y c -> Node Red (Node Black a x b) y (Node Black c k r) | Node _ a x (Node Red b y c) -> Node Red (Node Black a x b) y (Node Black c k r) | Node _ a x b -> Node Black (Node Red a x b) k r | Leaf -> Leaf
         fun balanceRight (l : Tree) (k : Int) (r : Tree) : Tree = match r with | Node _ (Node Red a x b) y c -> Node Red (Node Black l k a) x (Node Black b y c) | Node _ a x (Node Red b y c) -> Node Red (Node Black l k a) x (Node Black b y c) | Node _ a x b -> Node Black l k (Node Red a x b) | Leaf -> Leaf
         fun ins (t : Tree) (k : Int) : Tree = match t with | Node Red l x r -> (if k < x then Node Red (ins l k) x r else if k > x then Node Red l x (ins r k) else t) | Node Black l x r -> (if k < x then (if isRed l then balanceLeft (ins l k) x r else Node Black (ins l k) x r) else if k > x then (if isRed r then balanceRight l x (ins r k) else Node Black l x (ins r k)) else t) | Leaf -> Node Red Leaf k Leaf
         fun insert (t : Tree) (k : Int) : Tree = match ins t k with | Node _ l x r -> Node Black l x r | Leaf -> Leaf
         fun make (n : Int) (t : Tree) : Tree = if n == 0 then t else make (n - 1) (insert t n)
         fun size (t : Tree) : Int = match t with | Leaf -> 0 | Node _ l _ r -> size l + 1 + size r
         def main = size (make 2000 Leaf)",
    );
    assert_eq!(got, "2000");
    assert!(
        acquired < 2_100,
        "{acquired} blocks acquired for 2000 insertions: the paths were copied"
    );
}

#[test]
fn an_arm_can_test_a_value_and_keep_it_whole() {
    // The second arm takes `xs` apart two cells deep and then uses all of
    // it; the first tests a literal in the same place. As a decision tree
    // the switch on `xs` keeps it in the arm that wants it and gives it up
    // in the others.
    let (got, _) = run_counting(
        "keep-arm",
        &format!(
            "{LIST} fun pick (xs : L) : Int = match xs with | Cons 0 rest -> total rest | Cons x (Cons y _) -> x * 100 + y + total xs | ys -> total ys
             def main = pick (Cons 0 (build 3 Nil)) + pick (build 4 Nil) + pick (Cons 7 Nil) + pick Nil"
        ),
    );
    assert_eq!(got, "125");
}

#[test]
fn a_nested_pattern_on_an_inlined_copy_counts_what_it_takes_apart() {
    // `first` is generic, so a release build copies it at `#Ref` and inlines
    // the copy: the `match` sees a `Opt #Ref`, which says nothing of what the
    // `Some` holds. The `Computed` there is still `xs`'s too, so the arm shares
    // the `Memo` inside it -- which it could only do knowing the `Memo` is a
    // reference, from `Computed`'s declaration. It was left unknown, the
    // share counted nothing, and the `Memo` was taken apart as the only
    // reference to it: its `deps` went with it while `xs` still held them,
    // and the second `match` read freed memory.
    let got = run(
        "nested-inlined-copy",
        &format!(
            "{LIST} use Opt.*
             use E.*
             use M.*
             use Ps.*
             data Opt a = None | Some a
             data M = Memo Int L
             data E = Computed M | Running Int
             data Ps a = PNil | PCons a (Ps a)
             fun first (xs : Ps a) : Opt a = match xs with | PCons x _ -> Some x | PNil -> None
             fun read (xs : Ps E) : Int = match first xs with | Some (Computed (Memo n deps)) -> n + total deps | _ -> 0
             def main = let xs = PCons (Computed (Memo 1 (build 3 Nil))) PNil in read xs * 100 + read xs"
        ),
    );
    assert_eq!(got, "707");
}
