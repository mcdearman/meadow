//! **Specialization: one copy of a number-generic definition per number type.**
//!
//! `fun fib n = let rec loop a b i = ... in loop 0 1 n` is generic over two
//! integer types, and nothing at run time says which ones a call means: every
//! backend is untyped. The operators do not need telling -- a `+` takes the type
//! of the values it meets (see [`crate::num`]) -- but a literal does. Its value
//! has to be made as *some* type, and made as an `Int` it wraps, so `fib 100`
//! said `BigInt` and computed with 64 bits.
//!
//! So before types are erased, this pass makes a copy of a definition for each
//! combination of number types it is used at. Where core writes
//! `fib [BigInt, BigInt]`, the mention becomes a mention of a copy of `fib` whose
//! number binders are gone and whose literals are `BigInt`s. Copies mention
//! other generic definitions at types that are now concrete, which asks for more
//! copies, until nothing new is asked for. There are finitely many number types,
//! so that always stops.
//!
//! Only number-class binders are specialized ([`VarKind::Num`] and
//! [`VarKind::Frac`]). A plain type variable needs no runtime representation, so
//! copying on one would only grow the program.
//!
//! The generic original stays. It is what runs where no number type can be
//! known -- a definition started directly, by a debugger or a test runner, with
//! its binders unfilled -- and there a literal is an `Int` and takes the type of
//! what it meets, as it always did.
//!
//! A local binding generic over a number type -- `loop` above, when it is --
//! is copied the same way, inside the scope it is bound in.

use crate::*;
use std::collections::{HashSet, VecDeque};

/// Where specialized copies' names start: clear of every unit, and of the
/// synthetic definitions a test runner appends (`hir::SYNTHETIC_BASE` up).
pub const SPECIALIZED_BASE: u32 = 0x7800_0000;

/// Specialize a whole program. Copies are appended after the definitions it
/// already has, so a definition's position -- which backends label by -- does
/// not move.
pub fn program(p: &Program) -> Program {
    let tops: HashMap<Var, Vec<usize>> = p
        .defs
        .iter()
        .filter_map(|d| {
            let positions = number_positions(&d.poly);
            (!positions.is_empty()).then_some((d.var, positions))
        })
        .collect();
    if tops.is_empty() && !p.defs.iter().any(|d| mentions_generic_local(&d.term)) {
        return p.clone();
    }
    let mut s = Specializer {
        tops,
        instances: HashMap::new(),
        queue: VecDeque::new(),
        scopes: Vec::new(),
        next: SPECIALIZED_BASE,
        origins: HashMap::new(),
    };
    let by_var: HashMap<Var, &Def> = p.defs.iter().map(|d| (d.var, d)).collect();

    let mut defs: Vec<Def> = p
        .defs
        .iter()
        .map(|d| Def {
            var: d.var,
            name: d.name,
            poly: d.poly.clone(),
            term: s.term(&d.term, &HashMap::new()),
        })
        .collect();

    while let Some((orig, key, var)) = s.queue.pop_front() {
        let d = by_var[&orig];
        let (poly, sigma) = specialize_poly(&d.poly, &key);
        let body = strip_number_binders(&d.term, &d.poly, &poly);
        let term = s.term(&body, &sigma);
        let term = s.freshen(&term);
        defs.push(Def { var, name: d.name, poly, term });
    }

    let mut origins = p.origins.clone();
    origins.extend(s.origins);
    Program {
        defs,
        entry: p.entry,
        ctor_fields: p.ctor_fields.clone(),
        variants: p.variants.clone(),
        origins,
    }
}

/// A shortcut for the common case: is there a generic local anywhere? Only
/// consulted when no top-level definition is generic, since then nothing else
/// could ask for a copy.
fn mentions_generic_local(t: &Term) -> bool {
    let mut found = false;
    walk(t, &mut |t| match t {
        Term::Let(_, poly, _, _) if !number_positions(poly).is_empty() => found = true,
        Term::LetRec(binds, _) if binds.iter().any(|(_, p, _)| !number_positions(p).is_empty()) => {
            found = true
        }
        _ => {}
    });
    found
}

