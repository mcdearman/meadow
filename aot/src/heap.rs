//! **Blocks, counted by reference**, the AxCut paper's way (`docs/AOT.md`).
//!
//! A reference is a word: `0` is nothing, an odd word is an object with no
//! block (a nullary constructor, a closure with no captures), and any other
//! word is the address of a block:
//!
//! ```text
//! word 0   extra references: u32 (0: the only one) | length: u32
//! word 1   kind: u8 | element descriptor: u4 | uniform: u1 | … | meta: u32
//! words    field descriptors, four bits each, sixteen to a word
//!          (absent for a uniform block, whose one descriptor is in word 1)
//! fields
//! ```
//!
//! What the paper does, and so what this does:
//!
//! - **share** adds one to the count;
//! - **erase** takes one from it, or -- if it was the last -- puts the block on
//!   the list of blocks _pending_, fields and all, without looking inside;
//! - **clean**: a block whose fields were moved out by the last reference
//!   loading them (the paper's `release`) is reusable at once;
//! - **acquire** takes a clean block of the size wanted, or fresh memory --
//!   and first erases some of what is pending, a bounded amount, so that no
//!   operation ever walks a structure and garbage never waits for long.
//!
//! The paper has one size of block and chains a bigger object through its last
//! field; Meadow has arrays and strings, which want indexing, so blocks come in
//! a size class per word count, with anything bigger allocated on its own.

/// What word 1's low byte says a block is. `meadow_llvm::emit::kind` agrees.
pub const DATA: u64 = 1;
pub const CLOSURE: u64 = 2;
pub const STRING: u64 = 3;
pub const ARRAY: u64 = 4;
pub const BIGINT: u64 = 5;
pub const CELL: u64 = 6;
pub const RECORD: u64 = 7;
pub const MUT_ARRAY: u64 = 8;
/// A resumption's one-shot flag.
pub const ONCE: u64 = 9;
pub const COMPACT: u64 = 10;
pub const CHANNEL: u64 = 11;
pub const TASK: u64 = 12;
pub const TVAR: u64 = 13;
/// A suspended stack segment: see `crate::segments`.
pub const STACK: u64 = 14;
/// What a compact's values live in: see `crate::prims`, `Compact`.
pub const REGION: u64 = 15;

pub const DESC_SHIFT: u32 = 8;
pub const UNIFORM: u64 = 1 << 12;

/// Word counts with a size class of their own; bigger blocks are allocated
/// alone and freed at once when clean.
const CLASSES: usize = 128;
/// Memory taken from the system at a time for the size classes, in words: the
/// first chunk is small -- a thread may allocate almost nothing -- and each
/// after it twice the last, up to this.
const CHUNK: usize = 1 << 17;
const FIRST_CHUNK: usize = 1 << 10;
/// Fields erased per step of the pending work.
const STEP: usize = 64;

pub type Word = u64;

/// A heap: one per green thread (`crate::ctx`), freed with it.
pub struct Heap {
    clean: Vec<Vec<*mut Word>>,
    /// Blocks whose last reference was erased, and how far into their fields
    /// erasing has got.
    pending: Vec<(*mut Word, usize)>,
    chunk: *mut Word,
    left: usize,
    /// Blocks acquired and not yet clean: what a leak check counts.
    live: isize,
    /// Which, when a leak check asks: see [`tracking`].
    blocks: Option<std::collections::HashSet<usize>>,
    /// The memory it has: chunks, and blocks too big for a size class --
    /// what dropping it gives back.
    chunks: Vec<(*mut Word, usize)>,
    large: std::collections::HashMap<usize, usize>,
}

impl Heap {
    pub fn new() -> Heap {
        Heap {
            clean: Vec::new(),
            pending: Vec::new(),
            chunk: std::ptr::null_mut(),
            left: 0,
            live: 0,
            blocks: None,
            chunks: Vec::new(),
            large: std::collections::HashMap::new(),
        }
    }
}

