//! The Meadow bytecode.
//!
//! This is the contract between the compiler and `meadow_rts`, and it is
//! deliberately dull: **straight-line instructions over a flat register file**.
//! Everything interesting — the sequent-calculus IR, the environment shapes, the
//! decision about which value lives in which register — happens in
//! `meadow_codegen` and is gone by the time an image reaches the runtime. The VM
//! sees registers and jumps.
//!
//! # Fixed width, 8 bytes each
//!
//! ```text
//!   0        1        2        3        4                       7
//! ┌────────┬────────┬────────┬────────┬────────────────────────────┐
//! │ opcode │   a    │   b    │   c    │            imm             │
//! └────────┴────────┴────────┴────────┴────────────────────────────┘
//!    u8       u8       u8       u8                u32
//! ```
//!
//! Three register operands and a 32-bit immediate; `b` and `c` together also
//! read as one 16-bit field ([`Instr::bc`]) where an opcode needs a wide operand
//! *and* a jump target. Registers are `u8`, so a program has at most 256 of them
//! live at once and the compiler says so if it needs more.
//!
//! Fixed width buys two things. A program counter is an **index**, so a jump
//! target is an instruction number and a disassembler never has to decode from
//! the start to find boundaries. And stepping *backwards* is well defined, which
//! variable-width encodings make awkward — see `meadow_rts::journal`.
//!
//! # There is no call stack
//!
//! The strangest thing about this instruction set, and the one that comes
//! straight from the IR above it: there is no `call`, no `ret`, and no frame
//! pointer. A closure, a continuation and a handler are all the same kind of
//! heap object, and entering one ([`Op::Invoke`]) rebuilds the register file and
//! jumps. Returning from a function *is* invoking the continuation it was given.
//!
//! So the machine's whole state is a program counter, one flat register file, a
//! heap, and a stack of installed handlers. Recursion does not grow anything the
//! VM owns; it grows the heap, which is collected.

use meadow_core::Prim;
use meadow_intern::InternedString;
use std::fmt;

/// A register. There is one flat file, not a frame — see the module docs.
pub type Reg = u8;

/// An index into [`Program::code`]. Also what a jump target is.
pub type Pc = u32;

