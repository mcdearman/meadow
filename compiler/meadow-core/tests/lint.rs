//! The core type checker, on core that is wrong on purpose.
//!
//! Every case here is a transformation a real optimisation pass could
//! plausibly get wrong: instantiating at the wrong type, dropping a type
//! argument, applying a function to the wrong thing, binding a pattern
//! variable at a type the scrutinee does not have. The point of a typed core
//! is that none of those reach the back end, and the point of these tests is
//! that the checker is what stops them.

use meadow_core::{lint, Def, Lit, Pat, Poly, Prim, Program, Term, Ty, TyVar};
use meadow_hir::VarId;
use meadow_infer::{Scheme, Type, VarKind};
use std::collections::HashMap;
use std::sync::Arc;

fn check(program: &Program) -> Vec<String> {
    lint::check(program, &Default::default(), &Default::default())
}

fn ok(program: &Program) {
    let problems = check(program);
    assert!(problems.is_empty(), "should have passed: {problems:?}");
}

fn fails(program: &Program, expect: &str) {
    let problems = check(program);
    assert!(
        problems.iter().any(|p| p.contains(expect)),
        "expected a complaint about {expect:?}, got {problems:?}"
    );
}

fn def(var: VarId, poly: Poly, term: Term) -> Program {
    Program {
        defs: vec![Def { var, name: "it".into(), poly, term }],
        entry: None,
        ..Default::default()
    }
}

fn int() -> Ty {
    Type::int()
}

/// `forall a. a -> a`, with `a` the rigid variable `id`.
fn ident_poly(id: u32) -> Poly {
    let a = Type::Var(id);
    Poly {
        binders: vec![TyVar { id, kind: VarKind::Type }],
        ty: Type::Fun(vec![a.clone()], Box::new(a), Box::new(Type::RowEmpty)),
    }
}

/// `id` and a `main` that uses it however the caller says.
fn with_id(use_it: impl FnOnce(VarId) -> Term) -> Program {
    let f = VarId(1);
    let x = VarId(3);
    let poly = ident_poly(9);
    Program {
        defs: vec![
            Def {
                var: f,
                name: "id".into(),
                poly: poly.clone(),
                term: Term::TyLam(
                    poly.binders.clone(),
                    Arc::new(Term::Lam(x, Type::Var(9), Arc::new(Term::Var(x)))),
                ),
            },
            Def {
                var: VarId(2),
                name: "main".into(),
                poly: Poly::mono(int()),
                term: use_it(f),
            },
        ],
        entry: None,
        ..Default::default()
    }
}

#[test]
fn a_well_typed_definition_passes() {
    ok(&with_id(|f| {
        Term::App(
            Arc::new(Term::TyApp(Arc::new(Term::Var(f)), vec![int()])),
            Arc::new(Term::Lit(Lit::Int(1))),
        )
    }));
}

#[test]
fn a_definition_whose_body_has_another_type_is_caught() {
    fails(
        &def(VarId(1), Poly::mono(int()), Term::Lit(Lit::Str("no".into()))),
        "definition's type is not what its body has",
    );
}

#[test]
fn applying_the_wrong_argument_type_is_caught() {
    let x = VarId(2);
    let term = Term::App(
        Arc::new(Term::Lam(x, int(), Arc::new(Term::Var(x)))),
        Arc::new(Term::Lit(Lit::Str("s".into()))),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "argument has the wrong type");
}

#[test]
fn applying_something_that_is_not_a_function_is_caught() {
    let term = Term::App(
        Arc::new(Term::Lit(Lit::Int(1))),
        Arc::new(Term::Lit(Lit::Int(2))),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "not a function");
}

#[test]
fn a_polymorphic_mention_without_type_arguments_is_caught() {
    // What a pass does when it moves a mention and forgets its instantiation.
    fails(
        &with_id(|f| {
            Term::App(Arc::new(Term::Var(f)), Arc::new(Term::Lit(Lit::Int(1))))
        }),
        "without type arguments",
    );
}

#[test]
fn instantiating_with_the_wrong_number_of_arguments_is_caught() {
    fails(
        &with_id(|f| {
            Term::App(
                Arc::new(Term::TyApp(Arc::new(Term::Var(f)), vec![int(), int()])),
                Arc::new(Term::Lit(Lit::Int(1))),
            )
        }),
        "type argument",
    );
}

#[test]
fn instantiating_at_the_wrong_type_is_caught() {
    // `id @String 1` — a specialization that substituted the wrong type.
    fails(
        &with_id(|f| {
            Term::App(
                Arc::new(Term::TyApp(Arc::new(Term::Var(f)), vec![Type::string()])),
                Arc::new(Term::Lit(Lit::Int(1))),
            )
        }),
        "argument has the wrong type",
    );
}

#[test]
fn branches_that_disagree_are_caught() {
    let term = Term::If(
        Arc::new(Term::Lit(Lit::Bool(true))),
        Arc::new(Term::Lit(Lit::Int(1))),
        Arc::new(Term::Lit(Lit::Str("two".into()))),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "branches of `if`");
}

