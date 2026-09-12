//! One spelling, several meanings, and the types to tell them apart.
//!
//! A module's own top-level names and the names its `use`s bring in live in one
//! layer, where a clash is not settled by import order but left to inference:
//! whichever candidate's type fits the context is the one meant. When none
//! does, or more than one still does once the definition is fully inferred,
//! that is an error that lists what was in scope.
//!
//! Locals still shadow, and the prelude is still shadowed rather than
//! overloaded -- defining your own `map` should not make every use of it a
//! question.

mod common;
use common::{errors_std_with, eval_main_std, eval_unit, unit_errors};
use meadow::Options;

/// A module with its own `Just`, next to `Std.Maybe`'s.
const BOX: &str = "use Std.Maybe (Maybe, Just, None)\n\
                   data Box = Just Int | Empty\n\
                   fun unbox (b : Box) = match b with | Just n -> n | Empty -> 0\n";

fn errors(src: &str) -> String {
    errors_std_with(src, Options::release())
}

// --- choosing by type ---------------------------------------------------------

#[test]
fn a_constructor_clashing_with_an_import_is_chosen_by_type() {
    let src = format!(
        "{BOX}def main = (unbox (Just 3), match Just \"s\" with | Just s -> s | None -> \"\")\n"
    );
    assert_eq!(eval_main_std(&src), "(3, \"s\")");
    assert_eq!(errors(&src), "");
}

#[test]
fn a_pattern_is_chosen_by_the_scrutinee() {
    let src = format!(
        "{BOX}fun orZero (m : Maybe Int) = match m with | Just n -> n | None -> 0\n\
         def main = orZero (Just 4) + unbox Empty\n"
    );
    assert_eq!(eval_main_std(&src), "4");
}

#[test]
fn names_tied_together_are_chosen_together() {
    // Two `size`s and two `Just`s: no single name settles it, but only one pair
    // has a type.
    let src = "use Std.Maybe (Maybe, Just, None)\n\
               use Std.Collections.Map (size)\n\
               data Box = Just Int | Empty\n\
               fun size (b : Box) = match b with | Just n -> n | Empty -> 0\n\
               def main = size (Just 5)\n";
    assert_eq!(eval_main_std(src), "5");
    assert_eq!(errors(src), "");
}

#[test]
fn a_value_clashing_across_modules_is_chosen_by_type() {
    let shapes = "data Shape = Circle Int | Square Int\n\
                  fun size s = match s with | Circle r -> 3 * r * r | Square w -> w * w\n";
    let main = "use Shapes (Shape, size)\n\
                data Box = Box Int\n\
                fun size b = match b with | Box n -> n\n\
                def main = (size (Circle 2), size (Box 5))\n";
    assert_eq!(eval_unit(&[("Shapes", shapes), ("", main)]), "(12, 5)");
}

#[test]
fn a_local_shadows_rather_than_overloads() {
    let src = format!("{BOX}def main = let unbox = \\x -> x + 1 in unbox 1\n");
    assert_eq!(eval_main_std(&src), "2");
}

#[test]
fn the_prelude_is_shadowed_not_overloaded() {
    // `map` is the prelude's too; this one is simply the one meant.
    let src = "fun map f x = f x\ndef main = map (\\n -> n + 1) 41\n";
    assert_eq!(eval_main_std(src), "42");
}

#[test]
fn a_generalized_definition_stays_polymorphic() {
    let src = format!("{BOX}fun wrap x : Maybe a = Just x\ndef main = (wrap 1, wrap \"s\")\n");
    assert_eq!(eval_main_std(&src), "(Just(1), Just(\"s\"))");
}

#[test]
fn a_local_binding_waits_for_the_body_around_it() {
    let src = format!("{BOX}def main = let b = Just 7 in unbox b\n");
    assert_eq!(eval_main_std(&src), "7");
}

// --- when the types do not settle it -----------------------------------------

#[test]
fn nothing_settling_it_is_ambiguous() {
    let src = format!("{BOX}def main = let f = \\x -> match x with | Just n -> 1 | _ -> 0 in 2\n");
    assert_eq!(
        errors(&src),
        "ambiguous `Just`: 2 of the candidates in scope fit `a -> b`\n\
         candidates in scope:\n  \
         Box.Just : Int -> Box  (fits)\n  \
         Maybe.Just : a -> Maybe a  (fits)"
    );
}

