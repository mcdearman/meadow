//! Shared compact regions: immutable blocks of slots that belong to no heap.
//!
//! A compact region used to belong to the heap that made it. Now it belongs to
//! the process, and a heap only *refers* to it: any number of green threads,
//! on any OS threads, can hold addresses into one region and read it with no
//! copying and no locking. That is what makes a `Compact` free to send to
//! another thread, and what lets a `TVar` hold its value where every thread
//! can read it.
//!
//! # Why sharing is safe
//!
//! A region is **closed** -- nothing in it points outside it -- and
//! **immutable once published**. Objects are only ever appended, past the end
//! of what any address can reach, by one appender at a time holding the
//! region's lock, and an address into the new objects reaches anyone only
//! after the append is finished. So a reader never sees a slot that is still
//! being written, and never needs a lock to read.
//!
//! # Addresses
//!
//! Region addresses start at [`REGION_BASE`], above every heap address,
//! and are unique in the process: [`SPACE`] hands out ranges to blocks and takes
//! them back when a block is freed. So an address means the same slot in every
//! heap, which is what lets a pointer into a region be copied between heaps
//! unchanged.
//!
//! # Lifetime
//!
//! A region is reference-counted. A heap holds a count for each region it has
//! addresses into, and gives it up when a collection finds none left; a `TVar`
//! holds one for its value; a parcel in transit holds one for what it carries.
//! When the last goes, the region's blocks are freed and their addresses
//! returned.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::heap::Kind;
use crate::object::{Head, write_header};
use crate::value::{Addr, Value, Word};

/// Addresses below this are a heap's own; at or above it, a region.
pub const REGION_BASE: Addr = 1 << 31;

/// Slots in a region's first block. Later blocks double, so a region built by
/// many small appends still has few blocks.
pub const FIRST_BLOCK: usize = 1 << 12;

/// Region address ranges in use, sorted: `(base, end)`.
static SPACE: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The lowest free range of `cap` region addresses, now taken.
fn reserve(cap: usize) -> Addr {
    let mut space = lock(&SPACE);
    let mut start = REGION_BASE as u64;
    let mut at = space.len();
    for (i, &(base, end)) in space.iter().enumerate() {
        if base >= start + cap as u64 {
            at = i;
            break;
        }
        start = end;
    }
    assert!(
        start + cap as u64 <= u32::MAX as u64,
        "compact regions have used up the address space"
    );
    space.insert(at, (start, start + cap as u64));
    start as Addr
}

fn release(base: Addr) {
    let mut space = lock(&SPACE);
    if let Ok(i) = space.binary_search_by_key(&(base as u64), |&(b, _)| b) {
        space.remove(i);
    }
}

/// One contiguous run of region addresses, `base..base + cap`. Its slots never
/// move.
pub struct Block {
    pub base: Addr,
    pub cap: usize,
    /// The region it belongs to, for a collector marking what it reached.
    pub region: u32,
    slots: Box<[UnsafeCell<Word>]>,
}

// Slots are written only by the region's appender, under its lock, and only
// where no published address reaches; everyone else only reads.
unsafe impl Sync for Block {}
unsafe impl Send for Block {}

impl Block {
    fn new(region: u32, cap: usize) -> Block {
        let base = reserve(cap);
        let slots = (0..cap).map(|_| UnsafeCell::new(0)).collect();
        Block {
            base,
            cap,
            region,
            slots,
        }
    }

    /// The slot at address `a`, which must be in this block.
    #[inline]
    pub fn get(&self, a: Addr) -> Word {
        // Safety: see the `Sync` impl -- a slot an address reaches is no
        // longer written.
        unsafe { *self.slots[(a - self.base) as usize].get() }
    }

    /// Write slot `i`. Only the appender does, and only past what is published.
    fn put(&self, i: usize, w: Word) {
        // Safety: see the `Sync` impl.
        unsafe { *self.slots[i].get() = w }
    }

    /// The header of the object at `a`, which must be in this block.
    pub fn head(&self, a: Addr) -> Head {
        Head::read(self.get(a), self.get(a + 1))
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        release(self.base);
    }
}

/// A region: its blocks, and how full they are.
pub struct Region {
    pub id: u32,
    contents: Mutex<Contents>,
}

