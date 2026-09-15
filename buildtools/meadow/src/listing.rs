//! **Programs as text**: what `--emit bytecode` and `--emit asm` write.
//!
//! A build makes two things a person might want to read: the bytecode image
//! the VM runs, and the machine code an `aot` build compiles from it. Both are
//! binary where they are used. This is them written out -- the image as
//! [`meadow_bytecode::Program::disassemble`] prints it, and the machine code
//! decoded back into instructions, one function per block of the image.
//!
//! The assembly is a listing, not a source file to assemble: every address is
//! an offset into the program's code, and what the code reaches outside it --
//! the runtime, the image -- is found through the machine's tables at run time,
//! which an object file's relocations are not needed for.

use meadow_bytecode::{Pc, Program};
use meadow_compiler::OptLevel;
use meadow_rts::codegen::{self, Arch, Compiled};
use std::fmt::Write;

/// What a build writes for a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emit {
    /// The bytecode image, `<name>.mbc`: what the VM runs, and what `meadow
    /// exec` and `meadow link` take.
    Image,
    /// The image as text, `<name>.mbc.txt`.
    Bytecode,
    /// The native code as text, `<name>.s`.
    Asm,
    /// A native executable, linked.
    Exe,
}

impl Emit {
    pub const ALL: [Emit; 4] = [Emit::Image, Emit::Bytecode, Emit::Asm, Emit::Exe];

    pub fn parse(s: &str) -> Option<Emit> {
        Self::ALL.into_iter().find(|e| e.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Emit::Image => "image",
            Emit::Bytecode => "bytecode",
            Emit::Asm => "asm",
            Emit::Exe => "exe",
        }
    }
}

/// The image as text.
pub fn bytecode(image: &Program) -> String {
    image.disassemble()
}

/// The machine code `image` compiles to for `arch` at `opt`, as text.
pub fn asm(image: &Program, arch: Arch, opt: OptLevel) -> String {
    listing(&codegen::compile(image, arch, opt), opt)
}

/// Compiled native code as text: a label for each block's function, and each
/// instruction at its offset, with its bytes.
pub fn listing(compiled: &Compiled, opt: OptLevel) -> String {
    let arch = match compiled.arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    };
    let mut starts: Vec<(u32, Pc)> = compiled.blocks.iter().map(|&(pc, at)| (at, pc)).collect();
    starts.sort();
    starts.dedup_by_key(|(at, _)| *at);
    let places = Places {
        starts: starts.clone(),
        len: compiled.code.len() as u64,
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "; meadow native code: {arch}, -{}, {} functions, {} bytes",
        opt.name(),
        starts.len(),
        compiled.code.len()
    );
    let _ = writeln!(
        out,
        "; `fn_pcN` is the function for the bytecode block at pc N, and `fn_pcN+k` k bytes into it; \
         addresses are offsets into the code"
    );
    let len = compiled.code.len() as u32;
    // Code before the first function is the program's own, shared by them.
    let mut regions: Vec<(u32, u32, Option<Pc>)> = Vec::new();
    if starts.first().is_none_or(|&(at, _)| at > 0) {
        regions.push((0, starts.first().map_or(len, |&(at, _)| at), None));
    }
    for (i, &(at, pc)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(len, |&(next, _)| next);
        regions.push((at, end, Some(pc)));
    }
    for (start, end, pc) in regions {
        if start == end {
            continue;
        }
        out.push('\n');
        match pc {
            Some(pc) => {
                let _ = writeln!(out, "{}:", label(pc));
            }
            None => {
                let _ = writeln!(out, "; shared");
            }
        }
        let code = &compiled.code[start as usize..end as usize];
        match compiled.arch {
            Arch::X86_64 => x86_64(&mut out, code, start as u64, &places),
            Arch::Aarch64 => aarch64(&mut out, code, start as u64, &places),
        }
    }
    out
}

fn label(pc: Pc) -> String {
    format!("fn_pc{pc}")
}

