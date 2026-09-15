//! A [`Program`] as bytes: what a native executable carries, so the runtime
//! it links against has the program's tables -- its constants, method tables,
//! primitives and messages -- without the compiler.
//!
//! Little-endian, lengths as `u32`, strings as UTF-8 behind their length. The
//! debug information is not written: a native executable has no debugger to
//! give it to yet.

use crate::{Const, Instr, Program};
use meadow_core::{Prim, num::Width};
use meadow_intern::InternedString;

const MAGIC: &[u8; 8] = b"MDWIMG03";

/// `program`, as bytes [`decode`] reads back.
pub fn encode(program: &Program) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.u32(program.code.len() as u32);
    for i in &program.code {
        w.0.extend_from_slice(&i.encode());
    }
    w.u32(program.consts.len() as u32);
    for c in &program.consts {
        w.konst(c);
    }
    w.u32(program.methods.len() as u32);
    for m in &program.methods {
        w.u32s(m);
    }
    w.u32(program.shapes.len() as u32);
    for s in &program.shapes {
        w.strs(s);
    }
    w.strs(&program.labels);
    w.u32(program.prims.len() as u32);
    for p in &program.prims {
        w.u16(p.code());
    }
    w.u32(program.ops.len() as u32);
    for (e, o) in &program.ops {
        w.str(e);
        w.str(o);
    }
    w.strs(&program.ctors);
    let mut fields: Vec<_> = program.ctor_fields.iter().collect();
    fields.sort_by(|a, b| a.0.cmp(b.0));
    w.u32(fields.len() as u32);
    for (ctor, names) in fields {
        w.str(ctor);
        w.strs(names);
    }
    w.u32(program.messages.len() as u32);
    for m in &program.messages {
        w.str(m);
    }
    w.u32s(&program.entries);
    match program.entry {
        Some(pc) => {
            w.0.push(1);
            w.u32(pc);
        }
        None => w.0.push(0),
    }
    w.u16(program.regs);
    w.u32(program.gc_maps.len() as u32);
    for m in &program.gc_maps {
        w.u32(m.regs.len() as u32);
        for (r, held) in &m.regs {
            w.0.push(*r);
            match held {
                crate::Held::Ref => w.0.push(0),
                crate::Held::Scalar => w.0.push(1),
                crate::Held::Var(d) => {
                    w.0.push(2);
                    w.0.push(*d);
                }
                crate::Held::Any => w.0.push(3),
            }
        }
    }
    w.u32s(&program.gc_at);
    w.u32s(&program.operands_at);
    w.u32(program.operands.len() as u32);
    for d in &program.operands {
        w.u16(*d);
    }
    w.u32(program.results.len() as u32);
    w.0.extend(program.results.iter().map(|d| *d as u8));
    w.0.push(program.entry_result as u8);
    w.0
}

