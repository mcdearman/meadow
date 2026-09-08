//! A **CEK abstract machine** for `meadow_core`.
//!
//! The evaluator is an explicit `(Control, Environment, Kontinuation)` loop rather
//! than a recursive tree-walk. The continuation is a `Vec<K>` of stack frames —
//! one per "hole" in a partly-evaluated term — which is what makes algebraic
//! effects implementable:
//!
//! * `perform E.op arg` scans the kontinuation top-down for the nearest matching
//!   `HandleMark`, **splits the stack there**, and hands the sliced-off prefix to
//!   the handler clause as a resumption ([`Value::Cont`]).
//! * resuming (calling that `Value::Cont`) splices the captured frames back on.
//!
//! Handlers are **deep** (the captured slice includes the `HandleMark`, so a
//! resumption re-enters under the same handler) and **one-shot** (each `Cont` may
//! be resumed at most once — enforced by a `take`n `Option`).

use meadow_core as core;
use meadow_core::fmt_float;
use meadow_intern::InternedString;
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

type Term = core::Term;
type Var = core::Var;

#[derive(Debug, Clone)]
pub enum Value {
    /// Fixed-width integer (`Int`, i.e. i64).
    Int(i64),
    /// Arbitrary-precision integer (`BigInt`) — produced by `toBigInt` and the
    /// `~`-suffixed operators, never by a literal.
    BigInt(BigInt),
    Float(f64),
    Bool(bool),
    Str(InternedString),
    Unit,
    Tuple(Vec<Value>),
    /// The one builtin collection: a persistent, `Rc`-shared contiguous buffer.
    /// `Rc` gives O(1) clone + structural sharing; `Rc::make_mut` lets `arraySet`
    /// / `arrayPush` mutate in place when the buffer is uniquely held.
    Array(Rc<Vec<Value>>),
    List(Vec<Value>),
    Record(BTreeMap<InternedString, Value>),
    Ctor(InternedString, Vec<Value>),
    Closure {
        param: Var,
        body: Rc<Term>,
        env: Env,
    },
    /// A partially applied primitive.
    Builtin {
        op: core::Prim,
        args: Vec<Value>,
    },
    /// A captured (one-shot, deep) continuation — a slice of stack frames.
    Cont(Rc<RefCell<Option<Vec<K>>>>),
}

#[derive(Debug)]
pub struct RuntimeError {
    pub msg: String,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime error: {}", self.msg)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, RuntimeError> {
    Err(RuntimeError { msg: msg.into() })
}

// --- environments ----------------------------------------------------------

#[derive(Debug)]
pub struct Frame {
    slots: RefCell<Vec<(Var, Value)>>,
    parent: Option<Env>,
}

pub type Env = Rc<Frame>;

fn root_env() -> Env {
    Rc::new(Frame {
        slots: RefCell::new(Vec::new()),
        parent: None,
    })
}

fn child(parent: &Env) -> Env {
    Rc::new(Frame {
        slots: RefCell::new(Vec::new()),
        parent: Some(parent.clone()),
    })
}

fn define(env: &Env, var: Var, val: Value) {
    env.slots.borrow_mut().push((var, val));
}

fn lookup(env: &Env, var: Var) -> Option<Value> {
    let mut cur = Some(env.clone());
    while let Some(frame) = cur {
        if let Some((_, v)) = frame.slots.borrow().iter().rev().find(|(k, _)| *k == var) {
            return Some(v.clone());
        }
        cur = frame.parent.clone();
    }
    None
}

// --- the machine ---------------------------------------------------------

pub type FieldTable = std::collections::HashMap<InternedString, Vec<InternedString>>;

#[derive(Debug, Clone)]
pub struct HandlerData {
    clauses: Vec<core::HClause>,
    ret: Option<(Var, Rc<Term>)>,
}

/// One continuation frame: "given the value of the sub-expression currently being
/// evaluated, here is what to do next".
#[derive(Debug, Clone)]
pub enum K {
    /// `App`: the function is evaluated; evaluate this argument next.
    EvalArg {
        arg: Rc<Term>,
        env: Env,
    },
    /// `App`: the argument is evaluated; apply the saved function to it.
    ApplyTo {
        func: Value,
    },
    If {
        then: Rc<Term>,
        els: Rc<Term>,
        env: Env,
    },
    /// `Let` / `Lam` application: bind `var` to the incoming value, run `body`.
    Bind {
        var: Var,
        body: Rc<Term>,
        env: Env,
    },
    LetRec {
        scope: Env,
        pending: Vec<(Var, Term)>,
        body: Rc<Term>,
    },
    BuildTuple {
        done: Vec<Value>,
        pending: Vec<Term>,
        env: Env,
    },
    BuildArray {
        done: Vec<Value>,
        pending: Vec<Term>,
        env: Env,
    },
    BuildList {
        done: Vec<Value>,
        pending: Vec<Term>,
        env: Env,
    },
    BuildCtor {
        name: InternedString,
        done: Vec<Value>,
        pending: Vec<Term>,
        env: Env,
    },
    BuildPrim {
        op: core::Prim,
        done: Vec<Value>,
        pending: Vec<Term>,
        env: Env,
    },
    BuildRecord {
        done: Vec<(InternedString, Value)>,
        pending: Vec<(InternedString, Term)>,
        env: Env,
    },
    Proj(usize),
    Sel(InternedString),
    /// `Extend`: the record is evaluated; evaluate the new field value.
    ExtendVal {
        label: InternedString,
        val: Rc<Term>,
        env: Env,
    },
    /// `Extend`: the field value is evaluated; insert it into the saved record.
    ExtendWith {
        label: InternedString,
        rec: Value,
    },
    /// `ListCons`: the head is evaluated; evaluate the tail.
    ConsHead {
        tail: Rc<Term>,
        env: Env,
    },
    /// `ListCons`: the tail is evaluated; prepend the saved head.
    ConsBuild {
        head: Value,
    },
    Match {
        arms: Rc<Vec<(core::Pat, Term)>>,
        env: Env,
    },
    /// `Perform`: the operation argument is evaluated; unwind to a handler.
    PerformWith {
        effect: InternedString,
        op: InternedString,
    },
    /// A handler boundary sitting on the stack.
    HandleMark(Rc<HandlerData>, Env),
}

enum Control {
    Eval(Rc<Term>, Env),
    Ret(Value),
}

struct Machine<'a> {
    ctrl: Control,
    kont: Vec<K>,
    fields: &'a FieldTable,
}

