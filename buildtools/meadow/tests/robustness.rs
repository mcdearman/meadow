//! Every way a Meadow program can reach the edge of the runtime.
//!
//! The rule these enforce: a Meadow program is *input*, and no input should be
//! able to panic the interpreter. Every case below must come back as a runtime
//! error the driver can print — never an index-out-of-bounds, an overflow panic,
//! or a hang. A panic here would be a crash in `meadow run`, and in the language
//! server a crash of the editor's backend.

mod common;
use common::{eval_main_std, eval_expr_std};

/// A runtime error, not a panic and not a wrong answer.
fn fails_with(expr: &str, needle: &str) {
    let out = eval_expr_std(expr);
    assert!(
        out.contains(needle),
        "expected an error mentioning {needle:?}, got: {out}"
    );
}

// --- arithmetic --------------------------------------------------------------

#[test]
fn division_and_modulo_by_zero_are_errors() {
    fails_with("1 / 0", "division by zero");
    fails_with("1 % 0", "modulo by zero");
    // Through a variable, so it cannot be folded away at compile time.
    assert!(eval_main_std("fun d a b = a / b\ndef main = d 1 0\n").contains("division by zero"));
}

#[test]
fn integer_overflow_wraps_rather_than_panicking() {
    // Rust would panic on overflow in a debug build; these must wrap instead.
    assert_eq!(eval_expr_std("9223372036854775807 + 1"), "-9223372036854775808");
    assert_eq!(eval_expr_std("0 - 9223372036854775807 - 2"), "9223372036854775807");
    assert_eq!(eval_expr_std("9223372036854775807 * 2"), "-2");
}

#[test]
fn a_huge_exponent_is_refused_not_attempted() {
    fails_with("2 ^ 9223372036854775807", "u32");
}

#[test]
fn shifts_past_the_word_size_do_not_panic() {
    // `wrapping_shl` masks the distance, so these are defined rather than UB —
    // odd, but the important part is that nothing crashes.
    for e in ["shl 1 64", "shr 1 64", "ushr 1 64", "shl 1 (0 - 1)"] {
        let out = eval_expr_std(e);
        assert!(
            !out.contains("panic") && !out.is_empty(),
            "{e} produced {out}"
        );
    }
}

// --- arrays ------------------------------------------------------------------

#[test]
fn out_of_bounds_array_access_is_an_error() {
    fails_with("arrayGet #[1, 2] 5", "out of bounds");
    fails_with("arraySet #[1, 2] 5 0", "out of bounds");
}

#[test]
fn a_negative_index_is_rejected_before_it_becomes_huge() {
    // `as usize` on a negative would wrap to an enormous index.
    fails_with("arrayGet #[1, 2] (0 - 1)", "negative");
    fails_with("arraySet #[1, 2] (0 - 1) 0", "negative");
}

#[test]
fn popping_an_empty_array_is_an_error_not_a_panic() {
    fails_with("arrayPop #[]", "empty");
}

#[test]
fn slice_bounds_are_clamped_rather_than_trusted() {
    assert_eq!(eval_expr_std("arraySlice #[1, 2, 3] 0 99"), "#[1, 2, 3]");
    assert_eq!(eval_expr_std("arraySlice #[1, 2, 3] 99 0"), "#[]");
    assert_eq!(eval_expr_std("arraySlice #[1, 2, 3] (0 - 5) 2"), "#[1, 2]");
}

// --- strings and bytes -------------------------------------------------------

#[test]
fn invalid_utf8_bytes_do_not_panic() {
    // `bytesToString` is lossy rather than fatal: 0xFF is not valid UTF-8.
    let out = eval_expr_std("bytesToString #[255, 254]");
    assert!(!out.is_empty() && !out.contains("panic"), "got {out}");
}

#[test]
fn byte_values_outside_a_byte_do_not_corrupt_anything() {
    let out = eval_expr_std("bytesToString #[300, 0 - 5]");
    assert!(!out.contains("panic"), "got {out}");
}

#[test]
fn malformed_hex_is_rejected() {
    assert_eq!(eval_expr_std("bytesFromHex \"zz\""), "None");
    assert_eq!(eval_expr_std("bytesFromHex \"abc\""), "None");
    assert_eq!(eval_expr_std("bytesFromHex \"\""), "Just(#[])");
}

// --- references --------------------------------------------------------------

#[test]
fn a_reference_cannot_be_made_to_contain_itself() {
    // `setRef r r` needs `a = Ref a`, which the occurs check refuses — so the
    // cyclic structure that would hang `show` or `==` cannot be built.
    let out = eval_main_std("def main = let r = newRef 0 in setRef r r\n");
    assert!(
        out.contains("compile errors"),
        "a self-referential cell should not typecheck: {out}"
    );
}

