//! `String` <-> byte-array primitives and `Std.Bytes`.

mod common;
use common::{eval_expr, eval_expr_std, eval_main_std, schemes};

#[test]
fn string_byte_bridge() {
    insta::assert_snapshot!(eval_expr("stringToBytes \"AB\""), @"#[65, 66]");
    insta::assert_snapshot!(eval_expr("bytesToString #[72, 105]"), @r#""Hi""#);
    // multi-byte UTF-8 round-trips
    insta::assert_snapshot!(eval_expr("bytesToString (stringToBytes \"h\u{e9}llo\")"), @r#""héllo""#);
}

#[test]
fn hex() {
    insta::assert_snapshot!(eval_expr("bytesToHex #[0, 15, 255, 171]"), @r#""000fffab""#);
    insta::assert_snapshot!(eval_expr("bytesFromHex \"000fffab\""), @"Just(#[0, 15, 255, 171])");
    insta::assert_snapshot!(eval_expr("bytesFromHex \"00F\""), @"None"); // odd length
    insta::assert_snapshot!(eval_expr("bytesFromHex \"zz\""), @"None"); // not hex
    insta::assert_snapshot!(eval_expr("bytesToHex (stringToBytes \"Hi\")"), @r#""4869""#);
}

#[test]
fn hex_prim_types() {
    insta::assert_snapshot!(schemes("def a = bytesToHex\ndef b = bytesFromHex\n"), @r"
    a : #[Int] -> String
    b : String -> Maybe #[Int]
    ");
}

#[test]
fn std_bytes_accessors() {
    insta::assert_snapshot!(eval_expr_std("bytesLength #[1, 2, 3]"), @"3");
    insta::assert_snapshot!(eval_expr_std("bytesGet #[1, 2, 3] 1"), @"Just(2)");
    insta::assert_snapshot!(eval_expr_std("bytesGet #[1, 2, 3] 9"), @"None");
    insta::assert_snapshot!(eval_expr_std("bytesSet #[1, 2, 3] 1 256"), @"#[1, 0, 3]"); // masked
    insta::assert_snapshot!(eval_expr_std("bytesPush #[1] 513"), @"#[1, 1]"); // masked
}

#[test]
fn std_bytes_multibyte() {
    insta::assert_snapshot!(eval_expr_std("bytesGetU16LE #[1, 2] 0"), @"513");
    insta::assert_snapshot!(eval_expr_std("bytesGetU16BE #[1, 2] 0"), @"258");
    insta::assert_snapshot!(eval_expr_std("bytesGetU32LE #[1, 0, 0, 0] 0"), @"1");
    insta::assert_snapshot!(eval_expr_std("bytesGetU32BE #[0, 0, 0, 1] 0"), @"1");
    // write then read back
    insta::assert_snapshot!(eval_main_std(
        "def b = bytesSetU32LE #[0, 0, 0, 0] 0 305419896\n\
         def main = (b, bytesGetU32LE b 0)\n"
    ), @"(#[120, 86, 52, 18], 305419896)");
    insta::assert_snapshot!(eval_main_std(
        "def b = bytesSetU16BE #[0, 0] 0 258\n\
         def main = (b, bytesGetU16BE b 0)\n"
    ), @"(#[1, 2], 258)");
}
