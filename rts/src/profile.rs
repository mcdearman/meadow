//! **Where a program spends itself**: sampling, and what the samples are.
//!
//! # The machine has no stack pointer, and that is the interesting part
//!
//! A profile wants a *stack* at each sample, and [`crate::Vm`] has nothing to
//! unwind one from -- no `call`, no `ret`, no stack pointer; see
//! `docs/RUNTIME.md`. What it has instead is a chain of continuations: the
//! function running holds the one it will answer, that one captured the one
//! *its* caller will answer, and so on down to the `halt` at the bottom. Most
//! links are frames on the frame stack and a few are heap closures; they are
//! laid out alike, and the walk does not care which it is on.
//!
//! Walking the chain is walking the call stack, and each link's entry pc -- a
//! frame's `meta`, or method 0 of a closure's table -- is where control goes
//! when the function under it returns: a return address, which is exactly the
//! frame a profile wants.
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

/// Times a second to sample, unless asked for another.
pub const HZ: u64 = 1000;

/// What makes a block entry a sample.
///
/// Two profiles, and they answer different questions. Counting entries gives a
/// profile of *work*: deterministic, the same twice for the same program, and
/// blind to anything that costs seconds without retiring instructions.
/// Counting ticks gives a profile of *time*: it sees a cache miss and a
/// collection, and it is the one to trust about which of two changes was
/// worth making.
///
/// A tick does not interrupt anything. A thread sleeps and moves a counter,
/// and each machine notices at the next block it enters -- so the walk happens
/// where the machine is consistent, and there is no signal handler reading a
/// half-built stack. Every machine running at the time takes one sample per
/// tick, which is what makes the counts come out proportional to processor
/// time rather than wall-clock.
#[derive(Debug, Clone)]
enum When {
    /// Every `n` block entries, counting down.
    Entries { every: u64, countdown: u64 },
    /// Once per tick of a shared clock, which somebody else is moving.
    Ticks {
        clock: std::sync::Arc<std::sync::atomic::AtomicU64>,
        seen: u64,
    },
}

/// A clock that moves on its own, for a profile of time.
///
/// Stops when it is dropped, so a run's profile cannot outlive the run and
/// leave a thread behind.
pub struct Ticker {
    clock: std::sync::Arc<std::sync::atomic::AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Ticker {
    /// A clock moving `hz` times a second.
    pub fn at(hz: u64) -> Ticker {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let clock = std::sync::Arc::new(AtomicU64::new(0));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (c, s) = (clock.clone(), stop.clone());
        let period = std::time::Duration::from_nanos(1_000_000_000 / hz.max(1));
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                std::thread::sleep(period);
                c.fetch_add(1, Ordering::Relaxed);
            }
        });
        Ticker { clock, stop }
    }

    /// A profile that samples on this clock.
    pub fn profile(&self, depth: usize) -> Profile {
        Profile {
            when: When::Ticks {
                clock: self.clock.clone(),
                seen: 0,
            },
            depth: depth.max(1),
            stacks: HashMap::new(),
            taken: 0,
            shallow: 0,
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Samples, by the stack they landed on.
#[derive(Debug, Clone)]
pub struct Profile {
    when: When,
    depth: usize,
    /// Innermost frame first, as a profile is usually read.
    stacks: HashMap<Vec<Pc>, u64>,
    taken: u64,
    /// Samples that got no further than the frame they were taken in, because
    /// the program carries no debug info to walk by.
    shallow: u64,
}

impl Profile {
    /// A profile that samples every `every` block entries.
    pub fn new(every: u64, depth: usize) -> Profile {
        Profile {
            when: When::Entries {
                every: every.max(1),
                countdown: every.max(1),
            },
            depth: depth.max(1),
            stacks: HashMap::new(),
            taken: 0,
            shallow: 0,
        }
    }

    /// Does this profile measure time rather than work?
    pub fn is_timed(&self) -> bool {
        matches!(self.when, When::Ticks { .. })
    }

    /// How deep a sample walks.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Is this block entry a sample? Counts down either way.
    pub fn due(&mut self) -> bool {
        match &mut self.when {
            When::Entries { every, countdown } => {
                *countdown -= 1;
                if *countdown > 0 {
                    return false;
                }
                *countdown = *every;
                true
            }
            When::Ticks { clock, seen } => {
                let now = clock.load(std::sync::atomic::Ordering::Relaxed);
                if now == *seen {
                    return false;
                }
                *seen = now;
                true
            }
        }
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

/// How many of each instruction a run retired.
///
/// Exact rather than sampled -- the interpreter passes through one place -- and
/// under the same feature as [`Sites`], because it is a counter on the hottest
/// loop in the runtime. What it answers is the question a stack profile cannot:
/// a program that is all in one function still spends its time on *something*,
/// and a calling convention that costs more than the arithmetic it carries
/// shows up here and nowhere else.
#[cfg(feature = "profile-alloc")]
#[derive(Debug, Clone)]
pub struct Ops {
    counts: Vec<u64>,
}

#[cfg(feature = "profile-alloc")]
impl Default for Ops {
    fn default() -> Ops {
        Ops {
            counts: vec![0; 256],
        }
    }
}

#[cfg(feature = "profile-alloc")]
impl Ops {
    pub fn note(&mut self, op: meadow_bytecode::Op) {
        self.counts[op as usize] += 1;
    }

    pub fn merge(&mut self, other: &Ops) {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Every instruction that ran at least once, most first.
    pub fn each(&self) -> Vec<(meadow_bytecode::Op, u64)> {
        let mut out: Vec<(meadow_bytecode::Op, u64)> = meadow_bytecode::Op::ALL
            .iter()
            .filter_map(|op| {
                let n = self.counts[*op as usize];
                (n > 0).then_some((*op, n))
            })
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out
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

    /// A timed profile samples when the clock moves, not when instructions
    /// do -- so a machine that enters a great many blocks between two ticks
    /// contributes one sample, and one that enters two contributes one as well.
    #[test]
    fn a_timed_profile_samples_when_the_clock_moves() {
        let ticker = Ticker::at(1000);
        let mut p = ticker.profile(8);
        assert!(p.is_timed());

        // Nothing has ticked past what it has seen yet, so nothing is due --
        // however many blocks go by.
        let before = (0..1000).filter(|_| p.due()).count();
        assert!(before <= 1, "at most the first tick: {before}");

        // Once the clock has moved, exactly one entry is a sample, and the
        // next is not until it moves again.
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(p.due(), "the clock moved");
        assert!(!p.due(), "and has not moved again");
    }

    /// The clock stops with the profile that was taken on it, so a run cannot
    /// leave a thread behind.
    #[test]
    fn a_ticker_stops_when_it_is_dropped() {
        let clock = {
            let ticker = Ticker::at(2000);
            let clock = ticker.clock.clone();
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert!(
                clock.load(std::sync::atomic::Ordering::Relaxed) > 0,
                "it ran"
            );
            clock
        };
        let at_drop = clock.load(std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let after = clock.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            after <= at_drop + 1,
            "kept ticking after the drop: {at_drop} then {after}"
        );
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
