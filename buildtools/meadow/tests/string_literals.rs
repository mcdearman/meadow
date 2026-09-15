//! Escapes and raw strings, on every machine, against `Std`. Interpolation has a
//! file of its own (`interpolation.rs`).

mod common;
use common::{cek_main_std, errors, run_main_std};

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
fn escapes_mean_their_characters() {
    assert_eq!(
        everywhere(
            r#"use Std.String as S
               def main = (S.byteLength "\a\b\f\v\e", "\x41\x7e", "caf\u{e9}", S.charLength "\u{1F600}", '\u{3bb}', '\x41')"#
        ),
        r#"(5, "A~", "café", 1, 'λ', 'A')"#
    );
}

#[test]
fn a_backslash_at_the_end_of_a_line_joins_it_to_the_next() {
    assert_eq!(
        everywhere("def main = \"one \\\n      two\""),
        r#""one two""#
    );
}

#[test]
fn raw_strings_hold_backslashes_quotes_and_dollars_as_they_are() {
    assert_eq!(
        everywhere(r###"def main = (r"C:\dir\${x}", r#"say "hi""#, r##"a "# b"##, r"" == "")"###),
        r##"("C:\\dir\\${x}", "say \"hi\"", "a \"# b", True)"##
    );
}

#[test]
fn a_raw_string_may_span_lines() {
    assert_eq!(everywhere("def main = r\"a\n  b\""), r#""a\n  b""#);
}

#[test]
fn bad_escapes_are_errors() {
    for (src, want) in [
        (r#"def main = "\q""#, "not an escape"),
        (r#"def main = "\xFF""#, "past ASCII"),
        (r#"def main = "\u{110000}""#, "not a Unicode character"),
        (r#"def main = "\u{}""#, "one to six hex digits"),
        (r##"def main = r#"never closed""##, "never closed"),
    ] {
        let got = errors(src);
        assert!(got.contains(want), "{src}: {got}");
    }
}
