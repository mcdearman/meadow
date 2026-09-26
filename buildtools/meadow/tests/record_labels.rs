//! A record never holds a label twice.
//!
//! Rows are unified by rewriting, first match first, so a record type with two
//! `x` fields was well-typed while the back ends laid the record out with one:
//! `{ x = 1 | { x = True } }` read its `x` at whichever type the reader chose.
//! Record literals and patterns now refuse a repeated label, and extending a
//! row requires that it lack the new labels -- a promise its row variable
//! carries, and which a generalized scheme carries to every instance.

mod common;
use common::{errors_std_with, eval_main_std};
use meadow::Options;

fn errors(src: &str) -> String {
    errors_std_with(src, Options::debug())
}

fn refused(src: &str) {
    let out = errors(src);
    assert!(
        out.contains("already has a field `x`"),
        "expected a duplicate-field error for:\n{src}\ngot: {out}"
    );
}

#[test]
fn a_literal_with_a_label_twice_is_refused() {
    refused("def r = { x = 1, x = 2 }\ndef main = 0\n");
}

#[test]
fn a_pattern_with_a_label_twice_is_refused() {
    refused(
        "fun f (r : { x : Int }) : Int = match r with | { x = a, x = b } -> a\n\
         def main = 0\n",
    );
}

#[test]
fn extending_a_record_that_has_the_field_is_refused() {
    refused("def r = { x = True }\ndef s = { x = 1 | r }\ndef main = 0\n");
}

#[test]
fn a_polymorphic_extension_applied_to_a_record_with_the_field_is_refused() {
    // The promise is part of `ext`'s scheme, so it holds at each use.
    refused(
        "fun ext r = { x = 1 | r }\n\
         def s = ext { x = True }\n\
         def main = 0\n",
    );
}

#[test]
fn the_promise_survives_another_generalization() {
    // `wrap` never mentions `x`; it inherits `ext`'s requirement through its
    // own scheme.
    refused(
        "fun ext r = { x = 1 | r }\n\
         fun wrap r = ext r\n\
         def s = wrap { x = True }\n\
         def main = 0\n",
    );
}

#[test]
fn extending_twice_with_one_label_is_refused() {
    refused(
        "fun ext r = { x = 1 | r }\n\
         fun twice r = ext (ext r)\n\
         def main = 0\n",
    );
}

#[test]
fn the_rest_of_an_open_pattern_lacks_its_fields() {
    // `rest` in `{ x = a | _ }` is what is not `x`, so a row variable bound
    // there cannot later be given an `x` of its own by extension.
    refused(
        "fun f r = match r with | { x = a | _ } -> { x = a | r }\n\
         def main = 0\n",
    );
}

#[test]
fn ordinary_extension_still_works() {
    let out = errors(
        "fun ext r = { x = 1 | r }\n\
         fun wrap r = ext r\n\
         def main = (wrap { y = 2 }).x + (ext { y = 40 }).y\n",
    );
    assert!(!out.contains("already has a field"), "{out}");
    assert_eq!(
        eval_main_std(
            "fun ext r = { x = 1 | r }\n\
             fun wrap r = ext r\n\
             def main = (wrap { y = 2 }).x + (ext { y = 40 }).y\n"
        ),
        "41"
    );
}

#[test]
fn distinct_labels_and_updates_are_unaffected() {
    assert_eq!(
        eval_main_std(
            "def r = { x = 1, y = 2 }\n\
             def s = { z = 3 | r }\n\
             def main = s.x + s.y + s.z + { r | x = 10 }.x\n"
        ),
        "16"
    );
}

#[test]
fn an_annotated_extension_keeps_its_promise() {
    // With a signature, the scheme comes from the annotation rather than from
    // generalizing the body; the promise the body makes must still reach it.
    refused(
        "fun ext (r : { | r }) : { x : Int | r } = { x = 1 | r }\n\
         def s = ext { x = True }\n\
         def main = 0\n",
    );
    refused(
        "fun ext : { | r } -> { x : Int | r } = \\p -> { x = 1 | p }\n\
         def s = ext { x = True }\n\
         def main = 0\n",
    );
}
