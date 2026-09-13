//! The heap, and a Cheney semispace collector.
//!
//! # The algorithm
//!
//! Two spaces of equal size. Everything is allocated by bumping a pointer in
//! one of them. When it fills:
//!
//! 1. **Forward the roots.** Copy each object a root points at into the other
//!    space, and overwrite its old header with a [`Slot::Forward`] to where it
//!    went. A second root pointing at the same object finds that forwarding and
//!    reuses it, so sharing survives and a cycle terminates.
//! 2. **Scan.** Walk the objects just copied, from the bottom, forwarding every
//!    address in their fields. Copying appends to the same region being walked,
//!    so the region *is* the queue — no auxiliary stack, and therefore no depth
//!    limit. A list of a million elements collects in constant extra space,
//!    which a mark-and-sweep with a recursive trace could not do.
//! 3. **Swap.** The other space becomes the live one, and what was left behind
//!    is garbage in bulk: nothing is freed one object at a time.
//!
//! Live data is copied, garbage is not, so a collection costs what *survives*
//! rather than what was allocated. Allocation is then a bounds check and an
//! increment.
//!
//! # The rule the rest of the runtime has to obey
//!
//! Objects move. An [`Addr`] held anywhere the collector cannot see becomes
//! wrong the moment a collection happens — so the VM never holds one across an
//! allocation. It calls [`crate::Vm::ensure`] first, with room for everything an
//! operation will build, and only then reads its arguments out of registers.
//! Registers are roots; Rust locals are not.
//!
//! # Layout
//!
//! One header slot, then the fields:
//!
//! ```text
//!   addr      Header { kind, len, meta }
//!   addr+1    field 0
//!   ...
//!   addr+len  field len-1
//! ```
//!
//! `meta` is per-kind: a constructor tag, a method table, a sign. Fields are
//! always [`Value`]s — even a `BigInt`'s digits, which are `Int`s and simply
//! contain no addresses for the scan to find. That costs memory and buys a
//! collector with no per-kind tracing rules at all.
//!
//! # Compact regions
//!
//! A collection copies everything alive, so a large structure that lives a long
//! time is paid for again at every one. `compact` moves such a structure out of
//! the semispaces into a **region**: blocks of slots at addresses from
//! [`REGION_BASE`] up, which the collector neither copies nor scans.
//!
//! Not scanning is sound because a region is **closed and immutable**. Copying
//! into one follows every pointer, so nothing in it refers to the semispaces or
//! to another region, and the kinds that can be written after they are built --
//! a `Ref`, a mutable array, a resumption -- are refused, along with functions.
//! So a region is one object as far as liveness goes: an address into it, or a
//! [`Kind::Compact`] handle naming it, found while collecting marks the whole
//! region live, and every region left unmarked afterwards is freed at once.
//!
//! Addresses tell the two apart with one comparison, which is all the ordinary
//! field access pays.

use std::collections::HashMap;

use crate::value::{Addr, Value};

/// Addresses below this are the semispace; at or above it, a region.
pub const REGION_BASE: Addr = 1 << 31;

/// Bytes one slot takes -- what `compactSize` multiplies by. The other engines
/// estimate with `meadow_core::compact::SLOT_BYTES`, which a test holds equal.
pub const SLOT_BYTES: usize = std::mem::size_of::<Slot>();

/// Slots in a region's first block. Later blocks double, so a region built by
/// many small `compactAdd`s still has few blocks.
const REGION_BLOCK: usize = 1 << 12;

/// Why a value cannot go into a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uncompactable {
    Ref,
    MutArray,
    Function,
}

impl Uncompactable {
    pub fn describe(self) -> &'static str {
        match self {
            Uncompactable::Ref => "a Ref",
            Uncompactable::MutArray => "a mutable array",
            Uncompactable::Function => "a function",
        }
    }
}

/// One contiguous run of region addresses. Allocated with its capacity
/// reserved, so nothing in it ever moves.
struct Block {
    base: Addr,
    /// Addresses reserved, `base..base + cap`. Kept rather than read from the
    /// `Vec`, whose capacity may be larger than was asked for.
    cap: usize,
    slots: Vec<Slot>,
    region: u32,
}

