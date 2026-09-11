//! **AxCut** — a machine-level sequent-calculus IR.
//!
//! From Schuster, Müller, Ostermann and Brachthäuser, *Compiling Classical
//! Sequent Calculus to Stock Hardware: The Duality of Compilation* (OOPSLA
//! 2025). The grammar below follows the artifact's `Syntax.idr`; the abstract
//! machine in `meadow_rts::axcut` follows its `Semantics.idr`.
//!
//! # What makes it a machine language
//!
//! A textbook sequent calculus (λμμ̃ — the one in "Grokking the Sequent
//! Calculus") has producers, consumers, and a *cut* `⟨p | c⟩` that runs one
//! against the other. AxCut is what you get after cut elimination: there is no
//! `Cut` constructor at all, because every cut has already been reduced away.
//! What remains is seven statements and no expressions whatsoever.
//!
//! The consequence that matters, and the reason for the whole exercise:
//!
//! > **`Jump` and `Invoke` take no arguments.**
//!
//! Arguments are not passed. They are already *in the environment*, in the right
//! order, because a [`Statement::Substitute`] put them there. The environment is
//! an ordered list — a register file — and `Substitute` is the only thing that
//! rearranges it. Code generation for a call is therefore a permutation of
//! registers followed by a branch, and register allocation stops being a search
//! problem: the IR already says where everything goes.
//!
//! # The seven statements
//!
//! ```text
//!   substitute [x, y] in {(a, b) => s}   rebuild the environment as [x, y]
//!   jump L                               tail-jump; environment unchanged
//!   let z = K#2(x, y); s                 build a data value
//!   switch x { K#2 => s0, else => s1 }   branch on a tag, binding the fields
//!   new f {…methods…} capturing [x]; s   build codata: a closure, or a handler
//!   invoke f#0                           call a method
//!   extern add(x, y) { {(r) => s} }      a primitive; its blocks are its
//!                                        continuations
//! ```
//!
//! Three more — `handle`, `unhandle` and `perform` — are **not** from the paper.
//! AxCut has no handlers because Effekt compiles effects away before reaching
//! it, and Meadow does not. They are grouped separately below so a reader knows
//! which is which.
//!
//! Data (`let` / `switch`) and codata (`new` / `invoke`) are exact duals, which
//! is the "duality" of the title. A closure is codata with one method. A
//! *continuation* is also codata with one method — which is why returning from a
//! function is `invoke ret#0` and needs no separate mechanism, and why an effect
//! handler is an ordinary object rather than a special form.
//!
//! # Linearity
//!
//! The paper's environment is linear: a statement consumes exactly the names it
//! mentions, and `Substitute` is what duplicates, drops and reorders. This crate
//! **does not yet enforce that** — see [`Statement::Substitute`]. Names are
//! checked for scope, not for use count. Getting there is what would let the
//! register allocator read its answer off the IR instead of computing it.

use meadow_core::Prim;
use meadow_intern::InternedString;

/// A name in the environment.
///
/// Not a variable in the usual sense: it is a *position*, and the environment is
/// a register file. Reusing `meadow_core`'s variable numbering keeps lowering
/// from having to rename everything it touches.
pub type Name = meadow_hir::VarId;

/// Re-exported so a consumer of the IR can build names without depending on the
/// front end's crate graph — the runtime does exactly that.
pub use meadow_hir::VarId;

/// Which constructor of a data type, or which method of a codata object.
pub type Tag = u32;

/// A top-level block, reachable by [`Statement::Jump`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub u32);

/// A block: parameter names, and a body that runs with those names bound.
///
/// The parameters are the shape the environment must have on entry. A caller
/// arranges it with [`Statement::Substitute`] and then jumps.
#[derive(Debug, Clone)]
pub struct Block {
    pub params: Vec<Name>,
    pub body: Statement,
}

#[derive(Debug, Clone)]
pub enum Statement {
    /// `substitute [x, y, …] in block` — rebuild the environment from the named
    /// values, in that order, and run the block.
    ///
    /// The whole of calling convention, argument passing, closure conversion and
    /// register allocation lives here. A `jump` carries no arguments precisely
    /// because a `substitute` has already put them where the target expects
    /// them, so lowering this is emitting moves and nothing else.
    ///
    /// In the paper this is where the linear environment is duplicated, dropped
    /// and permuted, and the discipline is what makes the moves minimal. Here it
    /// is just a permutation with repeats allowed.
    Substitute(Vec<Name>, Box<Block>),

