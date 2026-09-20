//! **Where a program spends itself**: sampling, and what the samples are.
//!
//! # The machine has no stack, and that is the interesting part
//!
//! A profile wants a *stack* at each sample, and [`crate::Vm`] has none -- see
//! `docs/RUNTIME.md`. What it has instead is a chain of continuation objects:
//! the function running holds the one it will answer, that one captured the one
//! *its* caller will answer, and so on down to the `halt` at the bottom.
//!
//! So the stack is there; it is on the heap rather than below the stack
//! pointer. Walking the chain is walking the call stack, and each link's
//! method-0 entry pc is where control goes when the function under it returns
//! -- a return address, which is exactly the frame a profile wants.
//!
//! Three things make that walk possible, and all three already existed for the
//! debugger:
//!
//! * [`meadow_bytecode::DebugInfo::env_of`] and `envs` say which name is in
//!   which register at any pc;
//! * `returns` says which names are a function's *own* return continuation, as
//!   against `continuations`, the ones it makes for calls of its own -- the
//!   difference between where this function goes and where it sends others;
//! * a closure keeps its captures in fields `0..len`, in the same order its
//!   method's block takes them, so the register a name sits in at the method's
//!   entry is the field it was captured into.
//!
//! # Sampling where a block is entered
//!
//! [`crate::Vm::advance`] is consulted at each block the machine enters, by the
//! interpreter, the JIT and the ahead-of-time backend alike. Sampling there
//! costs one branch, needs no signal handler and no access to another thread's
//! machine, and gives the same answer twice for the same program -- which a
//! profile that is used to decide things ought to.
//!
//! What it measures is instructions, not seconds. Time spent in the collector
//! is not here; `--gc-stats` is where that lives.

use meadow_bytecode::{DebugInfo, Pc, Reg};
use std::collections::HashMap;

/// Block entries between samples, unless asked for another.
pub const EVERY: u64 = 1000;

/// Frames to walk, unless asked for another. Deep enough for real recursion to
/// show, bounded because a chain can be as long as a program is recursive.
pub const DEPTH: usize = 64;

/// Samples, by the stack they landed on.
#[derive(Debug, Clone)]
pub struct Profile {
    every: u64,
    depth: usize,
    countdown: u64,
    /// Innermost frame first, as a profile is usually read.
    stacks: HashMap<Vec<Pc>, u64>,
    taken: u64,
    /// Samples that got no further than the frame they were taken in, because
    /// the program carries no debug info to walk by.
    shallow: u64,
}

impl Profile {
    pub fn new(every: u64, depth: usize) -> Profile {
        Profile {
            every: every.max(1),
            depth: depth.max(1),
            countdown: every.max(1),
            stacks: HashMap::new(),
            taken: 0,
            shallow: 0,
        }
    }

    /// How deep a sample walks.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Is this block entry a sample? Counts down either way.
    pub fn due(&mut self) -> bool {
        self.countdown -= 1;
        if self.countdown > 0 {
            return false;
        }
        self.countdown = self.every;
        true
    }

    pub fn record(&mut self, stack: Vec<Pc>) {
        self.taken += 1;
        if stack.len() <= 1 {
            self.shallow += 1;
        }
        *self.stacks.entry(stack).or_insert(0) += 1;
    }

    pub fn taken(&self) -> u64 {
        self.taken
    }

    /// Samples that saw only the frame they were taken in. A profile that is
    /// nearly all of these was taken of a program built without debug info,
    /// and is a list of places rather than a tree.
    pub fn shallow(&self) -> u64 {
        self.shallow
    }

    /// Every stack and its count, innermost frame first.
    pub fn stacks(&self) -> impl Iterator<Item = (&[Pc], u64)> {
        self.stacks.iter().map(|(s, n)| (s.as_slice(), *n))
    }

    /// Take in another thread's samples. Each green thread profiles its own
    /// machine, and a run's profile is all of them.
    pub fn merge(&mut self, other: &Profile) {
        self.taken += other.taken;
        self.shallow += other.shallow;
        for (stack, n) in &other.stacks {
            *self.stacks.entry(stack.clone()).or_insert(0) += n;
        }
    }
}

/// The register holding the function's own return continuation at `pc`.
///
/// `returns` less `continuations`: the first is where *this* function goes when
/// it is done, the second are the ones it hands to calls it makes. Only the
/// first is a frame below this one.
pub fn continuation_reg(debug: &DebugInfo, pc: usize) -> Option<Reg> {
    let which = *debug.env_of.get(pc)? as usize;
    debug
        .envs
        .get(which)?
        .iter()
        .find(|(name, _)| debug.returns.contains(name) && !debug.continuations.contains(name))
        .map(|(_, r)| *r)
}