fn walk(t: &Term, f: &mut impl FnMut(&Term)) {
    f(t);
    match t {
        Term::Var(_) | Term::Lit(_) | Term::Error => {}
        Term::Lam(_, _, b) | Term::TyLam(_, b) | Term::Proj(b, _) | Term::Loc(_, b) => walk(b, f),
        Term::TyApp(b, _) | Term::Sel(b, _, _) | Term::Perform(_, _, b, _) => walk(b, f),
        Term::App(a, b) | Term::Let(_, _, a, b) | Term::Extend(a, _, b) => {
            walk(a, f);
            walk(b, f);
        }
        Term::LetRec(binds, body) => {
            for (_, _, t) in binds {
                walk(t, f);
            }
            walk(body, f);
        }
        Term::If(c, a, b) => {
            walk(c, f);
            walk(a, f);
            walk(b, f);
        }
        Term::Tuple(xs) | Term::Array(xs, _) | Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
            xs.iter().for_each(|x| walk(x, f))
        }
        Term::Record(fs) => fs.iter().for_each(|(_, x)| walk(x, f)),
        Term::Case(s, arms, _) => {
            walk(s, f);
            arms.iter().for_each(|(_, b)| walk(b, f));
        }
        Term::Handle { body, clauses, ret, .. } => {
            walk(body, f);
            clauses.iter().for_each(|c| walk(&c.body, f));
            if let Some((_, _, b)) = ret {
                walk(b, f);
            }
        }
    }
}

/// The positions of a polytype's number-class binders.
fn number_positions(poly: &Poly) -> Vec<usize> {
    poly.binders
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.kind, VarKind::Num | VarKind::Frac))
        .map(|(i, _)| i)
        .collect()
}

/// The number types a literal can be made as.
fn number_type(ty: &Ty) -> Option<InternedString> {
    match ty {
        InferType::Con(n, args) if args.is_empty() => matches!(
            &**n,
            "Int" | "BigInt" | "Float" | "Float32" | "Int8" | "Int16" | "Int32" | "UInt8"
                | "UInt16" | "UInt32" | "UInt64"
        )
        .then_some(*n),
        _ => None,
    }
}

/// A polytype with its number binders filled in by `key`, in order: the binders
/// left, and the substitution for the ones taken.
fn specialize_poly(poly: &Poly, key: &[InternedString]) -> (Poly, HashMap<u32, Ty>) {
    let mut sigma = HashMap::new();
    let mut binders = Vec::new();
    let mut k = key.iter();
    for b in &poly.binders {
        if matches!(b.kind, VarKind::Num | VarKind::Frac) {
            let name = k.next().expect("a type for every number binder");
            sigma.insert(b.id, InferType::Con(*name, Vec::new()));
        } else {
            binders.push(*b);
        }
    }
    (Poly { binders, ty: subst_rigid(&poly.ty, &sigma) }, sigma)
}

/// The body of a generic binding, its `TyLam` rebuilt with only the binders the
/// copy keeps.
fn strip_number_binders(t: &Term, old: &Poly, new: &Poly) -> Term {
    match t {
        Term::Loc(l, inner) => Term::Loc(*l, Arc::new(strip_number_binders(inner, old, new))),
        Term::TyLam(binders, body) if !old.binders.is_empty() && *binders == old.binders => {
            if new.binders.is_empty() {
                (**body).clone()
            } else {
                Term::TyLam(new.binders.clone(), body.clone())
            }
        }
        other => other.clone(),
    }
}

/// A local binding generic over a number type, while its scope is rewritten:
/// the copies asked for so far.
struct Local {
    var: Var,
    positions: Vec<usize>,
    wanted: Vec<(Vec<InternedString>, Var)>,
}

