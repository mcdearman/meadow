//! How an object is laid out in words.
//!
//! Every slot of the heap, of the old generation and of a region is one
//! [`Word`], and a field holds a value's bits and nothing else. What the bits
//! are -- an address, an `Int`, a `Float` -- is written once, in the object's
//! header, as a 4-bit descriptor per field (see [`meadow_core::desc`]). A
//! collector reads the descriptors to find the addresses; everything else reads
//! them to know what it is looking at.
//!
//! ```text
//!   a+0   len (32) | reserved (19) | uniform (1) | uniform desc (4) | kind (8)
//!   a+1   descriptors of fields 0..8 (32)        | meta (32)
//!   a+2   descriptors of fields 8..24, 16 per word -- only while needed
//!   ...
//!   a+h   field 0
//!   ...
//! ```
//!
//! An array, a mutable array and a `BigInt` hold values of one type, so their
//! header says **uniform** and gives one descriptor for every element: never
//! more than two words, however long they are. Anything else has a descriptor
//! per field: two words up to eight fields, and a word per sixteen after that.
//!
//! A nursery object that has been copied has its first word replaced by
//! [`FORWARD`] and the address it went to.

use crate::heap::Kind;
use crate::value::{Addr, Word};
use meadow_core::compact::{DESCS_PER_WORD, INLINE_DESCS, header_slots};
use meadow_core::desc::{self, Desc};

/// The kind byte of a first word left behind by a copy.
pub const FORWARD: u8 = 0xFF;

const UNIFORM: Word = 1 << 12;

/// The first word of an object moved to `to`.
#[inline]
pub fn forward(to: Addr) -> Word {
    FORWARD as Word | (to as Word) << 32
}

/// Where the object whose first word is `w` went, if it has been moved.
#[inline]
pub fn forwarded(w: Word) -> Option<Addr> {
    (w as u8 == FORWARD).then_some((w >> 32) as Addr)
}

/// An object's header, read.
#[derive(Debug, Clone, Copy)]
pub struct Head {
    pub kind: Kind,
    pub len: u32,
    pub meta: u32,
    /// The descriptor of every field, for a uniform object.
    uniform: Option<Desc>,
    /// The second word, whose high half is the first fields' descriptors.
    second: Word,
}

impl Head {
    /// The header whose first two words are `w0` and `w1`.
    #[inline(always)]
    pub fn read(w0: Word, w1: Word) -> Head {
        debug_assert_ne!(w0 as u8, FORWARD, "reading a forwarded header");
        Head {
            kind: Kind::from_byte(w0 as u8),
            len: (w0 >> 32) as u32,
            meta: w1 as u32,
            uniform: (w0 & UNIFORM != 0).then_some(((w0 >> 8) & 0xF) as Desc),
            second: w1,
        }
    }

    /// Words before the first field.
    #[inline]
    pub fn header(&self) -> usize {
        header_slots(self.uniform.is_some(), self.len as usize)
    }

    /// Words the whole object takes.
    #[inline]
    pub fn size(&self) -> usize {
        self.header() + self.len as usize
    }

    /// Field `i`'s descriptor. `word(k)` reads the object's `k`th word, for a
    /// field whose descriptor is past the second.
    #[inline(always)]
    pub fn desc(&self, i: usize, word: impl Fn(usize) -> Word) -> Desc {
        if let Some(d) = self.uniform {
            return d;
        }
        if i < INLINE_DESCS {
            return ((self.second >> (32 + 4 * i)) & 0xF) as Desc;
        }
        let j = i - INLINE_DESCS;
        ((word(2 + j / DESCS_PER_WORD) >> (4 * (j % DESCS_PER_WORD))) & 0xF) as Desc
    }

    /// Call `f` with the index of every field in `from..to` that holds an
    /// address.
    #[inline]
    pub fn pointers(
        &self,
        from: usize,
        to: usize,
        word: impl Fn(usize) -> Word,
        mut f: impl FnMut(usize),
    ) {
        match self.uniform {
            Some(desc::REF) => (from..to).for_each(f),
            Some(_) => {}
            None => {
                for i in from..to {
                    if self.desc(i, &word) == desc::REF {
                        f(i);
                    }
                }
            }
        }
    }

    /// Does any field hold an address?
    pub fn may_point(&self) -> bool {
        match self.uniform {
            Some(d) => d == desc::REF && self.len > 0,
            None => true,
        }
    }
}