impl Default for Heap {
    fn default() -> Heap {
        Heap::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let words = |n: usize| std::alloc::Layout::array::<Word>(n).expect("a block fits memory");
        for (p, n) in self.chunks.drain(..) {
            // Safety: allocated with this layout in `take`.
            unsafe { std::alloc::dealloc(p as *mut u8, words(n)) };
        }
        for (p, n) in self.large.drain() {
            // Safety: likewise.
            unsafe { std::alloc::dealloc(p as *mut u8, words(n)) };
        }
    }
}

fn with<T>(f: impl FnOnce(&mut Heap) -> T) -> T {
    // Safety: the running green thread's heap, and nothing here re-enters
    // it while it is borrowed -- erasing pushes onto `pending` rather than
    // recursing.
    f(unsafe { &mut (*crate::ctx::get()).heap })
}

/// Is `v` the address of a block?
#[inline]
pub fn is_block(v: Word) -> bool {
    v != 0 && v & 1 == 0
}

#[inline]
fn ptr(v: Word) -> *mut Word {
    v as *mut Word
}

/// Word `i` of the block at `v`.
#[inline]
pub fn word(v: Word, i: usize) -> Word {
    // Safety: callers ask for words inside a live block.
    unsafe { *ptr(v).add(i) }
}

#[inline]
pub fn set_word(v: Word, i: usize, w: Word) {
    // Safety: as `word`.
    unsafe { *ptr(v).add(i) = w }
}

pub fn len(v: Word) -> usize {
    (word(v, 0) >> 32) as usize
}

pub fn kind(v: Word) -> u64 {
    word(v, 1) & 0xFF
}

pub fn meta(v: Word) -> u32 {
    (word(v, 1) >> 32) as u32
}

fn uniform(v: Word) -> bool {
    word(v, 1) & UNIFORM != 0
}

/// Where the fields start.
pub fn first_field(v: Word) -> usize {
    if uniform(v) {
        2
    } else {
        2 + len(v).div_ceil(16)
    }
}

/// Words the block takes.
fn size(v: Word) -> usize {
    first_field(v) + len(v)
}

/// Field `i`'s descriptor.
pub fn field_desc(v: Word, i: usize) -> i64 {
    if uniform(v) {
        ((word(v, 1) >> DESC_SHIFT) & 15) as i64
    } else {
        ((word(v, 2 + i / 16) >> (4 * (i % 16))) & 15) as i64
    }
}

pub fn field(v: Word, i: usize) -> Word {
    word(v, first_field(v) + i)
}

/// Write field `i` of the non-uniform block at `v`, and its descriptor.
pub fn set_field(v: Word, i: usize, x: Word, d: i64) {
    set_word(v, first_field(v) + i, x);
    if !uniform(v) {
        let at = 2 + i / 16;
        let shift = 4 * (i % 16);
        let w = (word(v, at) & !(15 << shift)) | (((d as u64) & 15) << shift);
        set_word(v, at, w);
    }
}

/// One reference more to `v`, described by `d`.
#[inline]
pub fn share(v: Word, d: i64) {
    if d == meadow_core::desc::REF && is_block(v) {
        // Safety: a live block's count.
        unsafe { *(v as *mut u32) += 1 }
    }
}

/// One reference fewer.
#[inline]
pub fn erase(v: Word, d: i64) {
    if d == meadow_core::desc::REF && is_block(v) {
        // Safety: a live block's count.
        let rc = unsafe { &mut *(v as *mut u32) };
        if *rc == 0 {
            with(|h| h.pending.push((ptr(v), 0)));
        } else {
            *rc -= 1;
        }
    }
}

/// A block of `words` words, count 0, the rest for the caller to write.
pub fn acquire(words: usize) -> Word {
    with(|h| {
        // Work off what is pending: one step always, more while nothing clean
        // of this size is to be had.
        h.step();
        let want = words < CLASSES;
        while want && h.clean.get(words).is_none_or(Vec::is_empty) && !h.pending.is_empty() {
            h.step();
        }
        h.live += 1;
        let p = h.take(words);
        if let Some(b) = &mut h.blocks {
            b.insert(p as usize);
        }
        p
    })
}

