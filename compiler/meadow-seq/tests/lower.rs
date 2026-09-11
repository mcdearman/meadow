//! What the lowering actually emits.
//!
//! `meadow_rts`'s differential tests already check that the translation *means*
//! the same thing as the CEK machine. These check that it means it in the
//! intended shape — that returning is an `invoke` with no arguments, that
//! calling is a `substitute` and an `invoke`, and above all that a block's
//! parameters cover the whole environment. The last one is the property the rest
//! of the pipeline is going to rely on, and a test that reads the dump is the
//! cheapest way to notice it slipping.

use meadow_core::{Def, Lit, Prim, Program, Term};
use meadow_hir::VarId;
use meadow_seq::{lower_program, Block, Statement};
use std::sync::Arc;

fn main_def(term: Term) -> Program {
    let var = VarId(100);
    Program {
        defs: vec![Def {
            var,
            name: "main".into(),
            term,
        }],
        entry: Some(var),
        ctor_fields: Default::default(),
    }
}

#[test]
fn a_literal_is_an_extern_and_a_return() {
    let lowered = lower_program(&main_def(Term::Lit(Lit::Int(42))), meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());
    let text = lowered.program.pretty();
    // The literal producer, then the universal return sequence: arrange the
    // environment as exactly [k, v] and invoke the continuation's only method.
    assert!(text.contains("extern lit Int(42)()"), "{text}");
    assert!(text.contains("substitute ["), "{text}");
    assert!(text.contains("invoke "), "{text}");
    assert!(text.contains("#0"), "{text}");
}

#[test]
fn every_block_binds_the_whole_environment() {
    // `(1 + 2) * 3` — three operands, so the continuation of each has to keep
    // what came before it alive. If a `keep` set were wrong, some block's
    // parameter list would be shorter than the environment reaching it, and the
    // AxCut machine would refuse to enter it. Here we check the static side:
    // every `substitute` supplies exactly as many values as its block takes.
    let term = Term::Prim(
        Prim::Mul,
        vec![
            Term::Prim(Prim::Add, vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))]),
            Term::Lit(Lit::Int(3)),
        ],
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());

    let mut checked = 0;
    for def in &lowered.program.defs {
        walk(&def.block.body, &mut |s| {
            if let Statement::Substitute(sel, block) = s {
                assert_eq!(
                    sel.len(),
                    block.params.len(),
                    "substitute of {} into a block of {}",
                    sel.len(),
                    block.params.len()
                );
                checked += 1;
            }
        });
    }
    assert!(checked > 0, "no substitutions to check");
}

#[test]
fn calling_a_lambda_is_a_permutation_and_a_branch() {
    // `(\x -> x) 1`. The call site should end in exactly the two statements the
    // IR promises: rearrange the registers, then jump into the method.
    let x = VarId(1);
    let term = Term::App(
        Arc::new(Term::Lam(x, Arc::new(Term::Var(x)))),
        Arc::new(Term::Lit(Lit::Int(1))),
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());

    let mut found = false;
    for def in &lowered.program.defs {
        walk(&def.block.body, &mut |s| {
            if let Statement::Substitute(sel, block) = s
                && sel.len() == 3
                && matches!(block.body, Statement::Invoke(f, 0) if f == sel[0])
            {
                found = true;
            }
        });
    }
    assert!(
        found,
        "expected `substitute [f, arg, k] in {{ invoke f#0 }}`\n{}",
        lowered.program.pretty()
    );
}

#[test]
fn a_global_reference_is_a_jump_with_the_environment_untouched() {
    let g = VarId(50);
    let m = VarId(51);
    let program = Program {
        defs: vec![
            Def {
                var: g,
                name: "g".into(),
                term: Term::Lit(Lit::Int(7)),
            },
            Def {
                var: m,
                name: "main".into(),
                term: Term::Var(g),
            },
        ],
        entry: Some(m),
        ctor_fields: Default::default(),
    };
    let lowered = lower_program(&program, meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());

    // `substitute [k] in {(k) => jump #0}` — a one-element environment, which is
    // exactly what `g`'s block takes, so the jump needs no shuffling.
    let main = &lowered.program.defs[1].block.body;
    match main {
        Statement::Substitute(sel, block) => {
            assert_eq!(sel.len(), 1);
            assert_eq!(block.params.len(), 1);
            assert!(matches!(block.body, Statement::Jump(l) if l.0 == 0));
        }
        other => panic!("expected a substitute and a jump, got {other:?}"),
    }
}

