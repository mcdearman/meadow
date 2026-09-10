//! AxCut → bytecode.
//!
//! The back end. Everything the sequent IR says about control — continuations,
//! handlers, the duality between calling and returning — is compiled away here,
//! and what comes out is a flat instruction array over a flat register file.
//! `meadow_rts` never learns any of it.
//!
//! # Why this pass is short
//!
//! Because AxCut already did the hard part. Its environment is an **ordered
//! list**, and a block's parameters name the whole of it, so at every program
//! point the set of live values and their order is written down in the IR. That
//! is the information a register allocator normally has to recover by computing
//! liveness, and here it is simply read off.
//!
//! What is left is bookkeeping of three kinds:
//!
//! * **Renaming is free.** `substitute [x, y] in {(a, b) => s}` emits *no code*
//!   at all when the block is laid out inline — it is a change of names, and the
//!   values are already in registers. Same for a `switch` arm and for the
//!   continuation of a primitive. Moves appear only at [`Statement::Jump`] and
//!   [`Statement::Invoke`], the two places control actually leaves.
//! * **A jump needs a permutation.** The target block wants its parameters in
//!   `r0..rn`, so [`Gen::parallel_move`] emits the moves — breaking cycles with
//!   one scratch register, which is the only place this pass has to think.
//! * **Blocks get laid out and patched.** A `new` puts its methods somewhere
//!   reachable and records their addresses in a method table.
//!
//! # Halting is an ordinary invoke
//!
//! Instruction 0 is `halt r0`, and the machine starts with `r0` holding a
//! closure whose one method is that instruction. A program's entry block takes
//! one parameter — the continuation to answer with — so "return from `main`"
//! needs no special case anywhere: it is the same `invoke` as every other
//! return, and it happens to land on a `halt`.
//!
//! # What is not done yet
//!
//! Two things, and the second is the bigger one.
//!
//! **Register reuse.** A name is given a register when it is bound and keeps it
//! for the rest of the block, so a long straight-line block climbs through the
//! register file even when most of what it holds is dead. The IR knows exactly
//! when a value dies — that is what the environment shrinking at each
//! `substitute` means — so this is a matter of reading it, not of computing it.
//! Until then a block needing more than 256 registers is a hard error rather
//! than a spill, which is the honest failure mode: it says the allocator is
//! missing rather than quietly generating slow code. (The whole standard library
//! peaks at 29, so there is room to be unhurried about it.)
//!
//! **Contification.** Every non-trivial subexpression gets a continuation
//! object, and an object is a heap allocation — so `f (g x) + h y` allocates
//! three closures that are each entered exactly once and then dropped. Most of
//! them are known, one-shot and non-escaping, which is precisely the condition
//! for turning them back into a jump with no allocation at all. `meadow_seq`
//! already avoids the two commonest cases by hand (a variable and a literal need
//! no continuation), and doing it properly is the next real speed-up: it is
//! worth more than anything in this file.

use meadow_bytecode::{Const, Instr, Op, Pc, Program, Reg};
use meadow_core::{Lit, Prim};
use meadow_intern::InternedString;
use meadow_seq as seq;
use meadow_seq::{Block, Extern, Label, Name, Statement};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { msg: msg.into() })
}

/// How many registers there are. A `u8` operand, so this is the ceiling.
const REGISTERS: usize = 256;

/// Compile an AxCut program to a loadable image.
pub fn compile(seq: &seq::Program) -> Result<Program, Error> {
    Gen::new(seq).run()
}

/// One value in the environment: what the IR calls it, and where it is.
type Env = Vec<(Name, Reg)>;

fn reg_of(env: &Env, n: Name) -> Result<Reg, Error> {
    match env.iter().find(|(m, _)| *m == n) {
        Some((_, r)) => Ok(*r),
        None => err(format!("{n:?} is not in scope during code generation")),
    }
}

fn regs_of(env: &Env) -> Vec<Reg> {
    env.iter().map(|(_, r)| *r).collect()
}

/// The environment with the first slot named `n` dropped — what `invoke` leaves
/// for the method's arguments.
fn without(env: &Env, n: Name) -> Env {
    let mut out = env.clone();
    if let Some(i) = out.iter().position(|(m, _)| *m == n) {
        out.remove(i);
    }
    out
}