/// What a region is, for liveness and for `compactSize`.
struct Region {
    /// Bases of its blocks, in the order they were filled.
    blocks: Vec<Addr>,
    /// Slots in use across them.
    used: usize,
    marked: bool,
}

/// Where a region's contents ended, so a failed `compactAdd` can be undone.
#[derive(Clone, Copy)]
struct RegionEnd {
    blocks: usize,
    last_len: usize,
    used: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A constructor. `meta` is its tag; a tuple is the constructor `#tuple`.
    Data,
    /// The builtin `Array`.
    Array,
    /// Fields as `[label, value, label, value, …]`, sorted by label.
    ///
    /// Sorted, not in the order they were written, so that two records with the
    /// same fields compare and print alike however they were built — the same
    /// reason the CEK machine keeps them in a `BTreeMap`.
    Record,
    /// A closure, a continuation or a handler — the IR does not distinguish
    /// them. `meta` is the method table; the fields are the captures.
    Closure,
    /// A mutable cell: one field, and a value with identity.
    Ref,
    /// A mutable array, from `stNewArray`: its elements, written in place. The
    /// other value with identity.
    MutArray,
    /// `meta` is the sign (0 zero, 1 plus, 2 minus); the fields are base-2^32
    /// digits, least significant first, held as `Int`s.
    BigInt,
    /// A one-shot resumption: `[taken, k, (handled, handler, ret_k)…]`.
    Resume,
    /// A handle on a compact region: `meta` is the region, the one field is the
    /// value compacted into it. What keeps a region alive when the value itself
    /// is an immediate and points nowhere.
    Compact,
}

#[derive(Debug, Clone, Copy)]
pub enum Slot {
    Header { kind: Kind, len: u32, meta: u32 },
    Val(Value),
    /// Left behind in from-space when an object has been copied.
    Forward(Addr),
}

/// How many slots a fresh heap starts with. It doubles whenever a collection
/// leaves it more than half full.
const INITIAL: usize = 1 << 16;

pub struct Heap {
    space: Vec<Slot>,
    other: Vec<Slot>,
    top: usize,
    /// Collections so far, and slots allocated in total — the two numbers worth
    /// knowing when a program is unexpectedly slow.
    pub collections: u64,
    pub allocated: u64,
    /// Slots copied by collections in total, and the time they took: what a
    /// large live heap costs, and what compacting it saves.
    pub copied: u64,
    pub gc_nanos: u64,
    /// Every region block, sorted by base address.
    blocks: Vec<Block>,
    /// Regions by id; a freed id is `None` until reused.
    regions: Vec<Option<Region>>,
    /// Region slots written since the last collection. Regions are only freed
    /// by collecting, so a program that compacts in a loop and allocates little
    /// else needs this to ask for a collection now and then.
    region_growth: usize,
}

impl Default for Heap {
    fn default() -> Heap {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        Heap {
            space: vec![Slot::Val(Value::Unit); INITIAL],
            other: Vec::new(),
            top: 0,
            collections: 0,
            allocated: 0,
            copied: 0,
            gc_nanos: 0,
            blocks: Vec::new(),
            regions: Vec::new(),
            region_growth: 0,
        }
    }

    /// Has enough gone into regions since the last collection that dead ones
    /// should be looked for, even though the semispace has room?
    pub fn wants_collection(&self) -> bool {
        self.region_growth > self.space.len()
    }

    /// Slots held by live regions, in total.
    pub fn region_slots(&self) -> usize {
        self.regions.iter().flatten().map(|r| r.used).sum()
    }

    /// Slots in use, and slots there are.
    pub fn used(&self) -> usize {
        self.top
    }

    pub fn capacity(&self) -> usize {
        self.space.len()
    }

    pub fn room_for(&self, slots: usize) -> bool {
        self.top + slots <= self.space.len()
    }

