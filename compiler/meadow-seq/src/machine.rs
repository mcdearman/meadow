//! The AxCut abstract machine — **the IR's reference semantics**.
//!
//! A transcription of `Semantics.idr` from the OOPSLA 2025 artifact: a
//! statement, a program, and an environment, and seven transitions between them
//! — plus three more for effects, which AxCut does not have.
//!
//! It lives beside the IR rather than in the runtime, because it is not a
//! runtime. `meadow_rts` executes bytecode and knows nothing about sequents; the
//! whole point of `meadow_codegen` is that everything in this file is compiled
//! away. What this is for:
//!
//! * **Pinning down what the IR means** before a code generator gets an opinion
//!   about it. When the VM and this disagree, one of them is wrong and there is
//!   a small enough thing to read to find out which.
//! * **Checking the lowering on its own.** [`crate::lower`] has to get the shape
//!   of the environment exactly right at every program point — see its module
//!   docs — and running the result against the CEK machine tests precisely
//!   that, with no register allocation in the way.
//!
//! # The environment is the whole story
//!
//! There is no stack of frames and no notion of "the current registers" beyond a
//! single ordered list of values. Every transition rebuilds it:
//!
//! ```text
//!   substitute [x, y]   env := [lookup x, lookup y]      (and nothing else)
//!   jump L              env unchanged, renamed by L's parameters
//!   let x = K(a, b)     env := K(a, b) :: env
//!   switch x            env := fields(x) ++ env
//!   new f {…}           env := f :: env
//!   invoke f#i          env := captures(f) ++ (env without f)
//!   extern p(…)         env := result :: env    (a branch leaves env alone)
//! ```
//!
//! A block's parameters name *the entire environment* on entry, not just what is
//! new — so every transition here checks that the length matches, and a mismatch
//! is a bug in the lowering rather than in the program. That check is the point:
//! it is what turns "the register allocation is implicit in the IR" from a claim
//! into something enforced.
//!
//! # Effects
//!
//! The one piece of state that is not the environment. `handle` pushes a frame
//! naming a handler object, the operations it covers, and where the whole
//! `handle` expression's value goes; `perform` unwinds to the innermost frame
//! that covers the operation, and hands the clause three things: the argument, a
//! one-shot resumption, and that frame's continuation.
//!
//! The resumption carries the performing continuation *and the frames unwound
//! past, including the handler's own*. Putting the handler's frame back is what
//! makes handlers deep. One detail is easy to get wrong and worth naming: the
//! restored handler's continuation is **not** the one it was installed with. It
//! becomes the continuation of the `resume` call, so that when the body finally
//! returns, its value flows into the middle of the clause that resumed it —
//! which is why `handle (perform op + 1) with { op _ r -> r 5 + 100 }` is 106 and
//! not 6.
//!
//! # Divergences from the CEK machine
//!
//! Three, all recorded here rather than hidden:
//!
//! * **`let` and `new` do not consume.** The paper's environment is linear and
//!   both build from a prefix which they then drop. Here they prepend and leave
//!   the rest alone. `substitute` is still the only shrinking operation, and the
//!   lowering emits one at every call and return, so environments stay bounded.
//! * **A definition is re-evaluated at every reference.** The lowering turns a
//!   global — and a `letrec` binding — into a `jump`, which is what makes
//!   recursion need no back-patching; the CEK instead evaluates each definition
//!   once at load. For a right-hand side that is a lambda or a literal the two
//!   agree. For one that performs an effect they do not, and the fix is a
//!   memoising thunk per definition.
//! * **No native effects.** An unhandled `Fs`, `Process`, `Random` or `Time`
//!   operation is an error here; the CEK discharges it against the real world.
//!   `Test.fail` is the exception, because a test runner needs it.

use meadow_core::{Lit, Prim};
use meadow_intern::InternedString;
use crate::{Block, Extern, Name, Program, Statement, Tag};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// A value in the machine.
///
/// The paper's `Number` / `Product` / `Conduct`, with the scalars widened to
/// Meadow's literal types and three extras: the builtin `Array` and record,
/// which are not data in the constructor sense, and the resumption a `perform`
/// hands its handler.
#[derive(Debug, Clone)]
pub enum Value<'p> {
    Int(i64),
    BigInt(Rc<BigInt>),
    Float(f64),
    Bool(bool),
    Str(InternedString),
    Char(char),
    Unit,
    /// Data: a constructor and its fields. Tuples are the constructor `#tuple`.
    Data(InternedString, Tag, Rc<Fields<'p>>),
    /// The builtin `Array` — the only primitive collection.
    Array(Rc<Vec<Value<'p>>>),
    Record(Rc<BTreeMap<InternedString, Value<'p>>>),
    /// A mutable cell — the only value with identity.
    Ref(Rc<RefCell<Value<'p>>>),
    /// Codata: captured values and a method table, which is a slice of the
    /// program rather than a copy of it.
    Obj(Rc<Object<'p>>),
    /// A one-shot resumption: the continuation a `perform` was cut from, and the
    /// handler frames to put back.
    Resume(Rc<RefCell<Option<Captured<'p>>>>),
    /// The continuation the entry point is handed. Invoking it stops the machine.
    Halt,
}

/// A data value's fields.
///
/// A newtype only so [`Drop`] can be iterative. A `Cons` chain is as deep as it
/// is long, and the derived drop glue recurses once per element — which is what
/// aborted the CEK machine at a few thousand elements before it was fixed. The
/// same shape appears here for the same reason.
#[derive(Debug, Clone)]
pub struct Fields<'p>(Vec<Value<'p>>);

impl<'p> std::ops::Deref for Fields<'p> {
    type Target = Vec<Value<'p>>;
    fn deref(&self) -> &Vec<Value<'p>> {
        &self.0
    }
}

impl Drop for Fields<'_> {
    fn drop(&mut self) {
        // Dismantle with a worklist: move each child out, and if this was the
        // last reference to another node, move its children onto the same list
        // rather than letting the drop nest. Arrays and records join in because
        // a `Vector` is a tree of arrays of data, so a deep value need not be
        // deep in constructors alone.
        let mut stack: Vec<Value> = std::mem::take(&mut self.0);
        while let Some(v) = stack.pop() {
            match v {
                Value::Data(_, _, rc) => {
                    if let Ok(mut fields) = Rc::try_unwrap(rc) {
                        stack.append(&mut fields.0);
                    }
                }
                Value::Array(rc) => {
                    if let Ok(mut xs) = Rc::try_unwrap(rc) {
                        stack.append(&mut xs);
                    }
                }
                Value::Record(rc) => {
                    if let Ok(map) = Rc::try_unwrap(rc) {
                        stack.extend(map.into_values());
                    }
                }
                _ => {}
            }
        }
    }
}

/// Codata: a closure, a continuation or a handler. The paper makes no
/// distinction between them and neither does this.
#[derive(Debug)]
pub struct Object<'p> {
    pub captures: Vec<Value<'p>>,
    pub methods: &'p [Block],
}

/// An installed handler.
#[derive(Debug, Clone)]
pub struct Frame<'p> {
    ops: &'p [(InternedString, InternedString)],
    handler: Rc<Object<'p>>,
    /// Where the `handle` expression's value goes. Rebound when a resumption
    /// puts this frame back — see the module docs.
    ret_k: Value<'p>,
}

