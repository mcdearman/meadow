//! **Native code, from bytecode.**
//!
//! A baseline compiler: each block of bytecode (see [`crate::abi`]) becomes one
//! native function, instruction by instruction. What a word-level instruction
//! does -- a move, a constant, typed arithmetic, a compare-and-branch -- is
//! done inline, on the register file in memory. Everything else is handed back
//! to the interpreter one instruction at a time, through
//! [`crate::abi::meadow_exec`], and native code carries on or returns according
//! to what became of control.
//!
//! So the machine code is a translation of the typed bytecode, not a second
//! compiler: every engine runs the same instructions, and the difference is
//! only which of them an engine does itself.
//!
//! Two architectures, [`Arch::Aarch64`] and [`Arch::X86_64`], behind one small
//! [`Emit`] interface, and the same translation for both. The code needs no
//! relocations: it reaches the machine through fixed offsets in [`crate::Vm`]
//! -- see [`layout`] -- branches only within a function, and calls the
//! interpreter through a pointer the machine holds. That is what lets one
//! translation serve an object file on disk ([`object`]) and memory made
//! executable in place alike.
//!
//! # What native code keeps up
//!
//! The machine state an instruction changes: the register it writes, `live`
//! when that raises it, the pc when control leaves, and `steps`, counted in a
//! machine register and added to the machine's before every call and return.
//!
//! # Optimization
//!
//! The same [`OptLevel`] as the rest of the compiler, and the same promise:
//! each level runs the same instructions to the same effect, and differs only
//! in what it costs.
//!
//! * [`OptLevel::O0`] is the translation done literally, instruction by
//!   instruction: what a miscompilation is bisected against.
//! * [`OptLevel::O1`] keeps the machine's books where they are *observed*
//!   rather than where they change -- see [`Book`] -- and picks the short
//!   encodings of instructions with a constant operand. A run of arithmetic
//!   between two calls into the interpreter costs one addition to the step
//!   counter and at most one write to `live`, where it cost one of each per
//!   instruction.
//! * [`OptLevel::O2`] adds two passes that trade code size for speed. A
//!   block's function takes in the code that loops back into it (see
//!   [`region`]), so a loop whose body the bytecode lays out as blocks of their
//!   own runs round natively instead of returning to the machine at every
//!   block. And the registers that function uses most live in machine
//!   registers (see [`pins`]), in memory only where something other than the
//!   function could look.

pub mod a64;
pub mod object;
pub mod x64;

use crate::value::Value;
use meadow_bytecode::{Cond, Const, Instr, Op, Pc, Program, Reg};
use meadow_core::OptLevel;
use std::collections::HashMap;

/// Where the fields of [`crate::Vm`] native code touches are.
pub mod layout {
    /// `*mut u64`: the register file.
    pub const REGS: u32 = 0;
    pub const LIVE: u32 = 8;
    pub const PC: u32 = 16;
    pub const AT: u32 = 24;
    pub const STEPS: u32 = 32;
    /// `extern "C" fn(vm, pc) -> status`.
    pub const EXEC: u32 = 40;
    /// `*const u32`: every method table's pcs, in a row.
    pub const METHOD_PCS: u32 = 48;
    /// `*const u32`: where each table starts among them, and the last ends.
    pub const METHOD_STARTS: u32 = 56;
    /// The heap's nursery, as `crate::heap::Heap` begins.
    pub const BASE: u32 = 64;
    pub const CAP: u32 = 72;
    pub const TOP: u32 = 80;
    pub const ALLOCATED: u32 = 88;
    pub const REGION_GROWTH: u32 = 96;
}

/// A target instruction set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Aarch64,
    X86_64,
}

impl Arch {
    /// The architecture this process is running on, if it is one of these.
    pub fn host() -> Option<Arch> {
        if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else {
            None
        }
    }
}

/// A program's native code: the machine code, and where in it the function for
/// each block starts.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub arch: Arch,
    pub code: Vec<u8>,
    /// `(entry pc, offset into code)`, by pc.
    pub blocks: Vec<(Pc, u32)>,
}

/// A position in the code being emitted, bound once and jumped to any number of
/// times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(pub usize);

