//! Software transactional memory: the runtime's half.
//!
//! `Std.Stm` writes the control -- running a transaction, `retry`, `orElse`,
//! running again on a conflict -- as handlers. This is what a handler cannot
//! do: hold `TVar`s every green thread can see, keep each transaction's log,
//! and commit a log as one step. The algorithm, and why it is safe, is in
//! `meadow_core::stm`.
//!
//! # Where a `TVar`'s value lives
//!
//! In a shared region ([`crate::region`]), where every thread reads it in place:
//! reading a `TVar` holding a large map costs a pointer, not a copy of the map.
//! A write copies the new value into the region the old one is in, so whatever
//! it has in common with the old value -- a map with one entry more shares all
//! the rest -- is not copied again. That region only grows, so when it has grown
//! to several times the size of a fresh copy, a write starts a new one and the
//! old is freed once nothing reads it.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::region::Region;
use crate::value::Value;

/// Take a `TVar`'s cell.
///
/// A plain mutex, and two cleverer things were tried here and are both
/// measurably worse on `benchmarks/contention` -- eight threads moving money
/// between sixteen accounts, which is about as much contention as a program is
/// likely to arrange:
///
/// * **Spinning before waiting**, at every budget from 16 to 4096 tries:
///   504ms, 506ms, 526ms, 636ms for 0, 16, 64 and 256. A profile of this
///   benchmark is mostly `__psynch_mutexwait`, which looks like lock overhead
///   and is not: the transactions really do conflict, and a waiter that spins
///   is a waiter holding a core the thread it waits on could have used.
/// * **An `RwLock` per cell**, so that reads -- which only compare a version
///   and copy a pointer -- need not take turns: 729ms against 581ms. Rust's
///   `RwLock` costs more to acquire either way than a `Mutex`, and the readers
///   were contending on the same word regardless.
///
/// The cost was not here. One thread doing every transfer, with nobody to
/// conflict with, took longer than eight did -- so it was what a transaction
/// costs, and what every thread wrote on the way through one: a read lock over
/// the table of `TVar`s ([`Table`] has none now), the count of an `Arc<World>`
/// cloned per primitive, a region made and thrown away for every `Int` written,
/// a lock over every waiter taken by every commit ([`World::someone_waits`]),
/// a trip through the scheduler to commit at all, and two allocations inside
/// the commit. With those gone the same benchmark went from 345ms to 143ms on
/// one thread and 225ms to 103ms on eight, 39ms of either being startup.
///
/// What was *not* the cost, measured by taking it out: `Std.Stm` running the
/// control as an effect handler. A `handle` per transaction is within noise.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A value where every thread can read it: an immediate, or an address into a
/// shared region, held alive.
#[derive(Clone)]
pub struct Shared {
    pub region: Option<Arc<Region>>,
    pub root: Value,
    /// The region's size when it was last started afresh -- about what a copy
    /// of a value costs, for deciding when the region has grown too much.
    pub fresh: usize,
}

/// One `TVar`.
pub struct TVar {
    cell: Mutex<Cell>,
}

struct Cell {
    /// The clock value of the commit that last wrote it.
    version: u64,
    value: Shared,
}

/// Every `TVar` there is, by number, found **without a lock**.
///
/// This was an `RwLock<Vec<Arc<TVar>>>`, and a read lock is not free: taking
/// and dropping one is two writes to a word every thread shares. A transfer
/// between two accounts took it three times, so eight threads spent their
/// transactions passing that one cache line around, and ran no faster than one.
///
/// A `TVar` never moves and is never removed, so nothing needs excluding.
/// Segment `k` holds `64 << k` of them and is allocated whole the first time
/// one of them is made; finding `id` is a count of leading zeros and two
/// loads. Only making a `TVar` takes a lock, for the count.
struct Table {
    segments: [OnceLock<Box<[TVar]>>; 26],
    len: Mutex<u32>,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            segments: std::array::from_fn(|_| OnceLock::new()),
            len: Mutex::new(0),
        }
    }
}

impl Table {
    /// Which segment `id` is in, and where.
    fn place(id: u32) -> (usize, usize) {
        let n = id as u64 + 64;
        let k = 63 - n.leading_zeros() as usize - 6;
        (k, (n - (64 << k)) as usize)
    }

    fn get(&self, id: u32) -> &TVar {
        let (k, at) = Self::place(id);
        &self.segments[k].get().expect("a TVar that was made")[at]
    }

