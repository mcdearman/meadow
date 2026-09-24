//! Marking the old generation while the program keeps running.
//!
//! # The snapshot
//!
//! A cycle begins in a pause that empties the nursery into the old generation
//! and hands the roots to a [`Job`]. From then on the job marks everything that
//! was reachable *at that moment* -- snapshot-at-the-beginning -- on another OS
//! thread, while the program carries on.
//!
//! Everything reachable when the cycle ends was either reachable at its start
//! or allocated since, because a program cannot get hold of an object nothing
//! reaches. So two things make the snapshot enough:
//!
//! * **Allocation is black.** An object promoted or allocated in the old
//!   generation while a cycle marks is marked as it is made, and its lines
//!   stamped, so the marker never needs to look at it.
//! * **Overwrites are logged.** The marker could lose a path it has not walked
//!   yet if the program overwrote a field on it. Only `Ref`s, mutable arrays
//!   and a resumption's flag are ever overwritten, so only those writes pay: the
//!   old value goes into the heap's log, and the log to the job, which marks
//!   from it. That is the SATB barrier, and in a language whose data is
//!   immutable it is almost never run.
//!
//! # What the marker may read
//!
//! The marker only reads slots of objects that existed when the cycle began.
//! The program only writes old slots in three ways, and none of them races it:
//!
//! * **allocating**, into lines that were free when the cycle began -- which
//!   hold nothing the snapshot reaches;
//! * **overwriting** a field of a mutable object, which happens under the
//!   heap's mutation lock, and the marker holds that lock while it reads a
//!   mutable object's fields;
//! * **updating the remembered set** during a nursery collection, which only
//!   ever touches fields of mutable objects or of objects made since the cycle
//!   began -- it is empty when a cycle begins, since the nursery is -- and holds
//!   the same lock.
//!
//! Mark bits and line stamps are atomics, and a block the program adds after
//! the cycle began is not in the job's copy of the table, so the marker skips
//! addresses into it: everything in it is black.
//!
//! # What else it records
//!
//! Reading every field of every live object is also the one moment anything
//! sees every pointer in the old generation, so the marker notes the fields
//! pointing into blocks chosen for evacuation, and hands them to those blocks --
//! see [`crate::evacuate`].
//!
//! # Who runs it
//!
//! A small pool of OS threads, shared by every heap in the process, takes jobs
//! in turn. With no pool the program's own thread marks a slice at each
//! nursery collection, which is also what a heap falling behind does to catch
//! up: the pause stays bounded either way.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock};
use std::time::{Duration, Instant};

use crate::heap::Kind;
use crate::object::Head;
use crate::old::{self, Block, OLD_BASE};
use crate::region::{self, REGION_BASE};
use crate::value::Addr;
use meadow_core::desc;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Fields of one object read before the marker checks the time again, and
/// before it lets go of a large object to come back to it.
const CHUNK: usize = 4096;

/// Entries taken from the shared stack at once.
const BATCH: usize = 4096;

/// A cycle's marking.
pub struct Job {
    pub epoch: u32,
    /// The heap's block table when the cycle began.
    blocks: Vec<Arc<Block>>,
    /// Its region blocks then, sorted, to tell which region an address is in.
    regions: Vec<Arc<region::Block>>,
    mutation: Arc<Mutex<()>>,
    /// The top chunk of every detached stack segment a live object names.
    stacks: Mutex<std::collections::HashSet<Addr>>,
    grey: Mutex<Grey>,
    /// Regions reached, for the heap to keep.
    found: Mutex<Vec<u32>>,
    marked: AtomicU64,
    nanos: AtomicU64,
    queued: AtomicBool,
    cancelled: AtomicBool,
}

/// What is left to mark. An entry is an address, and in its high half the
/// field to carry on from: 0 for an object not yet looked at.
struct Grey {
    stack: Vec<u64>,
    /// Markers holding a batch taken from `stack`.
    working: usize,
}

fn entry(a: Addr, from: usize) -> u64 {
    a as u64 | (from as u64) << 32
}

impl Job {
    pub fn new(
        epoch: u32,
        blocks: Vec<Arc<Block>>,
        regions: Vec<Arc<region::Block>>,
        mutation: Arc<Mutex<()>>,
    ) -> Arc<Job> {
        Arc::new(Job {
            epoch,
            blocks,
            regions,
            mutation,
            stacks: Mutex::new(std::collections::HashSet::new()),
            grey: Mutex::new(Grey {
                stack: Vec::new(),
                working: 0,
            }),
            found: Mutex::new(Vec::new()),
            marked: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            queued: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        })
    }

