//! **Green threads, channels and transactions**, M:N over OS threads. See
//! `docs/SILO.md`, "Threads".
//!
//! A thread is a coroutine -- a stack segment, as a `handle` is -- with a
//! context of its own (`crate::ctx`): its own heap, so counting needs no
//! atomics, and what crosses between threads is copied ([`Parcel`]). A worker
//! per core takes ready threads off one queue and runs each until it waits: on
//! a thread it awaits, on an empty channel, in a transaction's `retry`, or by
//! yielding. `main` is a thread too, and the program ends when it does;
//! threads still running then are stopped. A thread that neither waits nor
//! finishes gives way at a safe point once its turn is up, so one that never
//! waits cannot keep a core to itself.
//!
//! None of this is here for a program that cannot spawn: it runs on the
//! thread that called it, with no scheduler, no workers and no safe points
//! (`crate::ctx`, and `meadow_llvm::emit`).
//!
//! A thread that waits says why in its context and suspends -- every segment
//! of it, out to the worker, as performing an operation further out does
//! ([`crate::segments`]). The worker files it, under the scheduler's lock, with
//! what it waits on, or puts it straight back if that is already there: no
//! wake-up can fall between the thread deciding to wait and its waiting.
//!
//! A transaction keeps its writes in a log, one level per `orElse`, and
//! reads a `TVar` only if nothing has written it since the transaction began.
//! Its commit checks that everything it read is as it was, and publishes its
//! writes, under the lock; either may find a conflict, and the transaction
//! runs again.

use crate::ctx::{self, Ctx, Request, Txn, Wake};
use crate::heap::{self, Word};
use crate::parcel::Parcel;
use crate::segments::{self, Down, SendSegment, Up};
use crate::value::{self, Val};
use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use meadow_core::{Prim, desc};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};

/// A thread's stack: reserved, and committed as it is touched.
const THREAD: usize = 256 << 20;
/// `main`'s: non-tail recursion over a long list is ordinary Meadow.
const MAIN: usize = 1 << 30;

/// A thread that is not running.
struct Green {
    ctx: Box<Ctx>,
    co: SendSegment,
}

#[derive(Default)]
struct Task {
    outcome: Option<Result<Arc<Parcel>, String>>,
    waiters: Vec<Box<Green>>,
}

/// A channel: what has been sent and not yet taken, and the threads waiting
/// to take something.
///
/// A lock of its own, not the scheduler's: a send and a receive that pass
/// each other touch this and nothing else, and channels that are busy at the
/// same time never meet. Taken before the scheduler's lock, never after --
/// which is the whole of the ordering that keeps the two apart.
#[derive(Default)]
struct Chan {
    state: Mutex<ChanState>,
}

#[derive(Default)]
struct ChanState {
    messages: VecDeque<Parcel>,
    receivers: VecDeque<Box<Green>>,
}

/// The channel a handle names: its block holds the address, as a `TVar`'s
/// does. One lives as long as the program.
fn chan(v: Val, op: &str) -> &'static Chan {
    let at = match v {
        Val::Ref(w) if heap::is_block(w) && heap::kind(w) == heap::CHANNEL => heap::field(w, 0),
        other => {
            let (w, d) = other.bits();
            crate::fail(&format!(
                "{op}: expected a channel, got {}",
                crate::show::show(w, d)
            ))
        }
    };
    // Safety: the address of a channel made by `ChannelNew`, which lives for
    // the whole run.
    unsafe { &*(at as *const Chan) }
}

fn chan_state(c: &Chan) -> std::sync::MutexGuard<'_, ChanState> {
    c.state.lock().unwrap_or_else(|p| p.into_inner())
}

/// What a `TVar` holds: a value that is a word and nothing more -- an `Int`,
/// a `Bool` -- kept as it is, and anything else as a parcel, since what a
/// `TVar` holds belongs to no thread's heap.
#[derive(Clone)]
enum Held {
    Imm(Word, i64),
    Big(Arc<Parcel>),
}

impl Held {
    fn of(v: Val) -> Held {
        let (w, d) = v.bits();
        if d != desc::REF || !heap::is_block(w) {
            return Held::Imm(w, d);
        }
        Held::Big(Arc::new(parcel(v)))
    }

