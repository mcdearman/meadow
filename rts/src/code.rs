//! The instruction set: **fixed-width, 8 bytes each**.
//!
//! ```text
//!   0        1        2        3        4                       7
//! ┌────────┬────────┬────────┬────────┬────────────────────────────┐
//! │ opcode │   a    │   b    │   c    │            imm             │
//! └────────┴────────┴────────┴────────┴────────────────────────────┘
//!    u8       u8       u8       u8                u32
//! ```
//!
//! Three register operands and one 32-bit immediate. Registers are `u8`, so a
//! frame has at most 256 of them; the compiler is responsible for staying under
//! that, and says so if it cannot.
//!
//! Fixed width buys three things this VM specifically wants:
//!
//! * **A program counter is an index**, not a byte offset, so a jump target is a
//!   plain instruction number and the disassembler never has to decode from the
//!   start to find instruction boundaries.
//! * **Stepping backwards is possible.** The journal (see [`crate::journal`])
//!   records `pc` as an index; with variable-width instructions "the previous
//!   instruction" is not well defined without decoding forward from a known
//!   point.
//! * Decoding is a load and three shifts, with no branch on length.
//!
//! The cost is code size — a `Move` carries three unused bytes — which is the
//! usual trade and not one that matters at this scale.

use std::fmt;

/// A register within the current frame.
pub type Reg = u8;

/// An index into a chunk's instruction list. Also what a jump target is.
pub type Pc = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Does nothing. Kept so a patched-out instruction need not move anything.
    Nop = 0,

    // --- moving values -------------------------------------------------------
    /// `r[a] = consts[imm]`
    Const,
    /// `r[a] = r[b]`
    Move,

    // --- control -------------------------------------------------------------
    /// `pc = imm`
    Jump,
    /// `if r[a] is False then pc = imm`
    JumpUnless,
    /// Return `r[a]` from the current frame.
    Ret,
    /// Stop, with `r[a]` as the program's result.
    Halt,

    // --- functions -----------------------------------------------------------
    /// `r[a] = closure(functions[imm], capturing r[b..b+c])`
    Closure,
    /// Call `r[b]` with `c` arguments starting at `r[b+1]`; result into `r[a]`.
    ///
    /// Arguments are required to sit directly above the callee so that a call
    /// needs one register operand rather than two, which is what keeps this
    /// instruction inside its four bytes.
    Call,
    /// Like [`Op::Call`], but reuses the current frame. The compiler emits this
    /// for a call in tail position, which is what makes unbounded recursion work
    /// without growing the stack.
    TailCall,

    // --- data ----------------------------------------------------------------
    /// `r[a] = Ctor(names[imm], r[b..b+c])`
    MakeCtor,
    /// `r[a] = Tuple(r[b..b+c])`
    MakeTuple,
    /// `r[a] = field imm of r[b]` — for a constructor or a tuple.
    Field,
    /// `if r[a] is not a constructor named names[imm] then pc = c` — the
    /// building block of a `case`.
    JumpUnlessCtor,

    // --- primitives ----------------------------------------------------------
    /// `r[a] = prim[imm](r[b..b+c])`
    Prim,

    // --- effects -------------------------------------------------------------
    /// Push a stack segment whose handler is `handlers[imm]`, and continue.
    ///
    /// Everything between here and the matching [`Op::PopSeg`] runs in that
    /// segment, so a `perform` inside it finds this handler by walking outward
    /// one segment at a time rather than one frame at a time.
    PushSeg,
    /// Pop the current segment; `r[a]` is the value the handled body produced,
    /// which the handler's `return` clause receives.
    PopSeg,
    /// Perform `ops[imm]` with argument `r[b]`, result into `r[a]`.
    ///
    /// Finds the innermost segment whose handler has a clause for the operation,
    /// *detaches* the segments above it as a resumption, and enters the clause.
    Perform,
    /// Resume the one-shot continuation in `r[b]` with value `r[c]`, result into
    /// `r[a]`. Re-attaches the detached segments.
    Resume,
}

impl Op {
    /// Every opcode, in numeric order — used to decode and to check the table is
    /// dense.
    pub const ALL: &'static [Op] = &[
        Op::Nop,
        Op::Const,
        Op::Move,
        Op::Jump,
        Op::JumpUnless,
        Op::Ret,
        Op::Halt,
        Op::Closure,
        Op::Call,
        Op::TailCall,
        Op::MakeCtor,
        Op::MakeTuple,
        Op::Field,
        Op::JumpUnlessCtor,
        Op::Prim,
        Op::PushSeg,
        Op::PopSeg,
        Op::Perform,
        Op::Resume,
    ];

    fn from_byte(b: u8) -> Option<Op> {
        Op::ALL.get(b as usize).copied()
    }
}

/// One instruction, decoded.
///
/// The packed form is [`Instr::encode`]; this is what the VM and the
/// disassembler work with. Keeping them separate means the encoding can change
/// without touching either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
    pub a: Reg,
    pub b: Reg,
    pub c: Reg,
    pub imm: u32,
}

