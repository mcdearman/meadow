//! `core` → AxCut.
//!
//! The translation is the classical one: **a value is returned by invoking a
//! continuation**. A continuation is codata with a single method, so returning
//! needs no mechanism of its own —
//!
//! ```text
//!   substitute [k, v] in {(k, v) => invoke k#0}
//! ```
//!
//! — and a function is codata too, with one method taking an argument and the
//! continuation to answer with. Calling and returning are one operation seen
//! from its two sides, which is the duality the IR is named for.
//!
//! Everything in `core` is covered. [`Unsupported`] has one variant left, for a
//! variable that is neither in scope nor a definition, which is a bug upstream
//! rather than a gap here.
//!
//! # The environment is positional, and this pass has to know its shape
//!
//! This is the part that makes the IR worth having and the pass awkward to
//! write. A block's parameters are not "the new bindings" — they name *the whole
//! environment* at that point, in order. So the translation cannot emit a
//! statement without knowing exactly what is live and in what order, and every
//! function here threads that list explicitly.
//!
//! The payoff is that nothing downstream has to reconstruct it. `substitute` is
//! literally the move sequence; `jump` and `invoke` carry no arguments because
//! there is nothing left to pass. A register allocator reads its answer off the
//! IR instead of computing liveness.
//!
//! The price is paid here, in [`Lower::bind`], which must be handed the set of
//! names that survive the subexpression it is sequencing — and that set is
//! computed from the free variables of everything still to come.
//!
//! # Definitions are labels, and so are `letrec` bindings
//!
//! A reference to a global lowers to `substitute [k] in {(k) => jump L}`: `jump`
//! leaves the environment alone, and the definition's block takes exactly the
//! continuation, so the two line up with no shuffling at all. Recursion needs no
//! back-patching, which is the reason to do it this way.
//!
//! `letrec` is the same mechanism with the free variables made explicit — plain
//! lambda lifting. A group's bindings share one parameter list, the variables
//! they capture from the enclosing scope, and a reference to one becomes
//! `substitute [a, b, k] in {(a, b, k) => jump L}`. Mutual recursion then costs
//! nothing extra, and no object has to point at itself, which is what a
//! closure-based encoding would need and which no `Rc` graph should have to do.
//!
//! It does mean such a binding is re-evaluated at every reference rather than
//! once, the way the CEK machine does it. For a right-hand side that is a lambda
//! or a literal — nearly all of them — that is unobservable. For one that
//! performs an effect it is not, and fixing it means giving each definition a
//! memoising thunk. See the note in `meadow_rts::axcut`.
//!
//! # Pattern matching
//!
//! `match` compiles to a chain of failure continuations: one codata object per
//! arm, each capturing the next, and a pattern that fails invokes it. That is
//! backtracking, not a decision tree — arms can retest what an earlier arm
//! already tested, so a wide `match` on one scrutinee does more work than it
//! needs to. It is correct and it is small.
//!
//! At [`OptLevel::O2`] the arms that dispatch on a constructor become a single
//! `switch` instead — [`Lower::case_tree`]. It is gated because a decision tree
//! is a code-size trade in general, and because the chain is what the arms fall
//! back to when the tree does not cover them, so both have to keep working.
//!
//! # Where this departs from the paper
//!
//! The paper's environment is **linear**: `let` and `new` consume the prefix
//! they build from. Here they prepend and leave the rest alone, so a name may be
//! used twice without an explicit duplication. `substitute` is still the only
//! thing that shrinks the environment, and it appears at every call and return,
//! so environments stay bounded — but the linearity that lets the paper's
//! backend place registers without analysis is not yet enforced.
//!
//! # Effects: evidence passing
//!
//! AxCut has no handlers, and after this pass Meadow's has none either. A
//! `handle` and a `perform` lower to ordinary objects, data and jumps, so every
//! engine below -- the reference machine, the bytecode VM, native code -- runs
//! effects without knowing they exist.
//!
//! **The evidence.** Every function takes one parameter more than it is
//! written with: the handlers installed where it was called, as a list of
//! `#ev(key, clause, target, rest)` entries, newest first. `key` names the
//! operation (`"State.get"`), `clause` is an object whose one method is that
//! operation's clause, and `target` is a `Ref` holding where the whole `handle`
//! expression's value goes. The empty list is `()`. A continuation does not
//! take it: it captures the evidence it needs like any other name, which is
//! also why resuming a continuation needs no evidence restored.
//!
//! **`handle body with clauses`** allocates `target = Ref k`, one clause object
//! per clause, and one entry per clause on top of the current evidence, then
//! runs `body` with that as its evidence and a return continuation `H` as its
//! continuation. `H` reads `target` and runs the `return` clause there, under
//! the evidence outside the handler.
//!
//! **`perform E.op x`** searches the evidence for `"E.op"` -- a jump to a small
//! block of its own, [`Lower::perform`] -- and then, with the entry found,
//! builds a one-shot resumption `R` and enters the clause with `x`, `R`, and
//! the value in `target`. With none found, the operation goes to the runtime
//! ([`Extern::Native`]): `Console`, `Fs` and the others the real world answers.
//!
//! **A tail-resumptive clause** -- `op x k -> k e`, which is most of them --
//! is entered with `x` and the performing code's own continuation instead, and
//! answers it with `e`: no resumption, no flag, nothing written. `k e` would
//! have done exactly that and sent what came of it where the clause's value
//! goes, which is the same place.
//!
//! **`resume v`**, called as `R` with continuation `c`, checks and sets `R`'s
//! flag, sets `target := c`, and continues the performing code with `v`. That
//! write is what makes handlers deep and the clause's `resume` return: when the
//! resumed body finishes, `H` reads the resume site out of `target`. It is safe
//! because a resumption runs at most once.
//!
//! Clauses and the `return` clause capture the evidence from outside the
//! handler, so an operation they perform goes past it, as it must.

use crate::{Block, Def, Extern, Label, Name, Program, Rep, Statement, Tag};
use meadow_core as core;
use meadow_core::{OptLevel, Pat, Term, Var};
use meadow_hir::VarId;
use meadow_intern::InternedString;
use std::collections::{HashMap, HashSet};

/// A `core` construct this pass does not translate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unsupported {
    /// A variable that is neither in scope nor a definition — an unsaturated
    /// primitive operator, most likely, which the front end should have
    /// eta-expanded.
    UnboundVar,
}

pub struct Lowered {
    pub program: Program,
    /// Empty means the program is fully covered.
    pub unsupported: HashSet<Unsupported>,
}

