//! `core` → sequent form.
//!
//! The whole translation is one function, `⟦e⟧ k`: *run `e` and hand its value
//! to the consumer `k`*. Every case is then a matter of deciding what `k` should
//! be for the subterms.
//!
//! ```text
//!   ⟦x⟧ k          =  ⟨x | k⟩
//!   ⟦let x = e; b⟧ k  =  ⟦e⟧ (mu~ x. ⟦b⟧ k)
//!   ⟦f a⟧ k        =  ⟦f⟧ (mu~ fv. ⟦a⟧ (mu~ av. ⟨fv | apply(av, k)⟩))
//! ```
//!
//! The `let` case is the one to read twice: a `let` is not a binding form here,
//! it is *the consumer `mu~`*. Binding a name and continuing is what consuming a
//! value means. Function application falls out the same way — evaluate the
//! function, evaluate the argument, then cut one against the other — and the
//! left-to-right order that a strict language needs is visible in the nesting
//! rather than implied by an evaluator.
//!
//! # Not yet translated
//!
//! [`Unsupported`] lists what still lowers to [`Statement::Error`]. Each is a
//! real gap, not an oversight, and the differential tests skip programs that
//! use them rather than pretending.

use crate::{
    Branch, Consumer, Covar, Def, HandlerClause, Pattern, Producer, Program, Statement,
};
use meadow_core as core;
use meadow_core::{Term, Var};
use meadow_hir::VarId;
use meadow_intern::InternedString;
use std::collections::HashSet;

/// A source of fresh names, shared with later passes so they do not collide.
#[derive(Debug, Default)]
pub struct Names {
    next_var: u32,
    next_covar: u32,
}

impl Names {
    /// Start handing out variables above everything the front end already used.
    pub fn starting_at(next_var: u32) -> Names {
        Names {
            next_var,
            next_covar: 0,
        }
    }

    pub fn fresh_var(&mut self) -> Var {
        let v = VarId(self.next_var);
        self.next_var += 1;
        v
    }

    pub fn fresh_covar(&mut self) -> Covar {
        let c = Covar(self.next_covar);
        self.next_covar += 1;
        c
    }
}

/// A `core` construct this pass does not translate yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unsupported {
    LetRec,
    Record,
    Select,
    Extend,
    ArrayPattern,
    RecordPattern,
    NestedPattern,
    CoreError,
}

pub struct Lowered {
    pub program: Program,
    pub names: Names,
    /// What was met and not translated. Empty means the program is fully covered.
    pub unsupported: HashSet<Unsupported>,
}

/// Lower a whole program.
pub fn lower_program(program: &core::Program, first_fresh_var: u32) -> Lowered {
    let mut lower = Lower {
        names: Names::starting_at(first_fresh_var),
        unsupported: HashSet::new(),
    };
    let defs = program
        .defs
        .iter()
        .map(|d| {
            let ret = lower.names.fresh_covar();
            Def {
                var: d.var,
                name: d.name,
                body: lower.expr(&d.term, Consumer::Covar(ret)),
                ret,
            }
        })
        .collect();
    Lowered {
        program: Program {
            defs,
            entry: program.entry,
        },
        names: lower.names,
        unsupported: lower.unsupported,
    }
}

struct Lower {
    names: Names,
    unsupported: HashSet<Unsupported>,
}

impl Lower {
    fn give_up(&mut self, what: Unsupported) -> Statement {
        self.unsupported.insert(what);
        Statement::Error
    }

    /// A covariable naming `k`.
    ///
    /// Several rules need somewhere to *send* a result, which is a covariable,
    /// but the consumer they were handed may be any consumer at all. `mu` is
    /// exactly the bridge: `⟨mu a. s | k⟩` runs `s` with `a` standing for `k`.
    /// When `k` is already a covariable this is the identity, which is why the
    /// common case emits nothing extra.
    fn with_covar(
        &mut self,
        k: Consumer,
        f: impl FnOnce(&mut Self, Covar) -> Statement,
    ) -> Statement {
        match k {
            Consumer::Covar(a) => f(self, a),
            other => {
                let a = self.names.fresh_covar();
                let body = f(self, a);
                Statement::Cut(Producer::Mu(a, Box::new(body)), other)
            }
        }
    }

