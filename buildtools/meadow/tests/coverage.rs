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
    let src = "use Wrapper.*\ndata Wrapper = Wrap Int\n\
               fun unwrap (Wrap n) = n\n\
               def main = unwrap (Wrap 7)\n";
    assert_eq!(errors_with(src, Options::debug()), "");
}

#[test]
fn refutable_def_binding_is_an_error() {
    let src = "use T.*\ndata T = A Int | B\ndef (A n) = A 1\ndef main = n\n";
    assert_eq!(
        errors_with(src, Options::debug()),
        "refutable pattern in binding: `B` is not matched"
    );
}

// --- methods --------------------------------------------------------------------
//
// An `impl`'s methods and a trait's defaults are functions like any other; the
// checker once walked only top-level bindings, so a `match` in a method went
// unchecked, and a missing case in release was a crash at run time.

const SIZE: &str = "trait Size a { fun size : a -> Int }\n";

#[test]
fn a_non_exhaustive_match_in_an_impl_method_is_reported() {
    let src = format!(
        "{SIZE}impl Size (Maybe a) {{ fun size m = match m with | Just n -> 1 }}\n\
         def main = size (Just 3)\n"
    );
    assert_eq!(errors_std_with(&src, Options::debug()), "");
    assert_eq!(
        errors_std_with(&src, Options::release()),
        "non-exhaustive patterns: `None` is not matched"
    );
}

#[test]
fn an_exhaustive_impl_method_is_accepted() {
    let src = format!(
        "{SIZE}impl Size (Maybe a) {{ fun size m = match m with | Just n -> 1 | None -> 0 }}\n\
         def main = size (Just 3)\n"
    );
    assert_eq!(errors_std_with(&src, Options::release()), "");
}

#[test]
fn a_refutable_parameter_of_an_impl_method_is_an_error() {
    let src = format!(
        "{SIZE}impl Size (Maybe a) {{ fun size (Just n) = 1 }}\n\
         def main = size (Just 3)\n"
    );
    let expected = "refutable pattern in function parameter: `None` is not matched";
    assert_eq!(errors_std_with(&src, Options::debug()), expected);
    assert_eq!(errors_std_with(&src, Options::release()), expected);
}

#[test]
fn a_non_exhaustive_match_in_a_default_method_is_reported() {
    let src = "trait Pick a {\n\
               \x20 fun pick : a -> Maybe Int\n\
               \x20 fun picked : a -> Int\n\
               \x20   | picked x = match pick x with | Just n -> n\n\
               }\n\
               impl Pick Int { fun pick n = Just n }\n\
               def main = picked 4\n";
    assert_eq!(errors_std_with(src, Options::debug()), "");
    assert_eq!(
        errors_std_with(src, Options::release()),
        "non-exhaustive patterns: `None` is not matched"
    );
}

#[test]
fn clause_methods_are_checked_as_a_whole() {
    // Clauses covering both `Bool`s are complete; dropping one is not.
    let both = "trait Tag a {\n\
                \x20 fun tag : a -> Bool -> Int\n\
                \x20   | tag x True  = 1\n\
                \x20   | tag x False = 0\n\
                }\n\
                impl Tag Int {}\n\
                def main = tag 3 True\n";
    assert_eq!(errors_std_with(both, Options::release()), "");
    let one = "trait Tag a { fun tag : a -> Bool -> Int }\n\
               impl Tag Int { fun tag x b = match b with | True -> x }\n\
               def main = tag 3 True\n";
    assert_eq!(
        errors_std_with(one, Options::release()),
        "non-exhaustive patterns: `False` is not matched"
    );
}