/// An operand that is a register or a constant.
#[derive(Debug, Clone, Copy)]
pub enum Operand {
    Reg(Reg),
    Imm(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// What one architecture's code for the translation looks like. Every method
/// emits code at the end of what is emitted so far; "`r`" is a register of the
/// bytecode machine, which lives in memory -- or, for the registers a function
/// pins, in a machine register, which the architecture's code moves to memory
/// and back where anything else could look.
pub trait Emit {
    /// How many bytecode registers a function can keep in machine registers.
    const PINS: usize;

    fn new() -> Self
    where
        Self: Sized;
    /// Where the next byte goes.
    fn offset(&self) -> usize;
    fn label(&mut self) -> Label;
    fn bind(&mut self, l: Label);
    /// How the next function is emitted: at `opt`, keeping `pins` -- at most
    /// [`Emit::PINS`] of them -- in machine registers. Before
    /// [`Emit::prologue`].
    fn configure(&mut self, opt: OptLevel, pins: &[Reg]);
    /// A function's entry: save what it uses, and find the register file.
    fn prologue(&mut self);
    /// Return `status`, having counted the steps taken.
    fn ret(&mut self, status: u32);
    /// Leave for `pc`: set the machine's pc -- and, for a jump, how many
    /// registers are live there -- then return [`crate::abi::JUMPED`].
    fn leave(&mut self, pc: Pc, live: Option<u32>);
    /// One instruction retired.
    fn step(&mut self);
    /// One fewer: the instruction is handed to the interpreter after all, and
    /// the interpreter counts it.
    fn unstep(&mut self);
    /// `n` instructions retired at once.
    fn add_steps(&mut self, n: u32);
    fn jump(&mut self, to: Label);
    /// `r[a] = r[b]`
    fn mov(&mut self, a: Reg, b: Reg);
    /// `r[a] = w`
    fn word(&mut self, a: Reg, w: u64);
    /// `r[a] = r[b] op c`, as `Int`s. `Div` and `Rem` with a zero divisor jump
    /// to `zero` instead.
    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label);
    fn float(&mut self, op: FloatOp, a: Reg, b: Reg, c: Reg);
    /// `r[a] = cond(r[b], c)`, as words.
    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand);
    fn cmp_float(&mut self, cond: Cond, a: Reg, b: Reg, c: Reg);
    /// `if not cond(r[x], y) goto to`
    fn branch_int(&mut self, cond: Cond, x: Reg, y: Operand, to: Label);
    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label);
    /// `if r[x] == 0 goto to`
    fn branch_zero(&mut self, x: Reg, to: Label);
    /// `live = n`
    fn set_live(&mut self, n: u32);
    /// `live = max(live, n)`: what writing a register does to `live`, unless
    /// the function was configured above [`OptLevel::O0`], where [`Book`] does
    /// this instead.
    fn raise_live_to(&mut self, n: u32);
    /// A loop's jump back: to `to`, unless this call of the function has taken
    /// [`BACK_EDGES`] of them, in which case to `over`.
    fn back_edge(&mut self, to: Label, over: Label);

    // The heap, where the common case is simple enough to do here: an object
    // in the nursery, with a header of two words (see `crate::object`).
    // Anything else goes to `slow`, where the interpreter does it.

    /// Fall through if `r[x]` is data tagged `tag`; `miss` if it is some other
    /// nursery object.
    fn tag_test(&mut self, x: Reg, tag: u32, miss: Label, slow: Label);
    /// `r[a] = ` field `i` of the data or array in `r[b]`.
    fn field(&mut self, a: Reg, b: Reg, i: u32, slow: Label);
    /// Enter method `method` of the closure in `r[obj]`, with the `argc`
    /// arguments at `r[base]`, as `Op::Invoke` does -- and return, leaving
    /// for it.
    fn invoke(&mut self, obj: Reg, method: u8, base: Reg, argc: u32, slow: Label);
    /// `r[a] = ` a new object whose header is `header` and whose fields are the
    /// `n` registers from `base`, bumped into the nursery if it has room.
    fn alloc(&mut self, a: Reg, header: [u64; 2], base: Reg, n: u32, slow: Label);
    /// Have the interpreter carry out the instruction at `pc`, and return what
    /// it says unless that is [`crate::abi::CONTINUE`].
    fn exec(&mut self, pc: Pc);
    /// The function is done: emit what its code shares.
    fn end(&mut self);
    /// The machine code, with every branch resolved.
    fn finish(self) -> Vec<u8>;
}

