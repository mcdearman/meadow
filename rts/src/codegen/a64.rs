//! AArch64 (AAPCS64): instructions encoded by hand.
//!
//! Machine registers during a block function: `x19` the machine, `x20` its
//! register file, `x21` steps not yet counted in the machine -- all saved by
//! the callee, so they survive the calls into the interpreter -- `x22` the
//! loop iterations this call has made, and `x9`
//! through `x11`, `d0` and `d1` for the instruction being done.

use super::{Emit, FloatOp, IntOp, Label, Operand, layout};
use meadow_bytecode::{Cond, Pc, Reg};

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
    fn slot(r: Reg) -> u32 {
        r as u32 * 8
    }

    /// `x10 = c`
    fn operand(&mut self, c: Operand) {
        match c {
            Operand::Reg(r) => self.ldr(X10, X20, Self::slot(r)),
            Operand::Imm(n) => self.imm(X10, n as u64),
        }
    }

    /// `live = max(live, a + 1)`
    fn raise_live(&mut self, a: Reg) {
        self.ldr(X10, X19, layout::LIVE);
        self.imm(X11, a as u64 + 1);
        self.cmp(X10, X11);
        // csel x10, x10, x11, hs
        self.put(0x9A80_0000 | X11 << 16 | HS << 12 | X10 << 5 | X10);
        self.str(X10, X19, layout::LIVE);
    }

    /// Store `x9` in `r[a]`, raising `live`.
    fn store(&mut self, a: Reg) {
        self.str(X9, X20, Self::slot(a));
        self.raise_live(a);
    }

    fn flush_steps(&mut self) {
        self.ldr(X9, X19, layout::STEPS);
        self.put(0x8B00_0000 | X21 << 16 | X9 << 5 | X9); // add x9, x9, x21
        self.str(X9, X19, layout::STEPS);
        self.imm(X21, 0);
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
        self.ldr(X9, X20, Self::slot(x));
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

    fn load_floats(&mut self, b: Reg, c: Reg) {
        self.ldr_d(0, X20, Self::slot(b));
        self.ldr_d(1, X20, Self::slot(c));
    }
}

impl Emit for Asm {
    fn new() -> Asm {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            ret_w0: None,
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

    fn prologue(&mut self) {
        self.put(0xA980_0000 | 0x7A << 15 | LR << 10 | SP << 5 | FP); // stp x29, x30, [sp, #-48]!
        self.put(0x9100_0000 | SP << 5 | FP); // add x29, sp, #0
        self.put(0xA900_0000 | 2 << 15 | X20 << 10 | SP << 5 | X19); // stp x19, x20, [sp, #16]
        self.str(X21, SP, 32);
        self.str(X22, SP, 40);
        self.mov_x(X19, X0);
        self.ldr(X20, X19, layout::REGS);
        self.imm(X21, 0);
        self.imm(X22, 0);
        self.ret_w0 = None;
    }

    fn ret(&mut self, status: u32) {
        self.flush_steps();
        self.put(0x5280_0000 | status << 5); // movz w0, #status
        self.restore_and_return();
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

    fn jump(&mut self, to: Label) {
        self.at(to, Fixup::B, 0x1400_0000);
    }

    fn mov(&mut self, a: Reg, b: Reg) {
        self.ldr(X9, X20, Self::slot(b));
        self.store(a);
    }

    fn word(&mut self, a: Reg, w: u64) {
        self.imm(X9, w);
        self.store(a);
    }

    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label) {
        self.ldr(X9, X20, Self::slot(b));
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
        self.str_d(0, X20, Self::slot(a));
        self.raise_live(a);
    }

    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand) {
        self.ldr(X9, X20, Self::slot(b));
        self.operand(c);
        self.cmp(X9, X10);
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
        self.ldr(X9, X20, Self::slot(x));
        self.operand(y);
        self.cmp(X9, X10);
        self.b_cond(Self::int_cond(cond) ^ 1, to);
    }

    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label) {
        self.load_floats(x, y);
        self.put(0x1E60_2000 | 1 << 16);
        self.b_cond(Self::float_cond(cond) ^ 1, to);
    }

    fn branch_zero(&mut self, x: Reg, to: Label) {
        self.ldr(X9, X20, Self::slot(x));
        self.at(to, Fixup::Imm19, 0xB400_0000 | X9);
    }

    fn set_live(&mut self, n: u32) {
        self.imm(X9, n as u64);
        self.str(X9, X19, layout::LIVE);
    }

    fn back_edge(&mut self, to: Label, over: Label) {
        self.put(0x9100_0400 | X22 << 5 | X22); // add x22, x22, #1
        self.imm(X9, super::BACK_EDGES as u64);
        self.cmp(X22, X9);
        self.b_cond(HS, over);
        self.jump(to);
    }

    fn unstep(&mut self) {
        self.put(0xD100_0400 | X21 << 5 | X21); // sub x21, x21, #1
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
            self.ldr(X9, X20, Self::slot(base) + j * 8);
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
        self.str(X17, X19, layout::PC);
        self.ret(crate::abi::JUMPED);
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
            self.ldr(X15, X20, Self::slot(base) + j * 8);
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
