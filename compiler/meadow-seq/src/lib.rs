//! **A sequent-calculus intermediate representation.**
//!
//! Between `core` (an expression language) and bytecode (a register machine)
//! there is a gap: expressions nest and return, machines jump and write
//! registers. The usual bridge is ANF or CPS. This is the third option — a
//! *sequent* IR, in the style of Binder and Ostermann's "Grokking the Sequent
//! Calculus" and the AxCut line of work that follows it.
//!
//! # What that means here
//!
//! An expression language has one kind of thing: a term, which *produces* a
//! value. A sequent language has two, and they are symmetric:
//!
//! * a **producer** makes a value — a literal, a variable, a constructor;
//! * a **consumer** uses one — a continuation, a pattern match, a call site.
//!
//! Nothing evaluates on its own. Computation is a **statement**, written
//! `⟨producer | consumer⟩` and called a *cut*: run this producer against that
//! consumer. `1 + 2` is not an expression that yields `3`; it is a statement
//! that hands `3` to whatever was waiting.
//!
//! Two things fall out, and they are why this is worth the extra pass:
//!
//! * **Control is explicit and first class.** A continuation is an ordinary
//!   consumer bound to a covariable, so `return`, tail calls, and — the reason
//!   this crate exists — *effect handlers* stop being special forms. A handler
//!   is a consumer that a `perform` looks for.
//! * **Everything is already named and flat.** A statement's operands are
//!   variables and covariables, never nested computations, so lowering to
//!   registers is a naming problem rather than a scheduling one.
//!
//! # Where this departs from the papers
//!
//! Faithfulness is claimed only for the shape, not the details. In particular
//! the substructural discipline that AxCut uses to make register allocation
//! trivial and handler capture cheap is **not** implemented — variables here are
//! ordinary, not linear, and [`crate::lower`] does not track uses. The
//! consequence is that `rts`'s register allocator has to do real work rather
//! than reading the answer off the IR. That is a deliberate first step, not a
//! claim to have reproduced the paper.

use meadow_core::{Lit, Prim, Var};
use meadow_intern::InternedString;

/// A consumer-side name: what a producer will be handed to.
///
/// The dual of [`Var`]. Kept distinct so that a cut cannot be built backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Covar(pub u32);

/// A label a statement can jump to, with arguments — the flattened form of a
/// join point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub u32);

/// Something that produces a value.
#[derive(Debug, Clone)]
pub enum Producer {
    Var(Var),
    Lit(Lit),
    /// `K(p, …)` — a constructor applied to producers.
    Ctor(InternedString, Vec<Producer>),
    Tuple(Vec<Producer>),
    /// A function. In sequent terms this is codata: it is defined by how it
    /// responds to being applied, so it binds both its argument and the
    /// covariable its result goes to.
    Lam {
        param: Var,
        ret: Covar,
        body: Box<Statement>,
    },
    /// `mu a. s` — a value defined by what it does to its consumer.
    ///
    /// This is the construct that makes control explicit: it names its own
    /// continuation `a` and then runs a statement, so "call/cc" is not a
    /// primitive but the ordinary way to speak about where a value is going.
    Mu(Covar, Box<Statement>),
}

/// Something that consumes a value.
#[derive(Debug, Clone)]
pub enum Consumer {
    Covar(Covar),
    /// `mu~ x. s` — bind the incoming value to `x` and run `s`.
    ///
    /// The dual of [`Producer::Mu`], and what a `let` becomes.
    MuTilde(Var, Box<Statement>),
    /// Apply the value to an argument, sending the result to a covariable.
    Apply(Box<Producer>, Covar),
    /// Match the value against patterns.
    Case(Vec<Branch>),
    /// The consumer at the very top: stop, with this value as the answer.
    Finish,
}

/// One arm of a [`Consumer::Case`].
#[derive(Debug, Clone)]
pub struct Branch {
    pub pat: Pattern,
    pub body: Statement,
}

/// Patterns, flattened to one level: a constructor and the names for its fields.
///
/// Nested patterns are compiled away before this IR — a sequent statement
/// should not need a pattern-match compiler inside it.
#[derive(Debug, Clone)]
pub enum Pattern {
    Ctor(InternedString, Vec<Var>),
    Lit(Lit),
    Tuple(Vec<Var>),
    Wildcard,
}

/// A computation.
#[derive(Debug, Clone)]
pub enum Statement {
    /// `⟨p | c⟩` — the cut. Run the producer against the consumer.
    Cut(Producer, Consumer),
    /// `let x = prim(args); s`
    ///
    /// Primitives are not producers: they are strict in all their arguments and
    /// have nowhere to send a continuation, so giving them their own statement
    /// keeps the cut rule honest.
    Prim {
        prim: Prim,
        args: Vec<Producer>,
        out: Var,
        next: Box<Statement>,
    },
    /// Branch on a producer that evaluates to a boolean.
    If {
        cond: Producer,
        then: Box<Statement>,
        els: Box<Statement>,
    },
    /// `jump L(args)` — a join point, so two branches can share a tail without
    /// duplicating it.
    Jump(Label, Vec<Producer>),
    /// `let L(params) = body; s`
    LetLabel {
        label: Label,
        params: Vec<Var>,
        body: Box<Statement>,
        next: Box<Statement>,
    },
    /// `perform Effect.op(arg)`, sending the result to a covariable.
    ///
    /// In a fully sequent treatment this would be a cut against a handler
    /// consumer found on the stack. It is kept explicit because the runtime's
    /// segmented stack is what does the finding, and hiding that behind a cut
    /// would make the IR lie about the cost.
    Perform {
        effect: InternedString,
        op: InternedString,
        arg: Producer,
        ret: Covar,
    },
    /// `handle body with { … }`
    Handle {
        body: Box<Statement>,
        clauses: Vec<HandlerClause>,
        /// `return x -> s`, defaulting to the identity.
        ret: Option<(Var, Box<Statement>)>,
        /// Where the whole handled computation's value goes.
        out: Covar,
    },
    /// A term the front end could not translate. Reaching one is a runtime error.
    Error,
}

/// One operation clause of a handler.
#[derive(Debug, Clone)]
pub struct HandlerClause {
    pub effect: InternedString,
    pub op: InternedString,
    pub param: Var,
    /// The resumption, bound as an ordinary variable — which is the point: a
    /// continuation is a value here, not a control construct.
    pub resume: Var,
    pub body: Statement,
}

/// A lowered top-level definition.
#[derive(Debug, Clone)]
pub struct Def {
    pub var: Var,
    pub name: InternedString,
    /// The body, as a statement that sends its answer to [`Def::ret`].
    pub body: Statement,
    pub ret: Covar,
}

/// A whole program in sequent form.
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub defs: Vec<Def>,
    pub entry: Option<Var>,
}

mod lower;
pub use lower::{lower_program, Names};

mod print;
