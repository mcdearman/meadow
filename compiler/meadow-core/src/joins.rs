//! **Naming a shared continuation**: turning a `let`-bound function that is
//! only ever tail-called into a [`Term::Join`].
//!
//! A `let f = \x -> … in …` where every mention of `f` is a saturated call in
//! tail position is not really a function. Nothing can hold it, nothing can
//! pass it anywhere, and nothing observes it after it answers -- it is a label
//! several places branch to. Compiled as a function it is a closure to
//! allocate, a capture list to fill and an indirect call to make; compiled as a
//! join point it is a block and a jump.
//!
//! # What has to hold
//!
//! For `f` bound to a lambda of `n` parameters, in the body of the `let`:
//!
//! * every mention of `f` is the head of a call with exactly `n` arguments --
//!   never bare, never partially applied, never an argument to something else;
//! * every one of those calls is in **tail position** of the body.
//!
//! Tail position is where the answer of the body is the answer of the call:
//! through `Loc`, either branch of an `if`, the body of a `let` (not its
//! right-hand side), every arm of a `case`, and nowhere else. A call anywhere
//! else has work waiting after it, so it is not a jump, and the block would
//! have to answer somewhere its jumps do not agree on.
//!
//! # What this is for
//!
//! The immediate saving is the closure. The larger one comes later: a
//! simplifier that pushes a context into the branches of a `case` -- the
//! case-of-case transformation -- has to put the context *somewhere*, and its
//! choices are to copy it into every branch, which can square the size of a
//! program, or to build a closure for it, which allocates. Naming it is the
//! third answer, and it is the reason GHC has join points at all.

use crate::*;

/// Every `let`-bound function in `p` that is only tail-called becomes a join
/// point.
pub fn program(p: &Program) -> Program {
    let mut out = p.clone();
    for d in &mut out.defs {
        d.term = term(&d.term);
    }
    out
}

/// One term, children first -- so a join point inside another's right-hand
/// side is found too.
pub fn term(t: &Term) -> Term {
    rewrite::term(t, &mut convert, &mut |q| q)
}

/// A rebuilt `let` of a lambda that is only tail-called, as a join point.
fn convert(t: Term) -> Term {
    let Term::Let(v, poly, rhs, body) = &t else {
        return t;
    };
    let Some((params, rhs_body)) = lambda(rhs) else {
        return t;
    };
    let arity = params.len();
    if !only_jumped_to(body, *v, arity) {
        return t;
    }
    let ty = returns(&poly.ty, arity);
    Term::Join {
        var: *v,
        params,
        ty: ty.clone(),
        rhs: Arc::new(rhs_body),
        body: Arc::new(rewrite_calls(body, *v, arity, &ty)),
    }
}

/// A lambda's parameters and its body, through any `Loc`. `None` for anything
/// else, including a type abstraction: a generic binding's value depends on the
/// types it is used at, and a join point has no room for that.
fn lambda(t: &Term) -> Option<(Vec<(Var, Ty)>, Term)> {
    let mut params = Vec::new();
    let mut here = t;
    loop {
        match here {
            Term::Loc(_, inner) => here = inner,
            Term::Lam(v, ty, body) => {
                params.push((*v, ty.clone()));
                here = body;
            }
            _ => break,
        }
    }
    (!params.is_empty()).then(|| (params, here.clone()))
}

/// What a function type answers after `n` arguments.
fn returns(ty: &Ty, n: usize) -> Ty {
    match ty {
        InferType::Fun(_, ret, _) if n > 0 => returns(ret, n - 1),
        _ => ty.clone(),
    }
}

/// Is every mention of `v` in `t` a saturated call to it in tail position?
fn only_jumped_to(t: &Term, v: Var, arity: usize) -> bool {
    let mut seen = 0;
    let ok = walk(t, v, arity, true, &mut seen);
    // A binding nothing mentions is left alone: making it a join point would
    // only move it, and `prune` is what removes it.
    ok && seen > 0
}

