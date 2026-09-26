//! AArch64 (AAPCS64): instructions encoded by hand.
//!
//! Machine registers during a block function: `x19` the machine, `x20` its
//! register file, `x21` steps not yet counted in the machine -- all saved by
//! the callee, so they survive the calls into the interpreter -- `x22` the
//! loop iterations this call has made, and `x9` through `x17`, `d0` and `d1`
//! for the instruction being done.
//!
//! `x2` to `x8` and `x23` to `x28` hold bytecode registers `r0` to `r12`, in
//! every function alike, so that one function goes on to another with nothing
//! moved: they are loaded from the register file where the machine enters
//! native code (the prologue), written to it before every call into the
//! interpreter and every return, and loaded again after every call. A
//! register a function only ever does float arithmetic on lives in one of
//! `d16` to `d31` for the function's duration instead (see
//! [`super::Emit::configure`]): moved in from its general register at the warm
//! entry and back out before control goes on to another function. Nothing
//! else touches any of them. Arithmetic on them is done in place -- `add x3,
//! x4, x5`, `fmul d17, d17, d18` -- and only a register that lives in memory
//! goes through `x9`, `x10`, `d0` or `d1`.

use super::vector::{Lanes, Plan, VOp};
use super::{Emit, FloatOp, IntOp, Label, Operand, UnaryOp, layout, thin};
use meadow_bytecode::{Cond, Instr, Op, Pc, Reg};
use meadow_core::OptLevel;

const X0: u32 = 0;
const X1: u32 = 1;
const X9: u32 = 9;
const X10: u32 = 10;
const X11: u32 = 11;
const X12: u32 = 12;
const X13: u32 = 13;
const X14: u32 = 14;
const X15: u32 = 15;
const X17: u32 = 17;
const X16: u32 = 16;
const X19: u32 = 19;
const X20: u32 = 20;
const X21: u32 = 21;
const X22: u32 = 22;
const X23: u32 = 23;
const X24: u32 = 24;
const X25: u32 = 25;
const X26: u32 = 26;
const X27: u32 = 27;
const X28: u32 = 28;
const FP: u32 = 29;
const LR: u32 = 30;
const SP: u32 = 31;

/// Where the fixed registers live, `r0` first: the argument registers, which
/// nothing here calls with, and the callee-saved ones the prologue keeps.
const PIN_REGS: [u32; 13] = [2, 3, 4, 5, 6, 7, 8, 23, 24, 25, 26, 27, 28];

/// Where a function's float registers live: the caller-saved half of the
/// vector file, which costs nothing to take, since every call spills them
/// anyway.
const FLOAT_PIN_REGS: [u32; 16] = [
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
];

/// The machine register a fixed bytecode register is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loc {
    X(u32),
    D(u32),
}

// Condition codes.
const EQ: u32 = 0;
const NE: u32 = 1;
const HS: u32 = 2;
const HI: u32 = 8;
const MI: u32 = 4;
const LS: u32 = 9;
const GE: u32 = 10;
const LT: u32 = 11;
const GT: u32 = 12;
const LE: u32 = 13;

#[derive(Clone, Copy)]
enum Fixup {
    /// `b`: 26 bits.
    B,
    /// `b.cond`, `cbz`, `cbnz`: 19 bits at bit 5.
    Imm19,
}

impl Fixup {
    /// Whether a branch this far, in instructions, fits.
    fn reaches(self, delta: i64) -> bool {
        match self {
            Fixup::B => (-(1 << 25)..(1 << 25)).contains(&delta),
            Fixup::Imm19 => (-(1 << 18)..(1 << 18)).contains(&delta),
        }
    }
}

pub struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, Label, Fixup)>,
    /// The current function's return of what the interpreter said.
    ret_w0: Option<Label>,
    /// Writing a register leaves `live` to the caller (see `codegen::Book`).
    defer_live: bool,
    /// Constant operands encoded into the instruction, where they fit.
    short: bool,
    /// The fixed bytecode registers, and where each lives in this function.
    pins: Vec<(Reg, Loc)>,
    /// Go straight on to the next block's native code where there is some.
    chain: bool,
}

impl Asm {
    fn put(&mut self, word: u32) {
        self.code.extend_from_slice(&word.to_le_bytes());
    }

    fn at(&mut self, l: Label, f: Fixup, word: u32) {
        self.fixups.push((self.code.len(), l, f));
        self.put(word);
    }

    fn ldr(&mut self, t: u32, n: u32, off: u32) {
        debug_assert!(off % 8 == 0 && off / 8 < 4096);
        self.put(0xF940_0000 | (off / 8) << 10 | n << 5 | t);
    }

    fn str(&mut self, t: u32, n: u32, off: u32) {
        debug_assert!(off % 8 == 0 && off / 8 < 4096);
        self.put(0xF900_0000 | (off / 8) << 10 | n << 5 | t);
    }

    fn ldr_d(&mut self, t: u32, n: u32, off: u32) {
        self.put(0xFD40_0000 | (off / 8) << 10 | n << 5 | t);
    }

    fn str_d(&mut self, t: u32, n: u32, off: u32) {
        self.put(0xFD00_0000 | (off / 8) << 10 | n << 5 | t);
    }

    /// `xd = imm`, in as few `movz`/`movk` as its nonzero halves need.
    fn imm(&mut self, d: u32, imm: u64) {
        self.put(0xD280_0000 | ((imm & 0xFFFF) as u32) << 5 | d);
        for hw in 1..4 {
            let part = (imm >> (16 * hw)) & 0xFFFF;
            if part != 0 {
                self.put(0xF280_0000 | hw << 21 | (part as u32) << 5 | d);
            }
        }
    }

    fn mov_x(&mut self, d: u32, s: u32) {
        self.put(0xAA00_03E0 | s << 16 | d);
    }

    /// `fmov xd, dn`
    fn fmov_xd(&mut self, d: u32, n: u32) {
        self.put(0x9E66_0000 | n << 5 | d);
    }

    /// `fmov dd, xn`
    fn fmov_dx(&mut self, d: u32, n: u32) {
        self.put(0x9E67_0000 | n << 5 | d);
    }

    /// `fmov dd, dn`
    fn fmov_dd(&mut self, d: u32, n: u32) {
        self.put(0x1E60_4000 | n << 5 | d);
    }

    fn cmp(&mut self, n: u32, m: u32) {
        self.put(0xEB00_001F | m << 16 | n << 5);
    }

    fn cset(&mut self, d: u32, cond: u32) {
        self.put(0x9A9F_07E0 | (cond ^ 1) << 12 | d);
    }

    fn b_cond(&mut self, cond: u32, to: Label) {
        self.at(to, Fixup::Imm19, 0x5400_0000 | cond);
    }

    /// Bytecode register `r`'s slot.
    fn slot(r: u32) -> u32 {
        r * 8
    }

    /// The machine register bytecode register `r` is pinned to, if it is.
    fn pinned(&self, r: u32) -> Option<Loc> {
        self.pins
            .iter()
            .find(|&&(p, _)| p as u32 == r)
            .map(|&(_, m)| m)
    }

    /// `xt = r[r]`
    fn get(&mut self, t: u32, r: u32) {
        match self.pinned(r) {
            Some(Loc::X(m)) if m != t => self.mov_x(t, m),
            Some(Loc::X(_)) => {}
            Some(Loc::D(m)) => self.fmov_xd(t, m),
            None => self.ldr(t, X20, Self::slot(r)),
        }
    }

    /// `r[r] = xt`, into its general register whatever it lives in for this
    /// function: what the fixed registers hold where another function is
    /// entered, after [`Asm::floats_out`].
    fn set_x(&mut self, t: u32, r: u32) {
        match PIN_REGS.get(r as usize) {
            Some(&m) if m != t => self.mov_x(m, t),
            Some(_) => {}
            None => self.str(t, X20, Self::slot(r)),
        }
    }

    /// `r[r] = xt`
    fn set(&mut self, t: u32, r: u32) {
        match self.pinned(r) {
            Some(Loc::X(m)) if m != t => self.mov_x(m, t),
            Some(Loc::X(_)) => {}
            Some(Loc::D(m)) => self.fmov_dx(m, t),
            None => self.str(t, X20, Self::slot(r)),
        }
    }