/// Which constructor of a data type, or which method of a heap object.
pub type Tag = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Does nothing. Kept so a patched-out instruction need not move anything.
    Nop = 0,

    // --- moving values ---------------------------------------------------
    /// `r[a] = r[b]`
    Move,
    /// `r[a] = consts[imm]`
    Const,

    // --- control ---------------------------------------------------------
    /// `pc = imm`. `a` is how many registers are live at the target, which is
    /// what the collector uses as its root set — see [`Op::Invoke`].
    Jump,
    /// `if r[a] is false then pc = imm`
    JumpUnless,
    /// `if r[a] is not data whose tag is bc then pc = imm`
    ///
    /// The tag is 16 bits so that the jump target can keep the full immediate.
    JumpUnlessTag,
    /// Stop, with `r[a]` as the result.
    Halt,
    /// Fail with `messages[imm]`.
    Error,

    // --- data ------------------------------------------------------------
    /// `r[a] = data(tag = imm, fields = r[b .. b+c])`
    MakeData,
    /// `r[a] = array(r[b .. b+c])`
    MakeArray,
    /// `r[a] = record(shapes[imm] zipped with r[b .. b+c])`
    MakeRecord,
    /// `r[a] = field imm of r[b]` — data, or an array element.
    Field,
    /// `r[a] = r[b].labels[imm]`
    Select,
    /// `r[a] = { r[b] with labels[imm] = r[c] }`
    Extend,

    // --- codata ----------------------------------------------------------
    /// `r[a] = closure(methods[imm], capturing r[b .. b+c])`
    ///
    /// One heap shape for closures, continuations and handlers, because the IR
    /// makes no distinction between them.
    Closure,
    /// Enter method `b` of the object in `r[a]`, with `imm` arguments starting
    /// at `r[c]`.
    ///
    /// The register file is rebuilt as the object's captures followed by those
    /// arguments, and the pc becomes the method's entry. That is the whole
    /// calling convention: no frame is pushed and nothing is saved, because the
    /// only way back is a continuation the caller already passed along.
    ///
    /// Invoking the machine's initial continuation halts it.
    Invoke,

    // --- primitives ------------------------------------------------------
    /// `r[a] = prims[imm](r[b .. b+c])` — for arity 3, where a window is
    /// cheaper than more operand fields.
    Prim,
    /// `r[a] = prims[imm](r[b])`
    Prim1,
    /// `r[a] = prims[imm](r[b], r[c])`
    ///
    /// Three-address, the way Lua spells `ADD A B C`. Without it every binary operation
    /// carried two `move`s to gather its arguments into a window — three
    /// instructions and two register writes to add two numbers.
    Prim2,
    /// `r[a] = prims[c](r[b], consts[imm])`
    ///
    /// A binary primitive whose right operand is a literal — `n - 1`. The
    /// constant is not loaded into a register first, so it costs no instruction,
    /// no register, and nothing at the next jump.
    ///
    /// `c` names the primitive here rather than `imm`, which the constant needs.
    /// Every opcode below follows that: `c` is the primitive.
    PrimK,
    /// `if not prims[c](r[a], r[b]) then pc = imm`
    ///
    /// A comparison and the branch that tests it, in one instruction. The
    /// boolean is never written anywhere the program can see.
    JumpUnlessPrim,
    /// `if not prims[c](r[a], consts[b]) then pc = imm`
    ///
    /// The same against a literal — `if n == 0`, and every `match` on a literal
    /// pattern. `b` is a constant index rather than a register, which caps it at
    /// 256: the compiler emits [`Op::PrimK`] and [`Op::JumpUnless`] instead when
    /// the constant it wants lives higher than that.
    JumpUnlessPrimK,

    // --- effects ---------------------------------------------------------
    /// Install the handler in `r[a]`, covering `handled[imm]`, whose value goes
    /// to the continuation in `r[b]`.
    Handle,
    /// Pop the innermost handler; `r[a]` becomes its continuation.
    ///
    /// A resumption rebinds that continuation to the point it was resumed from,
    /// so this reads it off the frame rather than from a register the compiler
    /// captured earlier.
    Unhandle,
    /// Perform `ops[imm]` with argument `r[a]`, answering the continuation in
    /// `r[b]`.
    ///
    /// Unwinds to the innermost handler covering the operation and enters its
    /// clause with the argument, a one-shot resumption, and the handler's own
    /// continuation.
    Perform,
}

impl Op {
    /// Every opcode, in numeric order — used to decode, and to check the table
    /// is dense.
    pub const ALL: &'static [Op] = &[
        Op::Nop,
        Op::Move,
        Op::Const,
        Op::Jump,
        Op::JumpUnless,
        Op::JumpUnlessTag,
        Op::Halt,
        Op::Error,
        Op::MakeData,
        Op::MakeArray,
        Op::MakeRecord,
        Op::Field,
        Op::Select,
        Op::Extend,
        Op::Closure,
        Op::Invoke,
        Op::Prim,
        Op::Prim1,
        Op::Prim2,
        Op::PrimK,
        Op::JumpUnlessPrim,
        Op::JumpUnlessPrimK,
        Op::Handle,
        Op::Unhandle,
        Op::Perform,
    ];

    fn from_byte(b: u8) -> Option<Op> {
        Op::ALL.get(b as usize).copied()
    }
}

/// One instruction, decoded.
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

    /// A destination and an immediate.
    pub fn ai(op: Op, a: Reg, imm: u32) -> Instr {
        Instr::new(op, a, 0, 0, imm)
    }

    /// Only an immediate — a jump.
    pub fn i(op: Op, imm: u32) -> Instr {
        Instr::new(op, 0, 0, 0, imm)
    }

    /// `b` and `c` read as one 16-bit operand.
    pub fn bc(self) -> u16 {
        u16::from_le_bytes([self.b, self.c])
    }

    /// Build one whose `b`/`c` carry a 16-bit operand.
    pub fn wide(op: Op, a: Reg, bc: u16, imm: u32) -> Instr {
        let [b, c] = bc.to_le_bytes();
        Instr { op, a, b, c, imm }
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

/// A literal the program can load.
///
/// Not `core::Lit`: that is a compiler type and this is an image format, and the
/// difference will matter the first time one of them wants to change.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Unit,
    Bool(bool),
    Int(i64),
    /// An integer literal whose inferred type is `BigInt`, widened at load.
    BigInt(i64),
    Float(f64),
    Str(InternedString),
    Char(char),
}

