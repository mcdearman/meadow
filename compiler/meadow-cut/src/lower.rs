//! Cut lowered to AxCut: the upper IR to the lower, which the runtimes
//! compile.
//!
//! Cut names its values and says where each goes. AxCut has no names to
//! speak of: an environment, in order, which every block's parameters list
//! whole, and seven statements that are each a cut with the rule it meets.
//! So lowering is deciding what is in the environment at each point, and
//! writing each cut as the statement it is:
//!
//! ```text
//!   <K(x, ..) | μ̃ y. s>          let y = K#t(x, ..); s
//!   <cocase { .. } | μ̃ f. s>     new f [what it mentions] { .. }; s
//!   <x | case { K(..) => s }>     switch x { #t (fields, env) => s }
//!   <f | m(x, ..; k, ..)>         substitute [x, .., k, .., f]; invoke f#m
//!   <x | k>                       substitute [x, k]; invoke k#0
//!   f(x, ..; k, ..)               substitute [x, .., k, ..]; jump f
//!   prim op(x, ..; c)             extern op(x, ..) -> (r, env); r to c
//! ```
//!
//! A consumer that is not a name already -- a `μ̃`, a `case` -- and has to be
//! one, to be passed, is made an object of one method that takes the value:
//! what it mentions from around it, captured. An object captures only what
//! its methods mention, in the order the environment has them.
//!
//! A top-level value is a global of the machine's, by its place among the
//! program's: the block a program starts at computes each in turn and then
//! calls the entry.
//!
//! **Not lowered yet**: `handle` and `perform`, a definition generic in a
//! representation, a descriptor, and a `μ` that is an argument, whose
//! representation nothing says. A program with one of them is refused, with
//! which.

use std::collections::{HashMap, HashSet};

use meadow_axcut as ax;
use meadow_axcut::{Block, Extern, Label, Name, Statement as S, Tag, VarId};
use meadow_intern::InternedString;
use meadow_rt::{Lit, Prim, desc};

use crate::{Answer, Arm, Consumer, Pattern, Producer, Program, Rep, Statement, Symbol};

type R<T> = Result<T, String>;

/// `p` as a program of AxCut, or why it is not one yet.
pub fn lower(p: &Program) -> R<ax::Program> {
    let mut l = Lower {
        out: ax::Program::default(),
        next: 0,
        next_tag: 0,
        defs: HashMap::new(),
        vals: HashMap::new(),
        methods: Vec::new(),
    };
    l.declare(p)?;
    for (i, d) in p.defs.iter().enumerate() {
        if !d.rep_vars.is_empty() {
            return Err(format!(
                "`{}` is generic in a representation, which is not lowered yet",
                d.symbol
            ));
        }
        let mut sc = Scope::default();
        for b in &d.params {
            let n = l.fresh(rep_of(&b.rep)?);
            sc.bind(&b.name, n);
        }
        for k in &d.conts {
            let n = l.fresh(ax::Rep::Ref);
            sc.bind(k, n);
            l.out.returns.insert(n);
        }
        let params = sc.env.clone();
        let body = l.statement(&d.body, &sc)?;
        l.out.defs.push(ax::Def {
            label: Label(i as u32),
            name: InternedString::from(d.symbol.to_string().as_str()),
            module: InternedString::from(""),
            block: Block { params, body },
        });
    }
    l.start(p)?;
    Ok(l.out)
}

struct Lower {
    out: ax::Program,
    next: u32,
    next_tag: Tag,
    /// Each definition's label, and how many values and continuations it
    /// takes.
    defs: HashMap<Symbol, (Label, usize)>,
    /// Each top-level value's place among the machine's globals, and how it
    /// is represented.
    vals: HashMap<Symbol, (i64, ax::Rep)>,
    /// Every method any object of the program has, by name: a method's tag
    /// is its place here, so that a call need not know which object it has.
    methods: Vec<String>,
}

/// What is in scope: the environment, in order, and what each name of Cut's
/// -- a value's or a continuation's -- is in it. `halt` is where a val's
/// value goes.
#[derive(Clone, Default)]
struct Scope {
    env: Vec<Name>,
    vars: HashMap<String, Name>,
    halt: Option<Name>,
}

impl Scope {
    fn bind(&mut self, name: &str, n: Name) {
        self.env.push(n);
        self.vars.insert(name.to_string(), n);
    }

    /// One more name, where a statement that makes a value puts it: first.
    fn push(&mut self, n: Name) {
        self.env.insert(0, n);
    }