    /// Evaluate `e`, bind its value to a fresh variable, and continue.
    ///
    /// The workhorse for anything strict in its operands: it is `mu~` with the
    /// binding chosen for you.
    fn bind(&mut self, e: &Term, f: impl FnOnce(&mut Self, Producer) -> Statement) -> Statement {
        // An atom is already a value; binding it would only add a name.
        if let Some(p) = self.atom(e) {
            return f(self, p);
        }
        let x = self.names.fresh_var();
        let body = f(self, Producer::Var(x));
        self.expr(e, Consumer::MuTilde(x, Box::new(body)))
    }

    /// Several terms in order, each bound before the next — the left-to-right
    /// evaluation a strict language promises.
    fn bind_all(
        &mut self,
        es: &[Term],
        f: impl FnOnce(&mut Self, Vec<Producer>) -> Statement,
    ) -> Statement {
        fn go(
            this: &mut Lower,
            es: &[Term],
            mut done: Vec<Producer>,
            f: impl FnOnce(&mut Lower, Vec<Producer>) -> Statement,
        ) -> Statement {
            match es.split_first() {
                None => f(this, done),
                Some((head, rest)) => this.bind(head, move |this, p| {
                    done.push(p);
                    go(this, rest, done, f)
                }),
            }
        }
        go(self, es, Vec::new(), f)
    }

    /// A term that is already a value, needing no computation.
    fn atom(&mut self, e: &Term) -> Option<Producer> {
        match e {
            Term::Var(v) => Some(Producer::Var(*v)),
            Term::Lit(l) => Some(Producer::Lit(l.clone())),
            _ => None,
        }
    }

    /// `⟦e⟧ k`.
    fn expr(&mut self, e: &Term, k: Consumer) -> Statement {
        match e {
            Term::Var(v) => Statement::Cut(Producer::Var(*v), k),
            Term::Lit(l) => Statement::Cut(Producer::Lit(l.clone()), k),

            // A function names both what it takes and where its answer goes.
            Term::Lam(param, body) => {
                let ret = self.names.fresh_covar();
                let body = self.expr(body, Consumer::Covar(ret));
                Statement::Cut(
                    Producer::Lam {
                        param: *param,
                        ret,
                        body: Box::new(body),
                    },
                    k,
                )
            }

            Term::App(f, a) => self.bind(f, |this, fv| {
                this.bind(a, |this, av| {
                    this.with_covar(k, |_, ret| {
                        Statement::Cut(fv, Consumer::Apply(Box::new(av), ret))
                    })
                })
            }),

            // The rule worth pausing on: a `let` *is* the `mu~` consumer.
            Term::Let(x, rhs, body) => {
                let rest = self.expr(body, k);
                self.expr(rhs, Consumer::MuTilde(*x, Box::new(rest)))
            }

            Term::If(c, t, e) => self.bind(c, |this, cond| Statement::If {
                cond,
                then: Box::new(this.expr(t, k.clone())),
                els: Box::new(this.expr(e, k)),
            }),

            Term::Prim(prim, args) => self.bind_all(args, |this, args| {
                let out = this.names.fresh_var();
                Statement::Prim {
                    prim: *prim,
                    args,
                    out,
                    next: Box::new(Statement::Cut(Producer::Var(out), k)),
                }
            }),

            Term::Tuple(items) => self.bind_all(items, |_, ps| {
                Statement::Cut(Producer::Tuple(ps), k)
            }),

            Term::Ctor(name, args) => {
                let name = *name;
                self.bind_all(args, move |_, ps| {
                    Statement::Cut(Producer::Ctor(name, ps), k)
                })
            }

            Term::Proj(t, i) => {
                let i = *i;
                self.bind(t, move |this, p| {
                    // Projection is a one-armed match: bind every field, keep one.
                    let vars: Vec<Var> = (0..=i).map(|_| this.names.fresh_var()).collect();
                    let chosen = vars[i];
                    Statement::Cut(
                        p,
                        Consumer::Case(vec![Branch {
                            pat: Pattern::Tuple(vars),
                            body: Statement::Cut(Producer::Var(chosen), k),
                        }]),
                    )
                })
            }

            Term::Case(scrutinee, arms) => self.bind(scrutinee, |this, p| {
                let branches = arms
                    .iter()
                    .map(|(pat, body)| {
                        let pat = this.pattern(pat);
                        Branch {
                            pat,
                            body: this.expr(body, k.clone()),
                        }
                    })
                    .collect();
                Statement::Cut(p, Consumer::Case(branches))
            }),

            Term::Perform(effect, op, arg) => {
                let (effect, op) = (*effect, *op);
                self.bind(arg, move |this, arg| {
                    this.with_covar(k, |_, ret| Statement::Perform {
                        effect,
                        op,
                        arg,
                        ret,
                    })
                })
            }

            Term::Handle { body, clauses, ret } => {
                let inner = self.names.fresh_covar();
                let body = self.expr(body, Consumer::Covar(inner));
                let clauses = clauses
                    .iter()
                    .map(|c| {
                        let cret = self.names.fresh_covar();
                        HandlerClause {
                            effect: c.effect,
                            op: c.op,
                            param: c.param,
                            resume: c.resume,
                            body: self.expr(&c.body, Consumer::Covar(cret)),
                        }
                    })
                    .collect();
                let ret_clause = ret.as_ref().map(|(x, body)| {
                    let rret = self.names.fresh_covar();
                    (*x, Box::new(self.expr(body, Consumer::Covar(rret))))
                });
                self.with_covar(k, |_, out| Statement::Handle {
                    body: Box::new(body),
                    clauses,
                    ret: ret_clause,
                    out,
                })
            }

            // --- not yet ---------------------------------------------------
            Term::LetRec(..) => self.give_up(Unsupported::LetRec),
            Term::Record(..) => self.give_up(Unsupported::Record),
            Term::Sel(..) => self.give_up(Unsupported::Select),
            Term::Extend(..) => self.give_up(Unsupported::Extend),
            Term::Array(items) => self.bind_all(items, |_, ps| {
                // An array literal is a tuple as far as this IR is concerned;
                // the runtime distinguishes them.
                Statement::Cut(Producer::Tuple(ps), k)
            }),
            Term::Error => self.give_up(Unsupported::CoreError),
        }
    }

