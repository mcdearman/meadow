//! The CEK machine is the specification; this checks the whole back end against
//! it.
//!
//! Each case is Meadow source, compiled once to `core` and then run **three
//! ways**:
//!
//! | | what it tests |
//! |---|---|
//! | `meadow_eval` | nothing — it *is* the specification |
//! | `meadow_seq::machine` | the lowering to AxCut, with no registers involved |
//! | `meadow_rts` | that, plus code generation, register allocation and the GC |
//!
//! Running the middle one matters even though the VM is what ships. When a case
//! fails, which of the two disagrees says immediately whether the bug is in the
//! translation or in the back end — and the AxCut machine additionally refuses
//! to enter a block whose parameters do not cover the whole environment, so a
//! lowering that miscounts what is live fails there with a shape error rather
//! than a wrong number.
//!
//! Agreement is on the printed result, because all three print the same way on
//! purpose. Anything the lowering cannot translate is asserted *loudly*: a case
//! that silently reported `Unsupported` and passed would be worse than no case.

use meadow_compiler::{compile_str, core};
use meadow_seq::machine;

/// How many steps a case may take. Generous — a bug that loops should fail a
/// test, not hang one.
const FUEL: u64 = 20_000_000;

/// Compile a source string to a `core::Program` whose entry point is `main`.
fn program(src: &str) -> core::Program {
    let (pkg, diags) = compile_str("diff", src);
    let hard: Vec<_> = diags.iter().map(|d| d.msg.clone()).collect();
    assert!(hard.is_empty(), "compile errors:\n{}", hard.join("\n"));
    let entry = pkg
        .exports
        .iter()
        .find(|e| &*e.name == "main")
        .map(|e| e.var);
    assert!(entry.is_some(), "the case has no `main`");
    core::Program {
        defs: pkg.defs.clone(),
        entry,
        ctor_fields: pkg.ctor_fields.clone(),
    }
}

/// Every optimization level, since the lowering differs between them.
const LEVELS: [meadow_core::OptLevel; 3] = [
    meadow_core::OptLevel::O0,
    meadow_core::OptLevel::O1,
    meadow_core::OptLevel::O2,
];

/// Run `src` all three ways, at every optimization level, and require the same
/// answer from all of them.
///
/// Returns the shared result, so a case can also assert what it should be —
/// otherwise machines that are wrong in the same way would agree and pass.
///
/// Every level rather than the default, because the levels are a *lowering*
/// decision: `-O2` compiles a `match` to a decision tree, so the cases below
/// are the only place each construct is put through both shapes one at a time.
/// On the whole standard library a disagreement says "something disagrees"; here
/// it says which construct.
#[track_caller]
fn agree(src: &str) -> String {
    let prog = program(src);

    let cek = meadow_eval::run(&prog).unwrap_or_else(|e| panic!("CEK failed: {}\n{src}", e.msg));
    let want = cek.to_string();

    for opt in LEVELS {
        let at = opt.name();
        let lowered = meadow_seq::lower_program(&prog, opt);
        assert!(
            lowered.unsupported.is_empty(),
            "lowering gave up at {at} on {:?}\n{src}",
            lowered.unsupported
        );

        let axcut = machine::Machine::run(&lowered.program, FUEL)
            .unwrap_or_else(|e| panic!("AxCut at {at} failed: {}\n{src}", e.msg));

        let image = meadow_codegen::compile(&lowered.program)
            .unwrap_or_else(|e| panic!("codegen at {at} failed: {}\n{src}", e.msg));
        let vm = meadow_rts::run(&image, FUEL).unwrap_or_else(|e| {
            panic!("VM at {at} failed: {}\n{src}\n{}", e.msg, image.disassemble())
        });

        assert_eq!(want, axcut.to_string(), "CEK vs AxCut at {at}\n{src}");
        assert_eq!(want, vm, "CEK vs VM at {at}\n{src}");
    }
    want
}

/// Compile all the way down, for the cases that check a *failure* — where the
/// harness above cannot help because there is no answer to compare.
fn image(prog: &core::Program) -> meadow_bytecode::Program {
    let lowered = meadow_seq::lower_program(prog, meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty(), "{:?}", lowered.unsupported);
    meadow_codegen::compile(&lowered.program).expect("codegen")
}

// --- the straight-line core ----------------------------------------------

// `compile_str` builds a package with no dependencies, so there is no `Std` and
// therefore no `true` / `false` — those are `Std.Bool` constructors, not
// literals. Booleans come from the comparison primitives instead, which is what
// `if` branches on anyway.

