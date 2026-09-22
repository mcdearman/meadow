//! `++`: an operator that is an ordinary name.
//!
//! Every operator is. `++` is bound like a value -- `fun (++) a b = ...` --
//! and `a ++ b` applies whatever `++` is in scope, so it is defined, exported,
//! imported and shadowed like any other name. `Std.String` binds it to `concat`
//! and the prelude re-exports it. (`tests/operators.rs` has the rest: fixity
//! declarations, and the operators that are trait methods.)

mod common;
use common::{errors, eval_expr_std, eval_main, eval_main_std, schemes, unit_errors};

#[test]
fn the_prelude_concatenates_strings_with_it() {
    assert_eq!(
        eval_expr_std(r#""hello" ++ ", " ++ "world""#),
        r#""hello, world""#
    );
    assert_eq!(eval_expr_std(r#"(++) "a" "b""#), r#""ab""#);
}

#[test]
fn it_binds_looser_than_application_and_tighter_than_comparison() {
    assert_eq!(eval_expr_std(r#""n=" ++ show 42"#), r#""n=42""#);
    assert_eq!(
        eval_expr_std(r#"("x" ++ "y" == "xy", "a" ++ "b" != "ab")"#),
        "(True, False)"
    );
}

#[test]
fn it_is_right_associative() {
    let src = "fun (++) a b = (a, b)\ndef main = 1 ++ 2 ++ 3\n";
    assert_eq!(eval_main(src), "(1, (2, 3))");
}

#[test]
fn a_program_can_define_its_own() {
    let src = "fun (++) a b = a + b\n\
               def nine = toInt 4 ++ 5\n";
    assert_eq!(schemes(src), "(++) : forall n. n -> n -> n\nnine : Int\n");
}

#[test]
fn a_local_definition_shadows_the_preludes() {
    let src = "fun (++) a b = b\ndef main = \"left\" ++ \"right\"\n";
    assert_eq!(eval_main_std(src), r#""right""#);
}

#[test]
fn it_is_imported_like_any_other_name() {
    let def = "@pub fun (++) a b = (b, a)\n";
    // Bare in the list, or in its own parentheses.
    for list in ["(++)", "((++))"] {
        let user = format!("use Ops {list}\ndef main = 1 ++ 2\n");
        assert_eq!(
            unit_errors(&[("", "mod Ops\n"), ("Ops", def), ("Main", &user)]),
            ""
        );
    }
    // Not imported, not in scope.
    let user = "def main = 1 ++ 2\n";
    assert_eq!(
        unit_errors(&[("", "mod Ops\n"), ("Ops", def), ("Main", user)]),
        "undefined variable: ++"
    );
}

#[test]
fn without_a_definition_it_is_unbound() {
    let errs = errors("def main = \"a\" ++ \"b\"\n");
    assert_eq!(errs, "undefined variable: ++");
}

#[test]
fn any_operator_can_be_bound() {
    // `++` is not special: every operator is a name.
    assert_eq!(eval_main("fun (<>) a b = a\ndef main = 1 <> 2\n"), "1");
    // Nor are the language's own, which a local definition shadows.
    assert_eq!(eval_main("fun (-) a b = a + b\ndef main = 1 - 2\n"), "3");
}
