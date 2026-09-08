//! Bit-shift operators / `Int` bitwise primitives and `Std.Bits`.

mod common;
use common::{eval_expr, eval_expr_std, schemes};

#[test]
fn shift_operators() {
    insta::assert_snapshot!(eval_expr("1 << 4"), @"16");
    insta::assert_snapshot!(eval_expr("255 >> 2"), @"63");
    insta::assert_snapshot!(eval_expr("0 - 8 >> 1"), @"-4"); // arithmetic (sign-extending)
    insta::assert_snapshot!(eval_expr("ushr (0 - 1) 60"), @"15"); // logical
}

#[test]
fn bitwise_prims() {
    insta::assert_snapshot!(eval_expr("bitAnd 12 10"), @"8");
    insta::assert_snapshot!(eval_expr("bitOr 12 10"), @"14");
    insta::assert_snapshot!(eval_expr("bitXor 12 10"), @"6");
    insta::assert_snapshot!(eval_expr("bitNot 0"), @"-1");
    insta::assert_snapshot!(eval_expr("popCount 255"), @"8");
}

#[test]
fn shift_ops_have_int_type() {
    insta::assert_snapshot!(schemes("def f = \\x -> \\n -> x << n\n"), @"f : Int -> Int -> Int
");
}

#[test]
fn std_bits_helpers() {
    insta::assert_snapshot!(eval_expr_std("lowMask 8"), @"255");
    insta::assert_snapshot!(eval_expr_std("testBit 5 0"), @"true");
    insta::assert_snapshot!(eval_expr_std("testBit 5 1"), @"false");
    insta::assert_snapshot!(eval_expr_std("setBit 0 3"), @"8");
    insta::assert_snapshot!(eval_expr_std("clearBit 15 1"), @"13");
    insta::assert_snapshot!(eval_expr_std("flipBit 0 5"), @"32");
    insta::assert_snapshot!(eval_expr_std("byteOf 1 65535"), @"255");
    insta::assert_snapshot!(eval_expr_std("fromBytes4 1 2 3 4"), @"67305985");
    insta::assert_snapshot!(eval_expr_std("countTrailingZeros 8"), @"3");
    insta::assert_snapshot!(eval_expr_std("countLeadingZeros 1"), @"63");
}