    /// The value, in the running thread's heap and owned by the caller.
    fn open(&self) -> (Word, i64) {
        match self {
            Held::Imm(w, d) => (*w, *d),
            Held::Big(p) => (p.open(), p.desc()),
        }
    }
}

/// A `TVar`: what it holds, the clock when it was last written, and the
/// threads waiting in `retry` for it to be written (keys of the scheduler's
/// `waiting`).
///
/// A lock of its own, rather than one lock over all of them: a transaction
/// holds it for as long as a word takes to copy, and threads touching
/// different `TVar`s never meet. What touches several locks them in address
/// order, so two of them cannot hold each other up.
///
/// One lives as long as the program: a `TVar` is a handle any thread may
/// hold, and nothing says when the last of them is gone.
struct TVar {
    state: Mutex<(Held, u64)>,
    waiters: Mutex<Vec<u64>>,
}

/// Counts commits that wrote something: what a `TVar`'s version is, and what
/// a transaction remembers as its beginning.
static CLOCK: AtomicU64 = AtomicU64::new(0);
/// Numbers `TVar`s and channels, for printing.
static NEXT_TVAR: AtomicU64 = AtomicU64::new(0);
static NEXT_CHAN: AtomicU64 = AtomicU64::new(0);

/// The cell a handle names. The block holds its address, so reading or
/// writing one is no lookup at all -- and a handle copied to another thread
/// carries the address, which is the same everywhere.
fn cell(v: Val, op: &str) -> &'static TVar {
    let at = match v {
        Val::Ref(w) if heap::is_block(w) && heap::kind(w) == heap::TVAR => heap::field(w, 0),
        other => {
            let (w, d) = other.bits();
            crate::fail(&format!(
                "{op}: expected a TVar, got {}",
                crate::show::show(w, d)
            ))
        }
    };
    // Safety: the address of a cell that lives for the whole run, put in the
    // block by `StmNew`.
    unsafe { &*(at as *const TVar) }
}

fn at(c: &TVar) -> usize {
    c as *const TVar as usize
}

/// Lock `cells`, each once, in address order: two transactions locking any of
/// the same cells take them in the same order, so neither waits on the other.
/// Answers the cells locked, in that order, with their state.
fn lock_all<'a>(cells: &[&'a TVar]) -> (Vec<usize>, Vec<std::sync::MutexGuard<'a, (Held, u64)>>) {
    let mut order: Vec<&'a TVar> = cells.to_vec();
    order.sort_unstable_by_key(|c| at(c));
    order.dedup_by_key(|c| at(c));
    let ats = order.iter().map(|c| at(c)).collect();
    let held = order
        .into_iter()
        .map(|c| c.state.lock().unwrap_or_else(|p| p.into_inner()))
        .collect();
    (ats, held)
}

#[derive(Default)]
struct World {
    ready: VecDeque<Box<Green>>,
    tasks: Vec<Task>,
    /// Threads waiting in `retry`, by a number each `TVar` they read lists.
    waiting: HashMap<u64, Box<Green>>,
    next_wait: u64,
    /// Threads being run by a worker now.
    running: usize,
    /// Whether the workers have started.
    started: bool,
    /// Whether `main` has finished: the program is over, and the workers
    /// stop wherever the threads they are running have got to.
    done: bool,
    /// What `main` answered, and its context, for [`main`] to take.
    answer: Option<(Box<Ctx>, Word)>,
}

static WORLD: LazyLock<Mutex<World>> = LazyLock::new(|| Mutex::new(World::default()));
static CHANGED: Condvar = Condvar::new();
/// Told when `main` has finished, for the thread waiting on it.
static FINISHED: Condvar = Condvar::new();

fn world() -> MutexGuard<'static, World> {
    WORLD.lock().unwrap_or_else(|p| p.into_inner())
}

unsafe extern "C" {
    /// Call the function value `f` with `arg`, under `ev`, and answer what it
    /// returns natively: defined by the emitted module.
    fn meadow_apply(f: Word, arg: Word, ev: Word) -> Word;
}