#[test]
fn a_variable_from_nowhere_is_reported() {
    // Every `core` construct lowers now, so the only thing left to report is a
    // variable that is neither in scope nor a definition — a bug upstream. It
    // still has to be *said* rather than turned into a plausible statement.
    let lowered = lower_program(&main_def(Term::Var(VarId(999))), meadow_core::OptLevel::default());
    assert!(
        lowered
            .unsupported
            .contains(&meadow_seq::Unsupported::UnboundVar),
        "an unbound variable should be reported, not silently mistranslated"
    );
}

#[test]
fn a_match_becomes_a_switch_with_a_default() {
    // `match x with | Just y -> y | _ -> 0`, as core builds it.
    let x = VarId(1);
    let y = VarId(2);
    let term = Term::Let(
        x,
        Arc::new(Term::Ctor("Just".into(), vec![Term::Lit(Lit::Int(9))])),
        Arc::new(Term::Case(
            Arc::new(Term::Var(x)),
            vec![
                (
                    meadow_core::Pat::Ctor("Just".into(), vec![meadow_core::Pat::Var(y)]),
                    Term::Var(y),
                ),
                (meadow_core::Pat::Wild, Term::Lit(Lit::Int(0))),
            ],
        )),
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());

    let mut switches = 0;
    for def in &lowered.program.defs {
        walk(&def.block.body, &mut |s| {
            if let Statement::Switch { arms, default, .. } = s {
                // One arm — the constructor the pattern names — and a default
                // that binds no fields, because it knows of none. `Just` has one
                // field, and the scrutinee stays, so the arm binds exactly one
                // more value than the default does.
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].1.params.len(), default.params.len() + 1);
                switches += 1;
            }
        });
    }
    assert_eq!(switches, 1, "{}", lowered.program.pretty());
}

#[test]
fn a_letrec_becomes_labels_sharing_one_parameter_list() {
    // `let n = 1 in letrec f = \x -> g x; g = \x -> f n in f 0`
    //
    // Both bindings capture `n`, so both blocks take `[n, k]` and a reference to
    // either is a substitute of that shape followed by a jump. The point is that
    // mutual recursion needs no object pointing at itself.
    let n = VarId(1);
    let f = VarId(2);
    let g = VarId(3);
    let a = VarId(4);
    let b = VarId(5);
    let term = Term::Let(
        n,
        Arc::new(Term::Lit(Lit::Int(1))),
        Arc::new(Term::LetRec(
            vec![
                (
                    f,
                    Term::Lam(a, Arc::new(Term::App(Arc::new(Term::Var(g)), Arc::new(Term::Var(a))))),
                ),
                (
                    g,
                    Term::Lam(b, Arc::new(Term::App(Arc::new(Term::Var(f)), Arc::new(Term::Var(n))))),
                ),
            ],
            Arc::new(Term::App(Arc::new(Term::Var(f)), Arc::new(Term::Lit(Lit::Int(0))))),
        )),
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());
    assert!(lowered.unsupported.is_empty());

    // One definition for `main`, one per `letrec` binding.
    assert_eq!(lowered.program.defs.len(), 3, "{}", lowered.program.pretty());
    for def in &lowered.program.defs[1..] {
        assert_eq!(
            def.block.params,
            vec![n, def.block.params[1]],
            "a lifted binding takes the group's captures, then the continuation"
        );
    }
}

/// Every statement in a tree, including the bodies of nested blocks.
fn walk(s: &Statement, f: &mut impl FnMut(&Statement)) {
    f(s);
    let blocks: Vec<&Block> = match s {
        Statement::Substitute(_, b) => vec![b],
        Statement::Switch { arms, default, .. } => {
            let mut bs: Vec<&Block> = arms.iter().map(|(_, b)| b).collect();
            bs.push(default);
            bs
        }
        Statement::Extern { blocks, .. } => blocks.iter().collect(),
        Statement::New { methods, rest, .. } => {
            walk(rest, f);
            methods.iter().collect()
        }
        Statement::Let { rest, .. } | Statement::Handle { rest, .. } => {
            walk(rest, f);
            vec![]
        }
        Statement::Unhandle { rest, .. } => {
            walk(rest, f);
            vec![]
        }
        Statement::Jump(_)
        | Statement::Invoke(..)
        | Statement::Perform { .. }
        | Statement::Error(_) => vec![],
    };
    for b in blocks {
        walk(&b.body, f);
    }
}