/// What a resumption holds: where the `perform` was, and what to reinstall.
#[derive(Debug)]
pub struct Captured<'p> {
    k: Value<'p>,
    frames: Vec<Frame<'p>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { msg: msg.into() })
}

/// The machine: where we are, what is in scope, and which handlers are up.
pub struct Machine<'p> {
    program: &'p Program,
    stmt: &'p Statement,
    /// Names for the environment's slots — the parameters of the block we are
    /// inside. The paper's environment is purely positional; keeping the names
    /// is what lets a statement say `invoke f#0` instead of counting.
    names: Vec<Name>,
    env: Vec<Value<'p>>,
    handlers: Vec<Frame<'p>>,
    /// Transitions taken, for the fuel limit and for reporting.
    pub steps: u64,
}

impl<'p> Machine<'p> {
    /// Start at the program's entry point.
    ///
    /// Its block takes one parameter, the continuation to answer with, and that
    /// continuation is [`Value::Halt`].
    pub fn new(program: &'p Program) -> Result<Self, Error> {
        let Some(entry) = program.entry else {
            return err("program has no entry point");
        };
        let Some(block) = program.block(entry) else {
            return err(format!("entry label {entry:?} is not defined"));
        };
        if block.params.len() != 1 {
            return err(format!(
                "entry block takes {} parameters, expected 1 (the continuation)",
                block.params.len()
            ));
        }
        Ok(Machine {
            program,
            stmt: &block.body,
            names: block.params.clone(),
            env: vec![Value::Halt],
            handlers: Vec::new(),
            steps: 0,
        })
    }

    /// Run to completion, or until `fuel` transitions have been taken.
    ///
    /// Bounded rather than open-ended because this is what differential tests
    /// call: a lowering bug that loops should fail a test, not hang it.
    pub fn run(program: &'p Program, fuel: u64) -> Result<Value<'p>, Error> {
        let mut m = Machine::new(program)?;
        loop {
            if m.steps >= fuel {
                return err(format!("ran for {fuel} steps without finishing"));
            }
            if let Some(v) = m.step()? {
                return Ok(v);
            }
        }
    }

    /// One transition. `Some` means the machine has finished.
    pub fn step(&mut self) -> Result<Option<Value<'p>>, Error> {
        self.steps += 1;
        match self.stmt {
            Statement::Substitute(sel, block) => {
                if sel.len() != block.params.len() {
                    return err(format!(
                        "substitute of {} values into a block of {} parameters",
                        sel.len(),
                        block.params.len()
                    ));
                }
                let vals = self.lookup_all(sel)?;
                self.enter(block, vals)?;
                Ok(None)
            }

            // The environment is untouched — which is exactly why a global can be
            // reached by one, and why recursion needs no back-patching.
            Statement::Jump(label) => {
                let Some(block) = self.program.block(*label) else {
                    return err(format!("jump to undefined label {label:?}"));
                };
                let vals = std::mem::take(&mut self.env);
                self.enter(block, vals)?;
                Ok(None)
            }

            Statement::Let {
                name,
                tag,
                ctor,
                fields,
                rest,
            } => {
                let vals = self.lookup_all(fields)?;
                let v = Value::Data(*ctor, *tag, Rc::new(Fields(vals)));
                self.push(*name, v);
                self.stmt = rest;
                Ok(None)
            }

            Statement::Switch {
                scrutinee,
                arms,
                default,
            } => {
                let v = self.lookup(*scrutinee)?;
                let hit = match &v {
                    Value::Data(_, tag, fields) => arms
                        .iter()
                        .find(|(t, _)| t == tag)
                        .map(|(_, b)| (b, fields.clone())),
                    _ => None,
                };
                match hit {
                    // The fields go on the front and the scrutinee stays — see
                    // the note on `Statement::Switch`. `invoke` is the only
                    // statement that consumes what it names.
                    Some((block, fields)) => {
                        let mut vals: Vec<Value> = fields.iter().cloned().collect();
                        vals.append(&mut self.env);
                        self.enter(block, vals)?;
                    }
                    // No arm — a different constructor, or not data at all. The
                    // default binds the environment unchanged, because it has no
                    // fields to name.
                    None => {
                        let vals = std::mem::take(&mut self.env);
                        self.enter(default, vals)?;
                    }
                }
                Ok(None)
            }

            Statement::New {
                name,
                captures,
                methods,
                rest,
            } => {
                let captured = self.lookup_all(captures)?;
                let obj = Value::Obj(Rc::new(Object {
                    captures: captured,
                    methods,
                }));
                self.push(*name, obj);
                self.stmt = rest;
                Ok(None)
            }

            Statement::Invoke(target, tag) => self.invoke(*target, *tag),

            Statement::Extern { op, args, blocks } => self.extern_op(op, args, blocks),

            Statement::Handle {
                handler,
                ops,
                k,
                rest,
            } => {
                let h = self.lookup(*handler)?;
                let Value::Obj(obj) = h else {
                    return err(format!("handler is {}, not codata", kind(&h)));
                };
                let ret_k = self.lookup(*k)?;
                self.handlers.push(Frame {
                    ops,
                    handler: obj,
                    ret_k,
                });
                self.stmt = rest;
                Ok(None)
            }

            // Where the body's value goes is on the frame, not in the
            // environment: a resumption rebinds it to the `resume` site.
            Statement::Unhandle { k, rest } => {
                let Some(frame) = self.handlers.pop() else {
                    return err("unhandle with no handler installed");
                };
                self.push(*k, frame.ret_k);
                self.stmt = rest;
                Ok(None)
            }

            Statement::Perform {
                effect,
                op,
                arg,
                k,
            } => {
                self.perform(*effect, *op, *arg, *k)?;
                Ok(None)
            }

            Statement::Error(msg) => err(*msg),
        }
    }

