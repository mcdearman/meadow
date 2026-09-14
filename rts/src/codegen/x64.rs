//! x86-64 (System V): instructions encoded by hand.
//!
//! Machine registers during a block function: `rbx` the machine, `r12` its
//! register file, `r13` steps not yet counted in the machine -- saved by the
//! callee, so they survive the calls into the interpreter -- `r14` the loop
//! iterations this call has made, and `rax`, `rcx`,
//! `rdx`, `xmm0` and `xmm1` for the instruction being done.

use super::{Emit, FloatOp, IntOp, Label, Operand, layout};
use meadow_bytecode::{Cond, Pc, Reg};

const RAX: u8 = 0;
const RCX: u8 = 1;
const RBX: u8 = 3;
const RSI: u8 = 6;
const R12: u8 = 12;
const R13: u8 = 13;

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

    fn slot(r: Reg) -> u32 {
        r as u32 * 8
    }

    /// `rcx = c`
    fn operand(&mut self, c: Operand) {
        match c {
            Operand::Reg(r) => self.load(RCX, R12, Self::slot(r)),
            Operand::Imm(n) => self.imm(RCX, n as u64),
        }
    }

    fn raise_live(&mut self, a: Reg) {
        self.load(RCX, RBX, layout::LIVE);
        self.bytes(&[0xBA]); // mov edx, a + 1
        self.bytes(&(a as u32 + 1).to_le_bytes());
        self.bytes(&[0x48, 0x39, 0xD1]); // cmp rcx, rdx
        self.bytes(&[0x48, 0x0F, 0x42, 0xCA]); // cmovb rcx, rdx
        self.save(RCX, RBX, layout::LIVE);
    }

    /// `r[a] = rax`, raising `live`.
    fn store(&mut self, a: Reg) {
        self.save(RAX, R12, Self::slot(a));
        self.raise_live(a);
    }

    fn flush_steps(&mut self) {
        // add [rbx + STEPS], r13
        self.rex_w(R13, RBX);
        self.bytes(&[0x01]);
        self.mem(R13, RBX, layout::STEPS);
        self.bytes(&[0x45, 0x31, 0xED]); // xor r13d, r13d
    }

    fn restore_and_return(&mut self) {
        self.bytes(&[0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C, 0x5B, 0x5D, 0xC3]);
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
        self.load(RAX, R12, Self::slot(x));
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

    /// `movsd xmm, [r12 + slot]`, or the store the other way.
    fn movsd(&mut self, xmm: u8, r: Reg, store: bool) {
        self.bytes(&[0xF2, 0x41, 0x0F, if store { 0x11 } else { 0x10 }]);
        self.mem(xmm, R12, Self::slot(r));
    }
}

impl Emit for Asm {
    fn new() -> Asm {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            ret_eax: None,
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
        // push rbp; mov rbp, rsp; push rbx; push r12; push r13; push r14 --
        // six words on the stack with the return address, so it stays aligned
        // for the calls.
        self.bytes(&[
            0x55, 0x48, 0x89, 0xE5, 0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56,
        ]);
        self.bytes(&[0x48, 0x89, 0xFB]); // mov rbx, rdi
        self.load(R12, RBX, layout::REGS);
        self.bytes(&[0x45, 0x31, 0xED]); // xor r13d, r13d
        self.bytes(&[0x45, 0x31, 0xF6]); // xor r14d, r14d
        self.ret_eax = None;
    }