/// The header of an object of `kind` and `meta` whose fields are described by
/// `descs`, as the words to write before them: `put(k, word)` for each.
pub fn write_header(
    kind: Kind,
    meta: u32,
    descs: impl ExactSizeIterator<Item = Desc>,
    mut put: impl FnMut(usize, Word),
) {
    let len = descs.len();
    let mut w0 = kind as u8 as Word | (len as Word) << 32;
    let mut w1 = meta as Word;
    if kind.is_uniform() {
        let mut one = None;
        for d in descs {
            debug_assert!(
                one.is_none_or(|o| o == d),
                "a {kind:?} holding values of more than one representation"
            );
            one = Some(d);
        }
        w0 |= UNIFORM | (one.unwrap_or(desc::UNIT) as Word) << 8;
        put(0, w0);
        put(1, w1);
        return;
    }
    let extra = header_slots(false, len) - 2;
    let mut words = vec![0 as Word; extra];
    for (i, d) in descs.enumerate() {
        debug_assert!((0..16).contains(&d), "a descriptor of 4 bits");
        if i < INLINE_DESCS {
            w1 |= (d as Word) << (32 + 4 * i);
        } else {
            let j = i - INLINE_DESCS;
            words[j / DESCS_PER_WORD] |= (d as Word) << (4 * (j % DESCS_PER_WORD));
        }
    }
    put(0, w0);
    put(1, w1);
    for (k, w) in words.into_iter().enumerate() {
        put(2 + k, w);
    }
}

/// Field `i`'s descriptor changed to `d`, in a header written by
/// [`write_header`] whose `k`th word `word(k)` reads: which word changes, and
/// what to. Only for an object that is not uniform.
pub fn with_desc(i: usize, d: Desc, word: impl Fn(usize) -> Word) -> (usize, Word) {
    let (k, shift) = if i < INLINE_DESCS {
        (1, 32 + 4 * i)
    } else {
        let j = i - INLINE_DESCS;
        (2 + j / DESCS_PER_WORD, 4 * (j % DESCS_PER_WORD))
    };
    (k, (word(k) & !(0xF << shift)) | (d as Word) << shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(kind: Kind, meta: u32, descs: &[Desc]) -> Vec<Word> {
        let mut words = vec![0; header_slots(kind.is_uniform(), descs.len())];
        write_header(kind, meta, descs.iter().copied(), |k, w| words[k] = w);
        words
    }

    #[test]
    fn a_small_object_has_two_words_of_header() {
        let words = build(Kind::Data, 7, &[desc::INT, desc::REF, desc::STR]);
        assert_eq!(words.len(), 2);
        let h = Head::read(words[0], words[1]);
        assert_eq!((h.kind, h.len, h.meta, h.size()), (Kind::Data, 3, 7, 5));
        let get = |k: usize| words[k];
        assert_eq!(
            (0..3).map(|i| h.desc(i, get)).collect::<Vec<_>>(),
            [desc::INT, desc::REF, desc::STR]
        );
        let mut ptrs = Vec::new();
        h.pointers(0, 3, get, |i| ptrs.push(i));
        assert_eq!(ptrs, [1]);
    }

    #[test]
    fn a_wide_object_takes_a_word_of_descriptors_per_sixteen_fields() {
        let descs: Vec<Desc> = (0..30)
            .map(|i| if i % 3 == 0 { desc::REF } else { desc::FLOAT })
            .collect();
        let mut words = build(Kind::Closure, 2, &descs);
        assert_eq!(words.len(), 2 + 2);
        let h = Head::read(words[0], words[1]);
        let got: Vec<Desc> = (0..30).map(|i| h.desc(i, |k| words[k])).collect();
        assert_eq!(got, descs);
        let (k, w) = with_desc(29, desc::CHAR, |k| words[k]);
        words[k] = w;
        let h = Head::read(words[0], words[1]);
        assert_eq!(h.desc(29, |k| words[k]), desc::CHAR);
        assert_eq!(h.desc(28, |k| words[k]), desc::FLOAT);
    }

    #[test]
    fn an_array_is_described_once() {
        let words = build(Kind::Array, 0, &[desc::REF; 100]);
        let h = Head::read(words[0], words[1]);
        assert_eq!((h.header(), h.size()), (2, 102));
        let mut n = 0;
        h.pointers(0, 100, |k| words[k], |_| n += 1);
        assert_eq!(n, 100);
    }

    #[test]
    fn a_moved_object_says_where_it_went() {
        assert_eq!(forwarded(forward(99)), Some(99));
        let words = build(Kind::Data, 0, &[]);
        assert_eq!(forwarded(words[0]), None);
    }
}