#[test]
fn reading_a_cell_of_a_cell_is_fine() {
    assert_eq!(
        eval_main_std("def main = getRef (getRef (newRef (newRef 5)))\n"),
        "5"
    );
}

// --- patterns and effects ----------------------------------------------------

#[test]
fn a_failed_match_is_an_error_not_a_panic() {
    let out = eval_main_std(
        "data T = A | B\nfun f x = match x with | A -> 1\ndef main = f B\n",
    );
    assert!(out.contains("non-exhaustive"), "got {out}");
}

#[test]
fn an_unhandled_effect_names_itself() {
    let out = eval_main_std(
        "effect E { op : Unit -> Int }\ndef main = op ()\n",
    );
    assert!(out.contains("unhandled effect") && out.contains("op"), "got {out}");
}

#[test]
fn calling_a_non_function_is_an_error() {
    let out = eval_main_std("fun apply f = f 1\ndef main = apply 2\n");
    assert!(
        out.contains("compile errors") || out.contains("not a function"),
        "got {out}"
    );
}

// --- the standard library's own edges ----------------------------------------

#[test]
fn string_operations_clamp_rather_than_index_blindly() {
    let src = "use Std.String as S\n\
               def main = (S.slice \"hi\" 99 200, S.drop \"hi\" 99, S.take \"hi\" 0, S.byteAt \"\" 0)\n";
    assert_eq!(eval_main_std(src), "(\"\", \"\", \"\", None)");
}

#[test]
fn path_operations_survive_nonsense_input() {
    let src = "use Std.Path as P\n\
               def main = (P.extension \"\", P.fileName \"\", P.normalize \"\", P.join \"\" \"\")\n";
    let out = eval_main_std(src);
    assert!(!out.contains("panic") && !out.contains("error"), "got {out}");
}

#[test]
fn json_rejects_deeply_nested_input_without_crashing() {
    // 200 nested arrays. Should parse or fail cleanly — not blow the Rust stack.
    let src = "use Std.Json as J\n\
               use Std.String as S\n\
               fun nest n = if n <= 0 then \"1\" else S.concatAll [\"[\", nest (n - 1), \"]\"]\n\
               def main = match J.parse (nest 200) with | Ok v -> True | Err e -> True\n";
    assert_eq!(eval_main_std(src), "true");
}

#[test]
fn sorting_pathological_input_terminates() {
    // All-equal and reversed are the inputs that make a naive quicksort
    // quadratic or, with a bad partition, loop forever.
    let src = "use Std.Sort (sort, isSorted)\n\
               use Std.Collections.Vector as V\n\
               def main =\n\
               \x20 ( isSorted (sort (V.replicate 500 1))\n\
               \x20 , isSorted (sort (V.reverse (V.range 0 500)))\n\
               \x20 )\n";
    assert_eq!(eval_main_std(src), "(true, true)");
}

// --- the known depth limit ---------------------------------------------------
//
// `Value::Ctor` holds a plain `Vec<Value>`, not an `Rc` — so binding a tail in
// `| Cons x rest ->` deep-copies the whole remaining list, recursively. Two
// consequences: `List` operations are O(n²), and past roughly 2000 elements the
// recursive `Value::clone` overflows the *Rust* stack and aborts the process —
// not a catchable error, and not something a Meadow program can guard against.
//
// `Vector` is unaffected: it is a tree, so it is only ~log32(n) deep. A 100_000
// element `Vector` is fine.
//
// These tests pin the sizes that work. If the limit ever regresses below them
// this fails; the fix is to share `Ctor`'s payload behind an `Rc` so that
// binding a tail is a refcount bump rather than a copy.

#[test]
fn lists_of_a_workable_size_are_safe() {
    let src = "use Std.Collections.List as L\n\
               def main = L.length (L.range 0 1000)\n";
    assert_eq!(eval_main_std(src), "1000");
}

#[test]
fn vectors_are_not_subject_to_the_list_depth_limit() {
    // The tree shape is what makes this fine at a size that would abort for a
    // `List` fifty times smaller.
    let src = "use Std.Collections.Vector as V\n\
               def main = V.len (V.range 0 50000)\n";
    assert_eq!(eval_main_std(src), "50000");
}

#[test]
fn deep_recursion_itself_is_fine() {
    // The continuation lives on the heap, so recursion depth is not the problem
    // — 200_000 frames, non-tail, is fine. Only the data structure is.
    let src = "fun go i = if i <= 0 then 0 else 1 + go (i - 1)\ndef main = go 200000\n";
    assert_eq!(eval_main_std(src), "200000");
}