    /// Mark from these addresses too: roots, or logged overwrites.
    pub fn push(&self, addrs: &[Addr]) {
        if addrs.is_empty() {
            return;
        }
        lock(&self.grey)
            .stack
            .extend(addrs.iter().map(|a| entry(*a, 0)));
    }

    fn has_work(&self) -> bool {
        !lock(&self.grey).stack.is_empty()
    }

    /// Nothing left to mark, and no one in the middle of marking. Only the
    /// heap's own thread can add more, so this stays true until it does.
    pub fn is_done(&self) -> bool {
        let g = lock(&self.grey);
        g.stack.is_empty() && g.working == 0
    }

    /// The heap is done with this job, or gone: stop working on it.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn marked(&self) -> u64 {
        self.marked.load(Ordering::Acquire)
    }

    pub fn nanos(&self) -> u64 {
        self.nanos.load(Ordering::Acquire)
    }

    pub fn found(&self) -> Vec<u32> {
        let mut f = std::mem::take(&mut *lock(&self.found));
        f.sort_unstable();
        f.dedup();
        f
    }

    /// Mark for up to about `budget`, and at most about `limit` fields.
    /// The detached stack segments live objects named, by top chunk.
    pub fn stacks(&self) -> std::collections::HashSet<Addr> {
        lock(&self.stacks).clone()
    }

    pub fn step(&self, budget: Duration, limit: usize) {
        let started = Instant::now();
        let mut local = {
            let mut g = lock(&self.grey);
            if g.stack.is_empty() {
                return;
            }
            g.working += 1;
            let keep = g.stack.len().saturating_sub(BATCH);
            g.stack.split_off(keep)
        };
        let mut found = Vec::new();
        let mut refs = Vec::new();
        let mut marked = 0u64;
        let mut work = 0usize;
        let mut total = 0usize;
        while let Some(e) = local.pop() {
            let done = self.scan(
                e,
                &mut local,
                &mut found,
                &mut refs,
                &mut marked,
                limit.min(CHUNK),
            );
            work += done;
            total += done;
            if total >= limit {
                break;
            }
            if work >= CHUNK {
                work = 0;
                if started.elapsed() >= budget || self.cancelled.load(Ordering::Acquire) {
                    break;
                }
            }
        }
        // What this batch found is published before it counts as finished:
        // the heap takes a job with nothing in hand as done, and reads these.
        if !found.is_empty() {
            lock(&self.found).extend(found);
        }
        if !refs.is_empty() {
            // Fields pointing into blocks chosen for evacuation go to those
            // blocks' lists, a block at a time.
            refs.sort_unstable();
            for group in refs.chunk_by(|a, b| a >> 32 == b >> 32) {
                self.blocks[(group[0] >> 32) as usize].record(group.iter().map(|e| *e as Addr));
            }
        }
        self.marked.fetch_add(marked, Ordering::AcqRel);
        self.nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::AcqRel);
        let mut g = lock(&self.grey);
        g.stack.extend(local);
        g.working -= 1;
    }

    /// One grey entry: mark the object, and push what its fields reach. The
    /// work done, in fields read.
    fn scan(
        &self,
        e: u64,
        local: &mut Vec<u64>,
        found: &mut Vec<u32>,
        refs: &mut Vec<u64>,
        marked: &mut u64,
        chunk: usize,
    ) -> usize {
        let a = e as Addr;
        let from = (e >> 32) as usize;
        if a >= REGION_BASE {
            if let Some(id) = self.region_of(a) {
                found.push(id);
            }
            return 1;
        }
        if a < OLD_BASE {
            return 1;
        }
        let bi = old::block_of(a);
        let Some(b) = self.blocks.get(bi).filter(|b| !b.is_empty()) else {
            // A block made since the cycle began: all of it is black.
            return 1;
        };
        // A frame-stack chunk: the heap that owns it scanned its frames as
        // roots when this cycle began, and the frames may be pushed and popped
        // under a marker at any time, so nothing here reads one.
        if b.state() == old::STACK {
            return 1;
        }
        let off = old::offset_of(a);
        if from == 0 && !b.mark(off, self.epoch) {
            return 1;
        }
        let word = |k: usize| {
            let s = a + k as Addr;
            self.blocks[old::block_of(s)].get(old::offset_of(s))
        };
        // A mutable object's descriptors change as its fields do, under the
        // mutation lock; its kind never does.
        let kind = Kind::from_byte(word(0) as u8);
        // A detached stack segment reachable from the program: the chunks it
        // names stay. Their frames were roots at the start of the cycle, so
        // nothing in them needs reading here -- see `Heap::finish_cycle`.
        if kind == Kind::Stack {
            lock(&self.stacks).insert(word(2) as Addr);
        }
        let _held =
            matches!(kind, Kind::Ref | Kind::MutArray | Kind::Resume).then(|| lock(&self.mutation));
        let h = Head::read(word(0), word(1));
        let (len, meta) = (h.len as usize, h.meta);
        if from == 0 {
            old::stamp_lines(&self.blocks, a, h.size(), self.epoch);
            b.add_live(h.size());
            *marked += h.size() as u64;
            if kind == Kind::Compact {
                found.push(meta);
            }
        }
        let end = len.min(from + chunk.max(1));
        let header = h.header();
        for i in from..end {
            if h.desc(i, word) != desc::REF {
                continue;
            }
            let s = a + (header + i) as Addr;
            {
                let x = word(header + i) as Addr;
                if x >= REGION_BASE {
                    if let Some(id) = self.region_of(x)
                        && found.last() != Some(&id)
                    {
                        found.push(id);
                    }
                } else if x >= OLD_BASE {
                    let xb = old::block_of(x);
                    if let Some(t) = self.blocks.get(xb)
                        && t.is_tracked()
                        && t.count_ref()
                    {
                        refs.push(entry(s, xb));
                    }
                    local.push(entry(x, 0));
                }
            }
        }
        if end < len {
            local.push(entry(a, end));
        }
        1 + end - from
    }

    fn region_of(&self, a: Addr) -> Option<u32> {
        let i = self.regions.partition_point(|b| b.base <= a);
        let b = self.regions.get(i.checked_sub(1)?)?;
        ((a - b.base) < b.cap as Addr).then_some(b.region)
    }
}