    /// Grow until `slots` fit, without collecting.
    ///
    /// For the case a collection cannot help with: one object larger than the
    /// whole heap, which a program building a big array does routinely.
    pub fn reserve(&mut self, slots: usize) {
        while self.top + slots > self.space.len() {
            let bigger = (self.space.len() * 2).max(INITIAL);
            self.space.resize(bigger, Slot::Val(Value::Unit));
        }
    }

    /// Allocate. The caller must have checked [`Heap::room_for`] — this is the
    /// bump, and it is the reason allocation is cheap enough not to think about.
    pub fn alloc(&mut self, kind: Kind, meta: u32, fields: &[Value]) -> Addr {
        let at = self.top;
        debug_assert!(self.room_for(1 + fields.len()), "allocated without room");
        self.space[at] = Slot::Header {
            kind,
            len: fields.len() as u32,
            meta,
        };
        for (i, v) in fields.iter().enumerate() {
            self.space[at + 1 + i] = Slot::Val(*v);
        }
        self.top += 1 + fields.len();
        self.allocated += 1 + fields.len() as u64;
        at as Addr
    }

    /// The slot at `a`, in the semispace or in a region.
    #[inline]
    fn slot(&self, a: Addr) -> &Slot {
        if a < REGION_BASE {
            &self.space[a as usize]
        } else {
            let b = &self.blocks[self.block_of(a)];
            &b.slots[(a - b.base) as usize]
        }
    }

    /// The index of the block holding region address `a`.
    fn block_of(&self, a: Addr) -> usize {
        Self::find_block(&self.blocks, a)
    }

    fn find_block(blocks: &[Block], a: Addr) -> usize {
        let i = blocks.partition_point(|b| b.base <= a);
        debug_assert!(i > 0, "{a} is below every region block");
        i - 1
    }

    fn head(&self, a: Addr) -> (Kind, u32, u32) {
        match *self.slot(a) {
            Slot::Header { kind, len, meta } => (kind, len, meta),
            other => unreachable!("{a} is not an object header: {other:?}"),
        }
    }

    /// Is there an object header at `a`? For a debugger reading an address it
    /// cannot vouch for; the machine itself never needs to ask.
    pub fn is_object(&self, a: Addr) -> bool {
        if a < REGION_BASE {
            return (a as usize) < self.top && matches!(self.space[a as usize], Slot::Header { .. });
        }
        let i = self.blocks.partition_point(|b| b.base <= a);
        if i == 0 {
            return false;
        }
        let b = &self.blocks[i - 1];
        matches!(b.slots.get((a - b.base) as usize), Some(Slot::Header { .. }))
    }

    /// Is `a` an address in a compact region?
    pub fn in_region(&self, a: Addr) -> bool {
        a >= REGION_BASE
    }

    pub fn kind(&self, a: Addr) -> Kind {
        self.head(a).0
    }

    pub fn len(&self, a: Addr) -> usize {
        self.head(a).1 as usize
    }

    pub fn meta(&self, a: Addr) -> u32 {
        self.head(a).2
    }

    pub fn set_meta(&mut self, a: Addr, meta: u32) {
        debug_assert!(a < REGION_BASE, "a region is immutable");
        if let Slot::Header { meta: m, .. } = &mut self.space[a as usize] {
            *m = meta;
        }
    }

    pub fn field(&self, a: Addr, i: usize) -> Value {
        match *self.slot(a + 1 + i as Addr) {
            Slot::Val(v) => v,
            other => unreachable!("field {i} of {a} is not a value: {other:?}"),
        }
    }

    pub fn set_field(&mut self, a: Addr, i: usize, v: Value) {
        debug_assert!(a < REGION_BASE, "a region is immutable");
        self.space[a as usize + 1 + i] = Slot::Val(v);
    }

    /// Every field, as a fresh `Vec` — for the places that want to read an
    /// object and then allocate, which cannot hold heap references across the
    /// allocation.
    pub fn fields(&self, a: Addr) -> Vec<Value> {
        (0..self.len(a)).map(|i| self.field(a, i)).collect()
    }