pub fn run(program: &core::Program) -> Result<Value, RuntimeError> {
    let env = root_env();
    // Placeholders first so recursive top-level references resolve.
    for def in &program.defs {
        define(&env, def.var, Value::Unit);
    }
    for def in &program.defs {
        let m = Machine {
            ctrl: Control::Eval(Rc::new(def.term.clone()), env.clone()),
            kont: Vec::new(),
            fields: &program.ctor_fields,
        };
        let v = m.run()?;
        define(&env, def.var, v);
    }
    match program.entry {
        Some(e) => lookup(&env, e).ok_or_else(|| RuntimeError {
            msg: "entry point not found".into(),
        }),
        None => Ok(Value::Unit),
    }
}

impl Machine<'_> {
    fn run(mut self) -> Result<Value, RuntimeError> {
        loop {
            if self.kont.is_empty() {
                if let Control::Ret(_) = &self.ctrl {
                    match std::mem::replace(&mut self.ctrl, Control::Ret(Value::Unit)) {
                        Control::Ret(v) => return Ok(v),
                        Control::Eval(..) => unreachable!(),
                    }
                }
            }
            self.step()?;
        }
    }

    fn step(&mut self) -> Result<(), RuntimeError> {
        match std::mem::replace(&mut self.ctrl, Control::Ret(Value::Unit)) {
            Control::Eval(term, env) => self.eval(term, env),
            Control::Ret(v) => self.ret(v),
        }
    }

    /// Decompose a term: push frames for its sub-expressions, then start on the
    /// first one; or, for a value form, return it directly.
    fn eval(&mut self, term: Rc<Term>, env: Env) -> Result<(), RuntimeError> {
        use core::Term as T;
        match &*term {
            T::Var(v) => {
                let val = lookup(&env, *v).ok_or_else(|| RuntimeError {
                    msg: format!("unbound variable {v:?}"),
                })?;
                self.ctrl = Control::Ret(val);
            }
            T::Lit(l) => self.ctrl = Control::Ret(lit_value(l)),
            T::Lam(param, body) => {
                self.ctrl = Control::Ret(Value::Closure {
                    param: *param,
                    body: body.clone(),
                    env,
                });
            }
            T::App(f, a) => {
                self.kont.push(K::EvalArg {
                    arg: a.clone(),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(f.clone(), env);
            }
            T::Let(v, rhs, body) => {
                self.kont.push(K::Bind {
                    var: *v,
                    body: body.clone(),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(rhs.clone(), env);
            }
            T::LetRec(binds, body) => {
                let scope = child(&env);
                for (v, _) in binds {
                    define(&scope, *v, Value::Unit);
                }
                // `pending` holds the not-yet-evaluated binds, innermost last; the
                // last entry is always the one currently being evaluated (its slot
                // gets filled when its value comes back — see `K::LetRec` in `ret`).
                let pending: Vec<(Var, Term)> = binds.iter().rev().cloned().collect();
                match pending.last().cloned() {
                    Some((_, rhs)) => {
                        self.kont.push(K::LetRec {
                            scope: scope.clone(),
                            pending,
                            body: body.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(rhs), scope);
                    }
                    None => {
                        let _ = pending;
                        self.ctrl = Control::Eval(body.clone(), scope);
                    }
                }
            }
            T::If(c, t, e) => {
                self.kont.push(K::If {
                    then: t.clone(),
                    els: e.clone(),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(c.clone(), env);
            }
            T::Tuple(items) => self.start_seq(items, env, SeqKind::Tuple),
            T::Array(items) => self.start_seq(items, env, SeqKind::Array),
            T::List(items) => self.start_seq(items, env, SeqKind::List),
            T::Ctor(name, args) => self.start_seq(args, env, SeqKind::Ctor(*name)),
            T::Prim(op, args) => self.start_seq(args, env, SeqKind::Prim(*op)),
            T::Record(fields) => {
                let pending: Vec<(InternedString, Term)> =
                    fields.iter().rev().map(|(l, t)| (*l, t.clone())).collect();
                match pending.last().cloned() {
                    Some((_, first)) => {
                        self.kont.push(K::BuildRecord {
                            done: vec![],
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(first), env);
                    }
                    None => self.ctrl = Control::Ret(Value::Record(BTreeMap::new())),
                }
            }
            T::Proj(t, i) => {
                self.kont.push(K::Proj(*i));
                self.ctrl = Control::Eval(t.clone(), env);
            }
            T::Sel(t, label) => {
                self.kont.push(K::Sel(*label));
                self.ctrl = Control::Eval(t.clone(), env);
            }
            T::Extend(rec, label, val) => {
                self.kont.push(K::ExtendVal {
                    label: *label,
                    val: val.clone(),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(rec.clone(), env);
            }
            T::ListCons(head, tail) => {
                self.kont.push(K::ConsHead {
                    tail: tail.clone(),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(head.clone(), env);
            }
            T::Case(scrut, arms) => {
                self.kont.push(K::Match {
                    arms: Rc::new(arms.clone()),
                    env: env.clone(),
                });
                self.ctrl = Control::Eval(scrut.clone(), env);
            }
            T::Perform(effect, op, arg) => {
                self.kont.push(K::PerformWith {
                    effect: *effect,
                    op: *op,
                });
                self.ctrl = Control::Eval(arg.clone(), env);
            }
            T::Handle { body, clauses, ret } => {
                let data = Rc::new(HandlerData {
                    clauses: clauses.clone(),
                    ret: ret.clone(),
                });
                self.kont.push(K::HandleMark(data, env.clone()));
                self.ctrl = Control::Eval(body.clone(), env);
            }
            T::Error => return err("evaluating an ill-formed expression"),
        }
        Ok(())
    }

    fn start_seq(&mut self, items: &[Term], env: Env, kind: SeqKind) {
        let mut pending: Vec<Term> = items.iter().rev().cloned().collect();
        match pending.pop() {
            Some(first) => {
                let done = Vec::new();
                self.kont.push(match kind {
                    SeqKind::Tuple => K::BuildTuple {
                        done,
                        pending,
                        env: env.clone(),
                    },
                    SeqKind::Array => K::BuildArray {
                        done,
                        pending,
                        env: env.clone(),
                    },
                    SeqKind::List => K::BuildList {
                        done,
                        pending,
                        env: env.clone(),
                    },
                    SeqKind::Ctor(name) => K::BuildCtor {
                        name,
                        done,
                        pending,
                        env: env.clone(),
                    },
                    SeqKind::Prim(op) => K::BuildPrim {
                        op,
                        done,
                        pending,
                        env: env.clone(),
                    },
                });
                self.ctrl = Control::Eval(Rc::new(first), env);
            }
            None => {
                self.ctrl = Control::Ret(match kind {
                    SeqKind::Tuple => Value::Tuple(vec![]),
                    SeqKind::Array => Value::Array(Rc::new(vec![])),
                    SeqKind::List => Value::List(vec![]),
                    SeqKind::Ctor(name) => Value::Ctor(name, vec![]),
                    SeqKind::Prim(_) => Value::Unit, // prims always have args
                });
            }
        }
    }

    /// A value came back; pop the top frame and combine.
    fn ret(&mut self, v: Value) -> Result<(), RuntimeError> {
        let Some(frame) = self.kont.pop() else {
            self.ctrl = Control::Ret(v);
            return Ok(());
        };
        match frame {
            K::EvalArg { arg, env } => {
                self.kont.push(K::ApplyTo { func: v });
                self.ctrl = Control::Eval(arg, env);
            }
            K::ApplyTo { func } => self.apply(func, v)?,
            K::If { then, els, env } => match v {
                Value::Bool(true) => self.ctrl = Control::Eval(then, env),
                Value::Bool(false) => self.ctrl = Control::Eval(els, env),
                other => return err(format!("`if` condition is not a Bool: {other}")),
            },
            K::Bind { var, body, env } => {
                let scope = child(&env);
                define(&scope, var, v);
                self.ctrl = Control::Eval(body, scope);
            }
            K::LetRec {
                scope,
                mut pending,
                body,
            } => {
                // the just-evaluated bind is the last `pending` entry
                let (var, _) = pending.pop().expect("letrec marker");
                define(&scope, var, v);
                match pending.last().cloned() {
                    Some((_, next_rhs)) => {
                        self.kont.push(K::LetRec {
                            scope: scope.clone(),
                            pending,
                            body,
                        });
                        self.ctrl = Control::Eval(Rc::new(next_rhs), scope);
                    }
                    None => self.ctrl = Control::Eval(body, scope),
                }
            }
            K::BuildTuple {
                mut done,
                mut pending,
                env,
            } => {
                done.push(v);
                match pending.pop() {
                    Some(next) => {
                        self.kont.push(K::BuildTuple {
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => self.ctrl = Control::Ret(Value::Tuple(done)),
                }
            }
            K::BuildArray {
                mut done,
                mut pending,
                env,
            } => {
                done.push(v);
                match pending.pop() {
                    Some(next) => {
                        self.kont.push(K::BuildArray {
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => self.ctrl = Control::Ret(Value::Array(Rc::new(done))),
                }
            }
            K::BuildList {
                mut done,
                mut pending,
                env,
            } => {
                done.push(v);
                match pending.pop() {
                    Some(next) => {
                        self.kont.push(K::BuildList {
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => self.ctrl = Control::Ret(Value::List(done)),
                }
            }
            K::BuildCtor {
                name,
                mut done,
                mut pending,
                env,
            } => {
                done.push(v);
                match pending.pop() {
                    Some(next) => {
                        self.kont.push(K::BuildCtor {
                            name,
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => self.ctrl = Control::Ret(Value::Ctor(name, done)),
                }
            }
            K::BuildPrim {
                op,
                mut done,
                mut pending,
                env,
            } => {
                done.push(v);
                match pending.pop() {
                    Some(next) => {
                        self.kont.push(K::BuildPrim {
                            op,
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => self.ctrl = Control::Ret(run_prim(op, done)?),
                }
            }
            K::BuildRecord {
                mut done,
                mut pending,
                env,
            } => {
                let (label, _) = pending.pop().expect("record marker");
                done.push((label, v));
                match pending.last().cloned() {
                    Some((_, next)) => {
                        self.kont.push(K::BuildRecord {
                            done,
                            pending,
                            env: env.clone(),
                        });
                        self.ctrl = Control::Eval(Rc::new(next), env);
                    }
                    None => {
                        self.ctrl = Control::Ret(Value::Record(done.into_iter().collect()));
                    }
                }
            }
            K::Proj(i) => match v {
                Value::Tuple(items) => {
                    let val = items.into_iter().nth(i).ok_or_else(|| RuntimeError {
                        msg: format!("tuple projection {i} out of range"),
                    })?;
                    self.ctrl = Control::Ret(val);
                }
                other => return err(format!("cannot project field {i} out of {other}")),
            },
            K::Sel(label) => {
                let val = match v {
                    Value::Record(map) => map.get(&label).cloned().ok_or_else(|| RuntimeError {
                        msg: format!("record has no field `{label}`"),
                    })?,
                    Value::Ctor(cname, vals) => self
                        .fields
                        .get(&cname)
                        .and_then(|fs| fs.iter().position(|f| *f == label))
                        .and_then(|i| vals.into_iter().nth(i))
                        .ok_or_else(|| RuntimeError {
                            msg: format!("`{cname}` has no field `{label}`"),
                        })?,
                    other => return err(format!("cannot select `.{label}` from {other}")),
                };
                self.ctrl = Control::Ret(val);
            }
            K::ExtendVal { label, val, env } => {
                self.kont.push(K::ExtendWith { label, rec: v });
                self.ctrl = Control::Eval(val, env);
            }
            K::ExtendWith { label, rec } => match rec {
                Value::Record(mut map) => {
                    map.insert(label, v);
                    self.ctrl = Control::Ret(Value::Record(map));
                }
                other => return err(format!("cannot extend non-record {other}")),
            },
            K::ConsHead { tail, env } => {
                self.kont.push(K::ConsBuild { head: v });
                self.ctrl = Control::Eval(tail, env);
            }
            K::ConsBuild { head } => match v {
                Value::List(mut items) => {
                    items.insert(0, head);
                    self.ctrl = Control::Ret(Value::List(items));
                }
                other => return err(format!("`Cons` tail is not a list: {other}")),
            },
            K::Match { arms, env } => {
                for (pat, body) in arms.iter() {
                    let scope = child(&env);
                    if match_pat(pat, &v, &scope) {
                        self.ctrl = Control::Eval(Rc::new(body.clone()), scope);
                        return Ok(());
                    }
                }
                return err("non-exhaustive pattern match");
            }
            K::PerformWith { effect, op } => self.perform(effect, op, v)?,
            K::HandleMark(data, henv) => {
                // body returned normally — run the `return` clause (or identity)
                match &data.ret {
                    Some((param, body)) => {
                        let scope = child(&henv);
                        define(&scope, *param, v);
                        self.ctrl = Control::Eval(body.clone(), scope);
                    }
                    None => self.ctrl = Control::Ret(v),
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, func: Value, arg: Value) -> Result<(), RuntimeError> {
        match func {
            Value::Closure { param, body, env } => {
                let scope = child(&env);
                define(&scope, param, arg);
                self.ctrl = Control::Eval(body, scope);
            }
            Value::Builtin { op, mut args } => {
                args.push(arg);
                self.ctrl = if args.len() >= op.arity() {
                    Control::Ret(run_prim(op, args)?)
                } else {
                    Control::Ret(Value::Builtin { op, args })
                };
            }
            Value::Cont(frames) => {
                let Some(saved) = frames.borrow_mut().take() else {
                    return err("continuation resumed more than once");
                };
                self.kont.extend(saved);
                self.ctrl = Control::Ret(arg);
            }
            other => return err(format!("{other} is not a function")),
        }
        Ok(())
    }

    /// Handle a `perform`: unwind the kontinuation to the nearest matching handler,
    /// capture the sliced-off prefix as a resumption, and run the handler clause.
    fn perform(
        &mut self,
        effect: InternedString,
        op: InternedString,
        arg: Value,
    ) -> Result<(), RuntimeError> {
        // find the nearest HandleMark (from the top) with a matching clause
        let idx = self.kont.iter().rposition(|k| {
            matches!(k, K::HandleMark(data, _)
                if data.clauses.iter().any(|c| c.effect == effect && c.op == op))
        });
        let Some(idx) = idx else {
            // No user handler: the runtime discharges a few built-in effects
            // itself. `Std.Fs` operations hit the real filesystem here (a `handle`
            // would have matched above and taken precedence).
            if &*effect == "Fs" {
                let result = native_fs(&op, arg)?;
                self.ctrl = Control::Ret(result);
                return Ok(());
            }
            if &*effect == "Process" {
                let result = native_process(&op, arg)?;
                self.ctrl = Control::Ret(result);
                return Ok(());
            }
            return err(format!("unhandled effect {effect}.{op}"));
        };

        // frames [idx..] — including the HandleMark itself, so a resumption
        // re-enters under the same handler (deep handlers)
        let captured = self.kont.split_off(idx);
        let (data, henv) = match &captured[0] {
            K::HandleMark(d, e) => (d.clone(), e.clone()),
            _ => unreachable!(),
        };
        let clause = data
            .clauses
            .iter()
            .find(|c| c.effect == effect && c.op == op)
            .expect("matched above")
            .clone();

        let cont = Value::Cont(Rc::new(RefCell::new(Some(captured))));
        let scope = child(&henv);
        define(&scope, clause.param, arg);
        define(&scope, clause.resume, cont);
        self.ctrl = Control::Eval(Rc::new(clause.body), scope);
        Ok(())
    }
}

enum SeqKind {
    Tuple,
    Array,
    List,
    Ctor(InternedString),
    Prim(core::Prim),
}

fn lit_value(lit: &core::Lit) -> Value {
    match lit {
        core::Lit::Int(i) => Value::Int(*i),
        core::Lit::BigInt(i) => Value::BigInt(BigInt::from(*i)),
        core::Lit::Float(x) => Value::Float(*x),
        core::Lit::Str(s) => Value::Str(*s),
        core::Lit::Bool(b) => Value::Bool(*b),
        core::Lit::Unit => Value::Unit,
    }
}

// --- pattern matching ------------------------------------------------------

fn match_pat(pat: &core::Pat, value: &Value, scope: &Env) -> bool {
    use core::Pat as P;
    match (pat, value) {
        (P::Wild, _) => true,
        (P::Var(v), _) => {
            define(scope, *v, value.clone());
            true
        }
        (P::As(v, sub), _) => {
            define(scope, *v, value.clone());
            match_pat(sub, value, scope)
        }
        (P::Lit(core::Lit::Int(a)), Value::Int(b)) => a == b,
        (P::Lit(core::Lit::BigInt(a)), Value::BigInt(b)) => &BigInt::from(*a) == b,
        (P::Lit(core::Lit::Float(a)), Value::Float(b)) => a == b,
        (P::Lit(core::Lit::Str(a)), Value::Str(b)) => a == b,
        (P::Lit(core::Lit::Bool(a)), Value::Bool(b)) => a == b,
        (P::Lit(core::Lit::Unit), Value::Unit) => true,
        (P::Tuple(ps), Value::Tuple(vs)) if ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::Array(ps), Value::Array(vs)) if ps.len() == vs.len() => {
            ps.iter().zip(vs.iter()).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::List(ps), Value::List(vs)) if ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::ListNil, Value::List(vs)) => vs.is_empty(),
        (P::ListCons(ph, pt), Value::List(vs)) if !vs.is_empty() => {
            match_pat(ph, &vs[0], scope) && match_pat(pt, &Value::List(vs[1..].to_vec()), scope)
        }
        (P::Ctor(name, ps), Value::Ctor(vname, vs)) if name == vname && ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::Record(fields), Value::Record(map)) => fields
            .iter()
            .all(|(label, p)| map.get(label).is_some_and(|v| match_pat(p, v, scope))),
        _ => false,
    }
}

// --- primitives ----------------------------------------------------------

fn run_prim(op: core::Prim, args: Vec<Value>) -> Result<Value, RuntimeError> {
    use core::Prim::*;

    let int2 = |a: &Value, b: &Value| -> Result<(i64, i64), RuntimeError> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok((*x, *y)),
            _ => err(format!("expected two Ints, got {a} and {b}")),
        }
    };
    let big2 = |a: &Value, b: &Value| -> Result<(BigInt, BigInt), RuntimeError> {
        match (a, b) {
            (Value::BigInt(x), Value::BigInt(y)) => Ok((x.clone(), y.clone())),
            _ => err(format!("expected two BigInts, got {a} and {b}")),
        }
    };
    let flt2 = |a: &Value, b: &Value| -> Result<(f64, f64), RuntimeError> {
        match (a, b) {
            (Value::Float(x), Value::Float(y)) => Ok((*x, *y)),
            _ => err(format!("expected two Floats, got {a} and {b}")),
        }
    };

    match op {
        Add | Sub | Mul | Div | Mod | Pow => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Add => x.wrapping_add(y),
                Sub => x.wrapping_sub(y),
                Mul => x.wrapping_mul(y),
                Div => {
                    if y == 0 {
                        return err("division by zero");
                    }
                    x.wrapping_div(y)
                }
                Mod => {
                    if y == 0 {
                        return err("modulo by zero");
                    }
                    x.wrapping_rem(y)
                }
                Pow => {
                    let e = u32::try_from(y).map_err(|_| RuntimeError {
                        msg: format!("`^` exponent must fit in u32, got {y}"),
                    })?;
                    x.wrapping_pow(e)
                }
                _ => unreachable!(),
            };
            Ok(Value::Int(r))
        }
        Lt | Gt | Le | Ge => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                _ => x >= y,
            };
            Ok(Value::Bool(r))
        }
        AddB | SubB | MulB | DivB | ModB | PowB => {
            let (x, y) = big2(&args[0], &args[1])?;
            let r = match op {
                AddB => x + y,
                SubB => x - y,
                MulB => x * y,
                DivB => {
                    if y.is_zero() {
                        return err("division by zero");
                    }
                    x / y
                }
                ModB => {
                    if y.is_zero() {
                        return err("modulo by zero");
                    }
                    x % y
                }
                PowB => {
                    let e = y.to_u32().ok_or_else(|| RuntimeError {
                        msg: format!("`^~` exponent must fit in u32, got {y}"),
                    })?;
                    x.pow(e)
                }
                _ => unreachable!(),
            };
            Ok(Value::BigInt(r))
        }
        LtB | GtB | LeB | GeB => {
            let (x, y) = big2(&args[0], &args[1])?;
            let r = match op {
                LtB => x < y,
                GtB => x > y,
                LeB => x <= y,
                _ => x >= y,
            };
            Ok(Value::Bool(r))
        }
        AddF | SubF | MulF | DivF => {
            let (x, y) = flt2(&args[0], &args[1])?;
            let r = match op {
                AddF => x + y,
                SubF => x - y,
                MulF => x * y,
                _ => x / y,
            };
            Ok(Value::Float(r))
        }
        LtF | GtF | LeF | GeF => {
            let (x, y) = flt2(&args[0], &args[1])?;
            let r = match op {
                LtF => x < y,
                GtF => x > y,
                LeF => x <= y,
                _ => x >= y,
            };
            Ok(Value::Bool(r))
        }
        ToFloat => match &args[0] {
            Value::Int(x) => Ok(Value::Float(*x as f64)),
            other => err(format!("`toFloat` expects an Int, got {other}")),
        },
        Floor => match &args[0] {
            Value::Float(x) => {
                let f = x.floor();
                if !f.is_finite() {
                    return err(format!("`floor` of a non-finite Float: {x}"));
                }
                Ok(Value::Int(f as i64))
            }
            other => err(format!("`floor` expects a Float, got {other}")),
        },
        ToBig => match &args[0] {
            Value::Int(x) => Ok(Value::BigInt(BigInt::from(*x))),
            other => err(format!("`toBigInt` expects an Int, got {other}")),
        },
        ToInt => match &args[0] {
            Value::BigInt(x) => x.to_i64().map(Value::Int).ok_or_else(|| RuntimeError {
                msg: format!("`toInt`: {x} does not fit in Int"),
            }),
            other => err(format!("`toInt` expects a BigInt, got {other}")),
        },
        Eq => Ok(Value::Bool(value_eq(&args[0], &args[1]))),
        Ne => Ok(Value::Bool(!value_eq(&args[0], &args[1]))),
        Neg => match &args[0] {
            Value::Int(x) => Ok(Value::Int(x.wrapping_neg())),
            other => err(format!("`neg` expects an Int, got {other}")),
        },
        Print => {
            print!("{}", args[0]);
            Ok(Value::Unit)
        }
        Println => {
            println!("{}", args[0]);
            Ok(Value::Unit)
        }

        // --- builtin `Array` -------------------------------------------------
        ArrayLen => Ok(Value::Int(as_array(&args[0])?.len() as i64)),
        ArrayGet => {
            let a = as_array(&args[0])?;
            let i = as_index(&args[1])?;
            a.get(i)
                .cloned()
                .ok_or_else(|| RuntimeError {
                    msg: format!("arrayGet: index {i} out of bounds (len {})", a.len()),
                })
        }
        ArrayGetOr => {
            let a = as_array(&args[1])?;
            let i = as_index(&args[2])?;
            Ok(a.get(i).cloned().unwrap_or_else(|| args[0].clone()))
        }
        ArraySet => {
            let mut rc = as_array(&args[0])?.clone();
            let i = as_index(&args[1])?;
            let buf = Rc::make_mut(&mut rc);
            if i >= buf.len() {
                return err(format!(
                    "arraySet: index {i} out of bounds (len {})",
                    buf.len()
                ));
            }
            buf[i] = args[2].clone();
            Ok(Value::Array(rc))
        }
        ArrayPush => {
            let mut rc = as_array(&args[0])?.clone();
            Rc::make_mut(&mut rc).push(args[1].clone());
            Ok(Value::Array(rc))
        }
        ArrayPop => {
            let mut rc = as_array(&args[0])?.clone();
            let buf = Rc::make_mut(&mut rc);
            if buf.pop().is_none() {
                return err("arrayPop: empty array");
            }
            Ok(Value::Array(rc))
        }
        ArraySlice => {
            let a = as_array(&args[0])?;
            let n = a.len() as i64;
            let from = as_int(&args[1])?.clamp(0, n) as usize;
            let to = as_int(&args[2])?.clamp(from as i64, n) as usize;
            Ok(Value::Array(Rc::new(a[from..to].to_vec())))
        }
        ArrayConcat => {
            let x = as_array(&args[0])?;
            let y = as_array(&args[1])?;
            if x.is_empty() {
                return Ok(args[1].clone());
            }
            if y.is_empty() {
                return Ok(args[0].clone());
            }
            let mut out = Vec::with_capacity(x.len() + y.len());
            out.extend(x.iter().cloned());
            out.extend(y.iter().cloned());
            Ok(Value::Array(Rc::new(out)))
        }

        // --- bitwise `Int` ops --------------------------------------------------
        Shl | Shr | Ushr | BitAnd | BitOr | BitXor => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Shl => x.wrapping_shl(y as u32),
                // arithmetic (sign-extending) right shift
                Shr => x.wrapping_shr(y as u32),
                // logical right shift (treat `x` as 64 unsigned bits)
                Ushr => (x as u64).wrapping_shr(y as u32) as i64,
                BitAnd => x & y,
                BitOr => x | y,
                BitXor => x ^ y,
                _ => unreachable!(),
            };
            Ok(Value::Int(r))
        }
        BitNot => Ok(Value::Int(!as_int(&args[0])?)),
        PopCount => Ok(Value::Int(as_int(&args[0])?.count_ones() as i64)),

        // --- bytes -----------------------------------------------------------
        StringToBytes => match &args[0] {
            Value::Str(s) => Ok(Value::Array(Rc::new(
                s.bytes().map(|b| Value::Int(b as i64)).collect(),
            ))),
            other => err(format!("`stringToBytes` expects a String, got {other}")),
        },
        BytesToString => {
            let buf = bytes_of(&args[0], "bytesToString")?;
            Ok(Value::Str(InternedString::from(
                String::from_utf8_lossy(&buf).into_owned(),
            )))
        }
        BytesToHex => {
            let buf = bytes_of(&args[0], "bytesToHex")?;
            let mut s = String::with_capacity(buf.len() * 2);
            for b in buf {
                s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
                s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
            }
            Ok(Value::Str(InternedString::from(s)))
        }
        BytesFromHex => {
            let s = match &args[0] {
                Value::Str(s) => *s,
                other => return err(format!("`bytesFromHex` expects a String, got {other}")),
            };
            let none = || Value::Ctor(InternedString::from("None"), vec![]);
            let bytes = s.as_bytes();
            if bytes.len() % 2 != 0 {
                return Ok(none());
            }
            let mut out = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.chunks_exact(2) {
                let hi = (pair[0] as char).to_digit(16);
                let lo = (pair[1] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => out.push(Value::Int(((h << 4) | l) as i64)),
                    _ => return Ok(none()),
                }
            }
            Ok(Value::Ctor(
                InternedString::from("Just"),
                vec![Value::Array(Rc::new(out))],
            ))
        }
    }
}

/// Read a `Value::Array` of `Int`s in 0..=255 into a byte buffer. `what` names the
/// caller for the error message.
fn bytes_of(v: &Value, what: &str) -> Result<Vec<u8>, RuntimeError> {
    let a = as_array(v)?;
    let mut buf = Vec::with_capacity(a.len());
    for x in a.iter() {
        match x {
            Value::Int(n) if (0..=255).contains(n) => buf.push(*n as u8),
            other => return err(format!("`{what}`: not a byte (0..255): {other}")),
        }
    }
    Ok(buf)
}

fn as_array<'a>(v: &'a Value) -> Result<&'a Rc<Vec<Value>>, RuntimeError> {
    match v {
        Value::Array(a) => Ok(a),
        other => err(format!("expected an Array, got {other}")),
    }
}

fn as_int(v: &Value) -> Result<i64, RuntimeError> {
    match v {
        Value::Int(i) => Ok(*i),
        other => err(format!("expected an Int, got {other}")),
    }
}

fn as_index(v: &Value) -> Result<usize, RuntimeError> {
    let i = as_int(v)?;
    if i < 0 {
        return err(format!("negative array index {i}"));
    }
    Ok(i as usize)
}

/// The runtime's default handler for the `Std.Fs` effect: perform the real
/// filesystem operation and return its result. Read/write ops yield
/// `Result String a` (`Err` carries the OS message); `exists` / `isFile` /
/// `isDir` yield `Bool`.
fn native_fs(op: &str, arg: Value) -> Result<Value, RuntimeError> {
    use std::fs;
    use std::path::Path;

    let sv = |s: String| Value::Str(InternedString::from(s));
    let ok = |v: Value| Value::Ctor(InternedString::from("Ok"), vec![v]);
    let ioerr = |e: std::io::Error| {
        Value::Ctor(
            InternedString::from("Err"),
            vec![Value::Str(InternedString::from(e.to_string()))],
        )
    };
    let unit = |r: std::io::Result<()>| match r {
        Ok(()) => ok(Value::Unit),
        Err(e) => ioerr(e),
    };
    let one = |v: &Value| -> Result<InternedString, RuntimeError> {
        match v {
            Value::Str(s) => Ok(*s),
            other => err(format!("Fs.{op}: expected a String, got {other}")),
        }
    };
    let two = |v: &Value| -> Result<(InternedString, InternedString), RuntimeError> {
        match v {
            Value::Tuple(xs) if xs.len() == 2 => Ok((one(&xs[0])?, one(&xs[1])?)),
            other => err(format!(
                "Fs.{op}: expected a (String, String) pair, got {other}"
            )),
        }
    };

    Ok(match op {
        "readToString" => match fs::read_to_string(&*one(&arg)?) {
            Ok(c) => ok(sv(c)),
            Err(e) => ioerr(e),
        },
        "readBytes" => match fs::read(&*one(&arg)?) {
            Ok(b) => ok(Value::List(
                b.into_iter().map(|x| Value::Int(i64::from(x))).collect(),
            )),
            Err(e) => ioerr(e),
        },
        "writeString" => {
            let (p, c) = two(&arg)?;
            unit(fs::write(&*p, c.as_bytes()))
        }
        "appendString" => {
            use std::io::Write;
            let (p, c) = two(&arg)?;
            let r = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&*p)
                .and_then(|mut f| f.write_all(c.as_bytes()));
            unit(r)
        }
        "removeFile" => unit(fs::remove_file(&*one(&arg)?)),
        "createDir" => unit(fs::create_dir(&*one(&arg)?)),
        "createDirAll" => unit(fs::create_dir_all(&*one(&arg)?)),
        "removeDir" => unit(fs::remove_dir(&*one(&arg)?)),
        "removeDirAll" => unit(fs::remove_dir_all(&*one(&arg)?)),
        "rename" => {
            let (a, b) = two(&arg)?;
            unit(fs::rename(&*a, &*b))
        }
        "copy" => {
            let (a, b) = two(&arg)?;
            match fs::copy(&*a, &*b) {
                Ok(n) => ok(Value::Int(n as i64)),
                Err(e) => ioerr(e),
            }
        }
        "readDir" => match fs::read_dir(&*one(&arg)?) {
            Ok(entries) => {
                let mut names = Vec::new();
                for e in entries {
                    match e {
                        Ok(en) => names.push(sv(en.file_name().to_string_lossy().into_owned())),
                        Err(e) => return Ok(ioerr(e)),
                    }
                }
                ok(Value::List(names))
            }
            Err(e) => ioerr(e),
        },
        "metadata" => match fs::metadata(&*one(&arg)?) {
            Ok(md) => {
                let mut rec = BTreeMap::new();
                rec.insert(InternedString::from("isFile"), Value::Bool(md.is_file()));
                rec.insert(InternedString::from("isDir"), Value::Bool(md.is_dir()));
                rec.insert(InternedString::from("len"), Value::Int(md.len() as i64));
                rec.insert(
                    InternedString::from("readonly"),
                    Value::Bool(md.permissions().readonly()),
                );
                ok(Value::Record(rec))
            }
            Err(e) => ioerr(e),
        },
        "exists" => Value::Bool(Path::new(&*one(&arg)?).exists()),
        "isFile" => Value::Bool(Path::new(&*one(&arg)?).is_file()),
        "isDir" => Value::Bool(Path::new(&*one(&arg)?).is_dir()),
        other => return err(format!("unhandled effect Fs.{other}")),
    })
}

/// The runtime's default handler for the `Std.Process` effect: spawn real child
/// processes / touch the real environment. A `Command` (from `Std.Process`) is
/// the tuple `(program, args, cwd, envVars) : (String, List String,
/// Maybe String, List (String, String))`; an `Output` is `(status, stdout,
/// stderr) : (Int, String, String)`.
fn native_process(op: &str, arg: Value) -> Result<Value, RuntimeError> {
    use std::process::Command as Proc;

    let sv = |s: String| Value::Str(InternedString::from(s));
    let ok = |v: Value| Value::Ctor(InternedString::from("Ok"), vec![v]);
    let errv = |m: String| Value::Ctor(InternedString::from("Err"), vec![sv(m)]);
    let just = |v: Value| Value::Ctor(InternedString::from("Just"), vec![v]);
    let none = || Value::Ctor(InternedString::from("None"), vec![]);

    let as_str = |v: &Value| -> Result<InternedString, RuntimeError> {
        match v {
            Value::Str(s) => Ok(*s),
            other => err(format!("Process.{op}: expected a String, got {other}")),
        }
    };
    fn as_list<'a>(op: &str, v: &'a Value) -> Result<&'a Vec<Value>, RuntimeError> {
        match v {
            Value::List(xs) => Ok(xs),
            other => err(format!("Process.{op}: expected a List, got {other}")),
        }
    }
    let as_cwd = |v: &Value| -> Result<Option<InternedString>, RuntimeError> {
        match v {
            Value::Ctor(n, args) if &**n == "None" && args.is_empty() => Ok(None),
            Value::Ctor(n, args) if &**n == "Just" && args.len() == 1 => Ok(Some(as_str(&args[0])?)),
            other => err(format!("Process.{op}: expected a Maybe String, got {other}")),
        }
    };
    let build = |v: &Value| -> Result<Proc, RuntimeError> {
        let t = match v {
            Value::Tuple(t) if t.len() == 4 => t,
            other => return err(format!("Process.{op}: expected a Command, got {other}")),
        };
        let program = as_str(&t[0])?;
        let mut cmd = Proc::new(&*program);
        for a in as_list(op, &t[1])? {
            cmd.arg(&*as_str(a)?);
        }
        if let Some(dir) = as_cwd(&t[2])? {
            cmd.current_dir(&*dir);
        }
        for e in as_list(op, &t[3])? {
            match e {
                Value::Tuple(kv) if kv.len() == 2 => {
                    cmd.env(&*as_str(&kv[0])?, &*as_str(&kv[1])?);
                }
                other => return err(format!("Process.{op}: expected a (String, String) pair, got {other}")),
            }
        }
        Ok(cmd)
    };

    Ok(match op {
        "spawn" => match build(&arg)?.output() {
            Ok(out) => ok(Value::Tuple(vec![
                Value::Int(out.status.code().unwrap_or(-1) as i64),
                sv(String::from_utf8_lossy(&out.stdout).into_owned()),
                sv(String::from_utf8_lossy(&out.stderr).into_owned()),
            ])),
            Err(e) => errv(e.to_string()),
        },
        "status" => match build(&arg)?.status() {
            Ok(st) => ok(Value::Int(st.code().unwrap_or(-1) as i64)),
            Err(e) => errv(e.to_string()),
        },
        "exit" => {
            let code = as_int(&arg)?;
            std::process::exit(code as i32);
        }
        "currentPid" => Value::Int(std::process::id() as i64),
        "argv" => Value::List(std::env::args().skip(1).map(sv).collect()),
        "getEnv" => match std::env::var(&*as_str(&arg)?) {
            Ok(v) => just(sv(v)),
            Err(_) => none(),
        },
        "setEnv" => {
            let t = match &arg {
                Value::Tuple(t) if t.len() == 2 => t,
                other => return err(format!("Process.setEnv: expected a (String, String) pair, got {other}")),
            };
            unsafe { std::env::set_var(&*as_str(&t[0])?, &*as_str(&t[1])?); }
            Value::Unit
        }
        "removeEnv" => {
            unsafe { std::env::remove_var(&*as_str(&arg)?); }
            Value::Unit
        }
        other => return err(format!("unhandled effect Process.{other}")),
    })
}

/// Names of the outer `Std.Collections.Vector` constructors — the values `[…]`
/// literal syntax produces. Their internal shape varies with how the vector was
/// built, so [`Display`] and [`value_eq`] flatten them to their element sequence.
fn is_vector_ctor(name: &str) -> bool {
    matches!(name, "VEmpty" | "VSingle" | "VFull")
}

/// Flatten a `Vector` value to its elements, in order. `None` if `v` is not a
/// recognizable vector (wrong ctor / arity / field types).
fn vector_elems(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Ctor(n, args) if &**n == "VEmpty" && args.is_empty() => Some(Vec::new()),
        Value::Ctor(n, args) if &**n == "VSingle" && args.len() == 1 => match &args[0] {
            Value::Array(xs) => Some(xs.iter().cloned().collect()),
            _ => None,
        },
        Value::Ctor(n, args) if &**n == "VFull" && args.len() == 7 => {
            let mut out = Vec::new();
            for i in [2usize, 3] {
                match &args[i] {
                    Value::Array(xs) => out.extend(xs.iter().cloned()),
                    _ => return None,
                }
            }
            vector_node_elems(&args[4], &mut out)?;
            for i in [5usize, 6] {
                match &args[i] {
                    Value::Array(xs) => out.extend(xs.iter().cloned()),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn vector_node_elems(n: &Value, out: &mut Vec<Value>) -> Option<()> {
    match n {
        Value::Ctor(name, args) if &**name == "VLeaf" && args.len() == 1 => match &args[0] {
            Value::Array(xs) => {
                out.extend(xs.iter().cloned());
                Some(())
            }
            _ => None,
        },
        Value::Ctor(name, args) if &**name == "VBranch" && args.len() == 2 => match &args[1] {
            Value::Array(kids) => {
                for k in kids.iter() {
                    vector_node_elems(k, out)?;
                }
                Some(())
            }
            _ => None,
        },
        _ => None,
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Ctor(na, _), Value::Ctor(nb, _)) if is_vector_ctor(na) && is_vector_ctor(nb) => {
            match (vector_elems(a), vector_elems(b)) {
                (Some(xs), Some(ys)) => {
                    xs.len() == ys.len() && xs.iter().zip(&ys).all(|(p, q)| value_eq(p, q))
                }
                _ => false,
            }
        }
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::Tuple(x), Value::Tuple(y)) | (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| value_eq(p, q))
        }
        (Value::Array(x), Value::Array(y)) => {
            Rc::ptr_eq(x, y)
                || (x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| value_eq(p, q)))
        }
        (Value::Ctor(n1, x), Value::Ctor(n2, y)) => {
            n1 == n2 && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| value_eq(p, q))
        }
        (Value::Record(x), Value::Record(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| value_eq(v, w)))
        }
        _ => false,
    }
}

// --- display -------------------------------------------------------------

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::BigInt(i) => write!(f, "{i}"),
            Value::Float(x) => f.write_str(&fmt_float(*x)),
            Value::Bool(b) => write!(f, "{b}"),
            // quote + escape, so a string is visually distinct from a bare ident
            Value::Str(s) => write!(f, "{:?}", &**s),
            Value::Unit => f.write_str("()"),
            Value::Tuple(items) => {
                f.write_str("(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Array(items) => {
                f.write_str("#[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::List(items) => {
                f.write_str("[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::Record(map) => {
                f.write_str("{ ")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k} = {v}")?;
                }
                f.write_str(" }")
            }
            // `Std.Collections.Vector` values print like a list: `[1, 2, 3]`.
            Value::Ctor(name, _) if is_vector_ctor(name) => {
                if let Some(xs) = vector_elems(self) {
                    f.write_str("[")?;
                    for (i, v) in xs.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{v}")?;
                    }
                    return f.write_str("]");
                }
                write!(f, "{name}(..)")
            }
            Value::Ctor(name, args) if args.is_empty() => write!(f, "{name}"),
            Value::Ctor(name, args) => {
                write!(f, "{name}(")?;
                for (i, v) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Closure { .. } => f.write_str("<closure>"),
            Value::Builtin { op, .. } => write!(f, "<builtin {op:?}>"),
            Value::Cont(_) => f.write_str("<continuation>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meadow_core::{HClause, Lit, Prim, Program, Term};
    use std::rc::Rc;

    fn v() -> Var {
        meadow_core::Var::fresh()
    }

    /// Evaluate a single term (as `def main = term`, entry `main`).
    fn eval_term(term: Term) -> Result<Value, RuntimeError> {
        let m = v();
        run(&Program {
            defs: vec![core::Def {
                var: m,
                name: "main".into(),
                term,
            }],
            entry: Some(m),
            ..Default::default()
        })
    }

    fn int(i: i64) -> Rc<Term> {
        Rc::new(Term::Lit(Lit::Int(i)))
    }

    #[test]
    fn arithmetic_and_application() {
        // (\x -> x + 1) 41
        let x = v();
        let body = Term::Prim(Prim::Add, vec![Term::Var(x), Term::Lit(Lit::Int(1))]);
        let term = Term::App(Rc::new(Term::Lam(x, Rc::new(body))), int(41));
        assert_eq!(eval_term(term).unwrap().to_string(), "42");
    }

    #[test]
    fn if_and_let() {
        let x = v();
        let term = Term::Let(
            x,
            int(10),
            Rc::new(Term::If(
                Rc::new(Term::Prim(
                    Prim::Lt,
                    vec![Term::Var(x), Term::Lit(Lit::Int(20))],
                )),
                int(1),
                int(2),
            )),
        );
        assert_eq!(eval_term(term).unwrap().to_string(), "1");
    }

    #[test]
    fn list_cons_prepends() {
        let term = Term::ListCons(
            int(0),
            Rc::new(Term::List(vec![
                Term::Lit(Lit::Int(1)),
                Term::Lit(Lit::Int(2)),
            ])),
        );
        assert_eq!(eval_term(term).unwrap().to_string(), "[0, 1, 2]");
    }

    #[test]
    fn recursion_via_letrec() {
        // letrec f = \n -> if n == 0 then 0 else n + f (n - 1) in f 5   => 15
        let f = v();
        let n = v();
        let lam = Term::Lam(
            n,
            Rc::new(Term::If(
                Rc::new(Term::Prim(
                    Prim::Eq,
                    vec![Term::Var(n), Term::Lit(Lit::Int(0))],
                )),
                int(0),
                Rc::new(Term::Prim(
                    Prim::Add,
                    vec![
                        Term::Var(n),
                        Term::App(
                            Rc::new(Term::Var(f)),
                            Rc::new(Term::Prim(
                                Prim::Sub,
                                vec![Term::Var(n), Term::Lit(Lit::Int(1))],
                            )),
                        ),
                    ],
                )),
            )),
        );
        let term = Term::LetRec(
            vec![(f, lam)],
            Rc::new(Term::App(Rc::new(Term::Var(f)), int(5))),
        );
        assert_eq!(eval_term(term).unwrap().to_string(), "15");
    }

    #[test]
    fn handler_state_like() {
        // handle (perform E.get () ; perform E.get ())  with
        //   get _ k -> k 7
        //   return x -> x
        // ==> 7  (each `get` resumes with 7; the body's value is the 2nd get)
        let k = v();
        let p = v();
        let x = v();
        let get =
            |_arg: Rc<Term>| Term::Perform("E".into(), "get".into(), Rc::new(Term::Lit(Lit::Unit)));
        let discard = v();
        let body = Term::Let(discard, Rc::new(get(int(0))), Rc::new(get(int(0))));
        let term = Term::Handle {
            body: Rc::new(body),
            clauses: vec![HClause {
                effect: "E".into(),
                op: "get".into(),
                param: p,
                resume: k,
                body: Term::App(Rc::new(Term::Var(k)), int(7)),
            }],
            ret: Some((x, Rc::new(Term::Var(x)))),
        };
        assert_eq!(eval_term(term).unwrap().to_string(), "7");
    }

    #[test]
    fn unhandled_effect_errors() {
        let term = Term::Perform("E".into(), "boom".into(), Rc::new(Term::Lit(Lit::Unit)));
        let e = eval_term(term).unwrap_err();
        assert!(e.msg.contains("unhandled effect E.boom"), "{}", e.msg);
    }

    #[test]
    fn one_shot_resume_twice_errors() {
        // clause resumes k, then tries to resume k again
        let k = v();
        let p = v();
        let term = Term::Handle {
            body: Rc::new(Term::Perform(
                "E".into(),
                "op".into(),
                Rc::new(Term::Lit(Lit::Unit)),
            )),
            clauses: vec![HClause {
                effect: "E".into(),
                op: "op".into(),
                param: p,
                resume: k,
                // k 1 ; k 2
                body: Term::Let(
                    v(),
                    Rc::new(Term::App(Rc::new(Term::Var(k)), int(1))),
                    Rc::new(Term::App(Rc::new(Term::Var(k)), int(2))),
                ),
            }],
            ret: None,
        };
        let e = eval_term(term).unwrap_err();
        assert!(e.msg.contains("resumed more than once"), "{}", e.msg);
    }

    #[test]
    fn value_display_forms() {
        assert_eq!(Value::Unit.to_string(), "()");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(
            Value::Ctor("Just".into(), vec![Value::Int(3)]).to_string(),
            "Just(3)"
        );
    }

    #[test]
    fn prim_arithmetic_direct() {
        assert_eq!(
            run_prim(Prim::Mul, vec![Value::Int(4), Value::Int(5)])
                .unwrap()
                .to_string(),
            "20"
        );
        assert!(run_prim(Prim::Div, vec![Value::Int(1), Value::Int(0)]).is_err());
    }

    #[test]
    fn structural_equality() {
        let a = Value::Tuple(vec![Value::Int(1), Value::List(vec![Value::Int(2)])]);
        let b = Value::Tuple(vec![Value::Int(1), Value::List(vec![Value::Int(2)])]);
        assert!(value_eq(&a, &b));
        assert!(!value_eq(&a, &Value::Int(1)));
    }
}
