//! The heap: a nursery, an old generation marked while the program runs, and
//! the regions it refers to.
//!
//! Every green thread has a heap of its own, so everything here is one
//! thread's business, and a pause stops only that thread. What this module is
//! built around is keeping those pauses short however much the thread keeps
//! alive -- well under a millisecond -- rather than making the total time spent
//! collecting as small as it can be.
//!
//! # The nursery
//!
//! New objects are bumped into the nursery, and when it fills it is collected
//! the Cheney way: the roots are copied into the other half, the copies are
//! scanned as a queue and what they reach copied after them, and what is left
//! behind is garbage in bulk. That costs what survives, and almost nothing
//! does. What has survived one nursery collection already is **promoted** into
//! the old generation at the next, so nothing is copied back and forth for long,
//! and the nursery has a fixed largest size -- [`GcConfig::nursery`] -- so a
//! nursery collection has a fixed longest pause.
//!
//! Until a heap fills its largest nursery, nothing is promoted and the nursery
//! just grows: most green threads never keep enough alive to need an old
//! generation at all, and pay nothing for one.
//!
//! # The old generation
//!
//! Immix blocks and lines -- see [`crate::old`] -- where objects do not move
//! while a cycle marks. It is collected by marking what is reachable, on another OS thread while the
//! program runs -- see [`crate::mark`] -- and then treating every line nothing
//! marked as free. A cycle costs the program two pauses: one to empty the
//! nursery and hand the roots to the marker, and one when marking is done, to
//! adopt its result. Neither does work that grows with the old generation.
//!
//! A cycle begins when the old generation has grown by as much as the last
//! cycle found alive (or [`GcConfig::min_trigger`], if that is more): memory
//! is traded for fewer cycles, and a program that keeps more alive is marked
//! less often for it.
//!
//! Between cycles, the sparsest blocks are **evacuated** a block or two per
//! pause: their survivors copied somewhere denser and the blocks given back.
//! Finding every pointer to a moved object without tracing the heap takes
//! lists the marker and the barriers keep -- see [`crate::evacuate`].
//! [`GcConfig::evacuate`] turns it off: less memory given back, and a slightly
//! shorter tail of pauses.
//!
//! # The remembered set
//!
//! A nursery collection does not look at the old generation, so it has to be
//! told where the old generation points into the nursery: those fields are its
//! remembered set, and it treats them as roots. Such a pointer can only be made
//! three ways -- promoting an object whose field points at something that stays
//! young, allocating a large object straight into the old generation, and
//! overwriting a field of a `Ref`, a mutable array or a resumption -- and each
//! of those checks and records. Data in Meadow is immutable, so the barrier on
//! an ordinary field write, which a generational collector for Java pays on
//! every assignment, is not there at all.
//!
//! # The rule the rest of the runtime has to obey
//!
//! Objects in the nursery move. An [`Addr`] held anywhere the collector cannot
//! see becomes wrong the moment a collection happens -- so the VM never holds
//! one across an allocation. It calls [`crate::Vm::ensure`] first, with room
//! for everything an operation will build, and only then reads its arguments
//! out of registers. Registers are roots; Rust locals are not.
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
//! An address says where it points by its range: below [`OLD_BASE`] the
//! nursery, below [`REGION_BASE`] the old generation, and above that a region.
//!
//! # Compact regions
//!
//! `compact` moves a structure out of the heap into a **region** -- see
//! [`crate::region`] -- which no heap owns and no collector copies or scans. A
//! heap only refers to the regions it has addresses into: an address into one,
//! or a [`Kind::Compact`] handle naming it, found while collecting keeps the
//! heap's reference, and a region a whole collection did not reach is let go.
//! A region several heaps refer to is read by all of them, with no copying,
//! which is what lets compacted data, and a `TVar`'s value, be shared between
//! threads. With an old generation in use, only a finished marking cycle has
//! seen everything, so only that lets a region go.
//!
//! # The other collector
//!
//! [`Collector::Copying`] is the nursery alone, growing without limit and
//! never promoting: one Cheney semispace collector, as this heap was before it
//! had generations. It is there to compare against.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::evacuate::Evacuation;
use crate::mark;
use crate::old::{self, OLD_BASE, Old};
use crate::region::{Block, Region};
use crate::value::{Addr, Value};

pub use crate::region::REGION_BASE;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Bytes one slot takes -- what `compactSize` multiplies by. The other engines
/// estimate with `meadow_core::compact::SLOT_BYTES`, which a test holds equal.
pub const SLOT_BYTES: usize = std::mem::size_of::<Slot>();

// --- configuration ---------------------------------------------------------------

/// Which collector heaps use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collector {
    /// A nursery, and an old generation marked concurrently. The default.
    Generational,
    /// One semispace, copied whole at every collection.
    Copying,
}

#[derive(Debug, Clone, Copy)]
pub struct GcConfig {
    pub collector: Collector,
    /// The nursery's largest size, in slots: what bounds a nursery pause.
    pub nursery: usize,
    /// OS threads marking old generations. With 0, each heap marks a slice at
    /// a time on its own thread, during nursery collections.
    pub mark_threads: usize,
    /// Old-generation slots allocated before a heap's first marking cycle.
    pub min_trigger: usize,
    /// After every marking cycle, check that it marked everything reachable.
    /// Slow: a whole trace in the pause. For tests.
    pub verify: bool,
    /// The most fields a pause reads marking, besides the time limit. For
    /// tests, to make marking take many collections.
    pub mark_slice: usize,
    /// Whether sparse old blocks are evacuated -- see [`crate::evacuate`].
    pub evacuate: bool,
}

impl GcConfig {
    /// The defaults, as `MEADOW_GC` (`generational` or `copying`),
    /// `MEADOW_GC_NURSERY`, `MEADOW_GC_MARK_THREADS`, `MEADOW_GC_TRIGGER` (in
    /// slots), `MEADOW_GC_EVACUATE` (`0` for off) and `MEADOW_GC_VERIFY` change
    /// them.
    pub fn from_env() -> GcConfig {
        let num = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
        };
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        GcConfig {
            collector: match std::env::var("MEADOW_GC").as_deref() {
                Ok("copying") => Collector::Copying,
                _ => Collector::Generational,
            },
            nursery: num("MEADOW_GC_NURSERY").unwrap_or(1 << 15).max(64),
            mark_threads: num("MEADOW_GC_MARK_THREADS").unwrap_or((cores / 4).max(1)),
            min_trigger: num("MEADOW_GC_TRIGGER").unwrap_or(1 << 18),
            verify: std::env::var_os("MEADOW_GC_VERIFY").is_some(),
            mark_slice: usize::MAX,
            evacuate: std::env::var("MEADOW_GC_EVACUATE").as_deref() != Ok("0"),
        }
    }
}

static CONFIG: OnceLock<GcConfig> = OnceLock::new();

/// Set how heaps made from now on collect. Only before the first heap is made:
/// false, and nothing changed, after.
pub fn configure(config: GcConfig) -> bool {
    CONFIG.set(config).is_ok()
}

/// How heaps collect: what [`configure`] set, or [`GcConfig::from_env`].
pub fn config() -> GcConfig {
    *CONFIG.get_or_init(GcConfig::from_env)
}

/// Overwrites logged before they are handed to the marker regardless of
/// whether a collection has come.
const SATB_FLUSH: usize = 4096;

/// How long a pause may spend marking, when there is no marking thread or the
/// one there is has fallen behind.
const ASSIST: Duration = Duration::from_micros(200);

/// How much a pause may spend evacuating sparse old blocks: a time, and slots
/// copied.
const EVACUATE: (Duration, usize) = (Duration::from_micros(25), 1 << 11);

/// Blocks looked at, per nursery collection, for ones to give back.
const RELEASE_BUDGET: usize = 64;

/// How many slots a fresh heap starts with. It doubles whenever a collection
/// leaves it more than half full.
const INITIAL: usize = 1 << 16;

/// How many a green thread's heap starts with. A thread may be one of very many,
/// so it starts small and grows like any other heap.
pub const THREAD_INITIAL: usize = 1 << 10;

// --- values that cannot go somewhere ------------------------------------------

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

/// Why a value cannot be passed to another thread -- see `meadow_core::thread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsendable {
    Ref,
    MutArray,
    Continuation,
}

impl Unsendable {
    pub fn describe(self) -> &'static str {
        match self {
            Unsendable::Ref => "a Ref",
            Unsendable::MutArray => "a mutable array",
            Unsendable::Continuation => "a continuation",
        }
    }
}

