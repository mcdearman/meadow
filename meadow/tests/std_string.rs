//! `Std.String` — string operations over the UTF-8 byte bridge.
//!
//! Offsets and lengths are in bytes, so the multi-byte cases are worth pinning
//! down alongside the ASCII ones.

mod common;
use common::eval_main_std;

fn s(body: &str) -> String {
    eval_main_std(&format!("use Std.String as S\ndef main = {body}\n"))
}

// --- building ----------------------------------------------------------------

#[test]
fn concat_joins_two_strings() {
    assert_eq!(s(r#"S.concat "foo" "bar""#), r#""foobar""#);
    assert_eq!(s(r#"S.concat "" "x""#), r#""x""#);
}

#[test]
fn concat_all_and_join() {
    assert_eq!(s(r#"S.concatAll ["a", "b", "c"]"#), r#""abc""#);
    assert_eq!(s(r#"S.join ", " ["a", "b", "c"]"#), r#""a, b, c""#);
    // A single element gets no separator, and none gets nothing.
    assert_eq!(s(r#"S.join ", " ["only"]"#), r#""only""#);
    assert_eq!(s(r#"S.join ", " []"#), r#""""#);
}

#[test]
fn repeat_repeats() {
    assert_eq!(s(r#"S.repeat "ab" 3"#), r#""ababab""#);
    assert_eq!(s(r#"S.repeat "ab" 0"#), r#""""#);
}

// --- measuring and slicing ---------------------------------------------------

#[test]
fn byte_length_counts_bytes_not_characters() {
    assert_eq!(s(r#"S.byteLength "hello""#), "5");
    assert_eq!(s(r#"S.isEmpty """#), "true");
    // "é" is two bytes in UTF-8 — the length is honest about that.
    assert_eq!(s("S.byteLength \"\u{e9}\""), "2");
}

#[test]
fn slice_take_drop_clamp_to_the_ends() {
    assert_eq!(s(r#"S.slice "hello" 1 3"#), r#""el""#);
    assert_eq!(s(r#"S.take "hello" 2"#), r#""he""#);
    assert_eq!(s(r#"S.drop "hello" 2"#), r#""llo""#);
    // Out-of-range offsets clamp rather than failing.
    assert_eq!(s(r#"S.slice "hi" 0 99"#), r#""hi""#);
    assert_eq!(s(r#"S.slice "hi" 5 9"#), r#""""#);
    assert_eq!(s(r#"S.drop "hi" 99"#), r#""""#);
}

#[test]
fn byte_at() {
    assert_eq!(s(r#"S.byteAt "abc" 1"#), "Just(98)");
    assert_eq!(s(r#"S.byteAt "abc" 9"#), "None");
    assert_eq!(s(r#"S.byteAt "abc" (0 - 1)"#), "None");
}

// --- searching ---------------------------------------------------------------

#[test]
fn index_of_finds_the_first_occurrence() {
    assert_eq!(s(r#"S.indexOf "l" "hello""#), "Just(2)");
    assert_eq!(s(r#"S.indexOf "lo" "hello""#), "Just(3)");
    assert_eq!(s(r#"S.indexOf "z" "hello""#), "None");
    // An empty needle sits at the start.
    assert_eq!(s(r#"S.indexOf "" "hello""#), "Just(0)");
}

#[test]
fn contains_starts_and_ends() {
    assert_eq!(s(r#"S.contains "ell" "hello""#), "true");
    assert_eq!(s(r#"S.contains "z" "hello""#), "false");
    assert_eq!(s(r#"S.startsWith "he" "hello""#), "true");
    assert_eq!(s(r#"S.startsWith "hello!" "hello""#), "false");
    assert_eq!(s(r#"S.endsWith "llo" "hello""#), "true");
    assert_eq!(s(r#"S.endsWith "he" "hello""#), "false");
}

#[test]
fn split_cuts_at_every_separator() {
    assert_eq!(s(r#"S.split "," "a,b,c""#), r#"["a", "b", "c"]"#);
    // Adjacent separators leave empty pieces, as they should.
    assert_eq!(s(r#"S.split "," "a,,b""#), r#"["a", "", "b"]"#);
    assert_eq!(s(r#"S.split "," "a""#), r#"["a"]"#);
    assert_eq!(s(r#"S.split ", " "a, b""#), r#"["a", "b"]"#);
    // An empty separator has no sensible answer, so the string comes back whole
    // rather than looping forever.
    assert_eq!(s(r#"S.split "" "abc""#), r#"["abc"]"#);
}

// --- whitespace --------------------------------------------------------------

#[test]
fn trimming() {
    assert_eq!(s(r#"S.trim "  hi  ""#), r#""hi""#);
    assert_eq!(s("S.trim \"\\t\\nhi\\r\\n\""), r#""hi""#);
    assert_eq!(s(r#"S.trimStart "  hi  ""#), r#""hi  ""#);
    assert_eq!(s(r#"S.trimEnd "  hi  ""#), r#""  hi""#);
    // All-whitespace collapses to empty, without running off either end.
    assert_eq!(s(r#"S.trim "   ""#), r#""""#);
}

#[test]
fn lines_ignores_one_trailing_newline() {
    assert_eq!(s("S.lines \"a\\nb\\nc\""), r#"["a", "b", "c"]"#);
    assert_eq!(s("S.lines \"a\\nb\\n\""), r#"["a", "b"]"#);
    assert_eq!(s(r#"S.lines """#), "[]");
}

// --- case --------------------------------------------------------------------

#[test]
fn ascii_case_conversion_leaves_the_rest_alone() {
    assert_eq!(s(r#"S.toUpper "Hello, World!""#), r#""HELLO, WORLD!""#);
    assert_eq!(s(r#"S.toLower "Hello, World!""#), r#""hello, world!""#);
    assert_eq!(s(r#"S.toUpper "123""#), r#""123""#);
}

// --- numbers -----------------------------------------------------------------

#[test]
fn from_int_renders_base_ten() {
    assert_eq!(s("S.fromInt 0"), r#""0""#);
    assert_eq!(s("S.fromInt 1234"), r#""1234""#);
    assert_eq!(s("S.fromInt (0 - 42)"), r#""-42""#);
}

#[test]
fn to_int_reads_base_ten_or_nothing() {
    assert_eq!(s(r#"S.toInt "1234""#), "Just(1234)");
    assert_eq!(s(r#"S.toInt "-42""#), "Just(-42)");
    assert_eq!(s(r#"S.toInt "0""#), "Just(0)");
    // Anything that is not entirely digits is rejected outright.
    assert_eq!(s(r#"S.toInt "12x""#), "None");
    assert_eq!(s(r#"S.toInt "x12""#), "None");
    assert_eq!(s(r#"S.toInt """#), "None");
    assert_eq!(s(r#"S.toInt "-""#), "None");
}

#[test]
fn int_round_trips() {
    assert_eq!(s("S.toInt (S.fromInt 987654)"), "Just(987654)");
    assert_eq!(s("S.toInt (S.fromInt (0 - 987654))"), "Just(-987654)");
}

// --- bytes -------------------------------------------------------------------

#[test]
fn bytes_round_trip() {
    assert_eq!(s(r#"S.fromBytes (S.toBytes "hello")"#), r#""hello""#);
    assert_eq!(s(r#"S.toBytes "abc""#), "#[97, 98, 99]");
}
