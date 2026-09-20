//! **Inlining a small top-level function at the places that call it.**
//!
//! # Not in the pipeline
//!
//! This pass is written, tested and *not run*. Turned on, it broke two
//! differential tests against the CEK machine -- a record pattern whose bound
//! name came out with no representation, and an STM program with a variable
//! out of scope by code generation -- and the cause was not found. What is here
//! is correct on everything it is tested against and is left for whoever picks
//! it up; `meadow_seq::lower_program` does not call it.
//!
//! It is worth picking up. Eleven of the thirteen definitions a matrix
//! multiply reaches are polymorphic wrappers like `St.get`, and each one costs
//! a closure, an indirect call, a jump and a return to do what a single
//! instruction does.
//!
//! A mention of a top-level name lowers to a *jump to its definition*, and a
//! jump is a transfer of control — so a call that is not in tail position has
//! to record where to come back to, and a machine with no call stack records
//! that as an object on the heap. That is the right price for a function. It is
//! a ruinous one for
//!
//! ```text
//! @pub fun get a i = stGetArray a i
//! ```
//!
//! which is what `Std.St`'s array accessors are: one line each, wrapping a
//! primitive that cannot transfer control at all. Lowering emits a primitive
//! where it stands, so `stGetArray a i` written out costs one instruction — and
//! `St.get a i`, which means exactly that, cost a closure, an indirect call, a
//! jump and a return. A matrix multiply's inner loop is three of these, so it
//! paid for six continuations and six calls per element to do three loads and a
//! store.
//!
//! # What is inlined
//!
//! A definition is a candidate when it is
//!
//! * **a function**: some lambdas, then a body;
//! * **type-applied at the call site** if it is polymorphic, so that its body
//!   can be instantiated. Core is still typed here and a name's representation
//!   comes from its type all the way down to code generation, so a copy of a
//!   body has to carry the types that copy is *for* — see [`retype`]. This is
//!   not an optional refinement: eleven of the thirteen definitions a matrix
//!   multiply reaches are polymorphic, `Std.St`'s array accessors among them,
//!   because they are generic in the state type `runSt` invents. Refusing them
//!   left this pass with nothing to do;
//! * **not recursive**, directly or through any chain of definitions — which is
//!   computed rather than guessed at, because a wrapper calling a wrapper is the
//!   ordinary case and refusing those would leave most of the library alone;
//! * **small**, under [`BUDGET`] nodes.
//!
//! and the call is **saturated**: exactly as many arguments as parameters.
//! Anything else is left as it is.
//!
//! # Why it is safe to copy a body
//!
//! Core binds every name once, and the rest of the compiler relies on it:
//! [`crate::simplify`] substitutes without renaming for that reason, and
//! `meadow_seq` numbers its environment by it. A body copied to two call sites
//! would break it. So every copy is [`freshen`]ed first — every binder in it,
//! and every mention of one, given a number nothing else has.
//!
//! The arguments become `let`s rather than being substituted, which keeps them
//! evaluated once and in order whatever the body does with its parameters.
//! [`crate::simplify`] then removes the ones that cost nothing to copy, which
//! is most of them, so `St.get a i` really does end up as the primitive and
//! nothing else.

use crate::*;
use std::collections::{HashMap, HashSet};

/// Where this pass's own names start: clear of units, synthetic definitions,
/// specialized copies, [`crate::globals`] and [`crate::simplify`].
pub const INLINE_BASE: u32 = 0x7D00_0000;

/// The most nodes a definition's body may have and still be copied to its call
/// sites.
///
/// Small, on purpose. This is not a general inliner with a cost model: it is
/// here for the one-line wrapper, and a one-line wrapper is four or five nodes.
/// Raising it trades code size for calls saved, and nothing measured so far
/// asks for that.
pub const BUDGET: usize = 16;

/// How many times inlining and simplification alternate. A wrapper around a
/// wrapper needs two; nothing in the standard library has needed three, and the
/// bound is what stops mutual recursion between two small definitions from
/// doubling the program on every pass.
const ROUNDS: usize = 3;