    fn var(&self, name: &str) -> R<Name> {
        self.vars
            .get(name)
            .copied()
            .ok_or_else(|| format!("`{name}` is not bound"))
    }
}

/// A statement that binds one more name, at the front of the environment --
/// where the machine puts what a statement makes -- and goes on: what is
/// still to come is its `rest`.
enum Step {
    Let(Name, Tag, InternedString, Vec<Name>),
    New(Name, Vec<Name>, Vec<Block>),
    /// A primitive with one continuation, whose parameters are these.
    Extern(Extern, Vec<Name>, Vec<Name>),
}

fn wrap(steps: Vec<Step>, mut rest: S) -> S {
    for step in steps.into_iter().rev() {
        rest = match step {
            Step::Let(name, tag, ctor, fields) => S::Let {
                name,
                tag,
                ctor,
                fields,
                rest: Box::new(rest),
            },
            Step::New(name, captures, methods) => S::New {
                name,
                captures,
                methods,
                rest: Box::new(rest),
            },
            Step::Extern(op, args, params) => S::Extern {
                op,
                args,
                blocks: vec![Block { params, body: rest }],
            },
        };
    }
    rest
}

impl Lower {
    fn fresh(&mut self, rep: ax::Rep) -> Name {
        let n = VarId(self.next);
        self.next += 1;
        self.out.reps.insert(n, rep);
        n
    }

    fn tag_of(&mut self, ctor: &str) -> Tag {
        let ctor = InternedString::from(ctor);
        if let Some(t) = self.out.tags.get(&ctor) {
            return *t;
        }
        let t = self.next_tag;
        self.next_tag += 1;
        self.out.tags.insert(ctor, t);
        t
    }

    fn method(&self, name: &str) -> R<Tag> {
        self.methods
            .iter()
            .position(|m| m == name)
            .map(|i| i as Tag)
            .ok_or_else(|| format!("no object of the program has a method `{name}`"))
    }

    /// What the program declares: its constructors' tags, its roles, its
    /// definitions' labels, its values' places and its objects' methods.
    fn declare(&mut self, p: &Program) -> R<()> {
        for d in &p.datas {
            for (name, _) in &d.ctors {
                self.tag_of(&d.symbol.child(name).to_string());
            }
        }
        for (role, ctor) in &p.roles {
            let Some(r) = ax::Role::named(role) else {
                return Err(format!("`{role}` is not a role a runtime knows"));
            };
            self.out
                .roles
                .insert(r, InternedString::from(ctor.to_string().as_str()));
        }
        for (i, d) in p.defs.iter().enumerate() {
            self.defs.insert(
                d.symbol.clone(),
                (Label(i as u32), d.params.len() + d.conts.len()),
            );
        }
        for (i, v) in p.vals.iter().enumerate() {
            self.vals
                .insert(v.symbol.clone(), (i as i64, rep_of(&v.rep)?));
        }
        let mut names = HashSet::new();
        for d in &p.defs {
            methods_of(&d.body, &mut names);
        }
        for v in &p.vals {
            methods_of(&v.body, &mut names);
        }
        self.methods = names.into_iter().collect();
        self.methods.sort();
        Ok(())
    }

