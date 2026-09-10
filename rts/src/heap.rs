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

use crate::value::{Addr, Value};

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
    /// A mutable cell: one field, and the only value with identity.
    Ref,
    /// `meta` is the sign (0 zero, 1 plus, 2 minus); the fields are base-2^32
    /// digits, least significant first, held as `Int`s.
    BigInt,
    /// A one-shot resumption: `[taken, k, (handled, handler, ret_k)…]`.
    Resume,
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
        }
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

    fn head(&self, a: Addr) -> (Kind, u32, u32) {
        match self.space[a as usize] {
            Slot::Header { kind, len, meta } => (kind, len, meta),
            other => unreachable!("{a} is not an object header: {other:?}"),
        }
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
        if let Slot::Header { meta: m, .. } = &mut self.space[a as usize] {
            *m = meta;
        }
    }

    pub fn field(&self, a: Addr, i: usize) -> Value {
        match self.space[a as usize + 1 + i] {
            Slot::Val(v) => v,
            other => unreachable!("field {i} of {a} is not a value: {other:?}"),
        }
    }

    pub fn set_field(&mut self, a: Addr, i: usize, v: Value) {
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
        self.collections += 1;

        let capacity = self.space.len();
        self.other.clear();
        self.other.resize(capacity, Slot::Val(Value::Unit));
        let mut top = 0usize;

        for r in roots.iter_mut() {
            if let Value::Obj(a) = *r {
                *r = Value::Obj(Self::forward(&mut self.space, &mut self.other, &mut top, a));
            }
        }

        // The copied region is its own work queue: appending to it while
        // walking it is what makes the traversal iterative.
        let mut scan = 0usize;
        while scan < top {
            let (_, len, _) = match self.other[scan] {
                Slot::Header { kind, len, meta } => (kind, len, meta),
                other => unreachable!("scan is not at a header: {other:?}"),
            };
            for i in 0..len as usize {
                if let Slot::Val(Value::Obj(a)) = self.other[scan + 1 + i] {
                    let to = Self::forward(&mut self.space, &mut self.other, &mut top, a);
                    self.other[scan + 1 + i] = Slot::Val(Value::Obj(to));
                }
            }
            scan += 1 + len as usize;
        }

        std::mem::swap(&mut self.space, &mut self.other);
        self.top = top;

        // More than half full after collecting means the next cycle would come
        // almost immediately. Grow instead, so collection stays amortised.
        if self.top * 2 > self.space.len() {
            let bigger = (self.space.len() * 2).max(INITIAL);
            self.space.resize(bigger, Slot::Val(Value::Unit));
        }
    }

    /// Copy one object into to-space if it is not there already, and answer
    /// where it is.
    fn forward(from: &mut [Slot], to: &mut [Slot], top: &mut usize, a: Addr) -> Addr {
        match from[a as usize] {
            // Already copied: every other reference to it lands here and shares.
            Slot::Forward(n) => n,
            Slot::Header { kind, len, meta } => {
                let at = *top;
                to[at] = Slot::Header { kind, len, meta };
                for i in 0..len as usize {
                    to[at + 1 + i] = from[a as usize + 1 + i];
                }
                *top += 1 + len as usize;
                from[a as usize] = Slot::Forward(at as Addr);
                at as Addr
            }
            other => unreachable!("{a} is not an object header: {other:?}"),
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