/// How many times a loop inside one native function goes round before the
/// function returns anyway: the scheduler gets its turn between calls, not
/// inside one.
pub const BACK_EDGES: u32 = 1 << 12;

/// Compile every block of `program` for `arch`, at `opt`.
pub fn compile(program: &Program, arch: Arch, opt: OptLevel) -> Compiled {
    match arch {
        Arch::Aarch64 => compile_with::<a64::Asm>(program, arch, opt),
        Arch::X86_64 => compile_with::<x64::Asm>(program, arch, opt),
    }
}

/// Compile the one block starting at `entry`, at `opt`: a function on its own,
/// for placing anywhere.
pub fn compile_block(program: &Program, arch: Arch, entry: Pc, opt: OptLevel) -> Vec<u8> {
    fn with<E: Emit>(program: &Program, entry: Pc, opt: OptLevel) -> Vec<u8> {
        let mut asm = E::new();
        block(&mut asm, program, entry, opt);
        asm.finish()
    }
    match arch {
        Arch::Aarch64 => with::<a64::Asm>(program, entry, opt),
        Arch::X86_64 => with::<x64::Asm>(program, entry, opt),
    }
}

fn compile_with<E: Emit>(program: &Program, arch: Arch, opt: OptLevel) -> Compiled {
    let entries = crate::abi::block_entries(program);
    let mut asm = E::new();
    let mut blocks = Vec::with_capacity(entries.len());
    for &entry in &entries {
        blocks.push((entry, asm.offset() as u32));
        block(&mut asm, program, entry, opt);
    }
    Compiled {
        arch,
        code: asm.finish(),
        blocks,
    }
}

/// Does control never fall through from `i` to the instruction after it?
fn ends(i: &Instr) -> bool {
    matches!(i.op, Op::Jump | Op::Invoke | Op::Halt | Op::Error)
}

/// Where native code can go from the instruction at `pc`, without returning:
/// the next instruction, and a branch's target -- not the targets of the
/// instructions the interpreter does, which return when they branch.
fn successors(code: &[Instr], pc: usize) -> impl Iterator<Item = usize> {
    let i = code[pc];
    let (next, target) = match i.op {
        Op::Jump => (None, Some(i.imm as usize)),
        Op::JumpUnless | Op::JumpUnlessTag | Op::BrI | Op::BrIK | Op::BrF => {
            (Some(pc + 1), Some(i.imm as usize))
        }
        Op::Invoke | Op::Halt | Op::Error => (None, None),
        _ => (Some(pc + 1), None),
    };
    let len = code.len();
    next.into_iter().chain(target).filter(move |&to| to < len)
}

/// How many instructions past its own block [`region`] looks for a loop in.
const REGION_BUDGET: usize = 512;

/// The instructions `entry`'s function holds, in the order it holds them:
/// the block, from `entry` up to and including the instruction that leaves --
/// through the entries of other blocks on the way, which are only where control
/// may also arrive from elsewhere.
///
/// At [`OptLevel::O2`], also every instruction within [`REGION_BUDGET`] that
/// control can go to from the block and come back into it from: the rest of
/// the loops the block is in, after it in pc order. What does not come back
/// stays out, and is left for, as from any block -- so what is repeated is
/// loops, not everything a function could reach.
fn region(code: &[Instr], entry: usize, opt: OptLevel) -> Vec<usize> {
    let mut end = entry;
    loop {
        let i = &code[end];
        end += 1;
        if ends(i) || end >= code.len() {
            break;
        }
    }
    let mut order: Vec<usize> = (entry..end).collect();
    if opt < OptLevel::O2 {
        return order;
    }

    // Forward: everything reachable, as far as the budget, and the edges into
    // each instruction found.
    let block = entry..end;
    let mut preds: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut work: Vec<usize> = order.clone();
    let mut seen: std::collections::HashSet<usize> = order.iter().copied().collect();
    while let Some(pc) = work.pop() {
        for to in successors(code, pc) {
            preds.entry(to).or_default().push(pc);
            if seen.len() - order.len() < REGION_BUDGET && seen.insert(to) {
                work.push(to);
            }
        }
    }
    // Backward from the block: what comes back into it.
    let mut back: std::collections::HashSet<usize> = order.iter().copied().collect();
    let mut work = order.clone();
    while let Some(pc) = work.pop() {
        for &from in preds.get(&pc).map_or(&[][..], Vec::as_slice) {
            if seen.contains(&from) && back.insert(from) {
                work.push(from);
            }
        }
    }
    let mut rest: Vec<usize> = back.into_iter().filter(|pc| !block.contains(pc)).collect();
    rest.sort_unstable();
    order.extend(rest);
    order
}