    /// `jump L` — transfer control, environment unchanged.
    Jump(Label),

    /// `let x = tag(fields); rest` — build a data value and bind it.
    Let {
        name: Name,
        /// Which constructor. The name is kept alongside for readable output and
        /// for talking to a runtime that identifies constructors by name.
        tag: Tag,
        ctor: InternedString,
        fields: Vec<Name>,
        rest: Box<Statement>,
    },

    /// `switch x { K#2 => …, else => … }` — branch on a constructor tag. The
    /// matching arm's parameters bind that constructor's fields, followed by the
    /// environment as it stands — `x` included.
    ///
    /// Two departures from the paper, both deliberate.
    ///
    /// Its `switch` is a dense list, one block per constructor, because its arms
    /// come from a typed constructor table. `core` carries no such grouping —
    /// constructor names are global and a `match` need not be exhaustive — so
    /// arms are keyed by tag and there is always a default, which binds the
    /// environment unchanged since it knows no fields.
    ///
    /// Its `switch` also *consumes* the scrutinee, because its environment is
    /// linear and a value still wanted would have been duplicated first. This
    /// one keeps it, for the same reason [`Statement::Let`] and
    /// [`Statement::New`] keep what they build from: `match o with | Just x -> o`
    /// is ordinary Meadow, and without the duplication pass that would make
    /// consuming safe, the honest thing is to leave the environment alone.
    /// [`Statement::Invoke`] is the exception and has to be — see its note.
    Switch {
        scrutinee: Name,
        arms: Vec<(Tag, Block)>,
        default: Box<Block>,
    },

    /// `new f { …methods… } capturing [x, …]; rest` — build codata.
    ///
    /// A closure, a continuation and an effect handler are all this. The methods
    /// run with the captured environment restored, plus whatever the caller
    /// arranged.
    New {
        name: Name,
        captures: Vec<Name>,
        methods: Vec<Block>,
        rest: Box<Statement>,
    },

    /// `invoke f#tag` — enter a method of a codata object. The method's
    /// environment is `f`'s captures followed by what was left behind `f`.
    ///
    /// Takes no arguments, for the same reason `jump` does not.
    ///
    /// This is the one statement that *consumes*, and it must: a method's
    /// parameters are `captures ++ arguments`, so `f` cannot still be sitting
    /// among them. It is also the one place consuming is safe without a
    /// duplication pass, because every `invoke` this crate emits is immediately
    /// preceded by a `substitute` that built the environment for it.
    Invoke(Name, Tag),

    /// `extern p(args) { blocks }` — a primitive.
    ///
    /// The blocks are its continuations, which is how branching primitives fit
    /// without a separate `if`: a branch takes two blocks, arithmetic takes one
    /// that binds the result. The paper's `Extern` carries a string, and
    /// `lit_5` is how a literal is written — there is no literal producer,
    /// because a producer would be an expression and there are none.
    Extern {
        op: Extern,
        args: Vec<Name>,
        blocks: Vec<Block>,
    },

    // --- beyond the paper: algebraic effects -----------------------------
    //
    // AxCut has no handlers. Effekt, the language it was built for, compiles
    // effects away *before* reaching it. Meadow's `core` still has `perform` and
    // `handle`, and rather than pretend otherwise these three statements add a
    // handler stack to the machine — the same shape `meadow_rts::stack` already
    // commits to for the bytecode VM.
    //
    // They are separated out because they are the part a reader should not
    // attribute to the paper, and because a later handler-passing transform
    // could remove them and leave the seven statements above intact.
    /// Install `handler` for the given operations; `k` is where the whole
    /// `handle` expression's value goes. The environment is unchanged.
    Handle {
        handler: Name,
        /// `(effect, operation)` per method of the handler object, in order.
        ops: Vec<(InternedString, InternedString)>,
        k: Name,
        rest: Box<Statement>,
    },

    /// Pop the innermost installed handler and bind its continuation as `k` —
    /// what the body's normal return does before running the `return` clause.
    ///
    /// Binding rather than popping is the point. The frame's continuation is
    /// **not** the one the `handle` was written next to: a resumption rebinds it
    /// to the `resume` call site, so that a body which was suspended and
    /// restarted returns into the middle of the clause that restarted it. The
    /// only way to reach the right one is to read it off the frame.
    Unhandle { k: Name, rest: Box<Statement> },