    // --- transitions ------------------------------------------------------

    /// Enter a block with `vals` as the environment.
    ///
    /// The length check is the invariant that makes this machine worth running:
    /// a block's parameters name the whole environment, so if they disagree the
    /// lowering built a program with no meaning, and saying so here is far more
    /// useful than reading a wrong answer later.
    fn enter(&mut self, block: &'p Block, vals: Vec<Value<'p>>) -> Result<(), Error> {
        if block.params.len() != vals.len() {
            return err(format!(
                "block takes {} parameters but the environment has {} values",
                block.params.len(),
                vals.len()
            ));
        }
        self.names = block.params.clone();
        self.env = vals;
        self.stmt = &block.body;
        Ok(())
    }

    fn invoke(&mut self, target: Name, tag: Tag) -> Result<Option<Value<'p>>, Error> {
        let v = self.lookup(target)?;
        match v {
            // Invoking the entry point's continuation is how a program ends: the
            // substitution before it left exactly the answer.
            Value::Halt => {
                let mut rest = self.without(target);
                match rest.len() {
                    1 => Ok(Some(rest.pop().expect("checked"))),
                    n => err(format!("halted with {n} values, expected 1")),
                }
            }
            Value::Obj(obj) => {
                let methods = obj.methods;
                let Some(block) = methods.get(tag as usize) else {
                    return err(format!(
                        "invoked method #{tag} of an object with {} methods",
                        methods.len()
                    ));
                };
                let mut vals = obj.captures.clone();
                vals.extend(self.without(target));
                self.enter(block, vals)?;
                Ok(None)
            }
            // Resuming is an ordinary call from the program's point of view, so
            // it arrives here: `substitute [r, v, k]; invoke r#0`.
            Value::Resume(cell) => {
                if tag != 0 {
                    return err(format!("a resumption has no method #{tag}"));
                }
                let Some(captured) = cell.borrow_mut().take() else {
                    return err("continuation resumed more than once");
                };
                let mut rest = self.without(target);
                if rest.len() != 2 {
                    return err(format!(
                        "resuming takes a value and a continuation, got {} values",
                        rest.len()
                    ));
                }
                let here = rest.pop().expect("checked");
                let arg = rest.pop().expect("checked");

                // Put the handler frames back, with the cut one now answering
                // the resume site rather than where it was installed.
                let mut frames = captured.frames;
                if let Some(f) = frames.first_mut() {
                    f.ret_k = here;
                }
                self.handlers.append(&mut frames);
                self.deliver(captured.k, arg)
            }
            other => err(format!("invoked a non-object value ({})", kind(&other))),
        }
    }

    /// Hand `v` to the continuation `k` — the machine-side spelling of `ret`.
    fn deliver(&mut self, k: Value<'p>, v: Value<'p>) -> Result<Option<Value<'p>>, Error> {
        match k {
            Value::Halt => Ok(Some(v)),
            Value::Obj(obj) => {
                let methods = obj.methods;
                let Some(block) = methods.first() else {
                    return err("a continuation with no method");
                };
                let mut vals = obj.captures.clone();
                vals.push(v);
                self.enter(block, vals)?;
                Ok(None)
            }
            other => err(format!("{} is not a continuation", kind(&other))),
        }
    }

    /// Unwind to the innermost handler covering `effect.op` and enter its clause.
    fn perform(
        &mut self,
        effect: InternedString,
        op: InternedString,
        arg: Name,
        k: Name,
    ) -> Result<(), Error> {
        let matches = |f: &Frame| f.ops.iter().any(|(e, o)| *e == effect && *o == op);
        let Some(idx) = self.handlers.iter().rposition(matches) else {
            // The CEK discharges `Fs`, `Process`, `Random` and `Time` against
            // the real world here. This machine does not, and says so rather
            // than inventing an answer. `Test.fail` is the exception: a failed
            // assertion is a runtime error with the assertion's own message,
            // which is what a test runner reads.
            if &*effect == "Test" && &*op == "fail" {
                let v = self.lookup(arg)?;
                return err(v.to_string());
            }
            return err(format!("unhandled effect {effect}.{op}"));
        };

        let argv = self.lookup(arg)?;
        let kv = self.lookup(k)?;

        let frames = self.handlers.split_off(idx);
        let obj = frames[0].handler.clone();
        let ret_k = frames[0].ret_k.clone();
        let method = frames[0]
            .ops
            .iter()
            .position(|(e, o)| *e == effect && *o == op)
            .expect("matched above");

        let resumption = Value::Resume(Rc::new(RefCell::new(Some(Captured {
            k: kv,
            frames,
        }))));

        let methods = obj.methods;
        let Some(block) = methods.get(method) else {
            return err(format!(
                "handler for {effect}.{op} has no method #{method}"
            ));
        };
        let mut vals = obj.captures.clone();
        vals.push(argv);
        vals.push(resumption);
        vals.push(ret_k);
        self.enter(block, vals)
    }

    fn extern_op(
        &mut self,
        op: &'p Extern,
        args: &'p [Name],
        blocks: &'p [Block],
    ) -> Result<Option<Value<'p>>, Error> {
        // A branching primitive with two continuations. Neither changes the
        // environment, which is why `if` needs no statement of its own.
        if op.is_branch() {
            let [on_false, on_true] = blocks else {
                return err(format!("a branch needs 2 continuations, got {}", blocks.len()));
            };
            // The three forms differ only in where the boolean comes from: a
            // register, a comparison of two, or a comparison against a literal.
            let v = match op {
                Extern::Branch => match args {
                    [arg] => self.lookup(*arg)?,
                    _ => return err(format!("a branch needs 1 argument, got {}", args.len())),
                },
                Extern::BranchPrim(p) => {
                    let vals = self.lookup_all(args)?;
                    prim(*p, &vals, &self.program.tags)?
                }
                Extern::BranchPrimK(p, l) => {
                    let mut vals = self.lookup_all(args)?;
                    vals.push(literal(l));
                    prim(*p, &vals, &self.program.tags)?
                }
                _ => unreachable!("is_branch covers exactly these"),
            };
            let block = if is_falsey(&v) { on_false } else { on_true };
            let vals = std::mem::take(&mut self.env);
            self.enter(block, vals)?;
            return Ok(None);
        }

        let [block] = blocks else {
            return err(format!(
                "a value-producing extern needs 1 continuation, got {}",
                blocks.len()
            ));
        };
        let vals = self.lookup_all(args)?;
        let v = match op {
            Extern::Branch | Extern::BranchPrim(_) | Extern::BranchPrimK(_, _) => {
                unreachable!("handled above")
            }
            Extern::Lit(l) => literal(l),
            Extern::Prim(p) => prim(*p, &vals, &self.program.tags)?,
            Extern::PrimK(p, l) => {
                let mut vals = vals.clone();
                vals.push(literal(l));
                prim(*p, &vals, &self.program.tags)?
            }
            Extern::Array => Value::Array(Rc::new(vals)),
            Extern::Record(labels) => {
                if labels.len() != vals.len() {
                    return err(format!(
                        "a record of {} labels given {} values",
                        labels.len(),
                        vals.len()
                    ));
                }
                Value::Record(Rc::new(
                    labels.iter().copied().zip(vals).collect::<BTreeMap<_, _>>(),
                ))
            }
            Extern::Select(label) => match &vals[0] {
                Value::Record(map) => match map.get(label) {
                    Some(v) => v.clone(),
                    None => return err(format!("no field `{label}` on this record")),
                },
                other => return err(format!("selected `.{label}` from {}", kind(other))),
            },
            Extern::Extend(label) => match &vals[0] {
                Value::Record(map) => {
                    let mut map = (**map).clone();
                    map.insert(*label, vals[1].clone());
                    Value::Record(Rc::new(map))
                }
                other => return err(format!("extended {} with `.{label}`", kind(other))),
            },
            Extern::Field(i) => match &vals[0] {
                Value::Data(name, _, fields) => match fields.get(*i) {
                    Some(v) => v.clone(),
                    None => {
                        return err(format!("`{name}` has no field {i}"));
                    }
                },
                Value::Array(xs) => match xs.get(*i) {
                    Some(v) => v.clone(),
                    None => return err(format!("array of {} has no element {i}", xs.len())),
                },
                other => return err(format!("took field {i} of {}", kind(other))),
            },
        };
        self.produce(block, v)?;
        Ok(None)
    }

    /// An extern that produces a value: it goes on the front, and the
    /// continuation's parameters cover it and everything that was there.
    fn produce(&mut self, block: &'p Block, v: Value<'p>) -> Result<(), Error> {
        let mut vals = vec![v];
        vals.append(&mut self.env);
        self.enter(block, vals)
    }

    // --- environment ------------------------------------------------------

    fn lookup(&self, n: Name) -> Result<Value<'p>, Error> {
        match self.names.iter().position(|m| *m == n) {
            Some(i) => Ok(self.env[i].clone()),
            None => err(format!("{n:?} is not in scope: {}", self.describe())),
        }
    }

    fn lookup_all(&self, ns: &[Name]) -> Result<Vec<Value<'p>>, Error> {
        ns.iter().map(|n| self.lookup(*n)).collect()
    }

    /// The environment with the first slot named `n` removed — the paper's
    /// `drop` of the head, generalised to a name because this machine's
    /// statements say which value they mean.
    fn without(&mut self, n: Name) -> Vec<Value<'p>> {
        let mut env = std::mem::take(&mut self.env);
        if let Some(i) = self.names.iter().position(|m| *m == n) {
            env.remove(i);
        }
        env
    }

    fn push(&mut self, n: Name, v: Value<'p>) {
        self.names.insert(0, n);
        self.env.insert(0, v);
    }

    // --- introspection ----------------------------------------------------

    /// The environment, named — what a stepper would show beside the statement.
    pub fn describe(&self) -> String {
        let mut out = String::from("[");
        for (i, (n, v)) in self.names.iter().zip(self.env.iter()).enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{n:?} = {v}"));
        }
        out.push(']');
        out
    }

    /// How many handlers are installed — the depth a `perform` may have to walk.
    pub fn handler_depth(&self) -> usize {
        self.handlers.len()
    }
}