/// No handlers: what a thread starts with.
fn no_evidence() -> Word {
    (u64::from(value::tag("#evnone")) << 1) | 1
}

fn cx() -> &'static mut Ctx {
    // Safety: the running thread's context, used by this OS thread alone
    // while the thread runs. Read afresh each time: see `crate::ctx`.
    unsafe { &mut *ctx::get() }
}

/// Stacks of threads that have finished, kept to start another with:
/// reserving one and setting its guard pages is what a `spawn` would
/// otherwise spend most of its time on.
static SPARE: Mutex<Vec<DefaultStack>> = Mutex::new(Vec::new());

fn spare() -> MutexGuard<'static, Vec<DefaultStack>> {
    SPARE.lock().unwrap_or_else(|p| p.into_inner())
}

fn coroutine(size: usize, body: impl FnOnce() -> Word + 'static) -> SendSegment {
    let kept = (size == THREAD).then(|| spare().pop()).flatten();
    let stack = kept.unwrap_or_else(|| {
        DefaultStack::new(size)
            .unwrap_or_else(|e| crate::fail(&format!("could not make a thread's stack: {e}")))
    });
    SendSegment(Coroutine::with_stack(
        stack,
        move |y: &Yielder<Down, Up>, _: Down| {
            segments::enter_thread(y);
            let v = body();
            segments::leave_thread();
            v
        },
    ))
}

/// Keep a finished thread's stack to start another with, if there is room:
/// twice a worker each, so that a burst of threads does not hold a great deal
/// of address space for the rest of the run.
fn keep_stack(co: SendSegment) {
    let mut spare = spare();
    if spare.len() < 2 * workers() {
        spare.push(co.0.into_stack());
    }
}

/// Run `entry` as the program's `main` thread, with the scheduler; answer
/// `main`'s context and what it returned, in that context's heap.
pub fn main(entry: impl FnOnce() -> Word + 'static) -> (Box<Ctx>, Word) {
    let co = coroutine(MAIN, entry);
    let mut w = world();
    w.tasks.push(Task::default());
    w.ready.push_back(Box::new(Green {
        ctx: Ctx::new(0),
        co,
    }));
    start_workers(&mut w);
    // This thread waits rather than running threads itself: whichever worker
    // `main` finishes on, the program is over then, and a worker still inside
    // a thread that never waits must not hold the answer up.
    while w.answer.is_none() {
        w = FINISHED.wait(w).unwrap_or_else(|p| p.into_inner());
    }
    w.answer.take().expect("main finished")
}

/// A worker: run ready threads until `main` has finished.
fn work() {
    loop {
        let mut g = {
            let mut w = world();
            loop {
                if w.done {
                    return;
                }
                if let Some(g) = w.ready.pop_front() {
                    w.running += 1;
                    break g;
                }
                if w.running == 0 {
                    crate::fail(
                        "deadlock: every thread is waiting, on a channel nobody will send to \
                         or on another waiting thread",
                    )
                }
                w = CHANGED.wait(w).unwrap_or_else(|p| p.into_inner());
            }
        };
        ctx::set(&mut *g.ctx);
        let went = g.co.0.resume(Down::Wake);
        match went {
            CoroutineResult::Return(v) if g.ctx.tid == 0 => {
                ctx::set(std::ptr::null_mut());
                let mut w = world();
                w.running -= 1;
                w.done = true;
                w.answer = Some((g.ctx, v));
                CHANGED.notify_all();
                FINISHED.notify_all();
                return;
            }
            CoroutineResult::Return(v) => {
                ctx::set(std::ptr::null_mut());
                // Safety: what a thread's body returns -- see `ThreadSpawn`.
                let out = *unsafe { Box::from_raw(v as *mut Result<Parcel, String>) };
                let tid = g.ctx.tid;
                keep_stack(g.co);
                drop(g.ctx);
                let mut w = world();
                w.running -= 1;
                finish(&mut w, tid, out);
            }
            CoroutineResult::Yield(Up::Park) => {
                let request = g.ctx.request.take();
                ctx::set(std::ptr::null_mut());
                // Not under the scheduler's lock: filing may have to take a
                // channel's or a `TVar`'s, and those come first.
                file(g, request);
                let mut w = world();
                w.running -= 1;
                CHANGED.notify_all();
            }
            CoroutineResult::Yield(Up::Detach { .. }) => segments::escaped(),
        }
    }
}