/// A value lifted out of one heap, to be put into another: the objects it
/// reaches, laid out as they will be, with addresses counted from the start
/// of the parcel. Holds nothing that belongs to a heap, so it can travel
/// between OS threads. What it reaches in shared regions stays where it is,
/// its addresses unchanged, and the parcel holds those regions alive.
#[derive(Clone)]
pub struct Parcel {
    slots: Vec<Slot>,
    root: Value,
    regions: Vec<Arc<Region>>,
}

impl std::fmt::Debug for Parcel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Parcel({} slots, {} regions)",
            self.slots.len(),
            self.regions.len()
        )
    }
}

impl Parcel {
    /// Slots it takes in the heap it is imported into.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// A region a heap has addresses into.
struct Adopted {
    region: Arc<Region>,
    /// How many of its blocks this heap has in [`Heap::blocks`]. A region only
    /// grows, and what an address reached when it arrived was in these.
    blocks: usize,
    marked: bool,
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
    /// A resumption's one-shot flag: `[taken]`. The resumption itself is a
    /// closure capturing it first -- see `meadow_seq`'s lowering -- so a
    /// resumption refused passage to another thread or into a region is refused
    /// as a continuation.
    Resume,
    /// A handle on a compact region: `meta` is the region, the one field is the
    /// value compacted into it. What keeps a region alive when the value itself
    /// is an immediate and points nowhere.
    Compact,
    /// A channel between green threads: `meta` is its number in the scheduler,
    /// and there are no fields. The channel itself lives outside every heap.
    Channel,
    /// A green thread, as `threadSpawn` answers it: `meta` is its number.
    Task,
    /// A `TVar`: `meta` is its number in the run's [`crate::stm::World`].
    TVar,
}

#[derive(Debug, Clone, Copy)]
pub enum Slot {
    Header {
        kind: Kind,
        len: u32,
        meta: u32,
    },
    Val(Value),
    /// Left behind in from-space when an object has been copied.
    Forward(Addr),
}

pub struct Heap {
    config: GcConfig,
    /// The nursery, and its other half for collecting into.
    space: Vec<Slot>,
    other: Vec<Slot>,
    top: usize,
    /// Nursery objects below this have survived a collection already.
    aged: usize,
    /// Whether survivors are promoted: once the nursery is as big as it gets.
    promoting: bool,
    old: Old,
    /// Old slots that may hold a nursery address, each once.
    remembered: Vec<Addr>,
    /// Old values overwritten while marking, not yet handed to the marker.
    satb: Vec<Addr>,
    /// Sparse old blocks being emptied, and the pointers into them.
    evac: Evacuation,
    marking: Option<Arc<mark::Job>>,
    /// Held to overwrite an old object's field while marking; see
    /// [`crate::mark`].
    mutation: Arc<Mutex<()>>,
    /// Nursery collections so far, and slots allocated in total — the two
    /// numbers worth knowing when a program is unexpectedly slow.
    pub collections: u64,
    pub allocated: u64,
    /// Slots copied within the nursery, and promoted out of it, in total.
    pub copied: u64,
    pub promoted: u64,
    /// Marking cycles begun, and time spent marking, on any thread.
    pub cycles: u64,
    pub mark_nanos: u64,
    /// Slots and blocks evacuated out of sparse old blocks.
    pub evacuated: u64,
    pub evacuated_blocks: u64,
    /// Time the program was stopped for the collector, in total and pause by
    /// pause.
    pub gc_nanos: u64,
    pub pauses: crate::pauses::Pauses,
    /// The blocks of every region this heap refers to, sorted by base address:
    /// where a region address is looked up.
    blocks: Vec<Arc<Block>>,
    /// Those regions, by id.
    regions: HashMap<u32, Adopted>,
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

impl Drop for Heap {
    fn drop(&mut self) {
        if let Some(job) = &self.marking {
            job.cancel();
        }
    }
}

impl Heap {
    pub fn new() -> Heap {
        Heap::with_capacity(INITIAL)
    }

    /// A heap of `slots` to begin with.
    pub fn with_capacity(slots: usize) -> Heap {
        Heap::with_config(slots, config())
    }

    pub fn with_config(slots: usize, config: GcConfig) -> Heap {
        let slots = match config.collector {
            Collector::Copying => slots,
            Collector::Generational => slots.min(config.nursery),
        };
        Heap {
            config,
            space: vec![Slot::Val(Value::Unit); slots.max(64)],
            other: Vec::new(),
            top: 0,
            aged: 0,
            promoting: false,
            old: Old::default(),
            remembered: Vec::new(),
            satb: Vec::new(),
            evac: Evacuation::default(),
            marking: None,
            mutation: Arc::new(Mutex::new(())),
            collections: 0,
            allocated: 0,
            copied: 0,
            promoted: 0,
            cycles: 0,
            mark_nanos: 0,
            evacuated: 0,
            evacuated_blocks: 0,
            gc_nanos: 0,
            pauses: Default::default(),
            blocks: Vec::new(),
            regions: HashMap::new(),
            region_growth: 0,
        }
    }

    /// Has enough gone into regions since the last collection that dead ones
    /// should be looked for, even though the nursery has room?
    pub fn wants_collection(&self) -> bool {
        self.region_growth > self.space.len()
    }

    /// Slots held by live regions, in total.
    pub fn region_slots(&self) -> usize {
        self.regions.values().map(|r| r.region.used()).sum()
    }

    /// Slots in use: the nursery's, and the old generation's as of the last
    /// cycle plus what has been allocated since.
    pub fn used(&self) -> usize {
        self.top + (self.old.live + self.old.allocated) as usize
    }

    /// Slots there are, in the nursery and the old generation.
    pub fn capacity(&self) -> usize {
        self.space.len() + self.old.capacity()
    }

    /// Slots in the old generation's blocks.
    pub fn old_slots(&self) -> usize {
        self.old.capacity()
    }

    /// Slots the last marking cycle found alive in the old generation.
    pub fn old_live(&self) -> usize {
        self.old.live as usize
    }

    /// Is there room for `slots` in the nursery? What decides whether an
    /// allocation has to collect first.
    pub fn room_for(&self, slots: usize) -> bool {
        self.top + slots <= self.space.len()
    }

    /// Make sure `slots` can be allocated without collecting.
    ///
    /// For what a collection cannot help with: one object larger than the
    /// whole nursery, which a program building a big array does routinely.
    /// The copying collector grows its one space; a generational heap grows its
    /// nursery as far as it may, and past that allocates in the old generation.
    pub fn reserve(&mut self, slots: usize) {
        if self.top + slots <= self.space.len() {
            return;
        }
        let limit = match self.config.collector {
            Collector::Copying => OLD_BASE as usize,
            Collector::Generational => self.config.nursery,
        };
        if self.top + slots > limit {
            match self.config.collector {
                Collector::Copying => panic!("the heap has used up its address space"),
                Collector::Generational => {
                    self.promoting = true;
                    return;
                }
            }
        }
        let mut size = self.space.len();
        while self.top + slots > size {
            size = (size * 2).max(64);
        }
        self.space.resize(size.min(limit), Slot::Val(Value::Unit));
    }

    /// Allocate. The caller must have checked [`Heap::room_for`], or
    /// [`Heap::reserve`]d — this is the bump, and it is the reason allocation is
    /// cheap enough not to think about.
    #[inline]
    pub fn alloc(&mut self, kind: Kind, meta: u32, fields: &[Value]) -> Addr {
        let at = self.top;
        let size = 1 + fields.len();
        if at + size > self.space.len() {
            return self.alloc_old(kind, meta, fields);
        }
        self.space[at] = Slot::Header {
            kind,
            len: fields.len() as u32,
            meta,
        };
        for (i, v) in fields.iter().enumerate() {
            self.space[at + 1 + i] = Slot::Val(*v);
        }
        self.top += size;
        self.allocated += size as u64;
        at as Addr
    }

    /// An object that does not fit in the nursery, in the old generation.
    #[cold]
    fn alloc_old(&mut self, kind: Kind, meta: u32, fields: &[Value]) -> Addr {
        let size = 1 + fields.len();
        if self.config.collector == Collector::Copying {
            self.reserve(size);
            return self.alloc(kind, meta, fields);
        }
        self.promoting = true;
        let at = self.old.alloc(size, self.marking.is_some());
        self.old.put(
            at,
            Slot::Header {
                kind,
                len: fields.len() as u32,
                meta,
            },
        );
        for (i, v) in fields.iter().enumerate() {
            self.init_old(at + 1 + i as Addr, *v);
        }
        self.allocated += size as u64;
        at
    }

