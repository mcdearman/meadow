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
//! Effects are the other departure, and a larger one: `handle` and `perform`
//! lower to three statements that are not in AxCut at all. See
//! [`Statement::Handle`].

use crate::{Block, Def, Extern, Label, Name, Program, Statement, Tag};
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
    };

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

    // Each definition is a block of one parameter: the continuation to answer
    // with. That is also exactly the environment a `jump` to it arrives with.
    let mut entry = None;
    for (i, d) in program.defs.iter().enumerate() {
        let label = Label(i as u32);
        let k = lower.fresh();
        let body = lower.expr(&d.term, &[k], k);
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
            let k = lower.fresh();
            let mut block_params = params;
            block_params.push(k);
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

    /// The names `terms` will need, minus `bound`, with labels expanded.
    fn wants(&self, terms: &[&Term], bound: &[Var]) -> HashSet<Var> {
        let mut want = HashSet::new();
        for t in terms {
            free_into(t, &mut want);
        }
        for v in bound {
            want.remove(v);
        }
        self.expand(want)
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
        f: impl FnOnce(&mut Self, Name, Vec<Name>) -> Statement,
    ) -> Statement {
        self.produces_as(op, args, env, None, f)
    }

    /// The same, with the bound name forced — for a `let` whose right-hand side
    /// needs no continuation.
    fn produces_as(
        &mut self,
        op: Extern,
        args: Vec<Name>,
        env: &[Name],
        name: Option<Name>,
        f: impl FnOnce(&mut Self, Name, Vec<Name>) -> Statement,
    ) -> Statement {
        let x = name.unwrap_or_else(|| self.fresh());
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
            Term::Var(v) => env.contains(v),
            Term::Lit(_) => true,
            // Building a closure allocates, but it does not go anywhere.
            Term::Lam(..) => true,
            Term::Prim(_, xs) | Term::Ctor(_, xs) | Term::Tuple(xs) | Term::Array(xs) => {
                xs.iter().all(|x| self.simple(x, env))
            }
            Term::Record(fs) => fs.iter().all(|(_, t)| self.simple(t, env)),
            Term::Sel(t, _) | Term::Proj(t, _) => self.simple(t, env),
            Term::Extend(t, _, v) => self.simple(t, env) && self.simple(v, env),
            Term::Let(x, rhs, body) => {
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
        match e {
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

            Term::Lit(l) => self.produces_as(Extern::Lit(l.clone()), vec![], env, name, f),

            Term::Lam(param, body) => {
                let want = self.wants(&[body], &[*param]);
                let captures = restrict(env, &want);
                let ik = self.fresh();
                let mut params = captures.clone();
                params.push(*param);
                params.push(ik);
                let inner = self.expr(body, &params, ik);

                let x = name.unwrap_or_else(|| self.fresh());
                let mut after: Vec<Name> = vec![x];
                after.extend_from_slice(env);
                let rest = f(self, x, after);
                Statement::New {
                    name: x,
                    captures,
                    methods: vec![Block {
                        params,
                        body: inner,
                    }],
                    rest: Box::new(rest),
                }
            }

            Term::Prim(p, args) => {
                let p = *p;
                // A literal operand rides along inside the `extern`, so it never
                // becomes a name and never occupies a register.
                if let Some((x, l)) = const_operand(p, args) {
                    return self.direct_all(&[x], env.to_vec(), move |this, xs, env1| {
                        this.produces_as(Extern::PrimK(p, l), xs, &env1, name, f)
                    });
                }
                self.direct_all(args, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Prim(p), xs, &env1, name, f)
                })
            }

            Term::Ctor(ctor, args) => {
                let ctor = *ctor;
                let tag = self.tag_of(ctor);
                self.direct_all(args, env.to_vec(), move |this, fields, env1| {
                    this.builds(ctor, tag, fields, &env1, name, f)
                })
            }

            Term::Tuple(items) => {
                let ctor = InternedString::from("#tuple");
                let tag = self.tag_of(ctor);
                self.direct_all(items, env.to_vec(), move |this, fields, env1| {
                    this.builds(ctor, tag, fields, &env1, name, f)
                })
            }

            Term::Array(items) => self.direct_all(items, env.to_vec(), move |this, xs, env1| {
                this.produces_as(Extern::Array, xs, &env1, name, f)
            }),

            Term::Record(fields) => {
                let labels: Vec<InternedString> = fields.iter().map(|(n, _)| *n).collect();
                let terms: Vec<Term> = fields.iter().map(|(_, t)| t.clone()).collect();
                self.direct_all(&terms, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Record(labels), xs, &env1, name, f)
                })
            }

            Term::Sel(rec, label) => {
                let label = *label;
                self.direct(
                    rec,
                    env,
                    None,
                    Box::new(move |this, r, env1| {
                        this.produces_as(Extern::Select(label), vec![r], &env1, name, f)
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
                        this.produces_as(Extern::Field(i), vec![x], &env1, name, f)
                    }),
                )
            }

            Term::Extend(rec, label, val) => {
                let label = *label;
                let terms = vec![(**rec).clone(), (**val).clone()];
                self.direct_all(&terms, env.to_vec(), move |this, xs, env1| {
                    this.produces_as(Extern::Extend(label), xs, &env1, name, f)
                })
            }

            Term::Let(x, rhs, body) => {
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
    fn builds(
        &mut self,
        ctor: InternedString,
        tag: Tag,
        fields: Vec<Name>,
        env: &[Name],
        name: Option<Name>,
        f: Then<'_>,
    ) -> Statement {
        let x = name.unwrap_or_else(|| self.fresh());
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
        let kk = self.fresh();
        let x = name.unwrap_or_else(|| self.fresh());

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
        match e {
            Term::Var(v) if env.contains(v) => self.ret(k, *v),

            // A label: arrange exactly what its block takes and jump. `jump`
            // leaves the environment alone, so the substitution is the entire
            // calling sequence.
            Term::Var(v) => match self.globals.get(v) {
                Some((label, extra)) => {
                    let label = *label;
                    let mut sel = extra.clone();
                    sel.push(k);
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

            Term::Lit(l) => self.produces(Extern::Lit(l.clone()), vec![], env, |this, x, _| {
                this.ret(k, x)
            }),

            // Codata with one method: the argument, and where to send the answer.
            Term::Lam(param, body) => {
                let want = self.wants(&[body], &[*param]);
                let captures = restrict(env, &want);

                let ik = self.fresh();
                let mut params = captures.clone();
                params.push(*param);
                params.push(ik);
                let inner = self.expr(body, &params, ik);

                let f = self.fresh();
                let rest = self.ret(k, f);
                Statement::New {
                    name: f,
                    captures,
                    methods: vec![Block {
                        params,
                        body: inner,
                    }],
                    rest: Box::new(rest),
                }
            }

            // A saturated call to a known function is a jump. The arguments go
            // where its block wants them and control leaves — no closure per
            // argument, no continuation to receive one. This is what makes a
            // loop a loop rather than a sequence of allocations.
            Term::App(..) if self.direct_call(e).is_some() => {
                let (worker, args) = self.direct_call(e).expect("checked");
                let args: Vec<Term> = args.into_iter().cloned().collect();
                self.sequence(&args, env, k, move |_this, names, _env| {
                    let mut sel = names;
                    sel.push(k);
                    Statement::Substitute(
                        sel.clone(),
                        Box::new(Block {
                            params: sel,
                            body: Statement::Jump(worker),
                        }),
                    )
                })
            }

            // Calling is arranging `[f, arg, k]` and invoking: the object drops
            // off the front and its method sees `captures ++ [arg, k]`.
            Term::App(fun, arg) => {
                let keep = self.keep(env, &[arg], &[k]);
                self.bind(
                    fun,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, fv, env1| {
                        let want: HashSet<Var> = [k, fv].into_iter().collect();
                        let keep = restrict(&env1, &want);
                        this.bind(
                            arg,
                            &env1,
                            &keep,
                            None,
                            Box::new(move |_this, av, _env2| {
                                Statement::Substitute(
                                    vec![fv, av, k],
                                    Box::new(Block {
                                        params: vec![fv, av, k],
                                        body: Statement::Invoke(fv, 0),
                                    }),
                                )
                            }),
                        )
                    }),
                )
            }

            Term::Let(x, rhs, body) => {
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
                let bound: Vec<Var> = binds.iter().map(|(v, _)| *v).collect();
                let rhs: Vec<&Term> = binds.iter().map(|(_, t)| t).collect();
                let want = self.wants(&rhs, &bound);
                let fvs = restrict(env, &want);

                // Register every label before lowering any right-hand side, so
                // the group can refer to itself in any direction.
                let labels: Vec<Label> = binds.iter().map(|_| self.fresh_label()).collect();
                for (v, l) in bound.iter().zip(&labels) {
                    self.globals.insert(*v, (*l, fvs.clone()));
                }

                for ((_, term), label) in binds.iter().zip(&labels) {
                    let kk = self.fresh();
                    let mut params = fvs.clone();
                    params.push(kk);
                    let body = self.expr(term, &params, kk);
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
                if let Term::Prim(p, cargs) = &**c {
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

            Term::Prim(prim, args) => {
                let prim = *prim;
                if let Some((x, l)) = const_operand(prim, args) {
                    return self.sequence(&[x], env, k, move |this, names, env1| {
                        this.produces(Extern::PrimK(prim, l), names, &env1, |this, out, _| {
                            this.ret(k, out)
                        })
                    });
                }
                self.sequence(args, env, k, move |this, names, env1| {
                    this.produces(Extern::Prim(prim), names, &env1, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            Term::Ctor(name, args) => {
                let ctor = *name;
                let tag = self.tag_of(ctor);
                self.sequence(args, env, k, move |this, fields, _| {
                    let x = this.fresh();
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
                self.sequence(items, env, k, move |this, fields, _| {
                    let x = this.fresh();
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

            Term::Array(items) => self.sequence(items, env, k, move |this, xs, env1| {
                this.produces(Extern::Array, xs, &env1, |this, out, _| this.ret(k, out))
            }),

            Term::Record(fields) => {
                let labels: Vec<InternedString> = fields.iter().map(|(n, _)| *n).collect();
                let terms: Vec<Term> = fields.iter().map(|(_, t)| t.clone()).collect();
                self.sequence(&terms, env, k, move |this, xs, env1| {
                    this.produces(Extern::Record(labels), xs, &env1, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            Term::Sel(rec, label) => {
                let label = *label;
                let keep = restrict(env, &[k].into_iter().collect());
                self.bind(
                    rec,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, r, env1| {
                        this.produces(Extern::Select(label), vec![r], &env1, |this, out, _| {
                            this.ret(k, out)
                        })
                    }),
                )
            }

            Term::Extend(rec, label, val) => {
                let label = *label;
                let terms = vec![(**rec).clone(), (**val).clone()];
                self.sequence(&terms, env, k, move |this, xs, env1| {
                    this.produces(Extern::Extend(label), xs, &env1, |this, out, _| {
                        this.ret(k, out)
                    })
                })
            }

            // Tuple projection cannot be a `switch`: the arity a `switch` arm
            // would have to name is not in the term.
            Term::Proj(t, i) => {
                let i = *i;
                let keep = restrict(env, &[k].into_iter().collect());
                self.bind(
                    t,
                    env,
                    &keep,
                    None,
                    Box::new(move |this, x, env1| {
                        this.produces(Extern::Field(i), vec![x], &env1, |this, out, _| {
                            this.ret(k, out)
                        })
                    }),
                )
            }

            Term::Case(scrutinee, arms) => {
                let mut want = HashSet::new();
                for (p, body) in arms {
                    let mut bound = Vec::new();
                    pat_vars(p, &mut bound);
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

            Term::Perform(effect, op, arg) => {
                let (effect, op) = (*effect, *op);
                let keep = restrict(env, &[k].into_iter().collect());
                self.bind(
                    arg,
                    env,
                    &keep,
                    None,
                    Box::new(move |_this, av, _env1| Statement::Perform {
                        effect,
                        op,
                        arg: av,
                        k,
                    }),
                )
            }

            Term::Handle { body, clauses, ret } => self.handle(body, clauses, ret.as_ref(), env, k),

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
        let (head, args) = call_spine(e);
        let Term::Var(v) = head else { return None };
        let &(label, arity) = self.workers.get(v)?;
        (arity == args.len()).then_some((label, args))
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
    fn case(
        &mut self,
        s: Name,
        arms: &[(Pat, Term)],
        live: Vec<Name>,
        k: Name,
    ) -> Statement {
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
        let rest = self.fresh();
        let mut env = vec![rest];
        env.extend_from_slice(live);

        let mut switch_arms = Vec::with_capacity(n);
        for (pat, term) in &arms[..n] {
            let Pat::Ctor(ctor, subs) = pat else {
                unreachable!("the prefix is constructor patterns");
            };
            let tag = self.tag_of(*ctor);
            let fields: Vec<Name> = subs.iter().map(|_| self.fresh()).collect();
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
        let fails: Vec<Name> = (0..=n).map(|_| self.fresh()).collect();

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
            Pat::Var(v) => self.rebind(subject, *v, env, ok),

            Pat::As(v, sub) => {
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
                let fields: Vec<Name> = subs.iter().map(|_| self.fresh()).collect();
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
                            this.produces(Extern::Select(*label), vec![subject], &env, |this, x, env1| {
                                this.match_pat(
                                    p,
                                    x,
                                    env1,
                                    fail,
                                    Box::new(move |this, env2| go(this, rest, subject, env2, fail, ok)),
                                )
                            })
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
            this.produces(Extern::Field(i), vec![subject], &env, move |this, x, env1| {
                let mut got = got;
                got.push((i, x));
                go(this, subs, i + 1, subject, env1, got, fail, ok)
            })
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

    /// `handle body with { … }`.
    ///
    /// Three objects and one frame: the handler, whose methods are the clauses;
    /// the body's continuation, which pops the frame and runs the `return`
    /// clause; and the frame itself, which is the only part that is not ordinary
    /// AxCut.
    fn handle(
        &mut self,
        body: &Term,
        clauses: &[core::HClause],
        ret: Option<&(Var, std::sync::Arc<Term>)>,
        env: &[Name],
        k: Name,
    ) -> Statement {
        // The handler object captures whatever its clauses need from here; the
        // argument, the resumption and the handler's own continuation all arrive
        // as parameters.
        let mut want = HashSet::new();
        for c in clauses {
            want.extend(self.wants(&[&c.body], &[c.param, c.resume]));
        }
        let caps_h = restrict(env, &want);

        let h = self.fresh();
        let mut methods = Vec::new();
        let mut ops = Vec::new();
        for c in clauses {
            let kh = self.fresh();
            let mut params = caps_h.clone();
            params.push(c.param);
            params.push(c.resume);
            params.push(kh);
            let body = self.expr(&c.body, &params, kh);
            methods.push(Block {
                params,
                body: body,
            });
            ops.push((c.effect, c.op));
        }

        // After `new h`, and after `new kb`, in that order.
        let mut env_h: Vec<Name> = vec![h];
        env_h.extend_from_slice(env);

        // The body's continuation. It does *not* capture `k`: where the value
        // goes is read off the handler frame, because a resumption may have
        // moved it. See [`Statement::Unhandle`].
        let kb = self.fresh();
        let (x, ret_body) = match ret {
            Some((p, t)) => (*p, Some(&**t)),
            None => (self.fresh(), None),
        };
        let caps_r = match ret_body {
            Some(t) => restrict(&env_h, &self.wants(&[t], &[x])),
            None => Vec::new(),
        };
        let mut params_r = caps_r.clone();
        params_r.push(x);

        let kk = self.fresh();
        let mut env_r: Vec<Name> = vec![kk];
        env_r.extend_from_slice(&params_r);
        let inner = match ret_body {
            Some(t) => self.expr(t, &env_r, kk),
            None => self.ret(kk, x),
        };

        let mut env_b: Vec<Name> = vec![kb];
        env_b.extend_from_slice(&env_h);
        let body = self.expr(body, &env_b, kb);

        Statement::New {
            name: h,
            captures: caps_h,
            methods,
            rest: Box::new(Statement::New {
                name: kb,
                captures: caps_r,
                methods: vec![Block {
                    params: params_r,
                    // The body returned normally: take the handler back off and
                    // pick up where its value goes, then run the `return`
                    // clause outside its own handler — the same place a
                    // `perform` clause runs.
                    body: Statement::Unhandle {
                        k: kk,
                        rest: Box::new(inner),
                    },
                }],
                rest: Box::new(Statement::Handle {
                    handler: h,
                    ops,
                    // The frame answers the `handle` expression's own
                    // continuation, not the body's.
                    k,
                    rest: Box::new(body),
                }),
            }),
        }
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

/// Free variables of `t`.
fn free_into(t: &Term, out: &mut HashSet<Var>) {
    fn go(t: &Term, bound: &mut Vec<Var>, out: &mut HashSet<Var>) {
        match t {
            Term::Var(v) => {
                if !bound.contains(v) {
                    out.insert(*v);
                }
            }
            Term::Lit(_) | Term::Error => {}
            Term::Lam(p, b) => {
                bound.push(*p);
                go(b, bound, out);
                bound.pop();
            }
            Term::App(f, a) => {
                go(f, bound, out);
                go(a, bound, out);
            }
            Term::Let(x, r, b) => {
                go(r, bound, out);
                bound.push(*x);
                go(b, bound, out);
                bound.pop();
            }
            Term::LetRec(binds, body) => {
                for (v, _) in binds {
                    bound.push(*v);
                }
                for (_, t) in binds {
                    go(t, bound, out);
                }
                go(body, bound, out);
                for _ in binds {
                    bound.pop();
                }
            }
            Term::If(a, b, c) => {
                go(a, bound, out);
                go(b, bound, out);
                go(c, bound, out);
            }
            Term::Tuple(xs) | Term::Array(xs) => {
                for x in xs {
                    go(x, bound, out);
                }
            }
            Term::Ctor(_, xs) | Term::Prim(_, xs) => {
                for x in xs {
                    go(x, bound, out);
                }
            }
            Term::Proj(t, _) | Term::Sel(t, _) => go(t, bound, out),
            Term::Extend(t, _, u) => {
                go(t, bound, out);
                go(u, bound, out);
            }
            Term::Record(fs) => {
                for (_, t) in fs {
                    go(t, bound, out);
                }
            }
            Term::Perform(_, _, a) => go(a, bound, out),
            Term::Case(s, arms) => {
                go(s, bound, out);
                for (p, t) in arms {
                    let before = bound.len();
                    pat_vars(p, bound);
                    go(t, bound, out);
                    bound.truncate(before);
                }
            }
            Term::Handle { body, clauses, ret } => {
                go(body, bound, out);
                for c in clauses {
                    bound.push(c.param);
                    bound.push(c.resume);
                    go(&c.body, bound, out);
                    bound.pop();
                    bound.pop();
                }
                if let Some((v, t)) = ret {
                    bound.push(*v);
                    go(t, bound, out);
                    bound.pop();
                }
            }
        }
    }
    go(t, &mut Vec::new(), out);
}

/// The variables a pattern binds.
fn pat_vars(p: &Pat, out: &mut Vec<Var>) {
    match p {
        Pat::Wild | Pat::Lit(_) => {}
        Pat::Var(v) => out.push(*v),
        Pat::As(v, sub) => {
            out.push(*v);
            pat_vars(sub, out);
        }
        Pat::Tuple(ps) | Pat::Array(ps) | Pat::Ctor(_, ps) => {
            for p in ps {
                pat_vars(p, out);
            }
        }
        Pat::Record(fs) => {
            for (_, p) in fs {
                pat_vars(p, out);
            }
        }
    }
}

/// Every variable a term mentions, bound or free — only for sizing the fresh
/// counter, where the distinction does not matter.
fn mentions(t: &Term, out: &mut HashSet<Var>) {
    match t {
        Term::Var(v) => {
            out.insert(*v);
        }
        Term::Lit(_) | Term::Error => {}
        Term::Lam(p, b) => {
            out.insert(*p);
            mentions(b, out);
        }
        Term::App(f, a) => {
            mentions(f, out);
            mentions(a, out);
        }
        Term::Let(x, r, b) => {
            out.insert(*x);
            mentions(r, out);
            mentions(b, out);
        }
        Term::LetRec(binds, body) => {
            for (v, t) in binds {
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
        Term::Tuple(xs) | Term::Array(xs) => {
            for x in xs {
                mentions(x, out);
            }
        }
        Term::Ctor(_, xs) | Term::Prim(_, xs) => {
            for x in xs {
                mentions(x, out);
            }
        }
        Term::Proj(t, _) | Term::Sel(t, _) => mentions(t, out),
        Term::Extend(t, _, u) => {
            mentions(t, out);
            mentions(u, out);
        }
        Term::Record(fs) => {
            for (_, t) in fs {
                mentions(t, out);
            }
        }
        Term::Perform(_, _, a) => mentions(a, out),
        Term::Case(s, arms) => {
            mentions(s, out);
            for (p, t) in arms {
                let mut vs = Vec::new();
                pat_vars(p, &mut vs);
                out.extend(vs);
                mentions(t, out);
            }
        }
        Term::Handle { body, clauses, ret } => {
            mentions(body, out);
            for c in clauses {
                out.insert(c.param);
                out.insert(c.resume);
                mentions(&c.body, out);
            }
            if let Some((v, t)) = ret {
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
    while let Term::Lam(p, body) = cur {
        params.push(*p);
        cur = body;
    }
    (params, cur)
}

/// A call, flattened: `f a b c` is `App(App(App(f, a), b), c)`.
fn call_spine(t: &Term) -> (&Term, Vec<&Term>) {
    let mut args = Vec::new();
    let mut cur = t;
    while let Term::App(f, a) = cur {
        args.push(&**a);
        cur = f;
    }
    args.reverse();
    (cur, args)
}