struct Specializer {
    /// Top-level definitions generic over a number type, and where their
    /// number binders are.
    tops: HashMap<Var, Vec<usize>>,
    instances: HashMap<(Var, Vec<InternedString>), Var>,
    /// Top-level copies asked for and not yet made: the original, the types,
    /// and the copy's name.
    queue: VecDeque<(Var, Vec<InternedString>, Var)>,
    /// Generic locals in scope, innermost last.
    scopes: Vec<Local>,
    next: u32,
    /// Each renamed binder, and what it was a copy of.
    origins: HashMap<Var, Var>,
}

impl Specializer {
    fn fresh(&mut self) -> Var {
        let v = hir::VarId(self.next);
        self.next += 1;
        v
    }

    /// The name of `x` at `args`, if `x` is generic over a number type and
    /// `args` says which: a copy, asked for if it is new.
    fn instance(&mut self, x: Var, args: &[Ty]) -> Option<(Var, Vec<Ty>)> {
        let positions = match self.scopes.iter().rev().find(|l| l.var == x) {
            Some(local) => local.positions.clone(),
            None => self.tops.get(&x)?.clone(),
        };
        let key: Vec<InternedString> =
            positions.iter().map(|&i| args.get(i).and_then(number_type)).collect::<Option<_>>()?;
        let rest: Vec<Ty> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| !positions.contains(i))
            .map(|(_, t)| t.clone())
            .collect();

