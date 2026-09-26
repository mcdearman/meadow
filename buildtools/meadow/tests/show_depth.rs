//! `show` walks a value's structure, and a value deep enough -- a long chain
//! of a user data type, or a cycle through a `Ref` -- used to overflow the
//! native stack and abort the process. It now stops at a bounded depth with an
//! ellipsis. Ordinary long lists are flattened and still print in full.
//!
//! These run on the bytecode VM, which is the engine `meadow run` uses.

mod common;
use common::run_main_std;

fn vm(src: &str) -> String {
    run_main_std(src, meadow::Engine::Vm)
}

const LIST: &str = "use L.*\n\
     data L = N | C Int L\n\
     fun build (i : Int) (acc : L) : L = if i == 0 then acc else build (i - 1) (C i acc)\n";

#[test]
fn a_shallow_user_value_prints_in_full() {
    let out = vm(&format!("{LIST}def main = show (build 3 N)"));
    // The harness renders the resulting `String` value with quotes.
    assert_eq!(out, "\"C(1, C(2, C(3, N)))\"", "{out}");
    assert!(
        !out.contains('…'),
        "a shallow value is not truncated: {out}"
    );
}

#[test]
fn a_deep_user_value_is_bounded_not_a_crash() {
    // 5000 deep: this is what aborted the process before. It must return, and
    // the ellipsis shows the depth guard fired rather than the stack blowing.
    let out = vm(&format!("{LIST}def main = show (build 5000 N)"));
    assert!(
        out.contains('…'),
        "expected a bounded render, got {} chars",
        out.len()
    );
    assert!(
        out.len() < 100_000,
        "the render should be bounded, got {} chars",
        out.len()
    );
}

#[test]
fn an_ordinary_long_list_still_prints_in_full() {
    // The builtin `List` is flattened, so its length does not count against the
    // depth guard: a 20000-element list prints every element.
    let out = vm("use Std.Collections.List as List\n\
         def main = let xs = List.range 0 20000 in stringByteLength (show xs)");
    assert!(
        !out.contains('…'),
        "a flat list should not be truncated: {out}"
    );
    assert!(
        out.parse::<i64>().map(|n| n > 20000).unwrap_or(false),
        "expected the length of a fully-printed list, got {out}"
    );
}

#[test]
fn a_cycle_through_a_ref_terminates() {
    // A self-referential value is infinitely deep; showing it must stop rather
    // than loop or overflow.
    let out = vm("use Std.Ref\n\
         use Std.Maybe.Maybe.*\n\
         data Node = Node (Ref (Maybe Node))\n\
         def main =\n\
         \x20 let r = newRef None in\n\
         \x20 let n = Node r in\n\
         \x20 let u = setRef r (Just n) in\n\
         \x20 show n");
    assert!(
        out.contains('…'),
        "a cycle should be cut off with an ellipsis, got {out}"
    );
    assert!(
        out.len() < 100_000,
        "a cycle's render should be bounded, got {} chars",
        out.len()
    );
}