/// Where a program's allocation comes from, by the instruction that asked for
/// it.
///
/// Only under `--features profile-alloc`. The accounting is two map lookups on
/// the bump allocator, which is the fast path a generational collector exists
/// to have, so it is not something to carry when nobody is looking.
///
/// Sites rather than stacks: an allocation happens inside [`crate::heap::Heap`],
/// which has no machine to walk a chain of continuations with. The instruction
/// is enough to name the function, which is the question people ask of an
/// allocation profile -- *what is making all this garbage* -- and the sampling
/// profile answers the rest.
#[cfg(feature = "profile-alloc")]
#[derive(Debug, Clone, Default)]
pub struct Sites {
    by_pc: HashMap<Pc, Allocated>,
}

/// What one instruction allocated.
#[cfg(feature = "profile-alloc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Allocated {
    pub objects: u64,
    pub slots: u64,
}

#[cfg(feature = "profile-alloc")]
impl Sites {
    /// One object of `slots` words, asked for at `pc`.
    pub fn note(&mut self, pc: Pc, slots: usize) {
        let e = self.by_pc.entry(pc).or_default();
        e.objects += 1;
        e.slots += slots as u64;
    }

    pub fn each(&self) -> impl Iterator<Item = (Pc, Allocated)> + '_ {
        self.by_pc.iter().map(|(pc, a)| (*pc, *a))
    }

    pub fn merge(&mut self, other: &Sites) {
        for (pc, a) in &other.by_pc {
            let e = self.by_pc.entry(*pc).or_default();
            e.objects += a.objects;
            e.slots += a.slots;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_pc.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_is_taken_every_nth_entry() {
        let mut p = Profile::new(3, 8);
        let due: Vec<bool> = (0..7).map(|_| p.due()).collect();
        assert_eq!(due, [false, false, true, false, false, true, false]);
    }

    #[test]
    fn samples_on_one_stack_are_counted_together() {
        let mut p = Profile::new(1, 8);
        p.record(vec![1, 2, 3]);
        p.record(vec![1, 2, 3]);
        p.record(vec![4]);
        let mut got: Vec<(Vec<Pc>, u64)> = p.stacks().map(|(s, n)| (s.to_vec(), n)).collect();
        got.sort();
        assert_eq!(got, [(vec![1, 2, 3], 2), (vec![4], 1)]);
        assert_eq!(p.taken(), 3);
        assert_eq!(p.shallow(), 1, "the one-frame stack");
    }

    #[test]
    fn merging_adds_the_counts_up() {
        let mut a = Profile::new(1, 8);
        a.record(vec![1, 2]);
        let mut b = Profile::new(1, 8);
        b.record(vec![1, 2]);
        b.record(vec![9]);
        a.merge(&b);
        assert_eq!(a.taken(), 3);
        let mut got: Vec<(Vec<Pc>, u64)> = a.stacks().map(|(s, n)| (s.to_vec(), n)).collect();
        got.sort();
        assert_eq!(got, [(vec![1, 2], 2), (vec![9], 1)]);
    }
}

#[cfg(all(test, feature = "profile-alloc"))]
mod alloc_tests {
    use super::*;

    #[test]
    fn allocation_is_counted_by_the_instruction_that_asked() {
        let mut s = Sites::default();
        s.note(7, 4);
        s.note(7, 6);
        s.note(9, 2);
        let mut got: Vec<(Pc, Allocated)> = s.each().collect();
        got.sort_by_key(|(pc, _)| *pc);
        assert_eq!(
            got,
            [
                (
                    7,
                    Allocated {
                        objects: 2,
                        slots: 10
                    }
                ),
                (
                    9,
                    Allocated {
                        objects: 1,
                        slots: 2
                    }
                ),
            ]
        );
    }

    #[test]
    fn merging_adds_the_sites_up() {
        let mut a = Sites::default();
        a.note(1, 3);
        let mut b = Sites::default();
        b.note(1, 5);
        b.note(2, 1);
        a.merge(&b);
        let mut got: Vec<(Pc, Allocated)> = a.each().collect();
        got.sort_by_key(|(pc, _)| *pc);
        assert_eq!(
            got,
            [
                (
                    1,
                    Allocated {
                        objects: 2,
                        slots: 8
                    }
                ),
                (
                    2,
                    Allocated {
                        objects: 1,
                        slots: 1
                    }
                ),
            ]
        );
    }
}