    /// Collect, rewriting `roots` in place.
    ///
    /// The caller passes every address it can still reach — registers, the
    /// handler stack — and reads them back changed. Anything not in `roots` is
    /// garbage by definition, including addresses sitting in Rust locals, which
    /// is why the VM allocates only at points where it holds none.
    pub fn collect(&mut self, roots: &mut [Value]) {
        let started = std::time::Instant::now();
        self.collections += 1;

        let capacity = self.space.len();
        self.other.clear();
        self.other.resize(capacity, Slot::Val(Value::Unit));
        let mut top = 0usize;
        for r in self.regions.iter_mut().flatten() {
            r.marked = false;
        }
        let mut gc = Gc {
            from: &mut self.space,
            to: &mut self.other,
            top: &mut top,
            blocks: &self.blocks,
            regions: &mut self.regions,
        };

        for r in roots.iter_mut() {
            if let Value::Obj(a) = *r {
                *r = Value::Obj(gc.forward(a));
            }
        }

        // The copied region is its own work queue: appending to it while
        // walking it is what makes the traversal iterative.
        let mut scan = 0usize;
        while scan < *gc.top {
            let len = match gc.to[scan] {
                Slot::Header { len, .. } => len,
                other => unreachable!("scan is not at a header: {other:?}"),
            };
            for i in 0..len as usize {
                if let Slot::Val(Value::Obj(a)) = gc.to[scan + 1 + i] {
                    let to = gc.forward(a);
                    gc.to[scan + 1 + i] = Slot::Val(Value::Obj(to));
                }
            }
            scan += 1 + len as usize;
        }

        std::mem::swap(&mut self.space, &mut self.other);
        self.top = top;
        self.copied += top as u64;

        // A region nothing reached is garbage, all of it at once.
        for id in 0..self.regions.len() {
            if matches!(&self.regions[id], Some(r) if !r.marked) {
                self.free_region(id as u32);
            }
        }
        self.region_growth = 0;

        // More than half full after collecting means the next cycle would come
        // almost immediately. Grow instead, so collection stays amortised.
        if self.top * 2 > self.space.len() {
            let bigger = (self.space.len() * 2).max(INITIAL);
            self.space.resize(bigger, Slot::Val(Value::Unit));
        }
        self.gc_nanos += started.elapsed().as_nanos() as u64;
    }

    // --- compact regions ----------------------------------------------------

    /// A new, empty region.
    pub fn new_region(&mut self) -> u32 {
        let region = Region { blocks: Vec::new(), used: 0, marked: false };
        match self.regions.iter().position(Option::is_none) {
            Some(id) => {
                self.regions[id] = Some(region);
                id as u32
            }
            None => {
                self.regions.push(Some(region));
                (self.regions.len() - 1) as u32
            }
        }
    }

    /// Slots in use by region `id`.
    pub fn region_used(&self, id: u32) -> usize {
        self.regions[id as usize].as_ref().map_or(0, |r| r.used)
    }

    /// Free region `id` now, rather than at the next collection. Only for a
    /// region nothing can refer to -- one whose `compact` failed.
    pub fn free_region(&mut self, id: u32) {
        if let Some(r) = self.regions[id as usize].take() {
            self.blocks.retain(|b| !r.blocks.contains(&b.base));
        }
    }

    fn region(&self, id: u32) -> &Region {
        self.regions[id as usize].as_ref().expect("a live region")
    }

    fn region_mut(&mut self, id: u32) -> &mut Region {
        self.regions[id as usize].as_mut().expect("a live region")
    }

    fn region_end(&self, id: u32) -> RegionEnd {
        let r = self.region(id);
        let last_len = match r.blocks.last() {
            Some(&base) => self.blocks[self.block_of(base)].slots.len(),
            None => 0,
        };
        RegionEnd { blocks: r.blocks.len(), last_len, used: r.used }
    }

    /// Undo everything written to region `id` after `end`.
    fn truncate_region(&mut self, id: u32, end: RegionEnd) {
        let extra: Vec<Addr> = self.region(id).blocks[end.blocks..].to_vec();
        self.blocks.retain(|b| !extra.contains(&b.base));
        let r = self.region_mut(id);
        r.blocks.truncate(end.blocks);
        r.used = end.used;
        if let Some(&base) = r.blocks.last() {
            let i = self.block_of(base);
            self.blocks[i].slots.truncate(end.last_len);
        }
    }