#[test]
fn a_non_boolean_condition_is_caught() {
    let term = Term::If(
        Arc::new(Term::Lit(Lit::Int(0))),
        Arc::new(Term::Lit(Lit::Int(1))),
        Arc::new(Term::Lit(Lit::Int(2))),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "condition of `if`");
}

#[test]
fn an_arm_that_does_not_produce_the_case_type_is_caught() {
    let term = Term::Case(
        Arc::new(Term::Lit(Lit::Int(0))),
        vec![(Pat::Var(VarId(2), int()), Term::Lit(Lit::Str("s".into())))],
        int(),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "arm of `case`");
}

#[test]
fn a_pattern_variable_bound_at_the_wrong_type_is_caught() {
    let term = Term::Case(
        Arc::new(Term::Lit(Lit::Int(0))),
        // Matching an `Int` and calling the binding a `String`.
        vec![(Pat::Var(VarId(2), Type::string()), Term::Lit(Lit::Int(1)))],
        int(),
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "pattern variable");
}

#[test]
fn an_unbound_variable_is_caught() {
    fails(
        &def(VarId(1), Poly::mono(int()), Term::Var(VarId(99))),
        "unbound variable",
    );
}

#[test]
fn a_primitive_with_the_wrong_arity_is_caught() {
    let term = Term::Prim(Prim::Add, vec![Term::Lit(Lit::Int(1))], int());
    fails(&def(VarId(1), Poly::mono(int()), term), "operand");
}

#[test]
fn projecting_past_the_end_of_a_tuple_is_caught() {
    let term = Term::Proj(
        Arc::new(Term::Tuple(vec![
            Term::Lit(Lit::Int(1)),
            Term::Lit(Lit::Int(2)),
        ])),
        5,
    );
    fails(&def(VarId(1), Poly::mono(int()), term), "projected field 5");
}

#[test]
fn an_imported_binding_is_known_by_its_scheme() {
    // Defined in another unit, so the checker has its scheme and nothing else.
    let outside = VarId(50);
    let scheme = Scheme {
        quant: vec![VarKind::Type],
        ty: Type::Fun(
            vec![Type::Bound(0)],
            Box::new(Type::Bound(0)),
            Box::new(Type::RowEmpty),
        ),
    };
    let imported: HashMap<VarId, Scheme> = [(outside, scheme)].into_iter().collect();

    let call = |at: Ty| Program {
        defs: vec![Def {
            var: VarId(1),
            name: "main".into(),
            poly: Poly::mono(int()),
            term: Term::App(
                Arc::new(Term::TyApp(Arc::new(Term::Var(outside)), vec![at])),
                Arc::new(Term::Lit(Lit::Int(1))),
            ),
        }],
        entry: None,
        ..Default::default()
    };

    let good = lint::check(&call(int()), &Default::default(), &imported);
    assert!(good.is_empty(), "{good:?}");

    let bad = lint::check(&call(Type::string()), &Default::default(), &imported);
    assert!(
        !bad.is_empty(),
        "an imported binding was used at a type it does not have"
    );
}

#[test]
fn effects_are_not_compared() {
    // Two arrows differing only in their latent effect are one type here:
    // core has no effect discipline, and every backend erases them.
    let x = VarId(2);
    let pure = Type::Fun(vec![int()], Box::new(int()), Box::new(Type::RowEmpty));
    let effectful = Type::Fun(
        vec![int()],
        Box::new(int()),
        Box::new(Type::RowExtend(
            "io".into(),
            Box::new(Type::Tuple(vec![])),
            Box::new(Type::RowEmpty),
        )),
    );
    let term = Term::Lam(x, int(), Arc::new(Term::Var(x)));
    ok(&def(VarId(1), Poly::mono(pure), term.clone()));
    ok(&def(VarId(1), Poly::mono(effectful), term));
}

#[test]
fn a_row_written_in_another_order_is_the_same_row() {
    let v = VarId(1);
    let row = |a: &str, b: &str| {
        Type::Record(Box::new(Type::RowExtend(
            a.into(),
            Box::new(int()),
            Box::new(Type::RowExtend(
                b.into(),
                Box::new(Type::string()),
                Box::new(Type::RowEmpty),
            )),
        )))
    };
    let _ = row("x", "y");
    // The record builds `x` then `y`; the annotation says `y` then `x`.
    let term = Term::Record(vec![
        ("x".into(), Term::Lit(Lit::Int(1))),
        ("y".into(), Term::Lit(Lit::Str("s".into()))),
    ]);
    ok(&def(
        v,
        Poly::mono(Type::Record(Box::new(Type::RowExtend(
            "y".into(),
            Box::new(Type::string()),
            Box::new(Type::RowExtend(
                "x".into(),
                Box::new(int()),
                Box::new(Type::RowEmpty),
            )),
        )))),
        term,
    ));
}

#[test]
fn the_unknown_type_is_accepted_anywhere() {
    // What lowering writes for a unit that already failed to compile: a
    // second complaint about the first one's consequences helps nobody.
    let term = Term::App(
        Arc::new(Term::Lam(
            VarId(2),
            meadow_core::unknown(),
            Arc::new(Term::Lit(Lit::Int(1))),
        )),
        Arc::new(Term::Lit(Lit::Str("anything".into()))),
    );
    ok(&def(VarId(1), Poly::mono(int()), term));
}
