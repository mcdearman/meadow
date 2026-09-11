//! Dependency-ordered inference — the `meadow-scc` pass and the binding groups
//! it hands to `meadow-infer`.
//!
//! Two things used to go wrong without it. A mention of a binding that had not
//! been inferred yet resolved to an unconstrained fresh variable, which silently
//! *discarded* the constraints on it — so a program could typecheck and then fail
//! at run time. And top-level definitions are evaluated in the order they are
//! lowered, so a `def` that used a function declared further down the file blew
//! up before it started.

mod common;
use common::{errors, eval_main, eval_main_std, eval_unit, schemes, schemes_std, unit_errors};

// --- the soundness hole ------------------------------------------------------

#[test]
fn a_forward_reference_is_checked_not_guessed() {
    // `addOne` is declared above `helper`, so `helper` has to be inferred first
    // for the call in `main` to be rejected.
    let src = "\
fun addOne n = helper n
fun helper n = n + 1
def main = addOne \"not a number\"
";
    assert!(
        errors(src).contains("type mismatch"),
        "forward reference went unchecked: {}",
        errors(src)
    );
}

#[test]
fn a_forward_reference_gets_the_type_it_should() {
    assert_eq!(
        schemes("fun addOne n = helper n\nfun helper n = n + 1\n"),
        "addOne : Int -> Int\nhelper : Int -> Int\n"
    );
}

// --- ordering ----------------------------------------------------------------

#[test]
fn a_def_may_use_a_function_declared_below_it() {
    // Evaluation is eager and follows lowering order, so this only works because
    // the pass moves `double` ahead of `main`.
    assert_eq!(eval_main("def main = double 21\nfun double n = n * 2\n"), "42");
}

#[test]
fn independent_bindings_keep_their_source_order() {
    assert_eq!(
        schemes("fun a x = x\nfun b x = x\nfun c x = x\n"),
        "a : forall a. a -> a\nb : forall a. a -> a\nc : forall a. a -> a\n"
    );
}

