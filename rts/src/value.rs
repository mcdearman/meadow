//! Runtime values.
//!
//! What the machine stores is a [`Word`]: a register holds one, and so does
//! every field of every object. A `Value` is a word *with what it is* -- the
//! form the primitives, `show` and the natives work in -- made from a word and
//! a descriptor ([`Value::from_bits`]) and back ([`Value::bits`]) where the
//! compiler or an object header says what the word is.
//!
//! A `Value` is `Copy`, and copying one never touches the heap. Everything
//! with a payload is an [`Addr`], an index into [`crate::heap::Heap`], and the
//! collector is what decides when the thing at that address goes away.
//!
//! That is the whole difference from `meadow_eval`, which reference-counts. Two
//! problems went away with the `Rc`s:
//!
//! * **Dropping is not recursive any more.** A `Cons` chain of a million
//!   elements used to unwind a million `Drop`s and overflow the stack, which is
//!   why the CEK's constructor payload has a hand-written iterative `Drop`.
//!   Nothing here has a destructor at all.
//! * **Cycles are collectable.** Reference counting cannot free a resumption
//!   that closes over a handler that holds the resumption; a copying collector
//!   never sees the question.
//!
//! What it costs is that a `Value` only means something *next to its heap*.

use meadow_core::desc::Desc;
use meadow_core::num::Width;
use meadow_intern::InternedString;

/// One machine word: what a heap slot holds.
pub type Word = u64;

/// An index into the heap's slot array. Not a pointer: the collector moves
/// objects, and every live `Addr` is rewritten when it does.
pub type Addr = u32;

/// # Layout
///
/// Fixed, for native code that is handed one: `repr(u8)` makes the variant's
/// number the first byte, and each variant's fields follow at their C offsets
/// -- see [`layout`]. 16 bytes whatever the variant. Registers and fields are
/// not `Value`s but [`Word`]s.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Value {
    Unit = layout::UNIT,
    Bool(bool) = layout::BOOL,
    Int(i64) = layout::INT,
    /// A sized integer -- its width and its bits, masked to it. Immediate like
    /// an `Int`, so it lives in a register or a field with no allocation.
    Word(meadow_core::num::Width, u64) = layout::WORD,
    Float(f64) = layout::FLOAT,
    Float32(f32) = layout::FLOAT32,
    Char(char) = layout::CHAR,
    /// Interned, and therefore not on the collected heap. Strings the program
    /// builds at run time are interned too, so they accumulate for the life of
    /// the process — the same as in the CEK machine, and the one allocation this
    /// runtime does not manage.
    Str(InternedString) = layout::STR,
    Obj(Addr) = layout::OBJ,
}

/// Where a [`Value`]'s parts are, for code that reads them as bytes.
pub mod layout {
    /// The variant, as its first byte.
    pub const TAG: usize = 0;
    pub const UNIT: u8 = 0;
    pub const BOOL: u8 = 1;
    pub const INT: u8 = 2;
    pub const WORD: u8 = 3;
    pub const FLOAT: u8 = 4;
    pub const FLOAT32: u8 = 5;
    pub const CHAR: u8 = 6;
    pub const STR: u8 = 7;
    pub const OBJ: u8 = 8;
    /// A `Bool`'s byte, and a `Word`'s width.
    pub const BYTE: usize = 1;
    /// A `Float32`, a `Char`, a `Str`'s key and an `Obj`'s address: 32 bits.
    pub const HALF: usize = 4;
    /// An `Int`, a `Float`, and a `Word`'s bits: 64 bits.
    pub const WIDE: usize = 8;
    pub const SIZE: usize = 16;
}

impl Value {
    pub fn addr(self) -> Option<Addr> {
        match self {
            Value::Obj(a) => Some(a),
            _ => None,
        }
    }

    /// The descriptor of this value's representation -- see
    /// [`meadow_core::desc`]. What a heap object records for each field, so
    /// that the field can be one word.
    pub fn desc(self) -> Desc {
        use meadow_core::desc;
        match self {
            Value::Unit => desc::UNIT,
            Value::Bool(_) => desc::BOOL,
            Value::Int(_) => desc::INT,
            Value::Word(w, _) => desc::word(w),
            Value::Float(_) => desc::FLOAT,
            Value::Float32(_) => desc::FLOAT32,
            Value::Char(_) => desc::CHAR,
            Value::Str(_) => desc::STR,
            Value::Obj(_) => desc::REF,
        }
    }

