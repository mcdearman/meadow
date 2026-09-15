//! A bottom-up rewrite of a term: every subterm and pattern rebuilt, then
//! handed to a function that may replace it. The traversal every small pass
//! over core needs, written once.

use crate::*;

/// Rebuild `t` bottom-up: children first, then `term` on the rebuilt node.
/// Patterns go through `pat` the same way, children first.
pub fn term(t: &Term, f: &mut dyn FnMut(Term) -> Term, p: &mut dyn FnMut(Pat) -> Pat) -> Term {
    let go = |x: &Term, f: &mut dyn FnMut(Term) -> Term, p: &mut dyn FnMut(Pat) -> Pat| {
        Arc::new(term(x, f, p))
    };
    let rebuilt = match t {
        Term::Var(_) | Term::Lit(_) | Term::Error => t.clone(),
        Term::Loc(l, inner) => Term::Loc(*l, go(inner, f, p)),
        Term::Lam(v, ty, body) => Term::Lam(*v, ty.clone(), go(body, f, p)),
        Term::TyLam(bs, body) => Term::TyLam(bs.clone(), go(body, f, p)),
        Term::App(a, b) => {
            let a = go(a, f, p);
            Term::App(a, go(b, f, p))
        }
        Term::TyApp(a, tys) => Term::TyApp(go(a, f, p), tys.clone()),
        Term::Let(v, poly, rhs, body) => {
            let rhs = go(rhs, f, p);
            Term::Let(*v, poly.clone(), rhs, go(body, f, p))
        }
        Term::LetRec(binds, body) => {
            let binds = binds
                .iter()
                .map(|(v, poly, t)| (*v, poly.clone(), term(t, f, p)))
                .collect();
            Term::LetRec(binds, go(body, f, p))
        }
        Term::If(c, a, b) => {
            let c = go(c, f, p);
            let a = go(a, f, p);
            Term::If(c, a, go(b, f, p))
        }
        Term::Tuple(xs) => Term::Tuple(xs.iter().map(|x| term(x, f, p)).collect()),
        Term::Proj(x, i) => Term::Proj(go(x, f, p), *i),
        Term::Array(xs, ty) => Term::Array(xs.iter().map(|x| term(x, f, p)).collect(), ty.clone()),
        Term::Record(fs) => Term::Record(fs.iter().map(|(l, x)| (*l, term(x, f, p))).collect()),
        Term::Sel(x, l, ty) => Term::Sel(go(x, f, p), *l, ty.clone()),
        Term::Extend(x, l, v) => {
            let x = go(x, f, p);
            Term::Extend(x, *l, go(v, f, p))
        }
        Term::Ctor(n, ty, xs) => {
            Term::Ctor(*n, ty.clone(), xs.iter().map(|x| term(x, f, p)).collect())
        }
        Term::Case(s, arms, ty) => {
            let s = go(s, f, p);
            let arms = arms
                .iter()
                .map(|(pt, g, b)| {
                    (
                        pattern(pt, p),
                        g.as_ref().map(|g| term(g, f, p)),
                        term(b, f, p),
                    )
                })
                .collect();
            Term::Case(s, arms, ty.clone())
        }
        Term::Prim(op, xs, ty) => {
            Term::Prim(*op, xs.iter().map(|x| term(x, f, p)).collect(), ty.clone())
        }
        Term::Perform(e, op, a, ty) => Term::Perform(*e, *op, go(a, f, p), ty.clone()),
        Term::Handle {
            body,
            clauses,
            ret,
            ty,
        } => {
            let body = go(body, f, p);
            let clauses = clauses
                .iter()
                .map(|c| HClause {
                    body: term(&c.body, f, p),
                    ..c.clone()
                })
                .collect();
            let ret = ret.as_ref().map(|(v, t, b)| (*v, t.clone(), go(b, f, p)));
            Term::Handle {
                body,
                clauses,
                ret,
                ty: ty.clone(),
            }
        }
    };
    f(rebuilt)
}

/// Rebuild a pattern bottom-up through `p`.
pub fn pattern(pt: &Pat, p: &mut dyn FnMut(Pat) -> Pat) -> Pat {
    let rebuilt = match pt {
        Pat::Wild | Pat::Var(..) | Pat::Lit(_) => pt.clone(),
        Pat::As(v, ty, sub) => Pat::As(*v, ty.clone(), Box::new(pattern(sub, p))),
        Pat::Tuple(ps) => Pat::Tuple(ps.iter().map(|x| pattern(x, p)).collect()),
        Pat::Array(ps) => Pat::Array(ps.iter().map(|x| pattern(x, p)).collect()),
        Pat::Ctor(n, ps) => Pat::Ctor(*n, ps.iter().map(|x| pattern(x, p)).collect()),
        Pat::Record(fs) => Pat::Record(fs.iter().map(|(l, x)| (*l, pattern(x, p))).collect()),
    };
    p(rebuilt)
}