    fn push(&self, value: Shared) -> u32 {
        let mut len = lock(&self.len);
        let id = *len;
        let (k, at) = Self::place(id);
        let segment = self.segments[k].get_or_init(|| {
            (0..64usize << k)
                .map(|_| TVar {
                    cell: Mutex::new(Cell {
                        version: 0,
                        value: Shared {
                            region: None,
                            root: Value::Unit,
                            fresh: 0,
                        },
                    }),
                })
                .collect()
        });
        lock(&segment[at].cell).value = value;
        *len += 1;
        id
    }
}

/// Every `TVar` of one run and the clock. Threads waiting on a `TVar` are the
/// scheduler's to keep: it parks them after checking [`World::changed`], and
/// wakes them with what [`World::commit`] wrote.
///
/// No lock over commits: [`World::commit`] locks the `TVar`s a transaction
/// touched, in order, and that is the whole of the mutual exclusion.
pub struct World {
    clock: AtomicU64,
    tvars: Table,
    /// How many threads are waiting on a `TVar`, or are about to be: what a
    /// commit reads to know whether it has anyone to wake. See
    /// [`World::someone_waits`].
    waiting: AtomicUsize,
}

impl Default for World {
    fn default() -> Self {
        World {
            clock: AtomicU64::new(0),
            tvars: Table::default(),
            waiting: AtomicUsize::new(0),
        }
    }
}

/// How many `TVar`s a commit handles without allocating.
const FEW: usize = 8;

/// How a commit went.
pub enum Commit {
    /// Something it read was written meanwhile: run it again.
    Conflict,
    /// Published, and nobody was waiting to hear.
    Done,
    /// Published, these `TVar`s written, and some thread is waiting on a
    /// `TVar`: the scheduler has to look whether it is one of these.
    Wake(Vec<u32>),
}

/// A transaction in progress: when it started, what it read, what it wrote.
pub struct Txn {
    /// False once it has been committed: what is left is its logs, for the
    /// next one to use -- see [`World::begin`].
    pub live: bool,
    pub start: u64,
    /// `(tvar, version read)`, once per `TVar`.
    pub reads: Vec<(u32, u64)>,
    /// Writes, innermost `orElse` last. Each frame holds a `TVar` at most once.
    pub writes: Vec<Vec<(u32, Shared)>>,
}

/// What reading a `TVar` in a transaction found.
pub enum Read {
    Value(Shared),
    /// Written since the transaction started: run it again.
    Conflict,
}

impl World {
    pub fn new_tvar(&self, value: Shared) -> u32 {
        self.tvars.push(value)
    }

    /// Start a transaction, in what is left of the thread's last one if there
    /// is one: its logs are emptied and kept, so a thread that runs
    /// transactions in a loop allocates for the first of them only.
    pub fn begin(&self, last: Option<Txn>) -> Txn {
        let mut txn = last.unwrap_or_else(|| Txn {
            live: false,
            start: 0,
            reads: Vec::new(),
            writes: Vec::new(),
        });
        txn.live = true;
        txn.start = self.clock.load(Ordering::SeqCst);
        txn.reads.clear();
        txn.writes.truncate(1);
        match txn.writes.first_mut() {
            Some(frame) => frame.clear(),
            None => txn.writes.push(Vec::new()),
        }
        txn
    }

    /// Read `id` in `txn`: its own write if it made one, or the committed value
    /// if nothing has written it since `txn` started.
    pub fn read(&self, txn: &mut Txn, id: u32) -> Read {
        for frame in txn.writes.iter().rev() {
            if let Some((_, s)) = frame.iter().find(|(t, _)| *t == id) {
                return Read::Value(s.clone());
            }
        }
        let cell = lock(&self.tvars.get(id).cell);
        if cell.version > txn.start {
            return Read::Conflict;
        }
        if !txn.reads.iter().any(|(t, _)| *t == id) {
            txn.reads.push((id, cell.version));
        }
        Read::Value(cell.value.clone())
    }

    /// The region a write to `id` should copy its value into, and how big a
    /// fresh copy was: the current value's region, unless it has grown to four
    /// times that, in which case a new one.
    pub fn region_for_write(&self, txn: &Txn, id: u32) -> (Arc<Region>, bool, usize) {
        let current = txn
            .writes
            .iter()
            .rev()
            .find_map(|f| f.iter().find(|(t, _)| *t == id).map(|(_, s)| s.clone()))
            .unwrap_or_else(|| lock(&self.tvars.get(id).cell).value.clone());
        match current.region {
            Some(r) if r.used() <= 4 * current.fresh.max(1024) => (r, false, current.fresh),
            _ => (Region::new(), true, 0),
        }
    }