#[test]
fn a_dependency_is_generalized_before_its_user() {
    // `pair` is only usable at two different types if it generalizes before
    // `main` is inferred — which is exactly what ordering the groups buys.
    let src = "\
def main = both
fun both = (wrap 1, wrap \"s\")
fun wrap x = (x, x)
";
    assert_eq!(eval_main(src), "((1, 1), (\"s\", \"s\"))");
    assert_eq!(
        schemes("fun wrap x = (x, x)\nfun both = (wrap 1, wrap \"s\")\n"),
        "wrap : forall a. a -> (a, a)\nboth : ((Int, Int), (String, String))\n"
    );
}

// --- recursion ---------------------------------------------------------------

#[test]
fn self_recursion_still_works() {
    assert_eq!(
        schemes("fun len xs = match xs with\n  | #[] -> 0\n  | _ -> 1\n"),
        "len : forall a. #[a] -> Int\n"
    );
    assert_eq!(
        eval_main("fun fact n = if n <= 1 then 1 else n * fact (n - 1)\ndef main = fact 5\n"),
        "120"
    );
}

#[test]
fn mutual_recursion_is_inferred_as_one_group() {
    // `True` / `False` are `Std.Bool` constructors, so this one needs the std lib.
    let src = "\
fun isEven n = if n == 0 then True else isOdd (n - 1)
fun isOdd n = if n == 0 then False else isEven (n - 1)
";
    assert_eq!(schemes_std(src), "isEven : Int -> Bool\nisOdd : Int -> Bool\n");
    assert_eq!(eval_main_std(&format!("{src}def main = isEven 10\n")), "true");
}

#[test]
fn mutual_recursion_is_checked_across_the_group() {
    // `ping` says its argument is an `Int`; `pong` passes it a `String`. Both are
    // monomorphic while the group is solved, so the two meet.
    let src = "\
fun ping n = if n == 0 then 0 else pong \"x\"
fun pong s = ping s
";
    assert!(
        errors(src).contains("type mismatch"),
        "mutual group went unchecked: {}",
        errors(src)
    );
}

#[test]
fn a_mutual_group_generalizes_together() {
    // Neither member constrains the element type, so both come out polymorphic —
    // but only once the whole group is solved.
    let src = "\
fun evens xs = match xs with
  | #[] -> #[]
  | _ -> odds xs
fun odds xs = match xs with
  | #[] -> #[]
  | _ -> evens xs
";
    assert_eq!(
        schemes(src),
        "evens : forall a b. #[a] -> #[b]\nodds : forall a b. #[a] -> #[b]\n"
    );
}

#[test]
fn a_group_may_use_an_earlier_group_polymorphically() {
    let src = "\
fun id2 x = x
fun f n = if n == 0 then id2 0 else g (n - 1)
fun g n = f (id2 n)
def main = (f 3, id2 \"s\")
";
    assert_eq!(eval_main(src), "(0, \"s\")");
}

// --- across modules ----------------------------------------------------------

#[test]
fn a_module_may_use_a_module_handed_over_after_it() {
    // Each module is its own namespace, so `Alpha` reaches `Zeta`'s names by
    // `use` — and that works only if `Zeta` is inferred and lowered first.
    assert_eq!(
        eval_unit(&[
            (
                "Alpha",
                "use Zeta (double)\ndef twenty = double 10\nfun describe n = double n\n"
            ),
            ("Zeta", "fun double n = n * 2\n"),
            ("", "use Alpha (twenty, describe)\ndef main = twenty + describe 6\n"),
        ]),
        "32"
    );
}

#[test]
fn a_cross_module_reference_is_checked() {
    let out = unit_errors(&[
        ("Alpha", "use Zeta (double)\ndef bad = double \"not a number\"\n"),
        ("Zeta", "fun double n = n * 2\n"),
    ]);
    assert!(out.contains("type mismatch"), "cross-module call went unchecked: {out}");
}

#[test]
fn mutually_recursive_modules_still_compile() {
    // Nothing orders these two, so the pass keeps them in the order given. They
    // are functions, not values, so evaluation is fine either way.
    assert_eq!(
        eval_unit(&[
            (
                "Ping",
                "use Pong (pong)\nfun ping n = if n == 0 then 0 else pong (n - 1)\n"
            ),
            ("Pong", "use Ping (ping)\nfun pong n = ping n\n"),
            ("", "use Ping (ping)\ndef main = ping 4\n"),
        ]),
        "0"
    );
}

#[test]
fn a_cycle_between_modules_is_still_checked() {
    // Nothing can order these two, so the placeholder a forward reference leaves
    // behind is what carries the constraint: `ping` wants an `Int`, `pong` hands
    // it a `String`, and the two have to meet even though neither module can be
    // inferred first.
    let out = unit_errors(&[
        ("Ping", "use Pong (pong)\nfun ping n = if n == 0 then 0 else pong \"x\"\n"),
        ("Pong", "use Ping (ping)\nfun pong s = ping s\n"),
    ]);
    assert!(out.contains("type mismatch"), "cyclic modules went unchecked: {out}");
}

#[test]
fn two_modules_may_define_the_same_name() {
    // Separate namespaces, so `map` in one is not `map` in the other — and a
    // module that wants both takes one of them under an alias.
    assert_eq!(
        eval_unit(&[
            ("Vec", "fun map f = f 1\n"),
            ("Lst", "fun map f = f 2\n"),
            (
                "",
                "use Vec (map)\nuse Lst as L\nfun ident x = x\ndef main = map ident + L.map ident\n"
            ),
        ]),
        "3"
    );
}

#[test]
fn a_sibling_can_be_taken_under_an_alias() {
    assert_eq!(
        eval_unit(&[
            ("Math", "fun double n = n * 2\n"),
            ("", "use Math as M\ndef main = M.double 21\n"),
        ]),
        "42"
    );
}