fn literal<'p>(l: &Lit) -> Value<'p> {
    match l {
        Lit::Int(n) => Value::Int(*n),
        Lit::BigInt(n) => Value::BigInt(Rc::new(BigInt::from(*n))),
        Lit::Float(x) => Value::Float(*x),
        Lit::Str(s) => Value::Str(*s),
        Lit::Char(c) => Value::Char(*c),
        Lit::Bool(b) => Value::Bool(*b),
        Lit::Unit => Value::Unit,
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::BigInt(_) => "BigInt",
        Value::Float(_) => "Float",
        Value::Bool(_) => "Bool",
        Value::Str(_) => "String",
        Value::Char(_) => "Char",
        Value::Unit => "()",
        Value::Data(..) => "data",
        Value::Array(_) => "Array",
        Value::Record(_) => "record",
        Value::Ref(_) => "Ref",
        Value::Obj(_) => "codata",
        Value::Resume(_) => "resumption",
        Value::Halt => "halt",
    }
}

/// What `Extern::Branch` treats as false. The compiler lowers `True`/`False`
/// patterns to `Lit(Bool)`, but a value built by naming the constructor arrives
/// as data, so both spellings are accepted — the same rule the CEK uses.
fn is_falsey(v: &Value) -> bool {
    match v {
        Value::Bool(b) => !b,
        Value::Data(name, _, fields) => &**name == "False" && fields.is_empty(),
        _ => false,
    }
}