    /// Write a field of an old object nothing else has seen yet: no marker can
    /// be reading it, and it held nothing to log.
    fn init_old(&mut self, s: Addr, v: Value) {
        self.old.put(s, Slot::Val(v));
        self.barriers(s, v);
    }

    /// Old slot `s` has just been given `v`: remember it if it points into the
    /// nursery, and record it if it points into a block chosen for evacuation.
    #[inline]
    fn barriers(&mut self, s: Addr, v: Value) {
        if let Value::Obj(x) = v {
            if x < OLD_BASE {
                if self.old.remember(s) {
                    self.remembered.push(s);
                }
            } else {
                self.evac.note(&self.old, s, x);
            }
        }
    }

    /// The slot at `a`, in the nursery, the old generation or a region.
    #[inline]
    fn slot(&self, a: Addr) -> Slot {
        if a < OLD_BASE {
            self.space[a as usize]
        } else if a < REGION_BASE {
            self.old.get(a)
        } else {
            self.blocks[Self::find_block(&self.blocks, a)].get(a)
        }
    }

    fn find_block(blocks: &[Arc<Block>], a: Addr) -> usize {
        let i = blocks.partition_point(|b| b.base <= a);
        debug_assert!(i > 0, "{a} is below every region block this heap knows");
        i - 1
    }

    fn head(&self, a: Addr) -> (Kind, u32, u32) {
        match self.slot(a) {
            Slot::Header { kind, len, meta } => (kind, len, meta),
            other => unreachable!("{a} is not an object header: {other:?}"),
        }
    }