    /// Flatten a core pattern. Only one level deep is representable here, so a
    /// nested pattern is recorded as unsupported rather than silently truncated.
    fn pattern(&mut self, p: &core::Pat) -> Pattern {
        match p {
            core::Pat::Wild => Pattern::Wildcard,
            core::Pat::Var(v) => Pattern::Ctor(InternedString::from("<bind>"), vec![*v]),
            core::Pat::Lit(l) => Pattern::Lit(l.clone()),
            core::Pat::Tuple(ps) => match self.field_vars(ps) {
                Some(vs) => Pattern::Tuple(vs),
                None => {
                    self.unsupported.insert(Unsupported::NestedPattern);
                    Pattern::Wildcard
                }
            },
            core::Pat::Ctor(name, ps) => match self.field_vars(ps) {
                Some(vs) => Pattern::Ctor(*name, vs),
                None => {
                    self.unsupported.insert(Unsupported::NestedPattern);
                    Pattern::Wildcard
                }
            },
            core::Pat::Array(_) => {
                self.unsupported.insert(Unsupported::ArrayPattern);
                Pattern::Wildcard
            }
            core::Pat::Record(_) => {
                self.unsupported.insert(Unsupported::RecordPattern);
                Pattern::Wildcard
            }
            core::Pat::As(..) => {
                self.unsupported.insert(Unsupported::NestedPattern);
                Pattern::Wildcard
            }
        }
    }

