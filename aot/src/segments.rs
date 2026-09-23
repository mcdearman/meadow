//! **Stack segments**: how an effect handler that captures its continuation
//! works on the native stack. See `docs/AOT.md`, "Effects: stack segments".
//!
//! Effects arrive as evidence passing, and a general clause is four
//! primitives: `Enter` where a `handle` begins, `Detach` where an operation
//! takes the continuation up to its handler, `Reattach` where a resumption
//! puts it back, and the one-shot flag. The emitted code hands each of the
//! first three the code that follows it, packed as a closure of one argument,
//! and returns natively with whatever the runtime answers:
//!
//! - [`meadow_enter`] runs its code on a **new segment** -- a coroutine with a
//!   stack of its own -- and answers what that code finally returns natively.
//! - [`meadow_detach`], called on the body's segment, **suspends** it and runs
//!   its code (building the resumption, calling the clause) on the handler's.
//!   A handler further out than the innermost segment suspends every segment
//!   in between: they are all part of what the continuation is.
//! - [`meadow_reattach`] resumes the suspended segments and runs its code on
//!   the innermost -- where invoking the operation's continuation, which
//!   returns natively, returns into the frames the segment kept.
//!
//! The switching is `corosensei`'s: guard pages, and on Windows the thread
//! information block kept in step, so stack probes work on a segment as on a
//! thread's own stack.
//!
//! A continuation never resumed is discarded when its stack object is
//! erased, without unwinding: whatever its frames held is not erased -- as a
//! cycle is not.

use crate::heap::{self, Word};
use crate::value::Val;
use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use meadow_core::Prim;
use meadow_core::desc;

/// A segment's stack: reserved, and committed as it is touched.
const SEGMENT: usize = 256 << 20;

/// What a segment says when it suspends: an operation wants the continuation
/// up to the handler whose target is `target`, and `then` is what runs on
/// that handler's segment, given the continuation.
pub(crate) enum Up {
    Detach {
        target: Word,
        then: Word,
    },
    /// A thread waits (`crate::sched`): every segment of it suspends, out to
    /// the scheduler.
    Park,
}

/// What a segment is given when it runs.
pub(crate) enum Down {
    Start,
    /// Resumed: run this, of one argument, there.
    Resume(Word),
    /// A waiting thread woken: what it was woken with is in its context.
    Wake,
}

pub(crate) type Segment = Coroutine<Down, Up, Word, DefaultStack>;

/// A segment, to be handed between OS threads with the thread it belongs to.
pub(crate) struct SendSegment(pub Segment);

// Safety: everything on a segment's stack is the emitted code's frames and
// this runtime's, which keep nothing tied to an OS thread across a switch --
// what they keep per thread is in `crate::ctx`, read afresh after every one.
unsafe impl Send for SendSegment {}

fn cx() -> &'static mut crate::ctx::Ctx {
    // Safety: the running thread's context, used by this OS thread alone
    // while the thread runs; callers borrow one field at a time.
    unsafe { &mut *crate::ctx::get() }
}

unsafe extern "C" {
    /// Method 0 of the closure `obj`, with `arg`: defined by the emitted
    /// module, since the methods are in its calling convention.
    fn meadow_invoke1(obj: Word, arg: Word) -> Word;
}

fn run(closure: Word, arg: Word) -> Word {
    // Safety: a closure the emitted code packed, whose method takes one
    // argument; the call owns both.
    unsafe { meadow_invoke1(closure, arg) }
}

/// Suspend the segment running now, through `y`, saying `up`; answer what it
/// is resumed with -- perhaps on another OS thread.
pub(crate) fn suspend(y: *const Yielder<Down, Up>, up: Up) -> Down {
    cx().running.pop();
    // Safety: the yielder of the innermost segment running, which is this one.
    let down = unsafe { &*y }.suspend(up);
    cx().running.push(y as *const ());
    down
}

pub(crate) fn innermost() -> Option<*const Yielder<Down, Up>> {
    cx().running.last().map(|y| *y as *const Yielder<Down, Up>)
}

pub(crate) fn escaped() -> ! {
    crate::fail(
        "an effect was performed after its handler had finished: the function \
         performing it escaped the `handle` that answers it",
    )
}

/// How many finished segments' stacks a thread keeps to use again.
const POOL: usize = 4;

/// A stack for a segment: one kept from a segment that has finished, or a
/// new one.
pub(crate) fn take_stack(size: usize) -> DefaultStack {
    if size == SEGMENT
        && let Some(s) = cx().stacks.pop()
    {
        return s;
    }
    DefaultStack::new(size)
        .unwrap_or_else(|e| crate::fail(&format!("could not make a stack segment: {e}")))
}

/// Keep `stack` for the next segment, if there is room.
fn give_stack(stack: DefaultStack) {
    let pool = &mut cx().stacks;
    if pool.len() < POOL {
        pool.push(stack);
    }
}