/// A loadable image: the code, and every table an instruction's immediate
/// indexes into.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub code: Vec<Instr>,
    pub consts: Vec<Const>,
    /// Method entry points, one list per [`Op::Closure`] shape.
    pub methods: Vec<Vec<Pc>>,
    /// Field names for [`Op::MakeRecord`], in argument order.
    pub shapes: Vec<Vec<InternedString>>,
    /// Field names for [`Op::Select`] and [`Op::Extend`].
    pub labels: Vec<InternedString>,
    /// Which primitive an [`Op::Prim`] runs.
    pub prims: Vec<Prim>,
    /// `(effect, operation)` for [`Op::Perform`].
    pub ops: Vec<(InternedString, InternedString)>,
    /// What each [`Op::Handle`] covers, in the order of the handler's methods.
    pub handled: Vec<Vec<(InternedString, InternedString)>>,
    /// Constructor name per tag. Only for printing and for the structural
    /// equality a `Vector` needs — the machine itself compares tags.
    pub ctors: Vec<InternedString>,
    /// What an [`Op::Error`] says.
    pub messages: Vec<String>,
    /// Entry point per top-level definition, in the compiler's label order. A
    /// test runner uses these to start somewhere other than `main`.
    pub entries: Vec<Pc>,
    pub entry: Option<Pc>,
    /// The most registers any block needs. Checked against 256 when built.
    pub regs: u16,
}

impl Program {
    /// The constructor a tag names, for printing.
    pub fn ctor(&self, tag: Tag) -> Option<InternedString> {
        self.ctors.get(tag as usize).copied()
    }

