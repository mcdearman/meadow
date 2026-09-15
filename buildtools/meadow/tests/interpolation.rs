//! `"a ${e} b"`: string interpolation, on every machine, against `Std`.

mod common;
use common::{cek_main_std, errors, run_main_std};

/// The program's answer on the CEK machine, required to be the VM's and the
/// JIT's too.
fn everywhere(src: &str) -> String {
    let cek = cek_main_std(src);
    assert_eq!(
        cek,
        run_main_std(src, meadow::Engine::Vm),
        "CEK vs VM\n{src}"
    );
    assert_eq!(
        cek,
        run_main_std(src, meadow::Engine::Jit),
        "CEK vs JIT\n{src}"
    );
    cek
}

#[test]
fn a_hole_renders_any_value_and_a_string_without_quotes() {
    assert_eq!(
        everywhere(
            r#"def main = "${"text"} ${[1, 2]} ${Just 'x'} ${1.5} ${()} ${{ a = 1 }} ${show "q"}""#
        ),
        r#""text [1, 2] Just('x') 1.5 () { a = 1 } \"q\"""#
    );
}

#[test]
fn a_hole_holds_any_expression() {
    assert_eq!(
        everywhere(
            r#"use Std.String as S
               fun greet (name : String) (age : Int) = "Hello ${S.toUpper name}, next year you are ${age + 1}."
               def main = (greet "ann" 41, "${ {a = 1}.a } ${if True then "y" else "n"} ${match Just 3 with | Just n -> n | None -> 0}")"#
        ),
        r#"("Hello ANN, next year you are 42.", "1 y 3")"#
    );
}

#[test]
fn strings_in_holes_have_holes_of_their_own() {
    assert_eq!(
        everywhere(r#"def main = "a ${"b ${"c ${1}"}"} d""#),
        r#""a b c 1 d""#
    );
}

#[test]
fn a_dollar_is_text_unless_it_opens_a_hole() {
    assert_eq!(
        everywhere(r#"def main = ("costs $5", "\${not a hole}", "\$${5}", "$")"#),
        r#"("costs $5", "${not a hole}", "$5", "$")"#
    );
}

#[test]
fn a_program_that_defines_display_does_not_change_what_a_hole_means() {
    assert_eq!(
        everywhere(
            r#"fun display x = "not this"
                      def main = "${42}""#
        ),
        r#""42""#
    );
}

#[test]
fn a_hole_spans_lines() {
    assert_eq!(
        everywhere("def main = \"sum: ${\n  1 +\n  2\n}\""),
        r#""sum: 3""#
    );
}

#[test]
fn broken_interpolations_are_reported() {
    for (src, want) in [
        (r#"def main = "${}""#, "needs an expression"),
        (r#"def main = "${1 + 2""#, "never closed"),
        (r#"def main = "${undefinedName}""#, "undefinedName"),
    ] {
        let got = errors(src);
        assert!(got.contains(want), "{src}: {got}");
    }
}
