//! **Compact regions: memory outside every heap, and one object to the rest
//! of the runtime.**
//!
//! `Std.Compact` promises three things (`lib/Std/src/Compact.mw`): a region is
//! "memory outside every heap, which the collector neither copies nor looks
//! inside"; "however big the region, a collection pays one mark for it, and
//! when nothing refers to the region any more it is freed all at once"; and "a
//! region belongs to no thread, so sending a compacted value to another copies
//! nothing". Counting references rather than tracing does not excuse any of
//! them -- it only changes what "one mark" means, which here is one count.
//!
//! A `Compact` is an ordinary counted block of two fields: the value, which is
//! a pointer into the region, and the region itself, held as a number so that
//! nothing follows or counts it. Everything the value reaches lives in the
//! region, copied there once by [`compact`], packed, and never counted again.
//!
//! # How a region knows when the last pointer into it has gone
//!
//! `getCompact` hands out a pointer **into** the region, and that pointer is
//! counted like any other -- so the region cannot be freed with the `Compact`
//! that named it, or a program that reads a compact and then lets go of the
//! compact is left holding freed memory. `meadow-rts` has no such problem: it
//! traces, so a collection finds the addresses a heap holds into a region and
//! the region goes when none are left. Counting cannot discover that; it has
//! to be told.
//!
//! So it is told, by the same counting every reference already does. A block
//! inside a region carries [`heap::STICKY`] references more than it has, which
//! puts its count far above anything an ordinary block reaches; sharing or
//! erasing one therefore knows, from the count it has already loaded, that it
//! is touching a region -- see [`heap::REGION_FLOOR`], and
//! `meadow_llvm::emit`'s helpers, which do the same test inline. It then adds
//! to or takes from the region's own count, and the region is freed when the
//! last reference into it goes, wherever that happens to be.
//!
//! Each block in a region is laid out behind a word holding the region it
//! belongs to, so finding the region from a block is one load and no lookup.
//!
//! # Why the bias as well
//!
//! The count of a block inside a region is never allowed to reach zero, which
//! is what the bias is for: zero means "the only reference" to the code that
//! loads a block's fields, and it would hand the block back to a size class --
//! region memory into a heap's free list. With the bias no block inside a
//! region is ever freed on its own, and the memory goes all at once, which is
//! what was promised.
//!
//! What this costs, said plainly: every program pays the compare that
//! recognises a region, on the path every share and erase takes, including
//! every program that never makes a compact. `meadow-rts` pays nothing for
//! the same thing, because it traces and its region addresses sit above every
//! heap address. Here the price buys `getCompact` staying O(1) while the
//! region is still freed the moment the last reference into it goes.
//!
//! Sharing a region between threads needs no lock. It is closed and immutable
//! once a reference to it exists, which [`compact`] establishes before it
//! hands one out; its own count is atomic; and the counts inside it are read
//! by nobody for a decision.
//!
//! # What may not go in
//!
//! A region is never looked inside, so nothing in it may change or hold code:
//! [`compact`] fails on a value that reaches a `Ref`, a mutable array, a
//! closure, a continuation, a channel, a thread or a `TVar`, rather than
//! quietly making a region that lies about any of the above. Data, arrays,
//! records, strings, numbers and other compacts are what is left, and a
//! compact inside a compact is kept as it is -- its region retained, its
//! contents not copied again.

use crate::heap::{self, Word};
use meadow_core::desc;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Words a region takes from the system at a time, when what is being put in
/// it is smaller than that.
const CHUNK: usize = 1 << 12;

/// Memory belonging to no heap, holding blocks and nothing else.
pub struct Region {
    inside: Mutex<Inside>,
    /// How many references into this region there are, from anywhere: a
    /// `Compact` block's, and every one a program is holding because
    /// `getCompact` gave it one or it loaded a field. The region goes when
    /// this reaches zero.
    refs: AtomicUsize,
}

struct Inside {
    /// The memory: a run of words each, packed with blocks from the start, so
    /// that the blocks in a chunk can be walked without an index.
    chunks: Vec<(*mut Word, usize)>,
    /// Words of the last chunk that are used.
    used: usize,
    /// Words of blocks the region holds, which is what `compactSize` reports.
    words: usize,
}