/// The registers `entry`'s function keeps in machine registers: at most `max`,
/// at [`OptLevel::O2`], chosen by how much the function does with them against
/// what keeping them costs.
///
/// A pinned register is loaded on entry, and moved to memory and back around
/// every call into the interpreter and before every return -- so each is
/// worth it only if the function reads and writes it more often than it calls
/// and returns. Both are counted where they are, eight times over inside a
/// loop.
fn pins(code: &[Instr], program: &Program, f: &Function, max: usize) -> Vec<Reg> {
    if max == 0 {
        return Vec::new();
    }
    // Which instructions are inside a loop of the function: from a backward
    // edge's target to its source, in the function's order.
    let mut looped = vec![false; f.order.len()];
    for (k, &pc) in f.order.iter().enumerate() {
        let mut edge = |to: usize| {
            if let Some(&t) = f.index.get(&to)
                && t <= k
            {
                looped[t..=k].iter_mut().for_each(|l| *l = true);
            }
        };
        successors(code, pc).for_each(&mut edge);
    }

    let mut uses = [0u64; 256];
    let mut cost = 1u64;
    for (k, &pc) in f.order.iter().enumerate() {
        let w = if looped[k] { 8 } else { 1 };
        let i = code[pc];
        let used: Vec<u32> = match i.op {
            Op::Nop => vec![],
            Op::Const if immediate(program, i.imm).is_some() => vec![i.a as u32],
            Op::AddI | Op::SubI | Op::MulI | Op::DivI | Op::ModI | Op::CmpI => {
                vec![i.a as u32, i.b as u32, i.c as u32]
            }
            Op::AddF | Op::SubF | Op::MulF | Op::DivF | Op::CmpF => {
                vec![i.a as u32, i.b as u32, i.c as u32]
            }
            Op::Move
            | Op::Field
            | Op::AddIK
            | Op::SubIK
            | Op::MulIK
            | Op::CmpIK
            | Op::BrI
            | Op::BrF => vec![i.a as u32, i.b as u32],
            Op::BrIK | Op::JumpUnless | Op::JumpUnlessTag => vec![i.a as u32],
            Op::Jump => {
                if !f.index.contains_key(&(i.imm as usize)) {
                    cost += w;
                }
                vec![]
            }
            // A return, after reading the arguments from memory.
            Op::Invoke => {
                cost += w;
                vec![i.a as u32]
            }
            Op::MakeData | Op::MakeArray | Op::Closure
                if header(program, pc, alloc_kind(i.op), 0, i.c as usize).is_some() =>
            {
                std::iter::once(i.a as u32)
                    .chain((0..i.c as u32).map(|j| i.b as u32 + j))
                    .collect()
            }
            // Handed to the interpreter: moved out and back in.
            _ => {
                cost += 2 * w;
                vec![]
            }
        };
        for r in used {
            if let Some(u) = uses.get_mut(r as usize) {
                *u += w;
            }
        }
    }
    let mut chosen: Vec<(u64, Reg)> = uses
        .iter()
        .enumerate()
        .filter(|&(_, &u)| u > cost)
        .map(|(r, &u)| (u, r as Reg))
        .collect();
    chosen.sort_unstable_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
    chosen.into_iter().take(max).map(|(_, r)| r).collect()
}

fn alloc_kind(op: Op) -> crate::heap::Kind {
    match op {
        Op::MakeData => crate::heap::Kind::Data,
        Op::MakeArray => crate::heap::Kind::Array,
        _ => crate::heap::Kind::Closure,
    }
}

