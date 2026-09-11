//! The virtual machine.
//!
//! A program counter, a flat register file, a collected heap, and a stack of
//! installed handlers. That is the whole of it — there is **no call stack**.
//!
//! # Why there is no call stack
//!
//! Because the compiler already removed the need for one. In the sequent IR the
//! back end works from, a function is handed the continuation it should answer,
//! and returning is entering that continuation. A closure, a continuation and a
//! handler are the same kind of heap object, and [`Op::Invoke`] does the same
//! thing to all three: rebuild the register file as the object's captures
//! followed by its arguments, and jump.
//!
//! So nothing is pushed on a call and nothing is popped on a return. Recursion
//! grows the heap, which is collected, rather than a stack, which is not. A
//! tail call is not an optimisation here; it is what an ordinary call already
//! is.
//!
//! # Registers and the collector
//!
//! One flat file of 256 registers, no frame pointer. The compiler uses 255 of
//! them; the top one is [`TEMP`], where a fused compare-and-branch puts the
//! boolean it is about to throw away. The collector's root set is
//! `r0..live`, and `live` is maintained by the instructions that know it:
//! [`Op::Jump`] carries the target block's arity, [`Op::Invoke`] sets it from the
//! object's captures plus its arguments, and any instruction writing `r[a]`
//! raises it to `a + 1`. Registers above `live` are dead by construction; ones
//! below it may be stale, which retains a bounded amount of garbage and is the
//! price of not emitting liveness metadata per instruction.
//!
//! The rule the whole file obeys: **never hold an address across an
//! allocation.** [`Vm::ensure`] is called first, with room for everything the
//! operation will build, and arguments are read out of registers afterwards.
//! Registers are roots; Rust locals are not.

use crate::heap::{Heap, Kind};
use crate::value::{Addr, Value};
use meadow_bytecode::{Const, Instr, Op, Pc, Program, Reg};
use meadow_intern::InternedString;

/// How many registers there are. The compiler refuses to emit a block needing
/// more.
pub const REGISTERS: usize = 256;

/// Where the scratch area begins. The VM keeps a few slots the program cannot
/// name, so that rebuilding the register file on a call can go through them
/// instead of through a heap-allocated `Vec` — it was one malloc per call.
/// Nothing allocates while they are in use, so the collector never sees them.
pub(crate) const SCRATCH: usize = REGISTERS;
const SCRATCH_LEN: usize = 256;

/// The whole register file, as one fixed-size block.
pub(crate) type Regs = [Value; REGISTERS + SCRATCH_LEN];

/// The one register the compiler will not use, kept for a value the program
/// cannot name: the boolean a fused compare-and-branch tests and discards.
///
/// It is deliberately **not** a collector root — writing it does not raise
/// `live` — which is safe only because a fused branch's primitive is always a
/// comparison, and no comparison allocates. `meadow_codegen` will not emit one
/// for anything else ([`meadow_core::Prim::compares`]), and its register
/// allocator stops one short of the file so this slot stays free.
pub(crate) const TEMP: Reg = (REGISTERS - 1) as Reg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { msg: msg.into() })
}

/// An installed handler.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// Which entry of [`Program::handled`] says what this covers.
    handled: u32,
    handler: Value,
    /// Where the `handle` expression's value goes.
    ///
    /// Not fixed: a resumption rebinds it to the point it was resumed from, so
    /// that a body which was suspended and restarted returns into the middle of
    /// the clause that restarted it.
    ret_k: Value,
}

pub struct Vm<'p> {
    pub(crate) program: &'p Program,
    pub(crate) heap: Heap,
    /// The register file: 256 the program can name, then a scratch area it
    /// cannot. A boxed array rather than a `Vec` so its length is a constant —
    /// which is what lets the bounds check on every register access fold away.
    pub(crate) regs: Box<Regs>,
    live: usize,
    handlers: Vec<Frame>,
    pc: usize,
    /// Instructions retired.
    pub steps: u64,
    /// The tag `False` has in this program, if it has one — what a conditional
    /// branches on when a program builds a boolean by naming its constructor
    /// rather than writing a literal.
    false_tag: Option<u32>,
}