/// The program [`encode`] wrote.
pub fn decode(bytes: &[u8]) -> Result<Program, String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(8)? != MAGIC {
        return Err("not a Meadow image, or one from another version".into());
    }
    let mut p = Program::default();
    for _ in 0..r.u32()? {
        let b = r.take(8)?;
        let instr = Instr::decode(b.try_into().expect("eight bytes"))
            .ok_or_else(|| format!("unknown opcode {}", b[0]))?;
        p.code.push(instr);
    }
    for _ in 0..r.u32()? {
        let c = r.konst()?;
        p.consts.push(c);
    }
    for _ in 0..r.u32()? {
        let m = r.u32s()?;
        p.methods.push(m);
    }
    for _ in 0..r.u32()? {
        let s = r.strs()?;
        p.shapes.push(s);
    }
    p.labels = r.strs()?;
    for _ in 0..r.u32()? {
        let c = r.u16()?;
        p.prims
            .push(Prim::from_code(c).ok_or_else(|| format!("unknown primitive {c}"))?);
    }
    for _ in 0..r.u32()? {
        let e = r.str()?;
        let o = r.str()?;
        p.ops.push((e, o));
    }
    p.ctors = r.strs()?;
    for _ in 0..r.u32()? {
        let ctor = r.str()?;
        let names = r.strs()?;
        p.ctor_fields.insert(ctor, names);
    }
    for _ in 0..r.u32()? {
        let m = r.str()?;
        p.messages.push(m.to_string());
    }
    p.entries = r.u32s()?;
    p.entry = match r.take(1)?[0] {
        0 => None,
        _ => Some(r.u32()?),
    };
    p.regs = r.u16()?;
    for _ in 0..r.u32()? {
        let mut regs = Vec::new();
        for _ in 0..r.u32()? {
            let reg = r.take(1)?[0];
            let held = match r.take(1)?[0] {
                0 => crate::Held::Ref,
                1 => crate::Held::Scalar,
                2 => crate::Held::Var(r.take(1)?[0]),
                3 => crate::Held::Any,
                t => return Err(format!("unknown register kind {t}")),
            };
            regs.push((reg, held));
        }
        p.gc_maps.push(crate::GcMap { regs });
    }
    p.gc_at = r.u32s()?;
    p.operands_at = r.u32s()?;
    for _ in 0..r.u32()? {
        let d = r.u16()?;
        p.operands.push(d);
    }
    let n = r.u32()? as usize;
    p.results = r
        .take(n)?
        .iter()
        .map(|b| *b as meadow_core::desc::Desc)
        .collect();
    p.entry_result = r.take(1)?[0] as meadow_core::desc::Desc;
    if r.at != bytes.len() {
        return Err("trailing bytes after the image".into());
    }
    Ok(p)
}

struct Writer(Vec<u8>);

