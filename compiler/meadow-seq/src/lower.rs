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
//! needs to. It is correct, it is small, and turning it into a decision tree is
//! a local change to [`Lower::case`] that nothing else depends on.
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
use meadow_core::{Pat, Term, Var};
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
pub fn lower_program(program: &core::Program) -> Lowered {
    let mut globals = HashMap::new();
    for (i, d) in program.defs.iter().enumerate() {
        globals.insert(d.var, (Label(i as u32), Vec::new()));
    }

    let mut lower = Lower {
        next_name: max_var(program) + 1,
        next_label: program.defs.len() as u32,
        globals,
        defs: Vec::new(),
        tags: HashMap::new(),
        next_tag: 0,
        unsupported: HashSet::new(),
    };

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

struct Lower {
    next_name: u32,
    next_label: u32,
    /// A definition or a `letrec` binding: where to jump, and the environment
    /// names its block expects before the continuation.
    globals: HashMap<Var, (Label, Vec<Name>)>,
    defs: Vec<Def>,
    tags: HashMap<InternedString, Tag>,
    next_tag: Tag,
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
        let x = self.fresh();
        let mut params = vec![x];
        params.extend_from_slice(env);
        let body = f(self, x, params.clone());
        Statement::Extern {
            op,
            args,
            blocks: vec![Block { params, body }],
        }
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
        // A variable already in scope is already a value; building a
        // continuation to receive it would be pure noise. The environment is
        // then unchanged, which is still an exact answer.
        if name.is_none()
            && let Term::Var(v) = e
            && env.contains(v)
        {
            return f(self, *v, env.to_vec());
        }

        // Two more that need no continuation, for the same reason: they produce
        // a value in one statement and cannot transfer control, so what follows
        // can simply be the `rest` of that statement.
        //
        // This is worth doing rather than leaving to a later pass. `n - 1`
        // has a literal operand, so without it every arithmetic expression in
        // every loop allocates a closure — and a loop that allocates per
        // iteration is a different machine from one that does not.
        match e {
            Term::Lit(l) => {
                let x = name.unwrap_or_else(|| self.fresh());
                let mut params = vec![x];
                params.extend_from_slice(env);
                let body = f(self, x, params.clone());
                return Statement::Extern {
                    op: Extern::Lit(l.clone()),
                    args: vec![],
                    blocks: vec![Block { params, body }],
                };
            }
            // A nullary constructor: `Nil`, `None`, `True`. Nothing to evaluate.
            Term::Ctor(ctor, args) if args.is_empty() => {
                let ctor = *ctor;
                let tag = self.tag_of(ctor);
                let x = name.unwrap_or_else(|| self.fresh());
                let mut after: Vec<Name> = vec![x];
                after.extend_from_slice(env);
                let rest = f(self, x, after);
                return Statement::Let {
                    name: x,
                    tag,
                    ctor,
                    fields: vec![],
                    rest: Box::new(rest),
                };
            }
            _ => {}
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
    fn case<'t>(
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

            Pat::Lit(l) => {
                let l = l.clone();
                self.produces(Extern::Lit(l), vec![], &env, move |this, lv, env1| {
                    this.produces(
                        Extern::Prim(core::Prim::Eq),
                        vec![lv, subject],
                        &env1,
                        move |this, b, env2| {
                            let no = this.enter(fail);
                            let yes = ok(this, env2.clone());
                            Statement::Extern {
                                op: Extern::Branch,
                                args: vec![b],
                                blocks: vec![
                                    Block {
                                        params: env2.clone(),
                                        body: no,
                                    },
                                    Block {
                                        params: env2,
                                        body: yes,
                                    },
                                ],
                            }
                        },
                    )
                })
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
                        this.produces(
                            Extern::Lit(core::Lit::Int(want)),
                            vec![],
                            &env1,
                            move |this, m, env2| {
                                this.produces(
                                    Extern::Prim(core::Prim::Eq),
                                    vec![n, m],
                                    &env2,
                                    move |this, b, env3| {
                                        let no = this.enter(fail);
                                        let yes = this.fields(subs, subject, env3.clone(), fail, ok);
                                        Statement::Extern {
                                            op: Extern::Branch,
                                            args: vec![b],
                                            blocks: vec![
                                                Block {
                                                    params: env3.clone(),
                                                    body: no,
                                                },
                                                Block {
                                                    params: env3,
                                                    body: yes,
                                                },
                                            ],
                                        }
                                    },
                                )
                            },
                        )
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