    /// Commit `txn`, which is over either way: if nothing it read has been
    /// written since, publish its writes.
    pub fn commit(&self, txn: &mut Txn) -> Commit {
        txn.live = false;
        let reads = &txn.reads;
        let writes = match txn.writes.first_mut() {
            Some(frame) => frame,
            None => return Commit::Done,
        };
        // Every `TVar` this transaction read *or* wrote, locked in one order.
        //
        // That is two-phase locking over the whole read and write set, which is
        // all serializability needs, and sorting is what stops two transactions
        // touching the same `TVar`s from deadlocking on each other. There is no
        // lock over commits as a whole: two transactions touching disjoint
        // `TVar`s have nothing to say to each other and commit at once.
        //
        // There used to be one, and it cost more than it looked. Eight threads
        // moving money between sixteen accounts took three times as long as one
        // thread doing all the same transfers, because every commit queued
        // behind every other and the threads spent their time being parked and
        // woken rather than working.
        //
        // On the stack when there are few of them, which is nearly always: a
        // commit that allocates twice is a commit that takes the allocator's
        // lock twice, in the one place every thread is in a hurry.
        let mut few = [0u32; FEW];
        let mut many = Vec::new();
        let count = reads.len() + writes.len();
        let ids: &mut [u32] = if count <= FEW {
            &mut few[..count]
        } else {
            many.resize(count, 0);
            &mut many
        };
        for (slot, id) in ids.iter_mut().zip(
            reads
                .iter()
                .map(|(t, _)| *t)
                .chain(writes.iter().map(|(t, _)| *t)),
        ) {
            *slot = id;
        }
        ids.sort_unstable();
        let mut n = 0;
        for i in 0..ids.len() {
            if n == 0 || ids[n - 1] != ids[i] {
                ids[n] = ids[i];
                n += 1;
            }
        }
        let ids = &ids[..n];
        let mut few_cells: [Option<MutexGuard<'_, Cell>>; FEW] = std::array::from_fn(|_| None);
        let mut many_cells = Vec::new();
        let cells: &mut [Option<MutexGuard<'_, Cell>>] = if n <= FEW {
            &mut few_cells[..n]
        } else {
            many_cells.resize_with(n, || None);
            &mut many_cells
        };
        for (slot, id) in cells.iter_mut().zip(ids) {
            *slot = Some(lock(&self.tvars.get(*id).cell));
        }
        let at = |id: u32| ids.binary_search(&id).expect("a locked TVar");
        if reads
            .iter()
            .any(|(id, version)| cells[at(*id)].as_ref().expect("locked above").version != *version)
        {
            return Commit::Conflict;
        }
        if writes.is_empty() {
            return Commit::Done;
        }
        // Every written `TVar` is locked before the clock moves, so a
        // transaction starting after this commit's version waits for the value
        // rather than seeing the old one.
        let version = self.clock.fetch_add(1, Ordering::SeqCst) + 1;
        // Read while the cells are still held, which is after the writes as
        // far as anyone waiting can tell -- see `World::wait_begins`.
        let wake = self.someone_waits();
        let mut written = Vec::new();
        for (id, value) in writes.drain(..) {
            let cell = cells[at(id)].as_mut().expect("locked above");
            cell.version = version;
            cell.value = value;
            if wake {
                written.push(id);
            }
        }
        if wake {
            Commit::Wake(written)
        } else {
            Commit::Done
        }
    }

    /// A thread means to wait. Called **before** it checks
    /// [`World::changed`], which is what makes [`World::someone_waits`] safe
    /// to act on: a commit publishes its writes and then reads the count, so
    /// either it sees this thread counted and goes to wake it, or this
    /// thread's check comes after the writes and it does not wait at all.
    pub fn wait_begins(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
    }

    /// A thread counted by [`World::wait_begins`] is not waiting any more: it
    /// was woken, or found it had no need to wait.
    pub fn wait_ends(&self) {
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }

    /// Might a thread be waiting on a `TVar`? Read after a commit, which has
    /// nobody to tell when this is false -- and that is nearly always, so a
    /// commit is nearly always the committing thread's business alone. It used
    /// to go through the scheduler and take the one lock over every waiter,
    /// which every thread then queued on at every transaction.
    pub fn someone_waits(&self) -> bool {
        self.waiting.load(Ordering::SeqCst) != 0
    }

    /// Has anything in `reads` been written since it was read?
    pub fn changed(&self, reads: &[(u32, u64)]) -> bool {
        reads
            .iter()
            .any(|(id, version)| lock(&self.tvars.get(*id).cell).version != *version)
    }
}

impl Txn {
    /// Record a write in the innermost frame.
    pub fn write(&mut self, id: u32, value: Shared) {
        let frame = self.writes.last_mut().expect("a frame");
        match frame.iter_mut().find(|(t, _)| *t == id) {
            Some(slot) => slot.1 = value,
            None => frame.push((id, value)),
        }
    }