    /// Is there an object header at `a`? For a debugger reading an address it
    /// cannot vouch for; the machine itself never needs to ask.
    pub fn is_object(&self, a: Addr) -> bool {
        if a < OLD_BASE {
            return (a as usize) < self.top
                && matches!(self.space[a as usize], Slot::Header { .. });
        }
        if a < REGION_BASE {
            return self.old.is_object(a);
        }
        let i = self.blocks.partition_point(|b| b.base <= a);
        if i == 0 {
            return false;
        }
        let b = &self.blocks[i - 1];
        (a - b.base) < b.cap as Addr && matches!(b.get(a), Slot::Header { .. })
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

    pub fn field(&self, a: Addr, i: usize) -> Value {
        match self.slot(a + 1 + i as Addr) {
            Slot::Val(v) => v,
            other => unreachable!("field {i} of {a} is not a value: {other:?}"),
        }
    }

    /// Overwrite a field. Only a `Ref`, a mutable array or a resumption is
    /// ever written after it is made, and this is where the barriers are: an
    /// old object's field that now points into the nursery is remembered, and
    /// while marking, the value it held is logged.
    pub fn set_field(&mut self, a: Addr, i: usize, v: Value) {
        let s = a + 1 + i as Addr;
        if a < OLD_BASE {
            self.space[s as usize] = Slot::Val(v);
            return;
        }
        debug_assert!(a < REGION_BASE, "a region is immutable");
        if self.marking.is_some() {
            let _held = lock(&self.mutation);
            if let Slot::Val(Value::Obj(x)) = self.old.get(s)
                && x >= OLD_BASE
            {
                self.satb.push(x);
            }
            self.old.put(s, Slot::Val(v));
        } else {
            self.old.put(s, Slot::Val(v));
        }
        self.barriers(s, v);
        if self.satb.len() >= SATB_FLUSH {
            self.flush_satb();
        }
    }

    /// Every field, as a fresh `Vec` — for the places that want to read an
    /// object and then allocate, which cannot hold heap references across the
    /// allocation.
    pub fn fields(&self, a: Addr) -> Vec<Value> {
        (0..self.len(a)).map(|i| self.field(a, i)).collect()
    }

    // --- collecting -----------------------------------------------------------------

    /// Collect, rewriting `roots` in place.
    ///
    /// The caller passes every address it can still reach — registers, the
    /// handler stack — and reads them back changed. Anything not in `roots` is
    /// garbage by definition, including addresses sitting in Rust locals, which
    /// is why the VM allocates only at points where it holds none.
    ///
    /// This is a nursery collection, and whatever the old generation's marking
    /// needs from a pause: to begin, to be helped along, or to finish.
    pub fn collect(&mut self, roots: &mut [Value]) {
        let started = Instant::now();
        self.collections += 1;
        let marked = self.marking.is_some();
        if marked {
            self.advance(roots);
        }
        // Finishing a cycle has a pause's work already; moving waits a pause.
        let finished = marked && self.marking.is_none();
        let begin = self.marking.is_none() && !self.old.is_empty() && self.wants_cycle();
        self.minor(roots, begin);
        if begin {
            self.begin_cycle(roots);
        } else if self.marking.is_none() && self.promoting {
            if !finished {
                self.evacuate(roots, Some(EVACUATE));
            }
            self.old.release(RELEASE_BUDGET);
        }
        if self.promoting && self.config.mark_threads > 0 {
            mark::tend_spares(self.config.mark_threads);
        }
        self.region_growth = 0;
        let took = started.elapsed().as_nanos() as u64;
        self.gc_nanos += took;
        self.pauses.record(took);
    }

    /// Collect everything now: finish the cycle in progress, if there is one,
    /// then run a whole cycle, marking on this thread. A long pause, for tests
    /// and for a program that has just let go of a great deal.
    pub fn collect_all(&mut self, roots: &mut [Value]) {
        while let Some(job) = self.marking.clone() {
            self.flush_satb();
            job.step(Duration::MAX, usize::MAX);
            if job.is_done() {
                self.finish_cycle(roots);
            }
        }
        self.minor(roots, !self.old.is_empty());
        if !self.old.is_empty() {
            self.begin_cycle(roots);
            let job = self.marking.clone().expect("a cycle just begun");
            while !job.is_done() {
                job.step(Duration::MAX, usize::MAX);
            }
            self.finish_cycle(roots);
        }
        self.evacuate(roots, None);
    }

    /// Move survivors out of sparse old blocks, within `budget` -- see
    /// [`crate::evacuate`]. Only between cycles, with the nursery collected.
    fn evacuate(&mut self, roots: &mut [Value], budget: Option<(Duration, usize)>) {
        if !self.config.evacuate || self.marking.is_some() || !self.evac.has_pending() {
            return;
        }
        self.evac.evacuate(
            &mut self.old,
            &mut self.space[..self.top],
            roots,
            &mut self.remembered,
            budget,
        );
        self.evacuated = self.evac.moved;
        self.evacuated_blocks = self.evac.blocks;
        if self.config.verify {
            self.verify_addresses(roots);
        }
    }

    /// Has the old generation grown enough since the last cycle to mark it
    /// again?
    fn wants_cycle(&self) -> bool {
        let live = self.old.live as usize;
        let growth = live.max(self.config.min_trigger.saturating_sub(live));
        self.old.allocated as usize >= growth || self.region_growth > self.space.len()
    }

    /// While marking: hand over the log, help if the marker is behind or there
    /// is none, and finish if it is done.
    fn advance(&mut self, roots: &mut [Value]) {
        let job = self.marking.clone().expect("marking");
        self.flush_satb();
        let live = self.old.live as usize;
        let trigger = (2 * live).max(self.config.min_trigger);
        let behind = self.old.allocated as usize > 2 * trigger;
        if self.config.mark_threads == 0 || behind {
            job.step(ASSIST, self.config.mark_slice);
        }
        if job.is_done() {
            self.finish_cycle(roots);
        }
    }

    fn flush_satb(&mut self) {
        if let Some(job) = &self.marking
            && !self.satb.is_empty()
        {
            job.push(&self.satb);
            self.satb.clear();
            if self.config.mark_threads > 0 {
                mark::submit(job, self.config.mark_threads);
            }
        }
    }

    /// The nursery is empty: start marking from `roots`.
    fn begin_cycle(&mut self, roots: &[Value]) {
        debug_assert_eq!(self.top, 0, "a cycle begins with the nursery empty");
        let epoch = self
            .old
            .epoch()
            .checked_add(1)
            .expect("marking cycles have run out of epochs");
        self.old.begin(epoch);
        self.evac.begin(&self.old);
        for r in self.regions.values_mut() {
            r.marked = false;
        }
        let job = mark::Job::new(
            epoch,
            self.old.blocks.clone(),
            self.blocks.clone(),
            self.mutation.clone(),
        );
        let mut grey = Vec::new();
        for r in roots {
            if let Value::Obj(x) = *r {
                if x >= REGION_BASE {
                    self.mark_region_at(x);
                } else {
                    debug_assert!(x >= OLD_BASE, "a root in the emptied nursery");
                    grey.push(x);
                }
            }
        }
        job.push(&grey);
        if self.config.mark_threads > 0 {
            mark::submit(&job, self.config.mark_threads);
        }
        self.marking = Some(job);
        self.cycles += 1;
    }

    /// Marking is done: adopt what it found.
    fn finish_cycle(&mut self, roots: &[Value]) {
        let job = self.marking.take().expect("marking");
        job.cancel();
        for id in job.found() {
            if let Some(r) = self.regions.get_mut(&id) {
                r.marked = true;
            }
        }
        if self.config.verify {
            self.verify(roots, job.epoch);
        }
        self.old.finish(job.marked());
        if self.config.evacuate {
            self.evac.finish(&self.old);
        }
        self.mark_nanos += job.nanos();

        let unreached: Vec<u32> = self
            .regions
            .iter()
            .filter(|(_, r)| !r.marked)
            .map(|(id, _)| *id)
            .collect();
        for id in unreached {
            self.free_region(id);
        }
        // A remembered field in a line the cycle freed belongs to a dead
        // object, and the line may be allocated into: forget it.
        let old = &self.old;
        self.remembered.retain(|s| {
            let keep = old.line_live(*s);
            if !keep {
                old.forget(*s);
            }
            keep
        });
    }

    /// Mark the region holding address `a`.
    fn mark_region_at(&mut self, a: Addr) {
        let id = self.blocks[Self::find_block(&self.blocks, a)].region;
        if let Some(r) = self.regions.get_mut(&id) {
            r.marked = true;
        }
    }

    /// Collect the nursery: copy what survives within it, or promote it, and
    /// with `promote_all` promote everything, leaving it empty.
    fn minor(&mut self, roots: &mut [Value], promote_all: bool) {
        let mut to = std::mem::take(&mut self.other);
        to.clear();
        to.resize(self.space.len(), Slot::Val(Value::Unit));
        // With nothing in the old generation, this collection sees everything,
        // and can tell which regions nothing reaches.
        let whole = self.old.is_empty() && self.marking.is_none();
        if whole {
            for r in self.regions.values_mut() {
                r.marked = false;
            }
        }
        let mut gc = Minor {
            from: &mut self.space,
            to: &mut to,
            top: 0,
            aged: self.aged,
            promote: self.promoting || promote_all,
            promote_all,
            black: self.marking.is_some(),
            old: &mut self.old,
            promoted: Vec::new(),
            remembered: Vec::new(),
            evac: &mut self.evac,
            blocks: &self.blocks,
            regions: &mut self.regions,
            whole,
            copied: 0,
            promoted_slots: 0,
        };

        for r in roots.iter_mut() {
            if let Value::Obj(a) = *r {
                *r = Value::Obj(gc.forward(a));
            }
        }

        {
            // A remembered field may be in an object the marker is reading.
            let _held = gc.black.then(|| lock(&self.mutation));
            for s in std::mem::take(&mut self.remembered) {
                match gc.old.get(s) {
                    Slot::Val(Value::Obj(x)) if x < OLD_BASE => {
                        let n = gc.forward(x);
                        gc.old.put(s, Slot::Val(Value::Obj(n)));
                        if n < OLD_BASE {
                            gc.remembered.push(s);
                        } else {
                            gc.old.forget(s);
                        }
                    }
                    _ => gc.old.forget(s),
                }
            }
        }

        // To-space is its own queue; promoted objects wait on a stack.
        let mut scan = 0usize;
        loop {
            if scan < gc.top {
                let len = match gc.to[scan] {
                    Slot::Header { len, .. } => len as usize,
                    other => unreachable!("scan is not at a header: {other:?}"),
                };
                for i in 0..len {
                    if let Slot::Val(Value::Obj(a)) = gc.to[scan + 1 + i] {
                        let n = gc.forward(a);
                        gc.to[scan + 1 + i] = Slot::Val(Value::Obj(n));
                    }
                }
                scan += 1 + len;
            } else if let Some(p) = gc.promoted.pop() {
                let len = match gc.old.get(p) {
                    Slot::Header { len, .. } => len as Addr,
                    other => unreachable!("promoted {p} is not a header: {other:?}"),
                };
                for s in p + 1..p + 1 + len {
                    if let Slot::Val(Value::Obj(a)) = gc.old.get(s) {
                        let n = gc.forward(a);
                        if a < OLD_BASE {
                            gc.old.put(s, Slot::Val(Value::Obj(n)));
                        }
                        if n < OLD_BASE {
                            if gc.old.remember(s) {
                                gc.remembered.push(s);
                            }
                        } else {
                            gc.evac.note(gc.old, s, n);
                        }
                    }
                }
            } else {
                break;
            }
        }

        let top = gc.top;
        let (copied, promoted) = (gc.copied, gc.promoted_slots);
        self.remembered = gc.remembered;
        self.other = std::mem::replace(&mut self.space, to);
        self.top = top;
        self.aged = top;
        self.copied += copied as u64;
        self.promoted += promoted as u64;

        if whole {
            // A region nothing reached is no longer this heap's business. It
            // is freed, all at once, when nothing else refers to it either.
            let unreached: Vec<u32> = self
                .regions
                .iter()
                .filter(|(_, r)| !r.marked)
                .map(|(id, _)| *id)
                .collect();
            for id in unreached {
                self.free_region(id);
            }
        }

        // More than half full after collecting means the next collection would
        // come almost immediately. Grow instead, so collection stays amortised
        // -- or, at the nursery's largest, start promoting.
        if self.top * 2 > self.space.len() {
            let bigger = (self.space.len() * 2).max(64);
            match self.config.collector {
                Collector::Copying => {
                    assert!(
                        bigger <= OLD_BASE as usize,
                        "the heap has used up its address space"
                    );
                    self.space.resize(bigger, Slot::Val(Value::Unit));
                }
                Collector::Generational if bigger <= self.config.nursery => {
                    self.space.resize(bigger, Slot::Val(Value::Unit));
                }
                Collector::Generational => self.promoting = true,
            }
        }
    }

    /// Check that every address reachable from `roots` is an object where it
    /// should be: what moving objects has to leave true.
    fn verify_addresses(&self, roots: &[Value]) {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<Addr> = roots.iter().filter_map(|r| r.addr()).collect();
        while let Some(a) = stack.pop() {
            if a >= REGION_BASE || !seen.insert(a) {
                continue;
            }
            assert!(
                self.is_object(a),
                "{a} is reachable but is not an object: a pointer was not moved"
            );
            for i in 0..self.len(a) {
                if let Value::Obj(x) = self.field(a, i) {
                    stack.push(x);
                }
            }
        }
    }

    /// Check that every old object reachable from `roots` is marked in `epoch`
    /// with its lines stamped, and every region reachable is marked: what a
    /// cycle has to have done before its result is adopted.
    fn verify(&self, roots: &[Value], epoch: u32) {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<Addr> = roots.iter().filter_map(|r| r.addr()).collect();
        while let Some(a) = stack.pop() {
            if a >= REGION_BASE {
                let id = self.blocks[Self::find_block(&self.blocks, a)].region;
                assert!(
                    self.regions.get(&id).is_some_and(|r| r.marked),
                    "region {id} is reachable but was not marked"
                );
                continue;
            }
            if !seen.insert(a) {
                continue;
            }
            let (kind, len, meta) = self.head(a);
            if a >= OLD_BASE {
                assert!(
                    self.old.is_marked(a, epoch),
                    "old object {a} ({kind:?}) is reachable but was not marked"
                );
                for s in a..a + 1 + len {
                    let b = &self.old.blocks[old::block_of(s)];
                    assert_eq!(
                        b.line(old::offset_of(s) / old::LINE),
                        epoch,
                        "a line of old object {a} was not stamped"
                    );
                }
            }
            if kind == Kind::Compact {
                assert!(
                    self.regions.get(&meta).is_some_and(|r| r.marked),
                    "compact handle {a}'s region {meta} was not marked"
                );
            }
            for i in 0..len as usize {
                if let Value::Obj(x) = self.field(a, i) {
                    stack.push(x);
                }
            }
        }
    }

    // --- passing values between heaps ----------------------------------------

    /// Lift `v` and everything it reaches out of this heap. The heap is only
    /// read. Sharing inside `v` survives. What is in a shared region is not
    /// copied: the parcel keeps the address, and holds the region.
    pub fn export(&self, v: Value) -> Result<Parcel, Unsendable> {
        let mut parcel = Parcel {
            slots: Vec::new(),
            root: v,
            regions: Vec::new(),
        };
        let Value::Obj(a) = v else {
            return Ok(parcel);
        };
        let mut copies: HashMap<Addr, Addr> = HashMap::new();
        parcel.root = Value::Obj(self.export_object(a, &mut parcel, &mut copies)?);
        // The parcel is its own queue, as to-space is for the collector.
        let mut scan = 0usize;
        while scan < parcel.slots.len() {
            let len = match parcel.slots[scan] {
                Slot::Header { len, .. } => len as usize,
                other => unreachable!("parcel scan is not at a header: {other:?}"),
            };
            for f in 0..len {
                if let Slot::Val(Value::Obj(x)) = parcel.slots[scan + 1 + f] {
                    let to = self.export_object(x, &mut parcel, &mut copies)?;
                    parcel.slots[scan + 1 + f] = Slot::Val(Value::Obj(to));
                }
            }
            scan += 1 + len;
        }
        Ok(parcel)
    }

    fn export_object(
        &self,
        a: Addr,
        parcel: &mut Parcel,
        copies: &mut HashMap<Addr, Addr>,
    ) -> Result<Addr, Unsendable> {
        if a >= REGION_BASE {
            let id = self.blocks[Self::find_block(&self.blocks, a)].region;
            self.carry(parcel, id);
            return Ok(a);
        }
        if let Some(&c) = copies.get(&a) {
            return Ok(c);
        }
        let (kind, len, meta) = self.head(a);
        match kind {
            Kind::Ref => return Err(Unsendable::Ref),
            Kind::MutArray => return Err(Unsendable::MutArray),
            Kind::Resume => return Err(Unsendable::Continuation),
            Kind::Compact => self.carry(parcel, meta),
            _ => {}
        }
        let at = parcel.slots.len() as Addr;
        parcel.slots.push(Slot::Header { kind, len, meta });
        for i in 0..len as usize {
            parcel.slots.push(Slot::Val(self.field(a, i)));
        }
        copies.insert(a, at);
        Ok(at)
    }

    /// Make the parcel hold region `id`, once.
    fn carry(&self, parcel: &mut Parcel, id: u32) {
        if parcel.regions.iter().any(|r| r.id == id) {
            return;
        }
        if let Some(r) = self.regions.get(&id) {
            parcel.regions.push(r.region.clone());
        }
    }

    /// Rebuild a parcel in this heap. The caller must have [`Heap::reserve`]d
    /// [`Parcel::len`] slots, as for any allocation.
    pub fn import(&mut self, p: &Parcel) -> Value {
        for r in &p.regions {
            self.adopt(r);
        }
        if !self.room_for(p.slots.len()) {
            return self.import_scattered(p);
        }
        let base = self.top as Addr;
        let rebase = |x: Addr| if x < REGION_BASE { x + base } else { x };
        for (i, s) in p.slots.iter().enumerate() {
            self.space[self.top + i] = match *s {
                Slot::Val(Value::Obj(x)) => Slot::Val(Value::Obj(rebase(x))),
                other => other,
            };
        }
        self.top += p.slots.len();
        self.allocated += p.slots.len() as u64;
        match p.root {
            Value::Obj(x) => Value::Obj(rebase(x)),
            v => v,
        }
    }

    /// A parcel too big for the nursery, rebuilt an object at a time wherever
    /// each fits: made first, then its fields filled in with where the
    /// objects they point at went.
    fn import_scattered(&mut self, p: &Parcel) -> Value {
        let mut at = vec![0 as Addr; p.slots.len()];
        let mut i = 0;
        while i < p.slots.len() {
            let Slot::Header { kind, len, meta } = p.slots[i] else {
                unreachable!("parcel walk is not at a header");
            };
            at[i] = self.alloc(kind, meta, &vec![Value::Unit; len as usize]);
            i += 1 + len as usize;
        }
        let rebase = |v: Value| match v {
            Value::Obj(x) if x < REGION_BASE => Value::Obj(at[x as usize]),
            v => v,
        };
        let mut i = 0;
        while i < p.slots.len() {
            let Slot::Header { len, .. } = p.slots[i] else {
                unreachable!("parcel walk is not at a header");
            };
            for f in 0..len as usize {
                let Slot::Val(v) = p.slots[i + 1 + f] else {
                    unreachable!("a parcel field is not a value");
                };
                let s = at[i] + 1 + f as Addr;
                let v = rebase(v);
                if s < OLD_BASE {
                    self.space[s as usize] = Slot::Val(v);
                } else {
                    self.init_old(s, v);
                }
            }
            i += 1 + len as usize;
        }
        rebase(p.root)
    }

    // --- compact regions ----------------------------------------------------

    /// Refer to `region`: from now on its addresses can be read here. Also how
    /// a heap learns of blocks a region grew since it last looked.
    pub fn adopt(&mut self, region: &Arc<Region>) {
        let contents = region.lock();
        let known = self.regions.get(&region.id).map_or(0, |r| r.blocks);
        for b in &contents.blocks[known.min(contents.blocks.len())..] {
            let at = self.blocks.partition_point(|x| x.base < b.base);
            self.blocks.insert(at, b.clone());
        }
        let blocks = contents.blocks.len().max(known);
        drop(contents);
        // A region adopted while a cycle marks was not in its snapshot, so
        // the cycle cannot tell whether it is reachable: it is kept.
        let marking = self.marking.is_some();
        self.regions
            .entry(region.id)
            .and_modify(|r| {
                r.blocks = blocks;
                r.marked |= marking;
            })
            .or_insert(Adopted {
                region: region.clone(),
                blocks,
                marked: marking,
            });
    }

    /// A new, empty region, referred to by this heap.
    pub fn new_region(&mut self) -> u32 {
        let region = Region::new();
        self.adopt(&region);
        region.id
    }

    /// The region `id`, if this heap refers to it.
    pub fn region(&self, id: u32) -> Option<&Arc<Region>> {
        self.regions.get(&id).map(|r| &r.region)
    }

    /// Slots in use by region `id`.
    pub fn region_used(&self, id: u32) -> usize {
        self.regions.get(&id).map_or(0, |r| r.region.used())
    }

    /// Stop referring to region `id`.
    pub fn free_region(&mut self, id: u32) {
        if self.regions.remove(&id).is_some() {
            self.blocks.retain(|b| b.region != id);
        }
    }

    /// Copy `v`, and everything it reaches, into region `id`, and answer the
    /// copy. What is already in `id` is shared rather than copied again, and
    /// sharing inside `v` survives: each object is copied once. What is in
    /// another region is copied too, so a region never points outside itself.
    ///
    /// Iterative, as the collector is: the objects just appended to the region
    /// are the queue. If `v` reaches something that cannot be compacted, the
    /// region is put back exactly as it was.
    pub fn compact_into(&mut self, id: u32, v: Value) -> Result<Value, Uncompactable> {
        let region = self
            .region(id)
            .expect("a region this heap refers to")
            .clone();
        let mut contents = region.lock();
        let end = contents.end();
        let before = contents.used;
        let result = self.copy_into(&mut contents, id, v, end);
        if result.is_err() {
            contents.truncate(end);
        }
        self.region_growth += contents.used - before;
        drop(contents);
        self.adopt(&region);
        result
    }

    fn copy_into(
        &self,
        contents: &mut crate::region::Contents,
        id: u32,
        v: Value,
        end: crate::region::End,
    ) -> Result<Value, Uncompactable> {
        let mut copies: HashMap<Addr, Addr> = HashMap::new();
        let root = match v {
            Value::Obj(a) => Value::Obj(self.copy_object(contents, id, a, &mut copies)?),
            other => other,
        };
        // Walk what was appended, fixing each field to the copy of what it
        // points at. Copies made on the way are appended, and walked in turn.
        let mut cursor = contents.cursor(end);
        while let Some((at, len)) = contents.next(&mut cursor) {
            let block = contents.block_of(at).expect("an appended object").clone();
            for f in 0..len {
                if let Slot::Val(Value::Obj(a)) = block.get(at + 1 + f as Addr) {
                    let to = self.copy_object(contents, id, a, &mut copies)?;
                    contents.set_field(at, f, Value::Obj(to));
                }
            }
        }
        Ok(root)
    }

    /// The copy of object `a` in region `id`, making it if there is none yet.
    /// Its fields are copied as they are, to be fixed by the walk.
    fn copy_object(
        &self,
        contents: &mut crate::region::Contents,
        id: u32,
        a: Addr,
        copies: &mut HashMap<Addr, Addr>,
    ) -> Result<Addr, Uncompactable> {
        if a >= REGION_BASE {
            // In this region already, published or appended just now.
            if contents.block_of(a).is_some() {
                return Ok(a);
            }
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
        let c = contents.alloc(kind, meta, &fields);
        copies.insert(a, c);
        Ok(c)
    }
}

/// A nursery collection in progress.
struct Minor<'h> {
    from: &'h mut Vec<Slot>,
    to: &'h mut Vec<Slot>,
    top: usize,
    /// Nursery objects below this are old enough to promote.
    aged: usize,
    promote: bool,
    promote_all: bool,
    /// Whether a cycle is marking, so what is promoted is marked as it is.
    black: bool,
    old: &'h mut Old,
    /// Promoted objects whose fields are still to be forwarded.
    promoted: Vec<Addr>,
    /// The remembered set as it will be after.
    remembered: Vec<Addr>,
    /// For recording promoted fields that point into blocks to be evacuated.
    evac: &'h mut Evacuation,
    blocks: &'h [Arc<Block>],
    regions: &'h mut HashMap<u32, Adopted>,
    /// Whether this collection sees everything, and so marks regions.
    whole: bool,
    copied: usize,
    promoted_slots: usize,
}

impl Minor<'_> {
    /// Where nursery object `a` is now: copied within the nursery, or promoted,
    /// the first time it is asked for, and the same place every time after.
    /// An old address stays put; a region address stays put and, if this
    /// collection sees everything, marks its region.
    fn forward(&mut self, a: Addr) -> Addr {
        if a >= REGION_BASE {
            if self.whole {
                let region = self.blocks[Heap::find_block(self.blocks, a)].region;
                self.mark(region);
            }
            return a;
        }
        if a >= OLD_BASE {
            return a;
        }
        match self.from[a as usize] {
            // Already moved: every other reference to it lands here and shares.
            Slot::Forward(n) => n,
            Slot::Header { kind, len, meta } => {
                if self.whole && kind == Kind::Compact {
                    self.mark(meta);
                }
                let size = 1 + len as usize;
                let promote = self.promote
                    && (self.promote_all
                        || (a as usize) < self.aged
                        || self.top + size > self.to.len());
                let at = if promote {
                    let at = self.old.alloc(size, self.black);
                    self.old.put(at, Slot::Header { kind, len, meta });
                    for i in 0..len as usize {
                        self.old
                            .put(at + 1 + i as Addr, self.from[a as usize + 1 + i]);
                    }
                    self.promoted.push(at);
                    self.promoted_slots += size;
                    at
                } else {
                    let at = self.top;
                    self.to[at] = Slot::Header { kind, len, meta };
                    for i in 0..len as usize {
                        self.to[at + 1 + i] = self.from[a as usize + 1 + i];
                    }
                    self.top += size;
                    self.copied += size;
                    at as Addr
                };
                self.from[a as usize] = Slot::Forward(at);
                at
            }
            other => unreachable!("{a} is not an object header: {other:?}"),
        }
    }