    /// `dt = r[r]`
    fn get_float(&mut self, t: u32, r: Reg) {
        match self.pinned(r as u32) {
            Some(Loc::D(m)) if m != t => self.fmov_dd(t, m),
            Some(Loc::D(_)) => {}
            Some(Loc::X(m)) => self.fmov_dx(t, m),
            None => self.ldr_d(t, X20, Self::slot(r as u32)),
        }
    }

    /// `r[r] = dt`
    fn set_float(&mut self, t: u32, r: Reg) {
        match self.pinned(r as u32) {
            Some(Loc::D(m)) if m != t => self.fmov_dd(m, t),
            Some(Loc::D(_)) => {}
            Some(Loc::X(m)) => self.fmov_xd(m, t),
            None => self.str_d(t, X20, Self::slot(r as u32)),
        }
    }

    /// `r[r]` as an `x` register: where it is, if that is one, or `scratch`
    /// with it loaded there.
    fn x(&mut self, r: Reg, scratch: u32) -> u32 {
        match self.pinned(r as u32) {
            Some(Loc::X(m)) => m,
            _ => {
                self.get(scratch, r as u32);
                scratch
            }
        }
    }

    /// The `x` register to compute `r[a]` in: its own, or `scratch`, from
    /// which [`Asm::store_from`] puts it where it goes.
    fn x_dst(&self, a: Reg, scratch: u32) -> u32 {
        match self.pinned(a as u32) {
            Some(Loc::X(m)) => m,
            _ => scratch,
        }
    }

    /// `r[r]` as a `d` register, as [`Asm::x`] is.
    fn d(&mut self, r: Reg, scratch: u32) -> u32 {
        match self.pinned(r as u32) {
            Some(Loc::D(m)) => m,
            _ => {
                self.get_float(scratch, r);
                scratch
            }
        }
    }

    /// The `d` register to compute `r[a]` in, as [`Asm::x_dst`] is.
    fn d_dst(&self, a: Reg, scratch: u32) -> u32 {
        match self.pinned(a as u32) {
            Some(Loc::D(m)) => m,
            _ => scratch,
        }
    }

    /// Every fixed register into its general register from memory: what the
    /// machine's entry does, ahead of the warm entry moving floats in.
    fn reload_x(&mut self) {
        for (r, m) in PIN_REGS.iter().enumerate() {
            self.ldr(*m, X20, Self::slot(r as u32));
        }
    }

    /// Every float register in from its general register: the warm entry.
    fn floats_in(&mut self) {
        for (r, m) in self.pins.clone() {
            if let Loc::D(d) = m {
                self.fmov_dx(d, PIN_REGS[r as usize]);
            }
        }
    }

    /// Every float register back out to its general register: before control
    /// goes on to another function, whose warm entry expects them there.
    fn floats_out(&mut self) {
        for (r, m) in self.pins.clone() {
            if let Loc::D(d) = m {
                self.fmov_xd(PIN_REGS[r as usize], d);
            }
        }
    }

    /// Every fixed register, to memory.
    fn spill(&mut self) {
        for (r, m) in self.pins.clone() {
            match m {
                Loc::X(m) => self.str(m, X20, Self::slot(r as u32)),
                Loc::D(m) => self.str_d(m, X20, Self::slot(r as u32)),
            }
        }
    }

    /// Every fixed register, from memory, to where it lives here.
    fn reload(&mut self) {
        for (r, m) in self.pins.clone() {
            match m {
                Loc::X(m) => self.ldr(m, X20, Self::slot(r as u32)),
                Loc::D(m) => self.ldr_d(m, X20, Self::slot(r as u32)),
            }
        }
    }

    /// `c` as an `x` register: where it is, or `scratch` with it put there.
    fn operand(&mut self, c: Operand, scratch: u32) -> u32 {
        match c {
            Operand::Reg(r) => self.x(r, scratch),
            Operand::Imm(n) => {
                self.imm(scratch, n as u64);
                scratch
            }
        }
    }

    /// `c` as the magnitude of a 12-bit immediate, and whether it is
    /// negative, where that is how it is encoded.
    fn short_imm(&self, c: Operand) -> Option<(u32, bool)> {
        match c {
            Operand::Imm(n) if self.short && (0..4096).contains(&n) => Some((n as u32, false)),
            Operand::Imm(n) if self.short && (-4095..0).contains(&n) => Some(((-n) as u32, true)),
            _ => None,
        }
    }

    /// `cmp xn, c`, `xn` not being `x10`.
    fn cmp_with(&mut self, n: u32, c: Operand) {
        match self.short_imm(c) {
            // cmp xn, #k -- or cmn, for a negative one.
            Some((k, false)) => self.put(0xF100_001F | k << 10 | n << 5),
            Some((k, true)) => self.put(0xB100_001F | k << 10 | n << 5),
            None => {
                let m = self.operand(c, X10);
                self.cmp(n, m);
            }
        }
    }

    /// `live = max(live, n)`
    fn raise(&mut self, n: u32) {
        self.ldr(X10, X19, layout::LIVE);
        self.imm(X11, n as u64);
        self.cmp(X10, X11);
        // csel x10, x10, x11, hs
        self.put(0x9A80_0000 | X11 << 16 | HS << 12 | X10 << 5 | X10);
        self.str(X10, X19, layout::LIVE);
    }

    /// `r[a]` was written: raise `live`, unless that is deferred.
    fn raised(&mut self, a: Reg) {
        if !self.defer_live {
            self.raise(a as u32 + 1);
        }
    }

    /// Store `x9` in `r[a]`, raising `live`.
    fn store(&mut self, a: Reg) {
        self.store_from(X9, a);
    }

    /// Store `xd` in `r[a]` -- nothing, if that is where it was computed --
    /// raising `live`.
    fn store_from(&mut self, d: u32, a: Reg) {
        self.set(d, a as u32);
        self.raised(a);
    }

    /// Store `dd` in `r[a]`, raising `live`.
    fn store_float_from(&mut self, d: u32, a: Reg) {
        self.set_float(d, a);
        self.raised(a);
    }

    fn flush_steps(&mut self) {
        self.ldr(X9, X19, layout::STEPS);
        self.put(0x8B00_0000 | X21 << 16 | X9 << 5 | X9); // add x9, x9, x21
        self.str(X9, X19, layout::STEPS);
        self.imm(X21, 0);
    }

    /// On to `out` if this call has made [`super::CHAINS`] jumps into other
    /// functions, counting this one.
    fn budget(&mut self, out: Label) {
        self.put(0x9100_0400 | X22 << 5 | X22); // add x22, x22, #1
        self.put(0xF100_001F | super::CHAINS << 10 | X22 << 5); // cmp x22, #CHAINS
        self.b_cond(HS, out);
    }

    /// Jump to the warm entry of the function whose start is in `x9`.
    fn enter_warm(&mut self) {
        self.put(0x9100_0000 | (Self::WARM as u32) << 10 | X9 << 5 | X9); // add x9, x9, #WARM
        self.put(0xD61F_0000 | X9 << 5); // br x9
    }

    /// Return `status` with the register file as it is in memory.
    fn ret_as_is(&mut self, status: u32) {
        self.flush_steps();
        self.put(0x5280_0000 | status << 5); // movz w0, #status
        self.restore_and_return();
    }

    fn restore_and_return(&mut self) {
        self.put(0xA940_0000 | 10 << 15 | X28 << 10 | SP << 5 | X27); // ldp x27, x28, [sp, #80]
        self.put(0xA940_0000 | 8 << 15 | X26 << 10 | SP << 5 | X25); // ldp x25, x26, [sp, #64]
        self.put(0xA940_0000 | 6 << 15 | X24 << 10 | SP << 5 | X23); // ldp x23, x24, [sp, #48]
        self.ldr(X22, SP, 40);
        self.ldr(X21, SP, 32);
        self.put(0xA940_0000 | 2 << 15 | X20 << 10 | SP << 5 | X19); // ldp x19, x20, [sp, #16]
        self.put(0xA8C0_0000 | 12 << 15 | LR << 10 | SP << 5 | FP); // ldp x29, x30, [sp], #96
        self.put(0xD65F_03C0); // ret
    }

    fn int_cond(c: Cond) -> u32 {
        match c {
            Cond::Eq => EQ,
            Cond::Ne => NE,
            Cond::Lt => LT,
            Cond::Le => LE,
            Cond::Gt => GT,
            Cond::Ge => GE,
        }
    }