    /// The block a program starts at: each top-level value computed, in the
    /// order written, and kept; then the entry, answering the continuation
    /// the block was given.
    fn start(&mut self, p: &Program) -> R<()> {
        let Some(entry) = &p.entry else {
            return Ok(());
        };
        let Some(&(label, takes)) = self.defs.get(entry) else {
            return Err(format!("the entry `{entry}` is not a definition"));
        };
        if takes != 1 {
            return Err(format!(
                "the entry `{entry}` takes one continuation and no values"
            ));
        }
        let k = self.fresh(ax::Rep::Ref);
        self.out.returns.insert(k);
        // Built from the last value back: what follows each is the method of
        // the continuation its body answers.
        let mut sc = Scope {
            env: vec![k],
            ..Scope::default()
        };
        // What each value's turn binds: the continuation that keeps it; in
        // that, the value, its place, and the unit keeping it answers.
        struct Turn<'a> {
            body: Scope,
            keep: Name,
            before: Vec<Name>,
            with_value: Vec<Name>,
            with_place: Vec<Name>,
            with_done: Vec<Name>,
            value: Name,
            place: Name,
            s: &'a Statement,
        }
        let mut turns: Vec<Turn> = Vec::new();
        for v in &p.vals {
            let rep = rep_of(&v.rep)?;
            let keep = self.fresh(ax::Rep::Ref);
            self.out.continuations.insert(keep);
            let before = sc.env.clone();
            let mut body = sc.clone();
            body.push(keep);
            body.halt = Some(keep);
            let value = self.fresh(rep);
            let place = self.fresh(ax::Rep::Int);
            let done = self.fresh(ax::Rep::Bits(desc::UNIT));
            let mut with_value = before.clone();
            with_value.push(value);
            let mut with_place = with_value.clone();
            with_place.insert(0, place);
            let mut with_done = with_place.clone();
            with_done.insert(0, done);
            sc.env = with_done.clone();
            turns.push(Turn {
                body,
                keep,
                before,
                with_value,
                with_place,
                with_done,
                value,
                place,
                s: &v.body,
            });
        }
        let mut rest = S::Substitute(
            vec![k],
            Box::new(Block {
                params: vec![k],
                body: S::Jump(label),
            }),
        );
        for (i, t) in turns.into_iter().enumerate().rev() {
            let kept = S::Extern {
                op: Extern::Lit(Lit::Int(i as i64)),
                args: Vec::new(),
                blocks: vec![Block {
                    params: t.with_place,
                    body: S::Extern {
                        op: Extern::Prim(Prim::GlobalSet),
                        args: vec![t.place, t.value],
                        blocks: vec![Block {
                            params: t.with_done,
                            body: rest,
                        }],
                    },
                }],
            };
            rest = S::New {
                name: t.keep,
                captures: t.before,
                methods: vec![Block {
                    params: t.with_value,
                    body: kept,
                }],
                rest: Box::new(self.statement(t.s, &t.body)?),
            };
        }
        let label = Label(self.out.defs.len() as u32);
        self.out.defs.push(ax::Def {
            label,
            name: InternedString::from("#start"),
            module: InternedString::from(""),
            block: Block {
                params: vec![k],
                body: rest,
            },
        });
        self.out.entry = Some(label);
        self.out.results.insert(
            label,
            match p.answer {
                Answer::Str => ax::Rep::Str,
                Answer::None => ax::Rep::Ref,
            },
        );
        Ok(())
    }

    fn statement(&mut self, s: &Statement, sc: &Scope) -> R<S> {
        let mut sc = sc.clone();
        let mut steps = Vec::new();
        let last = match s {
            // The value `body` gives to `k`: `k` is the consumer, by a name.
            Statement::Cut(Producer::Mu(k, body), c) => {
                let kn = self.reify(c, &mut sc, &mut steps)?;
                sc.vars.insert(k.clone(), kn);
                self.statement(body, &sc)?
            }
            Statement::Cut(p, c) => {
                let v = self.atom(p, &mut sc, &mut steps)?;
                self.give(v, c, &sc)?
            }
            Statement::Let(b, p, rest) => {
                let v = self.atom(p, &mut sc, &mut steps)?;
                sc.vars.insert(b.name.clone(), v);
                self.statement(rest, &sc)?
            }
            Statement::Call(f, args, conts) => {
                let Some(&(label, takes)) = self.defs.get(f) else {
                    return Err(format!("`{f}` is not a definition of this program"));
                };
                if takes != args.len() + conts.len() {
                    return Err(format!(
                        "`{f}` is called with the wrong number of arguments"
                    ));
                }
                let mut sel = Vec::new();
                for a in args {
                    sel.push(self.atom(a, &mut sc, &mut steps)?);
                }
                for c in conts {
                    sel.push(self.reify(c, &mut sc, &mut steps)?);
                }
                let params = self.distinct(&sel, None);
                S::Substitute(
                    sel,
                    Box::new(Block {
                        params,
                        body: S::Jump(label),
                    }),
                )
            }
            Statement::Prim(op, args, conts) => {
                let mut names = Vec::new();
                for a in args {
                    names.push(self.atom(a, &mut sc, &mut steps)?);
                }
                self.prim(op, names, conts, &sc)?
            }
            Statement::Error(msg) => S::Error(Box::leak(msg.clone().into_boxed_str())),
            Statement::Handle(_) | Statement::Perform(..) => {
                return Err("effects are not lowered to AxCut yet".to_string());
            }
        };
        Ok(wrap(steps, last))
    }

    /// `sel` as a block's parameters: each name once, a second mention under
    /// a name of its own -- and any mention of `target` but the last, which
    /// is the one an `invoke` consumes.
    fn distinct(&mut self, sel: &[Name], target: Option<Name>) -> Vec<Name> {
        let mut seen = HashSet::new();
        let last = sel.len().saturating_sub(1);
        let mut out = Vec::with_capacity(sel.len());
        for (i, &n) in sel.iter().enumerate() {
            let is_target = target == Some(n) && i != last;
            if is_target || !seen.insert(n) {
                let rep = self.out.reps.get(&n).copied().unwrap_or(ax::Rep::Ref);
                out.push(self.fresh(rep));
            } else {
                out.push(n);
            }
        }
        // The target is last: an earlier parameter may not have taken its name.
        if let (Some(t), Some(slot)) = (target, out.last_mut()) {
            *slot = t;
        }
        out
    }

    /// A primitive: a test, which takes two continuations and gives each
    /// `unit`, or one that answers a value to its one.
    fn prim(&mut self, op: &str, args: Vec<Name>, conts: &[Consumer], sc: &Scope) -> R<S> {
        if op == "if" {
            let [no, yes] = conts else {
                return Err("`prim if` takes two continuations".to_string());
            };
            return Ok(S::Extern {
                op: Extern::Branch,
                args,
                blocks: vec![self.branch(no, sc)?, self.branch(yes, sc)?],
            });
        }
        let Some(p) = prim_named(op) else {
            return Err(format!("`{op}` is not a primitive of the runtimes"));
        };
        match conts {
            [no, yes] => Ok(S::Extern {
                op: Extern::BranchPrim(p),
                args,
                blocks: vec![self.branch(no, sc)?, self.branch(yes, sc)?],
            }),
            [c] => {
                let rep = match c {
                    Consumer::MuTilde(b, _) => rep_of(&b.rep)?,
                    _ => self.answer_of(p, &args),
                };
                let r = self.fresh(rep);
                let mut inner = sc.clone();
                inner.push(r);
                let body = self.give(r, c, &inner)?;
                Ok(S::Extern {
                    op: Extern::Prim(p),
                    args,
                    blocks: vec![Block {
                        params: inner.env,
                        body,
                    }],
                })
            }
            _ => Err(format!("`prim {op}` takes one continuation, or two")),
        }
    }

    /// How what `p` answers is represented, where the continuation it goes
    /// to does not say: a name, which has no representation written.
    fn answer_of(&self, p: Prim, args: &[Name]) -> ax::Rep {
        use Prim::*;
        match p {
            Eq | Ne | Lt | Gt | Le | Ge | LtF | GtF | LeF | GeF => ax::Rep::Bits(desc::BOOL),
            ArrayLen | StringByteLength | StringCompare | CharCode | Hash | PopCount | BitWidth
            | ToInt | StArrayLen | CompactSize => ax::Rep::Int,
            ConcatStrings | StringSlice | Show | Display | BytesToString | BytesToHex
            | CharsToString => ax::Rep::Str,
            ToWord(w) => ax::Rep::Bits(desc::word(w)),
            ToFloat32 => ax::Rep::Bits(desc::FLOAT32),
            CharFromCode => ax::Rep::Bits(desc::CHAR),
            ToFloat | Floor => ax::Rep::Float,
            Add | Sub | Mul | Div | Mod | Pow | Neg | Shl | Shr | Ushr | BitAnd | BitOr
            | BitXor | BitNot | AddF | SubF | MulF | DivF => args
                .first()
                .and_then(|a| self.out.reps.get(a))
                .copied()
                .unwrap_or(ax::Rep::Int),
            SetRef | StSetArray => ax::Rep::Bits(desc::UNIT),
            _ => ax::Rep::Ref,
        }
    }

    /// A test's continuation, as the block the test goes to: it receives
    /// `unit`, which is made only if something reads it.
    fn branch(&mut self, c: &Consumer, sc: &Scope) -> R<Block> {
        let body = match c {
            Consumer::MuTilde(b, s) if !mentions(s, &b.name) => self.statement(s, sc)?,
            _ => {
                let u = self.fresh(ax::Rep::Bits(desc::UNIT));
                let mut inner = sc.clone();
                inner.push(u);
                let body = self.give(u, c, &inner)?;
                S::Extern {
                    op: Extern::Lit(Lit::Unit),
                    args: Vec::new(),
                    blocks: vec![Block {
                        params: inner.env,
                        body,
                    }],
                }
            }
        };
        Ok(Block {
            params: sc.env.clone(),
            body,
        })
    }

    /// The value named `v` given to `c`.
    fn give(&mut self, v: Name, c: &Consumer, sc: &Scope) -> R<S> {
        match c {
            Consumer::Var(_) | Consumer::Halt => {
                let k = self.named(c, sc)?;
                let sel = vec![v, k];
                let params = self.distinct(&sel, Some(k));
                Ok(S::Substitute(
                    sel,
                    Box::new(Block {
                        params,
                        body: S::Invoke(k, 0),
                    }),
                ))
            }
            Consumer::MuTilde(b, s) => {
                let mut inner = sc.clone();
                inner.vars.insert(b.name.clone(), v);
                self.statement(s, &inner)
            }
            Consumer::Case(arms) => self.switch(v, arms, sc),
            Consumer::Method(m, args, conts) => {
                let tag = self.method(m)?;
                let mut sc = sc.clone();
                let mut steps = Vec::new();
                let mut sel = Vec::new();
                for a in args {
                    sel.push(self.atom(a, &mut sc, &mut steps)?);
                }
                for k in conts {
                    sel.push(self.reify(k, &mut sc, &mut steps)?);
                }
                sel.push(v);
                let params = self.distinct(&sel, Some(v));
                Ok(wrap(
                    steps,
                    S::Substitute(
                        sel,
                        Box::new(Block {
                            params,
                            body: S::Invoke(v, tag),
                        }),
                    ),
                ))
            }
        }
    }

    fn switch(&mut self, v: Name, arms: &[Arm], sc: &Scope) -> R<S> {
        let mut out = Vec::new();
        let mut default = None;
        for arm in arms {
            let tag = match &arm.pattern {
                Pattern::Con(k) => self.tag_of(&k.to_string()),
                Pattern::Tuple => self.tag_of(meadow_rt::roles::TUPLE),
                Pattern::Default => {
                    default = Some(Block {
                        params: sc.env.clone(),
                        body: self.statement(&arm.body, sc)?,
                    });
                    continue;
                }
            };
            // An arm's parameters are the fields, then the environment.
            let mut inner = Scope {
                env: Vec::new(),
                vars: sc.vars.clone(),
                halt: sc.halt,
            };
            for f in &arm.fields {
                let n = self.fresh(rep_of(&f.rep)?);
                inner.bind(&f.name, n);
            }
            inner.env.extend(sc.env.iter().copied());
            let body = self.statement(&arm.body, &inner)?;
            out.push((
                tag,
                Block {
                    params: inner.env,
                    body,
                },
            ));
        }
        let default = default.unwrap_or_else(|| Block {
            params: sc.env.clone(),
            body: S::Error("no arm of the case matches"),
        });
        Ok(S::Switch {
            scrutinee: v,
            arms: out,
            default: Box::new(default),
        })
    }

    /// The name of a consumer that is one.
    fn named(&self, c: &Consumer, sc: &Scope) -> R<Name> {
        match c {
            Consumer::Var(k) => sc.var(k),
            Consumer::Halt => sc
                .halt
                .ok_or_else(|| "`halt` outside a top-level value".to_string()),
            _ => Err("not a name".to_string()),
        }
    }

    /// `c` by a name: itself, or an object of one method made of it, which
    /// captures what it mentions.
    fn reify(&mut self, c: &Consumer, sc: &mut Scope, steps: &mut Vec<Step>) -> R<Name> {
        if matches!(c, Consumer::Var(_) | Consumer::Halt) {
            return self.named(c, sc);
        }
        let mut free = Free::default();
        free.consumer(c);
        let captures = free.among(sc);
        let rep = match c {
            Consumer::MuTilde(b, _) => rep_of(&b.rep)?,
            _ => ax::Rep::Ref,
        };
        let x = self.fresh(rep);
        let mut inner = Scope {
            env: captures.clone(),
            vars: sc.vars.clone(),
            halt: sc.halt,
        };
        inner.env.push(x);
        let body = self.give(x, c, &inner)?;
        let k = self.fresh(ax::Rep::Ref);
        self.out.continuations.insert(k);
        steps.push(Step::New(
            k,
            captures,
            vec![Block {
                params: inner.env,
                body,
            }],
        ));
        sc.push(k);
        Ok(k)
    }

    /// `p` by a name: one it has, or one a step binds it to.
    fn atom(&mut self, p: &Producer, sc: &mut Scope, steps: &mut Vec<Step>) -> R<Name> {
        let lit = |l: &mut Lower, sc: &mut Scope, steps: &mut Vec<Step>, lit: Lit, rep: ax::Rep| {
            let n = l.fresh(rep);
            sc.push(n);
            steps.push(Step::Extern(Extern::Lit(lit), Vec::new(), sc.env.clone()));
            n
        };
        Ok(match p {
            Producer::Var(x) => sc.var(x)?,
            Producer::Int(i) => lit(self, sc, steps, Lit::Int(*i), ax::Rep::Int),
            Producer::Float(x) => lit(self, sc, steps, Lit::Float(*x), ax::Rep::Float),
            Producer::Char(c) => lit(self, sc, steps, Lit::Char(*c), ax::Rep::Bits(desc::CHAR)),
            Producer::Bool(b) => lit(self, sc, steps, Lit::Bool(*b), ax::Rep::Bits(desc::BOOL)),
            Producer::Unit => lit(self, sc, steps, Lit::Unit, ax::Rep::Bits(desc::UNIT)),
            Producer::Str(s) => lit(
                self,
                sc,
                steps,
                Lit::Str(InternedString::from(s.as_str())),
                ax::Rep::Str,
            ),
            Producer::Val(v) => {
                let Some(&(place, rep)) = self.vals.get(v) else {
                    return Err(format!("`{v}` is not a value of this program"));
                };
                let at = lit(self, sc, steps, Lit::Int(place), ax::Rep::Int);
                let n = self.fresh(rep);
                sc.push(n);
                steps.push(Step::Extern(
                    Extern::Prim(Prim::GlobalGet),
                    vec![at],
                    sc.env.clone(),
                ));
                n
            }
            Producer::Con(k, args) => {
                let name = k.to_string();
                self.data(&name, args, sc, steps)?
            }
            Producer::Tuple(items) => self.data(meadow_rt::roles::TUPLE, items, sc, steps)?,
            Producer::Array(items) => {
                let mut args = Vec::new();
                for x in items {
                    args.push(self.atom(x, sc, steps)?);
                }
                let n = self.fresh(ax::Rep::Ref);
                sc.push(n);
                steps.push(Step::Extern(Extern::Array, args, sc.env.clone()));
                n
            }
            Producer::Record(fields) => {
                let mut args = Vec::new();
                for (_, x) in fields {
                    args.push(self.atom(x, sc, steps)?);
                }
                let labels = fields
                    .iter()
                    .map(|(l, _)| InternedString::from(l.as_str()))
                    .collect();
                let n = self.fresh(ax::Rep::Ref);
                sc.push(n);
                steps.push(Step::Extern(Extern::Record(labels), args, sc.env.clone()));
                n
            }
            Producer::Cocase(methods) => {
                let mut free = Free::default();
                for m in methods {
                    free.method(m);
                }
                let captures = free.among(sc);
                let mut blocks: Vec<Block> = (0..self.methods.len())
                    .map(|_| Block {
                        params: Vec::new(),
                        body: S::Error("the object has no such method"),
                    })
                    .collect();
                for m in methods {
                    let mut inner = Scope {
                        env: captures.clone(),
                        vars: sc.vars.clone(),
                        halt: sc.halt,
                    };
                    for b in &m.params {
                        let n = self.fresh(rep_of(&b.rep)?);
                        inner.bind(&b.name, n);
                    }
                    for k in &m.conts {
                        let n = self.fresh(ax::Rep::Ref);
                        inner.bind(k, n);
                        self.out.returns.insert(n);
                    }
                    let params = inner.env.clone();
                    let body = self.statement(&m.body, &inner)?;
                    let tag = self.method(&m.name)? as usize;
                    blocks[tag] = Block { params, body };
                }
                let n = self.fresh(ax::Rep::Ref);
                sc.push(n);
                steps.push(Step::New(n, captures, blocks));
                n
            }
            Producer::Mu(..) => {
                return Err(
                    "a `μ` that is an argument is not lowered yet: nothing says how its value is represented"
                        .to_string(),
                );
            }
            Producer::Desc(_) => {
                return Err("a descriptor is not lowered yet".to_string());
            }
        })
    }

    fn data(
        &mut self,
        ctor: &str,
        args: &[Producer],
        sc: &mut Scope,
        steps: &mut Vec<Step>,
    ) -> R<Name> {
        let mut fields = Vec::new();
        for a in args {
            fields.push(self.atom(a, sc, steps)?);
        }
        let tag = self.tag_of(ctor);
        let n = self.fresh(ax::Rep::Ref);
        sc.push(n);
        steps.push(Step::Let(n, tag, InternedString::from(ctor), fields));
        Ok(n)
    }
}