    fn mark(&mut self, region: u32) {
        if let Some(r) = self.regions.get_mut(&region) {
            r.marked = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The copying collector: one space, and every collection sees everything,
    /// which is what the tests of it below count on.
    fn heap() -> Heap {
        Heap::with_config(
            INITIAL,
            GcConfig {
                collector: Collector::Copying,
                ..config()
            },
        )
    }

    #[test]
    fn allocation_is_a_bump_and_reading_back_works() {
        let mut h = heap();
        let a = h.alloc(Kind::Data, 7, &[Value::Int(1), Value::Int(2)]);
        assert_eq!(h.kind(a), Kind::Data);
        assert_eq!(h.meta(a), 7);
        assert_eq!(h.len(a), 2);
        assert_eq!(h.field(a, 1), Value::Int(2));
        assert_eq!(h.used(), 3);
    }

    #[test]
    fn collecting_keeps_what_is_rooted_and_drops_the_rest() {
        let mut h = heap();
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
        let mut h = heap();
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
        assert_eq!(
            h.field(cyc, 0),
            Value::Obj(cyc),
            "the cycle came back intact"
        );
    }

    #[test]
    fn a_long_chain_collects_without_recursing() {
        // 200_000 deep. Cheney scans the copied region as a queue, so this costs
        // no stack at all — which is exactly what the `Rc` version could not do
        // when it came to dropping one of these.
        let mut h = heap();
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
        let mut h = heap();
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
        let mut h = heap();
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
        let mut h = heap();
        let big = chain(&mut h, 1000);
        let region = h.new_region();
        h.compact_into(region, big).unwrap();
        assert!(h.region_slots() > 0);
        h.collect(&mut []);
        assert_eq!(h.region_slots(), 0);
        assert!(h.blocks.is_empty(), "its blocks went with it");
        // Nothing else refers to it, so it is gone: its addresses are free for
        // the next region.
        assert!(h.region(region).is_none());
    }

    #[test]
    fn sharing_inside_a_compacted_value_survives() {
        let mut h = heap();
        let shared = h.alloc(Kind::Data, 1, &[Value::Int(7)]);
        let pair = h.alloc(Kind::Data, 2, &[Value::Obj(shared), Value::Obj(shared)]);
        let region = h.new_region();
        let root = h
            .compact_into(region, Value::Obj(pair))
            .unwrap()
            .addr()
            .unwrap();
        assert_eq!(h.field(root, 0), h.field(root, 1), "one copy, not two");
        assert_eq!(h.region_used(region), 3 + 2);
    }

    #[test]
    fn adding_to_a_region_shares_what_is_already_there() {
        let mut h = heap();
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
        let mut h = heap();
        let ok = chain(&mut h, 10);
        let region = h.new_region();
        h.compact_into(region, ok).unwrap();
        let before = h.region_used(region);

        let cell = h.alloc(Kind::Ref, 0, &[Value::Int(1)]);
        let bad = h.alloc(Kind::Data, 1, &[Value::Int(0), Value::Obj(cell)]);
        assert_eq!(
            h.compact_into(region, Value::Obj(bad)),
            Err(Uncompactable::Ref)
        );
        assert_eq!(h.region_used(region), before);
        let f = h.alloc(Kind::Closure, 0, &[]);
        assert_eq!(
            h.compact_into(region, Value::Obj(f)),
            Err(Uncompactable::Function)
        );
    }

    #[test]
    fn a_region_spans_blocks_and_scans_across_them() {
        // Bigger than the first block, so the copy has to continue its scan in
        // the next one.
        let mut h = heap();
        let big = chain(&mut h, crate::region::FIRST_BLOCK as i64);
        let region = h.new_region();
        let root = h.compact_into(region, big).unwrap();
        assert!(h.region(region).unwrap().lock().blocks.len() > 1);
        assert_eq!(chain_len(&h, root), crate::region::FIRST_BLOCK);
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
    fn a_region_is_shared_between_heaps_without_copying() {
        let mut a = heap();
        let big = chain(&mut a, 10_000);
        let region = a.new_region();
        let root = a.compact_into(region, big).unwrap();
        let handle = a.alloc(Kind::Compact, region, &[root]);

        // Across to another heap: the handle is copied, the chain is not.
        let parcel = a.export(Value::Obj(handle)).unwrap();
        assert_eq!(parcel.len(), 2, "one handle object; the region stays put");
        let mut b = heap();
        b.reserve(parcel.len());
        let got = b.import(&parcel);
        drop(parcel);
        let inner = b.field(got.addr().unwrap(), 0);
        assert_eq!(inner, root, "the same address, in both heaps");

        // The heap that made it lets go; the other still reads it.
        let weak = std::sync::Arc::downgrade(a.region(region).unwrap());
        a.collect(&mut []);
        assert!(a.region(region).is_none());
        assert_eq!(chain_len(&b, inner), 10_000);

        // And when the last heap lets go, the region is freed.
        let mut roots = [got];
        b.collect(&mut roots);
        assert!(weak.upgrade().is_some(), "still reachable from b");
        b.collect(&mut []);
        assert!(weak.upgrade().is_none(), "nobody refers to it any more");
    }

    #[test]
    fn a_region_can_grow_while_another_heap_reads_it() {
        let mut a = heap();
        let first = chain(&mut a, 100);
        let region = a.new_region();
        let root = a.compact_into(region, first).unwrap();
        let mut b = heap();
        b.adopt(a.region(region).unwrap());

        // `a` appends far past the first block; `b` keeps reading what it had.
        let more = chain(&mut a, (crate::region::FIRST_BLOCK * 3) as i64);
        let root2 = a.compact_into(region, more).unwrap();
        assert_eq!(chain_len(&b, root), 100);
        // `b` learns of the new blocks when it adopts the region again.
        b.adopt(a.region(region).unwrap());
        assert_eq!(chain_len(&b, root2), crate::region::FIRST_BLOCK * 3);
    }

    #[test]
    fn the_heap_grows_rather_than_collecting_forever() {
        let mut h = heap();
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

    // --- the generational collector -------------------------------------------

    /// A generational heap with a nursery of `nursery` slots, marking on its
    /// own thread at most `slice` fields per pause, and checking every cycle.
    fn generational(nursery: usize, slice: usize, threads: usize) -> Heap {
        Heap::with_config(
            nursery,
            GcConfig {
                collector: Collector::Generational,
                nursery,
                mark_threads: threads,
                min_trigger: 1024,
                verify: true,
                mark_slice: slice,
                evacuate: true,
            },
        )
    }

    /// Allocate as the VM does: collect first if there is no room, with every
    /// live value in `roots`, and read what to allocate from there after.
    fn alloc_rooted(
        h: &mut Heap,
        roots: &mut [Value],
        kind: Kind,
        meta: u32,
        fields: &[usize],
    ) -> Value {
        if !h.room_for(1 + fields.len()) {
            h.collect(roots);
            h.reserve(1 + fields.len());
        }
        let fields: Vec<Value> = fields.iter().map(|i| roots[*i]).collect();
        Value::Obj(h.alloc(kind, meta, &fields))
    }

    /// A chain of `n` cells counting down to 0, built with its head rooted.
    fn rooted_chain(h: &mut Heap, roots: &mut Vec<Value>, n: i64) -> Value {
        roots.push(Value::Unit);
        let head = roots.len() - 1;
        roots[head] = alloc_rooted(h, roots, Kind::Data, 0, &[]);
        for i in 0..n {
            roots.push(Value::Int(i));
            let k = roots.len() - 1;
            let cell = alloc_rooted(h, roots, Kind::Data, 1, &[k, head]);
            roots.pop();
            roots[head] = cell;
        }
        roots.pop().expect("the head")
    }

    /// Is `v` a whole chain, counting down from `n - 1`?
    fn well_formed(h: &Heap, mut v: Value, n: i64) -> bool {
        let mut expect = n - 1;
        while let Value::Obj(a) = v {
            if h.len(a) == 0 {
                return expect == -1;
            }
            if h.field(a, 0) != Value::Int(expect) {
                return false;
            }
            expect -= 1;
            v = h.field(a, 1);
        }
        false
    }

    fn churn(h: &mut Heap, roots: &mut [Value], cells: usize) {
        for i in 0..cells {
            if !h.room_for(3) {
                h.collect(roots);
            }
            h.alloc(Kind::Data, 1, &[Value::Int(i as i64), Value::Unit]);
        }
    }

    #[test]
    fn what_survives_twice_is_promoted_and_stays_whole() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = Vec::new();
        let c = rooted_chain(&mut h, &mut roots, 2000);
        roots.push(c);
        for _ in 0..3 {
            h.collect(&mut roots);
        }
        assert!(roots[0].addr().unwrap() >= OLD_BASE, "promoted");
        assert!(h.promoted > 0);
        assert_eq!(h.used() - h.top, (h.old.live + h.old.allocated) as usize);
        assert!(well_formed(&h, roots[0], 2000));
    }

    #[test]
    fn a_young_object_only_an_old_ref_holds_survives() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = vec![Value::Unit];
        roots[0] = alloc_rooted(&mut h, &mut roots, Kind::Ref, 0, &[0]);
        h.promoting = true;
        h.collect(&mut roots);
        h.collect(&mut roots);
        let r = roots[0].addr().unwrap();
        assert!(r >= OLD_BASE);
        let young = h.alloc(Kind::Data, 5, &[Value::Int(7)]);
        h.set_field(r, 0, Value::Obj(young));
        assert_eq!(h.remembered.len(), 1);
        churn(&mut h, &mut roots, 20_000);
        let v = h.field(roots[0].addr().unwrap(), 0).addr().unwrap();
        assert_eq!((h.meta(v), h.field(v, 0)), (5, Value::Int(7)));
        assert!(v >= OLD_BASE, "promoted in time");
        assert!(h.remembered.is_empty(), "nothing old points young any more");
    }

    #[test]
    fn a_cycle_frees_what_died_and_keeps_what_did_not() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = Vec::new();
        let big = rooted_chain(&mut h, &mut roots, 50_000);
        roots.push(big);
        h.collect_all(&mut roots);
        let alive = h.old.live;
        assert!(alive >= 150_000);
        // Let it go, keep a small one.
        roots.clear();
        let small = rooted_chain(&mut h, &mut roots, 100);
        roots.push(small);
        h.collect_all(&mut roots);
        assert!(
            h.old.live < alive / 100,
            "{} of {alive} still counted",
            h.old.live
        );
        assert!(well_formed(&h, roots[0], 100));
        // Its lines are allocated into again rather than new blocks made.
        let blocks = h.old.real_blocks;
        let again = rooted_chain(&mut h, &mut roots, 20_000);
        roots.push(again);
        assert!(
            h.old.real_blocks <= blocks,
            "{} blocks, from {blocks}",
            h.old.real_blocks
        );
        assert!(well_formed(&h, roots[1], 20_000));
    }

    #[test]
    fn empty_blocks_are_given_back() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = Vec::new();
        let big = rooted_chain(&mut h, &mut roots, 200_000);
        roots.push(big);
        h.collect_all(&mut roots);
        let before = h.old.real_blocks;
        roots.clear();
        h.collect_all(&mut roots);
        churn(&mut h, &mut roots, 200_000);
        assert!(
            h.old.real_blocks < before / 4,
            "{} of {before} blocks kept",
            h.old.real_blocks
        );
    }

