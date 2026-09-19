//! AArch64 (AAPCS64): instructions encoded by hand.
//!
//! Machine registers during a block function: `x19` the machine, `x20` its
//! register file, `x21` steps not yet counted in the machine -- all saved by
//! the callee, so they survive the calls into the interpreter -- `x22` the
//! loop iterations this call has made, and `x9` through `x17`, `d0` and `d1`
//! for the instruction being done.
//!
//! `x2` to `x8` hold the bytecode registers a function pins (see
//! [`super::Emit::configure`]): loaded after the prologue, written to memory
//! before every call and return, and loaded again after every call. Nothing
//! else touches them.

use super::{Emit, FloatOp, IntOp, Label, Operand, layout, thin};
use meadow_bytecode::{Cond, Pc, Reg};
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
const FP: u32 = 29;
const LR: u32 = 30;
const SP: u32 = 31;

/// Where pinned registers live, in the order they are handed out.
const PIN_REGS: [u32; 7] = [2, 3, 4, 5, 6, 7, 8];

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
    /// Pinned bytecode registers, and the machine register each lives in.
    pins: Vec<(Reg, u32)>,
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
    fn pinned(&self, r: u32) -> Option<u32> {
        self.pins
            .iter()
            .find(|&&(p, _)| p as u32 == r)
            .map(|&(_, m)| m)
    }

    /// `xt = r[r]`
    fn get(&mut self, t: u32, r: u32) {
        match self.pinned(r) {
            Some(m) if m != t => self.mov_x(t, m),
            Some(_) => {}
            None => self.ldr(t, X20, Self::slot(r)),
        }
    }

    /// `r[r] = xt`
    fn set(&mut self, t: u32, r: u32) {
        match self.pinned(r) {
            Some(m) if m != t => self.mov_x(m, t),
            Some(_) => {}
            None => self.str(t, X20, Self::slot(r)),
        }
    }

    /// Every pinned register, to memory.
    fn spill(&mut self) {
        for (r, m) in self.pins.clone() {
            self.str(m, X20, Self::slot(r as u32));
        }
    }

    /// Every pinned register, from memory.
    fn reload(&mut self) {
        for (r, m) in self.pins.clone() {
            self.ldr(m, X20, Self::slot(r as u32));
        }
    }

    /// `x10 = c`
    fn operand(&mut self, c: Operand) {
        match c {
            Operand::Reg(r) => self.get(X10, r as u32),
            Operand::Imm(n) => self.imm(X10, n as u64),
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

    /// `cmp x9, c`
    fn cmp_x9(&mut self, c: Operand) {
        match self.short_imm(c) {
            // cmp x9, #k -- or cmn, for a negative one.
            Some((k, false)) => self.put(0xF100_001F | k << 10 | X9 << 5),
            Some((k, true)) => self.put(0xB100_001F | k << 10 | X9 << 5),
            None => {
                self.operand(c);
                self.cmp(X9, X10);
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

    /// Store `x9` in `r[a]`, raising `live`.
    fn store(&mut self, a: Reg) {
        self.set(X9, a as u32);
        if !self.defer_live {
            self.raise(a as u32 + 1);
        }
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
        self.ldr(X22, SP, 40);
        self.ldr(X21, SP, 32);
        self.put(0xA940_0000 | 2 << 15 | X20 << 10 | SP << 5 | X19); // ldp x19, x20, [sp, #16]
        self.put(0xA8C0_0000 | 6 << 15 | LR << 10 | SP << 5 | FP); // ldp x29, x30, [sp], #48
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
    fn nursery(&mut self, x: Reg, slow: Label) {
        self.get(X9, x as u32);
        self.imm(X10, crate::old::OLD_BASE as u64);
        self.cmp(X9, X10);
        self.b_cond(HS, slow);
        self.ldr(X11, X19, layout::BASE);
        self.put(0x8B00_0000 | X9 << 16 | 3 << 10 | X11 << 5 | X12); // add x12, x11, x9, lsl #3
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

    /// `dt = r[r]`
    fn get_float(&mut self, t: u32, r: Reg) {
        match self.pinned(r as u32) {
            Some(m) => self.put(0x9E67_0000 | m << 5 | t), // fmov dt, xm
            None => self.ldr_d(t, X20, Self::slot(r as u32)),
        }
    }

    fn load_floats(&mut self, b: Reg, c: Reg) {
        self.get_float(0, b);
        self.get_float(1, c);
    }

    /// `r[a] = d0`, raising `live`.
    fn store_float(&mut self, a: Reg) {
        match self.pinned(a as u32) {
            Some(m) => self.put(0x9E66_0000 | m), // fmov xm, d0
            None => self.str_d(0, X20, Self::slot(a as u32)),
        }
        if !self.defer_live {
            self.raise(a as u32 + 1);
        }
    }
}

impl Emit for Asm {
    const PINS: usize = PIN_REGS.len();
    const WARM: usize = 36;

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

    fn configure(&mut self, opt: OptLevel, pins: &[Reg]) {
        self.defer_live = opt >= OptLevel::O1;
        self.short = opt >= OptLevel::O1;
        self.chain = opt >= OptLevel::O1;
        self.pins = pins.iter().copied().zip(PIN_REGS).collect();
    }

    fn prologue(&mut self, warm: Option<Label>) {
        let start = self.offset();
        self.put(0xA980_0000 | 0x7A << 15 | LR << 10 | SP << 5 | FP); // stp x29, x30, [sp, #-48]!
        self.put(0x9100_0000 | SP << 5 | FP); // add x29, sp, #0
        self.put(0xA900_0000 | 2 << 15 | X20 << 10 | SP << 5 | X19); // stp x19, x20, [sp, #16]
        self.str(X21, SP, 32);
        self.str(X22, SP, 40);
        self.mov_x(X19, X0);
        self.ldr(X20, X19, layout::REGS);
        self.imm(X21, 0);
        self.imm(X22, 0);
        // A chained function arrives here, with the frame, the machine, its
        // register file, the steps and the loop count of the one it came from.
        assert_eq!(self.offset() - start, Self::WARM, "the warm entry moved");
        if let Some(warm) = warm {
            self.bind(warm);
        }
        self.reload();
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
        self.spill();
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
        self.get(X9, b as u32);
        self.store(a);
    }

    fn word(&mut self, a: Reg, w: u64) {
        self.imm(X9, w);
        self.store(a);
    }

    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label) {
        self.get(X9, b as u32);
        if let (IntOp::Add | IntOp::Sub, Some((k, negative))) = (op, self.short_imm(c)) {
            // add x9, x9, #k, or sub -- the other, for a negative constant.
            let sub = (op == IntOp::Sub) != negative;
            let base = if sub { 0xD100_0000 } else { 0x9100_0000 };
            self.put(base | k << 10 | X9 << 5 | X9);
            self.store(a);
            return;
        }
        self.operand(c);
        let (n, m) = (X9, X10);
        match op {
            IntOp::Add => self.put(0x8B00_0000 | m << 16 | n << 5 | X9),
            IntOp::Sub => self.put(0xCB00_0000 | m << 16 | n << 5 | X9),
            IntOp::Mul => self.put(0x9B00_7C00 | m << 16 | n << 5 | X9),
            IntOp::Div | IntOp::Rem => {
                self.at(zero, Fixup::Imm19, 0xB400_0000 | X10); // cbz x10, zero
                // `sdiv` wraps, as `i64::wrapping_div` does: MIN / -1 is MIN.
                self.put(0x9AC0_0C00 | m << 16 | n << 5 | X11); // sdiv x11, x9, x10
                if op == IntOp::Div {
                    self.mov_x(X9, X11);
                } else {
                    // msub x9, x11, x10, x9: x9 - x11 * x10
                    self.put(0x9B00_8000 | m << 16 | X9 << 10 | X11 << 5 | X9);
                }
            }
        }
        self.store(a);
    }

    fn float(&mut self, op: FloatOp, a: Reg, b: Reg, c: Reg) {
        self.load_floats(b, c);
        let base = match op {
            FloatOp::Add => 0x1E60_2800,
            FloatOp::Sub => 0x1E60_3800,
            FloatOp::Mul => 0x1E60_0800,
            FloatOp::Div => 0x1E60_1800,
        };
        self.put(base | 1 << 16); // d0 = d0 op d1
        self.store_float(a);
    }

    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand) {
        self.get(X9, b as u32);
        self.cmp_x9(c);
        self.cset(X9, Self::int_cond(cond));
        self.store(a);
    }

    fn cmp_float(&mut self, cond: Cond, a: Reg, b: Reg, c: Reg) {
        self.load_floats(b, c);
        self.put(0x1E60_2000 | 1 << 16); // fcmp d0, d1
        self.cset(X9, Self::float_cond(cond));
        self.store(a);
    }

    fn branch_int(&mut self, cond: Cond, x: Reg, y: Operand, to: Label) {
        self.get(X9, x as u32);
        self.cmp_x9(y);
        self.b_cond(Self::int_cond(cond) ^ 1, to);
    }

    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label) {
        self.load_floats(x, y);
        self.put(0x1E60_2000 | 1 << 16);
        self.b_cond(Self::float_cond(cond) ^ 1, to);
    }

    fn branch_zero(&mut self, x: Reg, to: Label) {
        self.get(X9, x as u32);
        self.at(to, Fixup::Imm19, 0xB400_0000 | X9);
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
        self.imm(X9, super::BACK_EDGES as u64);
        self.cmp(X22, X9);
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
            match *s {
                Step::Set(t, src) => self.thin_src(TMP + t as u32, src),
                Step::Add(t, a, b) => {
                    self.thin_src(WORK_A, a);
                    self.thin_src(WORK_B, b);
                    self.put(0x8B00_0000 | WORK_B << 16 | WORK_A << 5 | (TMP + t as u32));
                }
                Step::And(t, a, mask) => {
                    self.thin_src(WORK_A, a);
                    self.imm(WORK_B, mask);
                    self.put(0x8A00_0000 | WORK_B << 16 | WORK_A << 5 | (TMP + t as u32));
                }
                Step::Shr(t, a, n) => {
                    self.thin_src(WORK_A, a);
                    self.thin_shr(TMP + t as u32, WORK_A, n);
                }
                Step::Load(t, at) => {
                    self.thin_src(WORK_A, at);
                    self.thin_load(TMP + t as u32, WORK_A);
                }
                Step::Store(at, v) => {
                    self.thin_src(WORK_A, at);
                    self.ldr(WORK_B, X19, layout::BASE);
                    self.put(0x8B00_0000 | WORK_A << 16 | 3 << 10 | WORK_B << 5 | WORK_A);
                    self.thin_src(WORK_B, v);
                    self.str(WORK_B, WORK_A, 0);
                }
                // Branch away on the *negation*: the guard holding is the
                // ordinary case and falls through. Unsigned throughout, which
                // is what makes one `u <` reject a negative index as well as
                // one past the end.
                Step::Guard(c, a, b) => {
                    self.thin_src(WORK_A, a);
                    self.thin_src(WORK_B, b);
                    self.cmp(WORK_A, WORK_B);
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
                    self.thin_src(WORK_A, src);
                    self.set(WORK_A, r as u32);
                    if !self.defer_live {
                        self.raise(r as u32 + 1);
                    }
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

    fn invoke(&mut self, obj: Reg, method: u8, base: Reg, argc: u32, slow: Label) {
        if argc > 247 {
            self.jump(slow);
            return;
        }
        // The register file is rebuilt in memory below.
        self.spill();
        self.nursery(obj, slow);
        self.cmp_kind(crate::heap::Kind::Closure as u32);
        self.b_cond(NE, slow);
        self.length();
        self.put(0xF100_201F | X15 << 5); // cmp x15, #8
        self.b_cond(HI, slow);
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
        // The arguments out of the way, the captures in, the arguments after.
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
        self.put(0x9100_0800 | X10 << 5 | X11); // add x11, x10, #2
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
        if self.chain {
            let out = self.label();
            self.budget(out);
            self.ldr(X9, X19, layout::NATIVE_TABLE);
            self.put(0xF860_7800 | X17 << 16 | X9 << 5 | X9); // ldr x9, [x9, x17, lsl #3]
            self.at(out, Fixup::Imm19, 0xB400_0000 | X9); // cbz x9, out
            self.enter_warm();
            self.bind(out);
        }
        self.str(X17, X19, layout::PC);
        self.ret_as_is(crate::abi::JUMPED);
    }

    fn alloc(&mut self, a: Reg, header: [u64; 2], base: Reg, n: u32, slow: Label) {
        let size = 2 + n;
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
        self.imm(X15, header[0]);
        self.str(X15, X14, 0);
        self.imm(X15, header[1]);
        self.str(X15, X14, 8);
        for j in 0..n {
            self.get(X15, base as u32 + j);
            self.str(X15, X14, (2 + j) * 8);
        }
        self.str(X10, X19, layout::TOP);
        self.ldr(X15, X19, layout::ALLOCATED);
        self.put(0x9100_0000 | size << 10 | X15 << 5 | X15); // add x15, x15, #size
        self.str(X15, X19, layout::ALLOCATED);
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

    fn finish(mut self) -> Vec<u8> {
        for (at, l, f) in std::mem::take(&mut self.fixups) {
            let to = self.labels[l.0].expect("every label is bound");
            let delta = (to as i64 - at as i64) / 4;
            let word = u32::from_le_bytes(self.code[at..at + 4].try_into().expect("four bytes"));
            let word = match f {
                Fixup::B => {
                    assert!((-(1 << 25)..(1 << 25)).contains(&delta), "a branch too far");
                    word | (delta as u32 & 0x03FF_FFFF)
                }
                Fixup::Imm19 => {
                    assert!((-(1 << 18)..(1 << 18)).contains(&delta), "a branch too far");
                    word | (delta as u32 & 0x7FFFF) << 5
                }
            };
            self.code[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.code
    }
}

/// Unsigned lower, which the conditions above do not otherwise need.
const LO: u32 = 3;

/// The scratch registers a run of thin steps keeps its temporaries in, and the
/// two the emitter works in. See [`crate::codegen::thin`].
const TMP: u32 = X9;
const WORK_A: u32 = X14;
const WORK_B: u32 = X15;

impl Asm {
    /// `xd = src`.
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

    /// `xd = ` the heap word at slot `xn`.
    fn thin_load(&mut self, d: u32, n: u32) {
        self.ldr(WORK_B, X19, layout::BASE);
        self.put(0x8B00_0000 | n << 16 | 3 << 10 | WORK_B << 5 | WORK_A); // add x14, x15, xn, lsl #3
        self.ldr(d, WORK_A, 0);
    }
}
