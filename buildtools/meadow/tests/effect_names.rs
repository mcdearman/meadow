//! An effect's operations may be named what something elsewhere is named.
//!
//! The standard library's `State` has `get` and `put`, and every effect a
//! dependency declares used to be taken as declared here too: a program's own
//! `effect State s { get, put }` -- the tutorial's -- was refused as a
//! duplicate. And where two effects in scope shared an operation's name, the
//! one a handler answered was the first found in a hash map, so which it was
//! changed from one run of the compiler to the next.

mod common;
use common::{errors_std_with, run_main_std};
use meadow::{Engine, Options};

fn errors(src: &str) -> String {
    errors_std_with(src, Options::debug())
}

/// The tutorial's own `State`, handled the classic way.
const STATE: &str = "effect State s { get : () -> s, put : s -> () }\n\
                     fun counter () = let n = get () in let u = put (n + 1) in let m = get () in n + m\n\
                     def main =\n\
                     \x20 (handle counter () with {\n\
                     \x20   get u k -> \\s -> (k s) s,\n\
                     \x20   put v k -> \\s -> (k ()) v,\n\
                     \x20   return x -> \\s -> x\n\
                     \x20 }) 10\n";

#[test]
fn a_program_may_declare_the_tutorials_state() {
    assert_eq!(errors(STATE), "");
    assert_eq!(run_main_std(STATE, Engine::Vm), "21");
}

#[test]
fn which_effect_is_handled_is_the_same_every_time() {
    // Compiled again and again: the answer and the errors never change.
    let first = run_main_std(STATE, Engine::Vm);
    for _ in 0..8 {
        assert_eq!(run_main_std(STATE, Engine::Vm), first);
    }
    let ambiguous = "use Std.State (get)\n\
                     effect Mine { get : () -> Int }\n\
                     def main = handle get () + 1 with { get u k -> k 41 }\n";
    let said = errors(ambiguous);
    assert!(said.contains("ambiguous `get`"), "{said}");
    for _ in 0..8 {
        assert_eq!(errors(ambiguous), said, "the same message every time");
    }
}

#[test]
fn an_operation_may_share_a_prelude_functions_name() {
    for (op, want) in [("length", "31"), ("map", "31"), ("fold", "31")] {
        let src = format!(
            "effect Fx {{ {op} : Int -> Int }}\n\
             def main = handle {op} 3 + 1 with {{ {op} n k -> k (n * 10) }}\n"
        );
        assert_eq!(run_main_std(&src, Engine::Vm), want, "{op}");
    }
}

#[test]
fn the_standard_librarys_state_still_works_beside_a_programs_own() {
    // A program's `Mine.get` and the library's `State` in one module: the
    // handler answers the program's, and `runState` the library's.
    let src = "use Std.State (runState)\n\
               effect Mine { get : () -> Int }\n\
               def main = (handle get () + 1 with { get u k -> k 41 }, runState 0 (\\() -> 5))\n";
    assert_eq!(run_main_std(src, Engine::Vm), "(42, (5, 0))");
}

#[test]
fn one_module_declaring_an_operation_twice_is_still_an_error() {
    let e = errors(
        "effect A { ping : () -> Int }\n\
         effect B { ping : () -> Int }\n\
         def main = 0\n",
    );
    assert!(e.contains("operation `ping` is already defined"), "{e}");
}
