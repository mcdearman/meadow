//! What a compact may hold is checked where its type is known.
//!
//! A compact region is never looked inside, so nothing in it may change or be
//! code. Every runtime refuses such a value as it copies it, but Silo cannot
//! see all of it: a function that captures nothing is a word there, like a
//! constructor with no fields, and it let one through where every other
//! engine refused. The type tells them apart, so `compact` and `compactAdd`
//! are refused at compile time when the value's type can reach a function or
//! anything mutable -- through tuples, records and data types' fields.

mod common;
use common::{errors_std_with, eval_main_std};
use meadow::Options;

fn errors(src: &str) -> String {
    errors_std_with(src, Options::debug())
}

#[track_caller]
fn refused(src: &str, what: &str) {
    let out = errors(src);
    assert!(
        out.contains(&format!("a compact cannot hold {what}")),
        "expected `{what}` to be refused in:\n{src}\ngot: {out}"
    );
}

#[track_caller]
fn accepted(src: &str) {
    let out = errors(src);
    assert!(!out.contains("compact cannot hold"), "{out}");
    assert!(out.is_empty(), "{out}");
}

const HEAD: &str = "use Std.Compact as C\nuse Std.Maybe.Maybe.*\n";

#[test]
fn a_function_is_refused_wherever_it_is_nested() {
    for value in [
        "(\\x -> x + 1)",
        "(Just (\\x -> x + 1))",
        "[\\x -> x + 1]",
        "(1, \\x -> x + 1)",
        "{ f = \\x -> x + 1 }",
        "[Just (\\x -> x + 1)]",
    ] {
        refused(
            &format!("{HEAD}def main = C.size (C.make {value})\n"),
            "a function",
        );
    }
}

#[test]
fn a_function_in_a_user_types_field_is_refused() {
    refused(
        &format!(
            "{HEAD}data Handler = Handler String (Int -> Int)\n\
             use Handler.*\n\
             def main = C.size (C.make (Handler \"inc\" (\\x -> x + 1)))\n"
        ),
        "a function",
    );
}

#[test]
fn a_recursive_type_is_looked_through_once() {
    // A list of handlers: the function is two constructors down a recursive
    // type, and the walk must still end.
    refused(
        &format!(
            "{HEAD}data Hs = Done | More (Int -> Int) Hs\n\
             use Hs.*\n\
             def main = C.size (C.make (More (\\x -> x) Done))\n"
        ),
        "a function",
    );
    accepted(&format!(
        "{HEAD}data Tree = Leaf | Node Tree Int Tree\n\
         use Tree.*\n\
         def main = C.size (C.make (Node Leaf 1 Leaf))\n"
    ));
}

#[test]
fn mutable_things_are_refused() {
    refused(
        &format!("{HEAD}def main = let r = newRef 1 in C.size (C.make (Just r))\n"),
        "a Ref",
    );
}

#[test]
fn compact_add_checks_what_it_adds() {
    refused(
        &format!("{HEAD}def main = C.size (C.add (C.make 1) [\\x -> x])\n"),
        "a function",
    );
    accepted(&format!(
        "{HEAD}def main = C.size (C.add (C.make 1) [1, 2, 3])\n"
    ));
}

#[test]
fn the_primitive_passed_as_a_value_is_checked_too() {
    refused(
        &format!(
            "{HEAD}use Std.Collections.Vector as V\n\
             def main = V.len (V.map compact [\\x -> x + 1])\n"
        ),
        "a function",
    );
}

#[test]
fn immutable_data_is_accepted() {
    accepted(&format!(
        "{HEAD}use Std.Collections.HashMap as H\n\
         data Shape = Circle Int | Square Int\n\
         use Shape.*\n\
         def main =\n\
         \x20 let a = C.make (H.insert 1 \"one\" H.empty) in\n\
         \x20 let b = C.make [Circle 1, Square 2] in\n\
         \x20 let c = C.make ({{ name = \"x\", sizes = #[1, 2] }}, Just 'c') in\n\
         \x20 let d = C.make (C.make [1, 2]) in\n\
         \x20 C.size a + C.size b + C.size c + C.size d\n"
    ));
}

#[test]
fn a_type_argument_no_field_holds_is_not_held() {
    // `Tag`'s parameter is phantom: a `Tag (Int -> Int)` holds only an `Int`.
    accepted(&format!(
        "{HEAD}data Tag a = Tag Int\n\
         use Tag.*\n\
         fun size (t : Tag (Int -> Int)) = C.size (C.make t)\n\
         def main = size (Tag 1)\n"
    ));
}

#[test]
fn accepted_values_still_round_trip() {
    assert_eq!(
        eval_main_std(&format!(
            "{HEAD}def main = C.get (C.make [Just 1, None, Just 3]) == [Just 1, None, Just 3]\n"
        )),
        "True"
    );
}

#[test]
fn a_wrapper_of_a_wrapper_is_checked_at_its_calls() {
    // `keep` hands its parameter to `C.make`, which hands it to `compact`:
    // each is checked where it is called, with the type known there.
    refused(
        &format!(
            "{HEAD}fun keep x = C.make x\n\
             def keepAll = \\x -> keep x\n\
             def main = C.size (keepAll (Just (\\x -> x)))\n"
        ),
        "a function",
    );
    accepted(&format!(
        "{HEAD}fun keep x = C.make x\n\
         def main = C.size (keep [1, 2])\n"
    ));
}
