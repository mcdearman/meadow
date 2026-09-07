//! Snapshots of the diagnostics produced for ill-typed / ill-formed programs.
//! Inference never bails, so several errors can appear at once.

mod common;
use common::errors;

#[test]
fn undefined_variable() {
    insta::assert_snapshot!(errors("def x = y + 1\n"));
}

#[test]
fn type_mismatch_arith() {
    insta::assert_snapshot!(errors("def x = 1 + \"two\"\n"));
}

#[test]
fn if_branches_disagree() {
    insta::assert_snapshot!(errors("def x = if 1 < 2 then 1 else \"no\"\n"));
}

#[test]
fn occurs_check() {
    insta::assert_snapshot!(errors("def x = \\f -> f f\n"));
}

#[test]
fn unknown_type_in_data() {
    insta::assert_snapshot!(errors("data Bad = B Nope\n"));
}

#[test]
fn wrong_type_arity() {
    insta::assert_snapshot!(errors(
        "data Box a = Box a\ndata Bad = B Box\n"
    ));
}

#[test]
fn unbound_type_variable() {
    insta::assert_snapshot!(errors("data Bad a = B b\n"));
}

#[test]
fn duplicate_field() {
    insta::assert_snapshot!(errors("record R = { a : Int, a : Int }\n"));
}

#[test]
fn unknown_constructor() {
    insta::assert_snapshot!(errors("def x = Nope 1\n"));
}

#[test]
fn constructor_arg_mismatch() {
    insta::assert_snapshot!(errors(
        "data T = C Int\ndef x = C \"str\"\n"
    ));
}

#[test]
fn missing_record_field() {
    insta::assert_snapshot!(errors(
        "record P = { name : String, age : Int }\ndef p = P { name = \"x\" }\n"
    ));
}

#[test]
fn nominal_record_is_not_structural() {
    insta::assert_snapshot!(errors(
        "record P = { name : String }\n\
         fun getName r = r.name\n\
         def bad = getName (P { name = \"x\" })\n"
    ));
}

#[test]
fn parse_error() {
    insta::assert_snapshot!(errors("def x = \n"));
}

#[test]
fn plain_op_rejects_bigint() {
    // A `BigInt` cannot flow into a plain `Int` operator (no implicit widening of
    // a non-literal).
    insta::assert_snapshot!(errors("def x = toBigInt 3 + 4\n"));
}

#[test]
fn int_and_bigint_results_dont_unify() {
    // Literals coerce, but once `+~` has produced a `BigInt` it will not unify
    // with an `Int` result.
    insta::assert_snapshot!(errors("def x = 1 +~ 2 == 1 + 2\n"));
}