    /// Refs rewired at random while a cycle marks a few fields per pause: the
    /// marker keeps losing paths to overwrites, and has to find them in the
    /// log. `verify` checks every cycle, and the chains are checked whole.
    fn rewire(h: &mut Heap, steps: usize, refs: usize, longest: u64) {
        let mut roots = vec![Value::Unit; refs];
        for i in 0..refs {
            roots[i] = alloc_rooted(h, &mut roots, Kind::Ref, 0, &[refs - 1]);
            h.set_field(roots[i].addr().unwrap(), 0, Value::Unit);
        }
        // What each ref holds: the length of its chain, or none.
        let mut lens: Vec<Option<i64>> = vec![None; refs];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..steps {
            let a = (next() % refs as u64) as usize;
            let b = (next() % refs as u64) as usize;
            match next() % 4 {
                0 => {
                    let n = (next() % longest) as i64;
                    let c = rooted_chain(h, &mut roots, n);
                    h.set_field(roots[a].addr().unwrap(), 0, c);
                    lens[a] = Some(n);
                }
                1 => {
                    let (ra, rb) = (roots[a].addr().unwrap(), roots[b].addr().unwrap());
                    let (va, vb) = (h.field(ra, 0), h.field(rb, 0));
                    h.set_field(ra, 0, vb);
                    h.set_field(rb, 0, va);
                    lens.swap(a, b);
                }
                2 => {
                    // Held only by a register for a while.
                    let ra = roots[a].addr().unwrap();
                    roots.push(h.field(ra, 0));
                    h.set_field(ra, 0, Value::Unit);
                    churn(h, &mut roots, 64);
                    let v = roots.pop().unwrap();
                    h.set_field(roots[b].addr().unwrap(), 0, v);
                    lens[b] = lens[a].take();
                }
                _ => churn(h, &mut roots, 32),
            }
        }
        h.collect_all(&mut roots);
        for (i, len) in lens.iter().enumerate() {
            let v = h.field(roots[i].addr().unwrap(), 0);
            match len {
                Some(n) => assert!(well_formed(h, v, *n), "ref {i}'s chain of {n} is broken"),
                None => assert_eq!(v, Value::Unit),
            }
        }
        assert!(h.cycles > 3, "only {} cycles ran", h.cycles);
    }