        if let Some(depth) = self.scopes.iter().rposition(|l| l.var == x) {
            if let Some((_, v)) = self.scopes[depth].wanted.iter().find(|(k, _)| *k == key) {
                return Some((*v, rest));
            }
            let v = self.fresh();
            self.scopes[depth].wanted.push((key, v));
            return Some((v, rest));
        }
        if let Some(v) = self.instances.get(&(x, key.clone())) {
            return Some((*v, rest));
        }
        let v = self.fresh();
        self.instances.insert((x, key.clone()), v);
        self.queue.push_back((x, key, v));
        Some((v, rest))
    }

    fn lit(&self, l: &Lit, sigma: &HashMap<u32, Ty>) -> Lit {
        let ty = |id: &u32| sigma.get(id).and_then(number_type);
        match l {
            Lit::AnyInt(n, id) => match ty(id).as_deref() {
                Some("BigInt") => Lit::BigInt(*n),
                Some("Int") => Lit::Int(*n),
                Some(name) => match num::Width::from_type(name) {
                    Some(w) => Lit::Word(w, w.wrap(*n as i128)),
                    None => l.clone(),
                },
                None => l.clone(),
            },
            Lit::AnyFloat(x, id) => match ty(id).as_deref() {
                Some("Float32") => Lit::Float32(*x as f32),
                Some("Float") => Lit::Float(*x),
                _ => l.clone(),
            },
            other => other.clone(),
        }
    }

    fn ty(&self, t: &Ty, sigma: &HashMap<u32, Ty>) -> Ty {
        if sigma.is_empty() { t.clone() } else { subst_rigid(t, sigma) }
    }

    fn poly(&self, p: &Poly, sigma: &HashMap<u32, Ty>) -> Poly {
        Poly { binders: p.binders.clone(), ty: self.ty(&p.ty, sigma) }
    }

    fn pat(&self, p: &Pat, sigma: &HashMap<u32, Ty>) -> Pat {
        if sigma.is_empty() {
            return p.clone();
        }
        crate::rewrite::pattern(p, &mut |p| match p {
            Pat::Var(v, ty) => Pat::Var(v, subst_rigid(&ty, sigma)),
            Pat::As(v, ty, sub) => Pat::As(v, subst_rigid(&ty, sigma), sub),
            other => other,
        })
    }

    /// `t` with every variable it binds renamed to a fresh one.
    ///
    /// A copy is made from the same term as the original and every other copy,
    /// and so binds the same variables -- at different types. Everything below
    /// core takes a variable to name one binding, and reads its type from where
    /// it is bound, so each copy gets names of its own.
    fn freshen(&mut self, t: &Term) -> Term {
        let mut names = HashMap::new();
        freshen(t, self, &mut names)
    }

    fn arc(&mut self, t: &Term, sigma: &HashMap<u32, Ty>) -> Arc<Term> {
        Arc::new(self.term(t, sigma))
    }

    /// Rewrite `t` under `sigma`, the number types of the copy it is part of.
    fn term(&mut self, t: &Term, sigma: &HashMap<u32, Ty>) -> Term {
        match t {
            Term::Lit(l) => Term::Lit(self.lit(l, sigma)),
            Term::Var(_) | Term::Error => t.clone(),
            Term::TyApp(f, args) => {
                let args: Vec<Ty> = args.iter().map(|a| self.ty(a, sigma)).collect();
                if let Term::Var(x) = &**f {
                    if let Some((copy, rest)) = self.instance(*x, &args) {
                        return if rest.is_empty() {
                            Term::Var(copy)
                        } else {
                            Term::TyApp(Arc::new(Term::Var(copy)), rest)
                        };
                    }
                }
                Term::TyApp(self.arc(f, sigma), args)
            }
            Term::TyLam(binders, body) => Term::TyLam(binders.clone(), self.arc(body, sigma)),
            Term::Loc(l, inner) => Term::Loc(*l, self.arc(inner, sigma)),
            Term::Lam(v, ty, body) => Term::Lam(*v, self.ty(ty, sigma), self.arc(body, sigma)),
            Term::App(f, a) => Term::App(self.arc(f, sigma), self.arc(a, sigma)),
            Term::Let(v, poly, rhs, body) => {
                let positions = number_positions(poly);
                if positions.is_empty() {
                    return Term::Let(*v, self.poly(poly, sigma), self.arc(rhs, sigma), self.arc(body, sigma));
                }
                // The body says which copies it wants; each is the right-hand
                // side made again at those types, bound around the body.
                self.scopes.push(Local { var: *v, positions, wanted: Vec::new() });
                let body = self.term(body, sigma);
                let local = self.scopes.pop().expect("the scope just pushed");
                let mut out = body;
                for (key, copy) in local.wanted.iter().rev() {
                    let (p, mut inner) = specialize_poly(poly, key);
                    inner.extend(sigma.iter().map(|(k, v)| (*k, v.clone())));
                    let rhs = self.term(&strip_number_binders(rhs, poly, &p), &inner);
                    let rhs = self.freshen(&rhs);
                    out = Term::Let(*copy, self.poly(&p, &inner), Arc::new(rhs), Arc::new(out));
                }
                let original = self.term(rhs, sigma);
                Term::Let(*v, self.poly(poly, sigma), Arc::new(original), Arc::new(out))
            }
            Term::LetRec(binds, body) => self.let_rec(binds, body, sigma),
            Term::If(c, a, b) => Term::If(self.arc(c, sigma), self.arc(a, sigma), self.arc(b, sigma)),
            Term::Tuple(xs) => Term::Tuple(xs.iter().map(|x| self.term(x, sigma)).collect()),
            Term::Proj(x, i) => Term::Proj(self.arc(x, sigma), *i),
            Term::Array(xs, ty) => {
                Term::Array(xs.iter().map(|x| self.term(x, sigma)).collect(), self.ty(ty, sigma))
            }
            Term::Record(fs) => Term::Record(fs.iter().map(|(l, x)| (*l, self.term(x, sigma))).collect()),
            Term::Sel(x, l, ty) => Term::Sel(self.arc(x, sigma), *l, self.ty(ty, sigma)),
            Term::Extend(x, l, v) => Term::Extend(self.arc(x, sigma), *l, self.arc(v, sigma)),
            Term::Ctor(n, ty, xs) => {
                Term::Ctor(*n, self.ty(ty, sigma), xs.iter().map(|x| self.term(x, sigma)).collect())
            }
            Term::Case(s, arms, ty) => Term::Case(
                self.arc(s, sigma),
                arms.iter().map(|(p, b)| (self.pat(p, sigma), self.term(b, sigma))).collect(),
                self.ty(ty, sigma),
            ),
            Term::Prim(op, xs, ty) => {
                Term::Prim(*op, xs.iter().map(|x| self.term(x, sigma)).collect(), self.ty(ty, sigma))
            }
            Term::Perform(e, op, a, ty) => Term::Perform(*e, *op, self.arc(a, sigma), self.ty(ty, sigma)),
            Term::Handle { body, clauses, ret, ty } => Term::Handle {
                body: self.arc(body, sigma),
                clauses: clauses
                    .iter()
                    .map(|c| HClause {
                        effect: c.effect,
                        op: c.op,
                        param: c.param,
                        param_ty: self.ty(&c.param_ty, sigma),
                        resume: c.resume,
                        resume_ty: self.ty(&c.resume_ty, sigma),
                        body: self.term(&c.body, sigma),
                    })
                    .collect(),
                ret: ret.as_ref().map(|(v, t, b)| (*v, self.ty(t, sigma), self.arc(b, sigma))),
                ty: self.ty(ty, sigma),
            },
        }
    }

    /// A recursive group: the body, then every copy any of them asked for --
    /// including copies the copies ask for -- in one group with the originals.
    fn let_rec(&mut self, binds: &[(Var, Poly, Term)], body: &Term, sigma: &HashMap<u32, Ty>) -> Term {
        let generic: Vec<usize> =
            (0..binds.len()).filter(|&i| !number_positions(&binds[i].1).is_empty()).collect();
        let first = self.scopes.len();
        for &i in &generic {
            self.scopes.push(Local {
                var: binds[i].0,
                positions: number_positions(&binds[i].1),
                wanted: Vec::new(),
            });
        }
        let body = self.term(body, sigma);
        let mut out: Vec<(Var, Poly, Term)> =
            binds.iter().map(|(v, p, t)| (*v, self.poly(p, sigma), self.term(t, sigma))).collect();

        // Make copies until none is new.
        let mut made: HashSet<Var> = HashSet::new();
        loop {
            let pending: Vec<(usize, Vec<InternedString>, Var)> = generic
                .iter()
                .enumerate()
                .flat_map(|(j, &i)| {
                    self.scopes[first + j]
                        .wanted
                        .iter()
                        .filter(|(_, v)| !made.contains(v))
                        .map(move |(k, v)| (i, k.clone(), *v))
                        .collect::<Vec<_>>()
                })
                .collect();
            if pending.is_empty() {
                break;
            }
            for (i, key, copy) in pending {
                made.insert(copy);
                let (_, poly, rhs) = &binds[i];
                let (p, mut inner) = specialize_poly(poly, &key);
                inner.extend(sigma.iter().map(|(k, v)| (*k, v.clone())));
                let rhs = self.term(&strip_number_binders(rhs, poly, &p), &inner);
                let rhs = self.freshen(&rhs);
                let p = self.poly(&p, &inner);
                out.push((copy, p, rhs));
            }
        }
        self.scopes.truncate(first);
        Term::LetRec(out, Arc::new(body))
    }
}