    /// `perform E.op arg` answering `k`.
    ///
    /// Unwinds to the innermost handler with a clause for `E.op`, hands its
    /// clause the argument, a one-shot resumption, and the handler's own
    /// continuation. The resumption carries `k` *and* the handler frames that
    /// were unwound past — including the handler's own, which is what makes
    /// handlers deep.
    Perform {
        effect: InternedString,
        op: InternedString,
        arg: Name,
        k: Name,
    },

    /// Not in the paper. Reaching one is a runtime error, and it exists so that
    /// an untranslatable term is loud rather than silently missing.
    Error(&'static str),
}

/// What an [`Statement::Extern`] does.
///
/// The paper's is a bare string interpreted by the backend (`"add"`, `"ifz"`,
/// `"lit_5"`). Naming the three kinds keeps the lowering from building strings
/// only to parse them again.
#[derive(Debug, Clone)]
pub enum Extern {
    /// Produce a constant. One continuation, which binds it.
    Lit(meadow_core::Lit),
    /// A `meadow` primitive. One continuation, which binds the result.
    Prim(Prim),
    /// A binary primitive whose right operand is a literal — `n - 1`, `x == 0`.
    /// One argument, naming the left operand; one continuation.
    ///
    /// Folding the literal in is worth a variant of its own because of what it
    /// removes, which is not only the instruction that loaded it: the literal
    /// never gets a name, so it never joins the environment, never takes a
    /// register, and is never carried through the moves at the next jump. A loop
    /// counting down by one used to hoist `1` into the environment on every
    /// iteration.
    PrimK(Prim, meadow_core::Lit),
    /// Branch: two continuations, taken on false and true respectively.
    ///
    /// `if` is not a statement of its own — a branching primitive with two
    /// continuation blocks is all it ever was.
    Branch,
    /// Branch on a binary primitive: `if x < y`, in one statement rather than a
    /// comparison whose result is immediately tested and then thrown away.
    ///
    /// Two arguments, two continuations.
    BranchPrim(Prim),
    /// The same with a literal right operand: `if n == 0`, which is the shape
    /// every counting loop and every literal pattern ends up in.
    ///
    /// One argument, two continuations.
    BranchPrimK(Prim, meadow_core::Lit),

    /// Build a record. The labels name the arguments, in the same order.
    Record(Vec<InternedString>),
    /// `r.label`.
    Select(InternedString),
    /// `{ r | label = v }` — one argument the record, one the value.
    Extend(InternedString),
    /// Build the builtin `Array` from its arguments.
    Array,

    /// Field `i` of a data value, tuple or array.
    ///
    /// The paper would use `switch` for this, and for a `match` arm so does the
    /// lowering. But `core`'s tuple projection carries only an index, and its
    /// arity is not recoverable from the term — a `switch` arm has to name every
    /// field, so it cannot be written. This can, and it does not care.
    Field(usize),
}

/// A labelled block in the program table.
#[derive(Debug, Clone)]
pub struct Def {
    pub label: Label,
    pub name: InternedString,
    pub block: Block,
}

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub defs: Vec<Def>,
    /// Where execution starts.
    pub entry: Option<Label>,
    /// Constructor name -> tag, so `switch` arms and `let` agree on numbering.
    pub tags: std::collections::HashMap<InternedString, Tag>,
}

impl Program {
    pub fn block(&self, label: Label) -> Option<&Block> {
        self.defs
            .iter()
            .find(|d| d.label == label)
            .map(|d| &d.block)
    }
}

mod lower;
pub use lower::{lower_program, Lowered, Unsupported};

pub mod machine;

mod print;

impl Extern {
    /// Does this `extern` choose between two continuations rather than produce
    /// a value?
    ///
    /// The three branching forms differ only in how they get their boolean, and
    /// every consumer wants to know which group an operation is in before it
    /// cares which member.
    pub fn is_branch(&self) -> bool {
        matches!(
            self,
            Extern::Branch | Extern::BranchPrim(_) | Extern::BranchPrimK(_, _)
        )
    }
}

/// Can `p`'s operands be given in either order?
///
/// Only used to fold a literal *left* operand into the right-hand slot the
/// folded forms provide — `2 * x` should compile the way `x * 2` does. Listed
/// rather than derived, and deliberately short: a wrong entry here is a program
/// that runs and computes something else.
pub fn commutes(p: Prim) -> bool {
    use Prim::*;
    matches!(
        p,
        Add | Mul | Eq | Ne | AddF | MulF | AddB | MulB | BitAnd | BitOr | BitXor
    )
}