struct Gen<'a> {
    seq: &'a seq::Program,
    code: Vec<Instr>,

    consts: Vec<Const>,
    labels: Vec<InternedString>,
    shapes: Vec<Vec<InternedString>>,
    prims: Vec<Prim>,
    ops: Vec<(InternedString, InternedString)>,
    handled: Vec<Vec<(InternedString, InternedString)>>,
    messages: Vec<String>,

    /// Blocks that need their own address, in discovery order.
    regions: Vec<&'a Block>,
    region_pc: Vec<Option<Pc>>,
    pending: Vec<usize>,
    /// Instructions whose immediate is a region's address, filled in at the end.
    fixups: Vec<(usize, usize)>,
    /// Method tables, as region ids until they are resolved.
    method_tables: Vec<Vec<usize>>,
    label_region: HashMap<Label, usize>,

    max_reg: usize,
}

impl<'a> Gen<'a> {
    fn new(seq: &'a seq::Program) -> Gen<'a> {
        Gen {
            seq,
            code: Vec::new(),
            consts: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            prims: Vec::new(),
            ops: Vec::new(),
            handled: Vec::new(),
            messages: Vec::new(),
            regions: Vec::new(),
            region_pc: Vec::new(),
            pending: Vec::new(),
            fixups: Vec::new(),
            method_tables: Vec::new(),
            label_region: HashMap::new(),
            max_reg: 1,
        }
    }

    fn run(mut self) -> Result<Program, Error> {
        // Instruction 0 is the whole of "the program finished". Method table 0
        // holds it, and the machine starts with a closure over that table in
        // `r0` — so a program's entry block, which takes its continuation as its
        // one parameter, needs nothing special to return to.
        self.code.push(Instr::a(Op::Halt, 0));
        self.method_tables.push(vec![usize::MAX]);

        // Every top-level definition gets a region up front: a `jump` may name a
        // label whose block has not been reached yet.
        for def in &self.seq.defs {
            let id = self.region(&def.block);
            self.label_region.insert(def.label, id);
        }

        while let Some(id) = self.pending.pop() {
            let block = self.regions[id];
            self.region_pc[id] = Some(self.code.len() as Pc);
            // A region is entered with its parameters in r0..rn — that is what
            // `jump` and `invoke` arrange.
            let vals: Vec<Reg> = (0..block.params.len() as u16)
                .map(|i| i as Reg)
                .collect();
            self.track(block.params.len());
            self.emit_block(block, &vals)?;
        }

        for (at, region) in &self.fixups {
            let pc = self.region_pc[*region]
                .ok_or_else(|| Error {
                    msg: "a block was referenced but never emitted".into(),
                })?;
            self.code[*at].imm = pc;
        }

        let methods = self
            .method_tables
            .iter()
            .map(|table| {
                table
                    .iter()
                    .map(|id| {
                        // Table 0 is the halt continuation, whose one method is
                        // instruction 0.
                        if *id == usize::MAX {
                            Ok(0)
                        } else {
                            self.region_pc[*id].ok_or_else(|| Error {
                                msg: "a method was referenced but never emitted".into(),
                            })
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;

        // `tags` is name -> tag; the runtime wants the other direction, and only
        // for printing.
        let mut ctors = vec![InternedString::from("?"); self.seq.tags.len()];
        for (name, tag) in &self.seq.tags {
            if let Some(slot) = ctors.get_mut(*tag as usize) {
                *slot = *name;
            }
        }

        // Indexed by label, so a test runner can start at a definition other
        // than `main`. `lower_program` hands back labels 0..n densely and sorted.
        let mut entries: Vec<(Label, Pc)> = self
            .seq
            .defs
            .iter()
            .map(|def| {
                let id = self.label_region[&def.label];
                (def.label, self.region_pc[id].expect("emitted above"))
            })
            .collect();
        entries.sort_by_key(|(l, _)| *l);
        let entries: Vec<Pc> = entries.into_iter().map(|(_, pc)| pc).collect();

        let entry = self
            .seq
            .entry
            .and_then(|l| self.label_region.get(&l).copied())
            .and_then(|id| self.region_pc[id]);

        Ok(Program {
            code: self.code,
            consts: self.consts,
            methods,
            shapes: self.shapes,
            labels: self.labels,
            prims: self.prims,
            ops: self.ops,
            handled: self.handled,
            ctors,
            messages: self.messages,
            entries,
            entry,
            regs: self.max_reg as u16,
        })
    }

    // --- tables -----------------------------------------------------------

    fn region(&mut self, block: &'a Block) -> usize {
        let id = self.regions.len();
        self.regions.push(block);
        self.region_pc.push(None);
        self.pending.push(id);
        id
    }

    fn konst(&mut self, c: Const) -> u32 {
        match self.consts.iter().position(|k| *k == c) {
            Some(i) => i as u32,
            None => {
                self.consts.push(c);
                (self.consts.len() - 1) as u32
            }
        }
    }

    fn label(&mut self, l: InternedString) -> u32 {
        match self.labels.iter().position(|k| *k == l) {
            Some(i) => i as u32,
            None => {
                self.labels.push(l);
                (self.labels.len() - 1) as u32
            }
        }
    }

    fn prim(&mut self, p: Prim) -> u32 {
        match self.prims.iter().position(|k| *k == p) {
            Some(i) => i as u32,
            None => {
                self.prims.push(p);
                (self.prims.len() - 1) as u32
            }
        }
    }

    fn op(&mut self, e: InternedString, o: InternedString) -> u32 {
        match self.ops.iter().position(|k| *k == (e, o)) {
            Some(i) => i as u32,
            None => {
                self.ops.push((e, o));
                (self.ops.len() - 1) as u32
            }
        }
    }

    fn track(&mut self, high: usize) {
        self.max_reg = self.max_reg.max(high);
    }

    fn emit(&mut self, i: Instr) {
        self.code.push(i);
    }

    /// Emit an instruction whose immediate is a block's address, to be filled in
    /// once that block has been laid out.
    fn emit_to(&mut self, mut i: Instr, region: usize) {
        i.imm = 0;
        let at = self.code.len();
        self.code.push(i);
        self.fixups.push((at, region));
    }

    // --- registers --------------------------------------------------------

    /// The lowest register the environment is not using.
    fn free(&mut self, env: &Env) -> Result<Reg, Error> {
        let used: Vec<Reg> = regs_of(env);
        for r in 0..REGISTERS {
            if !used.contains(&(r as Reg)) {
                self.track(r + 1);
                return Ok(r as Reg);
            }
        }
        err("a block needs more than 256 registers; the allocator does not spill yet")
    }

    /// A run of `n` registers above everything the environment holds.
    ///
    /// Above, rather than merely unused, so that filling it can be a plain
    /// sequence of moves with nothing to clobber. A destination register chosen
    /// by [`Gen::free`] may fall inside it, which is safe: every instruction
    /// that reads a window reads it before writing its destination.
    fn window(&mut self, env: &Env, n: usize) -> Result<Reg, Error> {
        let base = regs_of(env)
            .iter()
            .map(|r| *r as usize + 1)
            .max()
            .unwrap_or(0);
        if base + n > REGISTERS {
            return err("a block needs more than 256 registers; the allocator does not spill yet");
        }
        self.track(base + n);
        Ok(base as Reg)
    }

    /// Move `srcs` into a window at `base`, in order.
    fn fill(&mut self, base: Reg, srcs: &[Reg]) {
        for (i, s) in srcs.iter().enumerate() {
            let d = base + i as Reg;
            if d != *s {
                self.emit(Instr::new(Op::Move, d, *s, 0, 0));
            }
        }
    }

    /// Move `srcs[i]` into register `i`, for a jump into a block that wants its
    /// parameters at the bottom of the file.
    fn parallel_move(&mut self, srcs: &[Reg]) -> Result<(), Error> {
        let scratch = srcs
            .iter()
            .map(|r| *r as usize + 1)
            .max()
            .unwrap_or(0)
            .max(srcs.len());
        if scratch >= REGISTERS {
            return err("a jump needs a scratch register and the file is full");
        }
        let moves = order_moves(srcs, scratch as Reg);
        if !moves.is_empty() {
            self.track(scratch + 1);
        }
        for (d, s) in moves {
            self.emit(Instr::new(Op::Move, d, s, 0, 0));
        }
        Ok(())
    }

    // --- statements -------------------------------------------------------

    fn emit_block(&mut self, block: &'a Block, vals: &[Reg]) -> Result<(), Error> {
        if block.params.len() != vals.len() {
            return err(format!(
                "block takes {} parameters but {} values reach it",
                block.params.len(),
                vals.len()
            ));
        }
        let env: Env = block
            .params
            .iter()
            .copied()
            .zip(vals.iter().copied())
            .collect();
        self.emit_stmt(&block.body, env)
    }

    fn emit_stmt(&mut self, s: &'a Statement, env: Env) -> Result<(), Error> {
        match s {
            // Pure renaming. The values are already where they are; the block
            // simply calls them something else.
            Statement::Substitute(sel, block) => {
                let vals = sel
                    .iter()
                    .map(|n| reg_of(&env, *n))
                    .collect::<Result<Vec<_>, _>>()?;
                self.emit_block(block, &vals)
            }

            Statement::Jump(label) => {
                let Some(&region) = self.label_region.get(label) else {
                    return err(format!("jump to undefined label {label:?}"));
                };
                let srcs = regs_of(&env);
                self.parallel_move(&srcs)?;
                let live = u8::try_from(srcs.len()).map_err(|_| Error {
                    msg: "a block takes more than 256 parameters".into(),
                })?;
                self.emit_to(Instr::new(Op::Jump, live, 0, 0, 0), region);
                Ok(())
            }

            Statement::Let {
                name,
                tag,
                fields,
                rest,
                ..
            } => {
                let srcs = fields
                    .iter()
                    .map(|n| reg_of(&env, *n))
                    .collect::<Result<Vec<_>, _>>()?;
                let n = u8::try_from(srcs.len()).map_err(|_| Error {
                    msg: "a constructor with more than 256 fields".into(),
                })?;
                let dst = self.free(&env)?;
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                self.emit(Instr::new(Op::MakeData, dst, base, n, *tag));
                let mut env = env;
                env.insert(0, (*name, dst));
                self.emit_stmt(rest, env)
            }

            Statement::Switch {
                scrutinee,
                arms,
                default,
            } => {
                let scr = reg_of(&env, *scrutinee)?;
                for (tag, arm) in arms {
                    let tag16 = u16::try_from(*tag).map_err(|_| Error {
                        msg: format!("constructor tag {tag} does not fit a switch operand"),
                    })?;
                    let test = self.code.len();
                    self.emit(Instr::wide(Op::JumpUnlessTag, scr, tag16, 0));

                    // The arm binds the constructor's fields, then the
                    // environment as it stands — the scrutinee included.
                    let nfields = arm.params.len() - env.len();
                    let base = self.window(&env, nfields)?;
                    let mut vals = Vec::with_capacity(arm.params.len());
                    for i in 0..nfields {
                        let d = base + i as Reg;
                        self.emit(Instr::new(Op::Field, d, scr, 0, i as u32));
                        vals.push(d);
                    }
                    vals.extend(regs_of(&env));
                    self.emit_block(arm, &vals)?;

                    // Every block body ends in a transfer, so the next
                    // instruction is where a failed test should land.
                    self.code[test].imm = self.code.len() as u32;
                }
                let vals = regs_of(&env);
                self.emit_block(default, &vals)
            }

            Statement::New {
                name,
                captures,
                methods,
                rest,
            } => {
                let srcs = captures
                    .iter()
                    .map(|n| reg_of(&env, *n))
                    .collect::<Result<Vec<_>, _>>()?;
                let ncap = u8::try_from(srcs.len()).map_err(|_| Error {
                    msg: "an object capturing more than 256 values".into(),
                })?;
                let table: Vec<usize> = methods.iter().map(|m| self.region(m)).collect();
                let table_id = self.method_tables.len() as u32;
                self.method_tables.push(table);

                let dst = self.free(&env)?;
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                self.emit(Instr::new(Op::Closure, dst, base, ncap, table_id));
                let mut env = env;
                env.insert(0, (*name, dst));
                self.emit_stmt(rest, env)
            }

            Statement::Invoke(target, tag) => {
                let obj = reg_of(&env, *target)?;
                let args = without(&env, *target);
                let srcs = regs_of(&args);
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                let method = u8::try_from(*tag).map_err(|_| Error {
                    msg: format!("method {tag} does not fit an invoke operand"),
                })?;
                self.emit(Instr::new(
                    Op::Invoke,
                    obj,
                    method,
                    base,
                    srcs.len() as u32,
                ));
                Ok(())
            }

            Statement::Extern { op, args, blocks } => self.emit_extern(op, args, blocks, env),

            Statement::Handle {
                handler,
                ops,
                k,
                rest,
            } => {
                let h = reg_of(&env, *handler)?;
                let kr = reg_of(&env, *k)?;
                let id = self.handled.len() as u32;
                self.handled.push(ops.clone());
                self.emit(Instr::new(Op::Handle, h, kr, 0, id));
                self.emit_stmt(rest, env)
            }

            Statement::Unhandle { k, rest } => {
                let dst = self.free(&env)?;
                self.emit(Instr::a(Op::Unhandle, dst));
                let mut env = env;
                env.insert(0, (*k, dst));
                self.emit_stmt(rest, env)
            }

            Statement::Perform {
                effect,
                op,
                arg,
                k,
            } => {
                let a = reg_of(&env, *arg)?;
                let kr = reg_of(&env, *k)?;
                let id = self.op(*effect, *op);
                self.emit(Instr::new(Op::Perform, a, kr, 0, id));
                Ok(())
            }

            Statement::Error(msg) => {
                let id = self.messages.len() as u32;
                self.messages.push((*msg).to_string());
                self.emit(Instr::i(Op::Error, id));
                Ok(())
            }
        }
    }

    fn emit_extern(
        &mut self,
        op: &'a Extern,
        args: &'a [Name],
        blocks: &'a [Block],
        env: Env,
    ) -> Result<(), Error> {
        // A branch is the only extern that does not produce a value, and the
        // only one with two continuations. Neither changes the environment.
        if let Extern::Branch = op {
            let [on_false, on_true] = blocks else {
                return err(format!("a branch needs 2 continuations, got {}", blocks.len()));
            };
            let [cond] = args else {
                return err(format!("a branch needs 1 argument, got {}", args.len()));
            };
            let c = reg_of(&env, *cond)?;
            let test = self.code.len();
            self.emit(Instr::ai(Op::JumpUnless, c, 0));
            let vals = regs_of(&env);
            self.emit_block(on_true, &vals)?;
            self.code[test].imm = self.code.len() as u32;
            return self.emit_block(on_false, &vals);
        }

        let [block] = blocks else {
            return err(format!(
                "a value-producing extern needs 1 continuation, got {}",
                blocks.len()
            ));
        };
        let srcs = args
            .iter()
            .map(|n| reg_of(&env, *n))
            .collect::<Result<Vec<_>, _>>()?;
        let dst = self.free(&env)?;

        match op {
            Extern::Branch => unreachable!("handled above"),
            Extern::Lit(l) => {
                let k = self.konst(constant(l));
                self.emit(Instr::ai(Op::Const, dst, k));
            }
            Extern::Prim(p) => {
                let id = self.prim(*p);
                let n = srcs.len() as u8;
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                self.emit(Instr::new(Op::Prim, dst, base, n, id));
            }
            Extern::Array => {
                let n = u8::try_from(srcs.len()).map_err(|_| Error {
                    msg: "an array literal of more than 256 elements".into(),
                })?;
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                self.emit(Instr::new(Op::MakeArray, dst, base, n, 0));
            }
            Extern::Record(fields) => {
                let n = u8::try_from(srcs.len()).map_err(|_| Error {
                    msg: "a record of more than 256 fields".into(),
                })?;
                let id = self.shapes.len() as u32;
                self.shapes.push(fields.clone());
                let base = self.window(&env, srcs.len())?;
                self.fill(base, &srcs);
                self.emit(Instr::new(Op::MakeRecord, dst, base, n, id));
            }
            Extern::Select(l) => {
                let id = self.label(*l);
                self.emit(Instr::new(Op::Select, dst, srcs[0], 0, id));
            }
            Extern::Extend(l) => {
                let id = self.label(*l);
                self.emit(Instr::new(Op::Extend, dst, srcs[0], srcs[1], id));
            }
            Extern::Field(i) => {
                self.emit(Instr::new(Op::Field, dst, srcs[0], 0, *i as u32));
            }
        }

        let mut vals = vec![dst];
        vals.extend(regs_of(&env));
        self.emit_block(block, &vals)
    }
}

/// Order the moves that put `srcs[i]` into register `i`, as `(dst, src)` pairs.
///
/// The one genuinely fiddly thing in this pass, and the reason it is a free
/// function with tests of its own: sources and destinations overlap, so the
/// moves have to be sequenced, and a cycle (`r0 ← r1`, `r1 ← r0`) has no valid
/// sequence at all. Emit whatever is safe, and when nothing is, park one value
/// in `scratch` — that breaks the cycle and the rest falls out.
///
/// A wrong answer here is not a crash. It is a program that runs and computes
/// something else.
fn order_moves(srcs: &[Reg], scratch: Reg) -> Vec<(Reg, Reg)> {
    let mut pending: Vec<(Reg, Reg)> = srcs
        .iter()
        .enumerate()
        .filter(|(i, s)| **s != *i as Reg)
        .map(|(i, s)| (*s, i as Reg))
        .collect();
    let mut out = Vec::new();

    while !pending.is_empty() {
        // A move is safe when nothing still pending reads its destination.
        let ready = pending
            .iter()
            .position(|(_, d)| !pending.iter().any(|(s, _)| s == d));
        match ready {
            Some(i) => {
                let (s, d) = pending.remove(i);
                out.push((d, s));
            }
            None => {
                let (s, _) = pending[0];
                out.push((scratch, s));
                for (src, _) in pending.iter_mut() {
                    if *src == s {
                        *src = scratch;
                    }
                }
            }
        }
    }
    out
}

fn constant(l: &Lit) -> Const {
    match l {
        Lit::Int(n) => Const::Int(*n),
        Lit::BigInt(n) => Const::BigInt(*n),
        Lit::Float(x) => Const::Float(*x),
        Lit::Str(s) => Const::Str(*s),
        Lit::Char(c) => Const::Char(*c),
        Lit::Bool(b) => Const::Bool(*b),
        Lit::Unit => Const::Unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a move sequence against a register file and see where things land.
    fn apply(srcs: &[Reg], scratch: Reg) -> Vec<u32> {
        // Each register starts holding a distinguishable value.
        let mut regs: Vec<u32> = (0..=scratch as u32 + 1).collect();
        for (d, s) in order_moves(srcs, scratch) {
            regs[d as usize] = regs[s as usize];
        }
        regs
    }

    #[track_caller]
    fn lands_correctly(srcs: &[Reg]) {
        let scratch = srcs
            .iter()
            .map(|r| *r + 1)
            .max()
            .unwrap_or(0)
            .max(srcs.len() as Reg);
        let regs = apply(srcs, scratch);
        for (i, s) in srcs.iter().enumerate() {
            assert_eq!(
                regs[i], *s as u32,
                "register {i} should hold what r{s} held, for {srcs:?}"
            );
        }
    }

    #[test]
    fn a_move_that_needs_no_moves_emits_none() {
        assert!(order_moves(&[0, 1, 2], 3).is_empty());
        assert!(order_moves(&[], 0).is_empty());
    }

    #[test]
    fn moves_are_ordered_so_nothing_is_clobbered() {
        // r0 <- r1, r1 <- r2: doing them in the written order would lose r1.
        lands_correctly(&[1, 2, 3]);
        lands_correctly(&[3, 2, 1, 0]);
        lands_correctly(&[2, 0, 1]);
    }

    #[test]
    fn a_cycle_is_broken_with_the_scratch_register() {
        // A swap has no valid order at all — one value has to be parked.
        lands_correctly(&[1, 0]);
        let moves = order_moves(&[1, 0], 2);
        assert!(
            moves.iter().any(|(d, _)| *d == 2),
            "a swap should use the scratch register: {moves:?}"
        );
        // Two independent cycles, and a cycle with a tail hanging off it.
        lands_correctly(&[1, 0, 3, 2]);
        lands_correctly(&[1, 2, 0]);
        lands_correctly(&[1, 2, 0, 0]);
    }

    #[test]
    fn duplicated_sources_all_arrive() {
        // `f f x` puts the same value in two places, and reading a register
        // twice has to keep working after the first write.
        lands_correctly(&[2, 2, 2]);
        lands_correctly(&[1, 1, 0]);
    }
}
