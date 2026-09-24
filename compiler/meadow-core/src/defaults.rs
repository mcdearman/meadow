//! **Unsolved type variables, defaulted.**
//!
//! Inference can finish with a variable nothing ever constrained: the type of
//! `None` in `assertEq None None`, or the effect row of a pure call bound by a
//! `let`. It is not generalized -- it belongs to no binder -- so core records
//! it as a type variable that nothing binds, in an instantiation or on an
//! annotation, and then that type means nothing in particular.
//!
//! Any type would do, which is the point of it being unconstrained, so it is
//! given one: what GHC does with an ambiguous type once nothing more can be
//! learned about it. By what the variable stands for --
//!
//! | it stands for | it becomes |
//! |---|---|
//! | an effect row, a record row | the empty row |
//! | an integer type (`Num`) | `Int` |
//! | a fractional type (`Frac`) | `Float` |
//! | any other type | `()` |
//!
//! -- read from the binder it instantiates when it is a type argument, and
//! from where it sits otherwise: the effect of a function type is a row, and
//! so is the tail of a row. A literal still waiting for its number type is
//! given the same default.
//!
//! "Nothing binds it" is judged per definition: a variable some binder in the
//! definition binds -- its own, a `TyLam`'s, a local `let`'s -- is bound, since
//! variable ids are unique to the unit and a binder's are its own.

use crate::*;
use meadow_infer::VarKind;
use std::collections::{HashMap, HashSet};

/// Default every unsolved variable in `defs`. `kinds` says what the binders of
/// each name a definition may instantiate stand for: this unit's definitions
/// and whatever it imports.
pub fn defs(defs: &mut [Def], imported: &HashMap<Var, Vec<VarKind>>) {
    let mut kinds: HashMap<Var, Vec<VarKind>> = imported.clone();
    for d in defs.iter() {
        kinds.insert(d.var, d.poly.binders.iter().map(|b| b.kind).collect());
    }
    for d in defs.iter_mut() {
        let mut bound: HashSet<u32> = d.poly.binders.iter().map(|b| b.id).collect();
        let mut local = kinds.clone();
        rewrite::visit(&d.term, &mut |t| {
            match t {
                Term::TyLam(bs, _) => bound.extend(bs.iter().map(|b| b.id)),
                Term::Let(v, p, _, _) => {
                    bound.extend(p.binders.iter().map(|b| b.id));
                    local.insert(*v, p.binders.iter().map(|b| b.kind).collect());
                }
                Term::LetRec(binds, _) => {
                    for (v, p, _) in binds {
                        bound.extend(p.binders.iter().map(|b| b.id));
                        local.insert(*v, p.binders.iter().map(|b| b.kind).collect());
                    }
                }
                _ => {}
            }
            true
        });
        let fix = Fix {
            bound: &bound,
            kinds: &local,
        };
        if !fix.needed(&d.term) {
            continue;
        }
        d.term = rewrite::term(&d.term, &mut |t| fix.node(t), &mut |p| fix.pat(p));
    }
}

struct Fix<'a> {
    bound: &'a HashSet<u32>,
    kinds: &'a HashMap<Var, Vec<VarKind>>,
}