/// The machine bookkeeping a function owes but has not written yet, from
/// [`OptLevel::O1`].
///
/// Instructions retired and the register high-water mark accumulate here while
/// the code between two *observation points* is emitted, and are settled --
/// one [`Emit::add_steps`], at most one [`Emit::raise_live_to`] -- at the next
/// one. What can observe them:
///
/// * a call into the interpreter, which reads `live` to find the collector's
///   roots, and whose steps are counted after the ones before it;
/// * a return, which hands both to the machine;
/// * a branch, since the code at the other end runs as if both were current;
/// * an instruction control reaches from more than one place, for the same
///   reason from the other side.
///
/// Everything between -- moves, constants, typed arithmetic, comparisons --
/// changes neither what the machine can see nor where it can go, so owing the
/// bookkeeping across it is invisible.
///
/// `known` is what `live` is at least, statically, which lets a raise the
/// machine has already done be skipped: after a jump's `live = n`, writing a
/// register below `n` needs nothing. It is forgotten where control joins,
/// since the other path may have known less.
struct Book {
    on: bool,
    steps: u32,
    live: u32,
    known: u32,
}

impl Book {
    fn new(opt: OptLevel) -> Book {
        Book {
            on: opt >= OptLevel::O1,
            steps: 0,
            live: 0,
            known: 0,
        }
    }

    /// One instruction retired.
    fn step<E: Emit>(&mut self, asm: &mut E) {
        if self.on {
            self.steps += 1;
        } else {
            asm.step();
        }
    }

    /// Register `a` was written.
    fn wrote(&mut self, a: Reg) {
        self.live = self.live.max(a as u32 + 1);
    }

    /// Settle everything owed: before anything that can observe it.
    fn sync<E: Emit>(&mut self, asm: &mut E) {
        if !self.on {
            return;
        }
        if self.steps > 0 {
            asm.add_steps(self.steps);
            self.steps = 0;
        }
        if self.live > self.known {
            asm.raise_live_to(self.live);
            self.known = self.live;
        }
        self.live = 0;
    }

    /// Settle the steps, and forget the writes: before something that sets
    /// `live` outright, which makes raising it first pointless.
    fn sync_steps<E: Emit>(&mut self, asm: &mut E) {
        if self.on && self.steps > 0 {
            asm.add_steps(self.steps);
        }
        self.steps = 0;
        self.live = 0;
    }

    /// `live` was just set to `n`.
    fn set(&mut self, n: u32) {
        self.known = n;
    }

    /// Where another path joins: settle, and assume nothing about `live`.
    fn join<E: Emit>(&mut self, asm: &mut E) {
        self.sync(asm);
        self.known = 0;
    }
}

/// One function being compiled: its instructions in order, and where each is.
struct Function {
    order: Vec<usize>,
    /// Position in `order`, by pc.
    index: HashMap<usize, usize>,
    labels: Vec<Label>,
    /// Where control leaves for a pc, having set it: `(label, pc)`.
    exits: Vec<(Label, Pc)>,
    /// A branch's way to a pc it cannot jump straight to: `(label, from, pc)`,
    /// `from` being the branch's position.
    detours: Vec<(Label, usize, usize)>,
}

impl Function {
    /// A label that leaves for `pc`.
    fn exit<E: Emit>(&mut self, asm: &mut E, pc: usize) -> Label {
        let l = asm.label();
        self.exits.push((l, pc as Pc));
        l
    }

    /// Go from the instruction at position `from` to the one at `pc`: a jump,
    /// if the function holds it further on; a counted jump back, if it holds
    /// it earlier -- so that every loop in the function goes through one --
    /// and otherwise, leave for it.
    fn goto<E: Emit>(&mut self, asm: &mut E, from: usize, pc: usize) {
        match self.index.get(&pc) {
            Some(&t) if t > from => asm.jump(self.labels[t]),
            Some(&t) => {
                let over = self.exit(asm, pc);
                asm.back_edge(self.labels[t], over);
            }
            None => asm.leave(pc as Pc, None),
        }
    }

    /// Where a branch at position `from` to `pc` goes: straight to the
    /// instruction further on, or to a detour that does what [`Function::goto`]
    /// does.
    fn target<E: Emit>(&mut self, asm: &mut E, from: usize, pc: usize) -> Label {
        match self.index.get(&pc) {
            Some(&t) if t > from => self.labels[t],
            _ => {
                let l = asm.label();
                self.detours.push((l, from, pc));
                l
            }
        }
    }

