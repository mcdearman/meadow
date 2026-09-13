//! What the engines share about compact regions.
//!
//! A region is the bytecode VM's: blocks of heap slots its collector neither
//! copies nor scans. The CEK and sequent machines reference-count, so for them
//! a `Compact` is a wrapper and nothing moves -- but they must refuse the same
//! values and fail with the same words, or `compact` would mean different
//! things on different engines. This is where those words live.
//!
//! The rule is the VM's, since it is the VM whose collector depends on it: a
//! region is closed and immutable, so nothing that can be written after it is
//! built may go in -- a `Ref`, a mutable array -- and neither may a function,
//! whose captures differ between engines (the CEK closes over a whole
//! environment, the others over exactly what the body uses).

/// The error for a value that reaches something a region cannot hold.
pub fn uncompactable(what: &str) -> String {
    format!("compact: cannot compact {what}; a compact region holds only immutable data")
}

/// Bytes one VM heap slot takes, for the engines that can only estimate what
/// `compactSize` would say: they count slots the way the VM lays objects out
/// and multiply.
pub const SLOT_BYTES: usize = 16;
