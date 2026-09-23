//! **Values crossing between threads.** Each green thread has a heap of its
//! own (`crate::ctx`), so what one hands another -- a spawned function and
//! what it captures, a message, a thread's answer, a `TVar`'s value -- is
//! copied: out of the sender's heap into a parcel, which belongs to no heap,
//! and out of the parcel into the receiver's. Sharing within the value is
//! kept, so a structure that is a DAG arrives as one.
//!
//! Immutable data copies without anyone being able to tell. What cannot be
//! copied so is refused, as `meadow-rts` refuses it: a `Ref`, a mutable array,
//! a continuation. A thread, a channel or a `TVar` is a handle to something
//! the scheduler keeps, and crosses as the same handle.

use crate::heap::{self, Word};
use meadow_core::desc;
use std::collections::HashMap;

/// A value lifted out of a heap.
#[derive(Clone)]
pub struct Parcel {
    /// The value's word -- or, for a block, its index in `blocks` -- and its
    /// descriptor.
    root: (Word, i64),
    blocks: Vec<Lifted>,
}

#[derive(Clone)]
struct Lifted {
    /// The whole block, header and all, with each reference field holding
    /// the index of the block it refers to.
    words: Vec<Word>,
    /// Which words those are.
    refs: Vec<usize>,
    /// How many references to it the parcel holds.
    incoming: u32,
}

impl Parcel {
    /// `v`, described by `d`, lifted out of the running thread's heap -- which
    /// keeps it -- or why it cannot be.
    pub fn of(v: Word, d: i64) -> Result<Parcel, String> {
        let mut blocks: Vec<Lifted> = Vec::new();
        let mut index: HashMap<Word, usize> = HashMap::new();
        if d != desc::REF || !heap::is_block(v) {
            return Ok(Parcel {
                root: (v, d),
                blocks,
            });
        }
        let mut todo = vec![v];
        index.insert(v, 0);
        blocks.push(lift(v)?);
        while let Some(b) = todo.pop() {
            let at = index[&b];
            let first = heap::first_field(b);
            let raw = heap::kind(b) == heap::STRING || heap::kind(b) == heap::BIGINT;
            if raw {
                continue;
            }
            for i in 0..heap::len(b) {
                let x = heap::field(b, i);
                if heap::field_desc(b, i) != desc::REF || !heap::is_block(x) {
                    continue;
                }
                let j = match index.get(&x) {
                    Some(j) => *j,
                    None => {
                        let j = blocks.len();
                        blocks.push(lift(x)?);
                        index.insert(x, j);
                        todo.push(x);
                        j
                    }
                };
                blocks[j].incoming += 1;
                blocks[at].words[first + i] = j as Word;
                blocks[at].refs.push(first + i);
            }
        }
        blocks[0].incoming += 1;
        Ok(Parcel {
            root: (0, d),
            blocks,
        })
    }

    /// The value, put into the running thread's heap: owned by the caller.
    pub fn open(&self) -> Word {
        if self.blocks.is_empty() {
            return self.root.0;
        }
        let made: Vec<Word> = self
            .blocks
            .iter()
            .map(|b| {
                let v = heap::acquire(b.words.len());
                for (i, w) in b.words.iter().enumerate() {
                    heap::set_word(v, i, *w);
                }
                // The count is the references besides one.
                let len = b.words[0] & 0xFFFF_FFFF_0000_0000;
                heap::set_word(v, 0, len | u64::from(b.incoming - 1));
                // What the sending thread's cycle collector thought of the
                // block it copied is about that heap's candidate list, not
                // this one's: a fresh block here has never been a candidate.
                heap::set_word(v, 1, b.words[1] & !heap::MARKS);
                v
            })
            .collect();
        for (b, v) in self.blocks.iter().zip(&made) {
            for &i in &b.refs {
                heap::set_word(*v, i, made[b.words[i] as usize]);
            }
        }
        made[0]
    }

    /// The descriptor of the value.
    pub fn desc(&self) -> i64 {
        self.root.1
    }
}

fn lift(v: Word) -> Result<Lifted, String> {
    let why = match heap::kind(v) {
        heap::CELL => Some("a Ref"),
        heap::MUT_ARRAY => Some("a mutable array"),
        heap::ONCE | heap::STACK => Some("a continuation"),
        _ => None,
    };
    if let Some(what) = why {
        return Err(meadow_core::thread::unsendable(what));
    }
    let n = heap::first_field(v) + heap::len(v);
    let mut words: Vec<Word> = (0..n).map(|i| heap::word(v, i)).collect();
    words[0] &= 0xFFFF_FFFF_0000_0000;
    Ok(Lifted {
        words,
        refs: Vec::new(),
        incoming: 0,
    })
}
