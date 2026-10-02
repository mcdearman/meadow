//! What the engines share about software transactional memory.
//!
//! Most of STM is Meadow: `Std.Stm`'s `atomically`, `retry` and `orElse` are
//! handlers, and a transaction is a function whose type allows it only `Stm` --
//! so it can be run again, as often as it takes, without anyone noticing.
//! What the engines provide is the part a handler cannot: `TVar`s every thread
//! can see, a transaction's log of what it read and wrote, and a commit that
//! checks the log and publishes the writes as one step.
//!
//! # The algorithm
//!
//! A global clock counts commits. A transaction notes the clock when it starts,
//! and every `TVar` carries the clock value of the commit that last wrote it.
//! Reading a `TVar` written *after* the transaction started is a conflict: what
//! it read so far and what it would read now might not belong to one moment, so
//! it stops and runs again rather than compute with an inconsistent view. So a
//! transaction never sees anything a serial execution could not have shown it.
//! Writes are kept in the log, where the transaction's own reads find them.
//! Committing checks that nothing read has been written since, and then writes
//! everything, under one lock.
//!
//! `retry` waits until something the transaction read is written, then runs it
//! again. `orElse` runs its first branch with a nested log; if that branch
//! retries, its writes are dropped -- its reads are kept, since they are what a
//! retry of the whole would wait on -- and the second runs instead.
//!
//! # What a `TVar` holds
//!
//! What a `Compact` does, and for the same reason: on the bytecode VM a `TVar`'s
//! value lives in a shared region, where every thread reads it in place. So a
//! `Ref`, a mutable array, a continuation and a function are refused.

/// The error for a value a `TVar` cannot hold.
pub fn unstorable(what: &str) -> String {
    format!("stm: a TVar cannot hold {what}; it holds only immutable data")
}

/// The error for a transaction operation outside `atomically`.
pub fn outside(op: &str) -> String {
    format!("stm: `{op}` has no transaction to belong to; use it inside `atomically`")
}