/// Run `program` from its entry point and render the result the way the CEK
/// machine would.
///
/// The rendering rather than the value, because a [`Value`] means nothing away
/// from the heap it indexes — and because printing is what every caller wanted.
pub fn run(program: &Program, fuel: u64) -> Result<String, Error> {
    let Some(entry) = program.entry else {
        return err("program has no entry point");
    };
    let mut vm = Vm::new(program);
    let v = vm.run(entry, fuel)?;
    Ok(vm.show(v))
}

impl<'p> Vm<'p> {
    pub fn new(program: &'p Program) -> Vm<'p> {
        let false_tag = program
            .ctors
            .iter()
            .position(|c| &**c == "False")
            .map(|i| i as u32);
        Vm {
            program,
            heap: Heap::new(),
            regs: Box::new([Value::Unit; REGISTERS + SCRATCH_LEN]),
            live: 0,
            handlers: Vec::new(),
            pc: 0,
            steps: 0,
            false_tag,
        }
    }

    /// Run from `entry` until the program halts or `fuel` instructions retire.
    ///
    /// Bounded rather than open-ended because this is what differential tests
    /// call: a code generation bug that loops should fail a test, not hang it.
    pub fn run(&mut self, entry: Pc, fuel: u64) -> Result<Value, Error> {
        self.start(entry);
        // Saturating: `u64::MAX` is how a caller says "no limit", and adding it
        // to a machine that has already run would otherwise overflow.
        let limit = self.steps.saturating_add(fuel);
        loop {
            if self.steps >= limit {
                return err(format!("ran for {fuel} instructions without finishing"));
            }
            if let Some(v) = self.step()? {
                return Ok(v);
            }
        }
    }

    /// Put the machine at `entry` with the halt continuation in `r0`.
    ///
    /// Method table 0 holds instruction 0, which is `halt r0`. An entry block
    /// takes one parameter — the continuation to answer with — so returning from
    /// `main` needs no special case: it is an ordinary invoke that lands there.
    pub fn start(&mut self, entry: Pc) {
        self.heap.reserve(1);
        let halt = self.heap.alloc(Kind::Closure, 0, &[]);
        self.regs[0] = Value::Obj(halt);
        self.live = 1;
        self.handlers.clear();
        self.pc = entry as usize;
    }

    /// One instruction. `Some` means the program halted.
    pub fn step(&mut self) -> Result<Option<Value>, Error> {
        let Some(&i) = self.program.code.get(self.pc) else {
            return err(format!("pc {} is outside the program", self.pc));
        };
        self.pc += 1;
        self.steps += 1;

        match i.op {
            Op::Nop => {}

            Op::Move => {
                let v = self.reg(i.b);
                self.set(i.a, v);
            }

            Op::Const => {
                let v = self.constant(i.imm)?;
                self.set(i.a, v);
            }

            Op::Jump => {
                self.pc = i.imm as usize;
                self.live = i.a as usize;
            }

            Op::JumpUnless => {
                if self.falsey(self.reg(i.a)) {
                    self.pc = i.imm as usize;
                }
            }

            Op::JumpUnlessTag => {
                let want = i.bc() as u32;
                let hit = match self.reg(i.a) {
                    Value::Obj(a) => {
                        self.heap.kind(a) == Kind::Data && self.heap.meta(a) == want
                    }
                    _ => false,
                };
                if !hit {
                    self.pc = i.imm as usize;
                }
            }

            Op::Halt => return Ok(Some(self.reg(i.a))),

            Op::Error => {
                let msg = self
                    .program
                    .messages
                    .get(i.imm as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("error {}", i.imm));
                return err(msg);
            }

            Op::MakeData => {
                let n = i.c as usize;
                self.ensure(1 + n);
                let base = i.b as usize;
                let a = { let Vm { heap, regs, .. } = self; heap.alloc(Kind::Data, i.imm, &regs[base..base + n]) };
                self.set(i.a, Value::Obj(a));
            }

            Op::MakeArray => {
                let n = i.c as usize;
                self.ensure(1 + n);
                let base = i.b as usize;
                let a = { let Vm { heap, regs, .. } = self; heap.alloc(Kind::Array, 0, &regs[base..base + n]) };
                self.set(i.a, Value::Obj(a));
            }

            Op::MakeRecord => {
                let n = i.c as usize;
                let Some(shape) = self.program.shapes.get(i.imm as usize) else {
                    return err(format!("no record shape {}", i.imm));
                };
                if shape.len() != n {
                    return err(format!(
                        "record shape has {} fields but {n} values were given",
                        shape.len()
                    ));
                }
                self.ensure(1 + 2 * n);
                // Sorted by label, so two records with the same fields are the
                // same object however they were written.
                let mut pairs: Vec<(InternedString, Value)> = self.program.shapes[i.imm as usize]
                    .iter()
                    .copied()
                    .zip((0..n).map(|j| self.reg(i.b + j as Reg)))
                    .collect();
                pairs.sort_by_key(|(l, _)| *l);
                let mut fields = Vec::with_capacity(2 * n);
                for (l, v) in pairs {
                    fields.push(Value::Str(l));
                    fields.push(v);
                }
                let a = self.heap.alloc(Kind::Record, 0, &fields);
                self.set(i.a, Value::Obj(a));
            }

            Op::Field => {
                let v = self.reg(i.b);
                let Some(a) = v.addr() else {
                    return err(format!("took field {} of {}", i.imm, v.kind()));
                };
                match self.heap.kind(a) {
                    Kind::Data | Kind::Array => {}
                    other => return err(format!("took field {} of a {other:?}", i.imm)),
                }
                let n = self.heap.len(a);
                if (i.imm as usize) >= n {
                    return err(format!("field {} of an object with {n}", i.imm));
                }
                let f = self.heap.field(a, i.imm as usize);
                self.set(i.a, f);
            }

            Op::Select => {
                let label = self.label(i.imm)?;
                let v = self.reg(i.b);
                let Some(a) = v.addr().filter(|a| self.heap.kind(*a) == Kind::Record) else {
                    return err(format!("selected `.{label}` from {}", v.kind()));
                };
                match self.record_get(a, label) {
                    Some(v) => self.set(i.a, v),
                    None => return err(format!("no field `{label}` on this record")),
                }
            }

            Op::Extend => {
                let label = self.label(i.imm)?;
                let src = self.reg(i.b);
                let Some(a) = src.addr().filter(|a| self.heap.kind(*a) == Kind::Record) else {
                    return err(format!("extended {} with `.{label}`", src.kind()));
                };
                // Measure, make room, then re-read: the collection `ensure` may
                // run would move the record we are about to copy.
                let existing = self.heap.len(a) / 2;
                self.ensure(1 + 2 * (existing + 1));
                let a = self.reg(i.b).addr().expect("still a record");
                let value = self.reg(i.c);

                let mut pairs: Vec<(InternedString, Value)> = Vec::with_capacity(existing + 1);
                for j in 0..self.heap.len(a) / 2 {
                    match self.heap.field(a, 2 * j) {
                        Value::Str(l) => pairs.push((l, self.heap.field(a, 2 * j + 1))),
                        other => return err(format!("record label is {}", other.kind())),
                    }
                }
                match pairs.iter_mut().find(|(l, _)| *l == label) {
                    Some(slot) => slot.1 = value,
                    None => pairs.push((label, value)),
                }
                pairs.sort_by_key(|(l, _)| *l);
                let mut fields = Vec::with_capacity(pairs.len() * 2);
                for (l, v) in pairs {
                    fields.push(Value::Str(l));
                    fields.push(v);
                }
                let out = self.heap.alloc(Kind::Record, 0, &fields);
                self.set(i.a, Value::Obj(out));
            }

            Op::Closure => {
                let n = i.c as usize;
                self.ensure(1 + n);
                let base = i.b as usize;
                let a = { let Vm { heap, regs, .. } = self; heap.alloc(Kind::Closure, i.imm, &regs[base..base + n]) };
                self.set(i.a, Value::Obj(a));
            }

            Op::Invoke => return self.invoke(i),

            // Three shapes of the same thing: one and two arguments name their
            // registers, three come from a window.
            Op::Prim | Op::Prim1 | Op::Prim2 => {
                let Some(&p) = self.program.prims.get(i.imm as usize) else {
                    return err(format!("no primitive {}", i.imm));
                };
                let srcs = match i.op {
                    Op::Prim1 => [i.b, 0, 0],
                    Op::Prim2 => [i.b, i.c, 0],
                    _ => [i.b, i.b.wrapping_add(1), i.b.wrapping_add(2)],
                };
                self.run_prim(p, srcs, i.a)?;
            }

            // The right operand is a constant, and it goes into the destination
            // register before the primitive runs: `run_prim` re-reads its
            // arguments out of registers after any collection, and writes the
            // result last, so the destination is exactly the right place to
            // park it. The compiler never gives a folded primitive a
            // destination that is also its left operand.
            Op::PrimK => {
                debug_assert_ne!(i.a, i.b, "a folded primitive overwrote its own operand");
                let p = self.primitive(i.c as u32)?;
                let v = self.constant(i.imm)?;
                self.set(i.a, v);
                self.run_prim(p, [i.b, i.a, 0], i.a)?;
            }

            // A comparison and the branch that tests it. The boolean goes
            // nowhere the program can see, so it needs no register of its own.
            Op::JumpUnlessPrim | Op::JumpUnlessPrimK => {
                let p = self.primitive(i.c as u32)?;
                // The boolean — and a folded constant — go in [`TEMP`], which
                // the program cannot name. Writing any register raises `live`,
                // and leaving it raised would make the whole file a collector
                // root until the next jump, so it is put back. Nothing can
                // observe the gap: a fused branch's primitive is a comparison,
                // and no comparison allocates.
                let live = self.live;
                let srcs = if i.op == Op::JumpUnlessPrimK {
                    let v = self.constant(i.b as u32)?;
                    self.set(TEMP, v);
                    [i.a, TEMP, 0]
                } else {
                    [i.a, i.b, 0]
                };
                self.run_prim(p, srcs, TEMP)?;
                let cond = self.reg(TEMP);
                self.live = live;
                if self.falsey(cond) {
                    self.pc = i.imm as usize;
                }
            }


            Op::Handle => {
                let handler = self.reg(i.a);
                let ret_k = self.reg(i.b);
                self.handlers.push(Frame {
                    handled: i.imm,
                    handler,
                    ret_k,
                });
            }

            Op::Unhandle => {
                let Some(frame) = self.handlers.pop() else {
                    return err("unhandle with no handler installed");
                };
                self.set(i.a, frame.ret_k);
            }

            Op::Perform => self.perform(i)?,
        }
        Ok(None)
    }

    // --- control ----------------------------------------------------------

    /// Enter a method of the object in `r[a]`, or resume a continuation.
    ///
    /// The one calling convention: the register file becomes the object's
    /// captures followed by the arguments, and the pc becomes the method's
    /// entry. Nothing is saved, because the only way back is a continuation the
    /// caller already passed as one of those arguments.
    fn invoke(&mut self, i: Instr) -> Result<Option<Value>, Error> {
        let obj = self.reg(i.a);
        let Some(a) = obj.addr() else {
            return err(format!("invoked {}, which is not an object", obj.kind()));
        };
        let base = i.c;
        let argc = i.imm as usize;

        match self.heap.kind(a) {
            Kind::Closure => {
                let ncap = self.heap.len(a);
                if ncap + argc > REGISTERS {
                    return err("a call needs more than 256 registers");
                }
                let table = self.heap.meta(a) as usize;
                let Some(&pc) = self
                    .program
                    .methods
                    .get(table)
                    .and_then(|t| t.get(i.b as usize))
                else {
                    return err(format!("no method #{} on this object", i.b));
                };
                // Arguments out of the way first: writing the captures into
                // r0.. would otherwise clobber the window they sit in. Through
                // the scratch area rather than a `Vec`, because this is the
                // calling convention and it ran a heap allocation per call.
                let base = base as usize;
                self.regs.copy_within(base..base + argc, SCRATCH);
                for j in 0..ncap {
                    self.regs[j] = self.heap.field(a, j);
                }
                self.regs.copy_within(SCRATCH..SCRATCH + argc, ncap);
                self.live = ncap + argc;
                self.pc = pc as usize;
                Ok(None)
            }

            // Resuming is an ordinary call from the program's point of view, so
            // it arrives here: `invoke r#0 (value, continuation)`.
            Kind::Resume => {
                if i.b != 0 {
                    return err(format!("a resumption has no method #{}", i.b));
                }
                if argc != 2 {
                    return err(format!(
                        "resuming takes a value and a continuation, got {argc} arguments"
                    ));
                }
                if self.heap.field(a, 0) == Value::Bool(true) {
                    return err("continuation resumed more than once");
                }
                self.heap.set_field(a, 0, Value::Bool(true));

                let value = self.reg(base);
                let here = self.reg(base + 1);
                let k = self.heap.field(a, 1);

                // Put the handler frames back, with the one this resumption was
                // cut from now answering the resume site.
                let n = (self.heap.len(a) - 2) / 3;
                for j in 0..n {
                    let handled = match self.heap.field(a, 2 + 3 * j) {
                        Value::Int(x) => x as u32,
                        other => return err(format!("resumption frame is {}", other.kind())),
                    };
                    let handler = self.heap.field(a, 3 + 3 * j);
                    let ret_k = if j == 0 {
                        here
                    } else {
                        self.heap.field(a, 4 + 3 * j)
                    };
                    self.handlers.push(Frame {
                        handled,
                        handler,
                        ret_k,
                    });
                }
                self.deliver(k, value)?;
                Ok(None)
            }

            other => err(format!("invoked a {other:?}")),
        }
    }

    /// Hand `v` to the continuation `k`.
    fn deliver(&mut self, k: Value, v: Value) -> Result<(), Error> {
        let Some(a) = k.addr().filter(|a| self.heap.kind(*a) == Kind::Closure) else {
            return err(format!("{} is not a continuation", k.kind()));
        };
        let table = self.heap.meta(a) as usize;
        let Some(&pc) = self.program.methods.get(table).and_then(|t| t.first()) else {
            return err("a continuation with no method");
        };
        let ncap = self.heap.len(a);
        if ncap + 1 > REGISTERS {
            return err("a continuation needs more than 256 registers");
        }
        for j in 0..ncap {
            self.regs[j] = self.heap.field(a, j);
        }
        self.regs[ncap] = v;
        self.live = ncap + 1;
        self.pc = pc as usize;
        Ok(())
    }

    /// Unwind to the innermost handler covering the operation and enter its
    /// clause.
    fn perform(&mut self, i: Instr) -> Result<(), Error> {
        let Some(&(effect, op)) = self.program.ops.get(i.imm as usize) else {
            return err(format!("no operation {}", i.imm));
        };
        let covers = |f: &Frame| {
            self.program.handled[f.handled as usize]
                .iter()
                .any(|(e, o)| *e == effect && *o == op)
        };
        let Some(idx) = self.handlers.iter().rposition(covers) else {
            // No handler. A few effects mean something anyway: `Fs`, `Process`,
            // `Random` and `Time` reach the real world (see [`crate::native`]),
            // and `Test.fail` is a failed assertion, which is a runtime error
            // carrying the assertion's own message for a test runner to read.
            if &*effect == "Test" && &*op == "fail" {
                let v = self.reg(i.a);
                return err(self.show(v));
            }
            let arg = self.reg(i.a);
            if let Some(v) = self.native(&effect, &op, arg)? {
                // Read the continuation back out of its register: building the
                // result may have collected, and registers are what the
                // collector updates.
                let k = self.reg(i.b);
                return self.deliver(k, v);
            }
            return err(format!("unhandled effect {effect}.{op}"));
        };

        // Make room while everything is still rooted: the argument and the
        // continuation are in registers, and the frames are still on the stack.
        let nframes = self.handlers.len() - idx;
        self.ensure(1 + 2 + 3 * nframes);

        let arg = self.reg(i.a);
        let k = self.reg(i.b);
        let frames = self.handlers.split_off(idx);
        let handler = frames[0].handler;
        let ret_k = frames[0].ret_k;
        let method = self.program.handled[frames[0].handled as usize]
            .iter()
            .position(|(e, o)| *e == effect && *o == op)
            .expect("matched above");

        // The resumption carries the performing continuation *and* the frames
        // unwound past — including the handler's own, which is what makes
        // handlers deep.
        let mut fields = Vec::with_capacity(2 + 3 * nframes);
        fields.push(Value::Bool(false));
        fields.push(k);
        for f in &frames {
            fields.push(Value::Int(f.handled as i64));
            fields.push(f.handler);
            fields.push(f.ret_k);
        }
        let resumption = Value::Obj(self.heap.alloc(Kind::Resume, 0, &fields));

        let Some(ha) = handler.addr().filter(|a| self.heap.kind(*a) == Kind::Closure) else {
            return err(format!("handler is {}, not an object", handler.kind()));
        };
        let table = self.heap.meta(ha) as usize;
        let Some(&pc) = self.program.methods.get(table).and_then(|t| t.get(method)) else {
            return err(format!("handler for {effect}.{op} has no clause #{method}"));
        };
        let ncap = self.heap.len(ha);
        if ncap + 3 > REGISTERS {
            return err("a handler clause needs more than 256 registers");
        }
        for j in 0..ncap {
            self.regs[j] = self.heap.field(ha, j);
        }
        self.regs[ncap] = arg;
        self.regs[ncap + 1] = resumption;
        self.regs[ncap + 2] = ret_k;
        self.live = ncap + 3;
        self.pc = pc as usize;
        Ok(())
    }

    // --- registers and the heap -------------------------------------------

    #[inline]
    pub(crate) fn reg(&self, r: Reg) -> Value {
        self.regs[r as usize]
    }

    #[inline]
    pub(crate) fn set(&mut self, r: Reg, v: Value) {
        self.regs[r as usize] = v;
        self.live = self.live.max(r as usize + 1);
    }

    /// Make room for `slots`, collecting and growing as needed.
    ///
    /// Everything reachable must be in a register below `live` or on the
    /// handler stack when this is called — see the module docs.
    pub(crate) fn ensure(&mut self, slots: usize) {
        if self.heap.room_for(slots) {
            return;
        }
        self.collect();
        self.heap.reserve(slots);
    }

    fn collect(&mut self) {
        let mut roots: Vec<Value> = Vec::with_capacity(self.live + self.handlers.len() * 2);
        roots.extend_from_slice(&self.regs[..self.live]);
        for f in &self.handlers {
            roots.push(f.handler);
            roots.push(f.ret_k);
        }

        self.heap.collect(&mut roots);

        let mut it = roots.into_iter();
        for j in 0..self.live {
            self.regs[j] = it.next().expect("root count");
        }
        for f in &mut self.handlers {
            f.handler = it.next().expect("root count");
            f.ret_k = it.next().expect("root count");
        }
    }

    fn primitive(&self, id: u32) -> Result<meadow_core::Prim, Error> {
        match self.program.prims.get(id as usize) {
            Some(p) => Ok(*p),
            None => err(format!("no primitive {id}")),
        }
    }

    fn constant(&mut self, id: u32) -> Result<Value, Error> {
        let Some(c) = self.program.consts.get(id as usize) else {
            return err(format!("no constant {id}"));
        };
        Ok(match *c {
            Const::Unit => Value::Unit,
            Const::Bool(b) => Value::Bool(b),
            Const::Int(n) => Value::Int(n),
            Const::Float(x) => Value::Float(x),
            Const::Str(s) => Value::Str(s),
            Const::Char(c) => Value::Char(c),
            Const::BigInt(n) => self.alloc_bigint(num_bigint::BigInt::from(n)),
        })
    }

    fn label(&self, id: u32) -> Result<InternedString, Error> {
        match self.program.labels.get(id as usize) {
            Some(l) => Ok(*l),
            None => err(format!("no field label {id}")),
        }
    }

    pub(crate) fn record_get(&self, a: Addr, label: InternedString) -> Option<Value> {
        for j in 0..self.heap.len(a) / 2 {
            if self.heap.field(a, 2 * j) == Value::Str(label) {
                return Some(self.heap.field(a, 2 * j + 1));
            }
        }
        None
    }

    /// What a conditional treats as false.
    ///
    /// The compiler lowers `True`/`False` patterns to boolean literals, but a
    /// value built by naming the constructor arrives as data — so both spellings
    /// are accepted, the same rule the CEK uses.
    pub(crate) fn falsey(&self, v: Value) -> bool {
        match v {
            Value::Bool(b) => !b,
            Value::Obj(a) => {
                self.heap.kind(a) == Kind::Data
                    && self.heap.len(a) == 0
                    && Some(self.heap.meta(a)) == self.false_tag
            }
            _ => false,
        }
    }

    // --- introspection ----------------------------------------------------

    /// The live registers, named by number — what a stepper shows beside the
    /// disassembly.
    pub fn describe(&self) -> String {
        let mut out = String::from("[");
        for j in 0..self.live {
            if j > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("r{j} = {}", self.show(self.regs[j])));
        }
        out.push(']');
        out
    }

    pub fn pc(&self) -> usize {
        self.pc
    }

    /// Collections so far and slots allocated in total.
    pub fn heap_stats(&self) -> (u64, u64) {
        (self.heap.collections, self.heap.allocated)
    }

    /// How many handlers are installed — the depth a `perform` may have to walk.
    pub fn handler_depth(&self) -> usize {
        self.handlers.len()
    }
}
