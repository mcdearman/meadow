//! **The simplifier**: the rewrites that are each too small to matter and
//! together decide how much work the back end is given.
//!
//! Every one of these fires on code nobody wrote. A person does not write
//! `case (case xs of …) of …`, or multiply by eight, or take apart a
//! constructor they built one line above. Desugaring, inlining and
//! specialization write all three constantly -- `if` over a condition that was
//! itself an `if`, an index computed as `i * width`, a `Maybe` made and matched
//! in the same expression -- and each one left in place is a branch, a multiply
//! or an allocation the machine performs at run time for no reason.
//!
//! # What it does
//!
//! **Case of known constructor.** `case C a b of … C x y -> e …` is `e` with
//! `x` and `y` bound to `a` and `b`. The scrutinee is not built, the tag is not
//! tested, and the fields are not read back out of a heap object. This is what
//! makes a `Maybe` returned by an inlined function cost nothing.
//!
//! **Case of case.** `case (case s of p -> e) of alts` becomes
//!
//! ```text
//! join j (x : t) = case x of alts in
//!   case s of p -> jump j e
//! ```
//!
//! -- the outer alternatives named once and jumped to from each inner arm,
//! rather than copied into every arm (which can square the program) or closed
//! over (which allocates). It pays because once the outer `case` is *inside*,
//! its scrutinee is often a constructor built right there, and the first
//! rewrite fires. A chain of `&&` over comparisons collapses to a chain of
//! branches this way, which is the case that made it worth writing.
//!
//! **Strength reduction.** Replacing an operation by a cheaper one that
//! computes the same thing: `x * 8` is `x << 3`; on an unsigned type `x / 8` is
//! `x >> 3` and `x % 8` is `x & 7`. A 64-bit multiply is three to five cycles
//! against one for a shift, and a division twenty to forty against one -- the
//! largest per-instruction win available anywhere, for a table lookup at
//! compile time. Signed division is *not* a shift (`-1 / 2` is `0`, `-1 >> 1`
//! is `-1`) and is left alone. The identities that hold at every width --
//! `x * 1`, `x + 0`, `x * 0` -- are folded here too, because the same rewrite
//! that turns `x * 1` into `x` is what stops `* 1` reappearing from an inlined
//! generic.
//!
//! # Types are not erased here
//!
//! Core is still typed at this point, and stays typed until lowering: a name's
//! representation comes from its type. So every term this pass builds carries
//! the type the term it replaced had, and where that type cannot be recovered
//! exactly the rewrite is **declined** rather than guessed. That is why
//! [`of_case`] gives up on an inner `if` whose branches say nothing about their
//! type: a join point whose parameter is typed wrongly is a value in the wrong
//! representation, which is not a slow program but an incorrect one.
//!
//! # Where it runs
//!
//! After [`crate::joins`], because case-of-case needs [`Term::Join`] to put the
//! outer context in, and before [`crate::globals`], because a mention of a
//! global becomes a jump there and a jump is not a term this can look into. It
//! repeats until nothing changes: each rewrite exposes the others, and the
//! cascade is the whole point.

use crate::*;

/// Where this pass's own names start: clear of units, synthetic definitions,
/// specialized copies and [`crate::globals`].
pub const SIMPLIFY_BASE: u32 = 0x7E00_0000;

/// How many times the sweep is repeated before giving up on a fixed point. The
/// bound is not an optimization budget -- nothing in the standard library needs
/// a third round -- it is there so that a pair of rewrites that undid each
/// other could not hang the compiler.
const ROUNDS: usize = 4;

/// Simplify every definition in `p`.
pub fn program(p: &Program) -> Program {
    let mut out = p.clone();
    let mut fresh = Fresh::above(p);
    for d in &mut out.defs {
        d.term = term(&d.term, &mut fresh);
    }
    out
}

