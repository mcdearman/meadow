//! The structural hash behind the `hash` primitive.
//!
//! Every engine walks a value and feeds what it finds here, in the same order,
//! through the same calls -- so the three of them agree on every hash, which the
//! differential tests hold them to. Keeping the mixing in one place is what
//! makes that possible: an engine only decides *what* a value is, never how it
//! is folded into a number.
//!
//! The rule the walk has to follow is the one `==` follows, since two values
//! that compare equal must hash equal:
//!
//! * a `Vector` is the sequence it denotes, not the tree that holds it -- two
//!   equal vectors can have different shapes, so feed [`Tag::Vector`], the length
//!   and then the elements in order;
//! * a record's fields go in label order, compared as strings;
//! * a tuple is the constructor `#tuple`, which is what two of the engines call
//!   it anyway;
//! * `0.0` and `-0.0` are equal, so [`Hasher::float`] folds them together;
//! * a `Ref` compares by identity, and nothing about a cell's identity is
//!   stable across a moving collector, so a `Ref` cannot be hashed -- nor can a
//!   function, which has no `==` worth the name. Both are errors, spelled by
//!   [`unhashable`].
//!
//! The output is an `Int`, and the same on every platform: nothing here depends
//! on pointer width, endianness or a random seed.

/// What kind of value comes next. Part of the hash, so `()` and `0` and `""` do
/// not collide by construction.
#[derive(Debug, Clone, Copy)]
pub enum Tag {
    Int = 1,
    BigInt = 2,
    Float = 3,
    Bool = 4,
    Char = 5,
    Str = 6,
    Unit = 7,
    /// A constructor: its canonical name, its arity, then its fields.
    Data = 8,
    /// A `Vector`: its length, then its elements.
    Vector = 9,
    /// An `Array`: its length, then its elements.
    Array = 10,
    /// A record: its field count, then each label followed by its value.
    Record = 11,
    /// A compact region's handle: the value compacted, which follows. Equal
    /// handles are equal values, so they hash as their contents do, tagged.
    Compact = 12,
}

/// Accumulates one hash. FxHash's mixing step per word, and splitmix64's
/// finalizer at the end so that every bit of the result depends on every bit
/// fed in -- a hash table indexes by the low bits.
#[derive(Debug, Clone)]
pub struct Hasher {
    h: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Hasher {
        Hasher {
            h: 0x6a09_e667_f3bc_c908,
        }
    }

    pub fn word(&mut self, w: u64) {
        self.h = (self.h.rotate_left(5) ^ w).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    pub fn tag(&mut self, t: Tag) {
        self.word(t as u64);
    }

    pub fn int(&mut self, n: i64) {
        self.tag(Tag::Int);
        self.word(n as u64);
    }

    pub fn float(&mut self, x: f64) {
        self.tag(Tag::Float);
        // `==` says `0.0 == -0.0`, so they must hash alike. NaN equals nothing,
        // not even itself, so any one hash for it is correct; one is chosen so
        // the answer does not depend on which NaN it was.
        let bits = if x == 0.0 {
            0
        } else if x.is_nan() {
            f64::NAN.to_bits()
        } else {
            x.to_bits()
        };
        self.word(bits);
    }

    pub fn bool(&mut self, b: bool) {
        self.tag(Tag::Bool);
        self.word(u64::from(b));
    }

    pub fn char(&mut self, c: char) {
        self.tag(Tag::Char);
        self.word(u64::from(c));
    }

    pub fn unit(&mut self) {
        self.tag(Tag::Unit);
    }

    pub fn str(&mut self, s: &str) {
        self.tag(Tag::Str);
        self.bytes(s.as_bytes());
    }

    /// A `BigInt` by its two's-complement bytes, least significant first.
    pub fn bigint(&mut self, le_bytes: &[u8]) {
        self.tag(Tag::BigInt);
        self.bytes(le_bytes);
    }

    /// The head of a constructor. Its fields follow, one value each.
    ///
    /// `True` and `False` are constructors in some programs and literals in
    /// others, and `==` does not care which; neither does this.
    pub fn data(&mut self, name: &str, arity: usize) {
        match (name, arity) {
            ("True" | "Bool.True", 0) => self.bool(true),
            ("False" | "Bool.False", 0) => self.bool(false),
            _ => {
                self.tag(Tag::Data);
                self.bytes(name.as_bytes());
                self.word(arity as u64);
            }
        }
    }

    /// The head of a `Compact`; the value inside follows.
    pub fn compact(&mut self) {
        self.tag(Tag::Compact);
    }

    /// The head of a `Vector` of `len` elements, which follow.
    pub fn vector(&mut self, len: usize) {
        self.tag(Tag::Vector);
        self.word(len as u64);
    }

    /// The head of an `Array` of `len` elements, which follow.
    pub fn array(&mut self, len: usize) {
        self.tag(Tag::Array);
        self.word(len as u64);
    }

    /// The head of a record of `fields` fields. Each follows as [`Hasher::str`]
    /// of its label, then its value, in label order.
    pub fn record(&mut self, fields: usize) {
        self.tag(Tag::Record);
        self.word(fields as u64);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        for chunk in b.chunks(8) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            self.word(u64::from_le_bytes(w));
        }
    }

    pub fn finish(&self) -> i64 {
        let mut z = self.h;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) as i64
    }
}

/// The error for a value `hash` refuses: `what` is "a Ref" or "a function".
pub fn unhashable(what: &str) -> String {
    format!("cannot hash {what}: it has no structure to compare by")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(f: impl FnOnce(&mut Hasher)) -> i64 {
        let mut h = Hasher::new();
        f(&mut h);
        h.finish()
    }

    #[test]
    fn signed_zeros_hash_alike() {
        assert_eq!(of(|h| h.float(0.0)), of(|h| h.float(-0.0)));
    }

    #[test]
    fn kinds_do_not_collide_by_construction() {
        let unit = of(|h| h.unit());
        let zero = of(|h| h.int(0));
        let empty = of(|h| h.str(""));
        let no = of(|h| h.bool(false));
        assert_ne!(unit, zero);
        assert_ne!(zero, empty);
        assert_ne!(zero, no);
    }

    #[test]
    fn a_constructed_bool_is_the_literal() {
        assert_eq!(of(|h| h.data("True", 0)), of(|h| h.bool(true)));
        assert_eq!(of(|h| h.data("Bool.False", 0)), of(|h| h.bool(false)));
    }

    #[test]
    fn nearby_ints_spread_like_random_ones() {
        // A table indexes by a few bits at a time. 1024 consecutive keys thrown
        // into 1024 slots by a uniform random function fill about 647 of them;
        // a hash that let neighbouring keys share bits would fill far fewer.
        for shift in [0, 5, 10, 30] {
            let slots: std::collections::HashSet<i64> = (0..1024)
                .map(|n| (of(|h| h.int(n)) >> shift) & 1023)
                .collect();
            assert!(
                slots.len() > 600,
                "{} distinct slots of 1024 at shift {shift}",
                slots.len()
            );
        }
    }
}