/// `tail` says whether `t` is in tail position of the body being examined.
fn walk(t: &Term, v: Var, arity: usize, tail: bool, seen: &mut usize) -> bool {
    match t {
        Term::Var(x) => *x != v,
        Term::Lit(_) | Term::Error => true,
        Term::Loc(_, inner) => walk(inner, v, arity, tail, seen),

        // A call: if its head is `v`, it has to be saturated and in tail
        // position, and its arguments must not mention `v` at all.
        Term::App(..) => {
            let (head, args) = spine(t);
            if let Term::Var(x) = head {
                if *x == v {
                    if !tail || args.len() != arity {
                        return false;
                    }
                    *seen += 1;
                    return args.iter().all(|a| walk(a, v, arity, false, seen));
                }
            }
            walk_children(t, v, arity, seen)
        }

        // Tail position travels through these.
        Term::If(c, a, b) => {
            walk(c, v, arity, false, seen)
                && walk(a, v, arity, tail, seen)
                && walk(b, v, arity, tail, seen)
        }
        Term::Let(_, _, rhs, body) => {
            walk(rhs, v, arity, false, seen) && walk(body, v, arity, tail, seen)
        }
        Term::Case(scrut, arms, _) => {
            walk(scrut, v, arity, false, seen)
                && arms.iter().all(|(_, guard, body)| {
                    guard
                        .as_ref()
                        .is_none_or(|g| walk(g, v, arity, false, seen))
                        && walk(body, v, arity, tail, seen)
                })
        }

        // A lambda's body is not this body's tail position: the call would
        // happen when the lambda is entered, which may be anywhere.
        Term::Lam(_, _, body) => walk(body, v, arity, false, seen),

        // Everything else: `v` may not appear at all.
        _ => walk_children(t, v, arity, seen),
    }
}

/// No child of `t` is in tail position, and none may mention `v`.
fn walk_children(t: &Term, v: Var, arity: usize, seen: &mut usize) -> bool {
    let mut ok = true;
    each_child(t, &mut |c| ok = ok && walk(c, v, arity, false, seen));
    ok
}

fn each_child(t: &Term, f: &mut impl FnMut(&Term)) {
    match t {
        Term::Var(_) | Term::Lit(_) | Term::Error => {}
        Term::Loc(_, b)
        | Term::TyLam(_, b)
        | Term::TyApp(b, _)
        | Term::Lam(_, _, b)
        | Term::Proj(b, _)
        | Term::Sel(b, _, _)
        | Term::Perform(_, _, b, _) => f(b),
        Term::App(a, b) | Term::Extend(a, _, b) => {
            f(a);
            f(b);
        }
        Term::Let(_, _, a, b) => {
            f(a);
            f(b);
        }
        Term::LetRec(binds, b) => {
            binds.iter().for_each(|(_, _, t)| f(t));
            f(b);
        }
        Term::Join { rhs, body, .. } => {
            f(rhs);
            f(body);
        }
        Term::Jump(_, args, _) => args.iter().for_each(f),
        Term::If(a, b, c) => {
            f(a);
            f(b);
            f(c);
        }
        Term::Tuple(xs) | Term::Array(xs, _) | Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
            xs.iter().for_each(f)
        }
        Term::Record(fs) => fs.iter().for_each(|(_, x)| f(x)),
        Term::Case(s, arms, _) => {
            f(s);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    f(g);
                }
                f(b);
            }
        }
        Term::Handle {
            body, clauses, ret, ..
        } => {
            f(body);
            clauses.iter().for_each(|c| f(&c.body));
            if let Some((_, _, r)) = ret {
                f(r);
            }
        }
    }
}

/// A call's head and its arguments, left to right.
fn spine(t: &Term) -> (&Term, Vec<&Term>) {
    let mut args = Vec::new();
    let mut here = t;
    loop {
        match here {
            Term::Loc(_, inner) => here = inner,
            Term::App(f, a) => {
                args.push(&**a);
                here = f;
            }
            _ => break,
        }
    }
    args.reverse();
    (here, args)
}

