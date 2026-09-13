//! Moving objects out of sparse old blocks, a few blocks per nursery pause.
//!
//! Old objects never move while a cycle marks, so a block most of whose
//! objects died keeps its survivors where they are, and the free lines around
//! them go to small objects only. Over time that strands memory. Evacuating a
//! block -- copying its survivors somewhere dense and giving the whole block
//! back -- fixes that, but moving an object means rewriting every pointer to it,
//! and nothing tells a collector that doesn't trace the whole heap where those
//! are. This module keeps a list of them for the blocks it means to move.
//!
//! # The lists
//!
//! When a cycle finishes, the sparsest blocks are chosen -- [`old::TRACKED`] --
//! and nothing is allocated into them again. Through the whole of the *next*
//! cycle, every field that points into one is recorded:
//!
//! * the **marker** records the ones it reads, which is every field of every
//!   object that was alive when that cycle began;
//! * the **barriers** record the rest: a promoted object's fields, a large
//!   object's allocated straight into the old generation, and every `set_field`.
//!
//! A pointer into a chosen block that exists when the move happens was either in
//! an object alive at the start of the cycle -- which the marker read in full,
//! and whose fields only `set_field` changes after -- or written since, by one of
//! the barriers. Roots and the nursery are not recorded, since they are scanned
//! at the move. So once that cycle has finished, a chosen block's list is
//! complete, and it is kept complete by the barriers until the block moves.
//!
//! Each chosen block keeps its own list, so the marker hands what it finds
//! straight to the block, and moving a block reads only its own.
//!
//! A recorded field may since have been overwritten, or have belonged to an
//! object that died and whose line was reused. So a field is only rewritten if
//! it still points into the block being moved -- and if it does, it needs
//! rewriting, whoever wrote it.
//!
//! # The move
//!
//! Between cycles only, so no marker is reading: in each nursery pause, until
//! a budget of time and work is spent, take a chosen block, copy each object
//! marked in the last cycle into ordinary blocks, and rewrite the fields
//! recorded pointing into it. Then, for the blocks taken, rewrite the copies'
//! own fields, the roots and the nursery -- which right after a nursery
//! collection holds only its few survivors -- and give the blocks back. A block chosen but not
//! moved by the time the next cycle begins goes back to being ordinary.
//!
//! A block with more pointers into it than [`old`] cares to record is abandoned
//! rather than moved; so is one holding part of an object bigger than a block.

use std::time::{Duration, Instant};

use crate::heap::Slot;
use crate::old::{self, BLOCK, OLD_BASE, Old};
use crate::region::REGION_BASE;
use crate::value::{Addr, Value};

/// A block a quarter full or less is worth moving. Moving costs what is in the
/// block and what points at it, and frees the whole block either way, so the
/// emptiest blocks give back the most memory for the least pause; and Immix
/// already reuses the free lines of a fuller block without moving anything.
const SPARSE: u32 = (BLOCK / 4) as u32;

/// The most blocks chosen at once.
const MOST_CHOSEN: usize = 4096;

#[derive(Default)]
pub struct Evacuation {
    /// Chosen when the last cycle finished; recorded through the next.
    tracking: Vec<usize>,
    /// Recorded through a whole cycle, and ready to move, sparsest last.
    pending: Vec<usize>,
    /// Slots and blocks moved, in total.
    pub moved: u64,
    pub blocks: u64,
}

/// Which blocks are being moved in this pause: a bit per block, so asking
/// about an address touches nothing of the block itself.
struct Moving {
    bits: Vec<u64>,
    /// Where each object of each moving block went, by offset; 0 if nowhere.
    to: Vec<(usize, Vec<Addr>)>,
}

impl Moving {
    #[inline]
    fn has(&self, x: Addr) -> bool {
        if !(OLD_BASE..REGION_BASE).contains(&x) {
            return false;
        }
        let b = old::block_of(x);
        self.bits
            .get(b / 64)
            .is_some_and(|w| w & (1 << (b % 64)) != 0)
    }

    /// Where `x`, in a moving block, went. Nowhere, if it was not marked alive:
    /// only a field of an object that is dead too can still point at it, and
    /// that field is left as it is.
    fn forward(&self, x: Addr) -> Option<Addr> {
        let (_, to) = self
            .to
            .iter()
            .rev()
            .find(|(bi, _)| *bi == old::block_of(x))
            .expect("a moving block");
        Some(to[old::offset_of(x)]).filter(|n| *n != 0)
    }
}

impl Evacuation {
    /// The barrier: old slot `s` now holds a pointer to `x`.
    #[inline]
    pub fn note(&self, old: &Old, s: Addr, x: Addr) {
        if (self.tracking.is_empty() && self.pending.is_empty())
            || !(OLD_BASE..REGION_BASE).contains(&x)
        {
            return;
        }
        let b = &old.blocks[old::block_of(x)];
        if b.is_tracked() && b.count_ref() {
            b.record([s]);
        }
    }

    /// A cycle is beginning: what was not moved in time stays where it is.
    pub fn begin(&mut self, old: &Old) {
        for bi in self.pending.drain(..) {
            if let Some(b) = old.blocks.get(bi).filter(|b| !b.is_empty()) {
                b.set_state(old::NORMAL);
            }
        }
    }