/// A literal operand rides inside the `extern` instead of becoming a value.
///
/// `n - 1` used to be two statements and two environment slots: one to produce
/// the `1`, one to subtract it, and the literal stayed live until something
/// dropped it. The folded form is what removes the register, not only the
/// instruction.
#[test]
fn a_literal_operand_is_folded_into_the_primitive() {
    let term = Term::Prim(
        Prim::Sub,
        vec![Term::Lit(Lit::Int(7)), Term::Lit(Lit::Int(1))],
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());

    let mut ops = Vec::new();
    for def in &lowered.program.defs {
        walk(&def.block.body, &mut |s| {
            if let Statement::Extern { op, args, .. } = s {
                ops.push((op.clone(), args.len()));
            }
        });
    }
    // The `7` is still produced — only the right operand folds — and the
    // subtraction takes it as its one argument.
    assert!(
        ops.iter()
            .any(|(op, n)| matches!(op, meadow_seq::Extern::PrimK(Prim::Sub, Lit::Int(1))) && *n == 1),
        "expected a folded `Sub .. 1`, got {ops:?}"
    );
    assert!(
        !ops.iter().any(|(op, _)| matches!(op, meadow_seq::Extern::Prim(Prim::Sub))),
        "the unfolded form should be gone: {ops:?}"
    );
}

/// `if n == 0` is one statement: the comparison, the literal and the branch.
#[test]
fn a_comparison_fuses_into_the_branch_that_tests_it() {
    let n = VarId(1);
    let term = Term::Let(
        n,
        Arc::new(Term::Lit(Lit::Int(3))),
        Arc::new(Term::If(
            Arc::new(Term::Prim(
                Prim::Eq,
                vec![Term::Var(n), Term::Lit(Lit::Int(0))],
            )),
            Arc::new(Term::Lit(Lit::Int(1))),
            Arc::new(Term::Lit(Lit::Int(2))),
        )),
    );
    let lowered = lower_program(&main_def(term), meadow_core::OptLevel::default());

    let mut found = false;
    for def in &lowered.program.defs {
        walk(&def.block.body, &mut |s| {
            if let Statement::Extern { op, args, blocks } = s {
                if let meadow_seq::Extern::BranchPrimK(Prim::Eq, Lit::Int(0)) = op {
                    assert_eq!(args.len(), 1, "one operand; the other is the literal");
                    assert_eq!(blocks.len(), 2, "false and true");
                    found = true;
                }
                assert!(
                    !matches!(op, meadow_seq::Extern::Branch),
                    "the unfused branch should be gone"
                );
            }
        });
    }
    assert!(found, "{}", lowered.program.pretty());
}

/// At `-O2` a `match` over distinct constructors is one `switch`.
#[test]
fn case_trees_replace_the_chain_at_o2() {
    use meadow_core::{OptLevel, Pat};

    // `match x with | Nothing -> 0 | Just y -> y | _ -> 0`
    let x = VarId(1);
    let y = VarId(2);
    let case = Term::Case(
        Arc::new(Term::Var(x)),
        vec![
            (Pat::Ctor("Nothing".into(), vec![]), Term::Lit(Lit::Int(0))),
            (
                Pat::Ctor("Just".into(), vec![Pat::Var(y)]),
                Term::Var(y),
            ),
            (Pat::Wild, Term::Lit(Lit::Int(0))),
        ],
    );
    let term = Term::Let(
        x,
        Arc::new(Term::Ctor("Just".into(), vec![Term::Lit(Lit::Int(9))])),
        Arc::new(case),
    );

    let arms_at = |opt| {
        let lowered = lower_program(&main_def(term.clone()), opt);
        let mut widths = Vec::new();
        for def in &lowered.program.defs {
            walk(&def.block.body, &mut |s| {
                if let Statement::Switch { arms, .. } = s {
                    widths.push(arms.len());
                }
            });
        }
        widths.sort_unstable();
        widths
    };

    // One switch per arm, each testing one tag ...
    assert_eq!(arms_at(OptLevel::O1), vec![1, 1]);
    // ... against one switch that tests both.
    assert_eq!(arms_at(OptLevel::O2), vec![2]);
}
