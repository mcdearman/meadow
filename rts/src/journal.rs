//! The journal: a log of **inverse operations**, so the machine can run backwards.
//!
//! Every write the VM makes appends the undo for that write. Stepping back is
//! then popping entries and applying them until the pc changes — no snapshots,
//! no replay from the start, and the cost is proportional to how far you step
//! rather than to how long the program has run.
//!
//! Why inverses rather than snapshots: a snapshot of the whole stack per
//! instruction is O(stack) each, and a program that recurses a million deep
//! makes that hopeless. An inverse is O(1) per write and most instructions write
//! once.
//!
//! Why not "just re-run from the start": that only works for deterministic
//! programs. This VM is meant to run `Std.Random`, `Std.Time` and `Std.Fs`,
//! whose results are not reproducible — an inverse log records what *did*
//! happen, which is the only thing that can be stepped back through.
//!
//! Recording is off by default ([`Journal::disabled`]). The VM checks one
//! boolean per write; a program that is not being debugged pays that and nothing
//! else.

use crate::value::Value;

/// The undo for one write.
///
/// Each variant says what the machine looked like *before* the operation, which
/// is exactly what applying it restores.
#[derive(Debug, Clone)]
pub enum Undo {
    /// A register held `was`.
    Reg { slot: usize, was: Value },
    /// The stack was `len` slots long — undoing a frame push.
    StackLen { len: usize },
    /// The frame stack was this deep.
    FrameLen { len: usize },
    /// The segment stack was this deep.
    SegLen { len: usize },
    /// A segment was detached by a `perform`; putting it back undoes the capture.
    SegRestore { at: usize, segments: Vec<crate::stack::Segment> },
    /// The program counter, recorded once per step so a step boundary is
    /// findable when walking backwards.
    Step { frame: usize, pc: u32 },
}

/// A recording of everything the machine has done.
#[derive(Debug, Default)]
pub struct Journal {
    entries: Vec<Undo>,
    /// When false, [`Journal::record`] is a no-op and nothing accumulates.
    recording: bool,
    /// How many steps have been taken. Also the "time" a debugger displays.
    steps: u64,
}

impl Journal {
    /// A journal that records nothing — the default for `meadow run`.
    pub fn disabled() -> Journal {
        Journal::default()
    }

    /// A journal that records everything, so the machine can be stepped back.
    pub fn recording() -> Journal {
        Journal {
            recording: true,
            ..Journal::default()
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// How many undo entries are held. A rough measure of memory owed to
    /// recording, and what a debugger shows as history depth.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn record(&mut self, undo: impl FnOnce() -> Undo) {
        // A closure rather than a value: building an `Undo` clones the old
        // register contents, and a disabled journal should not pay for that.
        if self.recording {
            self.entries.push(undo());
        }
    }

    /// Mark the start of an instruction. Stepping back runs until it reaches
    /// one of these.
    pub fn begin_step(&mut self, frame: usize, pc: u32) {
        self.steps += 1;
        if self.recording {
            self.entries.push(Undo::Step { frame, pc });
        }
    }

    /// Take back everything since — and including — the last step marker.
    ///
    /// Returns where the machine was when that instruction began, or `None` at
    /// the beginning of history.
    pub fn undo_step(&mut self) -> Option<(Vec<Undo>, usize, u32)> {
        if !self.recording {
            return None;
        }
        let mut taken = Vec::new();
        while let Some(entry) = self.entries.pop() {
            if let Undo::Step { frame, pc } = entry {
                self.steps -= 1;
                return Some((taken, frame, pc));
            }
            taken.push(entry);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_journal_keeps_nothing() {
        let mut j = Journal::disabled();
        j.begin_step(0, 0);
        j.record(|| Undo::StackLen { len: 7 });
        assert!(j.is_empty());
        // It still counts steps, which is what a progress display wants.
        assert_eq!(j.steps(), 1);
        assert!(j.undo_step().is_none());
    }

    #[test]
    fn undoing_returns_one_instructions_worth() {
        let mut j = Journal::recording();
        j.begin_step(0, 10);
        j.record(|| Undo::StackLen { len: 1 });
        j.record(|| Undo::StackLen { len: 2 });
        j.begin_step(0, 11);
        j.record(|| Undo::StackLen { len: 3 });

        let (undos, frame, pc) = j.undo_step().expect("one step back");
        assert_eq!((frame, pc), (0, 11));
        assert_eq!(undos.len(), 1, "only the second step's writes");

        let (undos, frame, pc) = j.undo_step().expect("two steps back");
        assert_eq!((frame, pc), (0, 10));
        assert_eq!(undos.len(), 2);

        assert!(j.undo_step().is_none(), "history is exhausted");
        assert_eq!(j.steps(), 0);
    }

    #[test]
    fn undo_entries_come_back_newest_first() {
        // They are applied in the order returned, and the newest write has to be
        // undone before the one it overwrote.
        let mut j = Journal::recording();
        j.begin_step(0, 0);
        j.record(|| Undo::StackLen { len: 1 });
        j.record(|| Undo::StackLen { len: 2 });
        let (undos, _, _) = j.undo_step().unwrap();
        match (&undos[0], &undos[1]) {
            (Undo::StackLen { len: a }, Undo::StackLen { len: b }) => {
                assert_eq!((*a, *b), (2, 1));
            }
            _ => panic!("unexpected entries"),
        }
    }
}
