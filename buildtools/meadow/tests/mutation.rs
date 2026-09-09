//! `Ref`, and the effect-based value restriction that keeps it sound.
//!
//! Meadow generalizes a binding only when its right-hand side is *pure*, which is
//! a stronger and simpler rule than ML's syntactic value restriction: it falls
//! out of the effect row rather than being a special case about syntax. `Ref` is
//! the first feature that depends on it.

mod common;
use common::{errors_std_with, eval_main_std, schemes_std};
use meadow::Options;

// --- the effect shows up in the type -----------------------------------------

#[test]
fn mutating_is_visible_in_the_type_and_purity_still_means_something() {
    assert_eq!(
        schemes_std(
            "fun bump r = setRef r (getRef r + 1)\n\
             fun untouched x = x + 1\n"
        ),
        "bump : forall e. Ref Int -> Unit ! { Mut | e }\nuntouched : Int -> Int\n"
    );
}

#[test]
fn the_effect_propagates_to_callers() {
    // Nothing in `outer` mentions mutation, but it calls something that mutates,
    // so its type says so.
    assert_eq!(
        schemes_std("fun inner r = getRef r\nfun outer r = inner r + 1\n"),
        "inner : forall a e. Ref a -> a ! { Mut | e }\nouter : forall e. Ref Int -> Int ! { Mut | e }\n"
    );
}

// --- soundness ---------------------------------------------------------------

#[test]
fn a_polymorphic_reference_is_rejected() {
    // The classic hole. If `r` were generalized to `forall a. Ref [a]`, each use
    // would instantiate separately: store `Int`s here, read `String`s there, and
    // crash at run time with no type error anywhere.
    let out = errors_std_with(
        "use Std.String as S\n\
         def r = newRef []\n\
         fun poison u = setRef r [1]\n\
         fun readAsStrings u = S.concatAll (getRef r)\n",
        Options::debug(),
    );
    assert!(
        out.contains("type mismatch"),
        "storing Ints and reading Strings should not typecheck: {out}"
    );
}

#[test]
fn a_cell_binding_is_monomorphic() {
    // Not an error — just not generalized. The `Ref` is usable, at one type.
    assert_eq!(
        schemes_std("def r = newRef []\nfun use1 u = setRef r [1]\n"),
        "r : Ref [Int]\nuse1 : forall a e. a -> Unit ! { Mut | e }\n"
    );
}

#[test]
fn a_function_returning_a_cell_still_generalizes() {
    // The restriction is about *allocating* during the binding, not about `Ref`
    // being involved: each call of `fresh` makes its own cell, so it is safe to
    // be polymorphic and would be needlessly crippled otherwise.
    assert_eq!(
        schemes_std("fun fresh x = newRef x\n"),
        "fresh : forall a e. a -> Ref a ! { Mut | e }\n"
    );
}

#[test]
fn purity_is_not_lost_by_merely_mentioning_a_cell() {
    // Passing a `Ref` around without reading or writing it performs nothing.
    assert_eq!(
        schemes_std("fun hold r = (r, r)\n"),
        "hold : forall a. a -> (a, a)\n"
    );
}

// --- behaviour ---------------------------------------------------------------

#[test]
fn a_cell_is_shared_not_copied() {
    let src = "\
def main =
  let r = newRef 0 in
  let alias = r in
  let a = setRef alias 42 in
  getRef r
";
    assert_eq!(eval_main_std(src), "42");
}

#[test]
fn mutation_survives_across_calls() {
    let src = "\
fun tick r = setRef r (getRef r + 1)

def main =
  let r = newRef 0 in
  let a = tick r in
  let b = tick r in
  let c = tick r in
  getRef r
";
    assert_eq!(eval_main_std(src), "3");
}

#[test]
fn cells_are_equal_by_identity_not_contents() {
    // Every other value in the language is equal when it looks alike. A `Ref` is
    // a place, so two cells holding the same thing are still two cells.
    assert_eq!(
        eval_main_std("def main = let r = newRef 1 in (r == r, newRef 1 == newRef 1)\n"),
        "(true, false)"
    );
}

#[test]
fn a_cell_prints_as_its_contents() {
    assert_eq!(eval_main_std("def main = show (newRef 7)\n"), "\"ref 7\"");
}
