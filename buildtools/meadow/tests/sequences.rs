//! The bracket syntax, across types, literals and patterns.
//!
//! One rule everywhere: a `;` means the linked `List`, and brackets without one
//! mean the default `Vector`. So `[a]`/`[a;]` are the types, `[x, y]`/`[x; y]`
//! the literals, and `[]`/`[;]` the empty patterns.

mod common;
use common::{errors_std_with, eval_main_std, schemes_std};
use meadow::Options;

// --- types -------------------------------------------------------------------

#[test]
fn a_semicolon_in_a_bracket_type_means_list() {
    let src = "\
data Box a = Box [a;]
data Bag a = Bag [a]
fun unbox b = match b with | Box xs -> xs
fun unbag b = match b with | Bag vs -> vs
";
    assert_eq!(
        schemes_std(src),
        "unbox : forall a. Box a -> List a\nunbag : forall a. Bag a -> [a]\n"
    );
}

#[test]
fn bracket_types_nest_either_way() {
    let src = "\
data A a = A [[a;];]
data B a = B [[a];]
data C a = C [[a;]]
fun ua x = match x with | A v -> v
fun ub x = match x with | B v -> v
fun uc x = match x with | C v -> v
";
    assert_eq!(
        schemes_std(src),
        "ua : forall a. A a -> List (List a)\n\
         ub : forall a. B a -> List [a]\n\
         uc : forall a. C a -> [List a]\n"
    );
}

#[test]
fn a_bracket_type_works_as_a_data_field_atom() {
    // `ty_atom` is a separate parser from `ty` — a positional field uses it, and
    // has to agree about what the `;` means.
    assert_eq!(
        schemes_std("data D a = D (Maybe [a;]) [a;] Int\nfun ud x = match x with | D m v n -> v\n"),
        "ud : forall a. D a -> List a\n"
    );
}

// --- literals ----------------------------------------------------------------

#[test]
fn one_semicolon_is_enough_to_make_a_list() {
    // `[x;]` is the only way to write a one-element list literal; `[x]` is a
    // one-element vector.
    assert_eq!(
        schemes_std("def a = [7;]\ndef b = [7]\ndef c = [1; 2; 3]\ndef d = [1, 2, 3]\n"),
        "a : List Int\nb : [Int]\nc : List Int\nd : [Int]\n"
    );
}

#[test]
fn the_empty_bracket_forms_are_distinct() {
    assert_eq!(
        schemes_std("def e = [;]\ndef v = []\n"),
        "e : forall a. List a\nv : forall a. [a]\n"
    );
}

#[test]
fn a_trailing_semicolon_is_allowed() {
    assert_eq!(
        schemes_std("def a = [1; 2;]\n"),
        "a : List Int\n"
    );
}

// --- patterns ----------------------------------------------------------------

#[test]
fn a_list_is_matched_with_empty_and_cons() {
    let src = "\
fun total xs = match xs with
  | [;] -> 0
  | x :: rest -> x + total rest

def main = total [1; 2; 3] + total [7;] + total [;]
";
    assert_eq!(eval_main_std(src), "13");
}

#[test]
fn the_empty_vector_pattern_matches_a_vector_emptied_at_run_time() {
    // `[]` is `VEmpty`, and the library keeps that the only representation of an
    // empty vector. This is the test that says so: the vector here is built
    // non-empty and emptied by `drop`, so a stale `VSingle #[]` would slip past.
    let src = "\
use Std.Collections.Vector as V

fun isEmpty v = match v with
  | [] -> 1
  | _ -> 0

def main =
  isEmpty []
  + isEmpty (V.drop [1, 2, 3] 3)
  + isEmpty (V.popBack [1])
  + isEmpty (V.filter (\\x -> x > 9) [1, 2, 3])
  + isEmpty (V.take [1, 2, 3] 0)
  + isEmpty [1, 2, 3]
";
    assert_eq!(eval_main_std(src), "5");
}

#[test]
fn a_non_empty_vector_pattern_is_rejected_with_a_way_out() {
    let out = errors_std_with("fun f v = match v with | [a, b] -> a\n", Options::debug());
    assert!(
        out.contains("`Vector` pattern can only be the empty `[]`"),
        "expected a vector-pattern diagnostic, got: {out}"
    );
}

// --- coverage ----------------------------------------------------------------

#[test]
fn the_empty_patterns_are_not_exhaustive_on_their_own() {
    let vector = errors_std_with("fun f v = match v with | [] -> 0\n", Options::release());
    assert!(
        vector.contains("non-exhaustive") && vector.contains("VSingle"),
        "expected the vector's other constructors, got: {vector}"
    );
    let list = errors_std_with("fun f xs = match xs with | [;] -> 0\n", Options::release());
    assert!(
        list.contains("non-exhaustive") && list.contains("Cons"),
        "expected `Cons` to be reported missing, got: {list}"
    );
}