/// One instruction: its offset, its bytes, and what it is.
fn line(out: &mut String, at: u64, bytes: &[u8], text: &str) {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = writeln!(out, "  {at:06x}  {:<24} {text}", hex.join(" "));
}

/// The functions of the code, by where they start.
#[derive(Clone)]
struct Places {
    starts: Vec<(u32, Pc)>,
    len: u64,
}

impl Places {
    /// An offset into the code, as the function it is in and how far in: a
    /// branch to another function lands past the part of its prologue that
    /// makes the frame, not at its start.
    fn name(&self, addr: u64) -> Option<String> {
        if addr >= self.len {
            return None;
        }
        let i = self.starts.partition_point(|&(at, _)| at as u64 <= addr);
        let (at, pc) = *self.starts.get(i.checked_sub(1)?)?;
        Some(match addr - at as u64 {
            0 => label(pc),
            k => format!("{}+{k:#x}", label(pc)),
        })
    }
}

/// Names what a branch or call lands on.
struct Labels(Places);

impl iced_x86::SymbolResolver for Labels {
    fn symbol(
        &mut self,
        instruction: &iced_x86::Instruction,
        _operand: u32,
        instruction_operand: Option<u32>,
        address: u64,
        _address_size: u32,
    ) -> Option<iced_x86::SymbolResult<'_>> {
        // Only where the operand is a place to go: a displacement or a
        // constant that happens to equal a function's offset is not one.
        let op = instruction_operand?;
        if instruction.op_kind(op) != iced_x86::OpKind::NearBranch64 {
            return None;
        }
        self.0
            .name(address)
            .map(|name| iced_x86::SymbolResult::with_string(address, name))
    }
}

fn x86_64(out: &mut String, code: &[u8], base: u64, places: &Places) {
    use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};
    let mut decoder = Decoder::with_ip(64, code, base, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::with_options(Some(Box::new(Labels(places.clone()))), None);
    formatter.options_mut().set_first_operand_char_index(8);
    let mut text = String::new();
    while decoder.can_decode() {
        let from = decoder.position();
        let instr = decoder.decode();
        let bytes = &code[from..decoder.position()];
        text.clear();
        if instr.is_invalid() {
            text.push_str(&format!(".byte {:#04x}", bytes[0]));
        } else {
            formatter.format(&instr, &mut text);
        }
        line(out, base + from as u64, bytes, &text);
    }
}

fn aarch64(out: &mut String, code: &[u8], base: u64, places: &Places) {
    use yaxpeax_arch::{Decoder, U8Reader};
    use yaxpeax_arm::armv8::a64::{InstDecoder, Opcode, Operand};
    let decoder = InstDecoder::default();
    for (i, word) in code.chunks(4).enumerate() {
        let at = base + 4 * i as u64;
        if word.len() < 4 {
            line(out, at, word, ".byte");
            continue;
        }
        let mut reader = U8Reader::new(word);
        let text = match decoder.decode(&mut reader) {
            Ok(instr) => {
                let mut text = instr.to_string();
                // Where a pc-relative operand points.
                let target = instr.operands.iter().find_map(|o| match o {
                    Operand::PCOffset(off) if instr.opcode == Opcode::ADRP => {
                        Some((at & !0xfff).wrapping_add(*off as u64))
                    }
                    Operand::PCOffset(off) => Some(at.wrapping_add(*off as u64)),
                    _ => None,
                });
                if let Some(to) = target {
                    match places.name(to) {
                        Some(name) => text.push_str(&format!("  ; {name}")),
                        None => text.push_str(&format!("  ; {to:#x}")),
                    }
                }
                text
            }
            Err(_) => format!(
                ".word {:#010x}",
                u32::from_le_bytes(word.try_into().unwrap())
            ),
        };
        line(out, at, word, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_name_it_is_parsed_from() {
        for e in Emit::ALL {
            assert_eq!(Emit::parse(e.name()), Some(e));
        }
        assert_eq!(Emit::parse("elf"), None);
    }
}