#[test]
fn literals() {
    assert_eq!(agree("def main = 42"), "42");
    assert_eq!(agree("def main = ()"), "()");
    assert_eq!(agree("def main = 'q'"), "'q'");
    assert_eq!(agree(r#"def main = "hi""#), "\"hi\"");
    assert_eq!(agree("def main = 1.5 +. 1.0"), "2.5");
}

#[test]
fn arithmetic_nests_and_keeps_its_order() {
    assert_eq!(agree("def main = 2 + 2"), "4");
    assert_eq!(agree("def main = 1 + 2 * 3 - 4"), "3");
    // Left to right, and each operand's continuation has to keep the operands
    // already computed alive — the case that catches a wrong `keep` set.
    assert_eq!(agree("def main = (1 + 2) * (3 + 4) - (5 + 6)"), "10");
}

#[test]
fn comparisons_and_conditionals() {
    assert_eq!(agree("def main = if 1 < 2 then 10 else 20"), "10");
    assert_eq!(agree("def main = if 1 > 2 then 10 else 20"), "20");
    // Both branches have to see the same environment, and it is not the one the
    // scrutinee was computed in.
    assert_eq!(
        agree("def main = let x = 5 in if x < 3 then x * 2 else x * 3"),
        "15"
    );
}

#[test]
fn let_bindings_nest_and_shadow() {
    assert_eq!(agree("def main = let x = 1 in x + 1"), "2");
    assert_eq!(agree("def main = let x = 1 in let y = 2 in x + y"), "3");
    // The inner `x` must not be captured as the outer one.
    assert_eq!(agree("def main = let x = 1 in let x = x + 10 in x"), "11");
}

// --- codata ---------------------------------------------------------------

#[test]
fn a_lambda_is_codata_with_one_method() {
    assert_eq!(agree("def main = (\\x -> x + 1) 41"), "42");
    assert_eq!(agree("def main = (\\x -> \\y -> x - y) 10 3"), "7");
}

#[test]
fn a_closure_captures_exactly_what_it_needs() {
    assert_eq!(
        agree("def main = let n = 10 in let f = \\x -> x + n in f 32"),
        "42"
    );
    // Two closures over different bindings, alive at the same time: the
    // capture lists have to be independent.
    assert_eq!(
        agree(
            "def main =
               let a = 1 in
               let f = \\x -> x + a in
               let b = 100 in
               let g = \\x -> x + b in
               f 0 + g 0"
        ),
        "101"
    );
}

#[test]
fn functions_are_values() {
    assert_eq!(
        agree(
            "fun twice f x = f (f x)
             def main = twice (\\n -> n * 3) 2"
        ),
        "18"
    );
    // A function returned from a function, applied later — the continuation of
    // the outer call has to survive the inner one.
    assert_eq!(
        agree(
            "fun adder n = \\m -> n + m
             def main = (adder 40) 2"
        ),
        "42"
    );
}

// --- globals and recursion ------------------------------------------------

#[test]
fn a_global_reference_is_a_jump() {
    assert_eq!(
        agree(
            "def x = 21
             def main = x + x"
        ),
        "42"
    );
    // Definition order in the source is not call order.
    assert_eq!(
        agree(
            "def main = a + b
             def a = 1
             def b = 2"
        ),
        "3"
    );
}

#[test]
fn recursion_needs_no_back_patching() {
    assert_eq!(
        agree(
            "fun fact n = if n <= 1 then 1 else n * fact (n - 1)
             def main = fact 10"
        ),
        "3628800"
    );
    assert_eq!(
        agree(
            "fun fib n = if n < 2 then n else fib (n - 1) + fib (n - 2)
             def main = fib 20"
        ),
        "6765"
    );
    // Mutual recursion, which is the case a back-patching scheme has to think
    // about and a jump table does not.
    assert_eq!(
        agree(
            "fun isEven n = if n == 0 then 0 == 0 else isOdd (n - 1)
             fun isOdd n = if n == 0 then 0 == 1 else isEven (n - 1)
             def main = isEven 100"
        ),
        "true"
    );
}

#[test]
fn a_deep_tail_call_does_not_grow_anything() {
    // 100_000 iterations. The AxCut machine has no stack at all — the whole
    // state is one environment — so this is a test that the translation of a
    // tail call really is a `substitute` and an `invoke`, with nothing left over.
    assert_eq!(
        agree(
            "fun count n acc = if n == 0 then acc else count (n - 1) (acc + n)
             def main = count 100000 0"
        ),
        "5000050000"
    );
}

// --- data -----------------------------------------------------------------

#[test]
fn constructors_build_data() {
    assert_eq!(
        agree(
            "data Shape = Circle Int | Rect Int Int
             def main = Rect 3 4"
        ),
        "Rect(3, 4)"
    );
    assert_eq!(
        agree(
            "data Shape = Circle Int | Rect Int Int
             def main = Circle (1 + 2)"
        ),
        "Circle(3)"
    );
}

#[test]
fn tuples_are_data_too() {
    assert_eq!(agree("def main = (1, 2)"), "(1, 2)");
    assert_eq!(agree("def main = (1 + 1, (2, 3))"), "(2, (2, 3))");
}

// --- the shape of the failure --------------------------------------------

// --- pattern matching -----------------------------------------------------

#[test]
fn matching_on_a_constructor() {
    assert_eq!(
        agree(
            "data Shape = Circle Int | Rect Int Int
             fun area s = match s with
               | Circle r -> 3 * r * r
               | Rect w h -> w * h
             def main = area (Rect 3 4) + area (Circle 2)"
        ),
        "24"
    );
}

#[test]
fn arms_are_tried_in_order_and_a_wildcard_catches() {
    assert_eq!(
        agree(
            "fun classify n = match n with
               | 0 -> \"zero\"
               | 1 -> \"one\"
               | _ -> \"many\"
             def main = (classify 0, classify 1, classify 7)"
        ),
        "(\"zero\", \"one\", \"many\")"
    );
}

#[test]
fn nested_patterns_backtrack_into_the_next_arm() {
    // The first arm matches `Just` and then fails on the inner literal. That
    // failure has to restore the whole environment — including the scrutinee,
    // which the `switch` for `Just` consumed — before the second arm can look
    // at it again. It is the case the failure-continuation chain exists for.
    assert_eq!(
        agree(
            "data Opt = None | Some Int
             fun f x = match x with
               | Some 0 -> \"zero\"
               | Some n -> \"some\"
               | None   -> \"none\"
             def main = (f (Some 0), f (Some 5), f None)"
        ),
        "(\"zero\", \"some\", \"none\")"
    );
}

#[test]
fn deeply_nested_constructor_patterns() {
    assert_eq!(
        agree(
            "data Tree = Leaf | Node Tree Int Tree
             fun sum t = match t with
               | Leaf -> 0
               | Node l v r -> sum l + v + sum r
             def main = sum (Node (Node Leaf 1 Leaf) 2 (Node Leaf 3 Leaf))"
        ),
        "6"
    );
}

#[test]
fn tuple_and_as_patterns() {
    assert_eq!(
        agree(
            "fun swap p = match p with
               | (a, b) -> (b, a)
             def main = swap (1, 2)"
        ),
        "(2, 1)"
    );
    // An `as` pattern binds the whole and then keeps matching the part.
    assert_eq!(
        agree(
            "data Opt = None | Some Int
             fun f x = match x with
               | Some n -> n
               | None -> 0
             def main = f (Some 7)"
        ),
        "7"
    );
}

#[test]
fn a_non_exhaustive_match_fails_the_same_way_everywhere() {
    // All three must *fail*, and `agree` only compares answers — so this one is
    // checked by hand.
    let prog = program(
        "data Opt = None | Some Int
         fun f x = match x with | Some n -> n
         def main = f None",
    );
    let cek = meadow_eval::run(&prog).expect_err("CEK should fail");
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    let axcut = machine::Machine::run(&lowered.program, FUEL).expect_err("AxCut should fail");
    let vm = meadow_rts::run(&image(&prog), FUEL).expect_err("the VM should fail");
    assert!(cek.msg.contains("non-exhaustive"), "{}", cek.msg);
    assert!(axcut.msg.contains("non-exhaustive"), "{}", axcut.msg);
    assert!(vm.msg.contains("non-exhaustive"), "{}", vm.msg);
}

// --- lists, and the recursion that builds them ---------------------------

#[test]
fn a_recursive_data_type_end_to_end() {
    assert_eq!(
        agree(
            "data Chain = Nil | Cons Int Chain
             fun range n = if n == 0 then Nil else Cons n (range (n - 1))
             fun total xs = match xs with
               | Nil -> 0
               | Cons x rest -> x + total rest
             def main = total (range 100)"
        ),
        "5050"
    );
}

/// A type of one's own named `Nil` / `Cons` is **not** the builtin list.
///
/// Both `Display`s special-case `List.Nil` / `List.Cons` to print `[1; 2; 3]`.
/// This used to fire for *any* type whose constructors were spelled that way,
/// because constructors shared one global namespace and there was nothing to
/// tell `Chain`'s `Cons` from the list's. Now a constructor's name is
/// `Type.Ctor`, so the special case applies to the list and to nothing else —
/// and a `Chain` prints as the `Chain` it is.
#[test]
fn a_lookalike_type_is_not_mistaken_for_the_builtin_list() {
    assert_eq!(
        agree(
            "data Chain = Nil | Cons Int Chain
             def main = Cons 1 (Cons 2 (Cons 3 Nil))"
        ),
        "Cons(1, Cons(2, Cons(3, Nil)))"
    );
}

// --- records, arrays, projection -----------------------------------------

#[test]
fn records_build_and_select() {
    assert_eq!(agree("def main = { x = 1, y = 2 }"), "{ x = 1, y = 2 }");
    assert_eq!(agree("def main = { x = 1, y = 2 }.y"), "2");
    // Row polymorphism: one accessor, records of different shapes.
    assert_eq!(
        agree(
            "fun getX r = r.x
             def main = (getX { x = 1 }, getX { x = 5, other = 9 })"
        ),
        "(1, 5)"
    );
}

#[test]
fn record_extension_has_no_surface_syntax_but_lowers() {
    // `{ r | x = 9 }` does not parse — the language has no record update. The
    // `core` term exists all the same, so the only way to check it lowers is to
    // build one, which is worth doing: `Extend` is a construct the front end
    // could start producing at any time.
    use meadow_compiler::core::{Def, Lit, Term};
    use std::sync::Arc;

    let var = meadow_compiler::hir::VarId(1);
    let term = Term::Extend(
        Arc::new(Term::Record(vec![
            ("x".into(), Term::Lit(Lit::Int(1))),
            ("y".into(), Term::Lit(Lit::Int(2))),
        ])),
        "x".into(),
        Arc::new(Term::Lit(Lit::Int(9))),
    );
    let prog = core::Program {
        defs: vec![Def::untyped(var, "main", term)],
        entry: Some(var),
        ctor_fields: Default::default(),
    };

    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());
    let cek = meadow_eval::run(&prog).expect("CEK");
    let axcut = machine::Machine::run(&lowered.program, FUEL).expect("AxCut");
    let vm = meadow_rts::run(&image(&prog), FUEL).expect("the VM");
    assert_eq!(cek.to_string(), "{ x = 9, y = 2 }");
    assert_eq!(cek.to_string(), axcut.to_string());
    assert_eq!(cek.to_string(), vm);
}

#[test]
fn a_record_pattern_matches_the_fields_it_names() {
    assert_eq!(
        agree(
            "fun getX r = match r with | { x = v } -> v
             def main = getX { x = 42 }"
        ),
        "42"
    );
}

#[test]
fn arrays_build_and_match_by_length() {
    assert_eq!(agree("def main = #[1, 2, 3]"), "#[1, 2, 3]");
    assert_eq!(agree("def main = arrayLen #[1, 2, 3]"), "3");
    assert_eq!(
        agree(
            "fun f a = match a with
               | #[x, y] -> x + y
               | _ -> 0
             def main = (f #[3, 4], f #[1, 2, 3])"
        ),
        "(7, 0)"
    );
}

#[test]
fn tuple_projection_does_not_need_the_arity() {
    // An irrefutable tuple parameter is where `core` produces `Proj`. It carries
    // only an index, so it cannot become a `switch` arm — every arm would have
    // to name every field, and the arity is not in the term. It becomes a field
    // extern instead.
    assert_eq!(agree("fun addPair (a, b) = a + b\ndef main = addPair (1, 2)"), "3");
    assert_eq!(
        agree("def main = let (a, b, c) = (10, 20, 30) in b"),
        "20"
    );
}

// --- effects --------------------------------------------------------------

#[test]
fn a_handler_that_never_resumes_aborts_the_body() {
    assert_eq!(
        agree(
            "effect Abort { bail : () -> Int }
             def main =
               handle 1 + bail () with {
                 bail u k -> 99
               }"
        ),
        "99"
    );
}

#[test]
fn a_handler_that_resumes_continues_the_body_in_place() {
    // The case that pins down what a resumption restores. The body computes
    // `5 + 1`, and that 6 comes back *inside* the clause, where 100 is added —
    // so the handler's continuation after resuming is the resume site, not
    // where the handler was installed.
    assert_eq!(
        agree(
            "effect Ask { ask : () -> Int }
             def main =
               handle ask () + 1 with {
                 ask u k -> k 5 + 100
               }"
        ),
        "106"
    );
}

#[test]
fn handlers_are_deep() {
    // `ask` is performed twice, and the second one must find the same handler —
    // which only happens if resuming puts the handler frame back.
    assert_eq!(
        agree(
            "effect Ask { ask : () -> Int }
             def main =
               handle ask () + ask () with {
                 ask u k -> k 5
               }"
        ),
        "10"
    );
    // And through a function call, which is what "deep" is usually about.
    assert_eq!(
        agree(
            "effect Ask { ask : () -> Int }
             fun twice u = ask () + ask ()
             def main = handle twice () with { ask u k -> k 3 }"
        ),
        "6"
    );
}

#[test]
fn a_return_clause_transforms_the_final_value() {
    assert_eq!(
        agree(
            "effect Ask { ask : () -> Int }
             def main =
               handle ask () with {
                 ask u k -> k 2,
                 return x -> x * 10
               }"
        ),
        "20"
    );
}

#[test]
fn state_by_hand_is_a_handler_returning_a_function() {
    // The standard encoding: each clause returns a function of the state, and
    // the `return` clause ignores it. It exercises a handler whose clauses build
    // closures over the resumption, which is where a wrong capture list shows.
    assert_eq!(
        agree(
            "effect St { get : () -> Int, put : Int -> () }
             def main =
               let f =
                 handle
                   let a = get () in
                   let u = put (a + 1) in
                   let b = get () in
                   a + b
                 with {
                   get u k -> \\s -> (k s) s,
                   put v k -> \\s -> (k ()) v,
                   return x -> \\s -> x
                 }
               in f 10"
        ),
        "21"
    );
}

#[test]
fn a_nested_handler_takes_precedence_over_an_outer_one() {
    assert_eq!(
        agree(
            "effect Ask { ask : () -> Int }
             def main =
               handle
                 (handle ask () with { ask u k -> k 1 }) + ask ()
               with {
                 ask u k -> k 100
               }"
        ),
        "101"
    );
}

#[test]
fn resuming_twice_is_refused_everywhere() {
    let prog = program(
        "effect Ask { ask : () -> Int }
         def main =
           handle ask () with {
             ask u k -> k 1 + k 2
           }",
    );
    let cek = meadow_eval::run(&prog).expect_err("CEK should refuse");
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    let axcut = machine::Machine::run(&lowered.program, FUEL).expect_err("AxCut should refuse");
    let vm = meadow_rts::run(&image(&prog), FUEL).expect_err("the VM should refuse");
    assert!(cek.msg.contains("more than once"), "{}", cek.msg);
    assert!(axcut.msg.contains("more than once"), "{}", axcut.msg);
    assert!(vm.msg.contains("more than once"), "{}", vm.msg);
}

// --- everything lowers ----------------------------------------------------

#[test]
fn nothing_in_core_is_left_untranslated() {
    // One program touching every `core` construct the front end can produce.
    // The assertion is on the *lowering*, not the answer: `Unsupported` has one
    // variant left and it means a bug upstream, so an empty set here is the
    // statement that this pass is complete.
    let prog = program(
        "data Opt = None | Some Int
         data Chain = Nil | Cons Int Chain
         effect Ask { ask : () -> Int }

         fun chain n = if n == 0 then Nil else Cons n (chain (n - 1))

         fun sum xs = match xs with
           | Nil -> { total = 0 }
           | Cons x rest -> { total = x + (sum rest).total }

         fun addPair (a, b) = a + b

         def main =
           let opt = Some 3 in
           let arr = #[1, 2, 3] in
           let r = sum (chain 4) in
           handle
             (match opt with | Some n -> n | None -> 0)
               + addPair (1, 2)
               + arrayLen arr
               + r.total
               + ask ()
           with {
             ask u k -> k 1,
             return x -> x
           }",
    );
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    assert!(
        lowered.unsupported.is_empty(),
        "still untranslated: {:?}",
        lowered.unsupported
    );

    let cek = meadow_eval::run(&prog).expect("CEK");
    let axcut = machine::Machine::run(&lowered.program, FUEL).expect("AxCut");
    let vm = meadow_rts::run(&image(&prog), FUEL).expect("the VM");
    assert_eq!(cek.to_string(), axcut.to_string());
    assert_eq!(cek.to_string(), vm);
}

// --- the heap -------------------------------------------------------------

/// Compile `src` and run it on the VM, answering both the result and the
/// machine, so a case can look at what collection did.
#[track_caller]
fn run_on_vm(src: &str) -> (String, u64, u64) {
    let prog = program(src);
    let img = image(&prog);
    let mut vm = meadow_rts::Vm::new(&img);
    let entry = img.entry.expect("an entry point");
    let v = vm
        .run(entry, FUEL)
        .unwrap_or_else(|e| panic!("VM failed: {}\n{src}", e.msg));
    let out = vm.show(v);
    let (collections, allocated) = vm.heap_stats();
    (out, collections, allocated)
}

#[test]
fn the_collector_runs_and_the_answer_survives_it() {
    // Builds a 200_000-element list and walks it. That is well past the initial
    // heap, so collection is not optional — and every one of them moves the list
    // the program is still holding.
    let src = "data Chain = Nil | Cons Int Chain
               fun build n = if n == 0 then Nil else Cons n (build (n - 1))
               fun total xs = match xs with
                 | Nil -> 0
                 | Cons x rest -> x + total rest
               def main = total (build 200000)";
    let (out, collections, allocated) = run_on_vm(src);
    assert_eq!(out, "20000100000");
    assert!(collections > 0, "the heap never filled — is the test too small?");
    assert!(allocated > 1_000_000, "only {allocated} slots allocated");

    // And the specification agrees, which is the point.
    assert_eq!(agree(src), "20000100000");
}

#[test]
fn garbage_is_actually_reclaimed() {
    // Allocates a fresh tuple every iteration and keeps none of them. If the
    // collector were retaining what it should not — a stale register, a root set
    // that is too wide — this would grow without bound and run out of memory
    // rather than finishing.
    let src = "fun loop n acc = if n == 0 then acc else loop (n - 1) (acc + 1)
               fun step n = let pair = (n, n) in loop 10 0
               fun outer n acc =
                 if n == 0 then acc else outer (n - 1) (acc + step n)
               def main = outer 20000 0";
    let (out, collections, _) = run_on_vm(src);
    assert_eq!(out, "200000");
    assert!(collections > 0);
}

#[test]
fn a_cycle_through_a_mutable_cell_does_not_leak_the_program() {
    // Reference counting cannot free a cell that points at something holding the
    // cell. A copying collector never has the question — this runs, where the
    // `Rc`-based machine would simply keep every one of them alive.
    let src = "fun spin n =
                 if n == 0 then 0
                 else
                   let cell = newRef 0 in
                   let ignored = setRef cell (getRef cell + n) in
                   getRef cell + spin (n - 1)
               def main = spin 20000";
    assert_eq!(agree(src), "200010000");
}

#[test]
fn a_deep_tail_loop_costs_the_vm_no_stack() {
    // The VM has no call stack at all, so this is not tail-call *optimisation* —
    // there is nothing to optimise away. It is also a GC test: the loop keeps
    // one accumulator alive and everything else has to go.
    let (out, _, _) = run_on_vm(
        "fun count n acc = if n == 0 then acc else count (n - 1) (acc + n)
         def main = count 100000 0",
    );
    assert_eq!(out, "5000050000");
}

/// A constructor prints the same on both machines, and prints *bare*.
///
/// A constructor's real name is `Type.Ctor` — two types may each own a `Leaf`,
/// and every pass after the resolver keys on the qualified form. Display is the
/// one place that has to undo it, in two separate implementations
/// (`meadow_eval`'s `Display` and `meadow_rts::show`), which is exactly the
/// shape of thing that drifts: the CEK machine kept printing `Maybe.Just(3)`
/// for a while after the VM had stopped.
#[test]
fn constructors_print_bare_and_agree() {
    // No `Std` here, so every constructor is one these cases declare.
    assert_eq!(
        agree("data Box = Wrap Int\ndef main = Wrap 3"),
        "Wrap(3)"
    );
    assert_eq!(
        agree("data Colour = Red | Green\ndef main = (Red, Green)"),
        "(Red, Green)"
    );
    // Two types owning one constructor name — the reason the qualified form
    // exists. Both print as themselves.
    assert_eq!(
        agree(
            "data Tree = Leaf | Node Tree Tree\n\
             data Rope = Leaf String | Node Rope Rope\n\
             def main = (Tree.Leaf, Rope.Leaf \"s\")"
        ),
        "(Leaf, Leaf(\"s\"))"
    );
}

/// `.field` on a `record` declaration's value, which is constructor data with
/// named fields rather than an anonymous record. The CEK machine looked the
/// name up in the constructor's field list; the VM and the sequent machine only
/// knew anonymous records, and failed with "selected `.x` from object".
#[test]
fn a_records_fields_select_by_name_everywhere() {
    assert_eq!(
        agree(
            "record Point = { x : Int, y : Int }\n\
             def origin = Point { x = 3, y = 4 }\n\
             def main = (origin.y, origin.x, (Point { x = 1, y = 2 }).y)"
        ),
        "(4, 3, 2)"
    );
    // Fields declared in a different order from the one they are written in.
    assert_eq!(
        agree(
            "record Pair = { second : Int, first : Int }\n\
             def main = let p = Pair { first = 1, second = 2 } in (p.first, p.second)"
        ),
        "(1, 2)"
    );
}

/// `hash` is one algorithm fed the same way by every engine, so all three give
/// the same number for the same value -- and a program that prints one, or
/// lays a table out by one, behaves the same wherever it runs.
#[test]
fn hashes_agree_everywhere() {
    agree(
        "data Shape = Circle Int | Rect Int Int\n\
         record Point = { x : Int, y : Int }\n\
         def main =\n\
         \x20 ( hash 0, hash (-7), hash 1.5, hash (-0.0), hash True, hash 'q'\n\
         \x20 , hash \"a string past eight bytes\", hash ()\n\
         \x20 , hash (1, \"two\", 3.0), hash [1; 2; 3], hash #[4, 5]\n\
         \x20 , hash (Rect 2 3), hash { a = 1, b = \"x\" }, hash (Point { x = 1, y = 2 })\n\
         \x20 , hash (toBigInt 12345678901234) )",
    );
}

/// Two values `==` calls equal hash alike, on every engine.
#[test]
fn equal_values_hash_alike() {
    assert_eq!(
        agree(
            "def main =\n\
             \x20 ( hash 0.0 == hash (-0.0)\n\
             \x20 , hash { a = 1, b = 2 } == hash { b = 2, a = 1 }\n\
             \x20 , hash (1 :: 2 :: [;]) == hash [1; 2]\n\
             \x20 , hash 1 == hash 2 )"
        ),
        "(true, true, true, false)"
    );
}

/// `runSt` and its mutable arrays behave alike everywhere: written in place,
/// copied by `stFreeze` / `stThaw`, and printed as what they hold.
#[test]
fn mutable_arrays_agree_everywhere() {
    assert_eq!(
        agree(
            "fun fill a i n = if i >= n then () else let _ = stSetArray a i (i * i) in fill a (i + 1) n\n\
             def main = runSt (\\() ->\n\
             \x20 let a = stNewArray 5 0 in\n\
             \x20 let _ = fill a 0 5 in\n\
             \x20 let frozen = stFreeze a in\n\
             \x20 let b = stThaw frozen in\n\
             \x20 let _ = stSetArray b 0 99 in\n\
             \x20 let r = stNewRef 1 in\n\
             \x20 let _ = stSetRef r (stGetRef r + stArrayLen b) in\n\
             \x20 (frozen, stFreeze b, stGetRef r))"
        ),
        "(#[0, 1, 4, 9, 16], #[99, 1, 4, 9, 16], 6)"
    );
}

// --- sized numbers -------------------------------------------------------

// A fixed-width integer is an unboxed word in every engine: in a register, in a
// constructor field, and in an array slot. A disagreement in wrapping, sign,
// display or equality between the three shows up here first.

#[test]
fn a_fixed_width_integer_wraps_at_its_width() {
    assert_eq!(agree("def main = toUInt8 250 + toUInt8 10"), "4");
    assert_eq!(agree("def main = toInt8 127 + toInt8 1"), "-128");
    assert_eq!(agree("def main = toUInt16 0 - toUInt16 1"), "65535");
    assert_eq!(agree("def main = toInt32 65536 * toInt32 65536"), "0");
    assert_eq!(agree("def main = toUInt64 (toInt (0 - 1))"), "18446744073709551615");
    // A conversion keeps the low bits, from a `BigInt` as from anything else.
    assert_eq!(agree("def main = toUInt8 (2 ^ 100 + 5)"), "5");
    assert_eq!(agree("def main = toInt16 40000"), "-25536");
}

#[test]
fn an_unconstrained_literal_is_a_bigint_and_does_not_overflow() {
    assert_eq!(agree("def main = 2 ^ 64"), "18446744073709551616");
    assert_eq!(agree("def main = toInt 2 ^ 64"), "0");
}

#[test]
fn shifts_and_bits_know_the_width_and_the_sign() {
    assert_eq!(agree("def main = toUInt32 (0 - 1) >> 28"), "15");
    assert_eq!(agree("def main = toInt32 (0 - 16) >> 2"), "-4");
    assert_eq!(agree("def main = bitNot (toUInt8 0)"), "255");
    assert_eq!(agree("def main = popCount (toInt16 (0 - 1))"), "16");
    assert_eq!(agree("def main = bitWidth (toUInt64 0)"), "64");
}

#[test]
fn sized_integers_compare_and_match_by_value() {
    assert_eq!(agree("def main = if toUInt8 200 > 100 then 1 else 0"), "1");
    assert_eq!(agree("def main = if toInt8 (0 - 1) < 0 then 1 else 0"), "1");
    assert_eq!(agree("def main = if toUInt16 5 == 5 then 1 else 0"), "1");
    assert_eq!(
        agree("def main = match toUInt8 3 with | 2 -> 20 | 3 -> 30 | _ -> 0"),
        "30"
    );
}

#[test]
fn sized_numbers_are_stored_in_fields_and_arrays() {
    assert_eq!(
        agree(
            "data P = P UInt8 Int16 Float32
             fun total p = match p with | P a b c -> (toInt a + toInt b, c)
             def main = total (P (toUInt8 255) (toInt16 (0 - 5)) (toFloat32 0.5))"
        ),
        "(250, 0.5)"
    );
    assert_eq!(
        agree("def main = let (b : #[UInt8]) = #[1, 255, 3] in (b, arrayGet b 1 + toUInt8 1)"),
        "(#[1, 255, 3], 0)"
    );
}

#[test]
fn float32_keeps_single_precision() {
    assert_eq!(agree("def main = toFloat32 1.5 +. toFloat32 0.25"), "1.75");
    // Printed as the shortest text that reads back as the same `Float32`.
    assert_eq!(agree("def main = toFloat32 0.1"), "0.1");
    assert_eq!(agree("def main = toFloat64 (toFloat32 0.1)"), "0.10000000149011612");
    assert_eq!(agree("def main = toFloat32 16777217.0"), "16777216.0");
}

#[test]
fn equal_integers_hash_alike_whatever_their_type() {
    assert_eq!(
        agree("def main = if hash (toUInt8 7) == hash (toInt 7) then 1 else 0"),
        "1"
    );
    assert_eq!(
        agree("def main = if hash (toInt32 7) == hash (toBigInt 7) then 1 else 0"),
        "1"
    );
}

// --- compact regions -----------------------------------------------------

// On the VM a compacted value lives outside the collected heap; on the other
// two nothing moves. The cases bind with `let` rather than `def`: the sequent
// machine evaluates a top-level `def` again at each use, so `def c = compact x`
// would be a new region every time `c` is mentioned, and the sizes would count
// that rather than what is being tested. Everything a program can see must still agree -- the
// value, `==`, `hash`, what is refused, and even `compactSize`, which the
// reference-counted engines estimate by counting what the VM would copy.

const CHAIN: &str = "data Chain = End | Link Int Chain
                     fun build n = if n == 0 then End else Link n (build (n - 1))
                     fun total xs = match xs with
                       | End -> 0
                       | Link x rest -> x + total rest\n";

#[test]
fn a_compacted_value_is_the_value() {
    let src = format!(
        "{CHAIN}def main =
           let xs = build 100 in
           let c = compact xs in
           (total (getCompact c), if getCompact c == xs then 1 else 0)"
    );
    assert_eq!(agree(&src), "(5050, 1)");
    assert_eq!(agree("def main = compact (1, \"two\", #[3.0])"), "compact (1, \"two\", #[3.0])");
}

#[test]
fn compacts_compare_and_hash_by_what_they_hold() {
    let src = format!(
        "{CHAIN}def a = compact (build 10)
         def b = compact (build 10)
         def main = (if a == b then 1 else 0, if hash a == hash b then 1 else 0,
                     if compact (build 3) == a then 1 else 0)"
    );
    assert_eq!(agree(&src), "(1, 1, 0)");
}

#[test]
fn adding_to_a_region_keeps_both_values_and_shares_the_old_one() {
    let src = format!(
        "{CHAIN}def main =
           let c = compact (build 100) in
           let before = compactSize c in
           let c2 = compactAdd c (Link 0 (getCompact c)) in
           (total (getCompact c), total (getCompact c2),
            compactSize c2 - before, compactSize c == compactSize c2)"
    );
    // One `Link` is three slots, and that is all the add copied.
    let bytes = 3 * meadow_core::compact::SLOT_BYTES;
    assert_eq!(agree(&src), format!("(5050, 5050, {bytes}, true)"));
}

#[test]
fn compact_size_is_the_same_on_every_engine() {
    let src = format!(
        "{CHAIN}def main =
           let xs = build 1000 in
           (compactSize (compact xs), compactSize (compact (xs, xs)),
            compactSize (compact (toInt 7)), compactSize (compact #[xs, xs, xs]))"
    );
    agree(&src);
}

#[test]
fn a_compact_can_hold_a_compact() {
    let src = format!(
        "{CHAIN}def main =
           let inner = compact (build 4) in
           let outer = compact (inner, inner) in
           match getCompact outer with
           | (a, b) -> (total (getCompact a), if a == b then 1 else 0)"
    );
    assert_eq!(agree(&src), "(10, 1)");
}

#[test]
fn what_cannot_be_compacted_fails_the_same_way_everywhere() {
    for (value, what) in [
        ("(1, newRef 2)", "a Ref"),
        ("Just (\\x -> x)", "a function"),
        ("#[stNewArray 2 0]", "a mutable array"),
    ] {
        let prog = program(&format!("data Opt a = None | Just a\ndef main = compact ({value})"));
        let cek = meadow_eval::run(&prog).expect_err("CEK should fail");
        let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
        let axcut = machine::Machine::run(&lowered.program, FUEL).expect_err("AxCut should fail");
        let vm = meadow_rts::run(&image(&prog), FUEL).expect_err("the VM should fail");
        let want = meadow_core::compact::uncompactable(what);
        assert_eq!(cek.msg, want);
        assert_eq!(axcut.msg, want);
        assert_eq!(vm.msg, want);
    }
}

#[test]
fn a_compacted_value_survives_collections_without_being_copied() {
    // A 50_000-link chain kept alive through a loop that allocates enough to
    // collect many times over. Compacted, the collections copy almost nothing;
    // left in the heap, every one of them copies the whole chain.
    let churn = "fun churn n acc = if n == 0 then acc else churn (n - 1) (acc + total (build 50))";
    let run = |keep: &str| {
        let src = format!(
            "{CHAIN}{churn}
             def main = let xs = {keep} in (churn 20000 0, total xs)"
        );
        let prog = program(&src);
        let img = image(&prog);
        let mut vm = meadow_rts::Vm::new(&img);
        let v = vm.run(img.entry.unwrap(), u64::MAX).unwrap_or_else(|e| panic!("{}", e.msg));
        (vm.show(v), vm.heap().collections, vm.heap().copied, vm.heap().region_slots())
    };
    let (plain, plain_gcs, plain_copied, _) = run("build 50000");
    let (compacted, gcs, copied, region) = run("getCompact (compact (build 50000))");
    assert_eq!(plain, "(25500000, 1250025000)");
    assert_eq!(compacted, plain);
    assert!(plain_gcs > 5 && gcs > 5, "both should collect: {plain_gcs}, {gcs}");
    assert_eq!(region, 1 + 50_000 * 3, "the chain is in a region, and nothing else is");
    assert!(
        copied * 4 < plain_copied,
        "compacting should spare most of the copying: {copied} slots against {plain_copied}"
    );
}

#[test]
fn regions_nothing_refers_to_are_freed() {
    // A thousand regions of a thousand links each, each dropped straight away.
    // Kept, they would be three million slots; collecting frees them.
    let src = format!(
        "{CHAIN}fun again n acc = if n == 0 then acc
                 else again (n - 1) (acc + total (getCompact (compact (build 1000))))
         def main = again 1000 0"
    );
    let prog = program(&src);
    let img = image(&prog);
    let mut vm = meadow_rts::Vm::new(&img);
    let v = vm.run(img.entry.unwrap(), u64::MAX).unwrap_or_else(|e| panic!("{}", e.msg));
    assert_eq!(vm.show(v), "500500000");
    assert!(vm.heap().collections > 0, "writing regions should have asked for a collection");
    assert!(
        vm.heap().region_slots() < 1_000_000,
        "dead regions were kept: {} slots",
        vm.heap().region_slots()
    );
}

// --- number-generic code at a number type -----------------------------------

// A function generic over its number type is copied for each type it is used
// at (`meadow_core::specialize`), so its literals are made as that type. Before
// that, a literal in generic code was an `Int` at run time whatever the call
// said, and `fib 100` -- typed `BigInt` -- wrapped at 64 bits.

const FIB: &str = "fun fib n =
                     let rec loop a b i =
                       if i == 0 then a
                       else loop b (a + b) (i - 1)
                     in loop 0 1 n\n";

#[test]
fn a_generic_function_computes_at_the_type_it_is_called_at() {
    // `fib 100` defaults to `BigInt`: exact, not wrapped.
    assert_eq!(agree(&format!("{FIB}def main = fib 100")), "354224848179261915075");
    assert_eq!(
        agree(&format!("{FIB}def main = (toInt 0 + fib 100, fib (toUInt8 13) + toUInt8 0)")),
        "(3736710778780434371, 233)"
    );
}

#[test]
fn a_literal_in_generic_code_wraps_at_the_width_it_is_used_at() {
    let src = "fun bump x = x + 200
               def main = (bump (toUInt8 100), bump (toInt16 32600), bump 1)";
    assert_eq!(agree(src), "(44, -32736, 201)");
}

#[test]
fn a_generic_local_is_copied_for_each_type_in_its_scope() {
    let src = "fun f u =
                 let g x = x + 1 in
                 (g (toUInt8 255), g (toInt8 127), g 9223372036854775807)
               def main = f ()";
    assert_eq!(agree(src), "(0, -128, 9223372036854775808)");
}

#[test]
fn a_generic_function_passed_as_a_value_is_copied_too() {
    let src = "fun addOne x = x + 1
               fun apply f x = f x
               def main = (apply addOne (toUInt8 255), apply addOne 9223372036854775807)";
    assert_eq!(agree(src), "(0, 9223372036854775808)");
}

#[test]
fn mutually_recursive_generic_functions_are_copied_together() {
    let src = "fun countDown n acc = if n == 0 then acc else countUp (n - 1) (acc + 1)
               fun countUp n acc = if n == 0 then acc else countDown (n - 1) (acc + 1)
               def main = (countDown (toInt 5) (toUInt8 254), countUp 3 0)";
    assert_eq!(agree(src), "(3, 3)");
}

#[test]
fn a_float_literal_in_generic_code_takes_the_float_type() {
    let src = "fun third x = x /. 3.0
               def main = (third (toFloat32 1.0), third 1.0)";
    assert_eq!(agree(src), "(0.33333334, 0.3333333333333333)");
}
