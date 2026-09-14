//! x86-64 (System V): instructions encoded by hand.
//!
//! Machine registers during a block function: `rbx` the machine, `r12` its
//! register file, `r13` steps not yet counted in the machine -- saved by the
//! callee, so they survive the calls into the interpreter -- `r14` the loop
//! iterations this call has made, and `rax`, `rcx`, `rdx`, `rsi`, `rdi`,
//! `xmm0` and `xmm1` for the instruction being done.
//!
//! `r8` to `r11` and `r15` hold the bytecode registers a function pins (see
//! [`super::Emit::configure`]): loaded after the prologue, written to memory
//! before every call and return, and loaded again after every call. Nothing
//! else touches them.

use super::{Emit, FloatOp, IntOp, Label, Operand, layout};
use meadow_bytecode::{Cond, Pc, Reg};
use meadow_core::OptLevel;

const RAX: u8 = 0;
const RCX: u8 = 1;
const RBX: u8 = 3;
const RSI: u8 = 6;
const R12: u8 = 12;
const R13: u8 = 13;

/// Where pinned registers live, in the order they are handed out.
const PIN_REGS: [u8; 5] = [8, 9, 10, 11, 15];

// Condition codes, as the low nibble of `jcc` and `setcc`.
const CC_BE: u8 = 0x6;
const CC_AE: u8 = 0x3;
const CC_E: u8 = 0x4;
const CC_NE: u8 = 0x5;
const CC_A: u8 = 0x7;
const CC_P: u8 = 0xA;
const CC_NP: u8 = 0xB;
const CC_L: u8 = 0xC;
const CC_GE: u8 = 0xD;
const CC_LE: u8 = 0xE;
const CC_G: u8 = 0xF;

pub struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// Where a 32-bit displacement to a label is, measured from the end of it.
    fixups: Vec<(usize, Label)>,
    ret_eax: Option<Label>,
    /// Writing a register leaves `live` to the caller (see `codegen::Book`).
    defer_live: bool,
    /// Constant operands encoded into the instruction, where they fit.
    short: bool,
    /// Pinned bytecode registers, and the machine register each lives in.
    pins: Vec<(Reg, u8)>,
    /// Go straight on to the next block's native code where there is some.
    chain: bool,
}

impl Asm {
    fn bytes(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }

    fn rel32(&mut self, to: Label) {
        self.fixups.push((self.code.len(), to));
        self.bytes(&[0; 4]);
    }

    /// `REX.W` with the extension bits for `reg` and `base`.
    fn rex_w(&mut self, reg: u8, base: u8) {
        self.bytes(&[0x48 | (reg >> 3) << 2 | (base >> 3)]);
    }

    /// ModRM, and SIB where the base needs one, for `[base + disp]`.
    fn mem(&mut self, reg: u8, base: u8, disp: u32) {
        self.bytes(&[0x80 | (reg & 7) << 3 | (base & 7)]);
        if base & 7 == 4 {
            self.bytes(&[0x24]);
        }
        self.bytes(&disp.to_le_bytes());
    }

    /// `reg = [base + disp]`, 64 bits.
    fn load(&mut self, reg: u8, base: u8, disp: u32) {
        self.rex_w(reg, base);
        self.bytes(&[0x8B]);
        self.mem(reg, base, disp);
    }

    /// `[base + disp] = reg`, 64 bits.
    fn save(&mut self, reg: u8, base: u8, disp: u32) {
        self.rex_w(reg, base);
        self.bytes(&[0x89]);
        self.mem(reg, base, disp);
    }

    /// `dst = src`, 64 bits.
    fn mov_rr(&mut self, dst: u8, src: u8) {
        if dst != src {
            self.rex_w(src, dst);
            self.bytes(&[0x89, 0xC0 | (src & 7) << 3 | (dst & 7)]);
        }
    }

    /// `reg = imm`, 64 bits.
    fn imm(&mut self, reg: u8, imm: u64) {
        self.bytes(&[0x48 | (reg >> 3), 0xB8 + (reg & 7)]);
        self.bytes(&imm.to_le_bytes());
    }