/// How a representation of Cut's is one of AxCut's.
fn rep_of(r: &Rep) -> R<ax::Rep> {
    use meadow_rt::num::Width;
    Ok(match r {
        Rep::I64 => ax::Rep::Int,
        Rep::F64 => ax::Rep::Float,
        Rep::F32 => ax::Rep::Bits(desc::FLOAT32),
        Rep::I8 => ax::Rep::Bits(desc::word(Width::I8)),
        Rep::I16 => ax::Rep::Bits(desc::word(Width::I16)),
        Rep::I32 => ax::Rep::Bits(desc::word(Width::I32)),
        Rep::U8 => ax::Rep::Bits(desc::word(Width::U8)),
        Rep::U16 => ax::Rep::Bits(desc::word(Width::U16)),
        Rep::U32 => ax::Rep::Bits(desc::word(Width::U32)),
        Rep::U64 => ax::Rep::Bits(desc::word(Width::U64)),
        Rep::Bool => ax::Rep::Bits(desc::BOOL),
        Rep::Char => ax::Rep::Bits(desc::CHAR),
        Rep::Unit => ax::Rep::Bits(desc::UNIT),
        Rep::Str => ax::Rep::Str,
        Rep::Ptr | Rep::Any => ax::Rep::Ref,
        Rep::Sym | Rep::Desc | Rep::Var(_) => {
            return Err(format!("a value represented as `{r}` is not lowered yet"));
        }
    })
}