/// A fresh name for binder `v`, remembered for the variables that mention it.
fn rename(v: Var, s: &mut Specializer, names: &mut HashMap<Var, Var>) -> Var {
    let n = s.fresh();
    names.insert(v, n);
    let origin = s.origins.get(&v).copied().unwrap_or(v);
    s.origins.insert(n, origin);
    n
}

fn freshen_pat(p: &Pat, s: &mut Specializer, names: &mut HashMap<Var, Var>) -> Pat {
    match p {
        Pat::Wild | Pat::Lit(_) => p.clone(),
        Pat::Var(v, ty) => Pat::Var(rename(*v, s, names), ty.clone()),
        Pat::As(v, ty, sub) => {
            let n = rename(*v, s, names);
            Pat::As(n, ty.clone(), Box::new(freshen_pat(sub, s, names)))
        }
        Pat::Tuple(ps) => Pat::Tuple(ps.iter().map(|x| freshen_pat(x, s, names)).collect()),
        Pat::Array(ps) => Pat::Array(ps.iter().map(|x| freshen_pat(x, s, names)).collect()),
        Pat::Ctor(c, ps) => Pat::Ctor(*c, ps.iter().map(|x| freshen_pat(x, s, names)).collect()),
        Pat::Record(fs) => {
            Pat::Record(fs.iter().map(|(l, x)| (*l, freshen_pat(x, s, names))).collect())
        }
    }
}