#[test]
fn a_top_level_definition_settles_its_own_names() {
    // A later use does not reach back: `v`'s type is its own business.
    let src = format!("{BOX}def v = Just 7\ndef main = unbox v\n");
    assert!(
        errors(&src).starts_with("ambiguous `Just`: 2 of the candidates in scope fit `Int -> a`"),
        "{}",
        errors(&src)
    );
    let annotated = format!("{BOX}def (v : Box) = Just 7\ndef main = unbox v\n");
    assert_eq!(errors(&annotated), "");
}

#[test]
fn no_candidate_fitting_names_the_type_wanted() {
    let src = format!("{BOX}fun f (b : Bool) = b\ndef main = f (Just 1)\n");
    assert_eq!(
        errors(&src),
        "no `Just` in scope has the type needed here, `Int -> Bool`\n\
         candidates in scope:\n  \
         Box.Just : Int -> Box\n  \
         Maybe.Just : a -> Maybe a"
    );
}

#[test]
fn one_name_settling_first_leaves_the_other_to_say_what_it_needed() {
    // `Just "no"` can only be `Maybe.Just`, so it is; then no `size` takes a
    // `Maybe String`.
    let src = "use Std.Maybe (Maybe, Just, None)\n\
               use Std.Collections.Map (size)\n\
               data Box = Just Int | Empty\n\
               fun size (b : Box) = match b with | Just n -> n | Empty -> 0\n\
               def main = size (Just \"no\")\n";
    let e = errors(src);
    assert!(
        e.starts_with("no `size` in scope has the type needed here, `Maybe String -> a`"),
        "{e}"
    );
}

#[test]
fn no_combination_fitting_is_one_error() {
    // Each name fits alone -- either `size` takes *something*, either `Just`
    // makes *something* -- but no `size` takes what any `Just` makes.
    let src = "use Std.Maybe (Maybe, Just, None)\n\
               use Std.Collections.Map (size)\n\
               data Box = Just Int | Empty\n\
               data Shape = Circle Int\n\
               fun size (s : Shape) = match s with | Circle r -> r\n\
               def main = size (Just 5)\n";
    let e = errors(src);
    assert!(e.starts_with("no reading of `size` and `Just` fits here"), "{e}");
    assert_eq!(e.lines().filter(|l| !l.starts_with(' ')).count(), 2, "one error:\n{e}");
}

#[test]
fn candidates_from_other_modules_say_where_they_are_from() {
    let shapes = "data Shape = Circle Int\nfun size s = match s with | Circle r -> r\n";
    let main = "use Shapes (Shape, size)\n\
                data Box = Box Int\n\
                fun size b = match b with | Box n -> n\n\
                def main = \\x -> size x\n";
    let e = unit_errors(&[("Shapes", shapes), ("", main)]);
    assert!(e.contains("size : Shape -> Int, from `Shapes`"), "{e}");
    assert!(e.contains("size : Box -> Int, from this module"), "{e}");
}

// --- duplicates ----------------------------------------------------------------

#[test]
fn defining_a_name_twice_in_one_module_is_an_error() {
    let e = unit_errors(&[("", "fun a x = 1\nfun a x = 2\ndef main = a 0\n")]);
    assert_eq!(e, "`a` is already defined in this module");
}

#[test]
fn a_def_and_a_fun_of_one_name_collide_too() {
    let e = unit_errors(&[("", "def a = 1\nfun a x = 2\ndef main = 0\n")]);
    assert_eq!(e, "`a` is already defined in this module");
}

#[test]
fn a_function_named_like_an_effect_operation_collides() {
    let e = unit_errors(&[("", "effect Ask { ask : () -> Int }\nfun ask x = x\ndef main = 0\n")]);
    assert_eq!(e, "`ask` is already defined in this module");
}

#[test]
fn the_same_name_in_two_modules_is_not_a_duplicate() {
    let a = "fun twice x = x + x\n";
    let main = "use A (twice)\nfun twice (s : String) = s\ndef main = twice 2\n";
    assert_eq!(unit_errors(&[("A", a), ("", main)]), "");
    assert_eq!(eval_unit(&[("A", a), ("", main)]), "4");
}