    fn ret(&mut self, status: u32) {
        self.flush_steps();
        self.bytes(&[0xB8]); // mov eax, status
        self.bytes(&status.to_le_bytes());
        self.restore_and_return();
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

    fn jump(&mut self, to: Label) {
        self.bytes(&[0xE9]);
        self.rel32(to);
    }

    fn mov(&mut self, a: Reg, b: Reg) {
        self.load(RAX, R12, Self::slot(b));
        self.store(a);
    }

    fn word(&mut self, a: Reg, w: u64) {
        self.imm(RAX, w);
        self.store(a);
    }

    fn int(&mut self, op: IntOp, a: Reg, b: Reg, c: Operand, zero: Label) {
        self.load(RAX, R12, Self::slot(b));
        self.operand(c);
        match op {
            IntOp::Add => self.bytes(&[0x48, 0x01, 0xC8]), // add rax, rcx
            IntOp::Sub => self.bytes(&[0x48, 0x29, 0xC8]), // sub rax, rcx
            IntOp::Mul => self.bytes(&[0x48, 0x0F, 0xAF, 0xC1]), // imul rax, rcx
            IntOp::Div | IntOp::Rem => {
                self.bytes(&[0x48, 0x85, 0xC9]); // test rcx, rcx
                self.jcc(CC_E, zero);
                // `idiv` traps on MIN / -1, where `wrapping_div` gives MIN and
                // `wrapping_rem` 0 -- which is `-x` and `0` for any `x`.
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
        self.store(a);
    }

    fn float(&mut self, op: FloatOp, a: Reg, b: Reg, c: Reg) {
        self.movsd(0, b, false);
        let opcode = match op {
            FloatOp::Add => 0x58,
            FloatOp::Sub => 0x5C,
            FloatOp::Mul => 0x59,
            FloatOp::Div => 0x5E,
        };
        self.bytes(&[0xF2, 0x41, 0x0F, opcode]); // xmm0 op= [r12 + c]
        self.mem(0, R12, Self::slot(c));
        self.movsd(0, a, true);
        self.raise_live(a);
    }

    fn cmp_int(&mut self, cond: Cond, a: Reg, b: Reg, c: Operand) {
        self.load(RAX, R12, Self::slot(b));
        self.operand(c);
        self.bytes(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
        self.bytes(&[0x0F, 0x90 | Self::int_cc(cond), 0xC0]); // setcc al
        self.bytes(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        self.store(a);
    }

    fn cmp_float(&mut self, cond: Cond, a: Reg, b: Reg, c: Reg) {
        self.movsd(0, b, false);
        self.movsd(1, c, false);
        self.float_bool(cond);
        self.bytes(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
        self.store(a);
    }

    fn branch_int(&mut self, cond: Cond, x: Reg, y: Operand, to: Label) {
        self.load(RAX, R12, Self::slot(x));
        self.operand(y);
        self.bytes(&[0x48, 0x39, 0xC8]); // cmp rax, rcx
        self.jcc(Self::int_cc(cond) ^ 1, to);
    }

    fn branch_float(&mut self, cond: Cond, x: Reg, y: Reg, to: Label) {
        self.movsd(0, x, false);
        self.movsd(1, y, false);
        self.float_bool(cond);
        self.bytes(&[0x84, 0xC0]); // test al, al
        self.jcc(CC_E, to);
    }

    fn branch_zero(&mut self, x: Reg, to: Label) {
        self.load(RAX, R12, Self::slot(x));
        self.bytes(&[0x48, 0x85, 0xC0]); // test rax, rax
        self.jcc(CC_E, to);
    }

    fn set_live(&mut self, n: u32) {
        self.save_imm(RBX, layout::LIVE, n);
    }

    fn back_edge(&mut self, to: Label, over: Label) {
        self.bytes(&[0x49, 0xFF, 0xC6]); // inc r14
        // cmp r14, BACK_EDGES
        self.bytes(&[0x49, 0x81, 0xFE]);
        self.bytes(&super::BACK_EDGES.to_le_bytes());
        self.jcc(CC_AE, over);
        self.jump(to);
    }

    fn unstep(&mut self) {
        self.bytes(&[0x49, 0xFF, 0xCD]); // dec r13
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
        self.nursery(obj, slow);
        self.bytes(&[0x83, 0xF9, crate::heap::Kind::Closure as u8]); // cmp ecx, Closure
        self.jcc(CC_NE, slow);
        self.length();
        self.bytes(&[0x48, 0x83, 0xFF, 0x08]); // cmp rdi, 8
        self.jcc(CC_A, slow);
        // The method's pc: table `meta`, entry `method`, if it has one.
        self.bytes(&[0x8B, 0x4A, 0x08]); // mov ecx, [rdx + 8]
        self.bytes(&[0x4C, 0x8B, 0x83]); // mov r8, [rbx + METHOD_STARTS]
        self.bytes(&layout::METHOD_STARTS.to_le_bytes());
        self.bytes(&[0x41, 0x8B, 0x04, 0x88]); // mov eax, [r8 + rcx*4]
        self.bytes(&[0x45, 0x8B, 0x4C, 0x88, 0x04]); // mov r9d, [r8 + rcx*4 + 4]
        self.bytes(&[0x41, 0x29, 0xC1]); // sub r9d, eax
        self.bytes(&[0x41, 0x81, 0xF9]); // cmp r9d, method
        self.bytes(&(method as u32).to_le_bytes());
        self.jcc(CC_BE, slow);
        self.bytes(&[0x05]); // add eax, method
        self.bytes(&(method as u32).to_le_bytes());
        self.bytes(&[0x4C, 0x8B, 0x83]); // mov r8, [rbx + METHOD_PCS]
        self.bytes(&layout::METHOD_PCS.to_le_bytes());
        self.bytes(&[0x45, 0x8B, 0x14, 0x80]); // mov r10d, [r8 + rax*4]
        // The arguments out of the way, the captures in, the arguments after.
        let scratch = crate::vm::SCRATCH as u32;
        for j in 0..argc {
            self.load(RAX, R12, Self::slot(base) + j * 8);
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
        self.bytes(&[0x4C, 0x89, 0x93]); // mov [rbx + PC], r10
        self.bytes(&layout::PC.to_le_bytes());
        self.ret(crate::abi::JUMPED);
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
            self.load(RSI, R12, Self::slot(base) + j * 8);
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
