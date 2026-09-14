//! Bit-shift operators, the bitwise primitives, and `Std.Num.Bits`.
//!
//! The bitwise operators work on every integer type, at that type's width. A
//! literal nothing pins down is a `BigInt`, which has no width at all -- so the
//! cases below that care about one say which type they mean.

mod common;
use common::{eval_expr, eval_expr_std, schemes};

#[test]
fn shift_operators() {
    insta::assert_snapshot!(eval_expr("1 << 4"), @"16");
    insta::assert_snapshot!(eval_expr("255 >> 2"), @"63");
    insta::assert_snapshot!(eval_expr("0 - 8 >> 1"), @"-4"); // arithmetic (sign-extending)
    insta::assert_snapshot!(eval_expr("ushr (toInt (0 - 1)) 60"), @"15"); // logical, at 64 bits
    insta::assert_snapshot!(eval_expr("ushr (toUInt8 255) 4"), @"15"); // and at 8
    insta::assert_snapshot!(eval_expr("toInt8 (0 - 128) >> 7"), @"-1");
    insta::assert_snapshot!(eval_expr("toUInt8 128 >> 7"), @"1");
}

#[test]
fn bitwise_prims() {
    insta::assert_snapshot!(eval_expr("bitAnd 12 10"), @"8");
    insta::assert_snapshot!(eval_expr("bitOr 12 10"), @"14");
    insta::assert_snapshot!(eval_expr("bitXor 12 10"), @"6");
    insta::assert_snapshot!(eval_expr("bitNot 0"), @"-1");
    insta::assert_snapshot!(eval_expr("bitNot (toUInt8 0)"), @"255");
    insta::assert_snapshot!(eval_expr("popCount 255"), @"8");
    insta::assert_snapshot!(eval_expr("popCount (toInt8 (0 - 1))"), @"8");
    insta::assert_snapshot!(eval_expr("bitWidth (toUInt16 0)"), @"16");
}

#[test]
fn a_shift_amount_is_an_int_whatever_is_shifted() {
    insta::assert_snapshot!(schemes("fun f x n = x << n\n"), @"f : forall n. n -> Int -> n
");
}

#[test]
fn std_bits_helpers() {
    insta::assert_snapshot!(eval_expr_std("toInt (lowMask 8)"), @"255");
    insta::assert_snapshot!(eval_expr_std("testBit 5 0"), @"True");
    insta::assert_snapshot!(eval_expr_std("testBit 5 1"), @"False");
    insta::assert_snapshot!(eval_expr_std("setBit 0 3"), @"8");
    insta::assert_snapshot!(eval_expr_std("clearBit 15 1"), @"13");
    insta::assert_snapshot!(eval_expr_std("flipBit 0 5"), @"32");
    insta::assert_snapshot!(eval_expr_std("byteOf 1 65535"), @"255");
    insta::assert_snapshot!(eval_expr_std("fromBytes4 1 2 3 4"), @"67305985");
    insta::assert_snapshot!(eval_expr_std("countTrailingZeros (toInt 8)"), @"3");
    insta::assert_snapshot!(eval_expr_std("countLeadingZeros (toInt 1)"), @"63");
    insta::assert_snapshot!(eval_expr_std("countLeadingZeros (toUInt8 1)"), @"7");
}