/// See [`Specializer::freshen`].
fn freshen(t: &Term, s: &mut Specializer, names: &mut HashMap<Var, Var>) -> Term {
    let go = |x: &Term, s: &mut Specializer, names: &mut HashMap<Var, Var>| {
        Arc::new(freshen(x, s, names))
    };
    match t {
        Term::Var(v) => Term::Var(names.get(v).copied().unwrap_or(*v)),
        Term::Lit(_) | Term::Error => t.clone(),
        Term::Loc(l, b) => Term::Loc(*l, go(b, s, names)),
        Term::TyLam(bs, b) => Term::TyLam(bs.clone(), go(b, s, names)),
        Term::TyApp(f, tys) => Term::TyApp(go(f, s, names), tys.clone()),
        Term::Lam(v, ty, b) => {
            let n = rename(*v, s, names);
            Term::Lam(n, ty.clone(), go(b, s, names))
        }
        Term::App(f, a) => {
            let f = go(f, s, names);
            Term::App(f, go(a, s, names))
        }
        Term::Let(v, poly, rhs, body) => {
            let rhs = go(rhs, s, names);
            let n = rename(*v, s, names);
            Term::Let(n, poly.clone(), rhs, go(body, s, names))
        }
        Term::LetRec(binds, body) => {
            let fresh: Vec<Var> = binds.iter().map(|(v, _, _)| rename(*v, s, names)).collect();
            let binds = binds
                .iter()
                .zip(fresh)
                .map(|((_, poly, t), n)| (n, poly.clone(), freshen(t, s, names)))
                .collect();
            Term::LetRec(binds, go(body, s, names))
        }
        Term::If(c, a, b) => {
            let c = go(c, s, names);
            let a = go(a, s, names);
            Term::If(c, a, go(b, s, names))
        }
        Term::Tuple(xs) => Term::Tuple(xs.iter().map(|x| freshen(x, s, names)).collect()),
        Term::Proj(x, i) => Term::Proj(go(x, s, names), *i),
        Term::Array(xs, ty) => {
            Term::Array(xs.iter().map(|x| freshen(x, s, names)).collect(), ty.clone())
        }
        Term::Record(fs) => {
            Term::Record(fs.iter().map(|(l, x)| (*l, freshen(x, s, names))).collect())
        }
        Term::Sel(x, l, ty) => Term::Sel(go(x, s, names), *l, ty.clone()),
        Term::Extend(x, l, v) => {
            let x = go(x, s, names);
            Term::Extend(x, *l, go(v, s, names))
        }
        Term::Ctor(c, ty, xs) => {
            Term::Ctor(*c, ty.clone(), xs.iter().map(|x| freshen(x, s, names)).collect())
        }
        Term::Case(scrut, arms, ty) => {
            let scrut = go(scrut, s, names);
            let arms = arms
                .iter()
                .map(|(p, b)| {
                    let p = freshen_pat(p, s, names);
                    (p, freshen(b, s, names))
                })
                .collect();
            Term::Case(scrut, arms, ty.clone())
        }
        Term::Prim(op, xs, ty) => {
            Term::Prim(*op, xs.iter().map(|x| freshen(x, s, names)).collect(), ty.clone())
        }
        Term::Perform(e, op, a, ty) => Term::Perform(*e, *op, go(a, s, names), ty.clone()),
        Term::Handle { body, clauses, ret, ty } => {
            let body = go(body, s, names);
            let clauses = clauses
                .iter()
                .map(|c| {
                    let param = rename(c.param, s, names);
                    let resume = rename(c.resume, s, names);
                    HClause {
                        param,
                        resume,
                        body: freshen(&c.body, s, names),
                        ..c.clone()
                    }
                })
                .collect();
            let ret = ret.as_ref().map(|(v, ty, b)| {
                let n = rename(*v, s, names);
                (n, ty.clone(), go(b, s, names))
            });
            Term::Handle { body, clauses, ret, ty: ty.clone() }
        }
    }
}