/// Lower a whole program.
///
/// Fresh names continue above the highest [`VarId`] the front end used, so an
/// invented name can never collide with a real one.
pub fn lower_program(program: &core::Program, opt: OptLevel) -> Lowered {
    // AxCut is untyped. Core's type abstractions and applications go first,
    // in one pass, so that nothing below has to see through one — see
    // [`core::erase`].
    //
    // Number-generic definitions are copied per number type first, while the
    // types that say which are still there -- see [`core::specialize`].
    // And each top-level value gets its cache, so it is evaluated once -- see
    // [`core::globals`].
    //
    // Types are *not* erased: a name's representation comes from its type,
    // and a call's type from the instantiation core wrote down. Lowering looks
    // through `TyLam` and `TyApp` itself -- see [`lam_spine`] and [`call_spine`].
    let program =
        &core::globals::program(&core::bools::program(&core::specialize::program(program)));
    let mut globals = HashMap::new();
    for (i, d) in program.defs.iter().enumerate() {
        globals.insert(d.var, (Label(i as u32), Vec::new()));
    }

    let mut lower = Lower {
        next_name: max_var(program) + 1,
        next_label: program.defs.len() as u32,
        globals,
        workers: HashMap::new(),
        defs: Vec::new(),
        tags: HashMap::new(),
        next_tag: 0,
        opt,
        unsupported: HashSet::new(),
        returns: HashSet::new(),
        continuations: HashSet::new(),
        ev: VarId(0),
        letrecs: HashSet::new(),
        worker_ev: HashSet::new(),
        polys: HashMap::new(),
        types: HashMap::new(),
        reps: HashMap::new(),
        variants: program.variants.clone(),
    };
    for d in &program.defs {
        lower.polys.insert(d.var, d.poly.clone());
        binders(&d.term, &mut lower.polys);
    }
    lower.ev = lower.fresh_ref();
    // Always a constructor of the program, used or not: a runtime starting a
    // function on a thread of its own has to hand it empty evidence, and
    // finds the tag by name.
    lower.tag_of(InternedString::from(EV_NONE));

    // A definition that is a lambda gets a second entry point, taking its
    // arguments directly. Registered before anything is lowered, so a call can
    // be compiled as a jump to a block that does not exist yet — including a
    // recursive one.
    for d in &program.defs {
        let (params, _) = lam_spine(&d.term);
        if !params.is_empty() {
            let label = lower.fresh_label();
            lower.workers.insert(d.var, (label, params.len()));
        }
    }

    // Which direct entry points need the evidence: none, to begin with, and
    // then every one whose body needs it given the ones found so far, until
    // nothing changes. Only ever adds, so it stops.
    loop {
        let mut changed = false;
        for d in &program.defs {
            if lower.workers.contains_key(&d.var) && !lower.worker_ev.contains(&d.var) {
                let (_, body) = lam_spine(&d.term);
                if lower.needs_ev(body) {
                    lower.worker_ev.insert(d.var);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Each definition is a block of one parameter: the continuation to answer
    // with. That is also exactly the environment a `jump` to it arrives with.
    // A definition takes no evidence: top-level values are pure, so a body
    // that needs some starts from none.
    let mut entry = None;
    for (i, d) in program.defs.iter().enumerate() {
        let label = Label(i as u32);
        let k = lower.function_return();
        let body = if lower.needs_ev(&d.term) {
            let ev = lower.fresh_ref();
            lower.ev = ev;
            let body = lower.expr(&d.term, &[ev, k], k);
            Statement::Let {
                name: ev,
                tag: lower.tag_of(InternedString::from(EV_NONE)),
                ctor: InternedString::from(EV_NONE),
                fields: vec![],
                rest: Box::new(body),
            }
        } else {
            lower.expr(&d.term, &[k], k)
        };
        lower.defs.push(Def {
            label,
            name: d.name,
            block: Block {
                params: vec![k],
                body,
            },
        });

        // The direct entry point. The block above still exists and is still
        // reached whenever the function is used as a *value* — passed to `map`,
        // partially applied, stored — where a real closure is the only answer.
        if let Some(&(worker, _)) = lower.workers.get(&d.var) {
            let (params, body) = lam_spine(&d.term);
            let k = lower.function_return();
            let ev = lower.fresh_ref();
            lower.ev = ev;
            let mut block_params = params;
            block_params.push(k);
            if lower.worker_ev.contains(&d.var) {
                block_params.push(ev);
            }
            let body = lower.expr(body, &block_params, k);
            lower.defs.push(Def {
                label: worker,
                name: d.name,
                block: Block {
                    params: block_params,
                    body,
                },
            });
        }

        if Some(d.var) == program.entry {
            entry = Some(label);
        }
    }

    // Lifted `letrec` blocks are pushed as they are discovered, so a definition
    // arrives after the blocks lifted out of it. Sorting by label puts the
    // table back in the order the labels read, which is what a dump should show.
    let mut defs = lower.defs;
    defs.sort_by_key(|d| d.label);

    Lowered {
        program: Program {
            defs,
            entry,
            tags: lower.tags,
            continuations: lower
                .continuations
                .difference(&lower.returns)
                .copied()
                .collect(),
            returns: lower.returns,
            ctor_fields: program.ctor_fields.clone(),
            origins: program.origins.clone(),
            reps: {
                let mut reps = lower.reps;
                for (v, poly) in &lower.polys {
                    reps.entry(*v).or_insert_with(|| Rep::of(&poly.ty));
                }
                reps
            },
        },
        unsupported: lower.unsupported,
    }
}

/// The highest variable the program mentions, so fresh names can start above it.
fn max_var(program: &core::Program) -> u32 {
    let mut seen = HashSet::new();
    let mut hi = 0;
    for d in &program.defs {
        hi = hi.max(d.var.0);
        seen.clear();
        mentions(&d.term, &mut seen);
        for v in &seen {
            hi = hi.max(v.0);
        }
    }
    hi
}

/// Split a binary primitive's arguments into "one operand, and a literal".
///
/// `n - 1` and `x == 0` are most of what arithmetic in a loop looks like, and a
/// literal operand needs neither a name nor a register — the folded `extern`
/// forms carry it. The literal has to end up on the *right*, which is where it
/// usually already is; a left-hand one is only moved across when the operation
/// does not care (see [`crate::commutes`]).
fn const_operand(p: core::Prim, args: &[Term]) -> Option<(Term, core::Lit)> {
    let [x, y] = args else {
        return None;
    };
    if let Term::Lit(l) = y {
        return Some((x.clone(), l.clone()));
    }
    match x {
        Term::Lit(l) if crate::commutes(p) => Some((y.clone(), l.clone())),
        _ => None,
    }
}

struct Lower {
    next_name: u32,
    next_label: u32,
    /// A definition or a `letrec` binding: where to jump, and the environment
    /// names its block expects before the continuation.
    globals: HashMap<Var, (Label, Vec<Name>)>,
    /// A top-level definition that is a lambda gets a second block: one that
    /// takes its arguments directly instead of returning a closure per argument.
    /// `(label, arity)` — a call with exactly that many arguments becomes a jump.
    workers: HashMap<Var, (Label, usize)>,
    defs: Vec<Def>,
    tags: HashMap<InternedString, Tag>,
    next_tag: Tag,
    /// What the back end is allowed to do beyond the unconditional minimum.
    opt: OptLevel,
    unsupported: HashSet<Unsupported>,
    /// See [`Program::returns`].
    returns: HashSet<Name>,
    /// See [`Program::continuations`].
    continuations: HashSet<Name>,
    /// The name holding the evidence -- the handlers in scope -- where lowering
    /// has got to. See the module docs.
    ev: Name,
    /// `letrec` bindings, whose blocks take the evidence.
    letrecs: HashSet<Var>,
    /// The definitions whose direct entry point takes the evidence. One that
    /// never performs, handles, or calls anything that might does not, and
    /// neither do calls to it -- so a first-order loop pays nothing for
    /// effects it does not use.
    worker_ev: HashSet<Var>,
    /// Every core variable's type, from where it is bound. Variables are
    /// unique, so this needs no scoping.
    polys: HashMap<Var, core::Poly>,
    /// The type of a name lowering invented to hold a core value, where known.
    types: HashMap<Name, core::Ty>,
    /// See [`Program::reps`]: the names lowering invented.
    reps: HashMap<Name, Rep>,
    /// Constructors' field types, for the fields a pattern binds.
    variants: meadow_infer::VariantEnv,
}

/// The constructor of an evidence entry: `#ev(key, clause, target, rest)`.
const EV: &str = "#ev";

/// The evidence with no handlers in it: an object, so that evidence is always
/// a reference, whatever it holds.
pub const EV_NONE: &str = "#evnone";

/// The same for a tail-resumptive clause -- `op x k -> k e` -- whose object
/// takes the argument and the performing code's own continuation, and so
/// needs no resumption at all.
const EV_TAIL: &str = "#evt";

/// If a clause only ever resumes, straight away, with a value computed without
/// the resumption -- `op x k -> k e` -- that value.
fn tail_resumptive(c: &core::HClause) -> Option<&Term> {
    let Term::App(f, e) = c.body.peel() else {
        return None;
    };
    if !matches!(f.peel(), Term::Var(v) if *v == c.resume) {
        return None;
    }
    let mut free = HashSet::new();
    core::free_vars_into(e, &mut free);
    (!free.contains(&c.resume)).then_some(&**e)
}

/// The continuation of a [`Lower::bind`]: given the name the value was bound to
/// and the environment that now holds, produce what runs next.
type Then<'a> = Box<dyn FnOnce(&mut Lower, Name, Vec<Name>) -> Statement + 'a>;

/// What to do once a pattern has matched, in the environment it left behind.
type Success<'a> = Box<dyn FnOnce(&mut Lower, Vec<Name>) -> Statement + 'a>;

impl Lower {
    fn fresh(&mut self) -> Name {
        let v = VarId(self.next_name);
        self.next_name += 1;
        v
    }

    /// A fresh name holding `rep`.
    fn fresh_as(&mut self, rep: Rep) -> Name {
        let v = self.fresh();
        self.reps.insert(v, rep);
        v
    }

    /// A fresh name holding a heap object: a closure, a continuation, data.
    fn fresh_ref(&mut self) -> Name {
        self.fresh_as(Rep::Ref)
    }

    /// A fresh name for a value of type `ty`, which may not be known.
    fn fresh_typed(&mut self, ty: Option<core::Ty>) -> Name {
        let v = self.fresh();
        self.set_type(v, ty);
        v
    }

    /// Say `n` holds a value of type `ty`, which may not be known.
    fn set_type(&mut self, n: Name, ty: Option<core::Ty>) {
        match ty {
            Some(ty) => {
                self.reps.insert(n, Rep::of(&ty));
                self.types.insert(n, ty);
            }
            None => {
                self.reps.entry(n).or_insert(Rep::Unknown);
            }
        }
    }

    /// The type of the value `n` holds, if it is known.
    fn type_of_name(&self, n: Name) -> Option<core::Ty> {
        match self.types.get(&n) {
            Some(ty) => Some(ty.clone()),
            None => self.polys.get(&n).map(|p| p.ty.clone()),
        }
    }

    /// The type of `t`, as far as a walk down its spine can tell -- enough to
    /// know how its value is represented.
    fn type_of(&self, t: &Term) -> Option<core::Ty> {
        use core::Ty;
        Some(match t {
            Term::Loc(_, inner) | Term::TyLam(_, inner) => return self.type_of(inner),
            Term::Var(v) => self.polys.get(v)?.ty.clone(),
            Term::TyApp(f, args) => match f.peel() {
                Term::Var(v) => self.polys.get(v)?.instantiate(args),
                other => return self.type_of(other),
            },
            Term::Lit(l) => lit_type(l),
            Term::Lam(_, param, body) => Ty::Fun(
                vec![param.clone()],
                Box::new(self.type_of(body).unwrap_or_else(core::unknown)),
                Box::new(Ty::RowEmpty),
            ),
            Term::App(f, _) => match self.type_of(f)? {
                Ty::Fun(params, ret, eff) if params.len() > 1 => {
                    Ty::Fun(params[1..].to_vec(), ret, eff)
                }
                Ty::Fun(_, ret, _) => *ret,
                _ => return None,
            },
            Term::Let(_, _, _, body) | Term::LetRec(_, body) => return self.type_of(body),
            Term::If(_, then, _) => return self.type_of(then),
            Term::Tuple(items) => Ty::Tuple(
                items
                    .iter()
                    .map(|x| self.type_of(x).unwrap_or_else(core::unknown))
                    .collect(),
            ),
            Term::Proj(t, i) => match self.type_of(t)? {
                Ty::Tuple(items) => items.get(*i)?.clone(),
                _ => return None,
            },
            Term::Array(_, elem) => Ty::Con(InternedString::from("Array"), vec![elem.clone()]),
            Term::Record(_) | Term::Extend(..) => Ty::Record(Box::new(Ty::RowEmpty)),
            Term::Sel(_, _, ty)
            | Term::Ctor(_, ty, _)
            | Term::Case(_, _, ty)
            | Term::Prim(_, _, ty)
            | Term::Perform(_, _, _, ty)
            | Term::Handle { ty, .. } => ty.clone(),
            Term::Error => return None,
        })
    }

    /// The types of constructor `ctor`'s fields, in a value of type `of`.
    fn field_types(&self, ctor: InternedString, of: Option<&core::Ty>) -> Option<Vec<core::Ty>> {
        let core::Ty::Con(name, args) = of? else {
            return None;
        };
        let bare = ctor.rsplit('.').next().unwrap_or(&ctor);
        let sig = self
            .variants
            .get(name)?
            .iter()
            .find(|v| *v.name == *ctor || *v.name == *bare)?;
        Some(
            sig.fields
                .iter()
                .map(|f| meadow_infer::subst_bound(f, args))
                .collect(),
        )
    }

    /// Names for the `n` fields of constructor `ctor` in a value of type `of`,
    /// each holding what the constructor's declaration says it does.
    fn field_names(&mut self, ctor: InternedString, n: usize, of: Option<&core::Ty>) -> Vec<Name> {
        let types = self.field_types(ctor, of);
        (0..n)
            .map(|i| {
                let ty = types.as_ref().and_then(|ts| ts.get(i).cloned());
                self.fresh_typed(ty)
            })
            .collect()
    }

    /// The type of field `label` of a record of type `of`.
    fn label_type(of: Option<&core::Ty>, label: InternedString) -> Option<core::Ty> {
        let core::Ty::Record(row) = of? else {
            return None;
        };
        let mut cur = &**row;
        while let core::Ty::RowExtend(l, ty, rest) = cur {
            if *l == label {
                return Some((**ty).clone());
            }
            cur = rest;
        }
        None
    }

    /// A fresh name for a function's own return continuation -- see
    /// [`Program::returns`].
    fn function_return(&mut self) -> Name {
        let k = self.fresh_ref();
        self.returns.insert(k);
        k
    }

    fn fresh_label(&mut self) -> Label {
        let l = Label(self.next_label);
        self.next_label += 1;
        l
    }

    fn tag_of(&mut self, ctor: InternedString) -> Tag {
        if let Some(t) = self.tags.get(&ctor) {
            return *t;
        }
        let t = self.next_tag;
        self.next_tag += 1;
        self.tags.insert(ctor, t);
        t
    }

    // --- liveness ---------------------------------------------------------

    /// Replace every label-bound name by the environment it needs.
    ///
    /// A `letrec` binding is not a value in the environment; reaching it means
    /// arranging its captured variables and jumping. So "this term mentions `f`"
    /// really means "this term needs whatever `f`'s block takes", and liveness
    /// has to be computed on the latter or a capture list comes out short.
    fn expand(&self, want: HashSet<Var>) -> HashSet<Var> {
        let mut out = HashSet::new();
        for v in want {
            match self.globals.get(&v) {
                Some((_, extra)) => out.extend(extra.iter().copied()),
                None => {
                    out.insert(v);
                }
            }
        }
        out
    }

    /// The names `terms` will need, minus `bound`, with labels expanded -- and
    /// the evidence, if any of them calls, performs or handles.
    fn wants(&self, terms: &[&Term], bound: &[Var]) -> HashSet<Var> {
        let mut want = HashSet::new();
        for t in terms {
            core::free_vars_into(t, &mut want);
        }
        for v in bound {
            want.remove(v);
        }
        let mut want = self.expand(want);
        if terms.iter().any(|t| self.needs_ev(t)) {
            want.insert(self.ev);
        }
        want
    }

    /// Does lowering `t` refer to the evidence in scope? A call passes it on, a
    /// `perform` searches it and a `handle` extends it. A lambda does not: its
    /// body gets the evidence its caller passes.
    fn needs_ev(&self, t: &Term) -> bool {
        match t {
            // A call passes the evidence on, unless it is a jump to a direct
            // entry point that does not take it.
            Term::App(..) => match self.direct_worker(t) {
                Some((f, args)) if !self.worker_ev.contains(&f) => {
                    args.iter().any(|a| self.needs_ev(a))
                }
                _ => true,
            },
            Term::Perform(..) | Term::Handle { .. } | Term::LetRec(..) => true,
            Term::Var(v) => self.letrecs.contains(v),
            Term::Lam(..) | Term::Lit(_) | Term::Error => false,
            Term::TyLam(_, b)
            | Term::TyApp(b, _)
            | Term::Loc(_, b)
            | Term::Proj(b, _)
            | Term::Sel(b, _, _) => self.needs_ev(b),
            Term::Let(_, _, r, b) | Term::Extend(r, _, b) => self.needs_ev(r) || self.needs_ev(b),
            Term::If(a, b, c) => self.needs_ev(a) || self.needs_ev(b) || self.needs_ev(c),
            Term::Tuple(xs) | Term::Array(xs, _) | Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
                xs.iter().any(|x| self.needs_ev(x))
            }
            Term::Record(fs) => fs.iter().any(|(_, x)| self.needs_ev(x)),
            Term::Case(s, arms, _) => {
                self.needs_ev(s) || arms.iter().any(|(_, b)| self.needs_ev(b))
            }
        }
    }

    /// Lower a lambda's body, whose parameters are `captures ++ [param, k, ev]`:
    /// the evidence is the caller's, not the one around the lambda.
    fn lambda(&mut self, param: Var, body: &Term, env: &[Name]) -> (Vec<Name>, Block) {
        let mut want = self.wants(&[body], &[param]);
        want.remove(&self.ev);
        let captures = restrict(env, &want);
        let ik = self.function_return();
        let iev = self.fresh_ref();
        let mut params = captures.clone();
        params.push(param);
        params.push(ik);
        params.push(iev);
        let outer = std::mem::replace(&mut self.ev, iev);
        let inner = self.expr(body, &params, ik);
        self.ev = outer;
        (
            captures,
            Block {
                params,
                body: inner,
            },
        )
    }

    /// What must stay live across the evaluation of something: whatever `terms`
    /// still need, plus `extra` (the continuation, values already computed).
    fn keep(&self, env: &[Name], terms: &[&Term], extra: &[Name]) -> Vec<Name> {
        let mut want = self.wants(terms, &[]);
        want.extend(extra.iter().copied());
        restrict(env, &want)
    }

    // --- the universal shapes --------------------------------------------

    /// `return v to k` — arrange the environment as exactly `[k, v]`, then
    /// invoke.
    ///
    /// The invoke carries no arguments. `k`'s method sees its own captures
    /// followed by whatever the substitution left behind it, which is `[v]` —
    /// which is why every continuation built by [`Lower::bind`] has parameters
    /// `captures ++ [x]`.
    fn ret(&mut self, k: Name, v: Name) -> Statement {
        Statement::Substitute(
            vec![k, v],
            Box::new(Block {
                params: vec![k, v],
                body: Statement::Invoke(k, 0),
            }),
        )
    }

    /// `invoke f` with nothing to hand it — how a failure continuation is
    /// entered, and the smallest possible use of the whole calling convention.
    fn enter(&mut self, f: Name) -> Statement {
        Statement::Substitute(
            vec![f],
            Box::new(Block {
                params: vec![f],
                body: Statement::Invoke(f, 0),
            }),
        )
    }

    /// An `extern` that produces one value, binds it, and continues.
    fn produces(
        &mut self,
        op: Extern,
        args: Vec<Name>,
        env: &[Name],
        ty: Option<core::Ty>,
        f: impl FnOnce(&mut Self, Name, Vec<Name>) -> Statement,
    ) -> Statement {
        self.produces_as(op, args, env, ty, None, f)
    }

    /// The same, with the bound name forced — for a `let` whose right-hand side
    /// needs no continuation.
    fn produces_as(
        &mut self,
        op: Extern,
        args: Vec<Name>,
        env: &[Name],
        ty: Option<core::Ty>,
        name: Option<Name>,
        f: impl FnOnce(&mut Self, Name, Vec<Name>) -> Statement,
    ) -> Statement {
        let x = match name {
            Some(n) => n,
            None => self.fresh_typed(ty),
        };
        let mut params = vec![x];
        params.extend_from_slice(env);
        let body = f(self, x, params.clone());
        Statement::Extern {
            op,
            args,
            blocks: vec![Block { params, body }],
        }
    }

    /// Can `e` produce a value without transferring control?
    ///
    /// If it can, whatever comes next is simply the `rest` of the statement that
    /// produced it, and [`Lower::bind`] needs no continuation object at all.
    /// That is the difference between an allocation and an indirect jump per
    /// comparison, per argument, per operand — which was most of what the
    /// machine did — and one `extern`.
    ///
    /// A variable is only simple when it is *in scope*: a reference to a
    /// top-level definition is a `jump`, which is exactly a transfer of control.
    /// `if` is excluded for a different reason. It does not transfer control
    /// away, but each of its branches would need its own copy of everything that
    /// follows, and duplicating the rest of a function per condition is how a
    /// compiler runs out of memory on real code.
    fn simple(&self, e: &Term, env: &[Name]) -> bool {
        match e {
            Term::Loc(_, inner) => self.simple(inner, env),
            Term::Var(v) => env.contains(v),
            Term::Lit(_) => true,
            // Building a closure allocates, but it does not go anywhere.
            // Erased before this pass runs.
            Term::TyLam(..) | Term::TyApp(..) => false,
            Term::Lam(..) => true,
            Term::Prim(_, xs, _) | Term::Ctor(_, _, xs) | Term::Tuple(xs) | Term::Array(xs, _) => {
                xs.iter().all(|x| self.simple(x, env))
            }
            Term::Record(fs) => fs.iter().all(|(_, t)| self.simple(t, env)),
            Term::Sel(t, _, _) | Term::Proj(t, _) => self.simple(t, env),
            Term::Extend(t, _, v) => self.simple(t, env) && self.simple(v, env),
            Term::Let(x, _, rhs, body) => {
                let mut inner = env.to_vec();
                inner.push(*x);
                self.simple(rhs, env) && self.simple(body, &inner)
            }
            _ => false,
        }
    }

    /// Lower a [`Lower::simple`] expression in place and continue.
    ///
    /// `f` receives the name its value was bound to and the environment that now
    /// holds — the whole of the old one plus the new binding, since nothing here
    /// truncates.
    fn direct(&mut self, e: &Term, env: &[Name], name: Option<Name>, f: Then<'_>) -> Statement {
        let ty = self.type_of(e);
        match e {
            Term::TyLam(_, inner) | Term::TyApp(inner, _) => self.direct(inner, env, name, f),
            Term::Loc(loc, inner) => {
                Statement::Mark(*loc, Box::new(self.direct(inner, env, name, f)))
            }
            // Already a value. With no name forced there is nothing at all to
            // emit; with one, a `substitute` gives it its second name.
            Term::Var(v) => match name {
                None => f(self, *v, env.to_vec()),
                Some(x) => {
                    let mut sel = env.to_vec();
                    sel.push(*v);
                    let mut params = env.to_vec();
                    params.push(x);
                    let body = f(self, x, params.clone());
                    Statement::Substitute(sel, Box::new(Block { params, body }))
                }
            },

            Term::Lit(l) => self.produces_as(Extern::Lit(l.clone()), vec![], env, ty, name, f),

            Term::Lam(param, _, body) => {
                let (captures, method) = self.lambda(*param, body, env);
                let x = name.unwrap_or_else(|| self.fresh_ref());
                let mut after: Vec<Name> = vec![x];
                after.extend_from_slice(env);
                let rest = f(self, x, after);
                Statement::New {
                    name: x,
                    captures,
                    methods: vec![method],
                    rest: Box::new(rest),
                }
            }

            Term::Prim(p, args, _) => {
                let p = *p;
                // A literal operand rides along inside the `extern`, so it never
                // becomes a name and never occupies a register.
                if let Some((x, l)) = const_operand(p, args) {
                    return self.direct_all(&[x], env.to_vec(), move |this, xs, env1| {
                        this.produces_as(Extern::PrimK(p, l), xs, &env1, ty, name, f)
                    });
                }
                self.direct_all(args, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Prim(p), xs, &env1, ty, name, f)
                })
            }

            Term::Ctor(ctor, _, args) => {
                let ctor = *ctor;
                let tag = self.tag_of(ctor);
                self.direct_all(args, env.to_vec(), move |this, fields, env1| {
                    this.builds(ctor, tag, fields, &env1, ty, name, f)
                })
            }

            Term::Tuple(items) => {
                let ctor = InternedString::from("#tuple");
                let tag = self.tag_of(ctor);
                self.direct_all(items, env.to_vec(), move |this, fields, env1| {
                    this.builds(ctor, tag, fields, &env1, ty, name, f)
                })
            }

            Term::Array(items, _) => self.direct_all(items, env.to_vec(), move |this, xs, env1| {
                this.produces_as(Extern::Array, xs, &env1, ty, name, f)
            }),

            Term::Record(fields) => {
                let labels: Vec<InternedString> = fields.iter().map(|(n, _)| *n).collect();
                let terms: Vec<Term> = fields.iter().map(|(_, t)| t.clone()).collect();
                self.direct_all(&terms, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Record(labels), xs, &env1, ty, name, f)
                })
            }

            Term::Sel(rec, label, _) => {
                let label = *label;
                self.direct(
                    rec,
                    env,
                    None,
                    Box::new(move |this, r, env1| {
                        this.produces_as(Extern::Select(label), vec![r], &env1, ty, name, f)
                    }),
                )
            }

            Term::Proj(t, i) => {
                let i = *i;
                self.direct(
                    t,
                    env,
                    None,
                    Box::new(move |this, x, env1| {
                        this.produces_as(Extern::Field(i), vec![x], &env1, ty, name, f)
                    }),
                )
            }

            Term::Extend(rec, label, val) => {
                let label = *label;
                let terms = vec![(**rec).clone(), (**val).clone()];
                self.direct_all(&terms, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Extend(label), xs, &env1, ty, name, f)
                })
            }

            Term::Let(x, _, rhs, body) => {
                let x = *x;
                self.direct(
                    rhs,
                    env,
                    Some(x),
                    Box::new(move |this, _x, env1| this.direct(body, &env1, name, f)),
                )
            }

            other => unreachable!("direct on something that transfers control: {other:?}"),
        }
    }

    /// `let x = K(fields); rest` — the data half of [`Lower::produces_as`].
    #[allow(clippy::too_many_arguments)]
    fn builds(
        &mut self,
        ctor: InternedString,
        tag: Tag,
        fields: Vec<Name>,
        env: &[Name],
        ty: Option<core::Ty>,
        name: Option<Name>,
        f: Then<'_>,
    ) -> Statement {
        let x = match name {
            Some(n) => n,
            None => self.fresh_typed(ty),
        };
        let mut after: Vec<Name> = vec![x];
        after.extend_from_slice(env);
        let rest = f(self, x, after);
        Statement::Let {
            name: x,
            tag,
            ctor,
            fields,
            rest: Box::new(rest),
        }
    }

    /// Several simple terms, left to right.
    fn direct_all(
        &mut self,
        es: &[Term],
        env: Vec<Name>,
        f: impl FnOnce(&mut Lower, Vec<Name>, Vec<Name>) -> Statement,
    ) -> Statement {
        fn go(
            this: &mut Lower,
            es: &[Term],
            env: Vec<Name>,
            done: Vec<Name>,
            f: Box<dyn FnOnce(&mut Lower, Vec<Name>, Vec<Name>) -> Statement + '_>,
        ) -> Statement {
            match es.split_first() {
                None => f(this, done, env),
                Some((head, rest)) => this.direct(
                    head,
                    &env,
                    None,
                    Box::new(move |this, x, env1| {
                        let mut done = done;
                        done.push(x);
                        go(this, rest, env1, done, f)
                    }),
                ),
            }
        }
        go(self, es, env, Vec::new(), Box::new(f))
    }

    /// Evaluate `e`, bind its value, and continue.
    ///
    /// `env` is the environment right now; `keep` is the subset of it that must
    /// still be live once `e` has produced a value — everything the continuation
    /// will refer to, including the outer continuation itself. Getting `keep`
    /// wrong is the one way to build an ill-formed program here, which is why
    /// every caller computes it from free variables rather than by hand.
    ///
    /// `name` forces the bound variable's name, for `let`.
    fn bind(
        &mut self,
        e: &Term,
        env: &[Name],
        keep: &[Name],
        name: Option<Name>,
        f: Then<'_>,
    ) -> Statement {
        // Anything that cannot transfer control is lowered where it stands, and
        // `keep` is not needed there: nothing is truncated, so everything that
        // was live still is.
        if self.simple(e, env) {
            return self.direct(e, env, name, f);
        }
        let kk = self.fresh_ref();
        let x = match name {
            Some(n) => n,
            None => {
                let ty = self.type_of(e);
                self.fresh_typed(ty)
            }
        };

        // On entry to the method: captures, then what the `substitute` in `ret`
        // left after the object itself — the single returned value.
        let mut inner: Vec<Name> = keep.to_vec();
        inner.push(x);
        let body = f(self, x, inner.clone());

        // `new` prepends the object to the environment.
        let mut outer: Vec<Name> = vec![kk];
        outer.extend_from_slice(env);
        let rest = self.expr(e, &outer, kk);

        Statement::New {
            name: kk,
            captures: keep.to_vec(),
            methods: vec![Block {
                params: inner,
                body,
            }],
            rest: Box::new(rest),
        }
    }

    /// Several terms, left to right — the order a strict language promises, made
    /// explicit rather than left to an evaluator.
    ///
    /// At each step the names that must survive are the caller's `base`, the
    /// values already computed, and the free variables of the terms still to
    /// come.
    fn bind_all(
        &mut self,
        es: &[Term],
        env: &[Name],
        base: &HashSet<Var>,
        f: Box<dyn FnOnce(&mut Lower, Vec<Name>, Vec<Name>) -> Statement + '_>,
    ) -> Statement {
        fn go(
            this: &mut Lower,
            es: &[Term],
            env: Vec<Name>,
            base: &HashSet<Var>,
            done: Vec<Name>,
            f: Box<dyn FnOnce(&mut Lower, Vec<Name>, Vec<Name>) -> Statement + '_>,
        ) -> Statement {
            match es.split_first() {
                None => f(this, done, env),
                Some((head, rest)) => {
                    let mut want = base.clone();
                    want.extend(done.iter().copied());
                    let rest_refs: Vec<&Term> = rest.iter().collect();
                    want.extend(this.wants(&rest_refs, &[]));
                    let keep = restrict(&env, &want);
                    this.bind(
                        head,
                        &env,
                        &keep,
                        None,
                        Box::new(move |this, x, env1| {
                            let mut done = done;
                            done.push(x);
                            go(this, rest, env1, base, done, f)
                        }),
                    )
                }
            }
        }
        go(self, es, env.to_vec(), base, Vec::new(), f)
    }

    // --- expressions ------------------------------------------------------

    /// `⟦e⟧ k` in environment `env` — run `e`, answer `k`.
    ///
    /// `env` must be the exact environment the machine will have, in order, and
    /// must contain `k`.
    fn expr(&mut self, e: &Term, env: &[Name], k: Name) -> Statement {
        self.continuations.insert(k);
        match e {
            Term::Loc(loc, inner) => Statement::Mark(*loc, Box::new(self.expr(inner, env, k))),
            // A type abstraction takes nothing at run time, and an instantiation
            // passes nothing; the types they carry were read where they mattered.
            Term::TyLam(_, inner) | Term::TyApp(inner, _) => self.expr(inner, env, k),
            Term::Var(v) if env.contains(v) => self.ret(k, *v),

            // A label: arrange exactly what its block takes and jump. `jump`
            // leaves the environment alone, so the substitution is the entire
            // calling sequence.
            Term::Var(v) => match self.globals.get(v) {
                Some((label, extra)) => {
                    let label = *label;
                    let mut sel = extra.clone();
                    sel.push(k);
                    if self.letrecs.contains(v) {
                        sel.push(self.ev);
                    }
                    Statement::Substitute(
                        sel.clone(),
                        Box::new(Block {
                            params: sel,
                            body: Statement::Jump(label),
                        }),
                    )
                }
                None => {
                    self.unsupported.insert(Unsupported::UnboundVar);
                    Statement::Error("unbound variable")
                }
            },

            Term::Lit(l) => {
                let ty = Some(lit_type(l));
                self.produces(Extern::Lit(l.clone()), vec![], env, ty, |this, x, _| {
                    this.ret(k, x)
                })
            }

            // Codata with one method: the argument, and where to send the answer.
            Term::Lam(param, _, body) => {
                let (captures, method) = self.lambda(*param, body, env);
                let f = self.fresh_ref();
                let rest = self.ret(k, f);
                Statement::New {
                    name: f,
                    captures,
                    methods: vec![method],
                    rest: Box::new(rest),
                }
            }

            // A saturated call to a known function is a jump. The arguments go
            // where its block wants them and control leaves — no closure per
            // argument, no continuation to receive one. This is what makes a
            // loop a loop rather than a sequence of allocations.
            Term::App(..) if self.direct_call(e).is_some() => {
                let (worker, args) = self.direct_call(e).expect("checked");
                let (f, _) = self.direct_worker(e).expect("checked");
                let args: Vec<Term> = args.into_iter().cloned().collect();
                let ev = self.worker_ev.contains(&f).then_some(self.ev);
                let base: HashSet<Var> = [k].into_iter().chain(ev).collect();
                self.bind_all(
                    &args,
                    env,
                    &base,
                    Box::new(move |_this, names, _env| {
                        let mut sel = names;
                        sel.push(k);
                        sel.extend(ev);
                        Statement::Substitute(
                            sel.clone(),
                            Box::new(Block {
                                params: sel,
                                body: Statement::Jump(worker),
                            }),
                        )
                    }),
                )
            }

            // Calling is arranging `[f, arg, k]` and invoking: the object drops
            // off the front and its method sees `captures ++ [arg, k]`.
            Term::App(fun, arg) => {
                let ev = self.ev;
                let keep = self.keep(env, &[arg], &[k, ev]);
                self.bind(
                    fun,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, fv, env1| {
                        let want: HashSet<Var> = [k, fv, ev].into_iter().collect();
                        let keep = restrict(&env1, &want);
                        this.bind(
                            arg,
                            &env1,
                            &keep,
                            None,
                            Box::new(move |_this, av, _env2| {
                                Statement::Substitute(
                                    vec![fv, av, k, ev],
                                    Box::new(Block {
                                        params: vec![fv, av, k, ev],
                                        body: Statement::Invoke(fv, 0),
                                    }),
                                )
                            }),
                        )
                    }),
                )
            }

            Term::Let(x, _, rhs, body) => {
                let mut want = self.wants(&[body], &[*x]);
                want.insert(k);
                let keep = restrict(env, &want);
                self.bind(
                    rhs,
                    env,
                    &keep,
                    Some(*x),
                    Box::new(move |this, _x, env1| this.expr(body, &env1, k)),
                )
            }

            // Lambda lifting: one label per binding, all sharing the group's
            // captured environment. See the module docs.
            Term::LetRec(binds, body) => {
                let bound: Vec<Var> = binds.iter().map(|(v, _, _)| *v).collect();
                let rhs: Vec<&Term> = binds.iter().map(|(_, _, t)| t).collect();
                // Each binding's block takes the evidence of whoever refers to
                // it, so it is not captured.
                let mut want = self.wants(&rhs, &bound);
                want.remove(&self.ev);
                let fvs = restrict(env, &want);

                // Register every label before lowering any right-hand side, so
                // the group can refer to itself in any direction.
                let labels: Vec<Label> = binds.iter().map(|_| self.fresh_label()).collect();
                for (v, l) in bound.iter().zip(&labels) {
                    self.globals.insert(*v, (*l, fvs.clone()));
                    self.letrecs.insert(*v);
                }

                for ((_, _, term), label) in binds.iter().zip(&labels) {
                    let kk = self.function_return();
                    let iev = self.fresh_ref();
                    let mut params = fvs.clone();
                    params.push(kk);
                    params.push(iev);
                    let outer = std::mem::replace(&mut self.ev, iev);
                    let body = self.expr(term, &params, kk);
                    self.ev = outer;
                    self.defs.push(Def {
                        label: *label,
                        name: InternedString::from("<letrec>"),
                        block: Block { params, body },
                    });
                }

                self.expr(body, env, k)
            }

            // Not a statement of its own: a branching primitive with two
            // continuation blocks is all `if` ever was. Neither branch changes
            // the environment, so both blocks take it as it stands.
            Term::If(c, t, e) => {
                let keep = self.keep(env, &[t, e], &[k]);

                // `if x < y` is one test, not a comparison whose result is
                // bound, immediately tested, and then never looked at again.
                // Fusing is only available when the condition cannot itself
                // transfer control — otherwise its operands are not values yet —
                // which is exactly the case `bind` would have handled without a
                // continuation anyway, so nothing else changes.
                if let Term::Prim(p, cargs, _) = &**c {
                    let p = *p;
                    if p.compares() && self.simple(c, env) {
                        let (op, operands) = match const_operand(p, cargs) {
                            Some((x, l)) => (Extern::BranchPrimK(p, l), vec![x]),
                            None => (Extern::BranchPrim(p), cargs.to_vec()),
                        };
                        return self.direct_all(&operands, env.to_vec(), move |this, xs, env1| {
                            let then = this.expr(t, &env1, k);
                            let els = this.expr(e, &env1, k);
                            Statement::Extern {
                                op,
                                args: xs,
                                blocks: vec![
                                    Block {
                                        params: env1.clone(),
                                        body: els,
                                    },
                                    Block {
                                        params: env1,
                                        body: then,
                                    },
                                ],
                            }
                        });
                    }
                }

                self.bind(
                    c,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, cv, env1| {
                        let then = this.expr(t, &env1, k);
                        let els = this.expr(e, &env1, k);
                        Statement::Extern {
                            op: Extern::Branch,
                            args: vec![cv],
                            blocks: vec![
                                Block {
                                    params: env1.clone(),
                                    body: els,
                                },
                                Block {
                                    params: env1,
                                    body: then,
                                },
                            ],
                        }
                    }),
                )
            }

            Term::Prim(prim, args, ty) => {
                let prim = *prim;
                let ty = Some(ty.clone());
                if let Some((x, l)) = const_operand(prim, args) {
                    return self.sequence(&[x], env, k, move |this, names, env1| {
                        this.produces(Extern::PrimK(prim, l), names, &env1, ty, |this, out, _| {
                            this.ret(k, out)
                        })
                    });
                }
                self.sequence(args, env, k, move |this, names, env1| {
                    this.produces(Extern::Prim(prim), names, &env1, ty, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            Term::Ctor(name, ty, args) => {
                let ctor = *name;
                let tag = self.tag_of(ctor);
                let ty = Some(ty.clone());
                self.sequence(args, env, k, move |this, fields, _| {
                    let x = this.fresh_typed(ty);
                    let rest = this.ret(k, x);
                    Statement::Let {
                        name: x,
                        tag,
                        ctor,
                        fields,
                        rest: Box::new(rest),
                    }
                })
            }

            Term::Tuple(items) => {
                let ctor = InternedString::from("#tuple");
                let tag = self.tag_of(ctor);
                let ty = self.type_of(e);
                self.sequence(items, env, k, move |this, fields, _| {
                    let x = this.fresh_typed(ty);
                    let rest = this.ret(k, x);
                    Statement::Let {
                        name: x,
                        tag,
                        ctor,
                        fields,
                        rest: Box::new(rest),
                    }
                })
            }

            Term::Array(items, _) => {
                let ty = self.type_of(e);
                self.sequence(items, env, k, move |this, xs, env1| {
                    this.produces(Extern::Array, xs, &env1, ty, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            Term::Record(fields) => {
                let labels: Vec<InternedString> = fields.iter().map(|(n, _)| *n).collect();
                let terms: Vec<Term> = fields.iter().map(|(_, t)| t.clone()).collect();
                let ty = self.type_of(e);
                self.sequence(&terms, env, k, move |this, xs, env1| {
                    this.produces(Extern::Record(labels), xs, &env1, ty, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            Term::Sel(rec, label, ty) => {
                let label = *label;
                let ty = Some(ty.clone());
                let keep = restrict(env, &[k].into_iter().collect());
                self.bind(
                    rec,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, r, env1| {
                        this.produces(Extern::Select(label), vec![r], &env1, ty, |this, out, _| {
                            this.ret(k, out)
                        })
                    }),
                )
            }

            Term::Extend(rec, label, val) => {
                let label = *label;
                let terms = vec![(**rec).clone(), (**val).clone()];
                let ty = self.type_of(e);
                self.sequence(&terms, env, k, move |this, xs, env1| {
                    this.produces(Extern::Extend(label), xs, &env1, ty, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            // Tuple projection cannot be a `switch`: the arity a `switch` arm
            // would have to name is not in the term.
            Term::Proj(t, i) => {
                let i = *i;
                let ty = self.type_of(e);
                let keep = restrict(env, &[k].into_iter().collect());
                self.bind(
                    t,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, x, env1| {
                        this.produces(Extern::Field(i), vec![x], &env1, ty, |this, out, _| {
                            this.ret(k, out)
                        })
                    }),
                )
            }

            Term::Case(scrutinee, arms, _) => {
                let mut want = HashSet::new();
                for (p, body) in arms {
                    let mut bound = Vec::new();
                    core::pat_vars(p, &mut bound);
                    want.extend(self.wants(&[body], &bound));
                }
                want.insert(k);
                let keep = restrict(env, &want);
                self.bind(
                    scrutinee,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, s, env1| this.case(s, arms, env1, k)),
                )
            }

            Term::Perform(effect, op, arg, ty) => {
                let (effect, op) = (*effect, *op);
                let tys = (self.type_of(arg), Some(ty.clone()));
                let ev = self.ev;
                let keep = restrict(env, &[k, ev].into_iter().collect());
                self.bind(
                    arg,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, av, _env1| this.perform(effect, op, av, k, ev, tys)),
                )
            }

            Term::Handle {
                body, clauses, ret, ..
            } => self.handle(body, clauses, ret.as_ref(), env, k),

            // A term the front end could not build. Lowering it to a statement
            // that fails is the faithful translation, not a gap.
            Term::Error => Statement::Error("ill-formed term"),
        }
    }

    /// Is this a saturated call to a definition with a direct entry point?
    ///
    /// Exact arity only. An under-applied call has to build a closure — that is
    /// what a partial application *is* — and an over-applied one returns
    /// something that is then called again, which the general path already
    /// handles correctly.
    fn direct_call<'t>(&self, e: &'t Term) -> Option<(Label, Vec<&'t Term>)> {
        let (f, args) = self.direct_worker(e)?;
        Some((self.workers[&f].0, args))
    }

    /// The same, answering which definition rather than where it starts.
    fn direct_worker<'t>(&self, e: &'t Term) -> Option<(Var, Vec<&'t Term>)> {
        let (head, args) = call_spine(e);
        let Term::Var(v) = head else { return None };
        let &(_, arity) = self.workers.get(v)?;
        (arity == args.len()).then_some((*v, args))
    }
    /// Evaluate a list of terms left to right, keeping `k` alive throughout.
    fn sequence(
        &mut self,
        terms: &[Term],
        env: &[Name],
        k: Name,
        f: impl FnOnce(&mut Lower, Vec<Name>, Vec<Name>) -> Statement,
    ) -> Statement {
        let base: HashSet<Var> = [k].into_iter().collect();
        self.bind_all(terms, env, &base, Box::new(f))
    }

    // --- pattern matching -------------------------------------------------

    /// A chain of failure continuations, one per arm.
    ///
    /// `f_i` captures everything live plus `f_{i+1}`, so an arm that fails
    /// half-way through a nested pattern can restore the whole environment by
    /// invoking one object — including the scrutinee, which a `switch` in the
    /// middle of the arm will have consumed.
    fn case(&mut self, s: Name, arms: &[(Pat, Term)], live: Vec<Name>, k: Name) -> Statement {
        if let Some(tree) = self
            .opt
            .case_trees()
            .then(|| self.case_tree(s, arms, &live, k))
            .flatten()
        {
            return tree;
        }
        self.case_chain(s, arms, live, k)
    }

    /// One `switch` over the arms that dispatch on a constructor, instead of one
    /// per arm.
    ///
    /// The chain below tests `Cons`, then — having failed — tests `Nil`, then
    /// tests whatever is next, and builds a failure object before any of it. A
    /// `match` over a wide type does that work per arm even though a value has
    /// exactly one tag. This takes the longest **prefix** of arms that are
    /// constructor patterns on distinct constructors and gives them one `switch`
    /// with one shared fallback, which is the whole of the common case; anything
    /// the prefix does not cover — a wildcard, a literal, a repeated constructor
    /// — is the fallback, compiled as a chain exactly as before, so arm order is
    /// preserved without having to reason about it.
    ///
    /// Returns `None` when there is nothing to gain: fewer than two arms would
    /// join the switch.
    ///
    /// This is the trade [`OptLevel::case_trees`] gates. Nested patterns are not
    /// distributed across the arms — `Just (Cons x xs)` still falls back to the
    /// chain when its inner pattern fails, and so retests the outer `Just` —
    /// because that is where a real decision tree starts duplicating the code
    /// its arms share.
    fn case_tree(
        &mut self,
        s: Name,
        arms: &[(Pat, Term)],
        live: &[Name],
        k: Name,
    ) -> Option<Statement> {
        let mut seen: HashSet<InternedString> = HashSet::new();
        let n = arms
            .iter()
            .take_while(|(p, _)| match p {
                Pat::Ctor(name, _) => seen.insert(*name),
                _ => false,
            })
            .count();
        if n < 2 {
            return None;
        }

        // One object for "none of these matched", shared by every arm — both by
        // the `default`, and by an arm whose *sub*-patterns fail after its tag
        // matched. It captures the environment as it stands, which includes the
        // scrutinee: the arms that follow still have to look at it.
        // `new` *prepends*, so this is the environment the switch runs in — the
        // order matters, and getting it wrong is an ill-formed program rather
        // than a slow one.
        let rest = self.fresh_ref();
        let scrutinee_ty = self.type_of_name(s);
        let mut env = vec![rest];
        env.extend_from_slice(live);

        let mut switch_arms = Vec::with_capacity(n);
        for (pat, term) in &arms[..n] {
            let Pat::Ctor(ctor, subs) = pat else {
                unreachable!("the prefix is constructor patterns");
            };
            let tag = self.tag_of(*ctor);
            let fields = self.field_names(*ctor, subs.len(), scrutinee_ty.as_ref());
            let mut arm_env = fields.clone();
            arm_env.extend_from_slice(&env);
            let pairs: Vec<(&Pat, Name)> = subs.iter().zip(fields.iter().copied()).collect();
            let body = self.match_all(
                pairs,
                arm_env.clone(),
                rest,
                Box::new(move |this, env1| this.expr(term, &env1, k)),
            );
            switch_arms.push((
                tag,
                Block {
                    params: arm_env,
                    body,
                },
            ));
        }

        let miss = self.enter(rest);
        let switch = Statement::Switch {
            scrutinee: s,
            arms: switch_arms,
            default: Box::new(Block {
                params: env,
                body: miss,
            }),
        };

        let body = if n == arms.len() {
            Statement::Error("non-exhaustive pattern match")
        } else {
            self.case(s, &arms[n..], live.to_vec(), k)
        };
        Some(Statement::New {
            name: rest,
            captures: live.to_vec(),
            methods: vec![Block {
                params: live.to_vec(),
                body,
            }],
            rest: Box::new(switch),
        })
    }

    fn case_chain<'t>(
        &mut self,
        s: Name,
        arms: &'t [(Pat, Term)],
        live: Vec<Name>,
        k: Name,
    ) -> Statement {
        let n = arms.len();
        let fails: Vec<Name> = (0..=n).map(|_| self.fresh_ref()).collect();

        // The chain is entered by invoking the first arm's object.
        let mut stmt = self.enter(fails[0]);

        // Wrap innermost-first, so `f_n` — the one nobody captures — ends up
        // outermost and is therefore built first at run time.
        for i in 0..=n {
            let (captures, body) = if i == n {
                (
                    live.clone(),
                    Statement::Error("non-exhaustive pattern match"),
                )
            } else {
                let fail = fails[i + 1];
                let mut caps = live.clone();
                caps.push(fail);
                let (pat, term) = &arms[i];
                let body = self.match_pat(
                    pat,
                    s,
                    caps.clone(),
                    fail,
                    Box::new(move |this, env| this.expr(term, &env, k)),
                );
                (caps, body)
            };
            stmt = Statement::New {
                name: fails[i],
                captures: captures.clone(),
                methods: vec![Block {
                    params: captures,
                    body,
                }],
                rest: Box::new(stmt),
            };
        }
        stmt
    }

    /// Match `p` against `subject`; on success run `ok`, on failure invoke
    /// `fail`.
    fn match_pat<'t>(
        &mut self,
        p: &'t Pat,
        subject: Name,
        env: Vec<Name>,
        fail: Name,
        ok: Success<'t>,
    ) -> Statement {
        match p {
            Pat::Wild => ok(self, env),

            // Binding is a rename: put the value at the end of the environment
            // under the pattern's name. `VarId`s are unique per binding site, so
            // this can never collide with something already there.
            Pat::Var(v, _) => self.rebind(subject, *v, env, ok),

            Pat::As(v, _, sub) => {
                let v = *v;
                self.rebind(
                    subject,
                    v,
                    env,
                    Box::new(move |this, env1| this.match_pat(sub, subject, env1, fail, ok)),
                )
            }

            // One statement: the literal rides in the `extern`, and the boolean
            // it is compared against never exists. This used to be three — load
            // the literal, compare, test — and it is the commonest pattern
            // there is.
            Pat::Lit(l) => {
                let no = self.enter(fail);
                let yes = ok(self, env.clone());
                Statement::Extern {
                    op: Extern::BranchPrimK(core::Prim::Eq, l.clone()),
                    args: vec![subject],
                    blocks: vec![
                        Block {
                            params: env.clone(),
                            body: no,
                        },
                        Block {
                            params: env,
                            body: yes,
                        },
                    ],
                }
            }

            // The one place a real `switch` appears. The constructor's fields go
            // on the front; the scrutinee stays, because an arm body may name it
            // — `match o with | Just x -> o` is ordinary.
            Pat::Ctor(name, subs) => {
                let tag = self.tag_of(*name);
                let of = self.type_of_name(subject);
                let fields = self.field_names(*name, subs.len(), of.as_ref());
                let mut arm_env = fields.clone();
                arm_env.extend_from_slice(&env);
                let pairs: Vec<(&Pat, Name)> = subs.iter().zip(fields.iter().copied()).collect();
                let body = self.match_all(pairs, arm_env.clone(), fail, ok);
                let miss = self.enter(fail);
                Statement::Switch {
                    scrutinee: subject,
                    arms: vec![(
                        tag,
                        Block {
                            params: arm_env,
                            body,
                        },
                    )],
                    default: Box::new(Block {
                        params: env,
                        body: miss,
                    }),
                }
            }

            Pat::Tuple(subs) => self.fields(subs, subject, env, fail, ok),

            Pat::Array(subs) => {
                // `#[p, …]` matches an array of exactly this length, so the
                // length test comes first and the extractions follow.
                let want = subs.len() as i64;
                self.produces(
                    Extern::Prim(core::Prim::ArrayLen),
                    vec![subject],
                    &env,
                    Some(con("Int")),
                    move |this, n, env1| {
                        let no = this.enter(fail);
                        let yes = this.fields(subs, subject, env1.clone(), fail, ok);
                        Statement::Extern {
                            op: Extern::BranchPrimK(core::Prim::Eq, core::Lit::Int(want)),
                            args: vec![n],
                            blocks: vec![
                                Block {
                                    params: env1.clone(),
                                    body: no,
                                },
                                Block {
                                    params: env1,
                                    body: yes,
                                },
                            ],
                        }
                    },
                )
            }

            // A record pattern names the fields it wants and ignores the rest,
            // which is what row polymorphism means here.
            Pat::Record(fields) => {
                fn go<'t>(
                    this: &mut Lower,
                    fields: &'t [(InternedString, Pat)],
                    subject: Name,
                    env: Vec<Name>,
                    fail: Name,
                    ok: Success<'t>,
                ) -> Statement {
                    match fields.split_first() {
                        None => ok(this, env),
                        Some(((label, p), rest)) => {
                            let of = this.type_of_name(subject);
                            let ty = Lower::label_type(of.as_ref(), *label);
                            this.produces(
                                Extern::Select(*label),
                                vec![subject],
                                &env,
                                ty,
                                |this, x, env1| {
                                    this.match_pat(
                                        p,
                                        x,
                                        env1,
                                        fail,
                                        Box::new(move |this, env2| {
                                            go(this, rest, subject, env2, fail, ok)
                                        }),
                                    )
                                },
                            )
                        }
                    }
                }
                go(self, fields, subject, env, fail, ok)
            }
        }
    }

    /// Extract `subs.len()` positional fields from `subject` and match each.
    fn fields<'t>(
        &mut self,
        subs: &'t [Pat],
        subject: Name,
        env: Vec<Name>,
        fail: Name,
        ok: Success<'t>,
    ) -> Statement {
        fn go<'t>(
            this: &mut Lower,
            subs: &'t [Pat],
            i: usize,
            subject: Name,
            env: Vec<Name>,
            got: Vec<(usize, Name)>,
            fail: Name,
            ok: Success<'t>,
        ) -> Statement {
            if i == subs.len() {
                let pairs: Vec<(&Pat, Name)> =
                    got.into_iter().map(|(j, n)| (&subs[j], n)).collect();
                return this.match_all(pairs, env, fail, ok);
            }
            let ty = match this.type_of_name(subject) {
                Some(core::Ty::Tuple(items)) => items.get(i).cloned(),
                Some(core::Ty::Con(_, args)) => args.first().cloned(),
                _ => None,
            };
            this.produces(
                Extern::Field(i),
                vec![subject],
                &env,
                ty,
                move |this, x, env1| {
                    let mut got = got;
                    got.push((i, x));
                    go(this, subs, i + 1, subject, env1, got, fail, ok)
                },
            )
        }
        go(self, subs, 0, subject, env, Vec::new(), fail, ok)
    }

    /// Match a list of patterns against a list of values, left to right.
    fn match_all<'t>(
        &mut self,
        pairs: Vec<(&'t Pat, Name)>,
        env: Vec<Name>,
        fail: Name,
        ok: Success<'t>,
    ) -> Statement {
        match pairs.split_first() {
            None => ok(self, env),
            Some(((p, x), rest)) => {
                let (p, x) = (*p, *x);
                let rest = rest.to_vec();
                self.match_pat(
                    p,
                    x,
                    env,
                    fail,
                    Box::new(move |this, env1| this.match_all(rest, env1, fail, ok)),
                )
            }
        }
    }

    /// Give `subject`'s value a second name at the end of the environment.
    fn rebind(
        &mut self,
        subject: Name,
        as_name: Name,
        env: Vec<Name>,
        ok: Success<'_>,
    ) -> Statement {
        let mut sel = env.clone();
        sel.push(subject);
        let mut params = env;
        params.push(as_name);
        let body = ok(self, params.clone());
        Statement::Substitute(sel, Box::new(Block { params, body }))
    }

    // --- effects ----------------------------------------------------------

    /// The evidence key of an operation.
    fn key(effect: InternedString, op: InternedString) -> InternedString {
        InternedString::from(format!("{effect}.{op}").as_str())
    }

    /// `perform effect.op av` answering `k`, with evidence `ev`.
    ///
    /// Each `perform` gets a block of its own that walks the evidence, lifted
    /// out the way a `letrec` binding is, with the argument and the
    /// continuation riding along: `[ev, av, k]`. Its key is a constant of the
    /// comparison, so the search allocates nothing and names nothing, and the
    /// innermost handler -- nearly always the one -- is one test away.
    fn perform(
        &mut self,
        effect: InternedString,
        op: InternedString,
        av: Name,
        k: Name,
        ev: Name,
        (arg_ty, res_ty): (Option<core::Ty>, Option<core::Ty>),
    ) -> Statement {
        let label = self.fresh_label();
        let (e, a, kk) = (self.fresh_ref(), self.fresh_typed(arg_ty), self.fresh_ref());
        let params = vec![e, a, kk];
        let key = core::Lit::Str(Self::key(effect, op));
        let mut arms = Vec::new();
        for (ctor, tail) in [(EV, false), (EV_TAIL, true)] {
            let ekey = self.fresh_as(Rep::Str);
            let (clause, target, rest) = (self.fresh_ref(), self.fresh_ref(), self.fresh_ref());
            let mut fields = vec![ekey, clause, target, rest];
            fields.extend_from_slice(&params);
            let hit = self.dispatch(tail, clause, target, a, kk, &fields, res_ty.clone());
            let next = Statement::Substitute(
                vec![rest, a, kk],
                Box::new(Block {
                    params: params.clone(),
                    body: Statement::Jump(label),
                }),
            );
            arms.push((
                self.tag_of(InternedString::from(ctor)),
                Block {
                    params: fields.clone(),
                    body: Statement::Extern {
                        op: Extern::BranchPrimK(core::Prim::Eq, key.clone()),
                        args: vec![ekey],
                        blocks: vec![
                            Block {
                                params: fields.clone(),
                                body: next,
                            },
                            Block {
                                params: fields,
                                body: hit,
                            },
                        ],
                    },
                },
            ));
        }
        let none = self.native(effect, op, a, kk, &params, res_ty);
        self.defs.push(Def {
            label,
            name: InternedString::from(format!("<perform {effect}.{op}>").as_str()),
            block: Block {
                params: params.clone(),
                body: Statement::Switch {
                    scrutinee: e,
                    arms,
                    default: Box::new(Block { params, body: none }),
                },
            },
        });
        Statement::Substitute(
            vec![ev, av, k],
            Box::new(Block {
                params: vec![ev, av, k],
                body: Statement::Jump(label),
            }),
        )
    }

    /// The operation is `clause`'s, in `env`. A tail-resumptive clause is a
    /// call answering `k` directly; any other gets a one-shot resumption.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &mut self,
        tail: bool,
        clause: Name,
        target: Name,
        av: Name,
        k: Name,
        env: &[Name],
        res_ty: Option<core::Ty>,
    ) -> Statement {
        if tail {
            return Statement::Substitute(
                vec![clause, av, k],
                Box::new(Block {
                    params: vec![clause, av, k],
                    body: Statement::Invoke(clause, 0),
                }),
            );
        }
        let flag = self.fresh_as(Rep::Bits);
        let taken = self.fresh_ref();
        let r = self.fresh_ref();
        let kh = self.fresh_ref();
        let mut env1 = vec![flag];
        env1.extend_from_slice(env);
        let mut env2 = vec![taken];
        env2.extend_from_slice(&env1);
        let mut env3 = vec![r];
        env3.extend_from_slice(&env2);
        let mut env4 = vec![kh];
        env4.extend_from_slice(&env3);
        let enter = Statement::Substitute(
            vec![clause, av, r, kh],
            Box::new(Block {
                params: vec![clause, av, r, kh],
                body: Statement::Invoke(clause, 0),
            }),
        );
        let resume = self.resumption(k, target, taken, res_ty);
        Statement::Extern {
            op: Extern::Lit(core::Lit::Unit),
            args: vec![],
            blocks: vec![Block {
                params: env1,
                body: Statement::Extern {
                    op: Extern::Prim(core::Prim::Once),
                    args: vec![flag],
                    blocks: vec![Block {
                        params: env2,
                        body: Statement::New {
                            name: r,
                            // The flag first: it is what a thread send or a
                            // `compact` meets first, and what makes it refuse
                            // this as a continuation.
                            captures: vec![taken, k, target],
                            methods: vec![resume],
                            rest: Box::new(Statement::Extern {
                                op: Extern::Prim(core::Prim::GetRef),
                                args: vec![target],
                                blocks: vec![Block {
                                    params: env4,
                                    body: enter,
                                }],
                            }),
                        },
                    }],
                },
            }],
        }
    }

    /// No handler in the program answers: the runtime does.
    fn native(
        &mut self,
        effect: InternedString,
        op: InternedString,
        av: Name,
        k: Name,
        env: &[Name],
        res_ty: Option<core::Ty>,
    ) -> Statement {
        self.produces(
            Extern::Native(effect, op),
            vec![av],
            env,
            res_ty,
            |this, out, _| this.ret(k, out),
        )
    }

    /// The method of a resumption capturing `[taken, k, target]`, called with a
    /// value, a continuation and evidence it has no use for: run once, send
    /// the handler's value to the caller from now on, and continue at `k`.
    fn resumption(&mut self, k: Name, target: Name, taken: Name, ty: Option<core::Ty>) -> Block {
        let (v, c, ev) = (self.fresh_typed(ty), self.fresh_ref(), self.fresh_ref());
        self.returns.insert(c);
        let params = vec![taken, k, target, v, c, ev];
        let first = self.fresh_as(Rep::Bits);
        let mut at_first = vec![first];
        at_first.extend_from_slice(&params);
        let u = self.fresh_as(Rep::Bits);
        let mut at_u = vec![u];
        at_u.extend_from_slice(&at_first);
        let go = self.ret(k, v);
        Block {
            params,
            body: Statement::Extern {
                op: Extern::Prim(core::Prim::TakeOnce),
                args: vec![taken],
                blocks: vec![Block {
                    params: at_first.clone(),
                    body: Statement::Extern {
                        op: Extern::Branch,
                        args: vec![first],
                        blocks: vec![
                            Block {
                                params: at_first.clone(),
                                body: Statement::Error("continuation resumed more than once"),
                            },
                            Block {
                                params: at_first,
                                body: Statement::Extern {
                                    op: Extern::Prim(core::Prim::SetRef),
                                    args: vec![target, c],
                                    blocks: vec![Block {
                                        params: at_u,
                                        body: go,
                                    }],
                                },
                            },
                        ],
                    },
                }],
            },
        }
    }

    /// `handle body with { … }` answering `k`, in `env`. See the module docs.
    fn handle(
        &mut self,
        body: &Term,
        clauses: &[core::HClause],
        ret: Option<&(Var, core::Ty, std::sync::Arc<Term>)>,
        env: &[Name],
        k: Name,
    ) -> Statement {
        let outer = self.ev;

        // Each step binds one name in front of the environment; the statements
        // are assembled afterwards, innermost first.
        enum Step {
            Target(Name),
            Clause(Name, Vec<Name>, Block),
            Key(Name, InternedString),
            Entry(Name, &'static str, Vec<Name>),
            Return(Name, Vec<Name>, Block),
        }
        let mut steps: Vec<(Step, Vec<Name>)> = Vec::new();
        let mut cur: Vec<Name> = env.to_vec();
        let bind = |cur: &mut Vec<Name>, n: Name| {
            let before = cur.clone();
            cur.insert(0, n);
            before
        };

        let target = self.fresh_ref();
        let before = bind(&mut cur, target);
        steps.push((Step::Target(target), before));

        let mut ev = outer;
        for c in clauses {
            // A tail-resumptive clause computes the value to resume with and
            // hands it straight to the performing code's continuation: `k e`
            // would have done exactly that, and then sent whatever came of it
            // where this clause's value goes, which is the same place.
            let tail = tail_resumptive(c);
            let (captures, params, clause_body) = match tail {
                Some(e) => {
                    let captures = restrict(&cur, &self.wants(&[e], &[c.param]));
                    let kh = self.fresh_ref();
                    self.continuations.insert(kh);
                    let mut params = captures.clone();
                    params.push(c.param);
                    params.push(kh);
                    let body = self.expr(e, &params, kh);
                    (captures, params, body)
                }
                None => {
                    let captures = restrict(&cur, &self.wants(&[&c.body], &[c.param, c.resume]));
                    let kh = self.function_return();
                    let mut params = captures.clone();
                    params.push(c.param);
                    params.push(c.resume);
                    params.push(kh);
                    let body = self.expr(&c.body, &params, kh);
                    (captures, params, body)
                }
            };
            let obj = self.fresh_ref();
            let before = bind(&mut cur, obj);
            steps.push((
                Step::Clause(
                    obj,
                    captures,
                    Block {
                        params,
                        body: clause_body,
                    },
                ),
                before,
            ));

            let key = self.fresh_as(Rep::Str);
            let before = bind(&mut cur, key);
            steps.push((Step::Key(key, Self::key(c.effect, c.op)), before));

            let entry = self.fresh_ref();
            let before = bind(&mut cur, entry);
            let ctor = if tail.is_some() { EV_TAIL } else { EV };
            steps.push((Step::Entry(entry, ctor, vec![key, obj, target, ev]), before));
            ev = entry;
        }

        // The body's continuation: wherever `target` says, through the
        // `return` clause, outside the handler.
        let h = self.fresh_ref();
        let (x, ret_body) = match ret {
            Some((p, _, t)) => (*p, Some(&**t)),
            None => {
                let ty = self.type_of(body);
                (self.fresh_typed(ty), None)
            }
        };
        let mut want = match ret_body {
            Some(t) => self.wants(&[t], &[x]),
            None => HashSet::new(),
        };
        want.insert(target);
        let caps = restrict(&cur, &want);
        let mut params = caps.clone();
        params.push(x);
        let kk = self.fresh_ref();
        let mut at_kk = vec![kk];
        at_kk.extend_from_slice(&params);
        let inner = match ret_body {
            Some(t) => self.expr(t, &at_kk, kk),
            None => self.ret(kk, x),
        };
        let h_method = Block {
            params: params.clone(),
            body: Statement::Extern {
                op: Extern::Prim(core::Prim::GetRef),
                args: vec![target],
                blocks: vec![Block {
                    params: at_kk,
                    body: inner,
                }],
            },
        };
        let before = bind(&mut cur, h);
        steps.push((Step::Return(h, caps, h_method), before));

        self.ev = ev;
        let mut stmt = self.expr(body, &cur, h);
        self.ev = outer;

        for (step, before) in steps.into_iter().rev() {
            let with = |n: Name| {
                let mut p = vec![n];
                p.extend_from_slice(&before);
                p
            };
            stmt = match step {
                Step::Target(n) => Statement::Extern {
                    op: Extern::Prim(core::Prim::NewRef),
                    args: vec![k],
                    blocks: vec![Block {
                        params: with(n),
                        body: stmt,
                    }],
                },
                Step::Clause(n, captures, method) | Step::Return(n, captures, method) => {
                    Statement::New {
                        name: n,
                        captures,
                        methods: vec![method],
                        rest: Box::new(stmt),
                    }
                }
                Step::Key(n, key) => Statement::Extern {
                    op: Extern::Lit(core::Lit::Str(key)),
                    args: vec![],
                    blocks: vec![Block {
                        params: with(n),
                        body: stmt,
                    }],
                },
                Step::Entry(n, ctor, fields) => Statement::Let {
                    name: n,
                    tag: self.tag_of(InternedString::from(ctor)),
                    ctor: InternedString::from(ctor),
                    fields,
                    rest: Box::new(stmt),
                },
            };
        }
        stmt
    }
}

/// Every variable `t` binds, with its type.
fn binders(t: &Term, out: &mut HashMap<Var, core::Poly>) {
    fn pat(p: &Pat, out: &mut HashMap<Var, core::Poly>) {
        match p {
            Pat::Wild | Pat::Lit(_) => {}
            Pat::Var(v, ty) => {
                out.insert(*v, core::Poly::mono(ty.clone()));
            }
            Pat::As(v, ty, sub) => {
                out.insert(*v, core::Poly::mono(ty.clone()));
                pat(sub, out);
            }
            Pat::Tuple(ps) | Pat::Array(ps) | Pat::Ctor(_, ps) => {
                ps.iter().for_each(|p| pat(p, out))
            }
            Pat::Record(fs) => fs.iter().for_each(|(_, p)| pat(p, out)),
        }
    }
    match t {
        Term::Var(_) | Term::Lit(_) | Term::Error => {}
        Term::Loc(_, b)
        | Term::TyLam(_, b)
        | Term::TyApp(b, _)
        | Term::Proj(b, _)
        | Term::Sel(b, _, _) => binders(b, out),
        Term::Lam(v, ty, b) => {
            out.insert(*v, core::Poly::mono(ty.clone()));
            binders(b, out);
        }
        Term::App(a, b) | Term::Extend(a, _, b) => {
            binders(a, out);
            binders(b, out);
        }
        Term::Let(v, poly, r, b) => {
            out.insert(*v, poly.clone());
            binders(r, out);
            binders(b, out);
        }
        Term::LetRec(binds, body) => {
            for (v, poly, t) in binds {
                out.insert(*v, poly.clone());
                binders(t, out);
            }
            binders(body, out);
        }
        Term::If(a, b, c) => {
            binders(a, out);
            binders(b, out);
            binders(c, out);
        }
        Term::Tuple(xs) | Term::Array(xs, _) | Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
            xs.iter().for_each(|x| binders(x, out))
        }
        Term::Record(fs) => fs.iter().for_each(|(_, x)| binders(x, out)),
        Term::Perform(_, _, a, _) => binders(a, out),
        Term::Case(s, arms, _) => {
            binders(s, out);
            for (p, b) in arms {
                pat(p, out);
                binders(b, out);
            }
        }
        Term::Handle {
            body, clauses, ret, ..
        } => {
            binders(body, out);
            for c in clauses {
                out.insert(c.param, core::Poly::mono(c.param_ty.clone()));
                // A resumption is a closure, whatever core managed to say its
                // type is.
                let resume = match &c.resume_ty {
                    ty @ core::Ty::Fun(..) => ty.clone(),
                    _ => core::Ty::Fun(
                        vec![c.param_ty.clone()],
                        Box::new(core::unknown()),
                        Box::new(core::Ty::RowEmpty),
                    ),
                };
                out.insert(c.resume, core::Poly::mono(resume));
                binders(&c.body, out);
            }
            if let Some((v, ty, b)) = ret {
                out.insert(*v, core::Poly::mono(ty.clone()));
                binders(b, out);
            }
        }
    }
}

/// A type with no parameters, by name.
fn con(name: &str) -> core::Ty {
    core::Ty::Con(InternedString::from(name), Vec::new())
}

/// The type of a literal.
fn lit_type(l: &core::Lit) -> core::Ty {
    use core::{Lit, Ty};
    let con = |n: &str| Ty::Con(InternedString::from(n), Vec::new());
    match l {
        Lit::Int(_) => con("Int"),
        Lit::BigInt(_) => con("BigInt"),
        Lit::Float(_) => con("Float"),
        Lit::Word(w, _) => con(w.name()),
        Lit::Float32(_) => con("Float32"),
        Lit::AnyInt(_, v) | Lit::AnyFloat(_, v) => Ty::Var(*v),
        Lit::Str(_) => con("String"),
        Lit::Char(_) => con("Char"),
        Lit::Bool(_) => con("Bool"),
        Lit::Unit => con("Unit"),
    }
}

/// The names of `env`, in order, that are in `want` — the environment shape a
/// continuation should capture.
///
/// Order comes from the environment rather than from the set, so lowering is
/// deterministic: a `HashSet`'s iteration order is not.
fn restrict(env: &[Name], want: &HashSet<Var>) -> Vec<Name> {
    let mut seen = HashSet::new();
    env.iter()
        .filter(|n| want.contains(n) && seen.insert(**n))
        .copied()
        .collect()
}

/// Every variable a term mentions, bound or free — only for sizing the fresh
/// counter, where the distinction does not matter.
fn mentions(t: &Term, out: &mut HashSet<Var>) {
    match t {
        Term::TyLam(_, b) | Term::TyApp(b, _) | Term::Loc(_, b) => mentions(b, out),
        Term::Var(v) => {
            out.insert(*v);
        }
        Term::Lit(_) | Term::Error => {}
        Term::Lam(p, _, b) => {
            out.insert(*p);
            mentions(b, out);
        }
        Term::App(f, a) => {
            mentions(f, out);
            mentions(a, out);
        }
        Term::Let(x, _, r, b) => {
            out.insert(*x);
            mentions(r, out);
            mentions(b, out);
        }
        Term::LetRec(binds, body) => {
            for (v, _, t) in binds {
                out.insert(*v);
                mentions(t, out);
            }
            mentions(body, out);
        }
        Term::If(a, b, c) => {
            mentions(a, out);
            mentions(b, out);
            mentions(c, out);
        }
        Term::Tuple(xs) | Term::Array(xs, _) => {
            for x in xs {
                mentions(x, out);
            }
        }
        Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
            for x in xs {
                mentions(x, out);
            }
        }
        Term::Proj(t, _) | Term::Sel(t, _, _) => mentions(t, out),
        Term::Extend(t, _, u) => {
            mentions(t, out);
            mentions(u, out);
        }
        Term::Record(fs) => {
            for (_, t) in fs {
                mentions(t, out);
            }
        }
        Term::Perform(_, _, a, _) => mentions(a, out),
        Term::Case(s, arms, _) => {
            mentions(s, out);
            for (p, t) in arms {
                let mut vs = Vec::new();
                core::pat_vars(p, &mut vs);
                out.extend(vs);
                mentions(t, out);
            }
        }
        Term::Handle {
            body, clauses, ret, ..
        } => {
            mentions(body, out);
            for c in clauses {
                out.insert(c.param);
                out.insert(c.resume);
                mentions(&c.body, out);
            }
            if let Some((v, _, t)) = ret {
                out.insert(*v);
                mentions(t, out);
            }
        }
    }
}
/// The parameters a term binds as nested lambdas, and what is left underneath.
///
/// `fun f a b = e` is `Lam(a, Lam(b, e))`, so this is how a definition's arity is
/// recovered — which is what lets a saturated call to it become a jump.
fn lam_spine(t: &Term) -> (Vec<Var>, &Term) {
    let mut params = Vec::new();
    let mut cur = t;
    // Through a polymorphic definition's type abstraction, which takes no
    // argument at run time.
    while let Term::TyLam(_, body) = cur {
        cur = body;
    }
    while let Term::Lam(p, _, body) = cur {
        params.push(*p);
        cur = body;
    }
    (params, cur)
}

/// A call, flattened: `f a b c` is `App(App(App(f, a), b), c)`.
fn call_spine(t: &Term) -> (&Term, Vec<&Term>) {
    let mut args = Vec::new();
    // Through positions: a debug build marks calls, and a call it marked
    // should still be recognised as the known call it is.
    let mut cur = t.peel();
    while let Term::App(f, a) = cur {
        args.push(&**a);
        cur = f.peel();
    }
    // Through an instantiation, which passes nothing at run time.
    while let Term::TyApp(f, _) = cur {
        cur = f.peel();
    }
    args.reverse();
    (cur, args)
}
