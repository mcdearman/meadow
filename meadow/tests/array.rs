//! The builtin `Array` type (`#[…]`).

mod common;
use common::{eval_expr, eval_main, schemes};

#[test]
fn literal_type_and_value() {
    insta::assert_snapshot!(schemes("def a = #[1, 2, 3]\ndef e = #[]\n"));
    insta::assert_snapshot!(eval_expr("#[1, 2, 3]"));
    insta::assert_snapshot!(eval_expr("#[]"));
}

#[test]
fn len_get_getor() {
    insta::assert_snapshot!(eval_expr("arrayLen #[10, 20, 30]"), @"3");
    insta::assert_snapshot!(eval_expr("arrayGet #[10, 20, 30] 1"), @"20");
    insta::assert_snapshot!(eval_expr("arrayGetOr 0 #[10, 20, 30] 9"), @"0");
    insta::assert_snapshot!(eval_expr("arrayLen #[]"), @"0");
}

#[test]
fn set_push_pop() {
    insta::assert_snapshot!(eval_expr("arraySet #[10, 20, 30] 1 99"), @"#[10, 99, 30]");
    insta::assert_snapshot!(eval_expr("arrayPush #[1, 2] 3"), @"#[1, 2, 3]");
    insta::assert_snapshot!(eval_expr("arrayPop #[1, 2, 3]"), @"#[1, 2]");
}

#[test]
fn slice_and_concat() {
    insta::assert_snapshot!(eval_expr("arraySlice #[1, 2, 3, 4, 5] 1 4"), @"#[2, 3, 4]");
    insta::assert_snapshot!(eval_expr("arraySlice #[1, 2, 3] 0 100"), @"#[1, 2, 3]");
    insta::assert_snapshot!(eval_expr("arrayConcat #[1, 2] #[3, 4]"), @"#[1, 2, 3, 4]");
}

#[test]
fn is_persistent() {
    // `arraySet` must not mutate the original binding.
    insta::assert_snapshot!(eval_main(
        "def a = #[1, 2, 3]\ndef b = arraySet a 0 9\ndef main = (a, b)\n"
    ), @"(#[1, 2, 3], #[9, 2, 3])");
}

#[test]
fn out_of_bounds_is_a_runtime_error() {
    insta::assert_snapshot!(eval_expr("arrayGet #[1, 2, 3] 5"));
    insta::assert_snapshot!(eval_expr("arrayPop #[]"));
}

#[test]
fn pattern_match_on_array() {
    insta::assert_snapshot!(eval_main(
        "fun sum3 a = match a with | #[x, y, z] -> x + y + z | _ -> 0\n\
         def main = (sum3 #[10, 20, 30], sum3 #[1, 2])\n"
    ), @"(60, 0)");
}
