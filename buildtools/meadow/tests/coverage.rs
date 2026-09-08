//! Pattern-coverage checks: `match` exhaustiveness (release-only) and
//! irrefutability of binding positions (always on).

mod common;
use common::{errors_std_with, errors_with};
use meadow::Options;

// --- `match` exhaustiveness -------------------------------------------------

#[test]
fn missing_variant_is_reported_in_release() {
    let src = "def main = match Just 1 with | Just x -> x\n";
    assert_eq!(errors_std_with(src, Options::debug()), "");
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `None` is not matched"
    );
}

#[test]
fn covering_every_variant_is_accepted() {
    let src = "def main = match Just 1 with | Just x -> x | None -> 0\n";
    assert_eq!(errors_std_with(src, Options::release()), "");
}

#[test]
fn wildcard_covers_the_rest() {
    let src = "def main = match Just 1 with | Just x -> x | _ -> 0\n";
    assert_eq!(errors_std_with(src, Options::release()), "");
}

#[test]
fn missing_list_case_names_the_constructor() {
    let src = "fun headOr d xs = match xs with | Nil -> d\ndef main = headOr 0 [1; 2]\n";
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `Cons _ _` is not matched"
    );
}

#[test]
fn nested_patterns_report_a_nested_witness() {
    let src = "def main = match Just (Just 1) with | Just (Just x) -> x | None -> 0\n";
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `Just None` is not matched"
    );
}

#[test]
fn bool_needs_both_cases() {
    let src = "def main = match True with | True -> 1\n";
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `False` is not matched"
    );
}

#[test]
fn literal_patterns_are_never_exhaustive_without_a_default() {
    let src = "fun classify n = match n with | 0 -> 1 | 1 -> 2\ndef main = classify 0\n";
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `_` is not matched"
    );
}

#[test]
fn tuple_scrutinee_reports_a_tuple_witness() {
    let src = "def main = match (True, True) with | (True, True) -> 1 | (False, _) -> 2\n";
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `(True, False)` is not matched"
    );
}

#[test]
fn a_matched_tuple_of_bools_can_be_complete() {
    let src = "def main = match (True, True) with \
               | (True, True) -> 1 | (True, False) -> 2 | (False, _) -> 3\n";
    assert_eq!(errors_std_with(src, Options::release()), "");
}

// --- irrefutability (checked under *both* profiles) --------------------------

#[test]
fn refutable_function_parameter_is_always_an_error() {
    let src = "fun f (Just x) = x\ndef main = f (Just 1)\n";
    let expected = "refutable pattern in function parameter: `None` is not matched";
    assert_eq!(errors_std_with(src, Options::debug()), expected);
    assert_eq!(errors_std_with(src, Options::release()), expected);
}

#[test]
fn refutable_lambda_parameter_is_an_error() {
    let src = "def main = (\\(Just x) -> x) (Just 1)\n";
    assert_eq!(
        errors_std_with(src, Options::debug()),
        "refutable pattern in lambda parameter: `None` is not matched"
    );
}

#[test]
fn tuple_and_record_parameters_are_irrefutable() {
    let src = "fun dist (x, y) = x * x + y * y\n\
               fun name { first, last } = first\n\
               def main = dist (3, 4)\n";
    assert_eq!(errors_std_with(src, Options::debug()), "");
}

#[test]
fn a_single_variant_constructor_parameter_is_irrefutable() {
    // `Wrapper` has exactly one constructor, so destructuring it cannot fail.
    let src = "data Wrapper = Wrap Int\n\
               fun unwrap (Wrap n) = n\n\
               def main = unwrap (Wrap 7)\n";
    assert_eq!(errors_with(src, Options::debug()), "");
}

#[test]
fn refutable_def_binding_is_an_error() {
    let src = "data T = A Int | B\ndef (A n) = A 1\ndef main = n\n";
    assert_eq!(
        errors_with(src, Options::debug()),
        "refutable pattern in binding: `B` is not matched"
    );
}