/// A region's blocks, and where its end is. Locked to append.
pub struct Contents {
    pub blocks: Vec<Arc<Block>>,
    /// Slots used in each block. A block's tail past its length may hold what a
    /// failed append left behind, and is never read.
    lens: Vec<usize>,
    /// Slots used across all of them.
    pub used: usize,
    id: u32,
}

/// A position in a region's contents -- see [`Contents::next`].
pub struct Cursor {
    block: usize,
    offset: usize,
}

/// Where a region ended, so a failed append can be undone.
#[derive(Clone, Copy)]
pub struct End {
    blocks: usize,
    last_len: usize,
    used: usize,
}

impl Region {
    pub fn new() -> Arc<Region> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Arc::new(Region {
            id,
            contents: Mutex::new(Contents {
                blocks: Vec::new(),
                lens: Vec::new(),
                used: 0,
                id,
            }),
        })
    }

    /// The contents, locked: to append, or to read the block list.
    pub fn lock(&self) -> MutexGuard<'_, Contents> {
        lock(&self.contents)
    }

    /// Slots in use.
    pub fn used(&self) -> usize {
        self.lock().used
    }
}

impl Contents {
    pub fn end(&self) -> End {
        End {
            blocks: self.blocks.len(),
            last_len: self.lens.last().copied().unwrap_or(0),
            used: self.used,
        }
    }

    /// Undo everything appended since `end`.
    pub fn truncate(&mut self, end: End) {
        self.blocks.truncate(end.blocks);
        self.lens.truncate(end.blocks);
        if let Some(last) = self.lens.last_mut() {
            *last = end.last_len;
        }
        self.used = end.used;
    }

    /// Append an object, its fields as given, and answer its address.
    pub fn alloc(&mut self, kind: Kind, meta: u32, fields: &[Value]) -> Addr {
        let header = meadow_core::compact::header_slots(kind.is_uniform(), fields.len());
        let size = header + fields.len();
        let fits = match (self.blocks.last(), self.lens.last()) {
            (Some(b), Some(&len)) => len + size <= b.cap,
            _ => false,
        };
        if !fits {
            let cap = self
                .blocks
                .last()
                .map_or(FIRST_BLOCK, |b| b.cap * 2)
                .max(size);
            self.blocks.push(Arc::new(Block::new(self.id, cap)));
            self.lens.push(0);
        }
        let b = self.blocks.last().expect("a block with room");
        let len = self.lens.last_mut().expect("a length per block");
        let at = *len;
        write_header(kind, meta, fields.iter().map(|v| v.desc()), |k, w| {
            b.put(at + k, w)
        });
        for (i, v) in fields.iter().enumerate() {
            b.put(at + header + i, v.bits());
        }
        *len += size;
        self.used += size;
        b.base + at as Addr
    }

    /// A cursor at `end`, for walking what is appended after it.
    pub fn cursor(&self, end: End) -> Cursor {
        match end.blocks {
            0 => Cursor {
                block: 0,
                offset: 0,
            },
            n => Cursor {
                block: n - 1,
                offset: end.last_len,
            },
        }
    }

    /// The next object at or after `cursor`, and the cursor moved past it:
    /// its address and header. Objects appended while walking are reached
    /// too, which is what lets a copy use the region as its own work queue.
    pub fn next(&self, cursor: &mut Cursor) -> Option<(Addr, Head)> {
        while cursor.block < self.blocks.len() {
            let b = &self.blocks[cursor.block];
            if cursor.offset < self.lens[cursor.block] {
                let at = b.base + cursor.offset as Addr;
                let head = b.head(at);
                cursor.offset += head.size();
                return Some((at, head));
            }
            cursor.block += 1;
            cursor.offset = 0;
        }
        None
    }

    /// The block holding address `a`, if it is this region's.
    pub fn block_of(&self, a: Addr) -> Option<&Arc<Block>> {
        self.blocks
            .iter()
            .find(|b| b.base <= a && a < b.base + b.cap as Addr)
    }

    /// Overwrite field `i` of the object at `a`, which this append made, with
    /// a value of the representation it holds already.
    pub fn set_field(&self, a: Addr, i: usize, v: Value) {
        let b = self.block_of(a).expect("an address in this region");
        let head = b.head(a);
        debug_assert_eq!(
            head.desc(i, |k| b.get(a + k as Addr)),
            v.desc(),
            "a region field changing what it holds"
        );
        b.put((a - b.base) as usize + head.header() + i, v.bits());
    }
}
