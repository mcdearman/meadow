//! One mistake, one diagnostic.
//!
//! Something the resolver has already rejected — an undefined name, an
//! unknown constructor or type — gets the error type, which agrees with
//! everything. Neither inference nor the coverage checker reports it a second
//! time, as a mismatch or a missing case. These run under the release profile,
//! where the exhaustiveness check is on, so a cascade has every chance to show.
//!
//! The second half is the other side of it: errors that *are* independent are
//! still all reported.

mod common;
use common::errors_std_with;
use meadow::Options;

fn errors(src: &str) -> String {
    errors_std_with(src, Options::release())
}

// --- the first error only -----------------------------------------------------

#[test]
fn unknown_constructor_bound_by_def() {
    assert_eq!(errors("def Great = 1\n"), "unknown constructor `Great`");
}

#[test]
fn unknown_constructor_as_a_parameter() {
    assert_eq!(errors("fun f (Nope x) = x\n"), "unknown constructor `Nope`");
}

#[test]
fn unknown_constructor_in_a_match_arm() {
    assert_eq!(
        errors("def main = match 1 with | Nope -> 0\n"),
        "unknown constructor `Nope`"
    );
}

#[test]
fn undefined_scrutinee() {
    assert_eq!(
        errors("data T = A | B\ndef main = match nope with | A -> 0\n"),
        "undefined variable: nope"
    );
}

#[test]
fn unknown_type_in_an_annotation() {
    assert_eq!(
        errors("fun f (x : Nope) : Int = x + 1\n"),
        "unknown type `Nope`"
    );
}

#[test]
fn wrong_arity_in_an_annotation() {
    assert_eq!(
        errors("fun f (x : Maybe) = match x with | Just y -> y\n"),
        "type `Maybe` takes 1 argument(s), got 0"
    );
}

#[test]
fn unknown_type_nested_in_an_annotation() {
    assert_eq!(
        errors("fun f (x : Maybe Nope) : Int = x\n"),
        "unknown type `Nope`"
    );
}

#[test]
fn a_variable_poisoned_by_its_pattern_stays_quiet() {
    // `x` meets the unknown constructor first; that it is later used as both an
    // `Int` and a `Bool` is not news about anything the programmer can see.
    assert_eq!(
        errors("fun f x = match x with | Nope -> x + 1 | _ -> if x then 1 else 2\n"),
        "unknown constructor `Nope`"
    );
}

#[test]
fn undefined_variable_in_arithmetic() {
    assert_eq!(errors("def main = nope + 1\n"), "undefined variable: nope");
}

// --- independent errors are all still reported --------------------------------

#[test]
fn a_refutable_pattern_around_an_unknown_constructor() {
    // `Just _` misses `None` whatever `Nope` was meant to be.
    assert_eq!(
        errors("fun f (Just Nope) = 1\n"),
        "unknown constructor `Nope`\nrefutable pattern in function parameter: `None` is not matched"
    );
}

#[test]
fn two_unrelated_errors() {
    assert_eq!(
        errors("def a = nope\ndef b = 1 + \"s\"\n"),
        "undefined variable: nope\ntype mismatch: `Int` vs `String`"
    );
}

#[test]
fn an_error_beside_a_real_mismatch() {
    assert_eq!(
        errors("def main = (nope, 1 + \"s\")\n"),
        "undefined variable: nope\ntype mismatch: `Int` vs `String`"
    );
}

#[test]
fn a_missing_case_is_still_missing() {
    assert_eq!(
        errors("data T = A | B\ndef main = match A with | A -> 0\n"),
        "non-exhaustive patterns: `B` is not matched"
    );
}