    /// `orElse`'s first branch succeeded: its writes join the frame around it.
    pub fn merge(&mut self) {
        if self.writes.len() < 2 {
            return;
        }
        let inner = self.writes.pop().expect("a nested frame");
        for (id, value) in inner {
            self.write(id, value);
        }
    }

    /// `orElse`'s first branch retried: drop its writes. Its reads stay.
    pub fn rollback(&mut self) {
        if self.writes.len() > 1 {
            self.writes.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Shared {
        Shared {
            region: None,
            root: Value::Int(n),
            fresh: 0,
        }
    }

    fn value(world: &World, id: u32) -> Value {
        let mut txn = world.begin(None);
        match world.read(&mut txn, id) {
            Read::Value(s) => s.root,
            Read::Conflict => panic!("nothing else is running"),
        }
    }

    /// Every id has a place of its own, across the segment boundaries.
    #[test]
    fn a_tvar_is_found_by_its_number() {
        assert_eq!(Table::place(0), (0, 0));
        assert_eq!(Table::place(63), (0, 63));
        assert_eq!(Table::place(64), (1, 0));
        assert_eq!(Table::place(191), (1, 127));
        assert_eq!(Table::place(192), (2, 0));
        let world = World::default();
        let ids: Vec<u32> = (0..1000).map(|n| world.new_tvar(int(n))).collect();
        for (n, id) in ids.iter().enumerate() {
            assert_eq!(*id, n as u32);
            assert_eq!(value(&world, *id), Value::Int(n as i64));
        }
    }

    #[test]
    fn a_commit_publishes_or_conflicts_and_its_logs_are_used_again() {
        let world = World::default();
        let a = world.new_tvar(int(1));
        let b = world.new_tvar(int(2));

        let mut first = world.begin(None);
        let mut second = world.begin(None);
        assert!(matches!(world.read(&mut first, a), Read::Value(_)));
        assert!(matches!(world.read(&mut second, a), Read::Value(_)));
        first.write(a, int(10));
        first.write(b, int(20));
        assert!(matches!(world.commit(&mut first), Commit::Done));
        assert!(!first.live);
        // It read `a` before that commit wrote it.
        second.write(b, int(99));
        assert!(matches!(world.commit(&mut second), Commit::Conflict));
        assert_eq!(value(&world, a), Value::Int(10));
        assert_eq!(value(&world, b), Value::Int(20));

        // The next transaction, in the last one's logs: nothing left over.
        let mut again = world.begin(Some(second));
        assert!(again.live && again.reads.is_empty());
        assert!(again.writes.len() == 1 && again.writes[0].is_empty());
        again.write(b, int(30));
        assert!(matches!(world.commit(&mut again), Commit::Done));
        assert_eq!(value(&world, b), Value::Int(30));
    }

    /// More `TVar`s than a commit keeps on the stack.
    #[test]
    fn a_large_commit_is_a_commit_too() {
        let world = World::default();
        let ids: Vec<u32> = (0..3 * FEW as i64)
            .map(|n| world.new_tvar(int(n)))
            .collect();
        let mut txn = world.begin(None);
        for id in &ids {
            assert!(matches!(world.read(&mut txn, *id), Read::Value(_)));
            txn.write(*id, int(-1));
        }
        assert!(matches!(world.commit(&mut txn), Commit::Done));
        assert!(ids.iter().all(|id| value(&world, *id) == Value::Int(-1)));
    }

    /// A commit names what it wrote only while someone is waiting to hear.
    #[test]
    fn a_commit_wakes_only_when_someone_waits() {
        let world = World::default();
        let a = world.new_tvar(int(0));
        world.wait_begins();
        let mut txn = world.begin(None);
        txn.write(a, int(1));
        match world.commit(&mut txn) {
            Commit::Wake(written) => assert_eq!(written, vec![a]),
            _ => panic!("a thread was waiting"),
        }
        world.wait_ends();
        let mut txn = world.begin(None);
        txn.write(a, int(2));
        assert!(matches!(world.commit(&mut txn), Commit::Done));
    }
}
