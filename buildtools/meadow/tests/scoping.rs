//! Name resolution: shadowing, and which binding a name actually refers to.
//!
//! The rule these pin down: a binding *with parameters* is recursive, so its own
//! name refers to itself; a binding *without* them is not, so its right-hand side
//! sees the enclosing scope and can shadow.

mod common;
use common::{eval_main, eval_main_std};

// --- shadowing ---------------------------------------------------------------

#[test]
fn a_let_can_shadow_and_still_read_the_old_value() {
    // Previously this resolved `x` on the right to the `x` being defined, and
    // failed at run time with `unbound variable`.
    assert_eq!(eval_main("def main = let x = 1 in let x = x + 1 in x\n"), "2");
}

#[test]
fn a_let_can_shadow_a_function_parameter() {
    assert_eq!(
        eval_main("fun f x = let x = x + 1 in x\ndef main = f 1\n"),
        "2"
    );
}

#[test]
fn a_let_can_shadow_a_top_level_binding() {
    // The REPL case: `def x = 1` then `let x = x in x` should be 1, not an error.
    assert_eq!(eval_main("def x = 1\ndef main = let x = x in x\n"), "1");
}

#[test]
fn shadowing_without_a_self_reference_still_works() {
    assert_eq!(eval_main("def main = let x = 1 in let x = 2 in x\n"), "2");
}

#[test]
fn a_tuple_pattern_can_shadow() {
    assert_eq!(
        eval_main("def main = let (a, b) = (1, 2) in let (a, b) = (b, a) in (a, b)\n"),
        "(2, 1)"
    );
}

// --- recursion is the exception ----------------------------------------------

#[test]
fn a_local_binding_with_parameters_is_recursive() {
    // No `rec` needed: parameters make it a function, and a function's own name
    // refers to itself. This is what shadowing must not break.
    assert_eq!(
        eval_main(
            "def main = let go n = if n == 0 then 0 else n + go (n - 1) in go 5\n"
        ),
        "15"
    );
}

#[test]
fn let_rec_is_accepted_as_the_same_thing() {
    assert_eq!(
        eval_main(
            "def main = let rec go n = if n == 0 then 0 else n + go (n - 1) in go 5\n"
        ),
        "15"
    );
}

#[test]
fn top_level_recursion_is_unaffected() {
    assert_eq!(
        eval_main("fun fact n = if n <= 1 then 1 else n * fact (n - 1)\ndef main = fact 5\n"),
        "120"
    );
}

#[test]
fn top_level_mutual_recursion_is_unaffected() {
    assert_eq!(
        eval_main_std(
            "fun isEven n = if n == 0 then True else isOdd (n - 1)\n\
             fun isOdd n = if n == 0 then False else isEven (n - 1)\n\
             def main = isEven 10\n"
        ),
        "true"
    );
}

// --- the top-level id must not leak ------------------------------------------

#[test]
fn a_local_binding_is_distinct_from_a_top_level_one_of_the_same_name() {
    // Resolution reuses a *predeclared* id for a top-level binding's own name.
    // That must not leak into the body, or the local `n` below would alias the
    // top-level `n` and both would read 1.
    assert_eq!(
        eval_main("def n = 100\nfun f u = let n = 1 in n\ndef main = (f (), n)\n"),
        "(1, 100)"
    );
}

#[test]
fn a_lambda_parameter_shadows_a_top_level_binding() {
    assert_eq!(eval_main("def y = 5\ndef main = (\\y -> y + 1) 10\n"), "11");
}
