//! The **high-level IR** — the resolver's output, and the input to inference and
//! lowering.
//!
//! Structurally it mirrors [`meadow_ast`], with two differences:
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

use meadow_intern::InternedString;
use meadow_span::Span;

/// Primitive operators, in the order their [`VarId`]s are handed out by
/// `rename::Resolver::with_prelude`. `infer` builds matching type schemes by
/// index, and `core`/the linker map names to `Prim`s, so the order is load-bearing.
///
/// `and` / `or` are *not* here — they are keyword operators the resolver desugars
/// to `if` (that is what makes them short-circuit); `not` is a `Std.Bool`
/// function.
pub const PRIMS: &[&str] = &[
    "print",
    "println",
    "+",
    "-",
    "*",
    "/",
    "%",
    "^",
    "==",
    "!=",
    "<",
    ">",
    "<=",
    ">=",
    "neg", //
    "+.",
    "-.",
    "*.",
    "/.",
    "<.",
    ">.",
    "<=.",
    ">=.",
    "toFloat",
    "floor", //
    // arbitrary-precision integer ops (operands `BigInt`) + Int/BigInt conversions
    "+~",
    "-~",
    "*~",
    "/~",
    "%~",
    "^~",
    "<~",
    ">~",
    "<=~",
    ">=~",
    "toBigInt",
    "toInt", //
    // the one builtin collection: `Array`
    "arrayLen",
    "arrayGet",
    "arrayGetOr",
    "arraySet",
    "arrayPush",
    "arrayPop",
    "arraySlice",
    "arrayConcat", //
    // bitwise ops on `Int`, and the `String` <-> byte-array bridge
    "shl",
    "shr",
    "ushr",
    "bitAnd",
    "bitOr",
    "bitXor",
    "bitNot",
    "popCount", //
    "stringToBytes",
    "bytesToString",
    "bytesToHex",
    "bytesFromHex", //
    // render any value as the REPL would print it
    "show", //
    // `Char` <-> its Unicode scalar value, and `String` <-> its characters
    "charCode",
    "charFromCode",
    "stringToChars",
    "charsToString", //
    // the one mutable cell; every operation carries the `Mut` effect
    "newRef",
    "getRef",
    "setRef",
];
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

    /// Start handing out ids from `n` — used when several modules of one package
    /// share a single dense id space (and one [`crate`]-wide side table).
    pub fn starting_at(n: usize) -> Self {
        Self { next: n as u32 }
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
    /// Top-level binding groups, in dependency order — see [`BindGroup`].
    ///
    /// Filled by `meadow-scc`, which also reorders `decls` to match. Empty
    /// before that pass runs, which a consumer should read as "no grouping
    /// information", not "no bindings".
    pub groups: Vec<BindGroup>,
}

/// One strongly connected component of the top-level binding dependency graph.
///
/// Bindings that refer to one another have to be typed together: each is
/// monomorphic while the group is being solved, and all of them generalize once
/// it is. Ordering the groups by dependency means every binding a group refers
/// to is already generalized by the time the group is inferred, which is what
/// makes a helper usable at two different types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindGroup {
    /// Indices into [`Module::decls`]. Every one names a [`Decl::Bind`].
    pub members: Vec<usize>,
    /// Whether any member refers to the group — a self- or mutual recursion.
    /// A lone non-recursive binding needs no monomorphic seeding.
    pub recursive: bool,
}

pub type LDecl = Node<Decl>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
    /// `mod Foo` — declares a child module. Consumed by the driver; inert here.
    Mod(InternedString),
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
    Con(Label, Vec<LTypeExpr>),
    /// `arg -> ret ! effect` (curried; effect on the last arrow).
    Fun(Vec<LTypeExpr>, LTypeExpr, Option<EffectRow>),
    Tuple(Vec<LTypeExpr>),
    /// `[a]` — the default sequence, an RRB `Vector`.
    Vector(LTypeExpr),
    /// `[a;]` — a linked `List`.
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
    /// Where the name was written — see [`DataDecl::name_span`].
    pub name_span: Span,
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
    /// Where the name was written.
    ///
    /// A type constructor is not resolved to an id — see [`TypeExpr::Con`] — so
    /// a reference to one is matched by name, and a name cannot say *where* the
    /// declaration is. An editor asked to go there needs to know, so the
    /// position is carried rather than recovered.
    pub name_span: Span,
    pub params: Vec<Ident>,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: InternedString,
    /// Where the name was written — see [`DataDecl::name_span`].
    pub name_span: Span,
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
    /// Where the name was written — see [`DataDecl::name_span`].
    pub name_span: Span,
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
    /// `#[e, ...]` -- a builtin `Array` literal.
    Array(Vec<LExpr>),
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
    /// `fun f a (x, y) = e` — parameters are irrefutable patterns.
    /// `fun f p q : T = e` -- the optional type is the declared *result*.
    Fun(Ident, Vec<LPat>, Option<LTypeExpr>, LExpr),
    Error,
}

impl Bind {
    /// Every name this binding introduces, in source order.
    pub fn bound_vars(&self) -> Vec<VarId> {
        let mut out = Vec::new();
        match self {
            Bind::Fun(name, ..) => out.push(*name.value()),
            Bind::Pat(pat, _) => pat_vars(pat, &mut out),
            Bind::Error => {}
        }
        out
    }
}

/// Every name a pattern introduces, in source order, appended to `out`.
pub fn pat_vars(pat: &LPat, out: &mut Vec<VarId>) {
    match pat.value() {
        Pat::Var(id) => out.push(*id.value()),
        // The annotation binds nothing; the pattern under it does.
        Pat::Ann(inner, _) => pat_vars(inner, out),
        Pat::As(id, sub) => {
            out.push(*id.value());
            pat_vars(sub, out);
        }
        Pat::Tuple(ps) | Pat::List(ps) | Pat::Array(ps) | Pat::Cons(_, ps) => {
            ps.iter().for_each(|p| pat_vars(p, out))
        }
        Pat::Record(fields, _) => fields.iter().for_each(|(_, p)| pat_vars(p, out)),
        Pat::Wildcard | Pat::Lit(_) | Pat::Unit | Pat::Error => {}
    }
}

pub type LPat = Node<Pat>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Wildcard,
    Var(Ident),
    /// `(p : T)` -- a pattern with a declared type. The annotation is kept
    /// rather than erased: inference unifies against it, and it is the only
    /// type an editor can point at outside a declaration.
    Ann(Box<LPat>, LTypeExpr),
    Lit(Lit),
    As(Ident, LPat),
    Cons(Label, Vec<LPat>),
    Tuple(Vec<LPat>),
    /// `#[p, ...]` -- matches a builtin `Array` of exactly this length.
    Array(Vec<LPat>),
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
    /// Fixed-width integer (`Int`, i.e. i64). No `BigInt` literal — see [`PRIMS`].
    Int(i64),
    /// Float literal as its IEEE-754 bit pattern (see `ast::Lit::Float`).
    Float(u64),
    String(InternedString),
    Char(char),
}
