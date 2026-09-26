//! A user package may not declare a type or effect under a built-in name.
//!
//! These names -- `Bool`, `List`, `Int`, `Float`, `Char`, the sized ints, and
//! the effect labels like `Mut` -- are never package-qualified, and the back
//! ends give a value of one the built-in's representation. A user declaration
//! under such a name therefore aliases the built-in and its values are read as
//! the wrong thing: a non-exhaustive match at `-O2`, or a segfault on the
//! native runtimes. The resolver refuses them; only the standard library, which
//! defines the real ones, may.

mod common;
use common::errors as errors_standalone;
use common::{errors_std_with, run_main_std};
use meadow::Options;

fn errors(src: &str) -> String {
    errors_std_with(src, Options::debug())
}

#[test]
fn a_user_bool_is_refused() {
    // The confirmed miscompile: a third constructor made the word-literal
    // rewrite of `True`/`False` confuse tags, crashing at -O2 and on Silo.
    let out = errors("data Bool = False | Unknown | True\ndef main = 0\n");
    assert!(out.contains("built-in name"), "{out}");
}

#[test]
fn a_user_representation_type_is_refused() {
    for ty in ["Int", "Float", "Char", "UInt8", "List", "String", "Array"] {
        let out = errors(&format!("data {ty} = MkThing Int\ndef main = 0\n"));
        assert!(
            out.contains("built-in name"),
            "declaring `{ty}` should be refused, got: {out}"
        );
    }
}

#[test]
fn a_user_effect_named_mut_is_refused() {
    // The critical hole: handling a user `Mut` effect discharged the built-in
    // `Mut` label, so an effectful `newRef` looked pure and was generalized --
    // a polymorphic mutable cell.
    let out = errors(
        "effect Mut { dummy : () -> () }\n\
         fun hide act = handle act () with { dummy u k -> k () }\n\
         fun mkCell u = hide (\\() -> newRef [])\n\
         def main = 0\n",
    );
    assert!(out.contains("built-in name"), "{out}");
}

#[test]
fn a_reserved_effect_label_is_refused() {
    for e in ["Mut", "Console", "Stm", "Thread", "Fs"] {
        let out = errors(&format!("effect {e} {{ op : () -> () }}\ndef main = 0\n"));
        assert!(
            out.contains("built-in name"),
            "declaring effect `{e}` should be refused, got: {out}"
        );
    }
}

#[test]
fn an_ordinary_user_type_is_still_fine() {
    // The fix must not reject a type whose name is not built in.
    let out = errors("data Colour = Red | Green | Blue\ndef main = 0\n");
    assert!(
        !out.contains("built-in name") && !out.contains("!!"),
        "{out}"
    );
    let ran = run_main_std(
        "data Shape = Circle Int | Square Int\n\
         use Shape.*\n\
         fun area (s : Shape) : Int = match s with | Circle r -> r * r * 3 | Square w -> w * w\n\
         def main = area (Square 4)",
        meadow::Engine::Vm,
    );
    assert_eq!(ran, "16", "{ran}");
}

#[test]
fn a_user_effect_with_its_own_name_is_still_fine() {
    let out = errors(
        "effect Logger { note : String -> () }\n\
         fun quiet act = handle act () with { note s k -> k () }\n\
         def main = 0\n",
    );
    assert!(
        !out.contains("built-in name") && !out.contains("!!"),
        "{out}"
    );
}

#[test]
fn a_program_built_without_the_standard_library_declares_its_own() {
    // With no dependencies there is no standard library to alias: the program
    // stands in for it, and declares what a primitive's type names (`Maybe`)
    // or what it handles itself (`Stm`).
    let out = errors_standalone(
        "data Maybe a = None | Just a
\
         effect Stm { retrySignal : () -> () }
\
         effect St { get : () -> Int }
\
         def main = 0
",
    );
    assert!(!out.contains("built-in name"), "{out}");
}

#[test]
fn a_standalone_program_still_may_not_redeclare_a_representation() {
    // `Bool`, `List`, `Int` and the rest have a fixed layout in every back end
    // whether or not the standard library is there.
    for ty in ["Bool", "List", "Int", "String"] {
        let out = errors_standalone(&format!(
            "data {ty} = A | B | C
def main = 0
"
        ));
        assert!(
            out.contains("built-in name"),
            "declaring `{ty}` should be refused, got: {out}"
        );
    }
}

#[test]
fn with_the_standard_library_maybe_and_result_are_refused() {
    for ty in ["Maybe", "Result", "Vector"] {
        let out = errors(&format!(
            "data {ty} a = Nope | Yep a
def main = 0
"
        ));
        assert!(
            out.contains("built-in name"),
            "declaring `{ty}` alongside Std should be refused, got: {out}"
        );
    }
}
