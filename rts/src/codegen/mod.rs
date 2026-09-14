//! **Native code, from bytecode.**
//!
//! A baseline compiler: each block of bytecode (see [`crate::abi`]) becomes one
//! native function, instruction by instruction, with nothing cleverer than the
//! instructions themselves. What a word-level instruction does -- a move, a
//! constant, typed arithmetic, a compare-and-branch -- is done inline, on the
//! register file in memory. Everything else is handed back to the interpreter
//! one instruction at a time, through [`crate::abi::meadow_exec`], and native
//! code carries on or returns according to what became of control.
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

pub mod a64;
pub mod object;
pub mod x64;

use crate::value::Value;
use meadow_bytecode::{Cond, Const, Instr, Op, Pc, Program, Reg};

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
/// bytecode machine, which lives in memory.
pub trait Emit {
    fn new() -> Self
    where
        Self: Sized;
    /// Where the next byte goes.
    fn offset(&self) -> usize;
    fn label(&mut self) -> Label;
    fn bind(&mut self, l: Label);
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

/// Compile every block of `program` for `arch`.
pub fn compile(program: &Program, arch: Arch) -> Compiled {
    match arch {
        Arch::Aarch64 => compile_with::<a64::Asm>(program, arch),
        Arch::X86_64 => compile_with::<x64::Asm>(program, arch),
    }
}

/// Compile the one block starting at `entry`: a function on its own, for
/// placing anywhere.
pub fn compile_block(program: &Program, arch: Arch, entry: Pc) -> Vec<u8> {
    fn with<E: Emit>(program: &Program, entry: Pc) -> Vec<u8> {
        let mut asm = E::new();
        block(&mut asm, program, entry);
        asm.finish()
    }
    match arch {
        Arch::Aarch64 => with::<a64::Asm>(program, entry),
        Arch::X86_64 => with::<x64::Asm>(program, entry),
    }
}

fn compile_with<E: Emit>(program: &Program, arch: Arch) -> Compiled {
    let entries = crate::abi::block_entries(program);
    let mut asm = E::new();
    let mut blocks = Vec::with_capacity(entries.len());
    for &entry in &entries {
        blocks.push((entry, asm.offset() as u32));
        block(&mut asm, program, entry);
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

/// One block: from `entry` up to and including the instruction that leaves.
///
/// Through the entries of other blocks on the way, which are only where control
/// may also arrive from elsewhere: a branch backwards, or to a later block's
/// code, returns, and the machine enters that block's own function.
fn block<E: Emit>(asm: &mut E, program: &Program, entry: Pc) {
    let code = &program.code;
    let mut end = entry as usize;
    loop {
        let i = &code[end];
        end += 1;
        if ends(i) || end >= code.len() {
            break;
        }
    }
    let range = entry as usize..end;
    let labels: Vec<Label> = range.clone().map(|_| asm.label()).collect();
    // A branch target inside the block is a jump; outside it, a return.
    let mut exits: Vec<(Label, Pc)> = Vec::new();
    // Fast paths' slow halves: where one starts, the instruction, and where to
    // carry on after it -- placed after the function's code, out of the way.
    let mut slows: Vec<(Label, Pc, Option<Label>)> = Vec::new();
    let next = |pc: usize| (pc + 1 < end).then(|| labels[pc + 1 - entry as usize]);
    let target = |asm: &mut E, pc: Pc, exits: &mut Vec<(Label, Pc)>| -> Label {
        let p = pc as usize;
        if range.contains(&p) && p > entry as usize {
            labels[p - entry as usize]
        } else {
            let l = asm.label();
            exits.push((l, pc));
            l
        }
    };

    asm.prologue();
    for pc in range.clone() {
        asm.bind(labels[pc - entry as usize]);
        let i = code[pc];
        let pc32 = pc as Pc;
        match i.op {
            Op::Nop => asm.step(),
            Op::Move => {
                asm.step();
                asm.mov(i.a, i.b);
            }
            Op::Const => match immediate(program, i.imm) {
                Some(w) => {
                    asm.step();
                    asm.word(i.a, w);
                }
                None => asm.exec(pc32),
            },
            // A jump back into this function is a loop, and stays native.
            Op::Jump if range.contains(&(i.imm as usize)) => {
                asm.step();
                asm.set_live(i.a as u32);
                let to = labels[i.imm as usize - entry as usize];
                let over = asm.label();
                exits.push((over, i.imm));
                asm.back_edge(to, over);
            }
            Op::Jump => {
                asm.step();
                asm.leave(i.imm, Some(i.a as u32));
            }
            Op::JumpUnless => {
                asm.step();
                let to = target(asm, i.imm, &mut exits);
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
                typed_int(asm, op, i, Operand::Reg(i.c), pc32);
            }
            Op::AddIK | Op::SubIK | Op::MulIK => {
                let op = match i.op {
                    Op::AddIK => IntOp::Add,
                    Op::SubIK => IntOp::Sub,
                    _ => IntOp::Mul,
                };
                typed_int(asm, op, i, Operand::Imm(i.imm as i32 as i64), pc32);
            }
            Op::AddF | Op::SubF | Op::MulF | Op::DivF => {
                let op = match i.op {
                    Op::AddF => FloatOp::Add,
                    Op::SubF => FloatOp::Sub,
                    Op::MulF => FloatOp::Mul,
                    _ => FloatOp::Div,
                };
                asm.step();
                asm.float(op, i.a, i.b, i.c);
            }
            Op::CmpI | Op::CmpIK | Op::CmpF | Op::BrI | Op::BrIK | Op::BrF => {
                let cond_byte = match i.op {
                    Op::CmpI | Op::CmpF => i.imm,
                    _ => i.c as u32,
                };
                let Some(cond) = Cond::from_byte(cond_byte) else {
                    // A malformed instruction: the interpreter says so.
                    asm.exec(pc32);
                    continue;
                };
                asm.step();
                match i.op {
                    Op::CmpI => asm.cmp_int(cond, i.a, i.b, Operand::Reg(i.c)),
                    Op::CmpIK => asm.cmp_int(cond, i.a, i.b, Operand::Imm(i.imm as i32 as i64)),
                    Op::CmpF => asm.cmp_float(cond, i.a, i.b, i.c),
                    Op::BrI => {
                        let to = target(asm, i.imm, &mut exits);
                        asm.branch_int(cond, i.a, Operand::Reg(i.b), to);
                    }
                    Op::BrIK => {
                        let to = target(asm, i.imm, &mut exits);
                        asm.branch_int(cond, i.a, Operand::Imm(i.b as i8 as i64), to);
                    }
                    _ => {
                        let to = target(asm, i.imm, &mut exits);
                        asm.branch_float(cond, i.a, i.b, to);
                    }
                }
            }
            Op::JumpUnlessTag => {
                let slow = asm.label();
                let miss = target(asm, i.imm, &mut exits);
                asm.step();
                asm.tag_test(i.a, i.bc() as u32, miss, slow);
                slows.push((slow, pc32, next(pc)));
            }
            Op::Field => {
                let slow = asm.label();
                asm.step();
                asm.field(i.a, i.b, i.imm, slow);
                slows.push((slow, pc32, next(pc)));
            }
            Op::Invoke => {
                let slow = asm.label();
                asm.step();
                asm.invoke(i.a, i.b, i.c, i.imm, slow);
                slows.push((slow, pc32, None));
            }
            Op::MakeData | Op::MakeArray | Op::Closure => {
                let kind = match i.op {
                    Op::MakeData => crate::heap::Kind::Data,
                    Op::MakeArray => crate::heap::Kind::Array,
                    _ => crate::heap::Kind::Closure,
                };
                let meta = if kind == crate::heap::Kind::Array {
                    0
                } else {
                    i.imm
                };
                match header(program, pc, kind, meta, i.c as usize) {
                    Some(h) => {
                        let slow = asm.label();
                        asm.step();
                        asm.alloc(i.a, h, i.b, i.c as u32, slow);
                        slows.push((slow, pc32, next(pc)));
                    }
                    None => asm.exec(pc32),
                }
            }
            // Everything else that touches the heap, the tables or the
            // scheduler.
            _ => asm.exec(pc32),
        }
    }
    let last = &code[end - 1];
    match last.op {
        Op::Jump => {}
        // Handed to the interpreter, which never says to carry on after one of
        // these; should it, the machine is where it said.
        Op::Invoke | Op::Halt | Op::Error => asm.ret(crate::abi::JUMPED),
        // Off the end of the program without leaving.
        _ => asm.leave(end as Pc, None),
    }
    for (l, pc, resume) in slows {
        asm.bind(l);
        asm.unstep();
        asm.exec(pc);
        match resume {
            Some(to) => asm.jump(to),
            // The block's last instruction, which leaves whatever happens.
            None if pc as usize + 1 >= end => match code[pc as usize].op {
                Op::Invoke | Op::Halt | Op::Error | Op::Jump => asm.ret(crate::abi::JUMPED),
                _ => asm.leave(end as Pc, None),
            },
            None => asm.ret(crate::abi::JUMPED),
        }
    }
    for (l, pc) in exits {
        asm.bind(l);
        asm.leave(pc, None);
    }
    asm.end();
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
fn typed_int<E: Emit>(asm: &mut E, op: IntOp, i: Instr, c: Operand, pc: Pc) {
    let zero = asm.label();
    let done = asm.label();
    asm.step();
    asm.int(op, i.a, i.b, c, zero);
    if matches!(op, IntOp::Div | IntOp::Rem) {
        asm.jump(done);
        asm.bind(zero);
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