    #[test]
    fn overwrites_while_marking_a_little_at_a_time_lose_nothing() {
        let mut h = generational(512, 16, 0);
        rewire(&mut h, 30_000, 48, 40);
    }

    #[test]
    fn overwrites_while_marking_concurrently_lose_nothing() {
        let mut h = generational(512, usize::MAX, 2);
        rewire(&mut h, 60_000, 48, 40);
    }

    #[test]
    fn a_region_only_an_old_object_reaches_lives_until_it_does_not() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = Vec::new();
        let c = rooted_chain(&mut h, &mut roots, 100);
        let region = h.new_region();
        roots.push(h.compact_into(region, c).unwrap());
        let holder = alloc_rooted(&mut h, &mut roots, Kind::Ref, 0, &[0]);
        roots[0] = holder;
        h.promoting = true;
        h.collect(&mut roots);
        h.collect(&mut roots);
        assert!(roots[0].addr().unwrap() >= OLD_BASE);
        h.collect_all(&mut roots);
        h.collect_all(&mut roots);
        assert!(h.region(region).is_some(), "still reachable");
        let inner = h.field(roots[0].addr().unwrap(), 0);
        assert_eq!(chain_len(&h, inner), 100);
        h.set_field(roots[0].addr().unwrap(), 0, Value::Unit);
        h.collect_all(&mut roots);
        assert!(h.region(region).is_none(), "let go once nothing reaches it");
    }

