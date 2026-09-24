//! Core stays well-typed through every pass, for the programs that stress
//! generalization.
//!
//! `core::lint` recomputes every type in a core program and checks it against
//! what the program claims -- and that every type variable a type mentions is
//! bound where it is mentioned. The compiler runs it after lowering, in debug
//! builds only, which is how an unsolved variable in a `let`-bound call's
//! instantiation went unseen: the tests were run in release. This runs it on
//! purpose, whatever the build, and not only after lowering but after each of
//! the passes `meadow_seq::lower_program` puts core through, in its order, so
//! a pass that breaks typing is named by the test that catches it.
//!
//! The programs are the shapes generalization gets wrong: a call bound by a
//! `let` inside a generic function, a local binding used at two types, local
//! functions lifted out of generic and constrained definitions, a parameter
//! nothing uses, effect-polymorphic higher-order code. Each is also run, on the
//! reference interpreter and on the VM, which have to agree.

use meadow::{Engine, OptLevel, Options, pipeline, runtime};
use meadow_compiler::core::{self, Program};

/// Lint `p`, failing with `stage` named if anything is wrong.
fn lint(p: &Program, stage: &str, name: &str) {
    let problems = core::lint::check(p, &p.variants, &Default::default());
    assert!(
        problems.is_empty(),
        "`{name}`: core lint failed after {stage}:\n{}",
        problems
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// `src`, compiled with `Std`, linted after lowering and after every pass of
/// `lower_program` at `opt`, then run on the CEK machine and the VM.
fn checked(name: &str, src: &str, opt: OptLevel) -> String {
    let (program, diags) = pipeline::compile_str_with_std(name, src, Options::debug());
    assert!(
        diags.is_empty(),
        "`{name}` does not compile: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let program = core::prune::prune(&program);
    lint(&program, "lowering", name);

    // `meadow_seq::lower_program`'s passes, in its order.
    let p = core::dictionaries::program(&program, opt);
    lint(&p, "dictionaries", name);
    let p = if opt.specializes() {
        core::specialize::release(&p)
    } else {
        core::specialize::program(&p)
    };
    lint(&p, "specialize", name);
    let p = core::globals::inline_literals(&core::bools::program(&p));
    lint(&p, "bools and literal globals", name);
    let p = if opt.inlines() {
        core::inline::program(&p)
    } else {
        p
    };
    lint(&p, "inline", name);
    let p = core::joins::program(&p);
    lint(&p, "joins", name);
    let p = core::simplify::program(&p);
    lint(&p, "simplify", name);
    let p = core::lift::program(&p, opt);
    lint(&p, "lift", name);
    let p = core::trmc::program(&p, opt);
    lint(&p, "trmc", name);

    let cek = runtime::run(&program, Engine::Cek, opt)
        .unwrap_or_else(|e| panic!("`{name}` fails on the CEK machine: {e}"));
    let vm = runtime::run(&program, Engine::Vm, opt)
        .unwrap_or_else(|e| panic!("`{name}` fails on the VM: {e}"));
    assert_eq!(vm, cek, "`{name}`: the VM and the CEK machine disagree");
    cek
}

/// At every optimization level, with the same answer.
fn is(name: &str, src: &str, want: &str) {
    for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
        assert_eq!(checked(name, src, opt), want, "`{name}` at {}", opt.name());
    }
}

#[test]
fn the_standard_library_stays_well_typed_through_every_pass() {
    // Everything `Std` defines, reached from a `main` that uses nothing: with
    // `prune` off in spirit -- `compile_str_with_std` links it whole, and the
    // lint runs before `prune` takes anything away.
    let (program, diags) =
        pipeline::compile_str_with_std("std", "def main = 0\n", Options::debug());
    assert!(diags.is_empty());
    lint(&program, "lowering", "Std");
    for opt in [OptLevel::O1, OptLevel::O2] {
        let p = core::dictionaries::program(&program, opt);
        lint(&p, "dictionaries", "Std");
        let p = if opt.specializes() {
            core::specialize::release(&p)
        } else {
            core::specialize::program(&p)
        };
        lint(&p, "specialize", "Std");
        let p = core::inline::program(&core::globals::inline_literals(&core::bools::program(&p)));
        lint(&p, "inline", "Std");
        let p = core::simplify::program(&core::joins::program(&p));
        lint(&p, "joins and simplify", "Std");
        let p = core::lift::program(&p, opt);
        lint(&p, "lift", "Std");
        let p = core::trmc::program(&p, opt);
        lint(&p, "trmc", "Std");
    }
}

#[test]
fn a_let_bound_call_in_a_generic_function() {
    // What was left unsolved: the effect of `step c i`, a pure call whose
    // row nothing outside the `let` constrains.
    is(
        "letcall",
        "fun step (c : String) (i : Int) : Int = if i >= 10 then i else step c (i + 1)\n\
         fun fold f (c : String) (i : Int) acc =\n\
         \x20 if i >= 3 then acc else let r = step c i in fold f c (i + 1) (f acc r)\n\
         def main = fold (\\a x -> a + x) \"x\" 0 0\n",
        "30",
    );
}

#[test]
fn a_local_binding_used_at_two_types() {
    is(
        "twotypes",
        "fun both u = let pair = \\a b -> (a, b) in (pair 1 \"one\", pair True 2.5)\n\
         def main = both ()\n",
        "((1, \"one\"), (True, 2.5))",
    );
}

#[test]
fn a_local_loop_in_a_generic_function_is_lifted_at_its_types() {
    // `go` captures `f`, whose type mentions the enclosing binders: the lifted
    // definition has to abstract over them, and every call instantiate them.
    is(
        "liftgeneric",
        "fun mapAll f xs =\n\
         \x20 let rec go ys acc = match ys with | Nil -> acc | Cons y rest -> go rest (Cons (f y) acc) in\n\
         \x20 go xs Nil\n\
         def main = (mapAll (\\x -> x + 1) (Cons 1 (Cons 2 Nil)), mapAll (\\s -> s ++ \"!\") (Cons \"a\" Nil))\n",
        "([3; 2], [\"a!\"])",
    );
}

#[test]
fn a_local_function_capturing_a_dictionary() {
    // `show` inside the local function needs `Display a`, which it captures
    // from the enclosing definition's dictionary.
    is(
        "liftdict",
        "fun describeAll xs =\n\
         \x20 let rec go ys = match ys with | Nil -> \"\" | Cons y rest -> show y ++ \";\" ++ go rest in\n\
         \x20 go xs\n\
         def main = (describeAll (Cons 1 (Cons 2 Nil)), describeAll (Cons True Nil))\n",
        "(\"1;2;\", \"True;\")",
    );
}

#[test]
fn nested_local_functions_capturing_one_another() {
    is(
        "nested",
        "fun outer (n : Int) =\n\
         \x20 let rec count i = if i >= n then 0 else 1 + count (i + 1) in\n\
         \x20 let rec twice j = if j >= 2 then 0 else count 0 + twice (j + 1) in\n\
         \x20 twice 0\n\
         def main = outer 4\n",
        "8",
    );
}

#[test]
fn a_local_function_used_as_a_value_and_called() {
    is(
        "asvalue",
        "use Std.Collections.Vector as V\n\
         fun scaled (k : Int) xs = let times x = x * k in (V.map times xs, times 10)\n\
         def main = scaled 3 [1, 2]\n",
        "([3, 6], 30)",
    );
}

#[test]
fn mutual_recursion_with_a_parameter_nothing_uses() {
    // `c` is generic in `s1`, which never reads it: the shape Scythe's state
    // functions had, and where a call's type argument went unbound.
    is(
        "unused",
        "fun s0 c (i : Int) : Int = if i >= 10 then i else s1 c (i + 1)\n\
         fun s1 c (i : Int) : Int = if i >= 10 then i else s0 c (i + 2)\n\
         fun run f c (i : Int) acc = if i >= 3 then acc else let r = s0 c i in run f c (i + 1) (f acc r)\n\
         def main = run (\\a x -> a + x) \"x\" 0 0\n",
        "31",
    );
}

#[test]
fn effect_polymorphic_higher_order_code() {
    is(
        "effects",
        "use Std.Console (withOutput)\n\
         fun twice f x = f (f x)\n\
         fun loud (s : String) = let _ = println s in s ++ \"!\"\n\
         def main = (twice (\\x -> x + 1) 1, withOutput (\\() -> twice loud \"a\"))\n",
        "(3, (\"a!!\", \"a\\na!\\n\"))",
    );
}

#[test]
fn a_tuple_returned_and_taken_apart() {
    is(
        "tuple",
        "fun split (i : Int) = (i / 2, i % 2)\n\
         fun sum (n : Int) (i : Int) (acc : Int) : Int =\n\
         \x20 if i >= n then acc else match split i with | (q, r) -> sum n (i + 1) (acc + q + r)\n\
         def main = sum 10 0 0\n",
        "25",
    );
}

#[test]
fn a_branch_inside_an_argument_and_a_let() {
    // The simplifier's joins for a branch in a primitive's argument and in a
    // `let`'s right-hand side, chained -- where a jump once got wrapped twice.
    is(
        "joins",
        "fun cls (b : Int) : Int = if b == 32 then 0 else if b == 10 then 1 else 2\n\
         fun go (i : Int) (acc : Int) : Int =\n\
         \x20 if i >= 40 then acc\n\
         \x20 else let k = cls i in go (i + 1) (acc + k * 3 + (if i > 20 then 1 else 0) + cls (i + 1))\n\
         def main = go 0 0\n",
        "327",
    );
}

#[test]
fn a_list_built_by_a_lifted_local_loop() {
    // Lifted, then a candidate for tail recursion modulo cons.
    is(
        "trmc",
        "fun upto (n : Int) = let rec go i = if i >= n then Nil else Cons i (go (i + 1)) in go 0\n\
         def main = upto 4\n",
        "[0; 1; 2; 3]",
    );
}
