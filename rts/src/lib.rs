//! # The Meadow runtime system
//!
//! A register bytecode VM, with three things it is trying to be:
//!
//! * **fast enough to replace the CEK machine** — registers rather than an
//!   explicit environment, fixed-width instructions, and a segmented stack so an
//!   effect operation costs the number of *handlers* between it and its handler
//!   rather than the number of frames;
//! * **introspectable** — the code, the stack, the segments and the handler
//!   table are all readable at any point, and disassembly is a first-class
//!   output rather than a debugging afterthought;
//! * **omniscient** — every write records its inverse, so the machine can step
//!   *backwards* as well as forwards.
//!
//! ## The CEK machine is the specification
//!
//! `meadow_eval` stays. It is small enough to read and to believe, it defines
//! what a Meadow program *means*, and this crate is checked against it: the same
//! `core::Program` run through both must produce the same value. Where they
//! disagree, the CEK is right by definition and this is a bug. That is the whole
//! reason for keeping a slow interpreter around after writing a fast one.
//!
//! ## The pipeline
//!
//! ```text
//!   core::Program
//!    │  meadow_seq::lower_program     expressions -> sequent statements
//!    ▼
//!   seq::Program                      producers, consumers, cuts
//!    │  rts::compile                  statements -> registers and jumps
//!    ▼
//!   rts::Chunk                        fixed 8-byte instructions
//!    │  rts::Vm::run
//!    ▼
//!   Value
//! ```
//!
//! The sequent step is not decoration. A register machine wants control to be
//! explicit — where does this result go, what runs next — and that is precisely
//! what a sequent IR makes syntactic: a continuation is a covariable, and an
//! effect handler is something a `perform` looks for rather than a special form
//! the code generator must know about. See `meadow_seq` for what is and is not
//! borrowed from the AxCut line of work.
//!
//! ## Status
//!
//! Early. What exists and is tested:
//!
//! * the instruction encoding ([`code`]) — fixed 8-byte, round-tripped;
//! * the value representation ([`value`]) — shared payloads, iterative equality;
//! * the segmented stack and one-shot resumptions ([`stack`]);
//! * the inverse-operation journal ([`journal`]).
//!
//! What does not exist yet: the code generator from `seq`, the interpreter loop,
//! and therefore any of the differential testing against the CEK. The pieces
//! above are the ones the rest is built on, and they are the ones whose design
//! is hard to change later.

pub mod code;
pub mod journal;
pub mod stack;
pub mod value;

pub use code::{Instr, Op, Pc, Reg};
pub use journal::{Journal, Undo};
pub use stack::{Captured, Frame, Handler, Resumption, Segment};
pub use value::{value_eq, Value};

/// A compiled function: its code, and how many registers it needs.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    pub name: meadow_intern::InternedString,
    pub code: Vec<Instr>,
    /// How many registers the frame needs. Checked against 256 when compiled.
    pub regs: u16,
    /// How many arguments it takes.
    pub arity: u8,
}

impl Chunk {
    /// A disassembly, one instruction per line, addresses on the left.
    ///
    /// Fixed-width instructions are what make this a loop rather than a decoder:
    /// line `n` is instruction `n`, and a jump to `@n` points at the line that
    /// says `n`.
    pub fn disassemble(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "fn {} ({} args, {} regs)", self.name, self.arity, self.regs);
        for (pc, instr) in self.code.iter().enumerate() {
            let _ = writeln!(out, "  {pc:>4}  {instr}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disassembly_numbers_lines_the_way_jumps_do() {
        // A jump target is an instruction index, so the listing has to agree —
        // otherwise every jump in a dump points at the wrong line.
        let chunk = Chunk {
            name: meadow_intern::InternedString::from("f"),
            code: vec![
                Instr::ai(Op::Const, 0, 7),
                Instr::i(Op::Jump, 0),
                Instr::a(Op::Ret, 0),
            ],
            regs: 1,
            arity: 0,
        };
        let text = chunk.disassemble();
        assert!(text.contains("     0  const"), "{text}");
        assert!(text.contains("     1  jump            @0"), "{text}");
        assert!(text.contains("     2  ret             r0"), "{text}");
    }
}