impl Writer {
    fn u16(&mut self, x: u16) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u32(&mut self, x: u32) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn u64(&mut self, x: u64) {
        self.0.extend_from_slice(&x.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, ss: &[InternedString]) {
        self.u32(ss.len() as u32);
        for s in ss {
            self.str(s);
        }
    }
    fn u32s(&mut self, xs: &[u32]) {
        self.u32(xs.len() as u32);
        for x in xs {
            self.u32(*x);
        }
    }
    fn konst(&mut self, c: &Const) {
        match c {
            Const::Unit => self.0.push(0),
            Const::Bool(b) => {
                self.0.push(1);
                self.0.push(*b as u8);
            }
            Const::Int(n) => {
                self.0.push(2);
                self.u64(*n as u64);
            }
            Const::BigInt(n) => {
                self.0.push(3);
                self.u64(*n as u64);
            }
            Const::Float(x) => {
                self.0.push(4);
                self.u64(x.to_bits());
            }
            Const::Word(w, bits) => {
                self.0.push(5);
                self.0.push(*w as u8);
                self.u64(*bits);
            }
            Const::Float32(x) => {
                self.0.push(6);
                self.u32(x.to_bits());
            }
            Const::Str(s) => {
                self.0.push(7);
                self.str(s);
            }
            Const::Char(ch) => {
                self.0.push(8);
                self.u32(*ch as u32);
            }
            Const::Text(s) => {
                self.0.push(9);
                self.str(s);
            }
        }
    }
}

struct Reader<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.bytes.len());
        let end = end.ok_or("the image ends too soon")?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }
    fn str(&mut self) -> Result<InternedString, String> {
        let n = self.u32()? as usize;
        let s = std::str::from_utf8(self.take(n)?).map_err(|e| e.to_string())?;
        Ok(InternedString::from(s))
    }
    fn strs(&mut self) -> Result<Vec<InternedString>, String> {
        (0..self.u32()?).map(|_| self.str()).collect()
    }
    fn u32s(&mut self) -> Result<Vec<u32>, String> {
        (0..self.u32()?).map(|_| self.u32()).collect()
    }
    fn konst(&mut self) -> Result<Const, String> {
        Ok(match self.take(1)?[0] {
            0 => Const::Unit,
            1 => Const::Bool(self.take(1)?[0] != 0),
            2 => Const::Int(self.u64()? as i64),
            3 => Const::BigInt(self.u64()? as i64),
            4 => Const::Float(f64::from_bits(self.u64()?)),
            5 => {
                let w = self.take(1)?[0] as usize;
                let w = *Width::ALL.get(w).ok_or("unknown width")?;
                Const::Word(w, self.u64()?)
            }
            6 => Const::Float32(f32::from_bits(self.u32()?)),
            7 => Const::Str(self.str()?),
            8 => Const::Char(char::from_u32(self.u32()?).ok_or("not a char")?),
            9 => Const::Text(self.str()?),
            t => return Err(format!("unknown constant tag {t}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Op;

    #[test]
    fn a_program_comes_back_as_it_went() {
        let mut p = Program::default();
        p.code = vec![Instr::new(Op::Prim2, 1, 2, 3, 4), Instr::ai(Op::Halt, 0, 0)];
        p.consts = vec![
            Const::Unit,
            Const::Bool(true),
            Const::Int(-7),
            Const::BigInt(9),
            Const::Float(1.5),
            Const::Word(Width::U8, 255),
            Const::Float32(2.5),
            Const::Str("héllo".into()),
            Const::Char('λ'),
        ];
        p.methods = vec![vec![0, 1], vec![]];
        p.shapes = vec![vec!["x".into(), "y".into()]];
        p.labels = vec!["x".into()];
        p.prims = vec![Prim::Add, Prim::ToWord(Width::I16), Prim::TakeOnce];
        p.ops = vec![("Console".into(), "writeOutput".into())];
        p.ctors = vec!["Nil".into(), "Cons".into()];
        p.ctor_fields
            .insert("Point".into(), vec!["x".into(), "y".into()]);
        p.messages = vec!["non-exhaustive pattern match".into()];
        p.entries = vec![0];
        p.entry = Some(0);
        p.regs = 12;
        p.gc_maps = vec![crate::GcMap {
            regs: vec![
                (0, crate::Held::Ref),
                (3, crate::Held::Var(9)),
                (5, crate::Held::Any),
                (4, crate::Held::Scalar),
            ],
        }];
        p.gc_at = vec![0, crate::NO_MAP];
        p.operands_at = vec![crate::NO_OPERANDS, 0];
        p.operands = vec![
            meadow_core::desc::INT as crate::DescSrc,
            crate::DESC_REG + 3,
        ];
        p.results = vec![meadow_core::desc::REF, meadow_core::desc::STR];
        p.entry_result = meadow_core::desc::UNIT;
        let back = decode(&encode(&p)).expect("decodes");
        assert_eq!(format!("{:?}", back.code), format!("{:?}", p.code));
        assert_eq!(back.consts, p.consts);
        assert_eq!(back.methods, p.methods);
        assert_eq!(back.shapes, p.shapes);
        assert_eq!(back.labels, p.labels);
        assert_eq!(back.prims, p.prims);
        assert_eq!(back.ops, p.ops);
        assert_eq!(back.ctors, p.ctors);
        assert_eq!(back.ctor_fields, p.ctor_fields);
        assert_eq!(back.messages, p.messages);
        assert_eq!(
            (back.entries, back.entry, back.regs),
            (p.entries, p.entry, p.regs)
        );
        assert_eq!((back.gc_maps, back.gc_at), (p.gc_maps, p.gc_at));
        assert_eq!(
            (
                back.operands_at,
                back.operands,
                back.results,
                back.entry_result
            ),
            (p.operands_at, p.operands, p.results, p.entry_result)
        );
    }

    #[test]
    fn a_truncated_image_is_refused() {
        let bytes = encode(&Program::default());
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(b"not an image at all").is_err());
    }
}
