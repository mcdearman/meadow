//! A module written where it is declared: `mod Name { … }`.
//!
//! A file is a module, and that is still the default: `mod Name` names the
//! one in `Name.mw`. With braces the module's declarations are written there
//! instead, as Rust's `mod name { … }`, and it is a module like any other --
//! its own namespace, its own visibility, reached by `use` from anywhere in
//! the package.
//!
//! And a module under the one being written qualifies by its path from
//! there, as Rust's does: after `mod Core`, `Core.name`; and for a module
//! `Expr` inside it, `Core.Expr.name`, `Core.Expr.Ty.Ctor` and a pattern
//! synonym `Core.Expr.P`.

mod common;
use common::{eval_unit, parse_ast, unit_errors};

/// `src` as the root module of a unit of its own, evaluated.
fn eval(src: &str) -> String {
    eval_unit(&[("", src)])
}

fn errors(src: &str) -> String {
    unit_errors(&[("", src)])
}

const CORE: &str = "\
@pub mod Core {
  fun secret n = n + 1

  @pub fun double n = secret n * 2 - 2

  @pub mod Expr {
    use Node.*

    @pub data Node = Int Int | Add Node Node

    @pub fun eval e =
      match e with
      | Int n -> n
      | Add a b -> eval a + eval b

    @pub pattern Lit n = Int n
    @pub pattern Two <- Int 2
  }
}
";

#[test]
fn a_function_of_an_inline_module_is_reached_by_its_name() {
    assert_eq!(eval(&format!("{CORE}def result = Core.double 4\n")), "8");
}

#[test]
fn a_module_inside_another_is_reached_by_the_path_to_it() {
    let src = format!(
        "{CORE}def result = Core.Expr.eval (Core.Expr.Node.Add (Core.Expr.Node.Int 2) (Core.Expr.Node.Int 5))\n"
    );
    assert_eq!(eval(&src), "7");
}

#[test]
fn a_constructor_is_matched_through_the_path_to_its_type() {
    let src = format!(
        "{CORE}fun which e = match e with | Core.Expr.Node.Int _ -> 1 | Core.Expr.Node.Add _ _ -> 2\n\
         def result = which (Core.Expr.Node.Add (Core.Expr.Node.Int 1) (Core.Expr.Node.Int 1))\n"
    );
    assert_eq!(eval(&src), "2");
}

#[test]
fn a_pattern_synonym_is_matched_and_built_through_the_path_to_its_module() {
    let src = format!(
        "{CORE}fun which e = match e with | Core.Expr.Two -> 0 | Core.Expr.Lit n -> n | _ -> 99\n\
         def result = (which (Core.Expr.Lit 2), which (Core.Expr.Lit 7))\n"
    );
    assert_eq!(eval(&src), "(0, 7)");
}

#[test]
fn what_an_inline_module_keeps_to_itself_is_not_reached() {
    let out = errors(&format!("{CORE}def result = Core.secret 1\n"));
    assert!(
        out.contains("`secret` is not exported by module `Core`"),
        "{out}"
    );
}

#[test]
fn a_synonym_taking_another_number_of_arguments_is_said_so() {
    let out = errors(&format!(
        "{CORE}fun which e = match e with | Core.Expr.Lit a b -> a | _ -> 0\ndef result = 1\n"
    ));
    assert!(out.contains("takes 1 argument, not 2"), "{out}");
}

#[test]
fn an_inline_module_is_used_like_one_with_a_file() {
    let modules = [
        ("", "mod Other\ndef result = Other.viaSibling\n"),
        (
            "Other",
            &*format!(
                "{CORE}use Other.Core.Expr as E\nuse Other.Core (double)\n\
                 @pub(pkg) def viaSibling = match E.Lit (double 3) with | E.Lit n -> n | _ -> 0\n"
            ),
        ),
    ];
    assert_eq!(eval_unit(&modules), "6");
}

#[test]
fn a_sibling_file_reaches_an_inline_module_by_its_path() {
    let modules = [
        (
            "",
            &*format!("mod Other\n{CORE}def result = Other.answer\n"),
        ),
        (
            "Other",
            "use Core.Expr as E\n@pub(pkg) def answer = E.eval (E.Lit 9)\n",
        ),
    ];
    assert_eq!(eval_unit(&modules), "9");
}

#[test]
fn a_macro_may_produce_a_module() {
    let src = "\
macro shout
  | ($name : ident, $n : expr) -> {
      @pub mod $name {
        @pub def loud = $n
      }
    }

shout!(Loud, 41)

def result = Loud.loud + 1
";
    assert_eq!(eval(src), "42");
}

#[test]
fn a_module_a_cfg_leaves_out_is_not_there() {
    let src = "\
@cfg(test)
mod Tests {
  @pub def broken = nothingOfThatName
}

def result = 3
";
    assert_eq!(eval(src), "3");
}

#[test]
fn an_empty_module_is_a_module() {
    assert_eq!(eval("mod Nothing {}\ndef result = 1\n"), "1");
}

#[test]
fn a_module_with_a_file_is_still_written_bare() {
    let ast = parse_ast("mod Tests\ndef x = 1\n");
    assert!(!ast.contains("Module("), "{ast}");
}