/// Inline what is worth inlining, simplifying after each round so that the
/// `let`s a call becomes collapse before the next one looks at them.
pub fn program(p: &Program) -> Program {
    let mut out = p.clone();
    let mut fresh = Fresh(INLINE_BASE);
    for _ in 0..ROUNDS {
        let Some(bodies) = candidates(&out) else {
            break;
        };
        let mut changed = false;
        for d in &mut out.defs {
            let next = term(&d.term, &bodies, &mut fresh);
            changed |= next != d.term;
            d.term = next;
        }
        if !changed {
            break;
        }
        out = simplify::program(&out);
    }
    out
}

/// A source of names nothing else uses.
struct Fresh(u32);

impl Fresh {
    fn var(&mut self) -> Var {
        let v = hir::VarId(self.0);
        self.0 += 1;
        v
    }
}

/// What a candidate definition is: the type binders a call has to supply, its
/// parameters, and what to do with them.
struct Body {
    binders: Vec<TyVar>,
    params: Vec<(Var, Ty)>,
    term: Term,
}

/// Every definition worth inlining, or `None` if there are none.
fn candidates(p: &Program) -> Option<HashMap<Var, Body>> {
    let recursive = recursive(p);
    let mut out = HashMap::new();
    for d in &p.defs {
        if recursive.contains(&d.var) {
            continue;
        }
        let Some(body) = function(&d.term) else {
            continue;
        };
        // A polymorphic definition's term is a `TyLam` binding exactly the
        // binders its type declares. One where they disagree is not something
        // to guess about: its body would be instantiated at the wrong types, or
        // not at all.
        if body.binders.len() != d.poly.binders.len() {
            continue;
        }
        if body.params.is_empty() || size(&body.term, BUDGET) > BUDGET {
            continue;
        }
        out.insert(d.var, body);
    }
    (!out.is_empty()).then_some(out)
}

/// A definition's type binders, parameters and body, if it is a function.
///
/// A polymorphic definition is a `TyLam` around the lambdas, and its binders
/// are what a call's [`Term::TyApp`] supplies. More than one `TyLam` is not a
/// shape core makes, and is refused rather than guessed at.
fn function(t: &Term) -> Option<Body> {
    let mut binders = Vec::new();
    let mut params = Vec::new();
    let mut t = t;
    loop {
        match t {
            Term::Loc(_, inner) => t = inner,
            Term::TyLam(bs, body) if binders.is_empty() && params.is_empty() => {
                binders = bs.clone();
                t = body;
            }
            Term::TyLam(..) => return None,
            Term::Lam(v, ty, body) => {
                params.push((*v, ty.clone()));
                t = body;
            }
            _ => {
                return Some(Body {
                    binders,
                    params,
                    term: t.clone(),
                });
            }
        }
    }
}

/// Definitions that can reach themselves through the definitions they mention.
///
/// Not just direct recursion: `a` calling `b` calling `a` would otherwise be
/// inlined into itself round after round, and the round limit would be all that
/// stopped it -- having already doubled the program twice.
fn recursive(p: &Program) -> HashSet<Var> {
    let own: HashSet<Var> = p.defs.iter().map(|d| d.var).collect();
    let calls: HashMap<Var, HashSet<Var>> = p
        .defs
        .iter()
        .map(|d| {
            let mut free = HashSet::new();
            free_vars_into(&d.term, &mut free);
            (d.var, free.intersection(&own).copied().collect())
        })
        .collect();
    // The transitive closure, a definition at a time: `reach[v]` is everything
    // `v` can get to. Programs here are thousands of definitions, so the square
    // is affordable and the simple thing is the right thing.
    let mut out = HashSet::new();
    for d in &p.defs {
        let mut seen = HashSet::new();
        let mut todo = vec![d.var];
        while let Some(v) = todo.pop() {
            for &w in calls.get(&v).into_iter().flatten() {
                if w == d.var {
                    out.insert(d.var);
                    todo.clear();
                    break;
                }
                if seen.insert(w) {
                    todo.push(w);
                }
            }
        }
    }
    out
}

