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
        (
            "Hidden",
            "fun secret n = n * 2\n@pub(pkg) fun shown n = n + 1\n",
        ),
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
            (
                "Hidden",
                "fun secret n = n * 2\n@pub(pkg) fun shown n = secret n + 1\n"
            ),
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
            (
                "Solo",
                "fun secret n = n * 2\n@pub(pkg) fun shown n = secret n\n"
            ),
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
        (
            "Outer",
            "use Outer.Inner (helper)\n@pub(pkg) fun useHelper n = helper n\n",
        ),
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
        (
            "Syntax",
            "data Expr = Lit Int\n@pub(pkg) fun lit n = Expr.Lit n\n",
        ),
        ("", "use Syntax (Expr)\ndef main = 1\n"),
    ]);
    assert!(
        out.contains("`Expr` is private to module `Syntax`"),
        "a private type crossed a module boundary: {out}"
    );
}

// --- constructors live under their type -------------------------------------
//
// As in Rust: naming a type brings the type, and its constructors are written
// `Type.Ctor` until a `use Module.Type (Ctor)` or `use Module.Type.*` says
// otherwise. Neither naming the type nor a glob `use Module` puts them in
// scope bare.

const EXPR: (&str, &str) = ("Syntax", "@pub(pkg) data Expr = Lit Int | Neg Expr\n");

#[test]
fn naming_a_type_leaves_its_constructors_under_it() {
    let out = unit_errors(&[
        EXPR,
        (
            "",
            "use Syntax (Expr)\nfun eval e = match e with\n  | Lit n -> n\n  | Neg i -> 0 - eval i\ndef main = 1\n",
        ),
    ]);
    assert!(
        out.contains("unknown constructor `Lit`"),
        "`Lit` came in with `Expr`: {out}"
    );
    assert!(
        out.contains("unknown constructor `Neg`"),
        "`Neg` came in with `Expr`: {out}"
    );
}

#[test]
fn a_named_type_qualifies_its_constructors() {
    assert_eq!(
        eval_unit(&[
            EXPR,
            (
                "",
                "use Syntax (Expr)\nfun eval e = match e with\n  | Expr.Lit n -> n\n  | Expr.Neg i -> 0 - eval i\ndef main = eval (Expr.Neg (Expr.Lit 5))\n"
            ),
        ]),
        "-5"
    );
}

#[test]
fn a_type_path_imports_just_the_type() {
    assert_eq!(
        eval_unit(&[
            EXPR,
            (
                "",
                "use Syntax.Expr\ndef main = match Expr.Lit 5 with | Expr.Lit n -> n | Expr.Neg e -> 0\n"
            )
        ]),
        "5"
    );
    let out = unit_errors(&[EXPR, ("", "use Syntax.Expr\ndef main = Lit 5\n")]);
    assert!(out.contains("unknown constructor `Lit`"), "{out}");
}

#[test]
fn a_type_path_with_a_list_imports_those_constructors() {
    assert_eq!(
        eval_unit(&[
            EXPR,
            (
                "",
                "use Syntax.Expr (Lit, Neg)\nfun eval e = match e with\n  | Lit n -> n\n  | Neg i -> 0 - eval i\ndef main = eval (Neg (Lit 5))\n"
            ),
        ]),
        "-5"
    );
    // Only the ones listed.
    let out = unit_errors(&[
        EXPR,
        ("", "use Syntax.Expr (Lit)\ndef main = Neg (Lit 5)\n"),
    ]);
    assert!(out.contains("unknown constructor `Neg`"), "{out}");
    assert!(!out.contains("`Lit`"), "{out}");
}

#[test]
fn a_type_path_glob_imports_every_constructor() {
    assert_eq!(
        eval_unit(&[
            EXPR,
            (
                "",
                "use Syntax.Expr.*\nfun eval e = match e with\n  | Lit n -> n\n  | Neg i -> 0 - eval i\ndef main = eval (Neg (Lit 5))\n"
            ),
        ]),
        "-5"
    );
}

#[test]
fn a_module_glob_does_not_flatten_constructors() {
    let out = unit_errors(&[EXPR, ("", "use Syntax\ndef main = Lit 5\n")]);
    assert!(out.contains("unknown constructor `Lit`"), "{out}");
    assert_eq!(
        eval_unit(&[
            EXPR,
            (
                "",
                "use Syntax\ndef main = match Expr.Lit 5 with | Expr.Lit n -> n | Expr.Neg e -> 0\n"
            )
        ]),
        "5"
    );
}

#[test]
fn a_constructor_named_as_a_module_item_says_where_it_lives() {
    let out = unit_errors(&[EXPR, ("", "use Syntax (Lit)\ndef main = 1\n")]);
    assert_eq!(
        out,
        "`Lit` is a constructor of `Expr`, not an item of `Syntax`"
    );
}

#[test]
fn a_dependency_constructor_named_as_a_module_item_says_where_it_lives() {
    let out = common::errors_std_with(
        "use Std.Maybe (Maybe, Just)\ndef main = 1\n",
        meadow::Options::debug(),
    );
    assert_eq!(
        out,
        "`Just` is a constructor of `Maybe`, not an item of `Std.Maybe`"
    );
}

#[test]
fn a_dependency_type_path_imports_its_constructors() {
    let src = "use Std.Either.Either (Left)\n\
               def main = match Left 1 with | Left n -> n | Either.Right s -> 0\n";
    assert_eq!(common::eval_main_std(src), "1");
}

#[test]
fn a_constructor_the_type_does_not_have_is_reported() {
    let out = unit_errors(&[EXPR, ("", "use Syntax.Expr (Lit, Add)\ndef main = 1\n")]);
    assert_eq!(out, "`Expr` has no constructor `Add`");
}

#[test]
fn a_glob_on_a_module_is_reported() {
    let out = unit_errors(&[EXPR, ("", "use Syntax.*\ndef main = 1\n")]);
    assert_eq!(
        out,
        "`use Syntax.*` names a module; `.*` is for a type's constructors"
    );
}

#[test]
fn a_private_types_constructors_cannot_be_imported() {
    let out = unit_errors(&[
        (
            "Syntax",
            "data Expr = Lit Int\n@pub(pkg) fun lit n = Expr.Lit n\n",
        ),
        ("", "use Syntax.Expr.*\ndef main = 1\n"),
    ]);
    assert!(
        out.contains("`Expr` is private to module `Syntax`"),
        "{out}"
    );
}

// --- the package boundary ----------------------------------------------------

#[test]
fn only_a_plain_pub_is_exported() {
    // `@pub(pkg)` stops at the package: what a dependent sees is `@pub`.
    let (cp, _) =
        meadow::pipeline::compile_str("m", "@pub(pkg) fun inside x = x\n@pub fun outside y = y\n");
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
    assert_eq!(
        names,
        vec!["f".to_string()],
        "still exported, as it used to be"
    );
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