    /// `[base + disp] = imm`, sign-extended from 32 bits.
    fn save_imm(&mut self, base: u8, disp: u32, imm: u32) {
        self.rex_w(0, base);
        self.bytes(&[0xC7]);
        self.mem(0, base, disp);
        self.bytes(&imm.to_le_bytes());
    }

    fn slot(r: u32) -> u32 {
        r * 8
    }

    /// The machine register bytecode register `r` is pinned to, if it is.
    fn pinned(&self, r: u32) -> Option<u8> {
        self.pins
            .iter()
            .find(|&&(p, _)| p as u32 == r)
            .map(|&(_, m)| m)
    }

    /// `reg = r[r]`
    fn get(&mut self, reg: u8, r: u32) {
        match self.pinned(r) {
            Some(m) => self.mov_rr(reg, m),
            None => self.load(reg, R12, Self::slot(r)),
        }
    }

    /// `r[r] = reg`
    fn put(&mut self, reg: u8, r: u32) {
        match self.pinned(r) {
            Some(m) => self.mov_rr(m, reg),
            None => self.save(reg, R12, Self::slot(r)),
        }
    }

    /// Every pinned register, to memory.
    fn spill(&mut self) {
        for (r, m) in self.pins.clone() {
            self.save(m, R12, Self::slot(r as u32));
        }
    }

    /// Every pinned register, from memory.
    fn reload(&mut self) {
        for (r, m) in self.pins.clone() {
            self.load(m, R12, Self::slot(r as u32));
        }
    }

    /// `rcx = c`
    fn operand(&mut self, c: Operand) {
        match c {
            Operand::Reg(r) => self.get(RCX, r as u32),
            Operand::Imm(n) => self.imm(RCX, n as u64),
        }
    }

    /// `c` as a sign-extended 32-bit immediate, where that is how it is
    /// encoded.
    fn short_imm(&self, c: Operand) -> Option<i32> {
        match c {
            Operand::Imm(n) if self.short => i32::try_from(n).ok(),
            _ => None,
        }
    }