    /// Room for an object of `slots` slots at the end of region `id`: the base
    /// of the block it goes in.
    fn region_room(&mut self, id: u32, slots: usize) -> usize {
        if let Some(&base) = self.region(id).blocks.last() {
            let i = self.block_of(base);
            let b = &self.blocks[i];
            if b.slots.len() + slots <= b.cap {
                return i;
            }
        }
        let previous = self.region(id).blocks.last().map(|&base| self.blocks[self.block_of(base)].cap);
        let cap = previous.map_or(REGION_BLOCK, |c| c * 2).max(slots);
        let base = self.free_range(cap);
        let block = Block { base, cap, slots: Vec::with_capacity(cap), region: id };
        let at = self.blocks.partition_point(|b| b.base < base);
        self.blocks.insert(at, block);
        self.region_mut(id).blocks.push(base);
        at
    }

    /// The lowest region address with `cap` free addresses after it.
    fn free_range(&self, cap: usize) -> Addr {
        let mut start = REGION_BASE as u64;
        for b in &self.blocks {
            if b.base as u64 >= start + cap as u64 {
                break;
            }
            start = b.base as u64 + b.cap as u64;
        }
        assert!(
            start + cap as u64 <= u32::MAX as u64,
            "compact regions have used up the address space"
        );
        start as Addr
    }

    /// Append an object to region `id`, its fields as given.
    fn region_alloc(&mut self, id: u32, kind: Kind, meta: u32, fields: &[Value]) -> Addr {
        let i = self.region_room(id, 1 + fields.len());
        let b = &mut self.blocks[i];
        let at = b.base + b.slots.len() as Addr;
        b.slots.push(Slot::Header { kind, len: fields.len() as u32, meta });
        b.slots.extend(fields.iter().map(|v| Slot::Val(*v)));
        self.region_mut(id).used += 1 + fields.len();
        self.region_growth += 1 + fields.len();
        at
    }

    /// Copy `v`, and everything it reaches, into region `id`, and answer the
    /// copy. What is already in `id` is shared rather than copied again, and
    /// sharing inside `v` survives: each object is copied once.
    ///
    /// Iterative, as the collector is: the objects just appended to the region
    /// are the queue. If `v` reaches something that cannot be compacted, the
    /// region is put back exactly as it was.
    pub fn compact_into(&mut self, id: u32, v: Value) -> Result<Value, Uncompactable> {
        let end = self.region_end(id);
        let result = self.copy_into(id, v, end);
        if result.is_err() {
            self.truncate_region(id, end);
        }
        result
    }

    fn copy_into(&mut self, id: u32, v: Value, end: RegionEnd) -> Result<Value, Uncompactable> {
        let mut copies: HashMap<Addr, Addr> = HashMap::new();
        let root = match v {
            Value::Obj(a) => Value::Obj(self.copy_object(id, a, &mut copies)?),
            other => other,
        };

        // Scan what was appended, block by block, fixing each field to the copy
        // of what it points at.
        let mut block = end.blocks.saturating_sub(1);
        let mut offset = if end.blocks == 0 { 0 } else { end.last_len };
        loop {
            let Some(&base) = self.region(id).blocks.get(block) else { break };
            let bi = self.block_of(base);
            if offset >= self.blocks[bi].slots.len() {
                if block + 1 >= self.region(id).blocks.len() {
                    break;
                }
                block += 1;
                offset = 0;
                continue;
            }
            let len = match self.blocks[bi].slots[offset] {
                Slot::Header { len, .. } => len as usize,
                other => unreachable!("region scan is not at a header: {other:?}"),
            };
            for f in 0..len {
                let bi = self.block_of(base);
                if let Slot::Val(Value::Obj(a)) = self.blocks[bi].slots[offset + 1 + f] {
                    let to = self.copy_object(id, a, &mut copies)?;
                    let bi = self.block_of(base);
                    self.blocks[bi].slots[offset + 1 + f] = Slot::Val(Value::Obj(to));
                }
            }
            offset += 1 + len;
        }
        Ok(root)
    }

