//! Core-IR lowering snapshots. `Program::pretty` renumbers variables `v0, v1, …`
//! in first-occurrence order so the output is stable across runs.

mod common;
use common::core_ir;

#[test]
fn curried_function() {
    insta::assert_snapshot!(core_ir("fun add a b = a + b\n"));
}

#[test]
fn nested_lambdas_curry_the_same() {
    insta::assert_snapshot!(core_ir("def add = \\a -> \\b -> a + b\n"));
}

#[test]
fn let_and_letrec() {
    insta::assert_snapshot!(core_ir(
        "fun f n =\n  let rec go k = if k == 0 then n else go (k - 1) in\n  go 3\n"
    ));
}

#[test]
fn match_lowers_to_case() {
    insta::assert_snapshot!(core_ir(
        "fun classify n = match n with | 0 -> \"z\" | _ -> \"nz\"\n"
    ));
}

#[test]
fn list_literal_and_cons() {
    // `[a; b; c]` is sugar for a `Cons`/`Nil` chain — `List` is an ordinary data
    // type, so there is no list form in core.
    insta::assert_snapshot!(core_ir(
        "def a = [1; 2; 3]\ndef b = Cons 0 a\n"
    ));
}

#[test]
fn tuple_pattern_binding_projects() {
    insta::assert_snapshot!(core_ir("def swapped = match (1, 2) with | (a, b) -> (b, a)\n"));
}

#[test]
fn record_construction_and_selection() {
    insta::assert_snapshot!(core_ir("def r = { x = 1 }\ndef v = r.x\n"));
}

#[test]
fn data_constructor_and_named_fields() {
    insta::assert_snapshot!(core_ir(
        "record P = { name : String, age : Int }\n\
         def p = P { age = 1, name = \"x\" }\n"
    ));
}

#[test]
fn operator_becomes_prim() {
    insta::assert_snapshot!(core_ir("def x = 2 * 3 + 1\n"));
}

#[test]
fn bare_operator_eta_expands() {
    insta::assert_snapshot!(core_ir("def plus = (+)\n"));
}

#[test]
fn a_polymorphic_function_is_a_type_abstraction() {
    // What makes core System F: `apply` binds its type variables, and each
    // mention of it says what they are at that call.
    insta::assert_snapshot!(core_ir(
        "fun apply f x = f x\n\
         def n = apply (\\y -> y + 1) 41\n\
         def s = apply (\\t -> t) \"hi\"\n"
    ));
}