    /// The positions control reaches from somewhere other than the position
    /// before: a branch's target, where a fast path's slow half comes back,
    /// and the first of a run that does not follow on from the one before.
    fn joins(&self, code: &[Instr]) -> Vec<bool> {
        let mut out = vec![false; self.order.len()];
        for (k, &pc) in self.order.iter().enumerate() {
            let i = code[pc];
            let mut mark = |to: usize| {
                if let Some(&t) = self.index.get(&to) {
                    out[t] = true;
                }
            };
            match i.op {
                Op::Jump | Op::JumpUnless | Op::BrI | Op::BrIK | Op::BrF => mark(i.imm as usize),
                Op::JumpUnlessTag => {
                    mark(i.imm as usize);
                    mark(pc + 1);
                }
                Op::Field | Op::MakeData | Op::MakeArray | Op::Closure => mark(pc + 1),
                _ => {}
            }
            if !ends(&i) && self.order.get(k + 1) != Some(&(pc + 1)) {
                mark(pc + 1);
            }
            if k > 0 {
                let before = self.order[k - 1];
                if ends(&code[before]) || before + 1 != pc {
                    out[k] = true;
                }
            }
        }
        out
    }
}

/// One function: the instructions [`region`] gives `entry`, at `opt`.
fn block<E: Emit>(asm: &mut E, program: &Program, entry: Pc, opt: OptLevel) {
    let code = &program.code;
    let order = region(code, entry as usize, opt);
    let index = order.iter().enumerate().map(|(k, &pc)| (pc, k)).collect();
    let labels = order.iter().map(|_| asm.label()).collect();
    let mut f = Function {
        order,
        index,
        labels,
        exits: Vec::new(),
        detours: Vec::new(),
    };
    let joins = f.joins(code);
    let pinned = if opt >= OptLevel::O2 {
        pins(code, program, &f, E::PINS)
    } else {
        Vec::new()
    };
    // Fast paths' slow halves: where one starts, and the instruction's
    // position -- placed after the function's code, out of the way.
    let mut slows: Vec<(Label, usize)> = Vec::new();

    asm.configure(opt, &pinned);
    asm.prologue();
    let mut book = Book::new(opt);
    for k in 0..f.order.len() {
        let pc = f.order[k];
        if joins[k] {
            book.join(asm);
        }
        asm.bind(f.labels[k]);
        let i = code[pc];
        let pc32 = pc as Pc;
        match i.op {
            Op::Nop => book.step(asm),
            // A register moved to itself is already where it is going.
            Op::Move if book.on && i.a == i.b => book.step(asm),
            Op::Move => {
                book.step(asm);
                asm.mov(i.a, i.b);
                book.wrote(i.a);
            }
            Op::Const => match immediate(program, i.imm) {
                Some(w) => {
                    book.step(asm);
                    asm.word(i.a, w);
                    book.wrote(i.a);
                }
                None => {
                    book.sync(asm);
                    asm.exec(pc32);
                }
            },
            Op::Jump => {
                book.step(asm);
                book.sync_steps(asm);
                let to = i.imm as usize;
                if f.index.contains_key(&to) {
                    asm.set_live(i.a as u32);
                    book.set(i.a as u32);
                    f.goto(asm, k, to);
                } else {
                    asm.leave(i.imm, Some(i.a as u32));
                }
            }
            Op::JumpUnless => {
                book.step(asm);
                book.sync(asm);
                let to = f.target(asm, k, i.imm as usize);
                asm.branch_zero(i.a, to);
            }
            Op::AddI | Op::SubI | Op::MulI | Op::DivI | Op::ModI => {
                let op = match i.op {
                    Op::AddI => IntOp::Add,
                    Op::SubI => IntOp::Sub,
                    Op::MulI => IntOp::Mul,
                    Op::DivI => IntOp::Div,
                    _ => IntOp::Rem,
                };
                typed_int(asm, &mut book, op, i, Operand::Reg(i.c), pc32);
            }
            Op::AddIK | Op::SubIK | Op::MulIK => {
                let op = match i.op {
                    Op::AddIK => IntOp::Add,
                    Op::SubIK => IntOp::Sub,
                    _ => IntOp::Mul,
                };
                let c = Operand::Imm(i.imm as i32 as i64);
                typed_int(asm, &mut book, op, i, c, pc32);
            }
            Op::AddF | Op::SubF | Op::MulF | Op::DivF => {
                let op = match i.op {
                    Op::AddF => FloatOp::Add,
                    Op::SubF => FloatOp::Sub,
                    Op::MulF => FloatOp::Mul,
                    _ => FloatOp::Div,
                };
                book.step(asm);
                asm.float(op, i.a, i.b, i.c);
                book.wrote(i.a);
            }
            Op::CmpI | Op::CmpIK | Op::CmpF | Op::BrI | Op::BrIK | Op::BrF => {
                let cond_byte = match i.op {
                    Op::CmpI | Op::CmpF => i.imm,
                    _ => i.c as u32,
                };
                let Some(cond) = Cond::from_byte(cond_byte) else {
                    // A malformed instruction: the interpreter says so.
                    book.sync(asm);
                    asm.exec(pc32);
                    fall_through(asm, &mut book, &mut f, code, k);
                    continue;
                };
                book.step(asm);
                match i.op {
                    Op::CmpI => asm.cmp_int(cond, i.a, i.b, Operand::Reg(i.c)),
                    Op::CmpIK => asm.cmp_int(cond, i.a, i.b, Operand::Imm(i.imm as i32 as i64)),
                    Op::CmpF => asm.cmp_float(cond, i.a, i.b, i.c),
                    Op::BrI => {
                        book.sync(asm);
                        let to = f.target(asm, k, i.imm as usize);
                        asm.branch_int(cond, i.a, Operand::Reg(i.b), to);
                    }
                    Op::BrIK => {
                        book.sync(asm);
                        let to = f.target(asm, k, i.imm as usize);
                        asm.branch_int(cond, i.a, Operand::Imm(i.b as i8 as i64), to);
                    }
                    _ => {
                        book.sync(asm);
                        let to = f.target(asm, k, i.imm as usize);
                        asm.branch_float(cond, i.a, i.b, to);
                    }
                }
                if matches!(i.op, Op::CmpI | Op::CmpIK | Op::CmpF) {
                    book.wrote(i.a);
                }
            }
            Op::JumpUnlessTag => {
                let slow = asm.label();
                book.step(asm);
                book.sync(asm);
                let miss = f.target(asm, k, i.imm as usize);
                asm.tag_test(i.a, i.bc() as u32, miss, slow);
                slows.push((slow, k));
            }
            Op::Field => {
                let slow = asm.label();
                book.step(asm);
                book.sync(asm);
                asm.field(i.a, i.b, i.imm, slow);
                book.wrote(i.a);
                slows.push((slow, k));
            }
            Op::Invoke => {
                let slow = asm.label();
                book.step(asm);
                book.sync(asm);
                asm.invoke(i.a, i.b, i.c, i.imm, slow);
                slows.push((slow, k));
            }
            Op::MakeData | Op::MakeArray | Op::Closure => {
                let kind = alloc_kind(i.op);
                let meta = if kind == crate::heap::Kind::Array {
                    0
                } else {
                    i.imm
                };
                match header(program, pc, kind, meta, i.c as usize) {
                    Some(h) => {
                        let slow = asm.label();
                        book.step(asm);
                        book.sync(asm);
                        asm.alloc(i.a, h, i.b, i.c as u32, slow);
                        book.wrote(i.a);
                        slows.push((slow, k));
                    }
                    None => {
                        book.sync(asm);
                        asm.exec(pc32);
                    }
                }
            }
            // Everything else that touches the heap, the tables or the
            // scheduler.
            _ => {
                book.sync(asm);
                asm.exec(pc32);
            }
        }
        fall_through(asm, &mut book, &mut f, code, k);
    }
    for (l, k) in slows {
        let pc = f.order[k];
        asm.bind(l);
        asm.unstep();
        asm.exec(pc as Pc);
        if ends(&code[pc]) {
            // Handed to the interpreter, which never says to carry on after one
            // of these; should it, the machine is where it said.
            asm.ret(crate::abi::JUMPED);
        } else {
            f.goto(asm, k, pc + 1);
        }
    }
    let mut d = 0;
    while d < f.detours.len() {
        let (l, from, pc) = f.detours[d];
        asm.bind(l);
        f.goto(asm, from, pc);
        d += 1;
    }
    for (l, pc) in std::mem::take(&mut f.exits) {
        asm.bind(l);
        asm.leave(pc, None);
    }
    asm.end();
}

