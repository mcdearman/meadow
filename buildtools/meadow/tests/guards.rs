//! `match` arms with guards, and `as`-patterns: what they mean, on every
//! machine, at every optimization level -- and what the checker says of them.

mod common;
use meadow::{Engine, OptLevel, Options, pipeline, runtime};

/// The CEK machine's answer, required of the VM and its JIT at every level
/// and of a release build (whose `match` arms become a switch).
fn agreed(src: &str) -> String {
    let (program, diags) = pipeline::compile_str_with_std("test", src, Options::debug());
    assert!(
        diags.is_empty(),
        "compile errors in\n{src}\n{}",
        diags
            .iter()
            .map(|d| d.msg.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let cek = runtime::run(&program, Engine::Cek, OptLevel::O1)
        .unwrap_or_else(|e| panic!("the CEK machine failed on\n{src}\n{e}"));
    for engine in [Engine::Vm, Engine::Jit] {
        for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let got = runtime::run(&program, engine, opt)
                .unwrap_or_else(|e| panic!("{engine:?} at {} failed: {e}", opt.name()));
            assert_eq!(got, cek, "{engine:?} at {} on\n{src}", opt.name());
        }
    }
    let (release, diags) = pipeline::compile_str_with_std("test", src, Options::release());
    assert!(
        diags.is_empty(),
        "release: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let got = runtime::run(&release, Engine::Jit, OptLevel::O2).unwrap();
    assert_eq!(got, cek, "a release build on\n{src}");
    cek
}

fn is(src: &str, expected: &str) {
    assert_eq!(agreed(src), expected, "{src}");
}

#[test]
fn a_guard_picks_among_arms_that_match() {
    is(
        "fun classify n = match n with\n\
         | x if x < 0 -> \"negative\"\n\
         | 0 -> \"zero\"\n\
         | x if x > 100 -> \"big\"\n\
         | _ -> \"positive\"\n\
         def main = (classify (0 - 5), classify 0, classify 500, classify 7)\n",
        r#"("negative", "zero", "big", "positive")"#,
    );
}

/// A guard that fails passes to the next arm even when the arm is a
/// constructor a release build switches on -- including another arm for the
/// same constructor.
#[test]
fn a_failed_guard_tries_the_arms_after_it() {
    is(
        "data Shape = Circle Int | Rect Int Int | Dot\n\
         use Shape.*\n\
         fun area s = match s with\n\
         | Circle r if r > 10 -> 999\n\
         | Circle r -> 3 * r * r\n\
         | Rect w h if w == h -> w * w\n\
         | Rect w h -> w * h\n\
         | Dot -> 0\n\
         def main = (area (Circle 20), area (Circle 2), area (Rect 3 3), area (Rect 2 5), area Dot)\n",
        "(999, 12, 9, 10, 0)",
    );
    // Guarded arms for distinct constructors, which a release build puts in one
    // switch, and the arms their guards fall back to after it.
    is(
        "data Shape = Circle Int | Rect Int Int | Dot\n\
         use Shape.*\n\
         fun area s = match s with\n\
         | Circle r if r > 10 -> 999\n\
         | Rect w h if w == h -> w * w\n\
         | Dot -> 0\n\
         | Circle r -> 3 * r * r\n\
         | Rect w h -> w * h\n\
         def main = (area (Circle 20), area (Circle 2), area (Rect 3 3), area (Rect 2 5), area Dot)\n",
        "(999, 12, 9, 10, 0)",
    );
    is(
        "fun firstEven xs = match xs with\n\
         | x :: _ if x % 2 == 0 -> Just x\n\
         | _ :: rest -> firstEven rest\n\
         | [;] -> None\n\
         def main = (firstEven [1; 3; 8; 10], firstEven [1; 3])\n",
        "(Just(8), None)",
    );
}

/// A guard is an expression like any other: it can call a function, and run
/// an effect, and only the guards of arms whose patterns matched are run.
#[test]
fn a_guard_can_call_and_perform() {
    is(
        "fun isBig n = n > 10\n\
         fun f n = match n with\n\
         | x if isBig x -> \"big\"\n\
         | _ -> \"small\"\n\
         def main = (f 20, f 3)\n",
        r#"("big", "small")"#,
    );
    is(
        "use Std.St as St\n\
         fun count n = runSt (\\() ->\n\
           let seen = St.newRef 0 in\n\
           let checks x = let _ = St.modifyRef seen (\\c -> c + 1) in x > 2 in\n\
           let r = match n with\n\
             | Just x if checks x -> x\n\
             | Just x if checks (x + 10) -> 0 - x\n\
             | _ -> 0 in\n\
           (r, St.getRef seen))\n\
         def main = (count (Just 5), count (Just 1), count None)\n",
        "((5, 1), (-1, 2), (0, 0))",
    );
}

#[test]
fn an_as_pattern_names_the_whole_of_what_it_matched() {
    is(
        "fun dup xs = match xs with\n\
         | x :: rest as whole -> (x, whole, rest)\n\
         | [;] as whole -> (0, whole, whole)\n\
         def main = (dup [1; 2; 3], dup [;])\n",
        "((1, [1; 2; 3], [2; 3]), (0, [], []))",
    );
    is(
        "fun f p = match p with\n\
         | ((a, b) as inner, c) if a + b == c -> (inner, True)\n\
         | (inner, _) -> (inner, False)\n\
         def main = (f ((1, 2), 3), f ((1, 2), 4))\n",
        "(((1, 2), True), ((1, 2), False))",
    );
    // Deepest name innermost, and a name for a part and the whole at once.
    is(
        "fun g xs = match xs with\n\
         | (x as y) :: _ as all -> (x + y, all)\n\
         | _ -> (0, xs)\n\
         def main = g [4; 5]\n",
        "(8, [4; 5])",
    );
    // In a `let` too, where a pattern binds.
    is(
        "def main = let (a, b) as pair = (1, 2) in (a + b, pair)\n",
        "(3, (1, 2))",
    );
}

#[test]
fn a_guarded_arm_covers_nothing() {
    let src = "fun f n = match n with\n| x if x > 0 -> 1\n| 0 -> 0\ndef main = f 1\n";
    let errors = common::errors_std_with(src, Options::release());
    assert!(errors.contains("non-exhaustive"), "{errors}");
    // With an arm after it that does cover, it is fine.
    let src = "fun f n = match n with\n| x if x > 0 -> 1\n| _ -> 0\ndef main = f 1\n";
    assert_eq!(common::errors_std_with(src, Options::release()), "");
}

#[test]
fn a_guard_is_a_bool_that_sees_the_pattern() {
    let errors = common::errors_std_with(
        "fun f n = match n with\n| x if x -> 1\n| _ -> 0\ndef main = f 1\n",
        Options::debug(),
    );
    assert!(errors.contains("Bool"), "{errors}");
    // The pattern's names are in scope in the guard, and not after the match.
    let errors = common::errors_std_with(
        "fun f n = match n with\n| x if y > 0 -> 1\n| _ -> 0\ndef main = f 1\n",
        Options::debug(),
    );
    assert!(errors.contains("undefined variable: y"), "{errors}");
}
