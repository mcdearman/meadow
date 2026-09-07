//! The **high-level IR** — the resolver's output, and the input to inference and
//! lowering.
//!
//! Structurally it mirrors [`crate::ast`], with two differences:
//!
//! * Every node is a [`Node<T>`] — value + [`Span`] + a dense [`NodeId`]. Inference
//!   keeps its results in a `Vec` indexed by `NodeId`, producing a fully annotated
//!   tree.
//! * Every identifier is a [`VarId`] (globally unique — see the module docs in
//!   `lib.rs`). Type-constructor and data-constructor names stay interned strings.
//!
//! Operators are gone (desugared to primitive [`Expr::App`]s). `data` / `record`
//! declarations arrive here already validated ([`Decl::Data`], [`Decl::Record`],
//! [`TypeExpr`]).

use crate::{intern::InternedString, span::Span};
use std::ops::Deref;
use std::sync::atomic::AtomicU32;

/// A stable identifier for every node/subnode in the HIR tree.
///
/// Ids are dense and allocated per program by [`NodeIdGen`], so downstream passes
/// (type inference in particular) can keep their results in cheap `Vec`-backed
/// side tables indexed by `NodeId.0` and hand back a fully annotated tree.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone)]
pub struct NodeIdGen {
    next: u32,
}

impl NodeIdGen {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn fresh(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next += 1;
        id
    }

    /// Number of ids handed out so far — i.e. the length a dense side table needs.
    pub fn count(&self) -> usize {
        self.next as usize
    }
}

impl Default for NodeIdGen {
    fn default() -> Self {
        Self::new()
    }
}

/// HIR spine wrapper: like `span::Located` but additionally carries a [`NodeId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node<T> {
    pub id: NodeId,
    pub span: Span,
    pub value: Box<T>,
}

impl<T> Node<T> {
    pub fn new(id: NodeId, value: T, span: Span) -> Self {
        Self {
            id,
            span,
            value: Box::new(value),
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn id(&self) -> NodeId {
        self.id
    }
}

impl<T> Deref for Node<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

pub type LModule = Node<Module>;

/// Label for a record field. Carries a [`NodeId`]/span like any other node, but is
/// a plain name rather than a resolved variable.
pub type Label = Node<InternedString>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: InternedString,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Node<Decl>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
    /// `use a.b.c` — recorded for the (package-wide) resolver; carries no runtime weight.
    Use(Vec<InternedString>),
    /// `data Node a = …`
    Data(DataDecl),
    /// `record Person = { … }`
    Record(RecordDecl),
    /// `effect State s { … }`
    Effect(EffectDecl),
    Error,
}

pub type LTypeExpr = Node<TypeExpr>;

/// A resolved type expression from a `data` / `record` declaration. Type-variable
/// names are resolved to [`VarId`]s (fresh per declaration); type-constructor names
/// stay interned strings and are validated against the tycon environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Var(Ident),
    Con(InternedString, Vec<LTypeExpr>),
    /// `arg -> ret ! effect` (curried; effect on the last arrow).
    Fun(Vec<LTypeExpr>, LTypeExpr, Option<EffectRow>),
    Tuple(Vec<LTypeExpr>),
    List(LTypeExpr),
}

/// A resolved effect annotation. `labels` names are effect constructors; `tail`
/// (if any) is a resolved type variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRow {
    pub labels: Vec<(InternedString, Vec<LTypeExpr>)>,
    pub tail: Option<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDecl {
    pub name: InternedString,
    pub params: Vec<Ident>,
    /// Each operation: `(name, its top-level VarId, declared type)`. The op is
    /// callable as a value, so it gets a `VarId` like a `def`.
    pub ops: Vec<(InternedString, Ident, LTypeExpr)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerArm {
    /// The effect this operation belongs to (resolved from `op`).
    pub effect: InternedString,
    pub op: InternedString,
    pub param: LPat,
    pub resume: Ident,
    pub body: LExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDecl {
    pub name: InternedString,
    pub params: Vec<Ident>,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: InternedString,
    pub fields: VariantFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantFields {
    Positional(Vec<LTypeExpr>),
    Named(Vec<(InternedString, LTypeExpr)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDecl {
    pub name: InternedString,
    pub params: Vec<Ident>,
    pub fields: Vec<(InternedString, LTypeExpr)>,
}

pub type LExpr = Node<Expr>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Var(Ident),
    Lit(Lit),
    Lam(Vec<LPat>, LExpr),
    App(LExpr, Vec<LExpr>),
    Let(Vec<Bind>, LExpr),
    If(LExpr, LExpr, LExpr),
    Match(LExpr, Vec<(LPat, LExpr)>),
    Tuple(Vec<LExpr>),
    List(Vec<LExpr>),
    /// Data-constructor application. Constructors are not resolved to [`VarId`]s yet
    /// (no `data` decls), so the name is kept as an opaque [`Label`].
    Cons(Label, Vec<LExpr>),
    /// `{ x = e, y = e | base }` — the optional trailing expr is a record to extend.
    Record(Vec<(Label, LExpr)>, Option<LExpr>),
    /// `e.label`
    Field(LExpr, Label),
    /// `handle e with { … }`
    Handle(LExpr, Vec<HandlerArm>, Option<(LPat, LExpr)>),
    Unit,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    Pat(LPat, LExpr),
    Fun(Ident, Vec<Ident>, LExpr),
    Error,
}

pub type LPat = Node<Pat>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Wildcard,
    Var(Ident),
    Lit(Lit),
    As(Ident, LPat),
    Cons(Label, Vec<LPat>),
    Tuple(Vec<LPat>),
    List(Vec<LPat>),
    /// `{ x, y = p | _ }` — `open` is true when the pattern ends in `| _`.
    Record(Vec<(Label, LPat)>, bool),
    Unit,
    Error,
}

pub type Ident = Node<VarId>;

static COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarId(pub u32);

impl VarId {
    pub fn fresh() -> Self {
        Self(COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    Int(i64),
    String(InternedString),
}