impl Instr {
    pub fn new(op: Op, a: Reg, b: Reg, c: Reg, imm: u32) -> Instr {
        Instr { op, a, b, c, imm }
    }

    /// An instruction with only a destination register.
    pub fn a(op: Op, a: Reg) -> Instr {
        Instr::new(op, a, 0, 0, 0)
    }

    /// An instruction with a destination and an immediate.
    pub fn ai(op: Op, a: Reg, imm: u32) -> Instr {
        Instr::new(op, a, 0, 0, imm)
    }

    /// An instruction with only an immediate — a jump.
    pub fn i(op: Op, imm: u32) -> Instr {
        Instr::new(op, 0, 0, 0, imm)
    }

    pub fn encode(self) -> [u8; 8] {
        let [i0, i1, i2, i3] = self.imm.to_le_bytes();
        [self.op as u8, self.a, self.b, self.c, i0, i1, i2, i3]
    }

    /// `None` if the first byte is not a known opcode — which can only happen
    /// for bytes this crate did not write.
    pub fn decode(bytes: [u8; 8]) -> Option<Instr> {
        Some(Instr {
            op: Op::from_byte(bytes[0])?,
            a: bytes[1],
            b: bytes[2],
            c: bytes[3],
            imm: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        })
    }
}

impl fmt::Display for Instr {
    /// Prints only the operands an opcode actually reads, so a disassembly is
    /// not three columns of noise.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = format!("{:?}", self.op).to_lowercase();
        match self.op {
            Op::Nop => write!(f, "{name}"),
            Op::Const => write!(f, "{name:<15} r{} <- k{}", self.a, self.imm),
            Op::Move => write!(f, "{name:<15} r{} <- r{}", self.a, self.b),
            Op::Jump => write!(f, "{name:<15} @{}", self.imm),
            Op::JumpUnless => write!(f, "{name:<15} r{} @{}", self.a, self.imm),
            Op::Ret | Op::Halt | Op::PopSeg => write!(f, "{name:<15} r{}", self.a),
            Op::Closure => write!(
                f,
                "{name:<15} r{} <- f{} [r{}..+{}]",
                self.a, self.imm, self.b, self.c
            ),
            Op::Call | Op::TailCall => {
                write!(f, "{name:<15} r{} <- r{}({} args)", self.a, self.b, self.c)
            }
            Op::MakeCtor => write!(
                f,
                "{name:<15} r{} <- n{}[r{}..+{}]",
                self.a, self.imm, self.b, self.c
            ),
            Op::MakeTuple => write!(f, "{name:<15} r{} <- (r{}..+{})", self.a, self.b, self.c),
            Op::Field => write!(f, "{name:<15} r{} <- r{}.{}", self.a, self.b, self.imm),
            Op::JumpUnlessCtor => {
                write!(f, "{name:<15} r{} != n{} @{}", self.a, self.imm, self.c)
            }
            Op::Prim => write!(
                f,
                "{name:<15} r{} <- p{}(r{}..+{})",
                self.a, self.imm, self.b, self.c
            ),
            Op::PushSeg => write!(f, "{name:<15} h{}", self.imm),
            Op::Perform => write!(f, "{name:<15} r{} <- o{}(r{})", self.a, self.imm, self.b),
            Op::Resume => write!(f, "{name:<15} r{} <- r{}(r{})", self.a, self.b, self.c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opcode_table_is_dense_and_in_order() {
        // `from_byte` indexes `ALL`, so a gap or a reordering would silently
        // decode to the wrong instruction.
        for (i, op) in Op::ALL.iter().enumerate() {
            assert_eq!(*op as usize, i, "{op:?} is out of place");
            assert_eq!(Op::from_byte(i as u8), Some(*op));
        }
        assert_eq!(Op::from_byte(Op::ALL.len() as u8), None);
    }

    #[test]
    fn every_instruction_round_trips() {
        for (i, op) in Op::ALL.iter().enumerate() {
            let instr = Instr::new(*op, 1, 2, 3, 0xDEAD_BEEF);
            let back = Instr::decode(instr.encode()).expect("decodes");
            assert_eq!(instr, back, "{op:?} (opcode {i}) did not round trip");
        }
    }

    #[test]
    fn an_instruction_is_exactly_eight_bytes() {
        // The whole design rests on this: a pc is an index, and stepping
        // backwards means subtracting one.
        assert_eq!(Instr::a(Op::Ret, 0).encode().len(), 8);
    }

    #[test]
    fn an_unknown_opcode_decodes_to_nothing_rather_than_something_wrong() {
        assert_eq!(Instr::decode([250, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn the_immediate_survives_its_full_range() {
        for imm in [0, 1, u32::MAX, 0x8000_0000] {
            let i = Instr::i(Op::Jump, imm);
            assert_eq!(Instr::decode(i.encode()).unwrap().imm, imm);
        }
    }
}