    #[test]
    fn a_parcel_bigger_than_the_nursery_is_rebuilt_in_pieces() {
        let mut a = heap();
        let big = chain(&mut a, 5000);
        let parcel = a.export(big).unwrap();
        let mut b = generational(256, usize::MAX, 0);
        b.reserve(parcel.len());
        let mut roots = vec![b.import(&parcel)];
        assert!(well_formed(&b, roots[0], 5000));
        churn(&mut b, &mut roots, 10_000);
        b.collect_all(&mut roots);
        assert!(well_formed(&b, roots[0], 5000));
    }

    #[test]
    fn an_array_bigger_than_a_block_of_young_objects() {
        let mut h = generational(1024, usize::MAX, 0);
        let n = old::BLOCK + 100;
        let mut roots = Vec::new();
        h.reserve(1 + n);
        let young = h.alloc(Kind::Data, 9, &[Value::Int(42)]);
        let fields = vec![Value::Obj(young); n];
        let arr = h.alloc(Kind::Array, 0, &fields);
        assert!(arr >= OLD_BASE, "too big for the nursery");
        roots.push(Value::Obj(arr));
        churn(&mut h, &mut roots, 50_000);
        h.collect_all(&mut roots);
        churn(&mut h, &mut roots, 50_000);
        let arr = roots[0].addr().unwrap();
        let first = h.field(arr, 0);
        assert_eq!(h.field(arr, n - 1), first, "one object, shared");
        assert_eq!(h.field(first.addr().unwrap(), 0), Value::Int(42));
    }

    // --- evacuation ---------------------------------------------------------------

    /// Eight chains built a cell of each at a time, so they lie interleaved in
    /// the old generation, and seven of them dropped: every block left an
    /// eighth full.
    fn sparse(h: &mut Heap, cells: i64) -> Vec<Value> {
        let mut roots = vec![Value::Unit; 8];
        for j in 0..8 {
            roots[j] = alloc_rooted(h, &mut roots, Kind::Data, 0, &[]);
        }
        for k in 0..cells {
            for j in 0..8 {
                roots.push(Value::Int(k));
                let at = roots.len() - 1;
                let cell = alloc_rooted(h, &mut roots, Kind::Data, 1, &[at, j]);
                roots.pop();
                roots[j] = cell;
            }
        }
        roots.truncate(1);
        roots
    }

    #[test]
    fn sparse_blocks_are_emptied_and_given_back() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = sparse(&mut h, 40_000);
        // The first cycle finds the blocks sparse and chooses them; the second
        // records every pointer into them, and then they move.
        h.collect_all(&mut roots);
        let before = h.old.real_blocks;
        // Each cycle moves up to an eighth of what is alive.
        for _ in 0..8 {
            h.collect_all(&mut roots);
            h.old.release(usize::MAX);
        }
        assert!(h.evacuated_blocks > 0, "nothing was evacuated");
        assert!(
            h.old.real_blocks * 2 < before,
            "{} blocks from {before}",
            h.old.real_blocks
        );
        assert!(well_formed(&h, roots[0], 40_000));
        // And it all still holds together through more cycles.
        churn(&mut h, &mut roots, 100_000);
        h.collect_all(&mut roots);
        assert!(well_formed(&h, roots[0], 40_000));
    }

    #[test]
    fn evacuation_a_little_per_pause_while_the_program_runs() {
        let mut h = generational(1024, usize::MAX, 0);
        let mut roots = sparse(&mut h, 40_000);
        h.collect_all(&mut roots);
        // The second cycle, and the moves after it, happen in ordinary pauses:
        // with pointers into the moving blocks from the nursery and a `Ref`.
        let r = alloc_rooted(&mut h, &mut roots, Kind::Ref, 0, &[0]);
        roots.push(r);
        let cycles = h.cycles;
        let mut pauses = 0;
        while h.evacuated_blocks == 0 || h.evac.has_pending() {
            roots.push(Value::Unit);
            let n = roots.len() - 1;
            roots[n] = alloc_rooted(&mut h, &mut roots, Kind::Data, 2, &[0]);
            let holder = roots.pop().unwrap();
            h.set_field(roots[1].addr().unwrap(), 0, holder);
            if h.cycles == cycles {
                // Ask for the second cycle, as enough promotion would.
                h.old.allocated += 1 << 20;
            }
            churn(&mut h, &mut roots, 2_000);
            pauses += 1;
            assert!(pauses < 10_000, "evacuation never finished");
        }
        assert!(h.cycles > cycles);
        assert!(h.evacuated_blocks > 1, "all in one pause");
        let held = h.field(roots[1].addr().unwrap(), 0).addr().unwrap();
        assert_eq!(h.field(held, 0), roots[0]);
        assert!(well_formed(&h, roots[0], 40_000));
    }

    #[test]
    fn rewiring_refs_while_blocks_move_loses_nothing() {
        let mut h = generational(1024, 64, 0);
        rewire(&mut h, 40_000, 512, 160);
        assert!(h.evacuated_blocks > 0, "nothing was evacuated");
    }

    #[test]
    fn rewiring_refs_while_blocks_move_concurrently_loses_nothing() {
        let mut h = generational(1024, usize::MAX, 2);
        rewire(&mut h, 60_000, 512, 160);
        assert!(h.evacuated_blocks > 0, "nothing was evacuated");
    }

    #[test]
    fn a_pointer_only_promotion_records_is_moved_too() {
        let mut h = generational(1024, 4096, 0);
        let mut roots = sparse(&mut h, 40_000);
        h.collect_all(&mut roots);
        // A cell deep in the chain, in a block chosen to move.
        let (mut cell, mut n) = (roots[0], 40_000);
        while !h.old.blocks[old::block_of(cell.addr().unwrap())].is_tracked() {
            cell = h.field(cell.addr().unwrap(), 1);
            n -= 1;
            assert!(n > 0, "no cell in a chosen block");
        }
        roots.push(cell);
        let r = alloc_rooted(&mut h, &mut roots, Kind::Ref, 0, &[0]);
        roots[1] = r;
        // The second cycle begins, and marks a little per pause.
        h.old.allocated += 1 << 20;
        while h.marking.is_none() {
            churn(&mut h, &mut roots, 1_000);
        }
        // Made now, the holder is not in the cycle's snapshot, so the marker
        // never reads it; once promoted, only an old `Ref` holds it, so no root
        // or nursery scan finds it either. Only promoting it records its field.
        roots.push(cell);
        let at = roots.len() - 1;
        let holder = alloc_rooted(&mut h, &mut roots, Kind::Data, 7, &[at]);
        roots.truncate(2);
        h.set_field(roots[1].addr().unwrap(), 0, holder);
        let mut pauses = 0;
        while h.evacuated_blocks == 0 || h.evac.has_pending() || h.marking.is_some() {
            churn(&mut h, &mut roots, 2_000);
            pauses += 1;
            assert!(pauses < 10_000, "evacuation never finished");
        }
        let holder = h.field(roots[1].addr().unwrap(), 0).addr().unwrap();
        assert!(holder >= OLD_BASE, "promoted");
        assert!(well_formed(&h, h.field(holder, 0), n));
        assert!(well_formed(&h, roots[0], 40_000));
    }
}