// Safety: a region is closed and immutable once published. What it holds is
// written by the one thread appending to it, holding `inside`, and a reference
// reaching any other thread is published only after the append that wrote it
// has finished -- so a reader never sees a block being written and needs no
// lock. See the module docs for the one word that is written afterwards.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

/// A region with one reference, empty.
fn new() -> *const Region {
    Box::into_raw(Box::new(Region {
        inside: Mutex::new(Inside {
            chunks: Vec::new(),
            used: 0,
            words: 0,
        }),
        refs: AtomicUsize::new(1),
    }))
}

/// The region a block inside one belongs to: the word before it, written
/// where it was copied in.
///
/// # Safety
///
/// `v` must be a block inside a region, which its count says (see
/// [`heap::REGION_FLOOR`]).
unsafe fn owner(v: Word) -> *const Region {
    // Safety: the caller's.
    unsafe { *(v as *const Word).sub(1) as *const Region }
}

/// One more reference into the region holding `v`. Called where a share finds
/// a count that says the block is in one.
///
/// # Safety
///
/// `v` must be a block inside a region.
pub unsafe fn shared(v: Word) {
    // Safety: the caller's.
    unsafe { retain(owner(v)) };
}

/// One fewer, and the memory with it if that was the last.
///
/// # Safety
///
/// `v` must be a block inside a region.
pub unsafe fn erased(v: Word) {
    // Safety: the caller's -- and the region cannot be freed under us, since
    // the reference being given up is one of its own.
    unsafe { release(owner(v)) };
}

/// What the emitted counting helpers call, having found a count that says the
/// block is inside a region: see `meadow_llvm::emit`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_region_shared(v: Word) {
    // Safety: the helper tested the count.
    unsafe { shared(v) };
}

#[unsafe(no_mangle)]
pub extern "C" fn meadow_region_erased(v: Word) {
    // Safety: as above.
    unsafe { erased(v) };
}

/// One more reference into `r`.
///
/// # Safety
///
/// `r` must be a region with a reference the caller holds.
pub unsafe fn retain(r: *const Region) {
    // Safety: the caller's.
    unsafe { &*r }.refs.fetch_add(1, Ordering::Relaxed);
}

/// One fewer. The last one takes the memory with it.
///
/// # Safety
///
/// `r` must be a region with a reference the caller is giving up.
pub unsafe fn release(r: *const Region) {
    // Safety: the caller's.
    if unsafe { &*r }.refs.fetch_sub(1, Ordering::Release) != 1 {
        return;
    }
    // Safety: the last reference is gone, so nobody can be reading it, and
    // this is the only thread that will free it.
    let region = unsafe { Box::from_raw(r as *mut Region) };
    let mut inside = region.inside.lock().expect("a region's lock");
    // A compact held inside this one keeps its own region: give that up now,
    // before the block saying so goes.
    for v in blocks(&inside) {
        if heap::kind(v) == heap::COMPACT {
            // Safety: written by `copy`, which retained it.
            unsafe { release(handle(v)) };
        }
    }
    for (p, n) in inside.chunks.drain(..) {
        let layout = std::alloc::Layout::array::<Word>(n).expect("a chunk fits memory");
        // Safety: allocated with this layout in `room`.
        unsafe { std::alloc::dealloc(p as *mut u8, layout) };
    }
}

/// Every block in the region, in the order they were put there. A chunk holds
/// blocks packed from its start, each behind the word saying which region it
/// is in, and a block says how long it is -- so walking one needs nothing
/// kept on the side.
fn blocks(inside: &Inside) -> Vec<Word> {
    let mut out = Vec::new();
    let last = inside.chunks.len().saturating_sub(1);
    for (i, &(p, n)) in inside.chunks.iter().enumerate() {
        let words = if i == last { inside.used } else { n };
        let mut at = 0;
        while at < words {
            // Safety: inside the chunk, at the word before a block.
            let v = unsafe { p.add(at + 1) } as Word;
            let size = heap::first_field(v) + heap::len(v);
            out.push(v);
            at += size + 1;
        }
    }
    out
}

