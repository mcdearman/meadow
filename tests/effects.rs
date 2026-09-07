//! Algebraic effects: `effect` declarations, effect inference, and `handle`.

mod common;
use common::{errors, eval_main, schemes};

#[test]
fn operation_carries_its_effect() {
    insta::assert_snapshot!(schemes(
        "effect State s { get : () -> s, put : s -> () }\n\
         fun tick u = let n = get () in let x = put (n + 1) in n\n"
    ));
}

#[test]
fn handler_discharges_the_effect() {
    insta::assert_snapshot!(schemes(
        "effect State s { get : () -> s, put : s -> () }\n\
         fun runState s0 act =\n\
           handle act () with {\n\
             get g k -> \\s -> (k s) s,\n\
             put s2 k -> \\s -> (k ()) s2,\n\
             return x -> \\s -> (x, s)\n\
           } s0\n"
    ));
}

#[test]
fn state_effect_runs() {
    // two ticks then a get: state threads 0 -> 1 -> 2, result is (2, 2)
    insta::assert_snapshot!(eval_main(
        "effect State s { get : () -> s, put : s -> () }\n\
         fun tick u = let n = get () in let x = put (n + 1) in n\n\
         fun runState s0 act =\n\
           handle act () with {\n\
             get g k -> \\s -> (k s) s,\n\
             put s2 k -> \\s -> (k ()) s2,\n\
             return x -> \\s -> (x, s)\n\
           } s0\n\
         def main = runState 0 (\\u -> let a = tick () in let b = tick () in get ())\n"
    ));
}

#[test]
fn effect_polymorphism_through_map() {
    // `map` over an effectful function keeps that function's effect
    insta::assert_snapshot!(schemes(
        "effect Log { log : String -> () }\n\
         fun map f xs = match xs with | Nil -> Nil | Cons x r -> Cons (f x) (map f r)\n\
         fun logAll xs = map (\\x -> log x) xs\n"
    ));
}

#[test]
fn saturated_curried_call_keeps_outer_arrow_pure() {
    // `foldl` applies its function argument with two args in one call (`f acc x`).
    // Only that saturating call performs the effect, so the fold function's type
    // is `a -> b -> a ! c` — the outer (`f acc`) arrow stays pure, no stray `! c`.
    insta::assert_snapshot!(schemes(
        "fun foldl f acc xs =\n\
         \x20 match xs with | Nil -> acc | Cons x r -> foldl f (f acc x) r\n"
    ));
}

#[test]
fn return_clause_is_optional() {
    insta::assert_snapshot!(eval_main(
        "effect E { ask : () -> Int }\n\
         fun run act = handle act () with { ask a k -> k 99 }\n\
         def main = run (\\u -> ask () + ask ())\n"
    ));
}

#[test]
fn unhandled_operation_is_a_runtime_error() {
    insta::assert_snapshot!(eval_main(
        "effect E { boom : () -> Int }\n\
         def main = boom ()\n"
    ));
}

#[test]
fn unknown_effect_in_signature() {
    insta::assert_snapshot!(errors("effect E { op : () -> Int ! Nope }\n"));
}

#[test]
fn unknown_operation_in_handler() {
    insta::assert_snapshot!(errors(
        "effect E { a : () -> Int }\n\
         fun run act = handle act () with { nope x k -> k 0 }\n"
    ));
}
