//! Parser snapshots. The AST carries no `VarId`s (only interned strings + spans),
//! so its `Debug` output is stable and safe to snapshot.

mod common;
use common::parse_ast;

#[test]
fn function_with_let_and_if() {
    insta::assert_snapshot!(parse_ast(
        "fun foo x =\n  let y = x + 1 in\n  if y == 0 then y else y * y\n"
    ));
}

#[test]
fn operators_precedence() {
    insta::assert_snapshot!(parse_ast("def e = 1 + 2 * 3 - 4 / 2 ^ 2\n"));
}

#[test]
fn lambda_match_tuple_list() {
    insta::assert_snapshot!(parse_ast(
        "def f = \\p -> match p with | (a, b) -> [a, b, a]\n"
    ));
}

#[test]
fn records_and_field_access() {
    insta::assert_snapshot!(parse_ast(
        "def r = { x = 1, y = 2 }\ndef v = r.x\ndef ext = { z = 3 | r }\n"
    ));
}

#[test]
fn constructor_application() {
    insta::assert_snapshot!(parse_ast("def xs = Cons 1 (Cons 2 Nil)\n"));
}

#[test]
fn attributes_on_declarations_and_fields() {
    insta::assert_snapshot!(parse_ast(
        "@pub\n@attr(Some, Set, Of, Attributes)\nfun f x = x\n\
         @pub record Person = { @pub name : String, age : Int }\n"
    ));
}

#[test]
fn use_with_selected_names() {
    insta::assert_snapshot!(parse_ast(
        "@pub use Std.Collections.List (map, filter, foldl)\n"
    ));
}

#[test]
fn cons_operator_is_sugar_for_cons_ctor() {
    // `a :: b :: Nil` (right-assoc) and `a :: rest` as a pattern both desugar to
    // the `Cons` constructor.
    insta::assert_snapshot!(parse_ast(
        "def xs = 1 :: 2 :: Nil\nfun uncons l = match l with | x :: rest -> x | Nil -> 0\n"
    ));
}

#[test]
fn data_declaration() {
    insta::assert_snapshot!(parse_ast(
        "data Tree a\n  = Tip\n  | Branch (Tree a) a (Tree a)\n"
    ));
}

#[test]
fn data_named_fields() {
    insta::assert_snapshot!(parse_ast(
        "data Node a = Leaf a | Inner { left : Node a, right : Node a }\n"
    ));
}

#[test]
fn record_declaration() {
    insta::assert_snapshot!(parse_ast(
        "record Person = {\n  name : String,\n  age : Int,\n}\n"
    ));
}

#[test]
fn function_type_in_field() {
    insta::assert_snapshot!(parse_ast(
        "data Thunk a = Thunk (Unit -> a)\n"
    ));
}

#[test]
fn use_declaration() {
    insta::assert_snapshot!(parse_ast("use std.list.map\n"));
}