/// Room for `need` words, contiguous, and where it starts.
fn room(inside: &mut Inside, need: usize) -> *mut Word {
    let fits = inside
        .chunks
        .last()
        .is_some_and(|(_, n)| n - inside.used >= need);
    if !fits {
        let n = need.max(CHUNK);
        let layout = std::alloc::Layout::array::<Word>(n).expect("a chunk fits memory");
        // Safety: a non-zero size.
        let p = unsafe { std::alloc::alloc(layout) } as *mut Word;
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        inside.chunks.push((p, n));
        inside.used = 0;
    }
    let (p, _) = *inside.chunks.last().expect("a chunk");
    // Safety: `used` words are used and `need` more fit.
    let at = unsafe { p.add(inside.used) };
    inside.used += need;
    inside.words += need;
    at
}

/// Is `v` a block this region already holds?
fn holds(inside: &Inside, v: Word) -> bool {
    let last = inside.chunks.len().saturating_sub(1);
    inside.chunks.iter().enumerate().any(|(i, &(p, n))| {
        let words = if i == last { inside.used } else { n };
        let base = p as Word;
        v >= base && v < base + (words as Word) * 8
    })
}

/// The region a `Compact` block names.
fn handle(c: Word) -> *const Region {
    heap::field(c, 1) as *const Region
}

/// Why a value cannot be compacted, if it cannot: a region is never looked
/// inside, so nothing in it may change or hold code.
fn refused(v: Word) -> Option<&'static str> {
    match heap::kind(v) {
        heap::CELL => Some("a Ref"),
        heap::MUT_ARRAY => Some("a mutable array"),
        heap::CLOSURE => Some("a function"),
        heap::ONCE | heap::STACK => Some("a continuation"),
        heap::CHANNEL => Some("a channel"),
        heap::TASK => Some("a thread"),
        heap::TVAR => Some("a TVar"),
        _ => None,
    }
}

/// Does a block of this kind hold references to follow? A string or a big
/// integer holds bytes; a compact holds a region of its own, and is kept whole.
fn walks(v: Word) -> bool {
    !matches!(heap::kind(v), heap::STRING | heap::BIGINT | heap::COMPACT)
}

/// The blocks of `v` that this region does not hold yet, each once, deepest
/// last -- and how many words they take. Fails on anything a region may not
/// hold.
fn plan(inside: &Inside, v: Word, d: i64) -> Result<(Vec<Word>, usize), String> {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    let mut words = 0;
    let mut todo = Vec::new();
    if d == desc::REF && heap::is_block(v) {
        todo.push(v);
    }
    while let Some(b) = todo.pop() {
        if let Some(what) = refused(b) {
            return Err(format!("a compact cannot hold {what}"));
        }
        // Already in this region: it is shared rather than copied again, which
        // is what `add` is for.
        if holds(inside, b) || !seen.insert(b) {
            continue;
        }
        order.push(b);
        words += heap::first_field(b) + heap::len(b);
        if !walks(b) {
            continue;
        }
        for i in 0..heap::len(b) {
            let x = heap::field(b, i);
            if heap::field_desc(b, i) == desc::REF && heap::is_block(x) {
                todo.push(x);
            }
        }
    }
    Ok((order, words))
}