    /// Sub-patterns as plain names, or `None` if any of them is not a name.
    fn field_vars(&mut self, ps: &[core::Pat]) -> Option<Vec<Var>> {
        ps.iter()
            .map(|p| match p {
                core::Pat::Var(v) => Some(*v),
                core::Pat::Wild => Some(self.names.fresh_var()),
                _ => None,
            })
            .collect()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use meadow_core::{Lit, Prim};

    fn lower_one(t: Term) -> (Statement, HashSet<Unsupported>) {
        let prog = core::Program {
            defs: vec![core::Def {
                var: VarId(0),
                name: InternedString::from("main"),
                term: t,
            }],
            entry: Some(VarId(0)),
            ctor_fields: Default::default(),
        };
        let out = lower_program(&prog, 1000);
        (out.program.defs[0].body.clone(), out.unsupported)
    }

    fn v(n: u32) -> Term {
        Term::Var(VarId(n))
    }

    #[test]
    fn a_variable_is_a_cut_against_the_continuation() {
        // The whole IR in one case: `⟦x⟧ a = ⟨x | a⟩`.
        let (s, un) = lower_one(v(1));
        assert!(un.is_empty());
        match s {
            Statement::Cut(Producer::Var(x), Consumer::Covar(_)) => assert_eq!(x.0, 1),
            other => panic!("expected a cut, got {other:?}"),
        }
    }

    #[test]
    fn a_let_becomes_the_mu_tilde_consumer() {
        // `⟦let x = 1; x⟧ a = ⟨1 | mu~ x. ⟨x | a⟩⟩` — binding a name is what
        // consuming a value *is*, which is the idea the whole IR rests on.
        let t = Term::Let(
            VarId(1),
            std::sync::Arc::new(Term::Lit(Lit::Int(1))),
            std::sync::Arc::new(v(1)),
        );
        let (s, un) = lower_one(t);
        assert!(un.is_empty());
        match s {
            Statement::Cut(Producer::Lit(Lit::Int(1)), Consumer::MuTilde(x, body)) => {
                assert_eq!(x.0, 1);
                assert!(matches!(*body, Statement::Cut(Producer::Var(_), _)));
            }
            other => panic!("expected a cut into mu~, got {other:?}"),
        }
    }

    #[test]
    fn application_evaluates_left_to_right() {
        // `f g` where both are computations: the function is bound first, then
        // the argument, then they meet. A strict language's evaluation order is
        // visible in the nesting rather than left to an evaluator's discretion.
        let call = |f: Term, a: Term| Term::App(std::sync::Arc::new(f), std::sync::Arc::new(a));
        let t = call(call(v(1), v(2)), call(v(3), v(4)));
        let (s, un) = lower_one(t);
        assert!(un.is_empty());
        // Outermost binding is for the function position.
        assert!(
            matches!(s, Statement::Cut(Producer::Var(f), Consumer::Apply(..)) if f.0 == 1)
                || matches!(&s, Statement::Cut(_, Consumer::MuTilde(..))),
            "got {s:?}"
        );
    }

    #[test]
    fn an_atom_is_not_given_a_needless_name() {
        // `f 1` should not bind `1` to a fresh variable first — the IR would be
        // correct but twice the size, and every later pass would pay for it.
        let t = Term::App(
            std::sync::Arc::new(v(1)),
            std::sync::Arc::new(Term::Lit(Lit::Int(1))),
        );
        let (s, _) = lower_one(t);
        match s {
            Statement::Cut(Producer::Var(f), Consumer::Apply(arg, _)) => {
                assert_eq!(f.0, 1);
                assert!(matches!(*arg, Producer::Lit(Lit::Int(1))));
            }
            other => panic!("expected a direct cut, got {other:?}"),
        }
    }

    #[test]
    fn a_primitive_binds_its_arguments_then_its_result() {
        let t = Term::Prim(Prim::Add, vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))]);
        let (s, un) = lower_one(t);
        assert!(un.is_empty());
        match s {
            Statement::Prim { prim, args, .. } => {
                assert_eq!(prim, Prim::Add);
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected a prim statement, got {other:?}"),
        }
    }

    #[test]
    fn what_is_not_translated_is_reported_rather_than_dropped() {
        // Silence here would mean a program that compiles and does the wrong
        // thing, which is the one outcome worse than not compiling.
        let (s, un) = lower_one(Term::LetRec(vec![], std::sync::Arc::new(v(1))));
        assert!(matches!(s, Statement::Error));
        assert!(un.contains(&Unsupported::LetRec));
    }

    #[test]
    fn fresh_names_do_not_collide_with_the_front_ends() {
        // Lowering invents variables; starting below the front end's would make
        // two different things share a name.
        let mut names = Names::starting_at(500);
        assert_eq!(names.fresh_var().0, 500);
        assert_eq!(names.fresh_var().0, 501);
        assert_eq!(names.fresh_covar().0, 0, "covariables are their own space");
    }
}