/// How many nodes `t` has, giving up once past `cap`.
fn size(t: &Term, cap: usize) -> usize {
    let mut n = 0;
    let mut count = |t: Term| {
        n += 1;
        t
    };
    // Bottom-up and whole, because `rewrite::term` has no way to stop early.
    // `cap` is small and so are the terms this is asked about -- a definition
    // whose body is enormous is counted once and then never looked at again,
    // since `candidates` runs per round on a program that is not growing much.
    rewrite::term(t, &mut count, &mut |p| p);
    n.min(cap + 1)
}

// --- the rewrite -----------------------------------------------------------

fn term(t: &Term, bodies: &HashMap<Var, Body>, fresh: &mut Fresh) -> Term {
    rewrite::term(
        t,
        &mut |t| match call(&t, bodies) {
            Some((body, tys, args)) => enter(body, tys, args, fresh),
            None => t,
        },
        &mut |p| p,
    )
}

/// `t` as a saturated call to something worth inlining: what to inline, the
/// types it is called at, and its arguments in order.
fn call<'b>(t: &Term, bodies: &'b HashMap<Var, Body>) -> Option<(&'b Body, Vec<Ty>, Vec<Term>)> {
    let mut args = Vec::new();
    let mut tys: Vec<Ty> = Vec::new();
    let mut head = t;
    loop {
        match head {
            Term::Loc(_, inner) => head = inner,
            Term::App(f, a) => {
                args.push((**a).clone());
                head = f;
            }
            // The instantiation core wrote down for a polymorphic name. It sits
            // under the applications, because it is part of the name.
            Term::TyApp(f, at) if tys.is_empty() => {
                tys = at.clone();
                head = f;
            }
            Term::Var(v) => {
                let body = bodies.get(v)?;
                // Saturated, and no more: a call with extra arguments applies
                // what the body answers, which is not this rewrite's business.
                // And instantiated exactly, or the body's types cannot be made
                // the ones this copy is for.
                if args.len() != body.params.len() || tys.len() != body.binders.len() {
                    return None;
                }
                args.reverse();
                return Some((body, tys, args));
            }
            _ => return None,
        }
    }
}

/// The body at these types, with its parameters bound to the arguments.
fn enter(body: &Body, tys: Vec<Ty>, args: Vec<Term>, fresh: &mut Fresh) -> Term {
    let at = retype(body, &tys);
    let copy = freshen(&at, fresh);

    copy.params
        .iter()
        .zip(args)
        .rev()
        .fold(copy.term, |acc, ((v, ty), arg)| {
            Term::Let(*v, Poly::mono(ty.clone()), Arc::new(arg), Arc::new(acc))
        })
}