/// The primitive Cut calls `name`: `meadow_rt::Prim`'s name, its first
/// letter small, and a conversion to a sized integer by its type's.
fn prim_named(name: &str) -> Option<Prim> {
    if let Some(p) = Prim::from_name(name)
        && matches!(p, Prim::ToWord(_))
    {
        return Some(p);
    }
    (0..u16::MAX).map_while(Prim::from_code).find(|p| {
        let shown = format!("{p:?}");
        let mut cs = shown.chars();
        cs.next().is_some_and(|c| {
            !shown.contains('(') && name == format!("{}{}", c.to_lowercase(), cs.as_str())
        })
    })
}

/// Whether `s` mentions the value or continuation named `name`.
fn mentions(s: &Statement, name: &str) -> bool {
    let mut free = Free::default();
    free.statement(s);
    free.names.contains(name)
}

/// The names something mentions and does not bind, and whether `halt` is
/// one of them.
#[derive(Default)]
struct Free {
    names: HashSet<String>,
    bound: Vec<String>,
    halt: bool,
}

impl Free {
    /// What was found, of the environment: in its order, each once.
    fn among(&self, sc: &Scope) -> Vec<Name> {
        let mut wanted: HashSet<Name> = self
            .names
            .iter()
            .filter_map(|n| sc.vars.get(n).copied())
            .collect();
        if self.halt {
            wanted.extend(sc.halt);
        }
        let mut seen = HashSet::new();
        sc.env
            .iter()
            .copied()
            .filter(|n| wanted.contains(n) && seen.insert(*n))
            .collect()
    }