/// After the instruction at position `k`: on to the next one, if it is not the
/// one emitted next -- or return, after an instruction that never falls
/// through.
fn fall_through<E: Emit>(asm: &mut E, book: &mut Book, f: &mut Function, code: &[Instr], k: usize) {
    let pc = f.order[k];
    let i = code[pc];
    if ends(&i) {
        // `Jump` has gone already, and so has a native `Invoke`; the
        // interpreter's, like `Halt` and `Error`, never says to carry on.
        if i.op != Op::Jump {
            asm.ret(crate::abi::JUMPED);
        }
    } else if f.order.get(k + 1) != Some(&(pc + 1)) {
        book.sync(asm);
        // Off the end of the program, this leaves without anywhere to go.
        f.goto(asm, k, pc + 1);
    }
}

/// The two header words of the object the instruction at `pc` builds, if they
/// can be known when it is compiled: every field's descriptor is, and they fit the second word
/// -- or, for an array, are all the same.
fn header(
    program: &Program,
    pc: usize,
    kind: crate::heap::Kind,
    meta: u32,
    n: usize,
) -> Option<[u64; 2]> {
    use meadow_bytecode::DESC_REG;
    let operands = program.operands(pc);
    let descs: Vec<meadow_core::desc::Desc> = (0..n)
        .map(|k| {
            operands
                .get(k)
                .copied()
                .filter(|d| *d < DESC_REG)
                .map(|d| d as meadow_core::desc::Desc)
        })
        .collect::<Option<_>>()?;
    if kind.is_uniform() {
        if descs.windows(2).any(|w| w[0] != w[1]) {
            return None;
        }
    } else if n > meadow_core::compact::INLINE_DESCS {
        return None;
    }
    let mut words = [0u64; 2];
    crate::object::write_header(kind, meta, descs.into_iter(), |k, w| words[k] = w);
    Some(words)
}

