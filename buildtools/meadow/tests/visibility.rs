//! How far out of its module a declaration can be seen.
//!
//! Rust's arrangement with the package in the crate's place: nothing is
//! visible outside its own module until it says so, `@pub(pkg)` gets it as far as
//! the rest of the package, and `@pub` is what a dependent can name.
//! `@pub(super)` is the narrow one -- the parent module and its subtree.
//!
//! The one departure: a unit that never mentions visibility has none at all,
//! so a two-file program need not annotate itself to see across its own files.

mod common;
use common::{eval_unit, unit_errors};

// --- `@pub(pkg)`: out of the module, not out of the package -----------------------

#[test]
fn a_plain_declaration_does_not_leave_its_module() {
    let out = unit_errors(&[
        ("Hidden", "fun secret n = n * 2\n@pub(pkg) fun shown n = n + 1\n"),
        ("", "use Hidden (secret)\ndef main = secret 21\n"),
    ]);
    assert!(
        out.contains("`secret` is private to module `Hidden`"),
        "a private name crossed a module boundary: {out}"
    );
}

#[test]
fn pub_pkg_reaches_the_rest_of_the_package() {
    assert_eq!(
        eval_unit(&[
            ("Hidden", "fun secret n = n * 2\n@pub(pkg) fun shown n = secret n + 1\n"),
            ("", "use Hidden (shown)\ndef main = shown 20\n"),
        ]),
        "41"
    );
}

#[test]
fn a_module_can_always_see_itself() {
    // Nothing here is marked, and nothing needs to be: the declarations and
    // their uses are in one module.
    assert_eq!(
        eval_unit(&[
            ("Solo", "fun secret n = n * 2\n@pub(pkg) fun shown n = secret n\n"),
            ("", "use Solo (shown)\ndef main = shown 21\n"),
        ]),
        "42"
    );
}

#[test]
fn an_unannotated_unit_has_no_visibility_at_all() {
    // The escape hatch: say nothing about visibility anywhere and a package's
    // modules see each other, which is what a small program wants.
    assert_eq!(
        eval_unit(&[
            ("Math", "fun double n = n * 2\n"),
            ("", "use Math (double)\ndef main = double 21\n"),
        ]),
        "42"
    );
}

// --- `@pub(super)` -----------------------------------------------------------

#[test]
fn pub_super_reaches_the_parent_and_no_further() {
    let ok = eval_unit(&[
        ("Outer.Inner", "@pub(super) fun helper n = n * 3\n"),
        ("Outer", "use Outer.Inner (helper)\n@pub(pkg) fun useHelper n = helper n\n"),
        ("", "use Outer (useHelper)\ndef main = useHelper 7\n"),
    ]);
    assert_eq!(ok, "21");

    let out = unit_errors(&[
        ("Outer.Inner", "@pub(super) fun helper n = n * 3\n"),
        ("Outer", "@pub(pkg) fun useHelper n = n\n"),
        ("", "use Outer.Inner (helper)\ndef main = helper 7\n"),
    ]);
    assert!(
        out.contains("visible only to the parent of module `Outer.Inner`"),
        "`@pub(super)` was visible to a stranger: {out}"
    );
}

// --- types and constructors follow the same rule -----------------------------

#[test]
fn a_type_is_as_visible_as_it_says() {
    let out = unit_errors(&[
        ("Syntax", "data Expr = Lit Int\n@pub(pkg) fun lit n = Expr.Lit n\n"),
        ("", "use Syntax (Expr)\ndef main = 1\n"),
    ]);
    assert!(
        out.contains("`Expr` is private to module `Syntax`"),
        "a private type crossed a module boundary: {out}"
    );
}

#[test]
fn a_pub_type_brings_its_constructors_with_it() {
    assert_eq!(
        eval_unit(&[
            ("Syntax", "@pub(pkg) data Expr = Lit Int | Neg Expr\n"),
            (
                "",
                "use Syntax (Expr)\nfun eval e = match e with\n  | Lit n -> n\n  | Neg i -> 0 - eval i\ndef main = eval (Neg (Lit 5))\n"
            ),
        ]),
        "-5"
    );
}

// --- the package boundary ----------------------------------------------------

#[test]
fn only_a_plain_pub_is_exported() {
    // `@pub(pkg)` stops at the package: what a dependent sees is `@pub`.
    let (cp, _) = meadow::pipeline::compile_str(
        "m",
        "@pub(pkg) fun inside x = x\n@pub fun outside y = y\n",
    );
    let names: Vec<String> = cp.exports.iter().map(|e| e.name.to_string()).collect();
    assert_eq!(names, vec!["outside".to_string()]);
}

#[test]
fn an_unknown_visibility_is_reported() {
    let out = unit_errors(&[("", "@pub(nowhere) fun f x = x\ndef main = f 1\n")]);
    assert!(
        out.contains("unknown visibility `@pub(nowhere)`"),
        "a nonsense visibility was accepted: {out}"
    );
}

/// `@pub(pack)` was the public spelling before `@pub` took that meaning, and old
/// code says it. It is reported, once, with the spelling it became -- `@pub`,
/// not the `@pub(pkg)` it resembles, which would hide the name from every
/// dependent -- and still resolved as what it meant, so that one message is the
/// only thing that breaks.
#[test]
fn the_old_pub_pack_says_what_it_is_now_called() {
    let out = unit_errors(&[("", "@pub(pack) fun f x = x\ndef main = f 1\n")]);
    assert_eq!(
        out.matches("`@pub(pack)` is now written `@pub`").count(),
        1,
        "reported once, with the new spelling: {out}"
    );
    assert!(!out.contains("unknown visibility"), "{out}");

    let (cp, _) = meadow::pipeline::compile_str("m", "@pub(pack) fun f x = x\n");
    let names: Vec<String> = cp.exports.iter().map(|e| e.name.to_string()).collect();
    assert_eq!(names, vec!["f".to_string()], "still exported, as it used to be");
}

#[test]
fn an_unknown_visibility_is_reported_once() {
    let out = unit_errors(&[("", "@pub(nowhere) fun f x = x\ndef main = f 1\n")]);
    assert_eq!(out.matches("unknown visibility").count(), 1, "{out}");
}

// --- the entry point ---------------------------------------------------------

#[test]
fn main_needs_no_marker_even_in_an_annotated_package() {
    // It used to: `main` was found among the exports, so adding a visibility attribute
    // anywhere meant `main` needed it too or the program had no entry point.
    assert_eq!(
        eval_unit(&[
            ("Math", "@pub(pkg) fun double n = n * 2\n"),
            ("", "use Math (double)\ndef main = double 21\n"),
        ]),
        "42"
    );
}