    fn name(&mut self, n: &str) {
        if !self.bound.iter().any(|b| b == n) {
            self.names.insert(n.to_string());
        }
    }

    fn under(&mut self, names: &[&str], f: impl FnOnce(&mut Free)) {
        let mark = self.bound.len();
        self.bound.extend(names.iter().map(|n| n.to_string()));
        f(self);
        self.bound.truncate(mark);
    }

    fn method(&mut self, m: &crate::Method) {
        let names: Vec<&str> = m
            .params
            .iter()
            .map(|b| b.name.as_str())
            .chain(m.conts.iter().map(String::as_str))
            .collect();
        self.under(&names, |f| f.statement(&m.body));
    }

    fn producer(&mut self, p: &Producer) {
        match p {
            Producer::Var(x) => self.name(x),
            Producer::Con(_, xs) | Producer::Tuple(xs) | Producer::Array(xs) => {
                xs.iter().for_each(|x| self.producer(x))
            }
            Producer::Record(fs) => fs.iter().for_each(|(_, x)| self.producer(x)),
            Producer::Mu(k, s) => self.under(&[k], |f| f.statement(s)),
            Producer::Cocase(ms) => ms.iter().for_each(|m| self.method(m)),
            Producer::Val(_)
            | Producer::Int(_)
            | Producer::Float(_)
            | Producer::Char(_)
            | Producer::Str(_)
            | Producer::Bool(_)
            | Producer::Unit
            | Producer::Desc(_) => {}
        }
    }