    /// The condition true after `fcmp` exactly when `c` holds IEEE's way:
    /// false, unordered, for all but `!=`.
    fn float_cond(c: Cond) -> u32 {
        match c {
            Cond::Eq => EQ,
            Cond::Ne => NE,
            Cond::Lt => MI,
            Cond::Le => LS,
            Cond::Gt => GT,
            Cond::Ge => GE,
        }
    }

    /// `x9 = r[x]`, and on to `slow` unless that is a nursery address; then
    /// `x12` the object's first slot, `x13` its first word, and `w14` its kind.
    /// Reach the object in `r[x]`: `x9` its address, `x12` where its first
    /// slot is in memory, `x13` its first header word and `w14` its kind. A
    /// compact region goes to `slow`.
    ///
    /// Two paths, chosen by the address's generation bit. A nursery object is
    /// one load off the nursery base, as it always was; an old one is the
    /// block-table walk [`Asm::thin_where`] does. It used to be that an old
    /// object went to `slow` here, and *every* object a long-lived structure is
    /// made of is old: `binarytrees` handed 55 million field reads and tag
    /// tests to the interpreter for that, on trees that native code had built.
    /// The branch keeps the young path at its old cost, which matters because
    /// young objects are the majority everywhere else.
    ///
    /// A field or capture is read at an offset from `x12`, which is only right
    /// if it is in the same block as the header. It is: an object bigger than
    /// a line takes a run of lines *within* a block, and one bigger than a
    /// block starts at a block's first slot and takes whole blocks -- and
    /// nothing here reads past the first 4000 slots of one.
    fn nursery(&mut self, x: Reg, slow: Label) {
        self.get(X9, x as u32);
        self.locate(slow);
    }

    /// [`Asm::nursery`], for an address already in `x9`.
    fn locate(&mut self, slow: Label) {
        let old = self.label();
        let found = self.label();
        self.thin_shr(X10, X9, super::addr::GEN_SHIFT);
        self.at(old, Fixup::Imm19, 0xB500_0000 | X10); // cbnz x10, old
        self.ldr(X11, X19, layout::BASE);
        self.put(0x8B00_0000 | X9 << 16 | 3 << 10 | X11 << 5 | X12); // add x12, x11, x9, lsl #3
        self.jump(found);
        self.bind(old);
        // Not a region: bit 31 clear, so the generation bit was the old one.
        self.thin_shr(X10, X9, super::addr::GEN_SHIFT + 1);
        self.at(slow, Fixup::Imm19, 0xB500_0000 | X10); // cbnz x10, slow
        // A frame in the current chunk -- what a return through one is -- is
        // reached from the chunk's base directly, one load rather than three.
        let walk = self.label();
        self.put(0xB940_0000 | (layout::FCUR / 4) << 10 | X19 << 5 | X10); // ldr w10, [x19, #FCUR]
        self.put(0xB940_0000 | (layout::FLIM / 4) << 10 | X19 << 5 | X11); // ldr w11, [x19, #FLIM]
        self.cmp(X9, X10);
        self.b_cond(LO, walk);
        self.cmp(X9, X11);
        self.b_cond(HS, walk);
        self.put(0xCB0A_0000 | X9 << 5 | X10); // sub x10, x9, x10
        self.ldr(X11, X19, layout::FBASE);
        self.put(0x8B00_0000 | X10 << 16 | 3 << 10 | X11 << 5 | X12); // add x12, x11, x10, lsl #3
        self.jump(found);
        self.bind(walk);
        self.thin_where(X12, X9);
        self.bind(found);
        self.ldr(X13, X12, 0);
        self.put(0x1200_1C00 | X13 << 5 | X14); // and w14, w13, #0xff
    }

    /// `cmp w14, #imm`
    fn cmp_kind(&mut self, imm: u32) {
        self.put(0x7100_001F | imm << 10 | X14 << 5);
    }

    /// `x15 = x13 >> 32`: the object's length.
    fn length(&mut self) {
        self.put(0xD360_FC00 | X13 << 5 | X15);
    }

    /// `fcmp dn, dm`
    fn fcmp(&mut self, n: u32, m: u32) {
        self.put(0x1E60_2000 | m << 16 | n << 5);
    }
}

impl Emit for Asm {
    const FIXED: usize = PIN_REGS.len();
    const WARM: usize = 48 + 4 * PIN_REGS.len();