    /// A disassembly, one instruction per line, addresses on the left.
    ///
    /// Fixed-width instructions are what make this a loop rather than a decoder:
    /// line `n` is instruction `n`, and a jump to `@n` points at the line that
    /// says `n`.
    pub fn disassemble(&self) -> String {
        use fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "; {} instructions, {} registers, entry @{}",
            self.code.len(),
            self.regs,
            match self.entry {
                Some(pc) => pc.to_string(),
                None => "-".into(),
            }
        );
        for (pc, instr) in self.code.iter().enumerate() {
            let _ = writeln!(out, "{pc:>6}  {}", self.show(*instr));
        }
        out
    }

    fn prim_name(&self, id: u32) -> String {
        match self.prims.get(id as usize) {
            Some(p) => format!("{p:?}"),
            None => format!("p{id}"),
        }
    }

    fn const_name(&self, id: u32) -> String {
        match self.consts.get(id as usize) {
            Some(c) => format!("{c:?}"),
            None => format!("k{id}"),
        }
    }

    /// One instruction, with its immediate resolved against the tables — the
    /// difference between a readable dump and a column of integers.
    pub fn show(&self, i: Instr) -> String {
        let name = format!("{:?}", i.op).to_lowercase();
        let k = |v: &Vec<InternedString>| {
            v.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
        };
        match i.op {
            Op::Nop => name,
            Op::Move => format!("{name:<14} r{} <- r{}", i.a, i.b),
            Op::Const => match self.consts.get(i.imm as usize) {
                Some(c) => format!("{name:<14} r{} <- {c:?}", i.a),
                None => format!("{name:<14} r{} <- k{}", i.a, i.imm),
            },
            Op::Jump => format!("{name:<14} @{} ({} live)", i.imm, i.a),
            Op::JumpUnless => format!("{name:<14} r{} @{}", i.a, i.imm),
            Op::JumpUnlessTag => {
                let ctor = self
                    .ctor(i.bc() as Tag)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("#{}", i.bc()));
                format!("{name:<14} r{} is {ctor} else @{}", i.a, i.imm)
            }
            Op::Halt => format!("{name:<14} r{}", i.a),
            Op::Error => match self.messages.get(i.imm as usize) {
                Some(m) => format!("{name:<14} {m:?}"),
                None => format!("{name:<14} m{}", i.imm),
            },
            Op::MakeData => {
                let ctor = self
                    .ctor(i.imm)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("#{}", i.imm));
                format!("{name:<14} r{} <- {ctor}(r{}..+{})", i.a, i.b, i.c)
            }
            Op::MakeArray => format!("{name:<14} r{} <- #[r{}..+{}]", i.a, i.b, i.c),
            Op::MakeRecord => match self.shapes.get(i.imm as usize) {
                Some(s) => format!("{name:<14} r{} <- {{{}}} r{}..+{}", i.a, k(s), i.b, i.c),
                None => format!("{name:<14} r{} <- s{} r{}..+{}", i.a, i.imm, i.b, i.c),
            },
            Op::Field => format!("{name:<14} r{} <- r{}.{}", i.a, i.b, i.imm),
            Op::Select => match self.labels.get(i.imm as usize) {
                Some(l) => format!("{name:<14} r{} <- r{}.{l}", i.a, i.b),
                None => format!("{name:<14} r{} <- r{}.l{}", i.a, i.b, i.imm),
            },
            Op::Extend => match self.labels.get(i.imm as usize) {
                Some(l) => format!("{name:<14} r{} <- {{ r{} | {l} = r{} }}", i.a, i.b, i.c),
                None => format!("{name:<14} r{} <- {{ r{} | l{} = r{} }}", i.a, i.b, i.imm, i.c),
            },
            Op::Closure => format!("{name:<14} r{} <- m{} [r{}..+{}]", i.a, i.imm, i.b, i.c),
            Op::Invoke => format!("{name:<14} r{}#{} (r{}..+{})", i.a, i.b, i.c, i.imm),
            Op::Prim1 => match self.prims.get(i.imm as usize) {
                Some(p) => format!("{name:<14} r{} <- {p:?}(r{})", i.a, i.b),
                None => format!("{name:<14} r{} <- p{}(r{})", i.a, i.imm, i.b),
            },
            Op::Prim2 => match self.prims.get(i.imm as usize) {
                Some(p) => format!("{name:<14} r{} <- {p:?}(r{}, r{})", i.a, i.b, i.c),
                None => format!("{name:<14} r{} <- p{}(r{}, r{})", i.a, i.imm, i.b, i.c),
            },
            Op::Prim => match self.prims.get(i.imm as usize) {
                Some(p) => format!("{name:<14} r{} <- {p:?}(r{}..+{})", i.a, i.b, i.c),
                None => format!("{name:<14} r{} <- p{}(r{}..+{})", i.a, i.imm, i.b, i.c),
            },
            // The folded forms name their primitive in `c`, and their constant
            // where the fields allow: `imm` for a value, `b` for a branch.
            Op::PrimK => {
                let p = self.prim_name(i.c as u32);
                let c = self.const_name(i.imm);
                format!("{name:<14} r{} <- {p}(r{}, {c})", i.a, i.b)
            }
            Op::JumpUnlessPrim => {
                let p = self.prim_name(i.c as u32);
                format!("{name:<14} {p}(r{}, r{}) else @{}", i.a, i.b, i.imm)
            }
            Op::JumpUnlessPrimK => {
                let p = self.prim_name(i.c as u32);
                let c = self.const_name(i.b as u32);
                format!("{name:<14} {p}(r{}, {c}) else @{}", i.a, i.imm)
            }
            Op::Handle => format!("{name:<14} r{} covering h{} -> r{}", i.a, i.imm, i.b),
            Op::Unhandle => format!("{name:<14} r{}", i.a),
            Op::Perform => match self.ops.get(i.imm as usize) {
                Some((e, o)) => format!("{name:<14} {e}.{o}(r{}) -> r{}", i.a, i.b),
                None => format!("{name:<14} o{}(r{}) -> r{}", i.imm, i.a, i.b),
            },
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
        assert_eq!(Instr::a(Op::Halt, 0).encode().len(), 8);
    }

    #[test]
    fn an_unknown_opcode_decodes_to_nothing_rather_than_something_wrong() {
        assert_eq!(Instr::decode([250, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn the_wide_operand_uses_both_spare_bytes() {
        // A tag needs more than 8 bits and a jump target needs the immediate, so
        // `JumpUnlessTag` has to fit a 16-bit operand into `b` and `c`.
        for bc in [0u16, 1, 255, 256, u16::MAX] {
            let i = Instr::wide(Op::JumpUnlessTag, 3, bc, 99);
            let back = Instr::decode(i.encode()).unwrap();
            assert_eq!(back.bc(), bc);
            assert_eq!(back.imm, 99);
            assert_eq!(back.a, 3);
        }
    }

    #[test]
    fn the_immediate_survives_its_full_range() {
        for imm in [0, 1, u32::MAX, 0x8000_0000] {
            let i = Instr::i(Op::Jump, imm);
            assert_eq!(Instr::decode(i.encode()).unwrap().imm, imm);
        }
    }
}