    fn consumer(&mut self, c: &Consumer) {
        match c {
            Consumer::Var(k) => self.name(k),
            Consumer::Halt => self.halt = true,
            Consumer::MuTilde(b, s) => self.under(&[&b.name], |f| f.statement(s)),
            Consumer::Case(arms) => {
                for arm in arms {
                    let names: Vec<&str> = arm.fields.iter().map(|b| b.name.as_str()).collect();
                    self.under(&names, |f| f.statement(&arm.body));
                }
            }
            Consumer::Method(_, args, conts) => {
                args.iter().for_each(|a| self.producer(a));
                conts.iter().for_each(|k| self.consumer(k));
            }
        }
    }

    fn statement(&mut self, s: &Statement) {
        match s {
            Statement::Cut(p, c) => {
                self.producer(p);
                self.consumer(c);
            }
            Statement::Call(_, args, conts) | Statement::Prim(_, args, conts) => {
                args.iter().for_each(|a| self.producer(a));
                conts.iter().for_each(|k| self.consumer(k));
            }
            Statement::Let(b, p, rest) => {
                self.producer(p);
                self.under(&[&b.name], |f| f.statement(rest));
            }
            Statement::Perform(_, args, c) => {
                args.iter().for_each(|a| self.producer(a));
                self.consumer(c);
            }
            Statement::Handle(h) => {
                for c in &h.clauses {
                    let names: Vec<&str> = c
                        .params
                        .iter()
                        .map(|b| b.name.as_str())
                        .chain([c.resumption.as_str(), c.cont.as_str()])
                        .collect();
                    self.under(&names, |f| f.statement(&c.body));
                }
                let (x, k, body) = &h.ret;
                self.under(&[&x.name, k], |f| f.statement(body));
                self.under(&[&h.body_cont], |f| f.statement(&h.body));
                self.consumer(&h.cont);
            }
            Statement::Error(_) => {}
        }
    }
}