    /// The copy of object `a` in region `id`, making it if there is none yet.
    /// Its fields are copied as they are, to be fixed by the scan.
    fn copy_object(
        &mut self,
        id: u32,
        a: Addr,
        copies: &mut HashMap<Addr, Addr>,
    ) -> Result<Addr, Uncompactable> {
        if a >= REGION_BASE && self.blocks[self.block_of(a)].region == id {
            return Ok(a);
        }
        if let Some(&c) = copies.get(&a) {
            return Ok(c);
        }
        let (kind, _, meta) = self.head(a);
        let meta = match kind {
            Kind::Ref => return Err(Uncompactable::Ref),
            Kind::MutArray => return Err(Uncompactable::MutArray),
            Kind::Closure | Kind::Resume => return Err(Uncompactable::Function),
            // A handle inside a compacted value now names the region it is in,
            // since that is where its contents are copied.
            Kind::Compact => id,
            _ => meta,
        };
        let fields = self.fields(a);
        let c = self.region_alloc(id, kind, meta, &fields);
        copies.insert(a, c);
        Ok(c)
    }
}

/// A collection in progress: the two spaces, and the regions it marks.
struct Gc<'h> {
    from: &'h mut Vec<Slot>,
    to: &'h mut Vec<Slot>,
    top: &'h mut usize,
    blocks: &'h [Block],
    regions: &'h mut Vec<Option<Region>>,
}