// --- the shapes `Std` builds out of data ---------------------------------
//
// A `List` is a `Cons` chain and a `Vector` is a tree of arrays. Both print and
// compare as the sequence they denote rather than as the tree they are, and both
// have to agree with the CEK machine exactly — `show` is a primitive, and the
// differential tests compare printed results.

fn list_items<'p>(v: &Value<'p>) -> Option<Vec<Value<'p>>> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Data(ref n, _, ref fs) if &**n == "Nil" && fs.is_empty() => return Some(out),
            Value::Data(ref n, _, ref fs) if &**n == "Cons" && fs.len() == 2 => {
                out.push(fs[0].clone());
                let next = fs[1].clone();
                cur = next;
            }
            _ => return None,
        }
    }
}

fn is_vector_ctor(name: &str) -> bool {
    matches!(name, "VEmpty" | "VSingle" | "VFull")
}

fn vector_elems<'p>(v: &Value<'p>) -> Option<Vec<Value<'p>>> {
    match v {
        Value::Data(n, _, fs) if &**n == "VEmpty" && fs.is_empty() => Some(Vec::new()),
        Value::Data(n, _, fs) if &**n == "VSingle" && fs.len() == 1 => match &fs[0] {
            Value::Array(xs) => Some(xs.iter().cloned().collect()),
            _ => None,
        },
        Value::Data(n, _, fs) if &**n == "VFull" && fs.len() == 7 => {
            let mut out = Vec::new();
            for i in [2usize, 3] {
                match &fs[i] {
                    Value::Array(xs) => out.extend(xs.iter().cloned()),
                    _ => return None,
                }
            }
            vector_node_elems(&fs[4], &mut out)?;
            for i in [5usize, 6] {
                match &fs[i] {
                    Value::Array(xs) => out.extend(xs.iter().cloned()),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn vector_node_elems<'p>(n: &Value<'p>, out: &mut Vec<Value<'p>>) -> Option<()> {
    match n {
        Value::Data(name, _, fs) if &**name == "VLeaf" && fs.len() == 1 => match &fs[0] {
            Value::Array(xs) => {
                out.extend(xs.iter().cloned());
                Some(())
            }
            _ => None,
        },
        Value::Data(name, _, fs) if &**name == "VBranch" && fs.len() == 2 => match &fs[1] {
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

/// Structural equality, iteratively — a `Cons` chain is as deep as it is long.
pub fn value_eq<'p>(a: &Value<'p>, b: &Value<'p>) -> bool {
    let mut stack = vec![(a.clone(), b.clone())];
    while let Some((a, b)) = stack.pop() {
        match (&a, &b) {
            // A `Vector` is a tree, so two equal ones need not have equal
            // shapes. Compare the sequences they denote instead.
            (Value::Data(na, _, _), Value::Data(nb, _, _))
                if is_vector_ctor(na) && is_vector_ctor(nb) =>
            {
                match (vector_elems(&a), vector_elems(&b)) {
                    (Some(xs), Some(ys)) if xs.len() == ys.len() => {
                        stack.extend(xs.into_iter().zip(ys));
                    }
                    _ => return false,
                }
            }
            (Value::Int(x), Value::Int(y)) if x == y => {}
            (Value::BigInt(x), Value::BigInt(y)) if x == y => {}
            (Value::Float(x), Value::Float(y)) if x == y => {}
            (Value::Bool(x), Value::Bool(y)) if x == y => {}
            (Value::Str(x), Value::Str(y)) if x == y => {}
            (Value::Char(x), Value::Char(y)) if x == y => {}
            (Value::Unit, Value::Unit) => {}
            (Value::Data(n1, _, x), Value::Data(n2, _, y)) => {
                if n1 != n2 {
                    return false;
                }
                // The same allocation is trivially equal to itself — which is
                // what makes `xs == xs` on a long list O(1) rather than O(n).
                if Rc::ptr_eq(x, y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().cloned().zip(y.iter().cloned()));
            }
            (Value::Array(x), Value::Array(y)) => {
                if Rc::ptr_eq(x, y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().cloned().zip(y.iter().cloned()));
            }
            (Value::Record(x), Value::Record(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                for (k, v) in x.iter() {
                    match y.get(k) {
                        Some(w) => stack.push((v.clone(), w.clone())),
                        None => return false,
                    }
                }
            }
            // Identity, not contents: a `Ref` is a place, and two cells holding
            // the same thing are still two cells.
            (Value::Ref(x), Value::Ref(y)) if Rc::ptr_eq(x, y) => {}
            _ => return false,
        }
    }
    true
}

/// The primitives, mirroring `meadow_eval::run_prim` operation for operation —
/// including the wrapping arithmetic and the two zero checks, because the
/// differential tests compare answers and an overflow that panicked on one side
/// and wrapped on the other would be a difference in the harness rather than in
/// the machine.
fn prim<'p>(
    op: Prim,
    args: &[Value<'p>],
    tags: &HashMap<InternedString, Tag>,
) -> Result<Value<'p>, Error> {
    use Prim::*;

    let int2 = |a: &Value, b: &Value| -> Result<(i64, i64), Error> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok((*x, *y)),
            _ => err(format!("expected two Ints, got {a} and {b}")),
        }
    };
    let big2 = |a: &Value, b: &Value| -> Result<(BigInt, BigInt), Error> {
        match (a, b) {
            (Value::BigInt(x), Value::BigInt(y)) => Ok(((**x).clone(), (**y).clone())),
            _ => err(format!("expected two BigInts, got {a} and {b}")),
        }
    };
    let flt2 = |a: &Value, b: &Value| -> Result<(f64, f64), Error> {
        match (a, b) {
            (Value::Float(x), Value::Float(y)) => Ok((*x, *y)),
            _ => err(format!("expected two Floats, got {a} and {b}")),
        }
    };
    let data = |name: &str, fields: Vec<Value<'p>>| {
        let name = InternedString::from(name);
        // A constructor a primitive builds must carry the tag the program's
        // `switch` arms were compiled with, or a `match` on the result would
        // miss. A name the program never mentions gets a tag no arm has.
        let tag = tags.get(&name).copied().unwrap_or(Tag::MAX);
        Value::Data(name, tag, Rc::new(Fields(fields)))
    };

    match op {
        Add | Sub | Mul | Div | Mod | Pow => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Add => x.wrapping_add(y),
                Sub => x.wrapping_sub(y),
                Mul => x.wrapping_mul(y),
                Div if y == 0 => return err("division by zero"),
                Div => x.wrapping_div(y),
                Mod if y == 0 => return err("modulo by zero"),
                Mod => x.wrapping_rem(y),
                _ => {
                    let e = u32::try_from(y).map_err(|_| Error {
                        msg: format!("`^` exponent must fit in u32, got {y}"),
                    })?;
                    x.wrapping_pow(e)
                }
            };
            Ok(Value::Int(r))
        }
        Lt | Gt | Le | Ge => {
            let (x, y) = int2(&args[0], &args[1])?;
            Ok(Value::Bool(match op {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                _ => x >= y,
            }))
        }
        AddB | SubB | MulB | DivB | ModB | PowB => {
            let (x, y) = big2(&args[0], &args[1])?;
            let r = match op {
                AddB => x + y,
                SubB => x - y,
                MulB => x * y,
                DivB if y.is_zero() => return err("division by zero"),
                DivB => x / y,
                ModB if y.is_zero() => return err("modulo by zero"),
                ModB => x % y,
                _ => {
                    let e = y.to_u32().ok_or_else(|| Error {
                        msg: format!("`^~` exponent must fit in u32, got {y}"),
                    })?;
                    x.pow(e)
                }
            };
            Ok(Value::BigInt(Rc::new(r)))
        }
        LtB | GtB | LeB | GeB => {
            let (x, y) = big2(&args[0], &args[1])?;
            Ok(Value::Bool(match op {
                LtB => x < y,
                GtB => x > y,
                LeB => x <= y,
                _ => x >= y,
            }))
        }
        AddF | SubF | MulF | DivF => {
            let (x, y) = flt2(&args[0], &args[1])?;
            Ok(Value::Float(match op {
                AddF => x + y,
                SubF => x - y,
                MulF => x * y,
                _ => x / y,
            }))
        }
        LtF | GtF | LeF | GeF => {
            let (x, y) = flt2(&args[0], &args[1])?;
            Ok(Value::Bool(match op {
                LtF => x < y,
                GtF => x > y,
                LeF => x <= y,
                _ => x >= y,
            }))
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
            Value::Int(x) => Ok(Value::BigInt(Rc::new(BigInt::from(*x)))),
            other => err(format!("`toBigInt` expects an Int, got {other}")),
        },
        ToInt => match &args[0] {
            Value::BigInt(x) => x.to_i64().map(Value::Int).ok_or_else(|| Error {
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

        // --- the builtin `Array` ---------------------------------------------
        ArrayLen => Ok(Value::Int(as_array(&args[0])?.len() as i64)),
        ArrayGet => {
            let a = as_array(&args[0])?;
            let i = as_index(&args[1])?;
            a.get(i).cloned().ok_or_else(|| Error {
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

        // --- bitwise ----------------------------------------------------------
        Shl | Shr | Ushr | BitAnd | BitOr | BitXor => {
            let (x, y) = int2(&args[0], &args[1])?;
            Ok(Value::Int(match op {
                Shl => x.wrapping_shl(y as u32),
                Shr => x.wrapping_shr(y as u32),
                Ushr => (x as u64).wrapping_shr(y as u32) as i64,
                BitAnd => x & y,
                BitOr => x | y,
                _ => x ^ y,
            }))
        }
        BitNot => Ok(Value::Int(!as_int(&args[0])?)),
        PopCount => Ok(Value::Int(as_int(&args[0])?.count_ones() as i64)),

        // --- text and bytes ---------------------------------------------------
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
                s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble"));
                s.push(char::from_digit((b & 0xf) as u32, 16).expect("nibble"));
            }
            Ok(Value::Str(InternedString::from(s)))
        }
        BytesFromHex => {
            let s = match &args[0] {
                Value::Str(s) => *s,
                other => return err(format!("`bytesFromHex` expects a String, got {other}")),
            };
            let bytes = s.as_bytes();
            if bytes.len() % 2 != 0 {
                return Ok(data("None", vec![]));
            }
            let mut out = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.chunks_exact(2) {
                match (
                    (pair[0] as char).to_digit(16),
                    (pair[1] as char).to_digit(16),
                ) {
                    (Some(h), Some(l)) => out.push(Value::Int(((h << 4) | l) as i64)),
                    _ => return Ok(data("None", vec![])),
                }
            }
            Ok(data("Just", vec![Value::Array(Rc::new(out))]))
        }
        Show => Ok(Value::Str(InternedString::from(args[0].to_string()))),
        CharCode => match &args[0] {
            Value::Char(c) => Ok(Value::Int(*c as i64)),
            other => err(format!("charCode: expected a Char, got {other}")),
        },
        CharFromCode => match &args[0] {
            Value::Int(n) => u32::try_from(*n)
                .ok()
                .and_then(char::from_u32)
                .map(Value::Char)
                .ok_or_else(|| Error {
                    msg: format!("charFromCode: {n} is not a Unicode scalar value"),
                }),
            other => err(format!("charFromCode: expected an Int, got {other}")),
        },
        StringToChars => match &args[0] {
            Value::Str(s) => Ok(Value::Array(Rc::new(s.chars().map(Value::Char).collect()))),
            other => err(format!("stringToChars: expected a String, got {other}")),
        },
        CharsToString => match &args[0] {
            Value::Array(xs) => {
                let mut out = String::with_capacity(xs.len());
                for v in xs.iter() {
                    match v {
                        Value::Char(c) => out.push(*c),
                        other => return err(format!("charsToString: expected a Char, got {other}")),
                    }
                }
                Ok(Value::Str(InternedString::from(out)))
            }
            other => err(format!("charsToString: expected an Array, got {other}")),
        },

        // --- the mutable cell -------------------------------------------------
        NewRef => Ok(Value::Ref(Rc::new(RefCell::new(args[0].clone())))),
        GetRef => match &args[0] {
            Value::Ref(cell) => Ok(cell.borrow().clone()),
            other => err(format!("getRef: expected a Ref, got {other}")),
        },
        SetRef => match &args[0] {
            Value::Ref(cell) => {
                *cell.borrow_mut() = args[1].clone();
                Ok(Value::Unit)
            }
            other => err(format!("setRef: expected a Ref, got {other}")),
        },
    }
}

fn as_array<'a, 'p>(v: &'a Value<'p>) -> Result<&'a Rc<Vec<Value<'p>>>, Error> {
    match v {
        Value::Array(a) => Ok(a),
        other => err(format!("expected an Array, got {other}")),
    }
}

fn as_int(v: &Value) -> Result<i64, Error> {
    match v {
        Value::Int(i) => Ok(*i),
        other => err(format!("expected an Int, got {other}")),
    }
}

fn as_index(v: &Value) -> Result<usize, Error> {
    match as_int(v)? {
        i if i >= 0 => Ok(i as usize),
        i => err(format!("expected a non-negative index, got {i}")),
    }
}

fn bytes_of(v: &Value, what: &str) -> Result<Vec<u8>, Error> {
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

impl std::fmt::Display for Value<'_> {
    /// Deliberately the same rendering as the CEK's. Two reasons, and the second
    /// is the load-bearing one: differential tests compare printed results, and
    /// `show` is a primitive, so a program can observe this.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::BigInt(n) => write!(f, "{n}"),
            Value::Float(x) => f.write_str(&meadow_core::fmt_float(*x)),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Str(s) => write!(f, "{:?}", &**s),
            Value::Char(c) => write!(f, "{c:?}"),
            Value::Unit => f.write_str("()"),
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
            Value::Data(name, _, fields) if &**name == "#tuple" => {
                f.write_str("(")?;
                for (i, v) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            // `Std.Collections.List` prints in its own literal syntax: `[1; 2]`.
            Value::Data(name, _, _) if matches!(&**name, "Nil" | "Cons") => {
                if let Some(xs) = list_items(self) {
                    f.write_str("[")?;
                    for (i, v) in xs.iter().enumerate() {
                        if i > 0 {
                            f.write_str("; ")?;
                        }
                        write!(f, "{v}")?;
                    }
                    return f.write_str("]");
                }
                write!(f, "{name}(..)")
            }
            // `Std.Collections.Vector` prints like a list: `[1, 2]`.
            Value::Data(name, _, _) if is_vector_ctor(name) => {
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
            Value::Data(name, _, fields) if fields.is_empty() => write!(f, "{name}"),
            Value::Data(name, _, fields) => {
                write!(f, "{name}(")?;
                for (i, v) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Ref(cell) => write!(f, "ref {}", cell.borrow()),
            Value::Obj(_) => f.write_str("<closure>"),
            Value::Resume(_) => f.write_str("<continuation>"),
            Value::Halt => f.write_str("<halt>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Def, Label, VarId};

    fn v(n: u32) -> Name {
        VarId(n)
    }

    fn program(body: Statement) -> Program {
        Program {
            defs: vec![Def {
                label: Label(0),
                name: InternedString::from("main"),
                block: Block {
                    params: vec![v(0)],
                    body,
                },
            }],
            entry: Some(Label(0)),
            tags: Default::default(),
        }
    }

    /// `substitute [k, x] in {(k, x) => invoke k#0}` — the universal return.
    fn ret(k: Name, x: Name) -> Statement {
        Statement::Substitute(
            vec![k, x],
            Box::new(Block {
                params: vec![k, x],
                body: Statement::Invoke(k, 0),
            }),
        )
    }

    #[test]
    fn a_program_ends_by_invoking_the_continuation_it_was_given() {
        // extern lit 7 -> (x, k); substitute [k, x] in { invoke k#0 }
        let p = program(Statement::Extern {
            op: Extern::Lit(Lit::Int(7)),
            args: vec![],
            blocks: vec![Block {
                params: vec![v(1), v(0)],
                body: ret(v(0), v(1)),
            }],
        });
        let out = Machine::run(&p, 100).expect("should finish");
        assert_eq!(out.to_string(), "7");
    }

    #[test]
    fn a_block_that_does_not_cover_the_environment_is_rejected() {
        // The extern prepends its result to an environment of one, so the
        // continuation must take two parameters. Taking one is the exact mistake
        // a lowering with a wrong liveness set makes, and it has to be loud —
        // otherwise the register assignment the IR claims to encode is a
        // fiction.
        let p = program(Statement::Extern {
            op: Extern::Lit(Lit::Int(7)),
            args: vec![],
            blocks: vec![Block {
                params: vec![v(1)],
                body: Statement::Error("unreachable"),
            }],
        });
        let e = Machine::run(&p, 100).expect_err("should be rejected");
        assert!(
            e.msg.contains("1 parameters") && e.msg.contains("2 values"),
            "{}",
            e.msg
        );
    }

    #[test]
    fn switch_dispatches_on_the_tag_and_binds_the_fields() {
        // let d = B#1(x); switch d { #1 (field, d, x, k) => return field, … }
        //
        // The scrutinee's fields go on the front and the scrutinee stays, so the
        // arm sees four values where the environment had three. Writing it out
        // is the clearest statement of what `switch` does — and of why an arm
        // body may still name the thing it matched on.
        let inner = Statement::Switch {
            scrutinee: v(2),
            arms: vec![(
                1,
                Block {
                    params: vec![v(3), v(2), v(1), v(0)],
                    body: ret(v(0), v(3)),
                },
            )],
            default: Box::new(Block {
                params: vec![v(2), v(1), v(0)],
                body: Statement::Error("wrong arm"),
            }),
        };
        let p = program(Statement::Extern {
            op: Extern::Lit(Lit::Int(5)),
            args: vec![],
            blocks: vec![Block {
                params: vec![v(1), v(0)],
                body: Statement::Let {
                    name: v(2),
                    tag: 1,
                    ctor: InternedString::from("B"),
                    fields: vec![v(1)],
                    rest: Box::new(inner),
                },
            }],
        });
        assert_eq!(Machine::run(&p, 100).unwrap().to_string(), "5");
    }

    #[test]
    fn a_switch_with_no_matching_arm_takes_the_default() {
        let inner = Statement::Switch {
            scrutinee: v(2),
            arms: vec![(
                9,
                Block {
                    params: vec![v(3), v(1), v(0)],
                    body: Statement::Error("wrong arm"),
                },
            )],
            default: Box::new(Block {
                params: vec![v(2), v(1), v(0)],
                body: ret(v(0), v(1)),
            }),
        };
        let p = program(Statement::Extern {
            op: Extern::Lit(Lit::Int(5)),
            args: vec![],
            blocks: vec![Block {
                params: vec![v(1), v(0)],
                body: Statement::Let {
                    name: v(2),
                    tag: 1,
                    ctor: InternedString::from("B"),
                    fields: vec![v(1)],
                    rest: Box::new(inner),
                },
            }],
        });
        assert_eq!(Machine::run(&p, 100).unwrap().to_string(), "5");
    }

    #[test]
    fn fuel_bounds_a_loop_instead_of_hanging_a_test() {
        // `jump` leaves the environment alone, so this is a genuine infinite
        // loop with no growth — nothing else would ever stop it.
        let p = program(Statement::Jump(Label(0)));
        let e = Machine::run(&p, 100).expect_err("should run out");
        assert!(e.msg.contains("100 steps"), "{}", e.msg);
    }

    #[test]
    fn data_prints_the_way_the_cek_prints_it() {
        let d = Value::Data(
            InternedString::from("Rect"),
            0,
            Rc::new(Fields(vec![Value::Int(3), Value::Int(4)])),
        );
        assert_eq!(d.to_string(), "Rect(3, 4)");
        let t = Value::Data(
            InternedString::from("#tuple"),
            0,
            Rc::new(Fields(vec![Value::Int(1), Value::Unit])),
        );
        assert_eq!(t.to_string(), "(1, ())");
        // A `List` prints as its elements, not as the chain it is.
        let nil = Value::Data(InternedString::from("Nil"), 0, Rc::new(Fields(vec![])));
        let one = Value::Data(
            InternedString::from("Cons"),
            1,
            Rc::new(Fields(vec![Value::Int(1), nil])),
        );
        assert_eq!(one.to_string(), "[1]");
    }

    #[test]
    fn dropping_a_long_chain_does_not_recurse() {
        // 200_000 deep — the shape that aborted the CEK before its `Drop` was
        // made iterative.
        let mut xs = Value::Data(InternedString::from("Nil"), 0, Rc::new(Fields(vec![])));
        for i in 0..200_000 {
            xs = Value::Data(
                InternedString::from("Cons"),
                1,
                Rc::new(Fields(vec![Value::Int(i), xs])),
            );
        }
        assert!(value_eq(&xs, &xs.clone()));
        drop(xs);
    }
}
