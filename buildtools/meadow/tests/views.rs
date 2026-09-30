//! View patterns, `(f -> p)`: `f` applied to what is matched there, and its
//! answer matched against `p` -- what they mean, on every machine, at every
//! optimization level, and what the checker says of them.

mod common;
use meadow::{Engine, OptLevel, Options, pipeline, runtime};

/// The CEK machine's answer, required of the VM and its JIT at every level
/// and of a release build.
fn agreed(src: &str) -> String {
    let (program, diags) =
        pipeline::compile_str_with_std("test", src, Options::debug().entry("result"));
    assert!(
        diags.is_empty(),
        "compile errors in\n{src}\n{}",
        diags
            .iter()
            .map(|d| d.msg.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let program = meadow_compiler::core::prune::prune(&program);
    let cek = runtime::run(&program, Engine::Cek, OptLevel::O1)
        .unwrap_or_else(|e| panic!("the CEK machine failed on\n{src}\n{e}"));
    for engine in [Engine::Vm, Engine::Jit] {
        for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let got = runtime::run(&program, engine, opt)
                .unwrap_or_else(|e| panic!("{engine:?} at {} failed: {e}", opt.name()));
            assert_eq!(got, cek, "{engine:?} at {} on\n{src}", opt.name());
        }
    }
    let (release, diags) =
        pipeline::compile_str_with_std("test", src, Options::release().entry("result"));
    assert!(
        diags.is_empty(),
        "release: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let release = meadow_compiler::core::prune::prune(&release);
    let got = runtime::run(&release, Engine::Jit, OptLevel::O2).unwrap();
    assert_eq!(got, cek, "a release build on\n{src}");
    cek
}

fn is(src: &str, expected: &str) {
    assert_eq!(agreed(src), expected, "{src}");
}

const SHAPES: &str = "data Shape = Circle Int | Rect Int Int | Dot\n\
     use Shape.*\n\
     fun square s = match s with\n\
     | Rect w h if w == h -> Just w\n\
     | _ -> None\n";

#[test]
fn a_view_matches_what_its_function_answers() {
    is(
        &format!(
            "{SHAPES}\
             fun describe s = match s with\n\
             | (square -> Just w) -> w * 100\n\
             | Rect w h -> w + h\n\
             | Circle r -> r\n\
             | Dot -> 0\n\
             def result = (describe (Rect 3 3), describe (Rect 2 5), describe (Circle 7), describe Dot)\n"
        ),
        "(300, 7, 7, 0)",
    );
}

/// A view that says no goes on to the arms after it -- a guard's fall
/// through, with the arms before it matched as they were.
#[test]
fn a_view_that_does_not_match_tries_the_arms_after_it() {
    is(
        "fun half n = if n % 2 == 0 then Just (n / 2) else None\n\
         fun f n = match n with\n\
         | 0 -> \"zero\"\n\
         | (half -> Just 1) -> \"two\"\n\
         | (half -> Just h) if h > 10 -> \"big and even\"\n\
         | (half -> Just _) -> \"even\"\n\
         | _ -> \"odd\"\n\
         def result = (f 0, f 2, f 30, f 4, f 5)\n",
        r#"("zero", "two", "big and even", "even", "odd")"#,
    );
}

#[test]
fn views_nest_and_see_what_is_bound_to_their_left() {
    // Inside a constructor, inside another view, and applied to a name the
    // pattern bound before it.
    is(
        "fun lookup k xs = match xs with\n\
         | [;] -> None\n\
         | (j, v) :: rest -> if j == k then Just v else lookup k rest\n\
         def table = [(1, \"one\"); (2, \"two\")]\n\
         fun name p = match p with\n\
         | (k, (lookup k -> Just n)) -> n\n\
         | _ -> \"?\"\n\
         fun both p = match p with\n\
         | Just ((\\i -> i * 2) -> k, (lookup k -> Just n)) -> n\n\
         | _ -> \"none\"\n\
         def result = (name (1, table), name (3, table), both (Just (1, table)), both None)\n",
        r#"("one", "?", "two", "none")"#,
    );
}

#[test]
fn a_view_is_a_parameter_and_a_binding() {
    is(
        "fun swap (a, b) = (b, a)\n\
         fun first (swap -> (_, a)) = a\n\
         def result =\n\
           let (swap -> (x, y)) = (1, 2) in\n\
           let g = \\(swap -> (p, _)) -> p in\n\
           (first (3, 4), x, y, g (5, 6))\n",
        "(3, 2, 1, 6)",
    );
}

#[test]
fn a_function_of_equations_can_match_through_a_view() {
    is(
        "fun half n = if n % 2 == 0 then Just (n / 2) else None\n\
         fun steps 1 acc = acc\n\
           | steps (half -> Just h) acc = steps h (acc + 1)\n\
           | steps n acc = steps (3 * n + 1) (acc + 1)\n\
         def result = steps 6 0\n",
        "8",
    );
}

/// The view is applied once for the whole pattern under it, however many
/// names that binds.
#[test]
fn a_view_is_applied_once() {
    is(
        "def result =\n\
           let n = newRef 0 in\n\
           let count = \\p -> let _ = setRef n (getRef n + 1) in p in\n\
           let (count -> (a, b, c)) = (1, 2, 3) in\n\
           (a + b + c, getRef n)\n",
        "(6, 1)",
    );
}

#[test]
fn a_view_whose_pattern_matches_everything_covers_everything() {
    let src = "fun double n = n * 2\n\
               fun f n = match n with\n\
               | (double -> m) -> m\n\
               def result = f 4\n";
    assert_eq!(
        common::errors_std_with(src, Options::release().entry("result")),
        ""
    );
    assert_eq!(agreed(src), "8");
}

#[test]
fn a_view_that_may_not_match_covers_nothing() {
    let src = "fun half n = if n % 2 == 0 then Just (n / 2) else None\n\
               fun f n = match n with\n\
               | (half -> Just h) -> h\n\
               | (half -> None) -> 0\n\
               def result = f 4\n";
    let errors = common::errors_std_with(src, Options::release().entry("result"));
    assert!(errors.contains("non-exhaustive"), "{errors}");
    // Nor can a parameter be one.
    let src = "fun half n = if n % 2 == 0 then Just (n / 2) else None\n\
               fun f (half -> Just h) = h\n\
               def result = f 4\n";
    let errors = common::errors_std_with(src, Options::debug().entry("result"));
    assert!(errors.contains("refutable pattern"), "{errors}");
}

#[test]
fn a_view_is_typed_as_a_function_of_what_it_matches() {
    let errors = common::errors_std_with(
        "fun len (s : String) = 0\n\
         fun f n = match n with\n\
         | (len -> 0) -> n + 1\n\
         | _ -> 0\n\
         def result = f 1\n",
        Options::debug().entry("result"),
    );
    assert!(errors.contains("String"), "{errors}");
    let errors = common::errors_std_with(
        "fun f n = match n with\n\
         | (\\x -> x + 1) -> 0\n\
         | _ -> 0\n\
         def result = f 1\n",
        Options::debug().entry("result"),
    );
    assert!(
        !errors.is_empty(),
        "a lambda is not a pattern, and has no `->` after it"
    );
}

/// What a view in a parameter does is done when the function is called, so it
/// is on the function's arrow, not the definition.
#[test]
fn a_view_in_a_parameter_does_what_the_function_does() {
    let schemes = common::schemes_std(
        "use Std.Console (println)\n\
         fun shout (s : String) : String ! { Console } = let _ = println s in s\n\
         fun greet (shout -> s) = s\n",
    );
    assert!(
        schemes
            .lines()
            .any(|l| l.starts_with("greet") && l.contains("Console")),
        "{schemes}"
    );
}