// --- the pool ------------------------------------------------------------------

struct Pool {
    queue: Mutex<VecDeque<Arc<Job>>>,
    ready: Condvar,
}

static POOL: OnceLock<Pool> = OnceLock::new();
static STARTED: Once = Once::new();

/// How long a marker works on one job before giving the next its turn.
const TURN: Duration = Duration::from_millis(2);

/// The pool, starting `threads` marking threads if none are running yet.
fn pool(threads: usize) -> &'static Pool {
    let pool = POOL.get_or_init(|| Pool {
        queue: Mutex::new(VecDeque::new()),
        ready: Condvar::new(),
    });
    STARTED.call_once(|| {
        for n in 0..threads.max(1) {
            std::thread::Builder::new()
                .name(format!("meadow-mark-{n}"))
                .spawn(move || work(pool))
                .expect("starting a marking thread");
        }
    });
    pool
}

/// Have a marking thread top up or trim the spare blocks, if they want it.
pub fn tend_spares(threads: usize) {
    if old::spares_want_tending() {
        let pool = pool(threads);
        let _q = lock(&pool.queue);
        pool.ready.notify_one();
    }
}

/// Hand `job` to the marking threads, starting `threads` of them if none are
/// running yet.
pub fn submit(job: &Arc<Job>, threads: usize) {
    let pool = pool(threads);
    if job.queued.swap(true, Ordering::AcqRel) {
        return;
    }
    lock(&pool.queue).push_back(job.clone());
    pool.ready.notify_one();
}

fn work(pool: &'static Pool) {
    loop {
        let job = {
            let mut q = lock(&pool.queue);
            loop {
                if let Some(j) = q.pop_front() {
                    break Some(j);
                }
                if old::spares_want_tending() {
                    break None;
                }
                q = pool.ready.wait(q).unwrap_or_else(|p| p.into_inner());
            }
        };
        let Some(job) = job else {
            old::tend_spares();
            continue;
        };
        if !job.cancelled.load(Ordering::Acquire) {
            job.step(TURN, usize::MAX);
        }
        if !job.cancelled.load(Ordering::Acquire) && job.has_work() {
            lock(&pool.queue).push_back(job);
            continue;
        }
        // Let go, then look again: work pushed between the check and letting go
        // would otherwise find the job still marked queued, and never run.
        job.queued.store(false, Ordering::Release);
        if !job.cancelled.load(Ordering::Acquire)
            && job.has_work()
            && !job.queued.swap(true, Ordering::AcqRel)
        {
            lock(&pool.queue).push_back(job);
        }
    }
}