impl Fix<'_> {
    /// Whether anything in `t` needs a default -- most definitions have none,
    /// and are not rebuilt.
    fn needed(&self, t: &Term) -> bool {
        let mut found = false;
        rewrite::visit(t, &mut |x| {
            let tys: Vec<&Ty> = match x {
                Term::TyApp(_, args) => args.iter().collect(),
                Term::Lam(_, ty, _) | Term::Array(_, ty) | Term::Sel(_, _, ty) => vec![ty],
                Term::Ctor(_, ty, _) | Term::Case(_, _, ty) | Term::Prim(_, _, ty) => vec![ty],
                Term::Perform(_, _, _, ty) | Term::Jump(_, _, ty) => vec![ty],
                Term::Let(_, p, _, _) => vec![&p.ty],
                Term::Join { params, ty, .. } => params
                    .iter()
                    .map(|(_, t)| t)
                    .chain(std::iter::once(ty))
                    .collect(),
                Term::Lit(Lit::AnyInt(_, v) | Lit::AnyFloat(_, v)) => {
                    found |= !self.bound.contains(v);
                    vec![]
                }
                _ => vec![],
            };
            found |= tys.iter().any(|t| self.unsolved(t));
            !found
        });
        // Patterns and handlers are rebuilt anyway when anything is; a
        // definition whose only unsolved variable sits in one is rare enough
        // to be caught by the full rewrite the next time something else is.
        found
    }

    fn unsolved(&self, t: &Ty) -> bool {
        match t {
            InferType::Var(v) => !self.bound.contains(v),
            InferType::Con(_, xs) | InferType::Tuple(xs) => xs.iter().any(|x| self.unsolved(x)),
            InferType::Fun(ps, r, e) => {
                ps.iter().any(|x| self.unsolved(x)) || self.unsolved(r) || self.unsolved(e)
            }
            InferType::Record(r) => self.unsolved(r),
            InferType::RowExtend(_, f, r) => self.unsolved(f) || self.unsolved(r),
            InferType::Bound(_) | InferType::RowEmpty | InferType::Error => false,
        }
    }

    /// `t` with its unsolved variables defaulted, `t` itself standing for a
    /// `kind`.
    fn ty(&self, t: &Ty, kind: VarKind) -> Ty {
        match t {
            InferType::Var(v) if !self.bound.contains(v) => default(kind),
            InferType::Var(_) | InferType::Bound(_) | InferType::RowEmpty | InferType::Error => {
                t.clone()
            }
            InferType::Con(n, xs) => {
                InferType::Con(*n, xs.iter().map(|x| self.ty(x, VarKind::Type)).collect())
            }
            InferType::Tuple(xs) => {
                InferType::Tuple(xs.iter().map(|x| self.ty(x, VarKind::Type)).collect())
            }
            InferType::Fun(ps, r, e) => InferType::Fun(
                ps.iter().map(|x| self.ty(x, VarKind::Type)).collect(),
                Box::new(self.ty(r, VarKind::Type)),
                Box::new(self.ty(e, VarKind::Effect)),
            ),
            InferType::Record(r) => InferType::Record(Box::new(self.ty(r, VarKind::Row))),
            InferType::RowExtend(l, f, r) => InferType::RowExtend(
                *l,
                Box::new(self.ty(f, VarKind::Type)),
                Box::new(self.ty(r, kind)),
            ),
        }
    }

    fn plain(&self, t: &Ty) -> Ty {
        self.ty(t, VarKind::Type)
    }

    fn node(&self, t: Term) -> Term {
        match t {
            Term::TyApp(f, args) => {
                let binders = match f.peel() {
                    Term::Var(v) => self.kinds.get(v).cloned(),
                    _ => None,
                };
                let args = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let kind = binders
                            .as_ref()
                            .and_then(|ks| ks.get(i).copied())
                            .unwrap_or(VarKind::Type);
                        self.ty(a, kind)
                    })
                    .collect();
                Term::TyApp(f, args)
            }
            Term::Lam(v, ty, b) => Term::Lam(v, self.plain(&ty), b),
            Term::Let(v, p, rhs, b) => Term::Let(
                v,
                Poly {
                    binders: p.binders.clone(),
                    ty: self.plain(&p.ty),
                },
                rhs,
                b,
            ),
            Term::LetRec(binds, b) => Term::LetRec(
                binds
                    .into_iter()
                    .map(|(v, p, t)| {
                        let ty = self.plain(&p.ty);
                        (
                            v,
                            Poly {
                                binders: p.binders,
                                ty,
                            },
                            t,
                        )
                    })
                    .collect(),
                b,
            ),
            Term::Array(xs, ty) => Term::Array(xs, self.plain(&ty)),
            Term::Sel(x, l, ty) => Term::Sel(x, l, self.plain(&ty)),
            Term::Ctor(n, ty, xs) => Term::Ctor(n, self.plain(&ty), xs),
            Term::Case(s, arms, ty) => Term::Case(s, arms, self.plain(&ty)),
            Term::Prim(p, xs, ty) => Term::Prim(p, xs, self.plain(&ty)),
            Term::Perform(e, op, a, ty) => Term::Perform(e, op, a, self.plain(&ty)),
            Term::Jump(j, args, ty) => Term::Jump(j, args, self.plain(&ty)),
            Term::Join {
                var,
                params,
                ty,
                rhs,
                body,
            } => Term::Join {
                var,
                params: params
                    .into_iter()
                    .map(|(v, t)| (v, self.plain(&t)))
                    .collect(),
                ty: self.plain(&ty),
                rhs,
                body,
            },
            Term::Handle {
                body,
                clauses,
                ret,
                ty,
            } => Term::Handle {
                body,
                clauses: clauses
                    .into_iter()
                    .map(|c| HClause {
                        param_ty: self.plain(&c.param_ty),
                        resume_ty: self.plain(&c.resume_ty),
                        ..c
                    })
                    .collect(),
                ret: ret.map(|(v, t, b)| (v, self.plain(&t), b)),
                ty: self.plain(&ty),
            },
            Term::Lit(Lit::AnyInt(n, v)) if !self.bound.contains(&v) => Term::Lit(Lit::Int(n)),
            Term::Lit(Lit::AnyFloat(x, v)) if !self.bound.contains(&v) => Term::Lit(Lit::Float(x)),
            t => t,
        }
    }

    fn pat(&self, p: Pat) -> Pat {
        match p {
            Pat::Var(v, ty) => Pat::Var(v, self.plain(&ty)),
            Pat::As(v, ty, sub) => Pat::As(v, self.plain(&ty), sub),
            p => p,
        }
    }
}

fn default(kind: VarKind) -> Ty {
    match kind {
        VarKind::Row | VarKind::Effect => InferType::RowEmpty,
        VarKind::Num => InferType::int(),
        VarKind::Frac => InferType::float(),
        _ => InferType::unit(),
    }
}