/// Copy `v` into the region, sharing what of it the region holds already, and
/// answer the word the value has inside it.
fn copy(inside: &mut Inside, r: *const Region, v: Word, d: i64) -> Result<Word, String> {
    let (order, words) = plan(inside, v, d)?;
    if order.is_empty() {
        return Ok(v);
    }
    // A word each for the region they are in, so a block can say where it
    // belongs without a lookup.
    let base = room(inside, words + order.len());
    let mut made: HashMap<Word, Word> = HashMap::new();
    let mut at = 0;
    for &b in &order {
        let size = heap::first_field(b) + heap::len(b);
        // Safety: `room` gave room for every block and its word, and the
        // sizes here are what it was asked for.
        unsafe { *base.add(at) = r as Word };
        let to = unsafe { base.add(at + 1) };
        for i in 0..size {
            // Safety: inside both blocks.
            unsafe { *to.add(i) = heap::word(b, i) };
        }
        let new = to as Word;
        // Never counted again: see the module docs.
        heap::set_word(
            new,
            0,
            ((heap::len(b) as Word) << 32) | Word::from(heap::STICKY),
        );
        heap::set_word(new, 1, heap::word(b, 1) & !heap::MARKS);
        // A compact inside this one keeps its own region, which this copy of
        // the block now names as well.
        if heap::kind(new) == heap::COMPACT {
            // Safety: the block being copied holds a reference to it.
            unsafe { retain(handle(new)) };
        }
        made.insert(b, new);
        at += size + 1;
    }
    // The copies still point at what they were copied from. Point them at
    // each other -- or leave them, where what they point at is in the region
    // already.
    for &b in &order {
        let new = made[&b];
        if !walks(new) {
            continue;
        }
        let first = heap::first_field(new);
        for i in 0..heap::len(new) {
            if heap::field_desc(new, i) != desc::REF {
                continue;
            }
            if let Some(&x) = made.get(&heap::field(new, i)) {
                heap::set_word(new, first + i, x);
            }
        }
    }
    Ok(*made.get(&v).unwrap_or(&v))
}

/// The `Compact` block for a value in `r`, which holds two references to it:
/// the block's own, given up in [`forget`] when the block dies, and -- when
/// the value really is inside the region -- the one its field holds, given up
/// where that field is erased like any other.
fn compact_block(v: Word, d: i64, r: *const Region) -> Word {
    // Only a reference can be in a region, and only a descriptor says a word
    // is one: `42` is an even number, not the address of a block.
    if d == desc::REF && heap::in_region(v) {
        // Safety: the caller holds a reference for the block being built.
        unsafe { retain(r) };
    }
    heap::build(heap::COMPACT, 0, &[v, r as Word], &[d, desc::INT])
}

/// `compact x`: a region holding a copy of `x`, and the compact naming it.
pub fn compact(v: Word, d: i64) -> Result<Word, String> {
    let r = new();
    // Safety: `new` gave a region with one reference, which the block made
    // below takes over.
    let region = unsafe { &*r };
    let made = {
        let mut inside = region.inside.lock().expect("a region's lock");
        copy(&mut inside, r, v, d)
    };
    match made {
        Ok(x) => Ok(compact_block(x, d, r)),
        Err(why) => {
            // Safety: the reference `new` gave, given up: nothing else has one.
            unsafe { release(r) };
            Err(why)
        }
    }
}

/// `compactAdd c x`: `x` copied into `c`'s region, sharing what of it is
/// there already, and a compact naming that region and the new value.
///
/// # Safety
///
/// `c` must be a `Compact` block.
pub unsafe fn add(c: Word, v: Word, d: i64) -> Result<Word, String> {
    let r = handle(c);
    // Safety: the caller's -- `c` holds a reference to it.
    let region = unsafe { &*r };
    let made = {
        let mut inside = region.inside.lock().expect("a region's lock");
        copy(&mut inside, r, v, d)
    };
    let x = made?;
    // Safety: as above; the compact made below has one of its own.
    unsafe { retain(r) };
    Ok(compact_block(x, d, r))
}

/// Bytes `c`'s region holds -- all of it, so every compact sharing a region
/// reports the same.
///
/// # Safety
///
/// `c` must be a `Compact` block.
pub unsafe fn bytes(c: Word) -> usize {
    // Safety: the caller's.
    let region = unsafe { &*handle(c) };
    let inside = region.inside.lock().expect("a region's lock");
    inside.words * 8
}

/// The last reference to a `Compact` block is going: its region loses the one
/// that block held. Called from [`crate::heap`], where the block is freed.
///
/// # Safety
///
/// `c` must be a `Compact` block whose fields are still in it.
pub unsafe fn forget(c: Word) {
    // Safety: the caller's.
    unsafe { release(handle(c)) };
}