/// Is the leak check on? `MEADOW_AOT_LEAKS`.
pub fn tracking() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MEADOW_AOT_LEAKS").is_some())
}

impl Heap {
    fn take(&mut self, words: usize) -> Word {
        let h = self;
        if h.blocks.is_none() && tracking() {
            h.blocks = Some(std::collections::HashSet::new());
        }
        let want = words < CLASSES;
        if !want {
            let layout = std::alloc::Layout::array::<Word>(words).expect("a block fits memory");
            // Safety: a non-zero size.
            let p = unsafe { std::alloc::alloc(layout) } as *mut Word;
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            h.large.insert(p as usize, words);
            return p as Word;
        }
        if let Some(p) = h.clean.get_mut(words).and_then(Vec::pop) {
            return p as Word;
        }
        if h.left < words {
            let size = h
                .chunks
                .last()
                .map_or(FIRST_CHUNK, |(_, n)| (2 * n).min(CHUNK));
            let layout = std::alloc::Layout::array::<Word>(size).expect("a chunk fits memory");
            // Safety: a non-zero size.
            let p = unsafe { std::alloc::alloc(layout) } as *mut Word;
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            h.chunks.push((p, size));
            h.chunk = p;
            h.left = size;
        }
        let p = h.chunk;
        // Safety: inside the chunk, which has `left` words from `chunk`.
        h.chunk = unsafe { p.add(words) };
        h.left -= words;
        p as Word
    }
}

/// The block at `v` is done with: its fields were moved out, or erased.
pub fn clean(v: Word) {
    let words = size(v);
    with(|h| h.clean_block(ptr(v), words));
}

impl Heap {
    fn clean_block(&mut self, p: *mut Word, words: usize) {
        self.live -= 1;
        if let Some(b) = &mut self.blocks {
            b.remove(&(p as usize));
        }
        if words >= CLASSES {
            let layout = std::alloc::Layout::array::<Word>(words).expect("a block fits memory");
            self.large.remove(&(p as usize));
            // Safety: allocated alone, with this layout, in `acquire`.
            unsafe { std::alloc::dealloc(p as *mut u8, layout) };
            return;
        }
        if self.clean.len() <= words {
            self.clean.resize_with(words + 1, Vec::new);
        }
        self.clean[words].push(p);
    }

    /// Erase up to [`STEP`] fields of the block pending longest.
    fn step(&mut self) {
        let Some((p, from)) = self.pending.pop() else {
            return;
        };
        let v = p as Word;
        let n = len(v);
        let to = (from + STEP).min(n);
        let first = first_field(v);
        let refs = kind(v) != STRING && kind(v) != BIGINT;
        if refs {
            for i in from..to {
                let d = field_desc(v, i);
                let x = word(v, first + i);
                if d == meadow_core::desc::REF && is_block(x) {
                    // Safety: a live block's count.
                    let rc = unsafe { &mut *(x as *mut u32) };
                    if *rc == 0 {
                        self.pending.push((ptr(x), 0));
                    } else {
                        *rc -= 1;
                    }
                }
            }
        }
        if to < n && refs {
            self.pending.push((p, to));
        } else {
            // A continuation nobody can resume now: its segments go.
            if kind(v) == STACK {
                crate::segments::discard(meta(v));
            }
            let words = first + n;
            self.clean_block(p, words);
        }
    }
}

/// Blocks acquired and not yet clean, on this thread -- after everything
/// pending has been erased.
pub fn live() -> isize {
    with(|h| {
        while !h.pending.is_empty() {
            h.step();
        }
        h.live
    })
}

// --- building -------------------------------------------------------------

