//! The `Char` type: literals, escapes, matching, and the `String` bridge.

mod common;
use common::{eval_main, eval_main_std, schemes};

// --- literals ----------------------------------------------------------------

#[test]
fn a_char_literal_has_type_char() {
    assert_eq!(schemes("def c = 'a'\n"), "c : Char\n");
}

#[test]
fn escapes_are_decoded_not_taken_literally() {
    // The character inside `'\n'` is a newline, not a backslash — which is what
    // taking the second character of the literal outright would give.
    assert_eq!(
        eval_main(r"def main = (charCode '\n', charCode '\t', charCode '\\', charCode '\'')"),
        "(10, 9, 92, 39)"
    );
}

#[test]
fn a_char_holds_a_scalar_not_a_byte() {
    assert_eq!(eval_main("def main = charCode 'é'\n"), "233");
}

// --- equality and matching ---------------------------------------------------

#[test]
fn chars_compare_structurally() {
    assert_eq!(
        eval_main("def main = ('a' == 'a', 'a' == 'b', 'a' != 'b')\n"),
        "(true, false, true)"
    );
}

#[test]
fn a_char_literal_is_a_pattern() {
    assert_eq!(
        eval_main(
            "fun f c = match c with | 'a' -> 1 | '\\n' -> 2 | _ -> 0\n\
             def main = (f 'a', f '\\n', f 'z')\n"
        ),
        "(1, 2, 0)"
    );
}

#[test]
fn matching_on_chars_is_checked_for_exhaustiveness() {
    use meadow::Options;
    let out = common::errors_with(
        "fun f c = match c with | 'a' -> 1\ndef main = f 'a'\n",
        Options::release(),
    );
    assert!(out.contains("non-exhaustive"), "got: {out}");
}

// --- primitives --------------------------------------------------------------

#[test]
fn codes_convert_both_ways() {
    assert_eq!(
        eval_main("def main = (charCode 'A', charFromCode 97, charFromCode (charCode 'é'))\n"),
        "(65, 'a', 'é')"
    );
}

#[test]
fn a_code_that_is_not_a_scalar_fails_loudly() {
    // Surrogates and anything above U+10FFFF are not characters, and there is no
    // `Char` to return for them.
    assert!(eval_main("def main = charFromCode 1114112\n").contains("not a Unicode scalar"));
    assert!(eval_main("def main = charFromCode 55296\n").contains("not a Unicode scalar"));
}

#[test]
fn strings_decode_to_chars_and_back() {
    assert_eq!(
        eval_main("def main = charsToString (stringToChars \"round trip é\")\n"),
        "\"round trip é\""
    );
    assert_eq!(
        eval_main("def main = stringToChars \"hé\"\n"),
        "#['h', 'é']"
    );
}

#[test]
fn show_renders_a_char_as_its_literal() {
    assert_eq!(eval_main("def main = (show 'x', show '\\n')\n"), r#"("'x'", "'\\n'")"#);
}

// --- Std.Char ----------------------------------------------------------------

#[test]
fn classification_is_ascii_only_and_says_so() {
    assert_eq!(
        eval_main_std(
            "use Std.Char as C\n\
             def main = (C.isAlpha 'a', C.isAlpha 'é', C.isAscii 'é', C.toUpper 'é')\n"
        ),
        "(true, false, false, 'é')"
    );
}

#[test]
fn std_char_converts_case_and_digits() {
    assert_eq!(
        eval_main_std(
            "use Std.Char as C\n\
             def main = (C.toUpper 'a', C.toLower 'A', C.digitToInt '7', C.hexDigitToInt 'f')\n"
        ),
        "('A', 'a', Just(7), Just(15))"
    );
}

// --- the String bridge -------------------------------------------------------

#[test]
fn byte_length_and_char_length_differ() {
    assert_eq!(
        eval_main_std(
            "use Std.String as S\n\
             def main = (S.byteLength \"héllo\", S.charLength \"héllo\")\n"
        ),
        "(6, 5)"
    );
}

#[test]
fn chars_round_trip_through_a_vector() {
    assert_eq!(
        eval_main_std(
            "use Std.String as S\n\
             def main = (S.chars \"hi\", S.fromChars ['h', 'i'], S.charAt \"héllo\" 1)\n"
        ),
        "(['h', 'i'], \"hi\", Just('é'))"
    );
}