/// Simplify one term until it stops changing.
pub fn term(t: &Term, fresh: &mut Fresh) -> Term {
    let mut out = t.clone();
    for _ in 0..ROUNDS {
        let next = rewrite::term(&out, &mut |t| simplify(t, fresh), &mut |p: Pat| p);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

/// A source of names nothing else uses.
pub struct Fresh(u32);

impl Fresh {
    pub fn new() -> Fresh {
        Fresh(SIMPLIFY_BASE)
    }

    /// Names clear of every one `p` already binds or mentions, as well as of
    /// the other passes' ranges.
    ///
    /// The pass runs more than once on a program: [`crate::inline`] runs it
    /// after each of its rounds, and the pipeline again after `joins`. Names
    /// dealt out from [`SIMPLIFY_BASE`] every time would be dealt out twice,
    /// and a binder bound twice is a name lowering finds out of scope, or
    /// without a representation, wherever the second binding is not the one
    /// it was looking at.
    pub fn above(p: &Program) -> Fresh {
        Fresh(SIMPLIFY_BASE.max(max_var(p) + 1))
    }

    fn var(&mut self) -> Var {
        let v = hir::VarId(self.0);
        self.0 += 1;
        v
    }
}

/// The largest variable number `p` binds or mentions anywhere.
pub fn max_var(p: &Program) -> u32 {
    let top = std::cell::Cell::new(0u32);
    let note = |v: &Var| top.set(top.get().max(v.0));
    for d in &p.defs {
        note(&d.var);
        rewrite::term(
            &d.term,
            &mut |t| {
                match &t {
                    Term::Var(v) | Term::Lam(v, ..) | Term::Let(v, ..) | Term::Jump(v, ..) => {
                        note(v)
                    }
                    Term::LetRec(binds, _) => binds.iter().for_each(|(v, ..)| note(v)),
                    Term::Join { var, params, .. } => {
                        note(var);
                        params.iter().for_each(|(v, _)| note(v));
                    }
                    Term::Handle { clauses, ret, .. } => {
                        for c in clauses {
                            note(&c.param);
                            note(&c.resume);
                        }
                        if let Some((v, ..)) = ret {
                            note(v);
                        }
                    }
                    _ => {}
                }
                t
            },
            &mut |pat| {
                if let Pat::Var(v, _) | Pat::As(v, ..) = &pat {
                    note(v);
                }
                pat
            },
        );
    }
    top.get()
}

impl Default for Fresh {
    fn default() -> Fresh {
        Fresh::new()
    }
}

/// One node, already rebuilt from simplified children.
fn simplify(t: Term, fresh: &mut Fresh) -> Term {
    match t {
        Term::Case(scrut, arms, ty) => {
            if let Some(t) = known_ctor(&scrut, &arms) {
                return t;
            }
            if let Some(t) = of_case(&scrut, &arms, &ty, fresh) {
                return t;
            }
            Term::Case(scrut, arms, ty)
        }
        Term::If(c, a, b) => match peel(&c) {
            Term::Lit(Lit::Bool(true)) => (*a).clone(),
            Term::Lit(Lit::Bool(false)) => (*b).clone(),
            _ => Term::If(c, a, b),
        },
        Term::Join {
            var,
            params,
            ty,
            rhs,
            body,
        } => join(var, params, ty, rhs, body),
        Term::Let(x, poly, rhs, body) => let_(x, poly, rhs, body),
        Term::Prim(p, args, ty) => prim(p, args, ty),
        // A lambda applied where it stands is a `let`: no closure to build,
        // and the argument goes wherever `let_` would put it. Lowering would
        // otherwise allocate the closure and enter it.
        Term::App(f, a) => match peel(&f) {
            Term::Lam(v, ty, body) => let_(*v, Poly::mono(ty.clone()), a, body.clone()),
            _ => Term::App(f, a),
        },
        t => t,
    }
}

/// `Loc` says where a term was written and nothing about what it is. Every
/// test here looks through one; every rewrite that keeps a term keeps it.
fn peel(t: &Term) -> &Term {
    match t {
        Term::Loc(_, inner) => peel(inner),
        t => t,
    }
}

// --- case of known constructor ---------------------------------------------

/// The bindings a pattern makes when it matches: the variable, the type its
/// annotation gives it, and the term it is bound to.
type Binds = Vec<(Var, Ty, Term)>;

/// `case C a b of … C x y -> e …`: the arm that matches, with its pattern's
/// variables `let`-bound to the fields.
///
/// An arm is only taken when its pattern *cannot* fail and every arm before it
/// definitely does not match. A guard decides at run time, so the first arm
/// with one stops the search rather than being chosen.
fn known_ctor(scrut: &Term, arms: &[(Pat, Option<Term>, Term)]) -> Option<Term> {
    let value = peel(scrut);
    // Only a value, and only one built out of values. A wildcard arm discards
    // the scrutinee, and discarding `f x` would be dropping the call: `case f x
    // of _ -> 1` has to keep running `f x`. Nothing below has to think about
    // that, because nothing below is reached unless there is no work to lose.
    if !is_value(value) {
        return None;
    }
    for (pat, guard, body) in arms {
        match matches(pat, value)? {
            // Definitely does not match: not taken, whatever its guard says.
            None => continue,
            Some(binds) => {
                if guard.is_some() {
                    return None;
                }
                let body = body.clone();
                return Some(binds.into_iter().rev().fold(body, |acc, (v, ty, t)| {
                    Term::Let(v, Poly::mono(ty), Arc::new(t), Arc::new(acc))
                }));
            }
        }
    }
    None
}

/// Whether `pat` matches the value `t`, and with what bindings.
///
/// `None` is "cannot tell" -- the value is not built here, so nothing may be
/// decided about it. `Some(None)` is a definite non-match and
/// `Some(Some(binds))` a definite match. Only a constructor application, a
/// tuple, an array literal and a literal are values in this sense; anything
/// else is unknown, and being unknown is the ordinary case.
fn matches(pat: &Pat, value: &Term) -> Option<Option<Binds>> {
    match (pat, value) {
        (Pat::Wild, _) => Some(Some(Vec::new())),
        (Pat::Var(v, ty), _) => Some(Some(vec![(*v, ty.clone(), value.clone())])),
        (Pat::As(v, ty, inner), _) => match matches(inner, value)? {
            None => Some(None),
            Some(mut binds) => {
                binds.push((*v, ty.clone(), value.clone()));
                Some(Some(binds))
            }
        },
        (Pat::Lit(a), Term::Lit(b)) => Some(same_lit(a, b).map(|yes| yes.then(Vec::new))?),
        (Pat::Ctor(name, pats), Term::Ctor(built, _, args)) => {
            if name != built {
                return Some(None);
            }
            // A constructor matched at the wrong arity is a program that did
            // not type-check; leave it for the checker to complain about.
            (pats.len() == args.len()).then(|| all(pats, args))?
        }
        (Pat::Tuple(pats), Term::Tuple(args)) if pats.len() == args.len() => all(pats, args),
        (Pat::Array(pats), Term::Array(args, _)) => {
            if pats.len() != args.len() {
                return Some(None);
            }
            all(pats, args)
        }
        _ => None,
    }
}

/// Whether `t` is a value: something already computed, that can be discarded
/// or duplicated without changing what the program does or how often it does
/// it. Building a constructor allocates, but *not* building one is the saving
/// this pass is after.
fn is_value(t: &Term) -> bool {
    match t {
        Term::Loc(_, inner) => is_value(inner),
        Term::Lit(_) | Term::Var(_) | Term::Lam(..) => true,
        Term::Ctor(_, _, args) | Term::Tuple(args) | Term::Array(args, _) => {
            args.iter().all(is_value)
        }
        Term::Record(fields) => fields.iter().all(|(_, t)| is_value(t)),
        _ => false,
    }
}

fn all(pats: &[Pat], args: &[Term]) -> Option<Option<Binds>> {
    let mut binds = Vec::new();
    for (p, a) in pats.iter().zip(args) {
        match matches(p, peel(a))? {
            None => return Some(None),
            Some(more) => binds.extend(more),
        }
    }
    Some(Some(binds))
}

/// Whether two literals are the same value, or `None` for a comparison this
/// pass declines to make. Deliberately not `PartialEq`: floats are not decided
/// here (`0.0` and `-0.0` are equal as numbers and distinct as bits, and
/// nothing good comes of choosing), and neither is a literal whose type is
/// still a variable, which [`crate::specialize`] may yet make either.
fn same_lit(a: &Lit, b: &Lit) -> Option<bool> {
    match (a, b) {
        (Lit::Int(x), Lit::Int(y)) | (Lit::BigInt(x), Lit::BigInt(y)) => Some(x == y),
        (Lit::Word(wa, x), Lit::Word(wb, y)) => Some(wa == wb && x == y),
        (Lit::Str(x), Lit::Str(y)) | (Lit::Sym(x), Lit::Sym(y)) => Some(x == y),
        (Lit::Char(x), Lit::Char(y)) => Some(x == y),
        (Lit::Bool(x), Lit::Bool(y)) => Some(x == y),
        (Lit::Unit, Lit::Unit) => Some(true),
        _ => None,
    }
}

// --- case of case ----------------------------------------------------------

/// `case (case s of p -> e) of alts` -- the outer alternatives named once and
/// jumped to, rather than copied into each inner arm.
fn of_case(
    scrut: &Term,
    arms: &[(Pat, Option<Term>, Term)],
    ty: &Ty,
    fresh: &mut Fresh,
) -> Option<Term> {
    let inner = peel(scrut);
    if !branches(inner) {
        return None;
    }
    // The join's parameter is the value the inner term answers with, which is
    // the value the outer `case` scrutinizes. If its type cannot be read off
    // exactly, decline: see the module docs on why this is not guessed.
    let scrut_ty = produces(inner)?;
    if is_unknown(&scrut_ty) {
        return None;
    }
    let (j, x) = (fresh.var(), fresh.var());
    let rhs = Term::Case(Arc::new(Term::Var(x)), arms.to_vec(), ty.clone());
    let body = tails(inner, ty, &mut |tail| Term::Jump(j, vec![tail], ty.clone()));
    Some(Term::Join {
        var: j,
        params: vec![(x, scrut_ty)],
        ty: ty.clone(),
        rhs: Arc::new(rhs),
        body: Arc::new(body),
    })
}

/// Whether pushing a context into this term's tails is worth doing: it answers
/// from more than one place, so the context would otherwise be copied into each
/// or closed over.
fn branches(t: &Term) -> bool {
    match t {
        Term::Loc(_, inner) => branches(inner),
        Term::If(..) => true,
        Term::Case(_, arms, _) => arms.len() > 1,
        // A `let` in front of a branch is still a branch, and so is a join
        // whose body branches. Both come out of desugaring constantly.
        Term::Let(_, _, _, body) | Term::Join { body, .. } => branches(body),
        _ => false,
    }
}

/// The type `t` produces, where core wrote it down. `None` means it did not,
/// which for this pass means "do not rewrite".
fn produces(t: &Term) -> Option<Ty> {
    match t {
        Term::Loc(_, inner) => produces(inner),
        Term::Case(_, _, ty) | Term::Prim(_, _, ty) | Term::Jump(_, _, ty) => Some(ty.clone()),
        Term::Ctor(_, ty, _) | Term::Sel(_, _, ty) | Term::Perform(_, _, _, ty) => Some(ty.clone()),
        Term::Join { ty, .. } => Some(ty.clone()),
        Term::Handle { ty, .. } => Some(ty.clone()),
        // A `let`'s answer is its body's, and an `if`'s is either branch's --
        // whichever of the two says so.
        Term::Let(_, _, _, body) => produces(body),
        Term::If(_, a, b) => produces(a).or_else(|| produces(b)),
        _ => None,
    }
}

/// Rebuild `t` with `f` applied at each of its tail positions, every rebuilt
/// node re-typed to `ty` -- which is what `f` produces, at every tail, by
/// construction.
///
/// Tail position is where the answer of `t` is the answer of the subterm:
/// through `Loc`, both branches of an `if`, every arm of a `case`, the body of
/// a `let`, and both the body *and* the right-hand side of a `join` (its
/// right-hand side is entered from the body's tails, so it answers where the
/// body does). The same definition [`crate::joins`] uses, for the same reason.
fn tails(t: &Term, ty: &Ty, f: &mut dyn FnMut(Term) -> Term) -> Term {
    match t {
        Term::Loc(l, inner) => Term::Loc(*l, Arc::new(tails(inner, ty, f))),
        Term::If(c, a, b) => Term::If(
            c.clone(),
            Arc::new(tails(a, ty, f)),
            Arc::new(tails(b, ty, f)),
        ),
        Term::Case(s, arms, _) => {
            let arms = arms
                .iter()
                .map(|(p, g, body)| (p.clone(), g.clone(), tails(body, ty, f)))
                .collect();
            Term::Case(s.clone(), arms, ty.clone())
        }
        Term::Let(v, poly, rhs, body) => {
            Term::Let(*v, poly.clone(), rhs.clone(), Arc::new(tails(body, ty, f)))
        }
        Term::Join {
            var,
            params,
            rhs,
            body,
            ..
        } => Term::Join {
            var: *var,
            params: params.clone(),
            ty: ty.clone(),
            rhs: Arc::new(tails(rhs, ty, f)),
            body: Arc::new(tails(body, ty, f)),
        },
        t => f(t.clone()),
    }
}

// --- let ------------------------------------------------------------------

/// `let x = v in e`, where `v` is a value: dropped if `x` is unused, and
/// substituted if that cannot cost anything.
///
/// Without this the two rewrites above stop one step short. [`known_ctor`]
/// leaves `let x = 7 in x`, and [`enter`] leaves `let x = A in case x of …`:
/// the constructor is known, and the `case` on it cannot see that because a
/// name is in between. Substituting removes the name and the next round
/// removes the `case`.
///
/// What it will not do is move work. A value under a lambda is built every time
/// the lambda runs, so a constructor is only substituted where it is used
/// exactly once and not inside one; a literal or a variable is free to copy
/// anywhere.
fn let_(x: Var, poly: Poly, rhs: Arc<Term>, body: Arc<Term>) -> Term {
    if !is_value(&rhs) {
        return Term::Let(x, poly, rhs, body);
    }
    let uses = count_var(&body, x);
    if uses == 0 {
        if !closed(&rhs) {
            // Kept, for the reason below. Substituting would drop it too --
            // there is nothing to substitute *into* -- so this has to come
            // first.
            return Term::Let(x, poly, rhs, body);
        }
        // A value nothing reads: not built at all.
        //
        // Only one that mentions nothing. Dropping a binding is the one
        // rewrite here that can shrink a *closure's* free variables, and
        // Meadow lets a program see that: `threadSpawn` refuses a function
        // that captures a `Ref`, so turning `\() -> let y = r in 0` into
        // `\() -> 0` turns a program that is refused into one that runs. The
        // differential tests against the CEK machine are where that showed up,
        // and they were right to: the engines have to agree about it, and the
        // CEK does not run this pass. A term with no free variables cannot
        // change any capture set, so that is where the rule stops.
        return (*body).clone();
    }
    let free_to_copy = trivial(&rhs) || (uses == 1 && !under_lambda(&body, x));
    if free_to_copy {
        return substitute(&body, x, &rhs);
    }
    Term::Let(x, poly, rhs, body)
}

/// Whether `t` mentions nothing -- so removing it cannot change what any
/// enclosing lambda closes over.
fn closed(t: &Term) -> bool {
    let mut free = std::collections::HashSet::new();
    free_vars_into(t, &mut free);
    free.is_empty()
}

/// A value that costs nothing to build, so copying it costs nothing either.
fn trivial(t: &Term) -> bool {
    matches!(peel(t), Term::Lit(_) | Term::Var(_))
}

fn count_var(t: &Term, x: Var) -> usize {
    let mut n = 0;
    let mut count = |t: Term| {
        if matches!(&t, Term::Var(v) if *v == x) {
            n += 1;
        }
        t
    };
    rewrite::term(t, &mut count, &mut |p| p);
    n
}

/// Whether `x` is read inside a lambda in `t` -- where a use is not one use but
/// one per call.
fn under_lambda(t: &Term, x: Var) -> bool {
    let mut found = false;
    let mut look = |t: Term| {
        if let Term::Lam(_, _, body) = &t {
            let mut free = std::collections::HashSet::new();
            free_vars_into(body, &mut free);
            found |= free.contains(&x);
        }
        t
    };
    rewrite::term(t, &mut look, &mut |p| p);
    found
}

/// `t` with every mention of `x` replaced by `to`. No renaming: core binds
/// every name once, so nothing `to` mentions can be captured on the way in.
fn substitute(t: &Term, x: Var, to: &Term) -> Term {
    let mut go = |t: Term| match &t {
        Term::Var(v) if *v == x => to.clone(),
        _ => t,
    };
    rewrite::term(t, &mut go, &mut |p| p)
}

// --- entering a join point -------------------------------------------------

/// A join point, with the jumps to it that are worth entering *here* entered
/// here -- and dropped entirely if that was all of them.
///
/// This is what makes case-of-case pay rather than merely rearrange. Pushing
/// the outer alternatives into a join leaves each branch jumping to it with the
/// value that branch answers, and where that value is a constructor built right
/// there, entering the join turns the jump into a `case` on a known constructor
/// -- which [`known_ctor`] then removes. `case (if c then A else B) of {A -> 1;
/// B -> 2}` collapses to `if c then 1 else 2`: no allocation, no tag test, one
/// branch.
///
/// The guard against copying the join everywhere is that it is only entered
/// where it *decides* something, or where there is nowhere else it is entered
/// from. A join that stays is a label and a jump, which is what it was for.
fn join(var: Var, params: Vec<(Var, Ty)>, ty: Ty, rhs: Arc<Term>, body: Arc<Term>) -> Term {
    // A join that jumps to itself is a loop (see `crate::inline`'s loops),
    // and entering it anywhere would unroll it once and leave the jump inside
    // pointing at nothing. It stays exactly as it is.
    if jumps_to(&rhs, var) {
        return Term::Join {
            var,
            params,
            ty,
            rhs,
            body,
        };
    }
    let body = enter(&body, var, &params, &rhs);
    if !jumps_to(&body, var) {
        return body;
    }
    Term::Join {
        var,
        params,
        ty,
        rhs,
        body: Arc::new(body),
    }
}

/// `body` with each worthwhile `jump j a…` replaced by `j`'s right-hand side,
/// its parameters `let`-bound to the arguments.
fn enter(body: &Term, j: Var, params: &[(Var, Ty)], rhs: &Term) -> Term {
    let once = count_jumps(body, j) == 1;
    replace_jumps(body, j, &mut |args| {
        if args.len() != params.len() {
            return None;
        }
        if !once && !decides(rhs, params, &args) {
            return None;
        }
        Some(
            params
                .iter()
                .zip(args)
                .rev()
                .fold(rhs.clone(), |acc, ((v, ty), arg)| {
                    Term::Let(*v, Poly::mono(ty.clone()), Arc::new(arg), Arc::new(acc))
                }),
        )
    })
}

/// Whether entering `rhs` with these arguments settles a `case` that would
/// otherwise be decided at run time -- the only reason to enter a join point
/// that is jumped to from more than one place.
fn decides(rhs: &Term, params: &[(Var, Ty)], args: &[Term]) -> bool {
    let Term::Case(scrut, arms, _) = peel(rhs) else {
        return false;
    };
    let Term::Var(x) = peel(scrut) else {
        return false;
    };
    params
        .iter()
        .zip(args)
        .find(|((v, _), _)| v == x)
        .is_some_and(|(_, arg)| known_ctor(arg, arms).is_some())
}

fn count_jumps(t: &Term, j: Var) -> usize {
    let mut n = 0;
    let mut count = |t: Term| {
        if matches!(&t, Term::Jump(v, _, _) if *v == j) {
            n += 1;
        }
        t
    };
    rewrite::term(t, &mut count, &mut |p| p);
    n
}

fn jumps_to(t: &Term, j: Var) -> bool {
    count_jumps(t, j) > 0
}

/// Rebuild `t` with each `jump j a…` that `f` answers for replaced by what it
/// answers. A jump `f` declines is left as it is.
fn replace_jumps(t: &Term, j: Var, f: &mut dyn FnMut(Vec<Term>) -> Option<Term>) -> Term {
    let mut go = |t: Term| match &t {
        Term::Jump(v, args, _) if *v == j => f(args.clone()).unwrap_or(t),
        _ => t,
    };
    rewrite::term(t, &mut go, &mut |p| p)
}

// --- strength reduction ----------------------------------------------------

/// Strength reduction and the algebraic identities, on an application whose
/// arguments have already been simplified.
fn prim(p: Prim, args: Vec<Term>, ty: Ty) -> Term {
    let [a, b] = &args[..] else {
        return Term::Prim(p, args, ty);
    };
    let k = int_of(peel(b));
    let reduced =
        match (p.untyped(), k) {
            // The operation is the identity on `a`.
            (Prim::Mul | Prim::Div, Some(1)) | (Prim::Add | Prim::Sub, Some(0)) => Some(a.clone()),
            // `x * 0` is `0`, and `b` is already a literal zero of the right type.
            // Not `x - x` or `x / x`: those need `x` to be pure, and a purity
            // analysis is not worth having for two rewrites.
            (Prim::Mul, Some(0)) => Some(b.clone()),
            // `x * 2^n` is `x << n` at every width and either sign: two's
            // complement multiplication and shifting wrap the same way.
            (Prim::Mul, Some(k)) => log2(k)
                .map(|n| Term::Prim(Prim::Shl, vec![a.clone(), like(b, n as i64)], ty.clone())),
            // A shift and a mask only for an unsigned type: `-1 / 2` is `0` where
            // `-1 >> 1` is `-1`, and `-1 % 2` is `-1` where `-1 & 1` is `1`.
            (Prim::Div, Some(k)) if unsigned(&ty) => log2(k)
                .map(|n| Term::Prim(Prim::Shr, vec![a.clone(), like(b, n as i64)], ty.clone())),
            (Prim::Mod, Some(k)) if unsigned(&ty) => log2(k)
                .map(|_| Term::Prim(Prim::BitAnd, vec![a.clone(), like(b, k - 1)], ty.clone())),
            _ => None,
        };
    reduced.unwrap_or(Term::Prim(p, args, ty))
}

/// The value of an integer literal, whatever width it is written at.
fn int_of(t: &Term) -> Option<i64> {
    match t {
        Term::Lit(Lit::Int(k) | Lit::BigInt(k) | Lit::AnyInt(k, _)) => Some(*k),
        Term::Lit(Lit::Word(_, w)) => i64::try_from(*w).ok(),
        _ => None,
    }
}

/// `log2(k)` when `k` is a power of two greater than one -- one is the identity
/// rule above -- and small enough for the shift to be defined at 64 bits.
fn log2(k: i64) -> Option<u32> {
    (k > 1 && k < (1 << 62) && k & (k - 1) == 0).then(|| k.trailing_zeros())
}

/// `n` as a literal of the same shape as the operand it replaces, so that a
/// sized operand keeps its width and an `AnyInt` keeps its type variable.
fn like(operand: &Term, n: i64) -> Term {
    match peel(operand) {
        Term::Lit(Lit::Word(w, _)) => Term::Lit(Lit::Word(*w, n as u64)),
        Term::Lit(Lit::AnyInt(_, v)) => Term::Lit(Lit::AnyInt(n, *v)),
        Term::Lit(Lit::BigInt(_)) => Term::Lit(Lit::BigInt(n)),
        _ => Term::Lit(Lit::Int(n)),
    }
}

/// Whether `ty` is one of the unsigned integer types, for which division and
/// remainder by a power of two are a shift and a mask.
fn unsigned(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Con(n, args)
            if args.is_empty() && matches!(&**n, "UInt8" | "UInt16" | "UInt32" | "UInt64")
    )
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

    fn int(n: i64) -> Term {
        Term::Lit(Lit::Int(n))
    }

    fn simplified(t: &Term) -> Term {
        term(t, &mut Fresh::new())
    }

    /// The rewrite the whole pass exists for: a constructor built and taken
    /// apart in the same expression never reaches the heap.
    #[test]
    fn a_constructor_matched_where_it_is_built_is_not_built() {
        // case Just 7 of { Nothing -> 0; Just x -> x }
        let x = v(1);
        let t = Term::Case(
            Arc::new(Term::Ctor(
                InternedString::from("Just"),
                con("Maybe"),
                vec![int(7)],
            )),
            vec![
                (
                    Pat::Ctor(InternedString::from("Nothing"), vec![]),
                    None,
                    int(0),
                ),
                (
                    Pat::Ctor(InternedString::from("Just"), vec![Pat::Var(x, con("Int"))]),
                    None,
                    Term::Var(x),
                ),
            ],
            con("Int"),
        );
        // Not `let x = 7 in x`: the binding goes too, so what is left is the
        // field itself and no trace that a `Maybe` was ever involved.
        assert_eq!(simplified(&t), int(7));
    }

    /// An arm that cannot match is passed over rather than stopping the search.
    #[test]
    fn a_definite_mismatch_is_not_what_stops_it() {
        let t = Term::Case(
            Arc::new(Term::Lit(Lit::Bool(true))),
            vec![
                (Pat::Lit(Lit::Bool(false)), None, int(0)),
                (Pat::Lit(Lit::Bool(true)), None, int(1)),
            ],
            con("Int"),
        );
        assert_eq!(simplified(&t), int(1));
    }

    /// A guard decides at run time, so an arm with one is never chosen here --
    /// even where its pattern matches for certain.
    #[test]
    fn a_guard_stops_the_search_rather_than_being_taken() {
        let t = Term::Case(
            Arc::new(Term::Lit(Lit::Bool(true))),
            vec![
                (Pat::Lit(Lit::Bool(true)), Some(Term::Var(v(9))), int(0)),
                (Pat::Wild, None, int(1)),
            ],
            con("Int"),
        );
        assert_eq!(simplified(&t), t, "nothing may be decided");
    }

    /// Floats are not compared here at all: `0.0` and `-0.0` are one number and
    /// two bit patterns, and a pass that picked one would be picking wrongly
    /// for somebody.
    #[test]
    fn a_float_pattern_is_left_to_the_machine() {
        let t = Term::Case(
            Arc::new(Term::Lit(Lit::Float(0.0))),
            vec![
                (Pat::Lit(Lit::Float(-0.0)), None, int(0)),
                (Pat::Wild, None, int(1)),
            ],
            con("Int"),
        );
        assert_eq!(simplified(&t), t);
    }

    /// Case of case: the outer alternatives end up in a join, entered from each
    /// branch of the inner `if`.
    #[test]
    fn the_outer_alternatives_are_named_once() {
        // case (if c then A else B) of { A -> 1; B -> 2 }
        let inner = Term::If(
            Arc::new(Term::Var(v(1))),
            Arc::new(Term::Ctor(InternedString::from("A"), con("T"), vec![])),
            Arc::new(Term::Ctor(InternedString::from("B"), con("T"), vec![])),
        );
        let t = Term::Case(
            Arc::new(inner),
            vec![
                (Pat::Ctor(InternedString::from("A"), vec![]), None, int(1)),
                (Pat::Ctor(InternedString::from("B"), vec![]), None, int(2)),
            ],
            con("Int"),
        );
        // The whole cascade: the outer alternatives go into a join, each branch
        // jumps to it with a constructor it built, entering the join turns each
        // jump into a `case` on a known constructor, that `case` is its own
        // arm, and the join -- now jumped to from nowhere -- goes. Two
        // allocations, a tag test and a branch become a branch.
        assert_eq!(
            simplified(&t),
            Term::If(
                Arc::new(Term::Var(v(1))),
                Arc::new(int(1)),
                Arc::new(int(2))
            )
        );
    }

    /// The join stays when it is doing its job: entered from two places, with
    /// nothing known about what reaches it, it is a label several branches
    /// share -- which is cheaper than the context copied into both.
    #[test]
    fn a_join_that_decides_nothing_is_kept() {
        // A branch whose answer is computed, so nothing about it is known --
        // but whose type core wrote down, so the join can be given one.
        let inner = Term::If(
            Arc::new(Term::Var(v(1))),
            Arc::new(Term::Prim(
                Prim::Add,
                vec![Term::Var(v(2)), Term::Var(v(3))],
                con("T"),
            )),
            Arc::new(Term::Var(v(3))),
        );
        let t = Term::Case(
            Arc::new(inner),
            vec![
                (Pat::Ctor(InternedString::from("A"), vec![]), None, int(1)),
                (Pat::Var(v(4), con("T")), None, int(2)),
            ],
            con("Int"),
        );
        let got = simplified(&t);
        let Term::Join { params, .. } = &got else {
            panic!("not a join: {got:?}");
        };
        assert_eq!(
            params[0].1,
            con("T"),
            "the join takes what the inner answers"
        );
    }

    /// Where the inner term's type is not written down, the rewrite is
    /// declined: core is still typed here, and a join parameter typed wrongly
    /// is a value in the wrong representation.
    #[test]
    fn an_untyped_scrutinee_is_left_alone() {
        let t = Term::Case(
            Arc::new(Term::If(
                Arc::new(Term::Var(v(1))),
                Arc::new(Term::Var(v(2))),
                Arc::new(Term::Var(v(3))),
            )),
            vec![
                (Pat::Lit(Lit::Int(0)), None, int(2)),
                (Pat::Var(v(4), con("Int")), None, int(1)),
            ],
            con("Int"),
        );
        assert_eq!(simplified(&t), t);
    }

    /// A wildcard arm throws the scrutinee away, so the scrutinee has to be
    /// something there is nothing to throw away *of*. Dropping `f x` would be
    /// dropping the call.
    #[test]
    fn a_wildcard_does_not_discard_work() {
        let call = Term::App(Arc::new(Term::Var(v(1))), Arc::new(int(2)));
        let t = Term::Case(Arc::new(call), vec![(Pat::Wild, None, int(1))], con("Int"));
        assert_eq!(simplified(&t), t, "the call has to still happen");
    }

    #[test]
    fn a_multiply_by_a_power_of_two_is_a_shift() {
        let t = Term::Prim(Prim::Mul, vec![Term::Var(v(1)), int(8)], con("Int"));
        let got = simplified(&t);
        assert_eq!(
            got,
            Term::Prim(Prim::Shl, vec![Term::Var(v(1)), int(3)], con("Int"))
        );
    }

    /// The one that is not allowed: `-1 / 2` is `0` and `-1 >> 1` is `-1`.
    #[test]
    fn a_signed_division_is_not_a_shift() {
        let t = Term::Prim(Prim::Div, vec![Term::Var(v(1)), int(8)], con("Int"));
        assert_eq!(simplified(&t), t);
        let u = Term::Prim(Prim::Div, vec![Term::Var(v(1)), int(8)], con("UInt64"));
        assert_eq!(
            simplified(&u),
            Term::Prim(Prim::Shr, vec![Term::Var(v(1)), int(3)], con("UInt64"))
        );
    }

    #[test]
    fn a_remainder_by_a_power_of_two_is_a_mask_when_unsigned() {
        let t = Term::Prim(Prim::Mod, vec![Term::Var(v(1)), int(8)], con("UInt32"));
        assert_eq!(
            simplified(&t),
            Term::Prim(Prim::BitAnd, vec![Term::Var(v(1)), int(7)], con("UInt32"))
        );
    }

    #[test]
    fn the_identities_fold() {
        let x = Term::Var(v(1));
        for (p, k) in [
            (Prim::Mul, 1),
            (Prim::Div, 1),
            (Prim::Add, 0),
            (Prim::Sub, 0),
        ] {
            let t = Term::Prim(p, vec![x.clone(), int(k)], con("Int"));
            assert_eq!(simplified(&t), x, "{p:?} by {k}");
        }
        let zero = Term::Prim(Prim::Mul, vec![x.clone(), int(0)], con("Int"));
        assert_eq!(simplified(&zero), int(0));
    }

    /// Not a power of two, so nothing happens -- the rewrite has to be checked
    /// somewhere, and a multiply that stays a multiply is the common case.
    #[test]
    fn an_ordinary_multiply_is_left_alone() {
        let t = Term::Prim(Prim::Mul, vec![Term::Var(v(1)), int(10)], con("Int"));
        assert_eq!(simplified(&t), t);
    }
}
