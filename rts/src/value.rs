//! Runtime values.
//!
//! A `Value` is **`Copy` and 16 bytes**, and copying one never touches the heap.
//! Everything with a payload is an [`Addr`], an index into [`crate::heap::Heap`],
//! and the collector is what decides when the thing at that address goes away.
//!
//! That is the whole difference from `meadow_eval`, which reference-counts. Two
//! problems went away with the `Rc`s:
//!
//! * **Dropping is not recursive any more.** A `Cons` chain of a million
//!   elements used to unwind a million `Drop`s and overflow the stack, which is
//!   why the CEK's constructor payload has a hand-written iterative `Drop`.
//!   Nothing here has a destructor at all.
//! * **Cycles are collectable.** Reference counting cannot free a resumption
//!   that closes over a handler that holds the resumption; a copying collector
//!   never sees the question.
//!
//! What it costs is that a `Value` only means something *next to its heap*.

use meadow_intern::InternedString;

/// An index into the heap's slot array. Not a pointer: the collector moves
/// objects, and every live `Addr` is rewritten when it does.
pub type Addr = u32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Char(char),
    /// Interned, and therefore not on the collected heap. Strings the program
    /// builds at run time are interned too, so they accumulate for the life of
    /// the process — the same as in the CEK machine, and the one allocation this
    /// runtime does not manage.
    Str(InternedString),
    Obj(Addr),
}

impl Value {
    pub fn addr(self) -> Option<Addr> {
        match self {
            Value::Obj(a) => Some(a),
            _ => None,
        }
    }

    /// A short tag for introspection — what a register dump shows per slot.
    pub fn kind(self) -> &'static str {
        match self {
            Value::Unit => "()",
            Value::Bool(_) => "Bool",
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Char(_) => "Char",
            Value::Str(_) => "String",
            Value::Obj(_) => "object",
        }
    }
}