/// Every method an object of `s` has or a call in it names.
fn methods_of(s: &Statement, out: &mut HashSet<String>) {
    fn producer(p: &Producer, out: &mut HashSet<String>) {
        match p {
            Producer::Con(_, xs) | Producer::Tuple(xs) | Producer::Array(xs) => {
                xs.iter().for_each(|x| producer(x, out))
            }
            Producer::Record(fs) => fs.iter().for_each(|(_, x)| producer(x, out)),
            Producer::Mu(_, s) => methods_of(s, out),
            Producer::Cocase(ms) => {
                for m in ms {
                    out.insert(m.name.clone());
                    methods_of(&m.body, out);
                }
            }
            _ => {}
        }
    }
    fn consumer(c: &Consumer, out: &mut HashSet<String>) {
        match c {
            Consumer::Var(_) | Consumer::Halt => {}
            Consumer::MuTilde(_, s) => methods_of(s, out),
            Consumer::Case(arms) => arms.iter().for_each(|a| methods_of(&a.body, out)),
            Consumer::Method(m, args, conts) => {
                out.insert(m.clone());
                args.iter().for_each(|a| producer(a, out));
                conts.iter().for_each(|k| consumer(k, out));
            }
        }
    }
    match s {
        Statement::Cut(p, c) => {
            producer(p, out);
            consumer(c, out);
        }
        Statement::Call(_, args, conts) | Statement::Prim(_, args, conts) => {
            args.iter().for_each(|a| producer(a, out));
            conts.iter().for_each(|k| consumer(k, out));
        }
        Statement::Let(_, p, rest) => {
            producer(p, out);
            methods_of(rest, out);
        }
        Statement::Perform(_, args, c) => {
            args.iter().for_each(|a| producer(a, out));
            consumer(c, out);
        }
        Statement::Handle(h) => {
            h.clauses.iter().for_each(|c| methods_of(&c.body, out));
            methods_of(&h.ret.2, out);
            methods_of(&h.body, out);
            consumer(&h.cont, out);
        }
        Statement::Error(_) => {}
    }
}