/// `body` with its type binders replaced by `tys`, everywhere a type appears.
///
/// Core stays typed until lowering, and a name's representation comes from its
/// type -- so a copy of a generic body placed where `a` is `Float` has to say
/// `Float`, or code generation will read the words the wrong way. Every node
/// that carries a type is rewritten here, and so is every annotated pattern.
fn retype(body: &Body, tys: &[Ty]) -> Body {
    if body.binders.is_empty() {
        return Body {
            binders: Vec::new(),
            params: body.params.clone(),
            term: body.term.clone(),
        };
    }
    let map: HashMap<u32, Ty> = body
        .binders
        .iter()
        .map(|b| b.id)
        .zip(tys.iter().cloned())
        .collect();
    let at = |ty: &Ty| subst_rigid(ty, &map);
    let term = rewrite::term(
        &body.term,
        &mut |t| match t {
            Term::Lam(v, ty, b) => Term::Lam(v, at(&ty), b),
            Term::Let(v, poly, rhs, b) => Term::Let(v, at_poly(&poly, &map), rhs, b),
            Term::LetRec(binds, b) => Term::LetRec(
                binds
                    .into_iter()
                    .map(|(v, poly, t)| (v, at_poly(&poly, &map), t))
                    .collect(),
                b,
            ),
            Term::TyApp(f, args) => Term::TyApp(f, args.iter().map(&at).collect()),
            Term::Array(xs, ty) => Term::Array(xs, at(&ty)),
            Term::Sel(x, l, ty) => Term::Sel(x, l, at(&ty)),
            Term::Ctor(n, ty, xs) => Term::Ctor(n, at(&ty), xs),
            Term::Case(s, arms, ty) => Term::Case(s, arms, at(&ty)),
            Term::Prim(p, xs, ty) => Term::Prim(p, xs, at(&ty)),
            Term::Perform(e, o, x, ty) => Term::Perform(e, o, x, at(&ty)),
            Term::Jump(j, xs, ty) => Term::Jump(j, xs, at(&ty)),
            Term::Join {
                var,
                params,
                ty,
                rhs,
                body,
            } => Term::Join {
                var,
                params: params.into_iter().map(|(v, t)| (v, at(&t))).collect(),
                ty: at(&ty),
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
                        param_ty: at(&c.param_ty),
                        resume_ty: at(&c.resume_ty),
                        ..c
                    })
                    .collect(),
                ret: ret.map(|(v, t, b)| (v, at(&t), b)),
                ty: at(&ty),
            },
            t => t,
        },
        &mut |p| match p {
            Pat::Var(v, ty) => Pat::Var(v, at(&ty)),
            Pat::As(v, ty, inner) => Pat::As(v, at(&ty), inner),
            p => p,
        },
    );
    Body {
        binders: Vec::new(),
        params: body.params.iter().map(|(v, ty)| (*v, at(ty))).collect(),
        term,
    }
}

/// A `let`'s polytype at the same substitution. Its own binders shadow, so
/// anything it quantifies is left alone.
fn at_poly(poly: &Poly, map: &HashMap<u32, Ty>) -> Poly {
    let mut map = map.clone();
    for b in &poly.binders {
        map.remove(&b.id);
    }
    Poly {
        binders: poly.binders.clone(),
        ty: subst_rigid(&poly.ty, &map),
    }
}

/// A copy of `body` in which every bound name is one nothing else uses.
///
/// Core binds every name once and the whole compiler below here relies on it.
/// Two copies of one body at two call sites would bind the same names twice, so
/// each copy is renamed as it is made.
fn freshen(body: &Body, fresh: &mut Fresh) -> Body {
    let mut map: HashMap<Var, Var> = HashMap::new();
    for (v, _) in &body.params {
        map.insert(*v, fresh.var());
    }
    for v in bound(&body.term) {
        map.entry(v).or_insert_with(|| fresh.var());
    }
    let rename = |v: &Var| map.get(v).copied().unwrap_or(*v);
    let term = rewrite::term(
        &body.term,
        &mut |t| match t {
            Term::Var(v) => Term::Var(rename(&v)),
            Term::Lam(v, ty, b) => Term::Lam(rename(&v), ty, b),
            Term::Let(v, poly, rhs, b) => Term::Let(rename(&v), poly, rhs, b),
            Term::LetRec(binds, b) => Term::LetRec(
                binds
                    .into_iter()
                    .map(|(v, poly, t)| (rename(&v), poly, t))
                    .collect(),
                b,
            ),
            Term::Join {
                var,
                params,
                ty,
                rhs,
                body,
            } => Term::Join {
                var: rename(&var),
                params: params.into_iter().map(|(v, ty)| (rename(&v), ty)).collect(),
                ty,
                rhs,
                body,
            },
            Term::Jump(j, args, ty) => Term::Jump(rename(&j), args, ty),
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
                        param: rename(&c.param),
                        resume: rename(&c.resume),
                        ..c
                    })
                    .collect(),
                ret: ret.map(|(v, t, b)| (rename(&v), t, b)),
                ty,
            },
            t => t,
        },
        &mut |p| match p {
            Pat::Var(v, ty) => Pat::Var(rename(&v), ty),
            Pat::As(v, ty, inner) => Pat::As(rename(&v), ty, inner),
            p => p,
        },
    );
    Body {
        binders: Vec::new(),
        params: body
            .params
            .iter()
            .map(|(v, ty)| (rename(v), ty.clone()))
            .collect(),
        term,
    }
}