/// `handle`: run `body` -- the code after `Enter`, of one argument -- on a
/// segment of its own, for the handler whose target is the `Ref` `target`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_enter(target: Word, body: Word) -> Word {
    let handler = {
        let c = cx();
        let h = c.next_handler;
        c.next_handler += 1;
        h
    };
    set_meta(target, handler);
    // Where the value of the whole `handle` goes -- the continuation the
    // lowering puts in the target -- is taken out, and `0` left in its place:
    // "return natively". So when that value is produced, whichever stack it is
    // produced on returns, this segment ends, and its stack goes back. Without
    // this the segment would run the rest of the program, since in AxCut the
    // code after a `handle` is the continuation the code inside it is given,
    // and handlers in a loop would nest one stack deep for each turn.
    let after = outer(target);
    let segment = Coroutine::with_stack(
        take_stack(SEGMENT),
        move |y: &Yielder<Down, Up>, _: Down| {
            cx().running.push(y as *const _ as *const ());
            let v = run(body, 0);
            cx().running.pop();
            v
        },
    );
    let v = drive(segment, Down::Start, handler);
    // The handle is done: carry on where its value was to go, on this stack.
    match after {
        0 => v,
        k => run(k, v),
    }
}

/// Take the continuation the `handle`'s value is to go to out of `target`,
/// leaving a native return in its place. Answers `0` when there is none to
/// take, which is itself a native return.
fn outer(target: Word) -> Word {
    if !heap::is_block(target) || heap::kind(target) != heap::CELL || heap::len(target) == 0 {
        return 0;
    }
    let k = heap::field(target, 0);
    heap::set_field(target, 0, 0, desc::REF);
    k
}

/// Run `segment`, for `handler`, until it finishes -- answering what it
/// returns -- or suspends for this handler, when `then` runs here with the
/// continuation. Suspending for a handler further out, or to wait, suspends
/// the segment this runs on too, and resumes this one when that is resumed.
fn drive(mut segment: Segment, mut down: Down, handler: u32) -> Word {
    loop {
        match segment.resume(down) {
            CoroutineResult::Return(v) => {
                give_stack(segment.into_stack());
                return v;
            }
            CoroutineResult::Yield(Up::Detach { target, then }) => {
                if meta_of(target) == handler {
                    let id = {
                        let s = &mut cx().suspended;
                        s.push(Some((SendSegment(segment), handler)));
                        s.len() - 1
                    };
                    let k =
                        heap::build(heap::STACK, id as u32, &[u64::from(handler)], &[desc::INT]);
                    return run(then, k);
                }
                let Some(y) = innermost() else { escaped() };
                down = suspend(y, Up::Detach { target, then });
            }
            CoroutineResult::Yield(Up::Park) => {
                let Some(y) = innermost() else {
                    crate::fail("a thread waited outside every thread")
                };
                down = suspend(y, Up::Park);
            }
        }
    }
}

/// An operation: suspend the segments up to the handler whose target is
/// `target`, and run `then` on that handler's with the continuation. Answers,
/// once resumed, what the resumption's code returns.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_detach(target: Word, then: Word) -> Word {
    let Some(y) = innermost() else { escaped() };
    match suspend(y, Up::Detach { target, then }) {
        Down::Resume(code) => run(code, 0),
        Down::Start | Down::Wake => unreachable!("a segment is resumed with code"),
    }
}

/// A resumption: resume the segments the stack object `k` holds, and run
/// `code` there.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_reattach(k: Word, code: Word) -> Word {
    let taken = (heap::is_block(k) && heap::kind(k) == heap::STACK)
        .then(|| {
            let id = heap::meta(k) as usize;
            cx().suspended.get_mut(id).and_then(Option::take)
        })
        .flatten();
    let Some((SendSegment(segment), handler)) = taken else {
        crate::fail("continuation resumed more than once")
    };
    drive(segment, Down::Resume(code), handler)
}

/// The stack object numbered `id` was erased: its segments will never be
/// resumed. Discarded without unwinding.
pub fn discard(id: u32) {
    let seg = cx().suspended.get_mut(id as usize).and_then(Option::take);
    if let Some((SendSegment(mut segment), _)) = seg {
        // Safety: nothing will resume it, and what its frames held is left
        // alone rather than unwound -- see the module docs.
        unsafe { segment.force_reset() };
        give_stack(segment.into_stack());
    }
}

/// A thread's own segment begins: operations and waits suspend through `y`.
pub(crate) fn enter_thread(y: &Yielder<Down, Up>) {
    cx().running.push(y as *const _ as *const ());
}

pub(crate) fn leave_thread() {
    cx().running.pop();
}

fn set_meta(v: Word, m: u32) {
    if heap::is_block(v) {
        heap::set_word(
            v,
            1,
            (heap::word(v, 1) & 0xFFFF_FFFF) | (u64::from(m) << 32),
        );
    }
}

fn meta_of(v: Word) -> u32 {
    if heap::is_block(v) { heap::meta(v) } else { 0 }
}

/// `Enter`, `Detach` and `Reattach` through the generic entry: the emitted
/// code calls the functions above instead, handing them what follows.
pub fn prim(p: Prim, _args: &[Val]) -> Word {
    crate::fail(&format!(
        "{p:?} reached the runtime without the code that follows it"
    ))
}