    fn new() -> Asm {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            ret_w0: None,
            defer_live: false,
            short: false,
            pins: Vec::new(),
            chain: false,
        }
    }

    fn offset(&self) -> usize {
        self.code.len()
    }

    fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    fn bind(&mut self, l: Label) {
        self.labels[l.0] = Some(self.code.len());
    }

    fn configure(&mut self, opt: OptLevel, floats: &[Reg]) {
        self.defer_live = opt >= OptLevel::O1;
        self.short = opt >= OptLevel::O1;
        self.chain = opt >= OptLevel::O1;
        let mut ds = FLOAT_PIN_REGS.iter();
        self.pins = PIN_REGS
            .iter()
            .enumerate()
            .map(|(r, &x)| {
                let r = r as Reg;
                match floats.contains(&r).then(|| ds.next()).flatten() {
                    Some(&d) => (r, Loc::D(d)),
                    None => (r, Loc::X(x)),
                }
            })
            .collect();
    }

    fn prologue(&mut self, warm: Option<Label>) {
        let start = self.offset();
        self.put(0xA980_0000 | 0x74 << 15 | LR << 10 | SP << 5 | FP); // stp x29, x30, [sp, #-96]!
        self.put(0x9100_0000 | SP << 5 | FP); // add x29, sp, #0
        self.put(0xA900_0000 | 2 << 15 | X20 << 10 | SP << 5 | X19); // stp x19, x20, [sp, #16]
        self.str(X21, SP, 32);
        self.str(X22, SP, 40);
        self.put(0xA900_0000 | 6 << 15 | X24 << 10 | SP << 5 | X23); // stp x23, x24, [sp, #48]
        self.put(0xA900_0000 | 8 << 15 | X26 << 10 | SP << 5 | X25); // stp x25, x26, [sp, #64]
        self.put(0xA900_0000 | 10 << 15 | X28 << 10 | SP << 5 | X27); // stp x27, x28, [sp, #80]
        self.mov_x(X19, X0);
        self.ldr(X20, X19, layout::REGS);
        self.imm(X21, 0);
        self.imm(X22, 0);
        self.reload_x();
        // A chained function arrives here, with the frame, the machine, its
        // register file, the fixed registers, the steps and the loop count of
        // the one it came from.
        assert_eq!(self.offset() - start, Self::WARM, "the warm entry moved");
        if let Some(warm) = warm {
            self.bind(warm);
        }
        self.floats_in();
        self.ret_w0 = None;
    }

    fn ret(&mut self, status: u32) {
        self.spill();
        self.ret_as_is(status);
    }

    fn chain(&mut self, pc: Pc, live: Option<u32>, to: Option<Label>) {
        if let Some(live) = live {
            self.imm(X9, live as u64);
            self.str(X9, X19, layout::LIVE);
        }
        self.floats_out();
        let out = self.label();
        self.budget(out);
        match to {
            Some(to) => self.jump(to),
            None => {
                self.ldr(X9, X19, layout::NATIVE_TABLE);
                self.imm(X10, pc as u64);
                self.put(0xF860_7800 | X10 << 16 | X9 << 5 | X9); // ldr x9, [x9, x10, lsl #3]
                self.at(out, Fixup::Imm19, 0xB400_0000 | X9); // cbz x9, out
                self.enter_warm();
            }
        }
        self.bind(out);
        self.spill();
        self.imm(X9, pc as u64);
        self.str(X9, X19, layout::PC);
        self.ret_as_is(crate::abi::JUMPED);
    }

    fn leave(&mut self, pc: Pc, live: Option<u32>) {
        self.imm(X9, pc as u64);
        self.str(X9, X19, layout::PC);
        if let Some(live) = live {
            self.imm(X9, live as u64);
            self.str(X9, X19, layout::LIVE);
        }
        self.ret(crate::abi::JUMPED);
    }

    fn end(&mut self) {
        // The interpreter's answers that are not "carry on", for every call
        // in the function: returned as they are.
        if let Some(l) = self.ret_w0.take() {
            self.bind(l);
            self.restore_and_return();
        }
    }

    fn step(&mut self) {
        self.put(0x9100_0400 | X21 << 5 | X21); // add x21, x21, #1
    }

    fn unstep(&mut self) {
        self.put(0xD100_0400 | X21 << 5 | X21); // sub x21, x21, #1
    }

    fn add_steps(&mut self, n: u32) {
        if n < 4096 {
            self.put(0x9100_0000 | n << 10 | X21 << 5 | X21); // add x21, x21, #n
        } else {
            self.imm(X9, n as u64);
            self.put(0x8B00_0000 | X9 << 16 | X21 << 5 | X21); // add x21, x21, x9
        }
    }

    fn jump(&mut self, to: Label) {
        self.at(to, Fixup::B, 0x1400_0000);
    }

    fn mov(&mut self, a: Reg, b: Reg) {
        match (self.pinned(a as u32), self.pinned(b as u32)) {
            (Some(Loc::D(m)), _) => self.get_float(m, b),
            (_, Some(Loc::D(s))) => self.set_float(s, a),
            (Some(Loc::X(m)), _) => self.get(m, b as u32),
            (None, _) => {
                let s = self.x(b, X9);
                self.set(s, a as u32);
            }
        }
        self.raised(a);
    }

    fn word(&mut self, a: Reg, w: u64) {
        let d = self.x_dst(a, X9);
        self.imm(d, w);
        self.store_from(d, a);
    }

    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label) {
        let n = self.x(b, X9);
        let d = self.x_dst(a, X9);
        if let (IntOp::Add | IntOp::Sub, Some((k, negative))) = (op, self.short_imm(c)) {
            // add xd, xn, #k, or sub -- the other, for a negative constant.
            let sub = (op == IntOp::Sub) != negative;
            let base = if sub { 0xD100_0000 } else { 0x9100_0000 };
            self.put(base | k << 10 | n << 5 | d);
            self.store_from(d, a);
            return;
        }
        // A shift by a constant is one instruction: `lsl`, `asr` and `lsr`
        // are `ubfm`/`sbfm` with the right immediates.
        if let (IntOp::Shl | IntOp::Shr | IntOp::Ushr, Operand::Imm(k)) = (op, c)
            && (1..64).contains(&k)
        {
            let k = k as u32;
            let word = match op {
                IntOp::Shl => 0xD340_0000 | ((64 - k) % 64) << 16 | (63 - k) << 10,
                IntOp::Shr => 0x9340_0000 | k << 16 | 63 << 10,
                _ => 0xD340_0000 | k << 16 | 63 << 10,
            };
            self.put(word | n << 5 | d);
            self.store_from(d, a);
            return;
        }
        let m = self.operand(c, X10);
        match op {
            IntOp::Add => self.put(0x8B00_0000 | m << 16 | n << 5 | d),
            IntOp::Sub => self.put(0xCB00_0000 | m << 16 | n << 5 | d),
            IntOp::Mul => self.put(0x9B00_7C00 | m << 16 | n << 5 | d),
            // lslv / asrv xd, xn, xm -- both take the count modulo 64, which
            // is what `wrapping_shl` does, so the interpreter and this agree
            // without a range check.
            IntOp::Shl => self.put(0x9AC0_2000 | m << 16 | n << 5 | d),
            IntOp::Shr => self.put(0x9AC0_2800 | m << 16 | n << 5 | d),
            IntOp::Ushr => self.put(0x9AC0_2400 | m << 16 | n << 5 | d), // lsrv
            IntOp::And => self.put(0x8A00_0000 | m << 16 | n << 5 | d),
            IntOp::Div | IntOp::Rem => {
                self.at(zero, Fixup::Imm19, 0xB400_0000 | m); // cbz xm, zero
                // `sdiv` wraps, as `i64::wrapping_div` does: MIN / -1 is MIN.
                if op == IntOp::Div {
                    self.put(0x9AC0_0C00 | m << 16 | n << 5 | d); // sdiv xd, xn, xm
                } else {
                    self.put(0x9AC0_0C00 | m << 16 | n << 5 | X11); // sdiv x11, xn, xm
                    // msub xd, x11, xm, xn: xn - x11 * xm
                    self.put(0x9B00_8000 | m << 16 | n << 10 | X11 << 5 | d);
                }
            }
        }
        self.store_from(d, a);
    }

    fn unary(&mut self, op: UnaryOp, a: Reg, b: Reg) {
        let n = self.x(b, X9);
        match op {
            // scvtf dd, xn
            UnaryOp::ToFloat => {
                let d = self.d_dst(a, 0);
                self.put(0x9E62_0000 | n << 5 | d);
                self.store_float_from(d, a);
            }
            // No scalar popcount before ARMv8.9, so through the vector unit,
            // as every compiler does it: fmov d0, xn; cnt v0.8b, v0.8b; addv
            // b0, v0.8b; fmov wd, s0.
            UnaryOp::PopCount => {
                self.fmov_dx(0, n);
                self.put(0x0E20_5800);
                self.put(0x0E31_B800);
                let d = self.x_dst(a, X9);
                self.put(0x1E26_0000 | d);
                self.store_from(d, a);
            }
        }
    }

    fn float(&mut self, op: FloatOp, a: Reg, b: Reg, c: Reg) {
        let n = self.d(b, 0);
        let m = self.d(c, 1);
        let d = self.d_dst(a, 0);
        let base = match op {
            FloatOp::Add => 0x1E60_2800,
            FloatOp::Sub => 0x1E60_3800,
            FloatOp::Mul => 0x1E60_0800,
            FloatOp::Div => 0x1E60_1800,
        };
        self.put(base | m << 16 | n << 5 | d); // dd = dn op dm
        self.store_float_from(d, a);
    }

    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand) {
        let n = self.x(b, X9);
        self.cmp_with(n, c);
        let d = self.x_dst(a, X9);
        self.cset(d, Self::int_cond(cond));
        self.store_from(d, a);
    }

    fn cmp_float(&mut self, cond: Cond, a: Reg, b: Reg, c: Reg) {
        let n = self.d(b, 0);
        let m = self.d(c, 1);
        self.fcmp(n, m);
        let d = self.x_dst(a, X9);
        self.cset(d, Self::float_cond(cond));
        self.store_from(d, a);
    }

    fn branch_int(&mut self, cond: Cond, x: Reg, y: Operand, to: Label) {
        let n = self.x(x, X9);
        self.cmp_with(n, y);
        self.b_cond(Self::int_cond(cond) ^ 1, to);
    }

    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label) {
        let n = self.d(x, 0);
        let m = self.d(y, 1);
        self.fcmp(n, m);
        self.b_cond(Self::float_cond(cond) ^ 1, to);
    }

    fn branch_zero(&mut self, x: Reg, to: Label) {
        let n = self.x(x, X9);
        self.at(to, Fixup::Imm19, 0xB400_0000 | n);
    }

    fn set_live(&mut self, n: u32) {
        self.imm(X9, n as u64);
        self.str(X9, X19, layout::LIVE);
    }

    fn raise_live_to(&mut self, n: u32) {
        self.raise(n);
    }

    fn back_edge(&mut self, to: Label, over: Label) {
        self.put(0x9100_0400 | X22 << 5 | X22); // add x22, x22, #1
        const { assert!(super::BACK_EDGES == 1 << 12) };
        self.put(0xF140_041F | X22 << 5); // cmp x22, #1, lsl #12
        self.b_cond(HS, over);
        self.jump(to);
    }

    fn tag_test(&mut self, x: Reg, tag: u32, miss: Label, slow: Label) {
        self.nursery(x, slow);
        self.cmp_kind(crate::heap::Kind::Data as u32);
        self.b_cond(NE, miss);
        self.put(0xB940_0000 | 2 << 10 | X12 << 5 | X14); // ldr w14, [x12, #8]: meta
        self.imm(X10, tag as u64);
        self.put(0x6B00_001F | X10 << 16 | X14 << 5); // cmp w14, w10
        self.b_cond(NE, miss);
    }

    fn steps(&mut self, run: &[thin::Step], slow: Label) {
        use thin::Step;
        debug_assert!(thin::temporaries(run) <= thin::TEMPORARIES);
        for s in run {
            let dst = |t: u8| TMP + t as u32;
            match *s {
                Step::Set(t, src) => self.thin_src(dst(t), src),
                Step::Add(t, a, b) => {
                    let n = self.thin_operand(a, WORK_A);
                    match small_imm(b) {
                        Some(k) => self.put(0x9100_0000 | k << 10 | n << 5 | dst(t)),
                        None => {
                            let m = self.thin_operand(b, WORK_B);
                            self.put(0x8B00_0000 | m << 16 | n << 5 | dst(t));
                        }
                    }
                }
                Step::And(t, a, mask) => {
                    let n = self.thin_operand(a, WORK_A);
                    match low_bits(mask) {
                        // and xd, xn, #(2^k - 1): N = 1, immr = 0, imms = k - 1.
                        Some(k) => self.put(0x9240_0000 | (k - 1) << 10 | n << 5 | dst(t)),
                        None => {
                            self.imm(WORK_B, mask);
                            self.put(0x8A00_0000 | WORK_B << 16 | n << 5 | dst(t));
                        }
                    }
                }
                Step::Shr(t, a, n) => {
                    let a = self.thin_operand(a, WORK_A);
                    self.thin_shr(dst(t), a, n);
                }
                Step::ShrBy(t, a, b) => {
                    let n = self.thin_operand(a, WORK_A);
                    let m = self.thin_operand(b, WORK_B);
                    self.put(0x9AC0_2400 | m << 16 | n << 5 | dst(t)); // lsr xt, xn, xm
                }
                Step::Load(t, at) => {
                    let at = self.thin_operand(at, WORK_A);
                    self.thin_where(WORK_B, at);
                    self.ldr(dst(t), WORK_B, 0);
                }
                Step::Store(at, v) => {
                    let at = self.thin_operand(at, WORK_A);
                    self.thin_where(WORK_B, at);
                    let v = self.thin_operand(v, WORK_A);
                    self.str(v, WORK_B, 0);
                }
                Step::Locate(t, at) => {
                    let at = self.thin_operand(at, WORK_A);
                    self.thin_where(dst(t), at);
                }
                Step::LoadAt(t, base, off) => {
                    let b = self.thin_operand(base, WORK_A);
                    match off {
                        thin::Src::Imm(k) if k < 4096 => self.ldr(dst(t), b, k as u32 * 8),
                        _ => {
                            let o = self.thin_operand(off, WORK_B);
                            self.ldr_idx(dst(t), b, o);
                        }
                    }
                }
                Step::StoreAt(base, off, v) => {
                    let b = self.thin_operand(base, WORK_A);
                    let o = self.thin_operand(off, WORK_B);
                    // The value last: `WORK_A` and `WORK_B` are taken, and
                    // it may be a temporary or a fixed register of its own.
                    let v = self.thin_operand(v, X16);
                    self.put(0xF820_7800 | o << 16 | b << 5 | v); // str xv, [xb, xo, lsl #3]
                }
                // Branch away on the *negation*: the guard holding is the
                // ordinary case and falls through. Unsigned throughout, which
                // is what makes one `u <` reject a negative index as well as
                // one past the end.
                Step::Guard(c, a, b) => {
                    // `a < 2^k` is "no bit at or above k", which is a shift and
                    // a test rather than a constant to build and compare
                    // against. The one that matters is the first guard of every
                    // expansion, whose bound is 2^31 and needs two instructions
                    // to materialize.
                    if let (Cond::Lt, thin::Src::Imm(w)) = (c, b)
                        && let Some(k) = log2(w)
                    {
                        let n = self.thin_operand(a, WORK_A);
                        self.thin_shr(WORK_B, n, k);
                        self.at(slow, Fixup::Imm19, 0xB500_0000 | WORK_B); // cbnz
                        continue;
                    }
                    let n = self.thin_operand(a, WORK_A);
                    match small_imm(b) {
                        // cmp xn, #imm, which is subs xzr, xn, #imm.
                        Some(k) => self.put(0xF100_001F | k << 10 | n << 5),
                        None => {
                            let m = self.thin_operand(b, WORK_B);
                            self.cmp(n, m);
                        }
                    }
                    let fails = match c {
                        Cond::Eq => NE,
                        Cond::Ne => EQ,
                        Cond::Lt => HS,
                        Cond::Le => HI,
                        Cond::Gt => LS,
                        Cond::Ge => LO,
                    };
                    self.b_cond(fails, slow);
                }
                Step::Put(r, src) => {
                    let v = self.thin_operand(src, WORK_A);
                    self.set(v, r as u32);
                }
            }
        }
    }

    fn field(&mut self, a: Reg, b: Reg, i: u32, slow: Label) {
        if i >= 4000 {
            self.jump(slow);
            return;
        }
        self.nursery(b, slow);
        self.cmp_kind(crate::heap::Kind::Array as u32);
        self.b_cond(HI, slow);
        self.length();
        self.put(0xF100_001F | i << 10 | X15 << 5); // cmp x15, #i
        self.b_cond(LS, slow);
        let two_words = self.label();
        self.put(0xF274_001F | X13 << 5); // tst x13, #UNIFORM (bit 12)
        self.b_cond(NE, two_words);
        self.put(0xF100_201F | X15 << 5); // cmp x15, #8
        self.b_cond(HI, slow);
        self.bind(two_words);
        self.ldr(X9, X12, (2 + i) * 8);
        self.store(a);
    }

    fn vector_loop(&mut self, plan: &Plan, scalar: Label) {
        // Vector registers: `v0`.. for the lanes; above the function's float
        // homes, two per array (where it is, how long it is) and one per
        // broadcast. Not enough of them, and there is no vector loop.
        let homes = self
            .pins
            .iter()
            .filter(|(_, m)| matches!(m, Loc::D(_)))
            .count() as u32;
        let mut next = 16 + homes;
        let mut take = || {
            next += 1;
            (next - 1 <= 31).then_some(next - 1)
        };
        let mut bases = Vec::new();
        let mut lens = Vec::new();
        for _ in &plan.arrays {
            let (Some(b), Some(l)) = (take(), take()) else {
                return;
            };
            bases.push(b);
            lens.push(l);
        }
        let mut casts = Vec::new();
        for _ in &plan.broadcasts {
            let Some(v) = take() else { return };
            casts.push(v);
        }
        let j = PIN_REGS[plan.j as usize];
        let lane = |l: Lanes| match l {
            Lanes::V(n) => n as u32,
            Lanes::B(n) => casts[n as usize],
        };

        // --- the preheader ---------------------------------------------------
        // Nothing to do at all: the scalar header exits.
        self.cmp_with(j, plan.hi);
        self.b_cond(GE, scalar);
        for (k, a) in plan.arrays.iter().enumerate() {
            self.get(X9, a.reg as u32);
            // Not a region, which is looked up by a search.
            self.thin_shr(X10, X9, super::addr::GEN_SHIFT + 1);
            self.at(scalar, Fixup::Imm19, 0xB500_0000 | X10); // cbnz x10, scalar
            self.thin_where(X12, X9);
            self.ldr(X13, X12, 0);
            self.put(0x1200_1C00 | X13 << 5 | X14); // and w14, w13, #0xff
            self.cmp_kind(a.kind as u32);
            self.b_cond(NE, scalar);
            self.put(0xF274_001F | X13 << 5); // tst x13, #UNIFORM
            self.b_cond(EQ, scalar);
            self.ubfx(X14, X13, thin::DESC_SHIFT, 4);
            self.put(0xF100_001F | (super::vector::ELEMENT as u32) << 10 | X14 << 5); // cmp x14, #FLOAT
            self.b_cond(NE, scalar);
            self.thin_shr(X15, X13, thin::LEN_SHIFT);
            self.fmov_dx(lens[k], X15);
            self.fmov_dx(bases[k], X12);
        }
        for &(a, b) in &plan.distinct {
            let x = self.x(plan.arrays[a].reg, X9);
            let y = self.x(plan.arrays[b].reg, X10);
            self.cmp(x, y);
            self.b_cond(EQ, scalar);
        }
        // Bounds, for the whole loop: the body's index arithmetic, run at
        // `j`'s first value and its last, and every index checked against
        // its array where the access happens -- the register is reused
        // after. An index is `j` plus an invariant, so those are its
        // extremes; unsigned, so a negative one fails too.
        let scratch = self.label();
        for last in [false, true] {
            if last {
                self.mov_x(X17, j);
                match plan.hi {
                    Operand::Reg(h) => self.put(0xD100_0400 | PIN_REGS[h as usize] << 5 | j), // sub j, hi, #1
                    Operand::Imm(k) => self.imm(j, (k - 1) as u64),
                }
            }
            for op in &plan.ops {
                match op {
                    VOp::Scalar(i) => self.scalar_op(*i, scratch),
                    VOp::Const(r, w) => self.word(*r, *w),
                    VOp::Load { array, idx, .. } | VOp::Store { array, idx, .. } => {
                        self.fmov_xd(X9, lens[*array]);
                        self.cmp(PIN_REGS[*idx as usize], X9);
                        self.b_cond(HS, scalar);
                    }
                    VOp::Float { .. } => {}
                }
            }
            if last {
                self.mov_x(j, X17);
            }
        }
        self.bind(scratch);
        // Where each array is, in a general register for the loop: `x12` to
        // `x17` are free in it, since nothing in the body walks a table or
        // looks up a method. Past six arrays the rest stay in their `d`s.
        const BASE_REGS: [u32; 6] = [X12, X13, X14, X15, X16, X17];
        let base_x: Vec<Option<u32>> = (0..plan.arrays.len())
            .map(|k| BASE_REGS.get(k).copied())
            .collect();
        for (k, x) in base_x.iter().enumerate() {
            if let Some(x) = x {
                self.fmov_xd(*x, bases[k]);
            }
        }
        for (k, &r) in plan.broadcasts.iter().enumerate() {
            match self.pinned(r as u32) {
                Some(Loc::D(m)) => self.put(0x4E08_0400 | m << 5 | casts[k]), // dup v.2d, vm.d[0]
                _ => {
                    self.get(X9, r as u32);
                    self.put(0x4E08_0C00 | X9 << 5 | casts[k]); // dup v.2d, x9
                }
            }
        }

        // --- two iterations a trip, while two remain ------------------------
        let top = self.label();
        let done = self.label();
        self.bind(top);
        self.put(0x9100_0800 | j << 5 | X9); // add x9, j, #2
        self.cmp_with(X9, plan.hi);
        self.b_cond(GT, done);
        for op in &plan.ops {
            match op {
                VOp::Scalar(i) => self.scalar_op(*i, done),
                VOp::Const(r, w) => self.word(*r, *w),
                VOp::Load { lane, array, idx } => {
                    let b = self.array_base(base_x[*array], bases[*array]);
                    let i = PIN_REGS[*idx as usize];
                    self.put(0x8B00_0000 | i << 16 | 3 << 10 | b << 5 | X9); // add x9, xb, xi, lsl #3
                    self.put(0x3DC0_0400 | X9 << 5 | *lane as u32); // ldr q, [x9, #16]
                }
                VOp::Store { array, idx, val } => {
                    let b = self.array_base(base_x[*array], bases[*array]);
                    let i = PIN_REGS[*idx as usize];
                    self.put(0x8B00_0000 | i << 16 | 3 << 10 | b << 5 | X9);
                    self.put(0x3D80_0400 | X9 << 5 | lane(*val)); // str q, [x9, #16]
                }
                VOp::Float { op, lane: d, a, b } => {
                    let base = match op {
                        FloatOp::Mul => 0x6E60_DC00,
                        FloatOp::Add => 0x4E60_D400,
                        FloatOp::Sub => 0x4EE0_D400,
                        FloatOp::Div => 0x6E60_FC00,
                    };
                    self.put(base | lane(*b) << 16 | lane(*a) << 5 | *d as u32); // fop v.2d
                }
            }
        }
        self.put(0x9100_0800 | j << 5 | j); // add j, j, #2
        self.add_steps(2 * plan.steps);
        self.put(0x9100_0400 | X22 << 5 | X22); // add x22, x22, #1
        self.put(0xF140_041F | X22 << 5); // cmp x22, #1, lsl #12
        self.b_cond(HS, done);
        self.jump(top);
        self.bind(done);
    }

    fn stub(&mut self, captures: u32, params: u32, warm: Label) -> bool {
        let argc = params - captures;
        // The arguments up past the captures, highest first, so that none is
        // overwritten before it has moved.
        for j in (0..argc).rev() {
            self.mov_x(PIN_REGS[(captures + j) as usize], PIN_REGS[j as usize]);
        }
        // The captures follow the header, which is longer than two words past
        // `compact::INLINE_DESCS` of them: see `object::write_header`.
        let hdr = meadow_core::compact::header_slots(false, captures as usize) as u32;
        for i in 0..captures {
            self.ldr(PIN_REGS[i as usize], X12, (hdr + i) * 8);
        }
        self.imm(X9, params as u64);
        self.str(X9, X19, layout::LIVE);
        self.jump(warm);
        true
    }

    fn invoke(&mut self, obj: Reg, method: u8, base: Reg, argc: u32, slow: Label) {
        if argc > 247 {
            self.jump(slow);
            return;
        }
        // Whichever way the call goes, the next function expects the fixed
        // registers in their general registers.
        self.floats_out();
        self.get(X9, obj as u32);
        // On the general path the object's address is kept past the
        // rebuilding of the register file, in the scratch slot after the
        // arguments, for the pop at the end.
        let saved = (crate::vm::SCRATCH as u32 + argc) * 8;
        let object = self.label();
        let generic = self.label();
        let generic_frame = self.label();
        // A return: the object is in the current chunk of the frame stack.
        // Everything in a stack chunk is a frame, so its kind needs no test;
        // its slot is one load from the chunk's base; its return pc is its
        // `meta`, and its one method is #0, so there is no table to look up.
        // A frame in a chunk below has chunks to release on the way down,
        // which the interpreter does: it fails the kind test below.
        self.put(0xB940_0000 | (layout::FCUR / 4) << 10 | X19 << 5 | X10); // ldr w10, [x19, #FCUR]
        self.put(0xB940_0000 | (layout::FLIM / 4) << 10 | X19 << 5 | X11); // ldr w11, [x19, #FLIM]
        self.cmp(X9, X10);
        self.b_cond(LO, object);
        self.cmp(X9, X11);
        self.b_cond(HS, object);
        self.put(0xCB0A_0000 | X9 << 5 | X10); // sub x10, x9, x10
        self.ldr(X11, X19, layout::FBASE);
        self.put(0x8B00_0000 | X10 << 16 | 3 << 10 | X11 << 5 | X12); // add x12, x11, x10, lsl #3
        self.put(0xB940_0000 | 2 << 10 | X12 << 5 | X17); // ldr w17, [x12, #8]
        if self.chain {
            // Returning pops the frame: the stack's top goes back to its
            // slot. Then on to the method entry, as for a call below.
            self.method_entry(generic_frame);
            self.put(0xB900_0000 | (layout::FSP / 4) << 10 | X19 << 5 | X9); // str w9, [x19, #FSP]
            self.args_down(base, argc);
            self.put(0xD61F_0000 | X16 << 5); // br x16
        }
        // The general path wants the header and the length, which a return
        // did not need.
        self.bind(generic_frame);
        self.ldr(X13, X12, 0);
        self.length();
        self.jump(generic);
        // A call: a closure, wherever it is.
        self.bind(object);
        self.locate(slow);
        self.cmp_kind(crate::heap::Kind::Closure as u32);
        self.b_cond(NE, slow);
        self.length();
        // The method's pc: table `meta`, entry `method`, if it has one.
        self.put(0xB940_0000 | 2 << 10 | X12 << 5 | X14); // ldr w14, [x12, #8]
        self.ldr(X16, X19, layout::METHOD_STARTS);
        self.put(0xB860_7800 | X14 << 16 | X16 << 5 | X10); // ldr w10, [x16, x14, lsl #2]
        self.put(0x9100_0400 | X14 << 5 | X14); // add x14, x14, #1
        self.put(0xB860_7800 | X14 << 16 | X16 << 5 | X11); // ldr w11, [x16, x14, lsl #2]
        self.put(0xCB00_0000 | X10 << 16 | X11 << 5 | X11); // sub x11, x11, x10
        self.put(0xF100_001F | (method as u32) << 10 | X11 << 5); // cmp x11, #method
        self.b_cond(LS, slow);
        self.put(0x9100_0000 | (method as u32) << 10 | X10 << 5 | X10); // add x10, x10, #method
        self.ldr(X16, X19, layout::METHOD_PCS);
        self.put(0xB860_7800 | X10 << 16 | X16 << 5 | X17); // ldr w17, [x16, x10, lsl #2]
        if self.chain {
            self.method_entry(generic);
            self.args_down(base, argc);
            self.put(0xD61F_0000 | X16 << 5); // br x16
        }
        self.bind(generic);
        // The general path: the register file rebuilt in memory, as the
        // interpreter does it -- the arguments out of the way, the captures
        // in, the arguments after. The captures follow a header of `x14`
        // words: two, and one more for every sixteen fields past the eighth,
        // whose descriptors no longer fit the second word (see
        // `object::write_header`; a closure and a frame are never uniform).
        self.spill();
        self.str(X9, X20, saved);
        self.put(0xD100_0000 | 8 << 10 | X15 << 5 | X14); // sub x14, x15, #8
        self.put(0xF100_001F | X14 << 5); // cmp x14, #0
        self.put(0x9A80_0000 | X14 << 16 | LT << 12 | 31 << 5 | X14); // csel x14, xzr, x14, lt
        self.put(0x9100_0000 | 15 << 10 | X14 << 5 | X14); // add x14, x14, #15
        self.thin_shr(X14, X14, 4);
        self.put(0x9100_0000 | 2 << 10 | X14 << 5 | X14); // add x14, x14, #2
        let scratch = crate::vm::SCRATCH as u32;
        for j in 0..argc {
            self.ldr(X9, X20, Self::slot(base as u32 + j));
            self.str(X9, X20, (scratch + j) * 8);
        }
        let top = self.label();
        let done = self.label();
        self.imm(X10, 0);
        self.bind(top);
        self.cmp(X10, X15);
        self.b_cond(HS, done);
        self.put(0x8B00_0000 | X14 << 16 | X10 << 5 | X11); // add x11, x10, x14
        self.put(0xF860_7800 | X11 << 16 | X12 << 5 | X9); // ldr x9, [x12, x11, lsl #3]
        self.put(0xF820_7800 | X10 << 16 | X20 << 5 | X9); // str x9, [x20, x10, lsl #3]
        self.put(0x9100_0400 | X10 << 5 | X10); // add x10, x10, #1
        self.jump(top);
        self.bind(done);
        for j in 0..argc {
            self.ldr(X9, X20, (scratch + j) * 8);
            self.put(0x9100_0000 | j << 10 | X15 << 5 | X11); // add x11, x15, #j
            self.put(0xF820_7800 | X11 << 16 | X20 << 5 | X9); // str x9, [x20, x11, lsl #3]
        }
        self.put(0x9100_0000 | argc << 10 | X15 << 5 | X9); // add x9, x15, #argc
        self.str(X9, X19, layout::LIVE);
        // Returning through a frame pops it: the stack's top goes back to the
        // frame's own slot. A closure's address is not in the current chunk
        // and is left alone.
        let kept = self.label();
        self.ldr(X9, X20, saved);
        self.chunk_of_current(X10, X11);
        self.thin_shr(X11, X9, super::addr::BLOCK_SHIFT);
        self.cmp(X10, X11);
        self.b_cond(NE, kept);
        self.put(0xB900_0000 | (layout::FSP / 4) << 10 | X19 << 5 | X9); // str w9, [x19, #FSP]
        self.bind(kept);
        if self.chain {
            let out = self.label();
            self.budget(out);
            self.ldr(X9, X19, layout::NATIVE_TABLE);
            self.put(0xF860_7800 | X17 << 16 | X9 << 5 | X9); // ldr x9, [x9, x17, lsl #3]
            self.at(out, Fixup::Imm19, 0xB400_0000 | X9); // cbz x9, out
            // The register file was rebuilt in memory: the next function
            // expects it in the fixed registers.
            self.reload_x();
            self.enter_warm();
            self.bind(out);
        }
        self.str(X17, X19, layout::PC);
        self.ret_as_is(crate::abi::JUMPED);
    }

    fn alloc(&mut self, a: Reg, header: &[u64], fields: &[Reg], slow: Label) {
        let hdr = header.len() as u32;
        let n = fields.len() as u32;
        let size = hdr + n;
        self.ldr(X9, X19, layout::TOP);
        self.put(0x9100_0000 | size << 10 | X9 << 5 | X10); // add x10, x9, #size
        self.ldr(X11, X19, layout::CAP);
        self.cmp(X10, X11);
        self.b_cond(HI, slow);
        // A heap that has put enough into regions wants a collection first.
        self.ldr(X12, X19, layout::REGION_GROWTH);
        self.cmp(X12, X11);
        self.b_cond(HI, slow);
        self.ldr(X13, X19, layout::BASE);
        self.put(0x8B00_0000 | X9 << 16 | 3 << 10 | X13 << 5 | X14); // add x14, x13, x9, lsl #3
        for (k, &w) in header.iter().enumerate() {
            self.imm(X15, w);
            self.str(X15, X14, k as u32 * 8);
        }
        for (j, &r) in (0..).zip(fields) {
            let s = self.x(r, X15);
            self.str(s, X14, (hdr + j) * 8);
        }
        self.str(X10, X19, layout::TOP);
        self.ldr(X15, X19, layout::ALLOCATED);
        self.put(0x9100_0000 | size << 10 | X15 << 5 | X15); // add x15, x15, #size
        self.str(X15, X19, layout::ALLOCATED);
        self.store(a);
    }

    fn frame(&mut self, a: Reg, header: &[u64], fields: &[Reg], slow: Label) {
        let hdr = header.len() as u32;
        let n = fields.len() as u32;
        let size = hdr + n;
        // w9 = fsp, w10 = fsp + size; over the chunk's end -- or no chunk yet,
        // when the end is zero -- is the interpreter's to sort out.
        self.put(0xB940_0000 | (layout::FSP / 4) << 10 | X19 << 5 | X9); // ldr w9, [x19, #FSP]
        self.put(0x1100_0000 | size << 10 | X9 << 5 | X10); // add w10, w9, #size
        self.put(0xB940_0000 | (layout::FLIM / 4) << 10 | X19 << 5 | X11); // ldr w11, [x19, #FLIM]
        self.cmp(X10, X11);
        self.b_cond(HI, slow);
        // The frame's slot in memory: x14 = fbase + (fsp - fcur) * 8.
        self.put(0xB940_0000 | (layout::FCUR / 4) << 10 | X19 << 5 | X12); // ldr w12, [x19, #FCUR]
        self.put(0xCB0C_0000 | X9 << 5 | X12); // sub x12, x9, x12
        self.ldr(X13, X19, layout::FBASE);
        self.put(0x8B00_0000 | X12 << 16 | 3 << 10 | X13 << 5 | WORK_A); // add x14, x13, x12, lsl #3
        for (k, &w) in header.iter().enumerate() {
            self.imm(X15, w);
            self.str(X15, WORK_A, k as u32 * 8);
        }
        for (j, &r) in (0..).zip(fields) {
            let s = self.x(r, X15);
            self.str(s, WORK_A, (hdr + j) * 8);
        }
        self.put(0xB900_0000 | (layout::FSP / 4) << 10 | X19 << 5 | X10); // str w10, [x19, #FSP]
        self.store(a);
    }

    fn exec(&mut self, pc: Pc) {
        self.flush_steps();
        self.spill();
        self.mov_x(X0, X19);
        self.put(0x5280_0000 | (pc & 0xFFFF) << 5 | X1); // movz w1
        if pc >> 16 != 0 {
            self.put(0x72A0_0000 | (pc >> 16) << 5 | X1); // movk w1, lsl 16
        }
        self.ldr(X16, X19, layout::EXEC);
        self.put(0xD63F_0000 | X16 << 5); // blr x16
        let ret = match self.ret_w0 {
            Some(l) => l,
            None => {
                let l = self.label();
                self.ret_w0 = Some(l);
                l
            }
        };
        self.at(ret, Fixup::Imm19, 0x3500_0000 | X0); // cbnz w0, ret
        self.reload();
    }

    fn reaches(&self) -> bool {
        self.fixups
            .iter()
            .all(|&(at, l, f)| match self.labels[l.0] {
                Some(to) => f.reaches((to as i64 - at as i64) / 4),
                None => true,
            })
    }

    fn finish(mut self) -> Vec<u8> {
        for (at, l, f) in std::mem::take(&mut self.fixups) {
            let to = self.labels[l.0].expect("every label is bound");
            let delta = (to as i64 - at as i64) / 4;
            let word = u32::from_le_bytes(self.code[at..at + 4].try_into().expect("four bytes"));
            let word = match f {
                Fixup::B => {
                    assert!(f.reaches(delta), "a branch too far");
                    word | (delta as u32 & 0x03FF_FFFF)
                }
                Fixup::Imm19 => {
                    assert!(f.reaches(delta), "a branch too far");
                    word | (delta as u32 & 0x7FFFF) << 5
                }
            };
            self.code[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.code
    }
}

/// Unsigned lower, which the conditions above do not otherwise need.
/// `src` as an `imm12`, which `add` and `cmp` take directly.
fn small_imm(src: thin::Src) -> Option<u32> {
    match src {
        thin::Src::Imm(w) if w < 4096 => Some(w as u32),
        _ => None,
    }
}

/// `k` where `mask` is the low `k` bits, which `and` takes as a logical
/// immediate. Every mask an expansion uses is one of these -- a kind byte, a
/// descriptor nibble, a flag -- so the general encoding is not worth writing.
fn low_bits(mask: u64) -> Option<u32> {
    (mask != 0 && mask != u64::MAX && (mask & (mask + 1)) == 0).then(|| mask.count_ones())
}

/// `k` where `w` is `2^k`, and `k` is a shift a 64-bit register can take.
fn log2(w: u64) -> Option<u32> {
    (w != 0 && w & (w - 1) == 0 && w.trailing_zeros() < 64).then(|| w.trailing_zeros())
}

const LO: u32 = 3;

/// The scratch registers a run of thin steps keeps its temporaries in, and the
/// two the emitter works in. See [`crate::codegen::thin`].
const TMP: u32 = X9;
const WORK_A: u32 = X14;
const WORK_B: u32 = X15;

impl Asm {
    /// `xd = src`.
    /// Where `src` already is, or `scratch` with it put there.
    ///
    /// A temporary is a register of its own, and a pinned bytecode register is
    /// in one too, so neither has to be moved anywhere first. That matters
    /// because a run of steps is a dozen of these: moving each one into a
    /// working register cost more instructions than the work did.
    fn thin_operand(&mut self, src: thin::Src, scratch: u32) -> u32 {
        match src {
            thin::Src::Tmp(t) => TMP + t as u32,
            thin::Src::Reg(r) => self.x(r, scratch),
            thin::Src::Imm(w) => {
                self.imm(scratch, w);
                scratch
            }
        }
    }

    fn thin_src(&mut self, d: u32, src: thin::Src) {
        match src {
            thin::Src::Reg(r) => self.get(d, r as u32),
            thin::Src::Tmp(t) => self.mov_x(d, TMP + t as u32),
            thin::Src::Imm(w) => self.imm(d, w),
        }
    }

    /// `xd = xn >> sh`, unsigned: `lsr`, which is `ubfm xd, xn, #sh, #63`.
    fn thin_shr(&mut self, d: u32, n: u32, sh: u32) {
        self.put(0xD340_0000 | sh << 16 | 63 << 10 | n << 5 | d);
    }

    /// `xd = ` the block number of the frame stack's current chunk, from its
    /// end: `(flim - 1) >> BLOCK_SHIFT`, which no address matches when there is
    /// no chunk yet. `xt` is scratch.
    fn chunk_of_current(&mut self, d: u32, t: u32) {
        self.put(0xB940_0000 | (layout::FLIM / 4) << 10 | X19 << 5 | t); // ldr wt, [x19, #FLIM]
        self.put(0xD100_0400 | t << 5 | t); // sub xt, xt, #1
        self.thin_shr(d, t, super::addr::BLOCK_SHIFT);
    }

    /// The general register holding an array's base in a vector loop: its
    /// own, or `x9` with it moved there from the `d` register that keeps it.
    fn array_base(&mut self, x: Option<u32>, d: u32) -> u32 {
        match x {
            Some(x) => x,
            None => {
                self.fmov_xd(X9, d);
                X9
            }
        }
    }

    /// One integer instruction of a vector loop's body, as the scalar code
    /// does it. Division does not qualify for a plan, so `zero` is never
    /// taken.
    fn scalar_op(&mut self, i: Instr, zero: Label) {
        let k = Operand::Imm(i.imm as i32 as i64);
        match i.op {
            Op::Move => self.mov(i.a, i.b),
            Op::AddIK => self.int(IntOp::Add, i.a, i.b, k, zero),
            Op::SubIK => self.int(IntOp::Sub, i.a, i.b, k, zero),
            Op::ShlIK => self.int(IntOp::Shl, i.a, i.b, k, zero),
            Op::MulIK => self.int(IntOp::Mul, i.a, i.b, k, zero),
            Op::AddI => self.int(IntOp::Add, i.a, i.b, Operand::Reg(i.c), zero),
            Op::SubI => self.int(IntOp::Sub, i.a, i.b, Operand::Reg(i.c), zero),
            Op::MulI => self.int(IntOp::Mul, i.a, i.b, Operand::Reg(i.c), zero),
            _ => unreachable!("a plan holds only what `vector::plan` admits"),
        }
    }

    /// `x16 = ` the method entry for the pc in `w17`, or on to `none` if it
    /// has none, or this call has made [`super::CHAINS`] jumps already. The
    /// table is there whenever native code runs at all. After it, the entry
    /// wants the arguments in `r[0..argc]`, in registers, and the object's
    /// first word's address in `x12`; it loads the captures itself.
    fn method_entry(&mut self, none: Label) {
        self.ldr(X16, X19, layout::NATIVE_METHODS);
        self.put(0xF860_7800 | X17 << 16 | X16 << 5 | X16); // ldr x16, [x16, x17, lsl #3]
        self.at(none, Fixup::Imm19, 0xB400_0000 | X16); // cbz x16, none
        self.budget(none);
    }

    /// The `argc` arguments at `r[base]` down to `r[0..argc]`, lowest first:
    /// with `base` above zero every one moves below where it was, so none is
    /// overwritten before it is read.
    fn args_down(&mut self, base: Reg, argc: u32) {
        if base == 0 {
            return;
        }
        for j in 0..argc {
            let s = self.x(base.wrapping_add(j as Reg), X9);
            self.set_x(s, j);
        }
    }

    /// `xd = ` the slot address `xn`, as a machine address.
    ///
    /// Three loads: the generation's table, the block's first slot, and -- left
    /// to the caller -- the slot itself. That is what it costs to reach the old
    /// generation at all, whose blocks are separate allocations with nothing
    /// contiguous to index; the nursery pays the same because a run of steps
    /// has no branch to tell the two apart with, and it is still two loads
    /// fewer than leaving native code.
    ///
    /// `xn` is left alone, so a caller that needs it again (a store, which
    /// wants the address and then the value) may keep it.
    fn thin_where(&mut self, d: u32, n: u32) {
        use super::addr::*;
        // x16 = &vm.heap.tables; x17 = addr >> 30, the generation.
        self.put(0x9100_0000 | layout::TABLES << 10 | X19 << 5 | X16); // add x16, x19, #TABLES
        self.thin_shr(X17, n, GEN_SHIFT);
        self.ldr_idx(X16, X16, X17); // ldr x16, [x16, x17, lsl #3]
        // x17 = (addr >> 13) & 0x1FFFF, the block; x16 = its first slot.
        self.ubfx(X17, n, BLOCK_SHIFT, BLOCK_MASK.count_ones());
        self.ldr_idx(X16, X16, X17); // ldr x16, [x16, x17, lsl #3]
        // d = that slot's address plus the offset within the block.
        self.ubfx(X17, n, 0, SLOT_MASK.count_ones());
        self.put(0x8B00_0000 | X17 << 16 | 3 << 10 | X16 << 5 | d); // add d, x16, x17, lsl #3
    }

    /// `xd = xn` bits `lsb ..< lsb + width`, zero-extended: `ubfx`, which is
    /// `ubfm xd, xn, #lsb, #(lsb + width - 1)`.
    fn ubfx(&mut self, d: u32, n: u32, lsb: u32, width: u32) {
        debug_assert!(width > 0 && lsb + width <= 64);
        self.put(0xD340_0000 | lsb << 16 | (lsb + width - 1) << 10 | n << 5 | d);
    }

    /// `ldr xt, [xn, xm, lsl #3]` -- word `xm` of the array at `xn`.
    fn ldr_idx(&mut self, t: u32, n: u32, m: u32) {
        self.put(0xF860_7800 | m << 16 | n << 5 | t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jump_past_what_b_reaches_is_said_not_to() {
        // `b` reaches 2^25 instructions either way, and its target here is
        // one past the gap. The code between is zeros, which
        // `Vec` gets from the allocator without touching.
        for (gap, reaches) in [((1 << 27) - 8, true), ((1 << 27) - 4, false)] {
            let mut asm = Asm::new();
            let far = asm.label();
            asm.jump(far);
            asm.code.resize(asm.code.len() + gap, 0);
            asm.bind(far);
            assert_eq!(asm.reaches(), reaches, "{gap} bytes on");
        }
    }
}
