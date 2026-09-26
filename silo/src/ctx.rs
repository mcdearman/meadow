//! **A green thread's context**: everything the runtime keeps for one thread
//! -- its heap, its stack segments, its literals and top-level values, its
//! transaction -- apart from every other thread's, so that none of it is ever
//! touched by two OS threads at once and counting needs no atomics.
//!
//! A thread moves between OS threads (`crate::sched`), so what is "current"
//! is set by the worker that resumes it, and read through [`get`] -- which is
//! never inlined. That matters: code that waits, and so may resume on another
//! OS thread, must not keep an OS thread's thread-local address from before
//! the wait, and a call LLVM cannot see into, made after the wait, is what
//! stops it.

use crate::heap::{Heap, Word};
use crate::segments::{Parked, Suspended};
use std::cell::Cell;
use std::collections::HashMap;

/// A transaction in progress (`crate::sched`): when it began, the `TVar`s it
/// read and the version of each it saw, and its writes -- values in this
/// thread's heap -- one level per `orElse` being tried.
pub struct Txn {
    pub start: u64,
    pub reads: Vec<(usize, u64)>,
    pub writes: Vec<Vec<(usize, Word, i64)>>,
}

/// What a waiting thread asks of the scheduler, which acts on it once the
/// thread has stopped: see `crate::sched`.
pub enum Request {
    Yield,
    Await(usize),
    Receive(usize),
    Wait(Vec<(usize, u64)>),
    /// The thread failed with this message, and is done.
    Failed(String),
}

/// What a thread is woken with.
pub enum Wake {
    Nothing,
    Value(crate::parcel::Parcel),
    Fail(String),
}

pub struct Ctx {
    pub heap: Heap,
    /// The segments running, innermost last: see `crate::segments`.
    pub running: Vec<*const ()>,
    /// Suspended segments, by the number their stack objects hold, with the
    /// handler each was suspended for and the segments suspended inside it.
    pub(crate) suspended: Vec<Option<Suspended>>,
    /// Segments suspended for a handler further out than their own, while
    /// that suspension lasts, innermost first: see `crate::segments::drive`.
    pub(crate) parked: Vec<Parked>,
    /// The running segment's shadow chain: see `crate::shadow`. Here for a
    /// program with threads; one without keeps it in a global.
    pub shadow: Word,
    pub next_handler: u32,
    /// String literals, made once per thread, by their bytes' address.
    pub literals: HashMap<usize, Word>,
    /// Top-level values, once computed on this thread: see `GlobalGet`.
    pub globals: Vec<Option<(Word, i64)>>,
    /// What the leak check does not count: see `crate::prims::roots`.
    pub kept: Vec<(Word, i64)>,
    pub txn: Option<Txn>,
    pub request: Option<Request>,
    pub wake: Wake,
    /// Stacks of segments that have finished, to be used again: making one
    /// reserves memory and sets guard pages, which a program full of
    /// handlers would otherwise do at every `handle`.
    pub(crate) stacks: Vec<corosensei::stack::DefaultStack>,
    /// Where a call puts the arguments that do not fit in registers.
    /// As many words as the emitted module's widest call needs:
    /// `meadow_spill_words`.
    pub spill: Vec<Word>,
    /// The thread's number: 0 is `main`.
    pub tid: usize,
}

// Safety: a context is used by one OS thread at a time -- the one running its
// thread -- and handed between them by the scheduler, under its lock.
unsafe impl Send for Ctx {}

impl Ctx {
    pub fn new(tid: usize) -> Box<Ctx> {
        Box::new(Ctx {
            heap: Heap::new(),
            running: Vec::new(),
            suspended: Vec::new(),
            parked: Vec::new(),
            shadow: 0,
            next_handler: 1,
            literals: HashMap::new(),
            globals: Vec::new(),
            kept: Vec::new(),
            txn: None,
            request: None,
            wake: Wake::Nothing,
            stacks: Vec::new(),
            // Safety: a constant the emitted module defines.
            spill: vec![0; unsafe { meadow_spill_words }.max(0) as usize],
            tid,
        })
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        // Dropping a suspended segment unwinds it, which a runtime that aborts
        // on a panic cannot do: they are let go, and their memory with them.
        for s in self.suspended.drain(..).flatten() {
            std::mem::forget(s);
        }
        for s in self.parked.drain(..) {
            std::mem::forget(s);
        }
    }
}

thread_local! {
    static CURRENT: Cell<*mut Ctx> = const { Cell::new(std::ptr::null_mut()) };
}

/// The one context of a program that cannot spawn: see [`threaded`].
static mut ONLY: *mut Ctx = std::ptr::null_mut();

unsafe extern "C" {
    /// Whether the program spawns a thread anywhere: the emitted module says
    /// so, since it knows what the program does. See `meadow_llvm::emit`.
    static meadow_threaded: u8;
    /// How many words a call passes in the spill area, at most: the
    /// emitted module says, having seen every call.
    static meadow_spill_words: i64;
}

/// Whether the program can ever have a second thread. When it cannot there is
/// nothing to move between OS threads, so the one context is a static, and
/// reading it is a load rather than a thread-local's lookup.
#[inline(always)]
pub fn threaded() -> bool {
    // Safety: a constant the emitted module defines.
    unsafe { meadow_threaded != 0 }
}

/// The running thread's context.
#[inline(always)]
pub fn get() -> *mut Ctx {
    if !threaded() {
        // Safety: set before the program runs, and never changed after.
        return unsafe { ONLY };
    }
    current()
}

/// The running thread's context, when threads move between OS threads. Never
/// inlined: see the module docs.
#[inline(never)]
fn current() -> *mut Ctx {
    let c = CURRENT.with(Cell::get);
    if c.is_null() {
        crate::fail("the native runtime ran outside every thread")
    }
    c
}

/// Whether there is a running thread's context on this OS thread.
#[inline(never)]
pub fn present() -> bool {
    if !threaded() {
        // Safety: as `get`.
        return !unsafe { ONLY }.is_null();
    }
    !CURRENT.with(Cell::get).is_null()
}

/// Make `c` the running thread's context on this OS thread.
#[inline(never)]
pub fn set(c: *mut Ctx) {
    if !threaded() {
        // Safety: the program has one thread, and this is called before it
        // runs.
        unsafe { ONLY = c };
        return;
    }
    CURRENT.with(|cur| cur.set(c));
}