/// A non-uniform block of `kind` and `meta` holding `fields`, described by
/// `descs`. No fields is no block.
pub fn build(kind: u64, meta: u32, fields: &[Word], descs: &[i64]) -> Word {
    let n = fields.len();
    if n == 0 {
        return (u64::from(meta) << 1) | 1;
    }
    let dw = n.div_ceil(16);
    let v = acquire(2 + dw + n);
    set_word(v, 0, (n as u64) << 32);
    set_word(v, 1, kind | (u64::from(meta) << 32));
    for w in 0..dw {
        let mut bits = 0u64;
        for (j, d) in descs.iter().enumerate().skip(16 * w).take(16) {
            bits |= ((*d as u64) & 15) << (4 * (j - 16 * w));
        }
        set_word(v, 2 + w, bits);
    }
    for (i, x) in fields.iter().enumerate() {
        set_word(v, 2 + dw + i, *x);
    }
    v
}

/// A uniform block of `kind` and `meta` of `n` words, every one described by
/// `d`, the words for the caller to write.
pub fn build_uniform(kind: u64, meta: u32, n: usize, d: i64) -> Word {
    let v = acquire(2 + n);
    set_word(v, 0, (n as u64) << 32);
    set_word(
        v,
        1,
        kind | ((d as u64 & 15) << DESC_SHIFT) | UNIFORM | (u64::from(meta) << 32),
    );
    v
}

/// A string of `bytes`, eight to a word.
pub fn string(bytes: &[u8]) -> Word {
    let n = bytes.len().div_ceil(8);
    let v = build_uniform(STRING, bytes.len() as u32, n, meadow_core::desc::INT);
    for i in 0..n {
        let chunk = &bytes[8 * i..bytes.len().min(8 * i + 8)];
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        set_word(v, 2 + i, Word::from_le_bytes(w));
    }
    v
}

/// The bytes of the string at `v`, copied out.
pub fn bytes(v: Word) -> Vec<u8> {
    str_bytes(v).to_vec()
}

/// The bytes of the string at `v`, where they are: eight to a word,
/// little-endian, from word 2 -- which is to say one run of bytes in memory.
/// Valid as long as the string is.
pub fn str_bytes<'a>(v: Word) -> &'a [u8] {
    // Safety: a live string block, whose words from 2 hold `meta` bytes.
    unsafe { std::slice::from_raw_parts((v as *const u8).add(16), meta(v) as usize) }
}

/// The blocks reachable from `roots`.
pub fn reachable(roots: &[(Word, i64)]) -> std::collections::HashSet<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Word, i64)> = roots.to_vec();
    while let Some((w, d)) = stack.pop() {
        if d != meadow_core::desc::REF || !is_block(w) || !seen.insert(w as usize) {
            continue;
        }
        if kind(w) != STRING && kind(w) != BIGINT {
            for i in 0..len(w) {
                stack.push((field(w, i), field_desc(w, i)));
            }
        }
    }
    seen
}

/// Live blocks not reachable from `roots` -- what the run leaked -- by kind
/// and constructor tag, most first; and how many there are.
pub fn leaked(roots: &[(Word, i64)]) -> (usize, Vec<(usize, u64, u32)>) {
    let kept = reachable(roots);
    with(|h| {
        while !h.pending.is_empty() {
            h.step();
        }
        let mut counts: std::collections::HashMap<(u64, u32), usize> =
            std::collections::HashMap::new();
        let mut total = 0;
        for b in h.blocks.iter().flatten() {
            if kept.contains(b) {
                continue;
            }
            total += 1;
            let v = *b as Word;
            let key = (
                kind(v),
                if kind(v) == DATA || kind(v) == CLOSURE {
                    meta(v)
                } else {
                    0
                },
            );
            *counts.entry(key).or_default() += 1;
        }
        let mut out: Vec<(usize, u64, u32)> =
            counts.into_iter().map(|((k, m), n)| (n, k, m)).collect();
        out.sort_by(|a, b| b.0.cmp(&a.0));
        (total, out)
    })
}