    /// A cycle finished: the blocks tracked through it are ready to move, and
    /// the next are chosen.
    pub fn finish(&mut self, old: &Old) {
        let mut ready = Vec::new();
        for bi in self.tracking.drain(..) {
            let b = &old.blocks[bi];
            if b.is_tracked() {
                ready.push(bi);
            } else {
                b.set_state(old::NORMAL);
            }
        }
        // Sparsest last, to be taken first.
        let epoch = old.live_epoch();
        ready.sort_unstable_by_key(|bi| std::cmp::Reverse(old.blocks[*bi].live(epoch)));
        self.pending = ready;
        self.tracking = choose(old);
        for bi in &self.tracking {
            old.blocks[*bi].set_state(old::TRACKED);
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Move chosen blocks' survivors out, until `budget` -- a time, and work
    /// counted in slots copied and fields rewritten -- is spent, or everything
    /// is moved if there is none. `nursery` is the nursery's objects, and
    /// `roots` everything else that can point into the old generation besides
    /// old objects themselves.
    pub fn evacuate(
        &mut self,
        old: &mut Old,
        nursery: &mut [Slot],
        roots: &mut [Value],
        remembered: &mut Vec<Addr>,
        budget: Option<(Duration, usize)>,
    ) {
        let started = Instant::now();
        let epoch = old.live_epoch();
        let mut moving = Moving {
            bits: vec![0; old.blocks.len().div_ceil(64)],
            to: Vec::new(),
        };
        let mut copies: Vec<Addr> = Vec::new();
        let (mut slots, mut work) = (0usize, 0usize);
        while let Some(bi) = self.pending.pop() {
            let b = old.blocks[bi].clone();
            if !b.is_tracked() {
                b.set_state(old::NORMAL);
                continue;
            }
            b.set_state(old::MOVING);
            moving.bits[bi / 64] |= 1 << (bi % 64);
            let mut to = vec![0 as Addr; BLOCK];
            let before = slots;
            for off in b.marked_offsets(epoch) {
                let Slot::Header { len, .. } = b.get(off) else {
                    unreachable!("a marked object in block {bi} has no header");
                };
                let size = 1 + len as usize;
                let n = old.alloc_moved(size);
                for i in 0..size {
                    old.put(n + i as Addr, b.get(off + i));
                }
                to[off] = n;
                copies.push(n);
                slots += size;
            }
            moving.to.push((bi, to));
            // The fields recorded pointing in. One in a block already taken
            // this pause -- this one included -- is in an object that has just
            // been copied, whose copy is rewritten below.
            let recorded = b.take_recorded();
            work += slots - before + recorded.len();
            for s in recorded {
                if moving.has(s) || old.blocks[old::block_of(s)].is_empty() {
                    continue;
                }
                if let Slot::Val(Value::Obj(x)) = old.get(s)
                    && (OLD_BASE..REGION_BASE).contains(&x)
                    && old::block_of(x) == bi
                    && let Some(n) = moving.forward(x)
                {
                    old.put(s, Slot::Val(Value::Obj(n)));
                }
            }
            // Another block only if one more, at the rate so far, still fits.
            let taken = moving.to.len() as u32;
            if let Some((time, most)) = budget
                && (work * (taken as usize + 1) >= most * taken as usize
                    || started.elapsed() * (taken + 1) >= time * taken)
            {
                break;
            }
        }
        if moving.to.is_empty() {
            return;
        }

        // The copies' own fields: into the moved blocks, the nursery, or other
        // chosen blocks, each kept track of as for any old field.
        for &n in &copies {
            let Slot::Header { len, .. } = old.get(n) else {
                unreachable!("a copy has no header");
            };
            for s in n + 1..n + 1 + len {
                if let Slot::Val(Value::Obj(x)) = old.get(s) {
                    let x = match moving.has(x).then(|| moving.forward(x)).flatten() {
                        Some(to) => {
                            old.put(s, Slot::Val(Value::Obj(to)));
                            to
                        }
                        None => x,
                    };
                    if x < OLD_BASE {
                        if old.remember(s) {
                            remembered.push(s);
                        }
                    } else {
                        self.note(old, s, x);
                    }
                }
            }
        }
        for r in roots.iter_mut() {
            if let Value::Obj(x) = *r
                && moving.has(x)
            {
                let to = moving
                    .forward(x)
                    .expect("a root points at an object not marked alive");
                *r = Value::Obj(to);
            }
        }
        let mut at = 0;
        while at < nursery.len() {
            let Slot::Header { len, .. } = nursery[at] else {
                unreachable!("nursery walk is not at a header");
            };
            for f in &mut nursery[at + 1..at + 1 + len as usize] {
                if let Slot::Val(Value::Obj(x)) = *f
                    && moving.has(x)
                    && let Some(to) = moving.forward(x)
                {
                    *f = Slot::Val(Value::Obj(to));
                }
            }
            at += 1 + len as usize;
        }
        remembered.retain(|s| !moving.has(*s));

        for (bi, _) in &moving.to {
            old.free_block(*bi);
        }
        self.moved += slots as u64;
        self.blocks += moving.to.len() as u64;
    }
}

/// The blocks to evacuate after the next cycle: the sparsest, while the old
/// generation holds a quarter again more than is alive, up to a quarter of
/// what is alive in all.
fn choose(old: &Old) -> Vec<usize> {
    let live = old.live as usize;
    if old.capacity() < live + live / 4 + 8 * BLOCK {
        return Vec::new();
    }
    let epoch = old.live_epoch();
    let mut sparse: Vec<(u32, usize)> = old
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !b.is_empty() && b.state() == old::NORMAL && !b.is_large())
        .map(|(i, b)| (b.live(epoch), i))
        .filter(|(l, _)| *l > 0 && *l <= SPARSE)
        .collect();
    sparse.sort_unstable();
    let most = (live / 4).max(2 * BLOCK);
    let mut total = 0;
    sparse
        .into_iter()
        .take_while(|(l, _)| {
            total += *l as usize;
            total <= most
        })
        .take(MOST_CHOSEN)
        .map(|(_, i)| i)
        .collect()
}