impl Gc<'_> {
    /// Copy one object into to-space if it is not there already, and answer
    /// where it is. An address in a region stays put and marks its region.
    fn forward(&mut self, a: Addr) -> Addr {
        if a >= REGION_BASE {
            let region = self.blocks[Heap::find_block(self.blocks, a)].region;
            self.mark(region);
            return a;
        }
        match self.from[a as usize] {
            // Already copied: every other reference to it lands here and shares.
            Slot::Forward(n) => n,
            Slot::Header { kind, len, meta } => {
                if kind == Kind::Compact {
                    self.mark(meta);
                }
                let at = *self.top;
                self.to[at] = Slot::Header { kind, len, meta };
                for i in 0..len as usize {
                    self.to[at + 1 + i] = self.from[a as usize + 1 + i];
                }
                *self.top += 1 + len as usize;
                self.from[a as usize] = Slot::Forward(at as Addr);
                at as Addr
            }
            other => unreachable!("{a} is not an object header: {other:?}"),
        }
    }

    fn mark(&mut self, region: u32) {
        if let Some(Some(r)) = self.regions.get_mut(region as usize) {
            r.marked = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_a_bump_and_reading_back_works() {
        let mut h = Heap::new();
        let a = h.alloc(Kind::Data, 7, &[Value::Int(1), Value::Int(2)]);
        assert_eq!(h.kind(a), Kind::Data);
        assert_eq!(h.meta(a), 7);
        assert_eq!(h.len(a), 2);
        assert_eq!(h.field(a, 1), Value::Int(2));
        assert_eq!(h.used(), 3);
    }

    #[test]
    fn collecting_keeps_what_is_rooted_and_drops_the_rest() {
        let mut h = Heap::new();
        let keep = h.alloc(Kind::Data, 1, &[Value::Int(42)]);
        let _drop = h.alloc(Kind::Data, 2, &[Value::Int(99)]);
        assert_eq!(h.used(), 4);

        let mut roots = [Value::Obj(keep)];
        h.collect(&mut roots);

        assert_eq!(h.used(), 2, "only the rooted object survived");
        let a = roots[0].addr().expect("still an object");
        assert_eq!(h.meta(a), 1);
        assert_eq!(h.field(a, 0), Value::Int(42));
    }

    #[test]
    fn sharing_survives_and_a_cycle_terminates() {
        let mut h = Heap::new();
        let shared = h.alloc(Kind::Data, 1, &[Value::Int(1)]);
        let a = h.alloc(Kind::Data, 2, &[Value::Obj(shared)]);
        let b = h.alloc(Kind::Data, 3, &[Value::Obj(shared)]);
        // A cycle: `a` points at itself through a mutable field. Reference
        // counting could never free this; a copying collector does not care.
        let cyc = h.alloc(Kind::Ref, 0, &[Value::Unit]);
        h.set_field(cyc, 0, Value::Obj(cyc));

        let mut roots = [Value::Obj(a), Value::Obj(b), Value::Obj(cyc)];
        h.collect(&mut roots);

        let (a, b, cyc) = (
            roots[0].addr().unwrap(),
            roots[1].addr().unwrap(),
            roots[2].addr().unwrap(),
        );
        assert_eq!(
            h.field(a, 0),
            h.field(b, 0),
            "one copy of the shared object, not two"
        );
        assert_eq!(h.field(cyc, 0), Value::Obj(cyc), "the cycle came back intact");
    }

    #[test]
    fn a_long_chain_collects_without_recursing() {
        // 200_000 deep. Cheney scans the copied region as a queue, so this costs
        // no stack at all — which is exactly what the `Rc` version could not do
        // when it came to dropping one of these.
        let mut h = Heap::new();
        let mut cur = h.alloc(Kind::Data, 0, &[]);
        for i in 0..200_000 {
            if !h.room_for(3) {
                let mut roots = [Value::Obj(cur)];
                h.collect(&mut roots);
                cur = roots[0].addr().unwrap();
            }
            cur = h.alloc(Kind::Data, 1, &[Value::Int(i), Value::Obj(cur)]);
        }
        let mut roots = [Value::Obj(cur)];
        h.collect(&mut roots);

        // Walk it to be sure every link came through.
        let mut n = 0;
        let mut at = roots[0].addr().unwrap();
        while h.len(at) == 2 {
            n += 1;
            at = h.field(at, 1).addr().unwrap();
        }
        assert_eq!(n, 200_000);
    }

    /// A `Cons`-like chain of `n` cells, newest first.
    fn chain(h: &mut Heap, n: i64) -> Value {
        let mut cur = Value::Obj(h.alloc(Kind::Data, 0, &[]));
        for i in 0..n {
            if !h.room_for(3) {
                let mut roots = [cur];
                h.collect(&mut roots);
                cur = roots[0];
            }
            cur = Value::Obj(h.alloc(Kind::Data, 1, &[Value::Int(i), cur]));
        }
        cur
    }

    fn chain_len(h: &Heap, mut v: Value) -> usize {
        let mut n = 0;
        while let Value::Obj(a) = v {
            if h.len(a) == 0 {
                break;
            }
            n += 1;
            v = h.field(a, 1);
        }
        n
    }

    #[test]
    fn the_engines_estimate_compact_size_with_the_real_slot() {
        assert_eq!(SLOT_BYTES, meadow_core::compact::SLOT_BYTES);
    }

    #[test]
    fn a_compacted_value_is_neither_copied_nor_scanned() {
        let mut h = Heap::new();
        let big = chain(&mut h, 50_000);
        let region = h.new_region();
        let root = h.compact_into(region, big).unwrap();
        assert!(h.in_region(root.addr().unwrap()));
        assert_eq!(h.region_used(region), 1 + 50_000 * 3);

        let handle = h.alloc(Kind::Compact, region, &[root]);
        let mut roots = [Value::Obj(handle)];
        let copied = h.copied;
        h.collect(&mut roots);
        // Only the handle came across; the chain stayed where it was.
        assert_eq!(h.copied - copied, 2);
        assert_eq!(h.used(), 2);
        let root = h.field(roots[0].addr().unwrap(), 0);
        assert_eq!(chain_len(&h, root), 50_000);
    }

    #[test]
    fn an_address_into_a_region_keeps_it_alive_without_the_handle() {
        let mut h = Heap::new();
        let big = chain(&mut h, 100);
        let region = h.new_region();
        let root = h.compact_into(region, big).unwrap();
        // A semispace object pointing into the region, and no handle at all.
        let holder = h.alloc(Kind::Data, 9, &[root]);
        let mut roots = [Value::Obj(holder)];
        h.collect(&mut roots);
        assert_eq!(h.region_used(region), 1 + 100 * 3);
        let root = h.field(roots[0].addr().unwrap(), 0);
        assert_eq!(chain_len(&h, root), 100);
    }

    #[test]
    fn an_unreachable_region_is_freed_at_the_next_collection() {
        let mut h = Heap::new();
        let big = chain(&mut h, 1000);
        let region = h.new_region();
        h.compact_into(region, big).unwrap();
        assert!(h.region_slots() > 0);
        h.collect(&mut []);
        assert_eq!(h.region_slots(), 0);
        assert!(h.blocks.is_empty(), "its blocks went with it");
        // And its id and addresses are there to be used again.
        let again = h.new_region();
        assert_eq!(again, region);
    }

    #[test]
    fn sharing_inside_a_compacted_value_survives() {
        let mut h = Heap::new();
        let shared = h.alloc(Kind::Data, 1, &[Value::Int(7)]);
        let pair = h.alloc(Kind::Data, 2, &[Value::Obj(shared), Value::Obj(shared)]);
        let region = h.new_region();
        let root = h.compact_into(region, Value::Obj(pair)).unwrap().addr().unwrap();
        assert_eq!(h.field(root, 0), h.field(root, 1), "one copy, not two");
        assert_eq!(h.region_used(region), 3 + 2);
    }

    #[test]
    fn adding_to_a_region_shares_what_is_already_there() {
        let mut h = Heap::new();
        let big = chain(&mut h, 1000);
        let region = h.new_region();
        let root = h.compact_into(region, big).unwrap();
        let before = h.region_used(region);
        // A new cell on top of the compacted chain: only the cell is copied.
        let longer = h.alloc(Kind::Data, 1, &[Value::Int(-1), root]);
        let root2 = h.compact_into(region, Value::Obj(longer)).unwrap();
        assert_eq!(h.region_used(region), before + 3);
        assert_eq!(chain_len(&h, root2), 1001);
    }

    #[test]
    fn a_mutable_cell_is_refused_and_leaves_the_region_as_it_was() {
        let mut h = Heap::new();
        let ok = chain(&mut h, 10);
        let region = h.new_region();
        h.compact_into(region, ok).unwrap();
        let before = h.region_used(region);

        let cell = h.alloc(Kind::Ref, 0, &[Value::Int(1)]);
        let bad = h.alloc(Kind::Data, 1, &[Value::Int(0), Value::Obj(cell)]);
        assert_eq!(h.compact_into(region, Value::Obj(bad)), Err(Uncompactable::Ref));
        assert_eq!(h.region_used(region), before);
        let f = h.alloc(Kind::Closure, 0, &[]);
        assert_eq!(h.compact_into(region, Value::Obj(f)), Err(Uncompactable::Function));
    }

    #[test]
    fn a_region_spans_blocks_and_scans_across_them() {
        // Bigger than the first block, so the copy has to continue its scan in
        // the next one.
        let mut h = Heap::new();
        let big = chain(&mut h, REGION_BLOCK as i64);
        let region = h.new_region();
        let root = h.compact_into(region, big).unwrap();
        assert!(h.region(region).blocks.len() > 1);
        assert_eq!(chain_len(&h, root), REGION_BLOCK);
        // Everything in it points inside it.
        let mut v = root;
        while let Value::Obj(a) = v {
            assert!(h.in_region(a));
            if h.len(a) == 0 {
                break;
            }
            v = h.field(a, 1);
        }
    }

    #[test]
    fn the_heap_grows_rather_than_collecting_forever() {
        let mut h = Heap::new();
        let before = h.capacity();
        // Keep everything alive, so collection can never reclaim anything.
        let mut live = Vec::new();
        while h.capacity() == before {
            if !h.room_for(2) {
                let mut roots: Vec<Value> = live.clone();
                h.collect(&mut roots);
                live = roots;
            }
            live.push(Value::Obj(h.alloc(Kind::Data, 0, &[Value::Int(0)])));
        }
        assert!(h.capacity() > before, "a full heap has to grow");
    }
}
