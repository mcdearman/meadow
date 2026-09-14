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
        variants: pkg.variants.clone(),
        origins: Default::default(),
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
            panic!(
                "VM at {at} failed: {}\n{src}\n{}",
                e.msg,
                image.disassemble()
            )
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
        "True"
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
            "use Shape.*\ndata Shape = Circle Int | Rect Int Int
             def main = Rect 3 4"
        ),
        "Rect(3, 4)"
    );
    assert_eq!(
        agree(
            "use Shape.*\ndata Shape = Circle Int | Rect Int Int
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
            "use Shape.*\ndata Shape = Circle Int | Rect Int Int
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
            "use Opt.*\ndata Opt = None | Some Int
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
            "use Tree.*\ndata Tree = Leaf | Node Tree Int Tree
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
            "use Opt.*\ndata Opt = None | Some Int
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
        "use Opt.*\ndata Opt = None | Some Int
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
            "use Chain.*\ndata Chain = Nil | Cons Int Chain
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
            "use Chain.*\ndata Chain = Nil | Cons Int Chain
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
        variants: Default::default(),
        origins: Default::default(),
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
    assert_eq!(
        agree("fun addPair (a, b) = a + b\ndef main = addPair (1, 2)"),
        "3"
    );
    assert_eq!(agree("def main = let (a, b, c) = (10, 20, 30) in b"), "20");
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
        "use Opt.*\ndata Opt = None | Some Int
         use Chain.*\ndata Chain = Nil | Cons Int Chain
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
    let src = "use Chain.*\ndata Chain = Nil | Cons Int Chain
               fun build n = if n == 0 then Nil else Cons n (build (n - 1))
               fun total xs = match xs with
                 | Nil -> 0
                 | Cons x rest -> x + total rest
               def main = total (build 200000)";
    let (out, collections, allocated) = run_on_vm(src);
    assert_eq!(out, "20000100000");
    assert!(
        collections > 0,
        "the heap never filled — is the test too small?"
    );
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
        agree("use Box.*\ndata Box = Wrap Int\ndef main = Wrap 3"),
        "Wrap(3)"
    );
    assert_eq!(
        agree("use Colour.*\ndata Colour = Red | Green\ndef main = (Red, Green)"),
        "(Red, Green)"
    );
    // Two types owning one constructor name — the reason the qualified form
    // exists. Both print as themselves.
    assert_eq!(
        agree(
            "use Tree.*\ndata Tree = Leaf | Node Tree Tree\n\
             use Rope.*\ndata Rope = Leaf String | Node Rope Rope\n\
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
        "use Shape.*\ndata Shape = Circle Int | Rect Int Int\n\
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
        "(True, True, True, False)"
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
    assert_eq!(
        agree("def main = toUInt64 (toInt (0 - 1))"),
        "18446744073709551615"
    );
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
            "use P.*\ndata P = P UInt8 Int16 Float32
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
    assert_eq!(
        agree("def main = toFloat64 (toFloat32 0.1)"),
        "0.10000000149011612"
    );
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

const CHAIN: &str = "use Chain.*\ndata Chain = End | Link Int Chain
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
    assert_eq!(
        agree("def main = compact (1, \"two\", #[3.0])"),
        "compact (1, \"two\", #[3.0])"
    );
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
    assert_eq!(agree(&src), format!("(5050, 5050, {bytes}, True)"));
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
        let prog = program(&format!(
            "use Opt.*\ndata Opt a = None | Just a\ndef main = compact ({value})"
        ));
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
    use meadow_rts::heap::{Collector, GcConfig, Heap};
    let run = |keep: &str, collector: Collector| {
        let src = format!(
            "{CHAIN}{churn}
             def main = let xs = {keep} in (churn 20000 0, total xs)"
        );
        let prog = program(&src);
        let img = image(&prog);
        let config = GcConfig {
            collector,
            ..GcConfig::from_env()
        };
        let mut vm = meadow_rts::Vm::with_heap(&img, Heap::with_config(1 << 16, config));
        let v = vm
            .run(img.entry.unwrap(), u64::MAX)
            .unwrap_or_else(|e| panic!("{}", e.msg));
        let h = vm.heap();
        (
            vm.show(v),
            h.collections,
            h.copied,
            h.promoted,
            h.region_slots(),
        )
    };

    // Copying: every collection copies the chain, unless it is compacted.
    let (plain, plain_gcs, plain_copied, _, _) = run("build 50000", Collector::Copying);
    let (compacted, gcs, copied, _, region) =
        run("getCompact (compact (build 50000))", Collector::Copying);
    assert_eq!(plain, "(25500000, 1250025000)");
    assert_eq!(compacted, plain);
    assert!(
        plain_gcs > 5 && gcs > 5,
        "both should collect: {plain_gcs}, {gcs}"
    );
    assert_eq!(
        region,
        1 + 50_000 * 3,
        "the chain is in a region, and nothing else is"
    );
    assert!(
        copied * 4 < plain_copied,
        "compacting should spare most of the copying: {copied} slots against {plain_copied}"
    );

    // Generational: the chain is promoted once, as it is built, and never
    // copied again either way -- what compacting spares there is marking it at
    // every cycle, which this program is too small to run. The same answer,
    // and the chain where it should be.
    let (plain, _, _, plain_promoted, _) = run("build 50000", Collector::Generational);
    let (compacted, _, _, _, region) = run(
        "getCompact (compact (build 50000))",
        Collector::Generational,
    );
    assert_eq!(compacted, plain);
    assert!(
        plain_promoted >= 50_000 * 3,
        "the chain outgrew the nursery"
    );
    assert_eq!(region, 1 + 50_000 * 3);
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
    let v = vm
        .run(img.entry.unwrap(), u64::MAX)
        .unwrap_or_else(|e| panic!("{}", e.msg));
    assert_eq!(vm.show(v), "500500000");
    assert!(
        vm.heap().collections > 0,
        "writing regions should have asked for a collection"
    );
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
    assert_eq!(
        agree(&format!("{FIB}def main = fib 100")),
        "354224848179261915075"
    );
    assert_eq!(
        agree(&format!(
            "{FIB}def main = (toInt 0 + fib 100, fib (toUInt8 13) + toUInt8 0)"
        )),
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

// --- green threads -----------------------------------------------------------

// The sequent machine has no scheduler, so these compare the CEK machine -- one
// thread at a time, in order -- against the VM at every optimization level,
// on one OS thread and on several. Only programs whose answer does not depend
// on the interleaving can agree, so that is what these are.

/// Run `src` on the CEK machine and on the VM with 1 and 8 workers, require
/// one answer -- or one failure -- from all of them, and return it.
#[track_caller]
fn threads_agree(src: &str) -> Result<String, String> {
    let prog = program(src);
    let want = meadow_eval::run(&prog)
        .map(|v| v.to_string())
        .map_err(|e| e.msg);
    for opt in LEVELS {
        let lowered = meadow_seq::lower_program(&prog, opt);
        assert!(lowered.unsupported.is_empty(), "{:?}", lowered.unsupported);
        let image = meadow_codegen::compile(&lowered.program).expect("codegen");
        for workers in [1, 8] {
            let got = meadow_rts::sched::run_with(&image, image.entry.unwrap(), FUEL, workers)
                .result
                .map_err(|e| e.msg);
            assert_eq!(
                want,
                got,
                "CEK vs VM at {} with {workers} workers\n{src}",
                opt.name()
            );
        }
    }
    want
}

const FIB_INT: &str = "fun fib (n : Int) = if n < 2 then n else fib (n - 1) + fib (n - 2)\n";

#[test]
fn a_spawned_thread_answers_through_await() {
    let src = format!("{FIB_INT}def main = let t = threadSpawn (\\() -> fib 20) in threadAwait t");
    assert_eq!(threads_agree(&src), Ok("6765".into()));
}

#[test]
fn threads_run_side_by_side_and_are_awaited_in_any_order() {
    let src = format!(
        "{FIB_INT}use Ts.*\ndata Ts = Done | More (Task Int) Ts
         fun spawnAll (n : Int) acc = if n == 0 then acc else spawnAll (n - 1) (More (threadSpawn (\\() -> fib n)) acc)
         fun sumAll ts acc = match ts with | Done -> acc | More t rest -> sumAll rest (acc + threadAwait t)
         def main = sumAll (spawnAll 20 Done) 0"
    );
    assert_eq!(threads_agree(&src), Ok("17710".into()));
}

#[test]
fn a_channel_carries_values_between_threads_in_order() {
    let src = "use L.*\ndata L = Nil | Cons Int L
               def main =
                 let ch = channelNew () in
                 let producer = threadSpawn (\\() ->
                   let rec go (i : Int) = if i > 5 then () else let _ = channelSend ch i in go (i + 1)
                   in go 1) in
                 let rec take (k : Int) acc = if k == 0 then acc else take (k - 1) (Cons (channelReceive ch) acc) in
                 let got = take 5 Nil in
                 let _ = threadAwait producer in
                 got";
    assert_eq!(
        threads_agree(src),
        Ok("Cons(5, Cons(4, Cons(3, Cons(2, Cons(1, Nil)))))".into())
    );
}

#[test]
fn channels_and_threads_can_themselves_be_sent() {
    // A worker is handed the channel to answer on, over another channel.
    let src = "def main =
                 let jobs = channelNew () in
                 let worker = threadSpawn (\\() ->
                   let job = channelReceive jobs in
                   match job with
                   | (n, reply) -> channelSend reply (n * 2)) in
                 let reply = channelNew () in
                 let _ = channelSend jobs (toInt 21, reply) in
                 let answer = channelReceive reply in
                 let _ = threadAwait worker in
                 answer";
    assert_eq!(threads_agree(src), Ok("42".into()));
}

#[test]
fn every_thread_has_its_own_mutable_state() {
    // Each thread makes a cell of its own and counts to a different number in
    // it. Nothing is shared, so nothing is lost however they interleave.
    let src = "fun count (n : Int) = let r = newRef (toInt 0) in
                 let rec go (i : Int) = if i == 0 then getRef r else let _ = setRef r (getRef r + 1) in go (i - 1)
                 in go n
               def main =
                 let a = threadSpawn (\\() -> count 3000) in
                 let b = threadSpawn (\\() -> count 5000) in
                 let c = threadSpawn (\\() -> count 7000) in
                 (threadAwait a, threadAwait b, threadAwait c)";
    assert_eq!(threads_agree(src), Ok("(3000, 5000, 7000)".into()));
}

#[test]
fn many_producers_one_consumer() {
    // Arrival order depends on scheduling; the total does not.
    let src = "fun produce ch (from : Int) = let rec go (i : Int) = if i == 0 then () else
                   let _ = channelSend ch (from + i) in go (i - 1) in go 100
               fun spawnProducers ch (k : Int) = if k == 0 then () else
                   let _ = threadSpawn (\\() -> produce ch (k * 1000)) in spawnProducers ch (k - 1)
               fun consume ch (n : Int) acc = if n == 0 then acc else consume ch (n - 1) (acc + channelReceive ch)
               def main = let ch = channelNew () in let _ = spawnProducers ch 16 in consume ch 1600 0";
    assert_eq!(threads_agree(src), Ok("13680800".into()));
}

#[test]
fn mutable_state_and_continuations_cannot_cross() {
    for (value, what) in [("newRef 1", "a Ref"), ("stNewArray 2 0", "a mutable array")] {
        let src = format!(
            "use L.*\ndata L = Nil | Cons Int L
             def main = let x = {value} in threadAwait (threadSpawn (\\() -> let y = x in 0))"
        );
        let src = src.replace("[;]", "Nil");
        assert_eq!(
            threads_agree(&src),
            Err(meadow_core::thread::unsendable(what)),
            "{value}"
        );
    }
    let src = "effect E { e : () -> Int }
               def main = handle (let n = e () in n) with {
                 e u k -> let _ = threadAwait (threadSpawn (\\() -> let j = k in 0)) in 0 }";
    assert_eq!(
        threads_agree(src),
        Err(meadow_core::thread::unsendable("a continuation"))
    );
}

#[test]
fn a_ref_in_scope_but_unused_does_not_stop_a_spawn() {
    // What crosses is what the function uses -- which the CEK machine, whose
    // closures hold their whole environment, has to work out to agree.
    let src = "def main = let r = newRef (toInt 1) in let x = toInt 41 in threadAwait (threadSpawn (\\() -> x + 1))";
    assert_eq!(threads_agree(src), Ok("42".into()));
}

#[test]
fn a_message_cannot_carry_a_ref_either() {
    let src = "def main = let ch = channelNew () in channelSend ch (newRef (toInt 0))";
    assert_eq!(
        threads_agree(src),
        Err(meadow_core::thread::unsendable("a Ref"))
    );
}

#[test]
fn a_failed_thread_fails_its_await_with_the_same_message() {
    let src = "def main = let t = threadSpawn (\\() -> toInt 1 / toInt 0) in threadAwait t + 1";
    assert_eq!(threads_agree(src), Err("division by zero".into()));
    // Nobody waiting: the failure stays in the thread.
    let src = "def main = let t = threadSpawn (\\() -> toInt 1 / toInt 0) in 7";
    assert_eq!(threads_agree(src), Ok("7".into()));
}

#[test]
fn waiting_on_what_can_never_answer_is_a_deadlock() {
    let src = "def main = let ch = channelNew () in threadAwait (threadSpawn (\\() -> channelReceive ch + toInt 1))";
    assert_eq!(
        threads_agree(src),
        Err(meadow_core::thread::DEADLOCK.into())
    );
}

#[test]
fn a_thread_that_never_waits_does_not_starve_the_others() {
    // `spin` would run forever; the answer comes from the other thread, and
    // `main` ending ends the program.
    let src = "fun spin (n : Int) = spin (n + 1)
               def main = let _ = threadSpawn (\\() -> spin 0) in
                 threadAwait (threadSpawn (\\() -> toInt 99))";
    let prog = program(src);
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    let image = meadow_codegen::compile(&lowered.program).unwrap();
    for workers in [1, 2] {
        let got = meadow_rts::sched::run_with(&image, image.entry.unwrap(), FUEL, workers).result;
        assert_eq!(got.map_err(|e| e.msg), Ok("99".into()), "{workers} workers");
    }
    assert_eq!(
        meadow_eval::run(&prog)
            .map(|v| v.to_string())
            .map_err(|e| e.msg),
        Ok("99".into())
    );
}

#[test]
fn threads_collect_their_own_heaps() {
    // Every thread allocates enough to collect several times. The collections
    // are each thread's own -- the stats count one heap per thread.
    let src = "use L.*\ndata L = Nil | Cons Int L
               fun build (n : Int) = if n == 0 then Nil else Cons n (build (n - 1))
               fun len xs = match xs with | Nil -> toInt 0 | Cons _ r -> 1 + len r
               fun churn (k : Int) acc = if k == 0 then acc else churn (k - 1) (acc + len (build 2000))
               def main =
                 let a = threadSpawn (\\() -> churn 50 0) in
                 let b = threadSpawn (\\() -> churn 50 0) in
                 threadAwait a + threadAwait b";
    let prog = program(src);
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    let image = meadow_codegen::compile(&lowered.program).unwrap();
    let out = meadow_rts::sched::run_with(&image, image.entry.unwrap(), FUEL, 4);
    assert_eq!(out.result.map_err(|e| e.msg), Ok("200000".into()));
    assert_eq!(out.stats.threads, 3, "main and two spawned");
    assert!(
        out.stats.collections >= 2,
        "only {} collections",
        out.stats.collections
    );
}

// --- top-level values -------------------------------------------------------

// A top-level `def` is evaluated once, when first needed, on every engine. The
// VM used to evaluate one at every mention and the CEK machine all of them at
// load, which nothing could see while defs were pure -- except `compactSize`,
// which counts a region the mentions share or do not.

#[test]
fn a_top_level_value_is_evaluated_once() {
    let src = format!(
        "{CHAIN}def c = compact (build 100)
         def main = let c2 = compactAdd c (Link 0 End) in (compactSize c, compactSize c2)"
    );
    // One region, grown by the add: both mentions of `c` are the same value.
    let bytes = (1 + 100 * 3 + 3 + 1) * meadow_core::compact::SLOT_BYTES;
    let (cek, vm) = (cek_and_vm(&src), format!("({bytes}, {bytes})"));
    assert_eq!(cek, vm);
}

#[test]
fn a_top_level_value_nobody_uses_is_never_evaluated() {
    let src = "def boom = toInt 1 / toInt 0
               def main = 7";
    assert_eq!(agree(src), "7");
}

#[test]
fn a_top_level_value_used_by_many_threads_is_the_same_value_in_each() {
    let src = format!(
        "{CHAIN}def big = build 1000
         def main =
           let a = threadSpawn (\\() -> total big) in
           let b = threadSpawn (\\() -> total big) in
           threadAwait a + threadAwait b + total big"
    );
    assert_eq!(threads_agree(&src), Ok("1501500".into()));
}

/// The CEK machine's answer, required to be the VM's too.
#[track_caller]
fn cek_and_vm(src: &str) -> String {
    let prog = program(src);
    let cek = meadow_eval::run(&prog)
        .unwrap_or_else(|e| panic!("CEK: {}", e.msg))
        .to_string();
    let vm = meadow_rts::run(&image(&prog), FUEL).unwrap_or_else(|e| panic!("VM: {}", e.msg));
    assert_eq!(cek, vm, "CEK vs VM\n{src}");
    cek
}

#[test]
fn spawned_work_spreads_to_idle_workers() {
    // Every thread is spawned by `main`, onto `main`'s worker. The others only
    // get any by stealing -- so with CPU-bound work and idle workers, they must.
    let src = format!(
        "{FIB_INT}use Ts.*\ndata Ts = Done | More (Task Int) Ts
         fun spawnAll (n : Int) acc = if n == 0 then acc else spawnAll (n - 1) (More (threadSpawn (\\() -> fib 22)) acc)
         fun sumAll ts acc = match ts with | Done -> acc | More t rest -> sumAll rest (acc + threadAwait t)
         def main = sumAll (spawnAll 16 Done) 0"
    );
    let prog = program(&src);
    let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
    let image = meadow_codegen::compile(&lowered.program).unwrap();
    let out = meadow_rts::sched::run_with(&image, image.entry.unwrap(), FUEL, 4);
    assert_eq!(out.result.map_err(|e| e.msg), Ok((17711 * 16).to_string()));
    assert!(out.stats.stolen > 0, "no worker took any work from main's");
}

#[test]
fn a_compact_crosses_threads_without_being_copied() {
    // On the VM the region is shared: the spawned threads read the chain where
    // `main` compacted it. Every engine gives the same answer.
    let src = format!(
        "{CHAIN}def main =
           let c = compact (build 5000) in
           let a = threadSpawn (\\() -> total (getCompact c)) in
           let b = threadSpawn (\\() -> compactSize c) in
           let ch = channelNew () in
           let _ = channelSend ch c in
           let d = channelReceive ch in
           (threadAwait a, threadAwait b == compactSize c, total (getCompact d))"
    );
    assert_eq!(threads_agree(&src), Ok("(12502500, True, 12502500)".into()));
}

#[test]
fn a_compacted_value_sent_to_a_thread_is_not_copied_into_its_heap() {
    // The same chain handed to a thread twice over: plain, it is copied into
    // the thread's heap; compacted, the thread reads the shared region. The
    // difference in what was allocated is the copy.
    let run = |value: &str| {
        let src = format!(
            "{CHAIN}def main =
               let c = {value} in
               threadAwait (threadSpawn (\\() -> total (getCompact c)))"
        );
        let prog = program(&src);
        let lowered = meadow_seq::lower_program(&prog, meadow_core::OptLevel::default());
        let image = meadow_codegen::compile(&lowered.program).unwrap();
        let out = meadow_rts::sched::run_with(&image, image.entry.unwrap(), FUEL, 2);
        assert_eq!(out.result.map_err(|e| e.msg), Ok("50005000".into()));
        out.stats.allocated
    };
    // The second also hands the plain chain to a thread before compacting it,
    // so it differs from the first only by that copy.
    let compacted = run("compact (build 10000)");
    let copied = run(
        "compact (let x = build 10000 in let t = threadSpawn (\\() -> total x) in let _ = threadAwait t in x)",
    );
    assert!(
        copied >= compacted + 30_000,
        "sending the plain chain should have copied 30_000 slots more: {copied} vs {compacted}"
    );
}

// --- software transactional memory ----------------------------------------

// `Std.Stm`'s handlers, as the cases below need them: these programs are
// compiled without the standard library.
const STM: &str = "use Maybe.*\ndata Maybe a = None | Just a
    effect Stm { retrySignal : () -> (), conflictSignal : () -> () }
    use Attempt.*\ndata Attempt a = Done a | Retried | Conflicted
    fun unreachable u = unreachable u
    fun readTVar tv = match stmRead tv with | Just x -> x | None -> let _ = conflictSignal () in unreachable ()
    fun retry u = let _ = retrySignal () in unreachable ()
    fun orElse (first : () -> a ! { Stm }) (second : () -> a ! { Stm }) =
      let _ = stmNest () in
      match handle Done (first ()) with { retrySignal u k -> Retried, conflictSignal u k -> Conflicted } with
      | Done x -> let _ = stmMerge () in x
      | Retried -> let _ = stmRollback () in second ()
      | Conflicted -> let _ = stmRollback () in let _ = conflictSignal () in unreachable ()
    fun atomically (f : () -> a ! { Stm }) =
      let _ = stmBegin () in
      match handle Done (f ()) with { retrySignal u k -> Retried, conflictSignal u k -> Conflicted } with
      | Done x -> if stmCommit () then x else atomically f
      | Retried -> let _ = stmWait () in atomically f
      | Conflicted -> atomically f\n";

#[test]
fn increments_from_many_threads_are_none_of_them_lost() {
    // Sixteen threads each add one a thousand times to one `TVar`. On the VM
    // they run in parallel and collide constantly; every collision is a
    // transaction run again, and the total is still exact.
    let src = format!(
        "{STM}use Ts.*\ndata Ts = End | More (Task ()) Ts
         fun bump tv (k : Int) = if k == 0 then () else
           let _ = atomically (\\() -> stmWrite tv (readTVar tv + 1)) in bump tv (k - 1)
         fun spawnAll tv (n : Int) acc = if n == 0 then acc else spawnAll tv (n - 1) (More (threadSpawn (\\() -> bump tv 1000)) acc)
         fun awaitAll ts = match ts with | End -> () | More t rest -> let _ = threadAwait t in awaitAll rest
         def main =
           let tv = stmNewIO (toInt 0) in
           let _ = awaitAll (spawnAll tv 16 End) in
           atomically (\\() -> readTVar tv)"
    );
    assert_eq!(threads_agree(&src), Ok("16000".into()));
}

#[test]
fn a_transaction_never_sees_half_of_another() {
    // Two `TVar`s whose sum is always 100, moved between by one thread and read
    // together by another. Any read that saw one side updated and not the other
    // would count as a mismatch.
    let src = format!(
        "{STM}fun shuffle a b (k : Int) = if k == 0 then () else
           let _ = atomically (\\() ->
             let x = readTVar a in
             let _ = stmWrite a (x - 1) in
             stmWrite b (readTVar b + 1)) in shuffle a b (k - 1)
         fun watch a b (k : Int) (bad : Int) = if k == 0 then bad else
           let s = atomically (\\() -> readTVar a + readTVar b) in
           watch a b (k - 1) (if s == 100 then bad else bad + 1)
         def main =
           let a = stmNewIO (toInt 100) in
           let b = stmNewIO (toInt 0) in
           let mover = threadSpawn (\\() -> shuffle a b 2000) in
           let watcher = threadSpawn (\\() -> watch a b 2000 0) in
           let _ = threadAwait mover in
           (threadAwait watcher, atomically (\\() -> (readTVar a, readTVar b)))"
    );
    assert_eq!(threads_agree(&src), Ok("(0, (-1900, 2000))".into()));
}

#[test]
fn retry_blocks_until_there_is_something_to_take() {
    // A one-slot mailbox: `take` retries while it is empty, `put` while it is
    // full. The consumer starts first and has to wait for every value.
    let src = format!(
        "{STM}fun take box = atomically (\\() -> match readTVar box with | Just x -> let _ = stmWrite box None in x | None -> retry ())
         fun put box x = atomically (\\() -> match readTVar box with | None -> stmWrite box (Just x) | Just _ -> retry ())
         fun consume box (k : Int) acc = if k == 0 then acc else consume box (k - 1) (acc + take box)
         fun produce box (k : Int) = if k == 0 then () else let _ = put box k in produce box (k - 1)
         def main =
           let box = stmNewIO None in
           let consumer = threadSpawn (\\() -> consume box 500 (toInt 0)) in
           let producer = threadSpawn (\\() -> produce box 500) in
           let _ = threadAwait producer in
           threadAwait consumer"
    );
    assert_eq!(threads_agree(&src), Ok("125250".into()));
}

#[test]
fn or_else_falls_back_and_keeps_only_what_succeeded() {
    let src = format!(
        "{STM}def main =
           let tv = stmNewIO (toInt 1) in
           let got = atomically (\\() ->
             orElse (\\() -> let _ = stmWrite tv 50 in retry ()) (\\() -> let _ = stmWrite tv (readTVar tv + 1) in readTVar tv)) in
           (got, atomically (\\() -> readTVar tv))"
    );
    assert_eq!(threads_agree(&src), Ok("(2, 2)".into()));
}

#[test]
fn a_tvar_holds_only_what_a_compact_can() {
    for (value, what) in [("newRef 1", "a Ref"), ("\\x -> x", "a function")] {
        let src = format!("{STM}def main = let tv = stmNewIO ({value}) in 0");
        assert_eq!(
            threads_agree(&src),
            Err(meadow_core::stm::unstorable(what)),
            "{value}"
        );
    }
}

#[test]
fn a_transaction_operation_outside_atomically_says_so() {
    let src = format!("{STM}def main = let tv = stmNewIO (toInt 1) in stmRead tv");
    assert_eq!(
        threads_agree(&src),
        Err(meadow_core::stm::outside("readTVar"))
    );
}

#[test]
fn waiting_on_a_tvar_nobody_writes_is_a_deadlock() {
    let src = format!(
        "{STM}def main = let tv = stmNewIO (toInt 0) in
           atomically (\\() -> if readTVar tv == 0 then retry () else readTVar tv)"
    );
    assert_eq!(
        threads_agree(&src),
        Err(meadow_core::thread::DEADLOCK.into())
    );
}
