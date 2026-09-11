//! The **abstract syntax tree** — the parser's output, before name resolution.
//!
//! Every node is wrapped in [`span::Located`] (value + source [`Span`]). Names are
//! still plain [`InternedString`]s here; [`meadow_rename`] turns them into
//! [`meadow_hir::VarId`]s and stamps node ids. Operators live as [`UnOp`] / [`BinOp`]
//! and are desugared to primitive calls during resolution, so the HIR has no
//! operator nodes.
//!
//! [`Span`]: meadow_span::Span

use meadow_intern::InternedString;
use meadow_span::Located;

pub type LModule = Located<Module>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub name: InternedString,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Located<Decl>;

/// An attribute: `@pub`, `@attr(A, B, C)`. Attached to a declaration (via
/// [`Decl::Attributed`]) or a record / named-variant field (see [`Field`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: Ident,
    pub args: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
    /// `mod Foo` — declares child module `Foo`; its source is supplied by the
    /// driver (filesystem discovery or the embedded stdlib table).
    Mod(Ident),
    /// `use a.b.c` / `use a.b (x, y, z)` — `names` empty means the whole module.
    Use(UseDecl),
    /// `data Node a = Leaf (Vector a) | Internal { ... }`
    Data(DataDecl),
    /// `record Person = { name : String }`
    Record(RecordDecl),
    /// `effect State s { get : () -> s, put : s -> () }`
    Effect(EffectDecl),
    /// One or more `@attr` lines in front of another declaration.
    Attributed(Vec<Attr>, Box<LDecl>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseDecl {
    pub path: Vec<Ident>,
    /// Selected names — `use a.b (x, y)`. Empty for a bare `use a.b`.
    pub names: Vec<Ident>,
    /// `use a.b as C` — the name the module is qualified by at use sites.
    /// Defaults to the last path segment.
    pub alias: Option<Ident>,
}

/// A record field or named-variant field, with any leading attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub attrs: Vec<Attr>,
    pub name: Ident,
    pub ty: LType,
}

pub type LType = Located<TypeExpr>;

/// A type expression as written in a `data` / `record` / `effect` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    /// lowercase type variable: `a`
    Var(Ident),
    /// type-constructor application: `Int`, `Vector a`, `Maybe (Vector Int)`
    Con(Ident, Vec<LType>),
    /// `a -> b ! e` (curried when lowered; the effect is on the last arrow).
    Fun(Vec<LType>, LType, Option<EffectRow>),
    /// `(a, b)`
    Tuple(Vec<LType>),
    /// `[a]` — the default sequence, an RRB `Vector`.
    Vector(LType),
    /// `[a;]` — a linked `List`. The `;` marks it, exactly as it does in the
    /// `[x; y]` literal and for the same reason: brackets alone mean `Vector`.
    List(LType),
}

/// An effect annotation `! <row>`: `! io`, `! e`, `! { io, State Int | e }`.
/// Closed iff `tail` is `None` and `labels` is non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRow {
    pub labels: Vec<(Ident, Vec<LType>)>,
    pub tail: Option<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub ops: Vec<Field>,
}

/// `op pat resume -> body` in a `handle` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerArm {
    pub op: Ident,
    pub param: LPat,
    pub resume: Ident,
    pub body: LExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: Ident,
    pub fields: VariantFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariantFields {
    /// `Leaf (Vector a) Int`
    Positional(Vec<LType>),
    /// `Internal { sizes : T, children : T }`
    Named(Vec<Field>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub fields: Vec<Field>,
}

pub type LExpr = Located<Expr>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Var(Ident),
    Lit(Lit),
    Lam(Vec<LPat>, LExpr),
    App(LExpr, Vec<LExpr>),
    Let(Vec<Bind>, LExpr),
    If(LExpr, LExpr, LExpr),
    Match(LExpr, Vec<(LPat, LExpr)>),
    UnOp(LUnOp, LExpr),
    BinOp(LBinOp, LExpr, LExpr),
    Tuple(Vec<LExpr>),
    /// `#[e, ...]` -- a builtin `Array` literal.
    Array(Vec<LExpr>),
    List(Vec<LExpr>),
    Cons(Ident, Vec<LExpr>),
    /// `Mod.name` / `Mod.Ctor` — a name qualified by a `use`d module. The resolver
    /// checks the qualifier then rewrites this to a plain `Var` / `Cons`.
    Qual(Ident, Ident),
    /// `{ x = e, y = e | base }` — trailing expr is the record being extended.
    Record(Vec<(Ident, LExpr)>, Option<LExpr>),
    /// `e.label`
    Field(LExpr, Ident),
    /// `handle e with { op p k -> …, return x -> … }`
    Handle(LExpr, Vec<HandlerArm>, Option<(LPat, LExpr)>),
    Unit,
    /// A `_` hole in expression position. Only legal inside an operator section
    /// `( … )`, where the parser rewrites the section into a lambda; anywhere
    /// else the resolver reports it.
    Hole,
}

