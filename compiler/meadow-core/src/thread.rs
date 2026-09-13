//! What the engines share about green threads.
//!
//! A green thread has a heap of its own and never touches another's. The only
//! ways a value crosses between threads are the function a thread is started
//! with, a message on a channel, and the result `await` hands back -- and each is
//! a copy. On the bytecode VM it is literally one: the value is lifted out of
//! one heap and rebuilt in the other, by the OS thread running the receiver. The
//! CEK machine shares immutable values instead of copying them, which no
//! program can tell apart from a copy -- provided nothing mutable crosses.
//!
//! So what may cross is decided here, and every engine refuses the same things
//! in the same words:
//!
//! * a `Ref` or a mutable array, which would be one cell reachable from two
//!   heaps -- shared mutable state, which is exactly what threads with their own
//!   heaps rule out;
//! * a continuation, which is a piece of the thread that captured it.
//!
//! A `Compact` crosses without being copied at all: its region belongs to no
//! heap, and every thread reads the same one.
//!
//! Functions may cross -- `spawn` could not work otherwise -- carrying copies of
//! exactly what they capture. The engines agree on what that is: the VM's
//! closures hold exactly their free variables, and the CEK machine looks at a
//! closure's free variables rather than its whole environment.
//!
//! Scheduling is not shared. The VM runs threads on every core, preempting each
//! after a slice of instructions; the CEK machine runs them one at a time, in
//! order. Programs whose answer depends on interleaving will differ, as they
//! would between two runs on the VM.

/// The error for a value that cannot be passed to another thread.
pub fn unsendable(what: &str) -> String {
    format!("thread: cannot pass {what} to another thread; threads share only immutable data")
}

/// The error when every thread is waiting and none can wake another.
pub const DEADLOCK: &str =
    "deadlock: every thread is waiting on a channel or a thread that will never answer";