/// A typed `Int` instruction, whose division by zero the interpreter reports.
fn typed_int<E: Emit>(asm: &mut E, book: &mut Book, op: IntOp, i: Instr, c: Operand, pc: Pc) {
    let zero = asm.label();
    let done = asm.label();
    book.step(asm);
    let divides = matches!(op, IntOp::Div | IntOp::Rem);
    // Dividing can go to the interpreter, which has to see the machine as it
    // is.
    if divides {
        book.sync(asm);
    }
    asm.int(op, i.a, i.b, c, zero);
    book.wrote(i.a);
    if divides {
        asm.jump(done);
        asm.bind(zero);
        asm.unstep();
        asm.exec(pc);
        asm.bind(done);
    } else {
        asm.bind(zero);
        asm.bind(done);
    }
}

/// A constant as the word a register holds, if it is the same word in every
/// process: not an interned string, and not a `BigInt`, which is made.
fn immediate(program: &Program, k: u32) -> Option<u64> {
    let v = match program.consts.get(k as usize)? {
        Const::Unit => Value::Unit,
        Const::Bool(b) => Value::Bool(*b),
        Const::Int(n) => Value::Int(*n),
        Const::Float(x) => Value::Float(*x),
        Const::Word(w, b) => Value::Word(*w, *b),
        Const::Float32(x) => Value::Float32(*x),
        Const::Char(c) => Value::Char(*c),
        Const::Str(_) | Const::BigInt(_) => return None,
    };
    Some(v.bits())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_layout_is_where_the_machine_is() {
        use super::layout::*;
        use std::mem::offset_of;
        type Vm = crate::Vm<'static>;
        assert_eq!(offset_of!(Vm, regs) as u32, REGS);
        assert_eq!(offset_of!(Vm, live) as u32, LIVE);
        assert_eq!(offset_of!(Vm, pc) as u32, PC);
        assert_eq!(offset_of!(Vm, at) as u32, AT);
        assert_eq!(offset_of!(Vm, steps) as u32, STEPS);
        assert_eq!(offset_of!(Vm, exec) as u32, EXEC);
        assert_eq!(offset_of!(Vm, method_pcs) as u32, METHOD_PCS);
        assert_eq!(offset_of!(Vm, method_starts) as u32, METHOD_STARTS);
        let heap = offset_of!(Vm, heap) as u32;
        type Heap = crate::heap::Heap;
        assert_eq!(heap + offset_of!(Heap, base) as u32, BASE);
        assert_eq!(heap + offset_of!(Heap, cap) as u32, CAP);
        assert_eq!(heap + offset_of!(Heap, top) as u32, TOP);
        assert_eq!(heap + offset_of!(Heap, allocated) as u32, ALLOCATED);
        assert_eq!(heap + offset_of!(Heap, region_growth) as u32, REGION_GROWTH);
    }
}
