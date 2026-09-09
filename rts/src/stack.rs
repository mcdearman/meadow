//! The **segmented stack**, and why effects want one.
//!
//! A one-shot deep handler has to do two things when an operation is performed:
//! find the nearest handler for it, and capture everything between that handler
//! and the `perform` as a resumption. On a single contiguous stack the capture
//! is a copy of every frame in between — O(depth), paid on every operation. The
//! CEK machine in `eval` does exactly that (`kont.split_off`), which is fine for
//! a specification and wrong for a runtime.
//!
//! Here the stack is a list of **segments**, and a `handle` starts a new one. A
//! `perform` walks *segments* outward rather than frames, and capturing is
//! moving whole segments out of the list: O(number of segments between), and the
//! frames inside them are never touched. Resuming moves them back.
//!
//! ```text
//!   segments:  [ main ][ handle A ][ handle B ]        <- perform op of A
//!                      ^^^^^^^^^^^^^^^^^^^^^^^
//!                      found A here, so everything above it
//!                      detaches as the resumption:
//!
//!   stack:     [ main ][ handle A ]      resumption: [ handle B ]
//! ```
//!
//! The segments are what make the resumption a value that can be dropped
//! (abandoning the computation, as `Stream.take` does) or re-attached exactly
//! once. Nothing enforces "exactly once" here beyond the resumption being moved
//! out of its register when used — the type system upstream is what guarantees
//! it, and [`Resumption::take`] returns `None` on a second use rather than
//! corrupting the stack.

use crate::code::Pc;
use crate::value::Value;
use meadow_intern::InternedString;
use std::cell::RefCell;
use std::rc::Rc;

/// One call frame: a window into the value stack plus where to go on return.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Index of this frame's register 0 in the value stack.
    pub base: usize,
    /// Which function's code is running.
    pub func: u32,
    /// Where to resume in the *caller* once this frame returns.
    pub return_pc: Pc,
    /// The caller's register to put the result in.
    pub return_reg: u8,
    /// Values captured by the closure being run.
    pub captures: Rc<Vec<Value>>,
}

/// A run of frames delimited by a handler.
///
/// The bottom segment has no handler and is where `main` runs.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Index into the frame stack where this segment begins.
    pub base_frame: usize,
    /// The handler that delimits it, if any.
    pub handler: Option<Handler>,
}

/// What a `handle` installed: which operations it answers, and where.
#[derive(Debug, Clone)]
pub struct Handler {
    /// Index into the program's handler table.
    pub id: u32,
    /// Operation name -> the function implementing that clause.
    ///
    /// A `Vec` rather than a map: a handler has a handful of clauses, and
    /// scanning three entries beats hashing.
    pub clauses: Vec<(InternedString, u32)>,
    /// The `return x -> e` clause, if the handler has one.
    pub ret: Option<u32>,
    /// Where to continue in the handler's own frame once the body finishes.
    pub resume_pc: Pc,
    /// Which register the body's result goes into.
    pub result_reg: u8,
    /// The frame that executed the `PushSeg`, so a clause runs in its scope.
    pub owner_frame: usize,
}

impl Handler {
    /// The function implementing `op`, if this handler answers it.
    pub fn clause_for(&self, op: InternedString) -> Option<u32> {
        self.clauses
            .iter()
            .find(|(name, _)| *name == op)
            .map(|(_, f)| *f)
    }
}

/// A captured, one-shot continuation: the segments detached by a `perform`.
///
/// Shared and interior-mutable so that a resumption can be stored in a data
/// structure, passed around, and then consumed exactly once — the second attempt
/// finds it empty.
#[derive(Debug, Clone)]
pub struct Resumption(Rc<RefCell<Option<Captured>>>);

/// The machine state a resumption puts back.
#[derive(Debug)]
pub struct Captured {
    pub segments: Vec<Segment>,
    pub frames: Vec<Frame>,
    /// The values belonging to those frames, lifted out of the value stack.
    pub values: Vec<Value>,
    /// Where the `perform` was, so resuming continues just after it.
    pub resume_pc: Pc,
    /// The register the performed operation's result goes into.
    pub result_reg: u8,
}

impl Resumption {
    pub fn new(captured: Captured) -> Resumption {
        Resumption(Rc::new(RefCell::new(Some(captured))))
    }

    /// Consume it. `None` if it has already been used — one-shot, enforced here
    /// rather than trusted.
    pub fn take(&self) -> Option<Captured> {
        self.0.borrow_mut().take()
    }

    /// Whether it is still usable. For introspection, not for control flow.
    pub fn is_live(&self) -> bool {
        self.0.borrow().is_some()
    }

    /// Two resumptions are the same one iff they are the same allocation —
    /// there is nothing else they could sensibly mean.
    pub fn same(&self, other: &Resumption) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(clauses: &[(&str, u32)]) -> Handler {
        Handler {
            id: 0,
            clauses: clauses
                .iter()
                .map(|(n, f)| (InternedString::from(*n), *f))
                .collect(),
            ret: None,
            resume_pc: 0,
            result_reg: 0,
            owner_frame: 0,
        }
    }

    #[test]
    fn a_handler_answers_only_its_own_operations() {
        let h = handler(&[("get", 1), ("put", 2)]);
        assert_eq!(h.clause_for(InternedString::from("get")), Some(1));
        assert_eq!(h.clause_for(InternedString::from("put")), Some(2));
        assert_eq!(h.clause_for(InternedString::from("nope")), None);
    }

    #[test]
    fn a_resumption_is_one_shot() {
        let r = Resumption::new(Captured {
            segments: vec![],
            frames: vec![],
            values: vec![],
            resume_pc: 3,
            result_reg: 1,
        });
        assert!(r.is_live());
        assert!(r.take().is_some(), "the first use works");
        assert!(!r.is_live());
        assert!(r.take().is_none(), "the second finds it spent");
    }

    #[test]
    fn a_clone_shares_the_one_shot_and_does_not_duplicate_it() {
        // Copying the value must not hand out a second use of the continuation:
        // resuming twice would run the segments after the `perform` twice.
        let r = Resumption::new(Captured {
            segments: vec![],
            frames: vec![],
            values: vec![],
            resume_pc: 0,
            result_reg: 0,
        });
        let copy = r.clone();
        assert!(r.same(&copy));
        assert!(r.take().is_some());
        assert!(copy.take().is_none(), "the copy is spent too");
    }
}