pub type LUnOp = Located<UnOp>;

/// The only prefix operator is `-` (there is no prefix `!` — `not` is a function).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp {
    Neg,
}

impl ToString for UnOp {
    fn to_string(&self) -> String {
        match self {
            UnOp::Neg => "neg",
        }
        .to_string()
    }
}

pub type LBinOp = Located<BinOp>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Neq,
    Lt,
    Gt,
    Leq,
    Geq,
    /// `and` / `or` — short-circuiting; the resolver desugars them to `if`, so
    /// they never reach a primitive.
    And,
    Or,
    /// Floating-point `+. -. *. /.` and `<. >. <=. >=.`.
    AddF,
    SubF,
    MulF,
    DivF,
    LtF,
    GtF,
    LeqF,
    GeqF,
    /// Arbitrary-precision `+~ -~ *~ /~ %~ ^~` and `<~ >~ <=~ >=~` (operands are
    /// `BigInt`).
    AddB,
    SubB,
    MulB,
    DivB,
    ModB,
    PowB,
    LtB,
    GtB,
    LeqB,
    GeqB,
}

impl ToString for BinOp {
    fn to_string(&self) -> String {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "^",
            BinOp::Eq => "==",
            BinOp::Neq => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Leq => "<=",
            BinOp::Geq => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::AddF => "+.",
            BinOp::SubF => "-.",
            BinOp::MulF => "*.",
            BinOp::DivF => "/.",
            BinOp::LtF => "<.",
            BinOp::GtF => ">.",
            BinOp::LeqF => "<=.",
            BinOp::GeqF => ">=.",
            BinOp::AddB => "+~",
            BinOp::SubB => "-~",
            BinOp::MulB => "*~",
            BinOp::DivB => "/~",
            BinOp::ModB => "%~",
            BinOp::PowB => "^~",
            BinOp::LtB => "<~",
            BinOp::GtB => ">~",
            BinOp::LeqB => "<=~",
            BinOp::GeqB => ">=~",
        }
        .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    Pat(LPat, LExpr),
    /// `fun f a (x, y) = e` — parameters are patterns, and must be *irrefutable*
    /// (checked after inference, since single-variant constructors are allowed).
    /// `fun f p q : T = e` -- parameters are patterns, and must be *irrefutable*
    /// (checked after inference, since single-variant constructors are allowed).
    /// The optional type is the declared *result*, written before the `=`.
    Fun(Ident, Vec<LPat>, Option<LType>, LExpr),
}

pub type LPat = Located<Pat>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Wildcard,
    Var(Ident),
    /// `(p : T)` — a pattern with a declared type.
    ///
    /// The only place a type is written outside a `data` / `record` / `effect`
    /// declaration, and so the only way a *parameter* gets one: a parameter is
    /// a pattern, and `fun f x : Int = …` could not say whether the annotation
    /// belongs to `x` or to `f`.
    Ann(Box<LPat>, LType),
    Lit(Lit),
    As(Ident, LPat),
    Cons(Ident, Vec<LPat>),
    /// `Mod.Ctor p q` — a constructor pattern qualified by a `use`d module.
    QualCons(Ident, Ident, Vec<LPat>),
    Tuple(Vec<LPat>),
    /// `#[p, ...]` -- matches a builtin `Array` of exactly this length.
    Array(Vec<LPat>),
    /// `[]` / `[p, q]` — a `Vector` pattern. Only the empty one is supported;
    /// `Vector` is a library type with no structural form, so the resolver
    /// rejects the rest rather than the parser, which lets it say why.
    Vector(Vec<LPat>),
    /// `[;]` / `[p; q]` / `[p;]` — a `List` pattern. The `;` marks it, as it does
    /// in the `[x; y]` literal and the `[a;]` type.
    List(Vec<LPat>),
    /// `{ x, y = p | _ }` — `open` (the trailing `| _`) is the bool.
    Record(Vec<(Ident, LPat)>, bool),
    Unit,
}

pub type Ident = Located<InternedString>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    /// Fixed-width integer — the default numeric literal (`Int`, i.e. i64). There
    /// is no `BigInt` literal; use `toBigInt` and the `~`-suffixed operators.
    Int(i64),
    /// A floating-point literal, stored as its IEEE-754 bit pattern so the AST
    /// stays `Eq` / `Hash`; decode with `f64::from_bits`.
    Float(u64),
    String(InternedString),
    /// A single Unicode scalar: `'a'`, `'\n'`.
    Char(char),
}
