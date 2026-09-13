//! **Top-level definitions, evaluated once.**
//!
//! A mention of a top-level name lowers to a jump to its definition, so without
//! this pass `def table = build 1000000` builds the table again at every use.
//! That is only a matter of speed, because a top-level `def` other than `main`
//! cannot perform effects -- the type checker refuses one that does -- and
//! evaluating a pure expression twice gives the same value twice.
//!
//! So each definition that is not a function gets a cache. Its body becomes
//!
//! ```text
//! if cached i then cached-value i
//! else let v = <body> in let _ = cache i v in v
//! ```
//!
//! with the three [`Prim::GlobalReady`], [`Prim::GlobalGet`] and
//! [`Prim::GlobalSet`], where `i` is the definition's position. The first use
//! computes and keeps the value; every later one reads it back. A definition
//! nothing uses is never evaluated, and one that fails is not cached, so it
//! fails the same way wherever it is used.
//!
//! A function needs none of this: its value is the closure, which is as cheap to
//! make as to look up, and calling it is supposed to run it every time. Nor does
//! a literal, or the entry point, which the runtime evaluates exactly once.
//!
//! The cache belongs to the machine running the code. On the bytecode VM that is
//! one green thread, and each keeps its own -- a thread never reads another's
//! heap -- so a definition is computed at most once per thread that uses it.
//! Runs on erased core, just before lowering.

use crate::*;

/// Where the pass's own names start: clear of units, of synthetic definitions,
/// and of specialized copies.
pub const GLOBALS_BASE: u32 = 0x7C00_0000;

pub fn program(p: &Program) -> Program {
    let mut next = GLOBALS_BASE;
    let mut fresh = || {
        let v = hir::VarId(next);
        next += 1;
        v
    };
    let defs = p
        .defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // The entry point runs once as the program; a literal costs nothing
            // to evaluate again.
            if Some(d.var) == p.entry || is_function(&d.term) || is_literal(&d.term) {
                return d.clone();
            }
            let index = || Term::Lit(Lit::Int(i as i64));
            let value = fresh();
            let ignored = fresh();
            let ty = || unknown();
            let compute = Term::Let(
                value,
                Poly::mono(ty()),
                Arc::new(d.term.clone()),
                Arc::new(Term::Let(
                    ignored,
                    Poly::mono(ty()),
                    Arc::new(Term::Prim(
                        Prim::GlobalSet,
                        vec![index(), Term::Var(value)],
                        ty(),
                    )),
                    Arc::new(Term::Var(value)),
                )),
            );
            let term = Term::If(
                Arc::new(Term::Prim(Prim::GlobalReady, vec![index()], ty())),
                Arc::new(Term::Prim(Prim::GlobalGet, vec![index()], ty())),
                Arc::new(compute),
            );
            Def {
                var: d.var,
                name: d.name,
                poly: d.poly.clone(),
                term,
            }
        })
        .collect();
    Program {
        defs,
        entry: p.entry,
        ctor_fields: p.ctor_fields.clone(),
    }
}

fn is_literal(t: &Term) -> bool {
    match t {
        Term::Loc(_, inner) => is_literal(inner),
        Term::Lit(_) => true,
        _ => false,
    }
}

/// Is this definition a function -- a value that is only a closure?
pub fn is_function(t: &Term) -> bool {
    match t {
        Term::Loc(_, inner) | Term::TyLam(_, inner) => is_function(inner),
        Term::Lam(..) => true,
        _ => false,
    }
}
