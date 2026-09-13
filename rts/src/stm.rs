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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use crate::region::Region;
use crate::value::Value;

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

/// Every `TVar` of one run, the clock, and the commit lock. Threads waiting on a
/// `TVar` are the scheduler's to keep: it parks them after checking
/// [`World::changed`], and wakes them with what [`World::commit`] wrote.
pub struct World {
    clock: AtomicU64,
    commit: Mutex<()>,
    tvars: RwLock<Vec<Arc<TVar>>>,
}

impl Default for World {
    fn default() -> Self {
        World {
            clock: AtomicU64::new(0),
            commit: Mutex::new(()),
            tvars: RwLock::new(Vec::new()),
        }
    }
}

/// A transaction in progress: when it started, what it read, what it wrote.
pub struct Txn {
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
        let mut tvars = self.tvars.write().unwrap_or_else(|p| p.into_inner());
        tvars.push(Arc::new(TVar {
            cell: Mutex::new(Cell { version: 0, value }),
        }));
        (tvars.len() - 1) as u32
    }

    fn tvar(&self, id: u32) -> Arc<TVar> {
        self.tvars.read().unwrap_or_else(|p| p.into_inner())[id as usize].clone()
    }

    pub fn begin(&self) -> Txn {
        Txn {
            start: self.clock.load(Ordering::SeqCst),
            reads: Vec::new(),
            writes: vec![Vec::new()],
        }
    }

    /// Read `id` in `txn`: its own write if it made one, or the committed value
    /// if nothing has written it since `txn` started.
    pub fn read(&self, txn: &mut Txn, id: u32) -> Read {
        for frame in txn.writes.iter().rev() {
            if let Some((_, s)) = frame.iter().find(|(t, _)| *t == id) {
                return Read::Value(s.clone());
            }
        }
        let tvar = self.tvar(id);
        let cell = lock(&tvar.cell);
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
            .unwrap_or_else(|| lock(&self.tvar(id).cell).value.clone());
        match current.region {
            Some(r) if r.used() <= 4 * current.fresh.max(1024) => (r, false, current.fresh),
            _ => (Region::new(), true, 0),
        }
    }

    /// Commit `txn`: if nothing it read has been written since, publish its
    /// writes, and answer which `TVar`s they were. `None` is a conflict.
    pub fn commit(&self, txn: Txn) -> Option<Vec<u32>> {
        let Txn {
            reads, mut writes, ..
        } = txn;
        let writes = writes.drain(..).next().unwrap_or_default();
        let _commit = lock(&self.commit);
        let mut ids: Vec<u32> = reads
            .iter()
            .map(|(t, _)| *t)
            .chain(writes.iter().map(|(t, _)| *t))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let tvars: Vec<Arc<TVar>> = ids.iter().map(|id| self.tvar(*id)).collect();
        let mut cells: Vec<MutexGuard<'_, Cell>> = tvars.iter().map(|t| lock(&t.cell)).collect();
        let at = |id: u32| ids.binary_search(&id).expect("a locked TVar");
        if reads
            .iter()
            .any(|(id, version)| cells[at(*id)].version != *version)
        {
            return None;
        }
        if writes.is_empty() {
            return Some(Vec::new());
        }
        // Every written `TVar` is locked before the clock moves, so a
        // transaction starting after this commit's version waits for the value
        // rather than seeing the old one.
        let version = self.clock.fetch_add(1, Ordering::SeqCst) + 1;
        let mut written = Vec::with_capacity(writes.len());
        for (id, value) in writes {
            let cell = &mut cells[at(id)];
            cell.version = version;
            cell.value = value;
            written.push(id);
        }
        Some(written)
    }

    /// Has anything in `reads` been written since it was read?
    pub fn changed(&self, reads: &[(u32, u64)]) -> bool {
        reads
            .iter()
            .any(|(id, version)| lock(&self.tvar(*id).cell).version != *version)
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