    /// `cmp rax, c`
    fn cmp_rax(&mut self, c: Operand) {
        match self.short_imm(c) {
            Some(k) => {
                self.bytes(&[0x48, 0x3D]);
                self.bytes(&k.to_le_bytes());
            }
            None => {
                self.operand(c);
                self.bytes(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
            }
        }
    }

    fn raise(&mut self, n: u32) {
        self.load(RCX, RBX, layout::LIVE);
        self.bytes(&[0xBA]); // mov edx, n
        self.bytes(&n.to_le_bytes());
        self.bytes(&[0x48, 0x39, 0xD1]); // cmp rcx, rdx
        self.bytes(&[0x48, 0x0F, 0x42, 0xCA]); // cmovb rcx, rdx
        self.save(RCX, RBX, layout::LIVE);
    }

    /// `r[a] = rax`, raising `live`.
    fn store(&mut self, a: Reg) {
        self.put(RAX, a as u32);
        if !self.defer_live {
            self.raise(a as u32 + 1);
        }
    }

    fn flush_steps(&mut self) {
        // add [rbx + STEPS], r13
        self.rex_w(R13, RBX);
        self.bytes(&[0x01]);
        self.mem(R13, RBX, layout::STEPS);
        self.bytes(&[0x45, 0x31, 0xED]); // xor r13d, r13d
    }

    /// On to `out` if this call has made [`super::CHAINS`] jumps into other
    /// functions, counting this one.
    fn budget(&mut self, out: Label) {
        self.bytes(&[0x49, 0xFF, 0xC6]); // inc r14
        self.bytes(&[0x49, 0x81, 0xFE]); // cmp r14, CHAINS
        self.bytes(&super::CHAINS.to_le_bytes());
        self.jcc(CC_AE, out);
    }

    /// Jump to the warm entry of the function whose start is in `rax`.
    fn enter_warm(&mut self) {
        self.bytes(&[0x48, 0x83, 0xC0, Self::WARM as u8]); // add rax, WARM
        self.bytes(&[0xFF, 0xE0]); // jmp rax
    }

    /// Return `status` with the register file as it is in memory.
    fn ret_as_is(&mut self, status: u32) {
        self.flush_steps();
        self.bytes(&[0xB8]); // mov eax, status
        self.bytes(&status.to_le_bytes());
        self.restore_and_return();
    }

    fn restore_and_return(&mut self) {
        self.bytes(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8
        self.bytes(&[
            0x41, 0x5F, 0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C, 0x5B, 0x5D, 0xC3,
        ]);
    }

    fn jcc(&mut self, cc: u8, to: Label) {
        self.bytes(&[0x0F, 0x80 | cc]);
        self.rel32(to);
    }

    fn int_cc(c: Cond) -> u8 {
        match c {
            Cond::Eq => CC_E,
            Cond::Ne => CC_NE,
            Cond::Lt => CC_L,
            Cond::Le => CC_LE,
            Cond::Gt => CC_G,
            Cond::Ge => CC_GE,
        }
    }

    /// `al = cond(xmm0, xmm1)`, IEEE's way.
    fn float_bool(&mut self, cond: Cond) {
        const UCOMISD_0_1: [u8; 4] = [0x66, 0x0F, 0x2E, 0xC1];
        const UCOMISD_1_0: [u8; 4] = [0x66, 0x0F, 0x2E, 0xC8];
        let setcc = |a: &mut Asm, cc: u8, reg: u8| a.bytes(&[0x0F, 0x90 | cc, 0xC0 | reg]);
        match cond {
            // Unordered sets ZF, PF and CF together.
            Cond::Eq => {
                self.bytes(&UCOMISD_0_1);
                setcc(self, CC_E, RAX);
                setcc(self, CC_NP, RCX);
                self.bytes(&[0x20, 0xC8]); // and al, cl
            }
            Cond::Ne => {
                self.bytes(&UCOMISD_0_1);
                setcc(self, CC_NE, RAX);
                setcc(self, CC_P, RCX);
                self.bytes(&[0x08, 0xC8]); // or al, cl
            }
            Cond::Lt => {
                self.bytes(&UCOMISD_1_0);
                setcc(self, CC_A, RAX);
            }
            Cond::Le => {
                self.bytes(&UCOMISD_1_0);
                setcc(self, CC_AE, RAX);
            }
            Cond::Gt => {
                self.bytes(&UCOMISD_0_1);
                setcc(self, CC_A, RAX);
            }
            Cond::Ge => {
                self.bytes(&UCOMISD_0_1);
                setcc(self, CC_AE, RAX);
            }
        }
    }

    /// `rax = r[x]`, and on to `slow` unless that is a nursery address; then
    /// `rdx` the object's first slot, `rsi` its first word, `ecx` its kind.
    fn nursery(&mut self, x: Reg, slow: Label) {
        self.get(RAX, x as u32);
        self.bytes(&[0x48, 0x3D]); // cmp rax, OLD_BASE
        self.bytes(&crate::old::OLD_BASE.to_le_bytes());
        self.jcc(CC_AE, slow);
        self.load(2, RBX, layout::BASE); // rdx
        self.bytes(&[0x48, 0x8D, 0x14, 0xC2]); // lea rdx, [rdx + rax*8]
        self.bytes(&[0x48, 0x8B, 0x32]); // mov rsi, [rdx]
        self.bytes(&[0x40, 0x0F, 0xB6, 0xCE]); // movzx ecx, sil
    }

    /// `rdi = rsi >> 32`: the object's length.
    fn length(&mut self) {
        self.bytes(&[0x48, 0x89, 0xF7, 0x48, 0xC1, 0xEF, 0x20]);
    }

    /// `xmm = r[r]`
    fn get_float(&mut self, xmm: u8, r: Reg) {
        match self.pinned(r as u32) {
            // movq xmm, m
            Some(m) => self.bytes(&[0x66, 0x48 | (m >> 3), 0x0F, 0x6E, 0xC0 | xmm << 3 | (m & 7)]),
            None => {
                self.bytes(&[0xF2, 0x41, 0x0F, 0x10]); // movsd xmm, [r12 + slot]
                self.mem(xmm, R12, Self::slot(r as u32));
            }
        }
    }

    /// `r[a] = xmm0`, raising `live`.
    fn store_float(&mut self, a: Reg) {
        match self.pinned(a as u32) {
            // movq m, xmm0
            Some(m) => self.bytes(&[0x66, 0x48 | (m >> 3), 0x0F, 0x7E, 0xC0 | (m & 7)]),
            None => {
                self.bytes(&[0xF2, 0x41, 0x0F, 0x11]); // movsd [r12 + slot], xmm0
                self.mem(0, R12, Self::slot(a as u32));
            }
        }
        if !self.defer_live {
            self.raise(a as u32 + 1);
        }
    }
}

impl Emit for Asm {
    const PINS: usize = PIN_REGS.len();
    const WARM: usize = 33;

    fn new() -> Asm {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            ret_eax: None,
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
        // push rbp; mov rbp, rsp; push rbx; push r12; push r13; push r14;
        // push r15; sub rsp, 8 -- eight words on the stack with the return
        // address, so it stays aligned for the calls.
        self.bytes(&[
            0x55, 0x48, 0x89, 0xE5, 0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48,
            0x83, 0xEC, 0x08,
        ]);
        self.bytes(&[0x48, 0x89, 0xFB]); // mov rbx, rdi
        self.load(R12, RBX, layout::REGS);
        self.bytes(&[0x45, 0x31, 0xED]); // xor r13d, r13d
        self.bytes(&[0x45, 0x31, 0xF6]); // xor r14d, r14d
        // A chained function arrives here, with the frame, the machine, its
        // register file, the steps and the loop count of the one it came from.
        assert_eq!(self.offset() - start, Self::WARM, "the warm entry moved");
        if let Some(warm) = warm {
            self.bind(warm);
        }
        self.reload();
        self.ret_eax = None;
    }

    fn ret(&mut self, status: u32) {
        self.spill();
        self.ret_as_is(status);
    }

    fn chain(&mut self, pc: Pc, live: Option<u32>, to: Option<Label>) {
        if let Some(live) = live {
            self.save_imm(RBX, layout::LIVE, live);
        }
        self.spill();
        let out = self.label();
        self.budget(out);
        match to {
            Some(to) => self.jump(to),
            None => {
                self.load(RAX, RBX, layout::NATIVE_TABLE);
                self.bytes(&[0x48, 0x8B, 0x80]); // mov rax, [rax + pc*8]
                self.bytes(&(pc * 8).to_le_bytes());
                self.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
                self.jcc(CC_E, out);
                self.enter_warm();
            }
        }
        self.bind(out);
        self.save_imm(RBX, layout::PC, pc);
        self.ret_as_is(crate::abi::JUMPED);
    }

    fn leave(&mut self, pc: Pc, live: Option<u32>) {
        self.save_imm(RBX, layout::PC, pc);
        if let Some(live) = live {
            self.save_imm(RBX, layout::LIVE, live);
        }
        self.ret(crate::abi::JUMPED);
    }

    fn end(&mut self) {
        if let Some(l) = self.ret_eax.take() {
            self.bind(l);
            self.restore_and_return();
        }
    }

    fn step(&mut self) {
        self.bytes(&[0x49, 0xFF, 0xC5]); // inc r13
    }

    fn unstep(&mut self) {
        self.bytes(&[0x49, 0xFF, 0xCD]); // dec r13
    }

    fn add_steps(&mut self, n: u32) {
        if n == 1 {
            self.step();
        } else {
            self.bytes(&[0x49, 0x81, 0xC5]); // add r13, n
            self.bytes(&n.to_le_bytes());
        }
    }

    fn jump(&mut self, to: Label) {
        self.bytes(&[0xE9]);
        self.rel32(to);
    }

    fn mov(&mut self, a: Reg, b: Reg) {
        self.get(RAX, b as u32);
        self.store(a);
    }

    fn word(&mut self, a: Reg, w: u64) {
        match u32::try_from(w) {
            Ok(w) if self.short => {
                self.bytes(&[0xB8]); // mov eax, w
                self.bytes(&w.to_le_bytes());
            }
            _ => self.imm(RAX, w),
        }
        self.store(a);
    }

    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label) {
        self.get(RAX, b as u32);
        match (op, self.short_imm(c)) {
            (IntOp::Add, Some(k)) => {
                self.bytes(&[0x48, 0x05]); // add rax, k
                self.bytes(&k.to_le_bytes());
            }
            (IntOp::Sub, Some(k)) => {
                self.bytes(&[0x48, 0x2D]); // sub rax, k
                self.bytes(&k.to_le_bytes());
            }
            (IntOp::Mul, Some(k)) => {
                self.bytes(&[0x48, 0x69, 0xC0]); // imul rax, rax, k
                self.bytes(&k.to_le_bytes());
            }
            _ => {
                self.operand(c);
                match op {
                    IntOp::Add => self.bytes(&[0x48, 0x01, 0xC8]), // add rax, rcx
                    IntOp::Sub => self.bytes(&[0x48, 0x29, 0xC8]), // sub rax, rcx
                    IntOp::Mul => self.bytes(&[0x48, 0x0F, 0xAF, 0xC1]), // imul rax, rcx
                    IntOp::Div | IntOp::Rem => {
                        self.bytes(&[0x48, 0x85, 0xC9]); // test rcx, rcx
                        self.jcc(CC_E, zero);
                        // `idiv` traps on MIN / -1, where `wrapping_div` gives
                        // MIN and `wrapping_rem` 0 -- which is `-x` and `0` for
                        // any `x`.
                        let normal = self.label();
                        let done = self.label();
                        self.bytes(&[0x48, 0x83, 0xF9, 0xFF]); // cmp rcx, -1
                        self.jcc(CC_NE, normal);
                        if op == IntOp::Div {
                            self.bytes(&[0x48, 0xF7, 0xD8]); // neg rax
                        } else {
                            self.bytes(&[0x31, 0xC0]); // xor eax, eax
                        }
                        self.jump(done);
                        self.bind(normal);
                        self.bytes(&[0x48, 0x99]); // cqo
                        self.bytes(&[0x48, 0xF7, 0xF9]); // idiv rcx
                        if op == IntOp::Rem {
                            self.bytes(&[0x48, 0x89, 0xD0]); // mov rax, rdx
                        }
                        self.bind(done);
                    }
                }
            }
        }
        self.store(a);
    }

    fn float(&mut self, op: FloatOp, a: Reg, b: Reg, c: Reg) {
        self.get_float(0, b);
        let opcode = match op {
            FloatOp::Add => 0x58,
            FloatOp::Sub => 0x5C,
            FloatOp::Mul => 0x59,
            FloatOp::Div => 0x5E,
        };
        if self.pinned(c as u32).is_some() {
            self.get_float(1, c);
            self.bytes(&[0xF2, 0x0F, opcode, 0xC1]); // xmm0 op= xmm1
        } else {
            self.bytes(&[0xF2, 0x41, 0x0F, opcode]); // xmm0 op= [r12 + c]
            self.mem(0, R12, Self::slot(c as u32));
        }
        self.store_float(a);
    }

    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand) {
        self.get(RAX, b as u32);
        self.cmp_rax(c);
        self.bytes(&[0x0F, 0x90 | Self::int_cc(cond), 0xC0]); // setcc al
        self.bytes(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        self.store(a);
    }

    fn cmp_float(&mut self, cond: Cond, a: Reg, b: Reg, c: Reg) {
        self.get_float(0, b);
        self.get_float(1, c);
        self.float_bool(cond);
        self.bytes(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        self.store(a);
    }

    fn branch_int(&mut self, cond: Cond, x: Reg, y: Operand, to: Label) {
        self.get(RAX, x as u32);
        self.cmp_rax(y);
        self.jcc(Self::int_cc(cond) ^ 1, to);
    }

    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label) {
        self.get_float(0, x);
        self.get_float(1, y);
        self.float_bool(cond);
        self.bytes(&[0x84, 0xC0]); // test al, al
        self.jcc(CC_E, to);
    }

    fn branch_zero(&mut self, x: Reg, to: Label) {
        self.get(RAX, x as u32);
        self.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
        self.jcc(CC_E, to);
    }

    fn set_live(&mut self, n: u32) {
        self.save_imm(RBX, layout::LIVE, n);
    }

    fn raise_live_to(&mut self, n: u32) {
        self.raise(n);
    }

    fn back_edge(&mut self, to: Label, over: Label) {
        self.bytes(&[0x49, 0xFF, 0xC6]); // inc r14
        // cmp r14, BACK_EDGES
        self.bytes(&[0x49, 0x81, 0xFE]);
        self.bytes(&super::BACK_EDGES.to_le_bytes());
        self.jcc(CC_AE, over);
        self.jump(to);
    }

    fn tag_test(&mut self, x: Reg, tag: u32, miss: Label, slow: Label) {
        self.nursery(x, slow);
        self.bytes(&[0x85, 0xC9]); // test ecx, ecx: Data
        self.jcc(CC_NE, miss);
        self.bytes(&[0x8B, 0x4A, 0x08]); // mov ecx, [rdx + 8]: meta
        self.bytes(&[0x81, 0xF9]); // cmp ecx, tag
        self.bytes(&tag.to_le_bytes());
        self.jcc(CC_NE, miss);
    }

    fn field(&mut self, a: Reg, b: Reg, i: u32, slow: Label) {
        self.nursery(b, slow);
        self.bytes(&[0x83, 0xF9, crate::heap::Kind::Array as u8]); // cmp ecx, Array
        self.jcc(CC_A, slow);
        self.length();
        self.bytes(&[0x48, 0x81, 0xFF]); // cmp rdi, i
        self.bytes(&i.to_le_bytes());
        self.jcc(CC_BE, slow);
        let two_words = self.label();
        self.bytes(&[0xF7, 0xC6, 0x00, 0x10, 0x00, 0x00]); // test esi, UNIFORM
        self.jcc(CC_NE, two_words);
        self.bytes(&[0x48, 0x83, 0xFF, 0x08]); // cmp rdi, 8
        self.jcc(CC_A, slow);
        self.bind(two_words);
        self.bytes(&[0x48, 0x8B, 0x82]); // mov rax, [rdx + (2 + i) * 8]
        self.bytes(&((2 + i) * 8).to_le_bytes());
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
        self.bytes(&[0x83, 0xF9, crate::heap::Kind::Closure as u8]); // cmp ecx, Closure
        self.jcc(CC_NE, slow);
        self.length();
        self.bytes(&[0x48, 0x83, 0xFF, 0x08]); // cmp rdi, 8
        self.jcc(CC_A, slow);
        // The method's pc: table `meta`, entry `method`, if it has one.
        self.bytes(&[0x8B, 0x4A, 0x08]); // mov ecx, [rdx + 8]
        self.load(RSI, RBX, layout::METHOD_STARTS);
        self.bytes(&[0x8B, 0x04, 0x8E]); // mov eax, [rsi + rcx*4]
        self.bytes(&[0x8B, 0x4C, 0x8E, 0x04]); // mov ecx, [rsi + rcx*4 + 4]
        self.bytes(&[0x29, 0xC1]); // sub ecx, eax
        self.bytes(&[0x81, 0xF9]); // cmp ecx, method
        self.bytes(&(method as u32).to_le_bytes());
        self.jcc(CC_BE, slow);
        self.bytes(&[0x05]); // add eax, method
        self.bytes(&(method as u32).to_le_bytes());
        self.load(RSI, RBX, layout::METHOD_PCS);
        self.bytes(&[0x8B, 0x34, 0x86]); // mov esi, [rsi + rax*4]
        // The arguments out of the way, the captures in, the arguments after.
        let scratch = crate::vm::SCRATCH as u32;
        for j in 0..argc {
            self.load(RAX, R12, Self::slot(base as u32 + j));
            self.save(RAX, R12, (scratch + j) * 8);
        }
        let top = self.label();
        let done = self.label();
        self.bytes(&[0x31, 0xC9]); // xor ecx, ecx
        self.bind(top);
        self.bytes(&[0x48, 0x39, 0xF9]); // cmp rcx, rdi
        self.jcc(CC_AE, done);
        self.bytes(&[0x48, 0x8B, 0x44, 0xCA, 0x10]); // mov rax, [rdx + rcx*8 + 16]
        self.bytes(&[0x49, 0x89, 0x04, 0xCC]); // mov [r12 + rcx*8], rax
        self.bytes(&[0x48, 0xFF, 0xC1]); // inc rcx
        self.jump(top);
        self.bind(done);
        for j in 0..argc {
            self.load(RAX, R12, (scratch + j) * 8);
            self.bytes(&[0x49, 0x89, 0x84, 0xFC]); // mov [r12 + rdi*8 + j*8], rax
            self.bytes(&(j * 8).to_le_bytes());
        }
        self.bytes(&[0x48, 0x8D, 0x87]); // lea rax, [rdi + argc]
        self.bytes(&argc.to_le_bytes());
        self.save(RAX, RBX, layout::LIVE);
        if self.chain {
            let out = self.label();
            self.budget(out);
            self.load(RAX, RBX, layout::NATIVE_TABLE);
            self.bytes(&[0x48, 0x8B, 0x04, 0xF0]); // mov rax, [rax + rsi*8]
            self.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
            self.jcc(CC_E, out);
            self.enter_warm();
            self.bind(out);
        }
        self.save(RSI, RBX, layout::PC);
        self.ret_as_is(crate::abi::JUMPED);
    }

    fn alloc(&mut self, a: Reg, header: [u64; 2], base: Reg, n: u32, slow: Label) {
        let size = 2 + n;
        self.load(RAX, RBX, layout::TOP);
        self.bytes(&[0x48, 0x8D, 0x88]); // lea rcx, [rax + size]
        self.bytes(&size.to_le_bytes());
        self.bytes(&[0x48, 0x3B, 0x8B]); // cmp rcx, [rbx + CAP]
        self.bytes(&layout::CAP.to_le_bytes());
        self.jcc(CC_A, slow);
        // A heap that has put enough into regions wants a collection first.
        self.load(2, RBX, layout::REGION_GROWTH); // rdx
        self.bytes(&[0x48, 0x3B, 0x93]); // cmp rdx, [rbx + CAP]
        self.bytes(&layout::CAP.to_le_bytes());
        self.jcc(CC_A, slow);
        self.load(2, RBX, layout::BASE);
        self.bytes(&[0x48, 0x8D, 0x14, 0xC2]); // lea rdx, [rdx + rax*8]
        self.imm(RSI, header[0]);
        self.bytes(&[0x48, 0x89, 0x32]); // mov [rdx], rsi
        self.imm(RSI, header[1]);
        self.bytes(&[0x48, 0x89, 0x72, 0x08]); // mov [rdx + 8], rsi
        for j in 0..n {
            self.get(RSI, base as u32 + j);
            self.bytes(&[0x48, 0x89, 0xB2]); // mov [rdx + (2 + j) * 8], rsi
            self.bytes(&((2 + j) * 8).to_le_bytes());
        }
        self.save(RCX, RBX, layout::TOP);
        self.bytes(&[0x48, 0x81, 0x83]); // add qword [rbx + ALLOCATED], size
        self.bytes(&layout::ALLOCATED.to_le_bytes());
        self.bytes(&size.to_le_bytes());
        self.store(a);
    }

    fn exec(&mut self, pc: Pc) {
        self.flush_steps();
        self.spill();
        self.bytes(&[0x48, 0x89, 0xDF]); // mov rdi, rbx
        self.bytes(&[0xB8 + RSI]); // mov esi, pc
        self.bytes(&pc.to_le_bytes());
        self.bytes(&[0xFF]); // call [rbx + EXEC]
        self.mem(2, RBX, layout::EXEC);
        self.bytes(&[0x85, 0xC0]); // test eax, eax
        let ret = match self.ret_eax {
            Some(l) => l,
            None => {
                let l = self.label();
                self.ret_eax = Some(l);
                l
            }
        };
        self.jcc(CC_NE, ret);
        self.reload();
    }

    fn finish(mut self) -> Vec<u8> {
        for (at, l) in std::mem::take(&mut self.fixups) {
            let to = self.labels[l.0].expect("every label is bound");
            let delta = to as i64 - (at as i64 + 4);
            let delta = i32::try_from(delta).expect("a branch too far");
            self.code[at..at + 4].copy_from_slice(&delta.to_le_bytes());
        }
        self.code
    }
}
