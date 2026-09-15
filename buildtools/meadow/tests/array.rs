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

/// Slicing near the end of a long array costs the slice, not the whole array
/// before it: it used to read every element up to `from` and throw them away,
/// which made a lexer cutting words out of a file quadratic.
#[test]
fn slicing_a_long_array_costs_the_slice() {
    let src = "use Std.String as S\n\n\
        fun go a i acc = if i == 0 then acc else go a (i - 1) (acc + arrayLen (arraySlice a 399990 400000))\n\n\
        def main = go (S.toBytes (S.repeat \"ab\" 200000)) 20000 0\n";
    let started = std::time::Instant::now();
    assert_eq!(common::eval_main_std(src), "200000");
    // Quadratic, this is 8 billion reads: minutes, even optimized.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "took {:?}",
        started.elapsed()
    );
}