/// Every name `t` binds anywhere inside it, patterns and handler clauses
/// included.
fn bound(t: &Term) -> HashSet<Var> {
    let out = std::cell::RefCell::new(HashSet::new());
    let mut note = |t: Term| {
        let mut out = out.borrow_mut();
        match &t {
            Term::Lam(v, _, _) | Term::Let(v, _, _, _) => {
                out.insert(*v);
            }
            Term::LetRec(binds, _) => out.extend(binds.iter().map(|(v, _, _)| *v)),
            Term::Join { var, params, .. } => {
                out.insert(*var);
                out.extend(params.iter().map(|(v, _)| *v));
            }
            Term::Handle { clauses, ret, .. } => {
                for c in clauses {
                    out.insert(c.param);
                    out.insert(c.resume);
                }
                if let Some((v, _, _)) = ret {
                    out.insert(*v);
                }
            }
            _ => {}
        }
        t
    };
    let mut in_pattern = |p: Pat| {
        if let Pat::Var(v, _) | Pat::As(v, _, _) = &p {
            out.borrow_mut().insert(*v);
        }
        p
    };
    rewrite::term(t, &mut note, &mut in_pattern);
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use meadow_hir::VarId;

    fn v(n: u32) -> Var {
        VarId(n)
    }

    fn con(name: &str) -> Ty {
        Ty::Con(InternedString::from(name), Vec::new())
    }

    /// `fun f a b = a + b`, the shape of every one-line wrapper in `Std`.
    fn wrapper(f: Var, poly: Poly) -> Def {
        let (a, b) = (v(10), v(11));
        let body = Term::Prim(Prim::Add, vec![Term::Var(a), Term::Var(b)], con("Int"));
        Def {
            var: f,
            name: InternedString::from("f"),
            poly,
            term: Term::Lam(
                a,
                con("Int"),
                Arc::new(Term::Lam(b, con("Int"), Arc::new(body))),
            ),
        }
    }

    fn calling(f: Var, args: Vec<Term>) -> Def {
        Def {
            var: v(1),
            name: InternedString::from("main"),
            poly: Poly::mono(con("Int")),
            term: args
                .into_iter()
                .fold(Term::Var(f), |g, a| Term::App(Arc::new(g), Arc::new(a))),
        }
    }

    fn run(defs: Vec<Def>) -> Term {
        let p = Program {
            defs,
            ..Default::default()
        };
        program(&p).defs[0].term.clone()
    }

    /// The whole point: a saturated call to a small wrapper becomes what the
    /// wrapper does, with no call left.
    #[test]
    fn a_saturated_call_to_a_wrapper_becomes_its_body() {
        let f = v(2);
        let got = run(vec![
            calling(f, vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))]),
            wrapper(f, Poly::mono(con("Int"))),
        ]);
        assert_eq!(
            got,
            Term::Prim(
                Prim::Add,
                vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))],
                con("Int")
            ),
            "{got:?}"
        );
    }

    /// A call with too few arguments is a partial application, which is a
    /// closure and not this pass's business.
    #[test]
    fn an_unsaturated_call_is_left_alone() {
        let f = v(2);
        let before = calling(f, vec![Term::Lit(Lit::Int(1))]);
        let got = run(vec![before.clone(), wrapper(f, Poly::mono(con("Int")))]);
        assert_eq!(got, before.term);
    }

    /// A polymorphic wrapper is inlined at the types the call instantiates it
    /// with, and its body says so afterwards. Core stays typed until lowering
    /// and representations come from types, so this is the part that has to be
    /// right rather than merely fast.
    #[test]
    fn a_polymorphic_wrapper_is_inlined_at_its_types() {
        // `fun f (x : t) = #[x] : Array t`, at `t = Int`.
        let (f, x) = (v(2), v(10));
        let t = Ty::Var(7);
        let binders = vec![TyVar {
            id: 7,
            kind: VarKind::Type,
        }];
        let d = Def {
            var: f,
            name: InternedString::from("f"),
            poly: Poly {
                binders: binders.clone(),
                ty: t.clone(),
            },
            term: Term::TyLam(
                binders,
                Arc::new(Term::Lam(
                    x,
                    t.clone(),
                    Arc::new(Term::Array(vec![Term::Var(x)], t.clone())),
                )),
            ),
        };
        let main = Def {
            var: v(1),
            name: InternedString::from("main"),
            poly: Poly::mono(con("Int")),
            term: Term::App(
                Arc::new(Term::TyApp(Arc::new(Term::Var(f)), vec![con("Int")])),
                Arc::new(Term::Lit(Lit::Int(5))),
            ),
        };
        let got = run(vec![main, d]);
        assert_eq!(
            got,
            Term::Array(vec![Term::Lit(Lit::Int(5))], con("Int")),
            "the element type is the one the call asked for"
        );
    }

    /// A definition whose declared polytype and whose term disagree about how
    /// many types it takes is refused rather than instantiated at none.
    #[test]
    fn a_definition_whose_type_and_term_disagree_is_refused() {
        let f = v(2);
        let poly = Poly {
            binders: vec![TyVar {
                id: 7,
                kind: VarKind::Type,
            }],
            ty: con("Int"),
        };
        // `wrapper` builds a term with no `TyLam`, so this one claims a binder
        // it does not have.
        let before = calling(f, vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))]);
        let got = run(vec![before.clone(), wrapper(f, poly)]);
        assert_eq!(got, before.term);
    }

    /// A definition that can reach itself is never copied into itself.
    #[test]
    fn a_recursive_definition_is_not_inlined() {
        let f = v(2);
        let mut d = wrapper(f, Poly::mono(con("Int")));
        d.term = Term::Lam(
            v(10),
            con("Int"),
            Arc::new(Term::Lam(
                v(11),
                con("Int"),
                Arc::new(Term::App(
                    Arc::new(Term::Var(f)),
                    Arc::new(Term::Var(v(10))),
                )),
            )),
        );
        let before = calling(f, vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))]);
        let got = run(vec![before.clone(), d]);
        assert_eq!(got, before.term);
    }

    /// Two calls to one wrapper must not end up binding the same names: core
    /// binds every name once and everything below here relies on it.
    #[test]
    fn two_call_sites_do_not_share_names() {
        // `fun f x = let z = x + x in z * z` -- one parameter, and a binder
        // inside that gets copied with the body.
        let (f, x, z) = (v(2), v(20), v(21));
        let sum = Term::Prim(Prim::Add, vec![Term::Var(x), Term::Var(x)], con("Int"));
        let body = Term::Let(
            z,
            Poly::mono(con("Int")),
            Arc::new(sum),
            Arc::new(Term::Prim(
                Prim::Mul,
                vec![Term::Var(z), Term::Var(z)],
                con("Int"),
            )),
        );
        let d = Def {
            var: f,
            name: InternedString::from("f"),
            poly: Poly::mono(con("Int")),
            term: Term::Lam(x, con("Int"), Arc::new(body)),
        };
        let call = |k: i64| Term::App(Arc::new(Term::Var(f)), Arc::new(Term::Lit(Lit::Int(k))));
        let main = Def {
            var: v(1),
            name: InternedString::from("main"),
            poly: Poly::mono(con("Int")),
            term: Term::Tuple(vec![call(1), call(2)]),
        };
        let got = run(vec![main, d]);
        let mut seen = std::collections::HashSet::new();
        let mut note = |t: Term| {
            if let Term::Let(b, _, _, _) = &t {
                assert!(seen.insert(*b), "{b:?} is bound twice in {got:?}");
            }
            t
        };
        rewrite::term(&got, &mut note, &mut |p| p);
        assert_eq!(seen.len(), 2, "two copies, two names: {got:?}");
        assert!(!seen.contains(&z), "and neither is the original");
    }
}