/// How many OS threads run green threads: `MEADOW_THREADS` if it is set, or
/// one per core -- as `meadow-glade` reads it.
fn workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(count_workers)
}

fn count_workers() -> usize {
    std::env::var("MEADOW_THREADS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// Start the workers and the timer, once: the thread that called [`main`]
/// waits for the answer instead of running threads itself.
fn start_workers(w: &mut World) {
    if w.started {
        return;
    }
    w.started = true;
    for i in 0..workers() {
        let _ = std::thread::Builder::new()
            .name(format!("meadow worker {i}"))
            .stack_size(8 << 20)
            .spawn(work);
    }
    let _ = std::thread::Builder::new()
        .name("meadow timer".into())
        .spawn(tick);
}

/// How long a thread may keep a worker while others wait.
const SLICE: std::time::Duration = std::time::Duration::from_millis(5);

/// The flag the emitted code reads at its safe points: see
/// `meadow_llvm::emit::safe_point`. Set only while a thread is waiting for a
/// core, so that a program whose threads are all running reads a byte and
/// carries on.
#[unsafe(no_mangle)]
pub static meadow_preempt: AtomicU8 = AtomicU8::new(0);

/// The timer: while the program runs, ask the running threads to give way
/// whenever another is ready and waiting for a core.
fn tick() {
    loop {
        std::thread::sleep(SLICE);
        let w = world();
        if w.done {
            return;
        }
        if !w.ready.is_empty() {
            meadow_preempt.store(1, Ordering::Relaxed);
        }
    }
}

/// A safe point, with the flag set: the thread has had its turn. It goes to
/// the back of the queue, and the next one runs.
///
/// The flag is cleared here rather than by the timer, so that one thread
/// giving way answers one timer tick: the others carry on until the next.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_preempted() {
    meadow_preempt.store(0, Ordering::Relaxed);
    // Nothing waiting, or nowhere to suspend to (the runtime's own calls into
    // the program, which run to the end): carry on.
    if world().ready.is_empty() || segments::innermost().is_none() {
        return;
    }
    park(Request::Yield);
}

fn wake(w: &mut World, mut g: Box<Green>, with: Wake) {
    g.ctx.wake = with;
    w.ready.push_back(g);
    CHANGED.notify_one();
}

/// Thread `tid` is done: keep its outcome for `await`, and wake whoever
/// awaits it.
fn finish(w: &mut World, tid: usize, out: Result<Parcel, String>) {
    let out = out.map(Arc::new);
    let waiters = std::mem::take(&mut w.tasks[tid].waiters);
    w.tasks[tid].outcome = Some(out.clone());
    for g in waiters {
        let with = match &out {
            Ok(p) => Wake::Value((**p).clone()),
            Err(m) => Wake::Fail(m.clone()),
        };
        wake(w, g, with);
    }
    CHANGED.notify_all();
}

/// A thread stopped to wait for `request`: put it where what it waits for
/// will find it -- or back to run, if that is already there.
fn file(mut g: Box<Green>, request: Option<Request>) {
    match request {
        None | Some(Request::Yield) => ready(g, Wake::Nothing),
        Some(Request::Await(t)) => {
            let mut w = world();
            match &w.tasks[t].outcome {
                Some(Ok(p)) => {
                    let p = (**p).clone();
                    wake(&mut w, g, Wake::Value(p));
                }
                Some(Err(m)) => {
                    let m = m.clone();
                    wake(&mut w, g, Wake::Fail(m));
                }
                None => w.tasks[t].waiters.push(g),
            }
        }
        Some(Request::Receive(c)) => {
            // Safety: the address of a channel that lives for the whole run.
            let c = unsafe { &*(c as *const Chan) };
            let mut state = chan_state(c);
            match state.messages.pop_front() {
                Some(p) => {
                    drop(state);
                    ready(g, Wake::Value(p));
                }
                None => state.receivers.push_back(g),
            }
        }
        Some(Request::Wait(reads)) => {
            // The cells this read are locked while it is checked and the
            // thread written down, so a commit either changes one of them
            // before the check or finds the thread among the waiters.
            let cells: Vec<&TVar> = reads
                .iter()
                // Safety: addresses of cells that live for the whole run.
                .map(|(id, _)| unsafe { &*(*id as *const TVar) })
                .collect();
            let (ats, held) = lock_all(&cells);
            let changed = reads
                .iter()
                .any(|(id, seen)| held[ats.binary_search(id).expect("locked")].1 != *seen);
            if changed {
                drop(held);
                ready(g, Wake::Nothing);
                return;
            }
            let mut w = world();
            let key = w.next_wait;
            w.next_wait += 1;
            for c in &cells {
                c.waiters
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(key);
            }
            drop(held);
            w.waiting.insert(key, g);
        }
        Some(Request::Failed(msg)) => {
            let tid = g.ctx.tid;
            // Never resumed, and not unwound.
            // Safety: nothing will resume it.
            unsafe { g.co.0.force_reset() };
            keep_stack(g.co);
            drop(g.ctx);
            let mut w = world();
            finish(&mut w, tid, Err(msg));
        }
    }
}

/// Put a thread back on the queue, with what it was woken with: the
/// scheduler's lock taken here and nowhere above it.
fn ready(g: Box<Green>, with: Wake) {
    let mut w = world();
    wake(&mut w, g, with);
}

/// Suspend the running thread -- every segment of it -- having said why in
/// its context; answer what it is woken with.
fn park(request: Request) -> Wake {
    cx().request = Some(request);
    let Some(y) = segments::innermost() else {
        crate::fail("a thread operation ran outside every thread")
    };
    match segments::suspend(y, Up::Park) {
        Down::Wake => std::mem::replace(&mut cx().wake, Wake::Nothing),
        Down::Start | Down::Resume(_) => unreachable!("a parked thread is woken"),
    }
}

/// A failure in a thread other than `main` fails that thread: `await` fails
/// with the same message, and nothing else is affected. Answers whether it
/// was one; for `main`, or outside every thread, the program stops.
pub fn fail_thread(msg: &str) -> bool {
    if !ctx::present() || cx().tid == 0 || segments::innermost().is_none() {
        return false;
    }
    loop {
        park(Request::Failed(msg.to_string()));
    }
}

fn handle(v: Val, kind: u64, what: &str, op: &str) -> usize {
    match v {
        Val::Ref(w) if heap::is_block(w) && heap::kind(w) == kind => heap::meta(w) as usize,
        other => {
            let (w, d) = other.bits();
            crate::fail(&format!(
                "{op}: expected {what}, got {}",
                crate::show::show(w, d)
            ))
        }
    }
}

fn outside(op: &str) -> ! {
    crate::fail(&meadow_core::stm::outside(op))
}

fn parcel(v: Val) -> Parcel {
    let (w, d) = v.bits();
    Parcel::of(w, d).unwrap_or_else(|e| crate::fail(&e))
}

/// What a waiting operation was woken with, as a value in this heap.
fn delivered(wake: Wake) -> Word {
    match wake {
        Wake::Value(p) => p.open(),
        Wake::Fail(m) => crate::fail(&m),
        Wake::Nothing => 0,
    }
}

/// The thread and transaction primitives. Arguments are borrowed, the
/// result owned.
pub fn prim(p: Prim, args: &[Val]) -> Word {
    use Prim::*;
    let arg = |i: usize| args[i];
    match p {
        ThreadSpawn => {
            let body = parcel(arg(0));
            let answer = match args.get(1) {
                Some(Val::Int(d)) => *d,
                _ => desc::ANY,
            };
            let tid = {
                let mut w = world();
                w.tasks.push(Task::default());
                w.tasks.len() - 1
            };
            let co = coroutine(THREAD, move || {
                let f = body.open();
                // Safety: a function value of the emitted module, owned by
                // the call.
                let v = unsafe { meadow_apply(f, 0, no_evidence()) };
                let out = Parcel::of(v, answer);
                heap::erase(v, answer);
                Box::into_raw(Box::new(out)) as Word
            });
            let mut w = world();
            let g = Box::new(Green {
                ctx: Ctx::new(tid),
                co,
            });
            wake(&mut w, g, Wake::Nothing);
            heap::build(heap::TASK, tid as u32, &[0], &[desc::INT])
        }
        ThreadAwait => {
            let t = handle(arg(0), heap::TASK, "a thread", "await");
            let ready = {
                let w = world();
                match w.tasks.get(t).map(|t| &t.outcome) {
                    None => Some(Wake::Fail(format!("await: there is no thread {t}"))),
                    Some(Some(Ok(p))) => Some(Wake::Value((**p).clone())),
                    Some(Some(Err(m))) => Some(Wake::Fail(m.clone())),
                    Some(None) => None,
                }
            };
            delivered(ready.unwrap_or_else(|| park(Request::Await(t))))
        }
        ThreadYield => {
            park(Request::Yield);
            0
        }
        ChannelNew => {
            let c: &'static Chan = Box::leak(Box::new(Chan::default()));
            let id = NEXT_CHAN.fetch_add(1, Ordering::Relaxed) as u32;
            heap::build(heap::CHANNEL, id, &[c as *const Chan as Word], &[desc::INT])
        }
        ChannelSend => {
            let c = chan(arg(0), "send");
            let message = parcel(arg(1));
            // The channel's lock, and the scheduler's only if somebody was
            // waiting -- and never both at once.
            let waiting = {
                let mut state = chan_state(c);
                match state.receivers.pop_front() {
                    Some(g) => Some(g),
                    None => {
                        state.messages.push_back(message);
                        return 0;
                    }
                }
            };
            if let Some(g) = waiting {
                ready(g, Wake::Value(message));
            }
            0
        }
        ChannelReceive => {
            let c = chan(arg(0), "receive");
            let got = chan_state(c).messages.pop_front();
            match got {
                Some(p) => p.open(),
                None => delivered(park(Request::Receive(c as *const Chan as usize))),
            }
        }

        StmNew => {
            // The cell lives for the whole run; the handle carries its
            // address, so no thread has to look it up.
            let held = Held::of(arg(0));
            let cell: &'static TVar = Box::leak(Box::new(TVar {
                state: Mutex::new((held, 0)),
                waiters: Mutex::new(Vec::new()),
            }));
            let id = NEXT_TVAR.fetch_add(1, Ordering::Relaxed) as u32;
            heap::build(heap::TVAR, id, &[at(cell) as Word], &[desc::INT])
        }
        StmRead => {
            let c = cell(arg(0), "readTVar");
            let Some(txn) = cx().txn.as_mut() else {
                outside("readTVar")
            };
            let written = txn
                .writes
                .iter()
                .rev()
                .flat_map(|level| level.iter().rev())
                .find(|(t, _, _)| *t == at(c))
                .map(|(_, x, d)| (*x, *d));
            if let Some((x, d)) = written {
                heap::share(x, d);
                return value::data("Maybe.Just", &[value::val(x, d)]);
            }
            let start = txn.start;
            let seen = {
                let state = c.state.lock().unwrap_or_else(|p| p.into_inner());
                (state.1 <= start).then(|| (state.0.clone(), state.1))
            };
            // Written since this transaction began: it runs again.
            let Some((held, version)) = seen else {
                return value::data("Maybe.None", &[]);
            };
            let txn = cx().txn.as_mut().expect("a transaction");
            if !txn.reads.iter().any(|(t, _)| *t == at(c)) {
                txn.reads.push((at(c), version));
            }
            let (x, d) = held.open();
            value::data("Maybe.Just", &[value::val(x, d)])
        }
        StmWrite => {
            let c = cell(arg(0), "writeTVar");
            let (x, d) = arg(1).bits();
            let Some(level) = cx().txn.as_mut().and_then(|t| t.writes.last_mut()) else {
                outside("writeTVar")
            };
            heap::share(x, d);
            let old = match level.iter_mut().find(|(t, _, _)| *t == at(c)) {
                Some(slot) => Some(std::mem::replace(slot, (at(c), x, d))),
                None => {
                    level.push((at(c), x, d));
                    None
                }
            };
            if let Some((_, ox, od)) = old {
                heap::erase(ox, od);
            }
            0
        }
        StmBegin => {
            let start = CLOCK.load(Ordering::Acquire);
            let last = cx().txn.replace(Txn {
                start,
                reads: Vec::new(),
                writes: vec![Vec::new()],
            });
            if let Some(t) = last {
                drop_writes(t.writes);
            }
            0
        }
        StmNest => {
            let Some(t) = cx().txn.as_mut() else {
                outside("orElse")
            };
            t.writes.push(Vec::new());
            0
        }
        StmMerge => {
            let Some(t) = cx().txn.as_mut() else {
                outside("orElse")
            };
            let (Some(top), true) = (t.writes.pop(), !t.writes.is_empty()) else {
                outside("orElse")
            };
            let below = t.writes.last_mut().expect("a level below");
            let mut replaced = Vec::new();
            for (id, x, d) in top {
                match below.iter_mut().find(|(t, _, _)| *t == id) {
                    Some(slot) => replaced.push(std::mem::replace(slot, (id, x, d))),
                    None => below.push((id, x, d)),
                }
            }
            drop_writes(vec![replaced]);
            0
        }
        StmRollback => {
            let Some(top) = cx().txn.as_mut().and_then(|t| t.writes.pop()) else {
                outside("orElse")
            };
            drop_writes(vec![top]);
            0
        }
        StmCommit => {
            let Some(txn) = cx().txn.take() else {
                outside("atomically")
            };
            // The last write to each, lifted out of this thread's heap before
            // anything is locked.
            let mut last: Vec<(usize, Held)> = Vec::new();
            for level in &txn.writes {
                for &(id, x, d) in level {
                    let held = Held::of(value::val(x, d));
                    match last.iter_mut().find(|(t, _)| *t == id) {
                        Some(slot) => slot.1 = held,
                        None => last.push((id, held)),
                    }
                }
            }
            // What it read must be as it was, and what it wrote goes out, as
            // one step: the cells it touched are locked for it, in address
            // order, and nothing else can be committing them meanwhile.
            let touched: Vec<&TVar> = txn
                .reads
                .iter()
                .map(|(id, _)| *id)
                .chain(last.iter().map(|(id, _)| *id))
                // Safety: addresses of cells that live for the whole run.
                .map(|id| unsafe { &*(id as *const TVar) })
                .collect();
            let (ats, mut held) = lock_all(&touched);
            let slot = |id: usize| ats.binary_search(&id).expect("locked");
            let valid = txn
                .reads
                .iter()
                .all(|(id, seen)| held[slot(*id)].1 == *seen);
            let mut keys = Vec::new();
            if valid && !last.is_empty() {
                let now = CLOCK.fetch_add(1, Ordering::AcqRel) + 1;
                for (id, value) in last {
                    let state = &mut held[slot(id)];
                    **state = (value, now);
                }
            }
            drop(held);
            if valid {
                for c in &touched {
                    let mut waiters = c.waiters.lock().unwrap_or_else(|p| p.into_inner());
                    keys.append(&mut waiters);
                }
            }
            wake_waiting(keys);
            drop_writes(txn.writes);
            u64::from(valid)
        }
        StmWait => {
            let Some(txn) = cx().txn.take() else {
                outside("retry")
            };
            drop_writes(txn.writes);
            if txn.reads.is_empty() {
                crate::fail("deadlock: a transaction retried having read nothing")
            }
            park(Request::Wait(txn.reads));
            0
        }
        _ => unreachable!("{p:?} is not a thread primitive"),
    }
}

/// Wake the threads whose `retry` was waiting on a `TVar` just written.
fn wake_waiting(keys: Vec<u64>) {
    if keys.is_empty() {
        return;
    }
    let mut w = world();
    for k in keys {
        if let Some(g) = w.waiting.remove(&k) {
            wake(&mut w, g, Wake::Nothing);
        }
    }
}

fn drop_writes(levels: Vec<Vec<(usize, Word, i64)>>) {
    for (_, x, d) in levels.into_iter().flatten() {
        heap::erase(x, d);
    }
}