/// Every saturated call to `v` becomes a [`Term::Jump`].
///
/// The arity is not a formality. [`rewrite::term`] rebuilds children before it
/// rebuilds their parent, so for `f a b` it reaches the bare `f` first, then
/// `f a`, then `f a b` -- and the spine of a bare `Var` is that `Var` with no
/// arguments. Converting on the head alone turns `f` into a jump with nothing
/// passed and leaves the application wrapped around it, which lowers to a
/// block entered with the wrong number of values. Only the node with exactly
/// `arity` arguments is the call.
fn rewrite_calls(t: &Term, v: Var, arity: usize, ty: &Ty) -> Term {
    rewrite::term(
        t,
        &mut |x| {
            let (head, args) = spine(&x);
            if matches!(head, Term::Var(h) if *h == v) && args.len() == arity {
                return Term::Jump(v, args.into_iter().cloned().collect(), ty.clone());
            }
            x
        },
        &mut |q| q,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use meadow_hir::VarId;

    fn v(n: u32) -> Var {
        VarId(n)
    }

    fn int(n: i64) -> Term {
        Term::Lit(Lit::Int(n))
    }

    fn call(f: Var, args: Vec<Term>) -> Term {
        args.into_iter()
            .fold(Term::Var(f), |g, a| Term::App(Arc::new(g), Arc::new(a)))
    }

    /// `let f = \x -> x in <body>`, as a `Let` of a one-parameter lambda.
    fn let_f(body: Term) -> Term {
        let f = v(1);
        let x = v(2);
        Term::Let(
            f,
            Poly::mono(InferType::Fun(
                vec![unknown()],
                Box::new(unknown()),
                Box::new(unknown()),
            )),
            Arc::new(Term::Lam(x, unknown(), Arc::new(Term::Var(x)))),
            Arc::new(body),
        )
    }

    fn is_join(t: &Term) -> bool {
        matches!(t, Term::Join { .. })
    }

    fn jumps(t: &Term) -> usize {
        let mut n = 0;
        fn go(t: &Term, n: &mut usize) {
            if matches!(t, Term::Jump(..)) {
                *n += 1;
            }
            each_child(t, &mut |c| go(c, n));
        }
        go(t, &mut n);
        n
    }

    /// The plain case: one saturated call, in tail position.
    #[test]
    fn a_function_only_tail_called_becomes_a_join_point() {
        let got = term(&let_f(call(v(1), vec![int(1)])));
        assert!(is_join(&got), "{got:?}");
        assert_eq!(jumps(&got), 1);
    }

    /// Several branches sharing one continuation -- the shape the whole thing
    /// exists for.
    #[test]
    fn both_branches_jump_to_the_same_place() {
        let body = Term::If(
            Arc::new(Term::Lit(Lit::Bool(true))),
            Arc::new(call(v(1), vec![int(1)])),
            Arc::new(call(v(1), vec![int(2)])),
        );
        let got = term(&let_f(body));
        assert!(is_join(&got), "{got:?}");
        assert_eq!(jumps(&got), 2, "one per branch, and the block is shared");
    }

    /// Not in tail position: the addition is still waiting when the call
    /// answers, so it is a call and not a jump.
    #[test]
    fn a_call_with_work_after_it_is_not_a_jump() {
        let body = Term::Prim(Prim::Add, vec![call(v(1), vec![int(1)]), int(2)], unknown());
        let got = term(&let_f(body));
        assert!(!is_join(&got), "{got:?}");
    }

    /// Bare, so something could hold it: it has to stay a value.
    #[test]
    fn a_function_used_as_a_value_is_not_a_join_point() {
        let got = term(&let_f(Term::Tuple(vec![Term::Var(v(1)), int(1)])));
        assert!(!is_join(&got), "{got:?}");
    }

    /// Partially applied, so the arity does not match.
    #[test]
    fn an_unsaturated_call_is_not_a_jump() {
        let f = v(1);
        let x = v(2);
        let y = v(3);
        let two = Term::Let(
            f,
            Poly::mono(unknown()),
            Arc::new(Term::Lam(
                x,
                unknown(),
                Arc::new(Term::Lam(y, unknown(), Arc::new(Term::Var(x)))),
            )),
            Arc::new(call(f, vec![int(1)])),
        );
        let got = term(&two);
        assert!(!is_join(&got), "{got:?}");
    }

    /// Inside a lambda the call happens whenever the lambda is entered, which
    /// is not this body's tail position.
    #[test]
    fn a_call_under_a_lambda_is_not_a_jump() {
        let body = Term::Lam(v(9), unknown(), Arc::new(call(v(1), vec![int(1)])));
        let got = term(&let_f(body));
        assert!(!is_join(&got), "{got:?}");
    }

    /// A binding nothing mentions is left for `prune`.
    #[test]
    fn a_function_nobody_calls_is_left_alone() {
        let got = term(&let_f(int(7)));
        assert!(!is_join(&got), "{got:?}");
    }

    /// The right-hand side of a `let` is not tail position; its body is.
    #[test]
    fn tail_position_travels_through_a_let_body_only() {
        let inner = Term::Let(
            v(8),
            Poly::mono(unknown()),
            Arc::new(call(v(1), vec![int(1)])),
            Arc::new(int(0)),
        );
        assert!(!is_join(&term(&let_f(inner))), "in the right-hand side");

        let outer = Term::Let(
            v(8),
            Poly::mono(unknown()),
            Arc::new(int(0)),
            Arc::new(call(v(1), vec![int(1)])),
        );
        assert!(is_join(&term(&let_f(outer))), "in the body");
    }

    /// Nested: the inner one is found even though it sits inside the outer
    /// one's body, because the traversal rebuilds children first.
    #[test]
    fn a_join_point_inside_another_is_found_too() {
        let inner = let_f(call(v(1), vec![int(1)]));
        let g = v(4);
        let z = v(5);
        let outer = Term::Let(
            g,
            Poly::mono(unknown()),
            Arc::new(Term::Lam(z, unknown(), Arc::new(inner))),
            Arc::new(call(g, vec![int(0)])),
        );
        let got = term(&outer);
        assert!(is_join(&got), "the outer one");
        let mut joins = 0;
        fn count(t: &Term, n: &mut usize) {
            if matches!(t, Term::Join { .. }) {
                *n += 1;
            }
            each_child(t, &mut |c| count(c, n));
        }
        count(&got, &mut joins);
        assert_eq!(joins, 2, "both");
    }
}