    /// The value as one word, with what it is left to its descriptor.
    #[inline]
    pub fn bits(self) -> Word {
        match self {
            Value::Unit => 0,
            Value::Bool(b) => b as Word,
            Value::Int(n) => n as Word,
            Value::Word(_, bits) => bits,
            Value::Float(x) => x.to_bits(),
            Value::Float32(x) => x.to_bits() as Word,
            Value::Char(c) => c as Word,
            Value::Str(s) => s.to_raw() as Word,
            Value::Obj(a) => a as Word,
        }
    }

    /// The value word `w` is, as descriptor `d` says. [`Value::bits`] undone.
    #[inline]
    pub fn from_bits(w: Word, d: Desc) -> Value {
        use meadow_core::desc;
        match d {
            desc::REF => Value::Obj(w as Addr),
            desc::INT => Value::Int(w as i64),
            desc::FLOAT => Value::Float(f64::from_bits(w)),
            desc::STR => Value::Str(
                InternedString::from_raw(w as u32).expect("a string's word is an interned key"),
            ),
            desc::UNIT => Value::Unit,
            desc::BOOL => Value::Bool(w != 0),
            desc::CHAR => Value::Char(char::from_u32(w as u32).expect("a character's word")),
            desc::FLOAT32 => Value::Float32(f32::from_bits(w as u32)),
            d if (desc::WORD..desc::WORD + Width::ALL.len() as Desc).contains(&d) => {
                Value::Word(Width::ALL[(d - desc::WORD) as usize], w)
            }
            d => panic!("a word described as {}, which says nothing", desc::name(d)),
        }
    }

    /// A short tag for introspection — what a register dump shows per slot.
    pub fn kind(self) -> &'static str {
        match self {
            Value::Unit => "()",
            Value::Bool(_) => "Bool",
            Value::Int(_) => "Int",
            Value::Word(w, _) => w.name(),
            Value::Float(_) => "Float",
            Value::Float32(_) => "Float32",
            Value::Char(_) => "Char",
            Value::Str(_) => "String",
            Value::Obj(_) => "object",
        }
    }
}

#[cfg(test)]
mod words {
    use super::*;

    #[test]
    fn a_value_is_its_word_and_its_descriptor() {
        for v in [
            Value::Unit,
            Value::Bool(true),
            Value::Int(-5),
            Value::Word(Width::I8, 0xFF),
            Value::Float(-0.5),
            Value::Float32(3.25),
            Value::Char('λ'),
            Value::Str("hello".into()),
            Value::Obj(1234),
        ] {
            assert_eq!(Value::from_bits(v.bits(), v.desc()), v);
        }
    }
}

#[cfg(test)]
mod size {
    /// Still two words with the sized numbers in it: everything the machine
    /// copies, it copies by value.
    #[test]
    fn a_value_is_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<super::Value>(), super::layout::SIZE);
    }

    /// What native code assumes about where things are, checked against what
    /// the compiler actually did.
    #[test]
    fn the_layout_is_where_native_code_looks() {
        use super::{Value, layout::*};
        use meadow_core::num::Width;
        let bytes = |v: Value| -> [u8; 16] {
            // Safety: a `Value` is 16 bytes of plain data.
            unsafe { std::mem::transmute(v) }
        };
        let at8 = |b: [u8; 16]| u64::from_le_bytes(b[WIDE..WIDE + 8].try_into().unwrap());
        let at4 = |b: [u8; 16]| u32::from_le_bytes(b[HALF..HALF + 4].try_into().unwrap());
        assert_eq!(bytes(Value::Unit)[TAG], UNIT);
        let b = bytes(Value::Bool(true));
        assert_eq!((b[TAG], b[BYTE]), (BOOL, 1));
        let b = bytes(Value::Int(-2));
        assert_eq!((b[TAG], at8(b)), (INT, (-2i64) as u64));
        let b = bytes(Value::Word(Width::U16, 0xBEEF));
        assert_eq!((b[TAG], b[BYTE], at8(b)), (WORD, Width::U16 as u8, 0xBEEF));
        let b = bytes(Value::Float(1.5));
        assert_eq!((b[TAG], at8(b)), (FLOAT, 1.5f64.to_bits()));
        let b = bytes(Value::Float32(2.5));
        assert_eq!((b[TAG], at4(b)), (FLOAT32, 2.5f32.to_bits()));
        let b = bytes(Value::Char('λ'));
        assert_eq!((b[TAG], at4(b)), (CHAR, 'λ' as u32));
        let b = bytes(Value::Obj(77));
        assert_eq!((b[TAG], at4(b)), (OBJ, 77));
        assert_eq!(bytes(Value::Str("x".into()))[TAG], STR);
    }
}
