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
use meadow_lexer::tt::Group;
use meadow_span::{Located, Span};

/// **Hygiene marks**: how a name a macro template wrote is told from a name the
/// call site wrote.
///
/// A marked name is `tmp#3`, the number being the expansion's. The `#` is what
/// makes it safe: an identifier cannot contain one, so no source can spell a
/// marked name and no two expansions collide. See
/// [`meadow_compiler::expand::hygiene`] for what the marks are for; this is
/// only how they are written, kept here so that the resolver can read one
/// without depending on the expander.
pub mod hygiene {
    use meadow_intern::InternedString;

    /// Not lexable inside an identifier, so a marked name is unforgeable.
    const MARK: char = '#';

    /// `name` as the expansion `id` wrote it.
    pub fn mark(name: InternedString, id: u32) -> InternedString {
        InternedString::from(format!("{name}{MARK}{id}"))
    }

    /// Whether this name came from a template.
    pub fn is_marked(name: InternedString) -> bool {
        name.contains(MARK)
    }

    /// `name` without its mark, or `name` when it has none. A name marked by
    /// nested expansions loses one mark at a time, outermost first.
    pub fn strip(name: InternedString) -> InternedString {
        match name.rfind(MARK) {
            Some(i) => InternedString::from(&name[..i]),
            None => name,
        }
    }
}

pub type LModule = Located<Module>;

/// An **unexpanded macro call**: `assertEq!(got, want)`, `vec![1; 2]`,
/// `config! { name = "demo" }`.
///
/// The argument is token trees, not a parsed anything: the parser stops at the
/// brackets and the expander decides what the tokens mean. One of these in the
/// tree means expansion has not run yet -- nothing past [`meadow_rename`] ever
/// sees one, because expansion either replaces it or reports why it could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacCall {
    /// The name, qualified if it was written that way: `Std.Test.assertEq!` is
    /// three segments. Never empty.
    pub path: Vec<Ident>,
    /// What was between the brackets, and which brackets they were.
    pub arg: Group,
}

impl MacCall {
    /// The name as written, for a message and for finding the macro:
    /// `assertEq`, `Std.Test.assertEq`.
    ///
    /// Hygiene marks come off: a macro is an item, so a template that calls
    /// another macro -- or itself, which is how a recursive one is written --
    /// means the macro of that name and not one private to the expansion.
    pub fn name(&self) -> String {
        self.path
            .iter()
            .map(|s| hygiene::strip(*s.value()).to_string())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// Where the name was written -- the whole path, without the argument. What
    /// an "unknown macro" points at.
    pub fn path_span(&self) -> Span {
        let first = self.path.first().expect("a call has a name").span;
        self.path.last().map_or(first, |l| first.extend(l.span))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub name: InternedString,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Located<Decl>;

/// An attribute: `@pub`, `@attr(A, B, C)`, `@cfg(all(unix, not(test)))`.
/// Attached to a declaration (via [`Decl::Attributed`]) or a record /
/// named-variant field or effect operation (see [`Field`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: Ident,
    /// The arguments that are plain names, in order: `pkg` of `@pub(pkg)`.
    pub args: Vec<Ident>,
    /// Every argument as written, names and the rest: what `@cfg` reads.
    pub meta: Vec<Meta>,
}

/// One argument of an attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meta {
    /// `unix`
    Word(Ident),
    /// `"+"` -- a string on its own, as `@token("+")` writes one. What it
    /// means is whatever reads the attribute; nothing in the language does.
    Text(Ident),
    /// `os = "linux"`
    Value(Ident, Ident),
    /// `not(test)`, `all(a, b)`
    List(Ident, Vec<Meta>),
}

impl Meta {
    /// The name it starts with: `os` of `os = "linux"`, `all` of `all(…)`.
    /// A bare string has no name of its own and answers with its text.
    pub fn name(&self) -> &Ident {
        match self {
            Meta::Word(n) | Meta::Value(n, _) | Meta::List(n, _) | Meta::Text(n) => n,
        }
    }
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
    /// `type Span = (Int, Int)` -- another name for a type, which means exactly
    /// what it stands for.
    TypeAlias(TypeAliasDecl),
    /// `fun name : T` / `def name : T` -- the type of a top-level binding,
    /// declared on a line of its own. Its variables are the binding's to be
    /// general in, as a Haskell signature's are.
    Sig(Ident, LType),
    /// One or more `@attr` lines in front of another declaration.
    Attributed(Vec<Attr>, Box<LDecl>),
    /// `derive! { Show for Colour }` -- a macro call standing where a
    /// declaration goes, until it is expanded into some.
    MacCall(MacCall),
    /// `macro swap | ($a, $b) -> { ($b, $a) }` -- a macro definition. Read by
    /// expansion and gone before name resolution: a macro is not a value, and
    /// nothing downstream has anywhere to put one.
    Macro(MacroDef),
}

/// A `macro` declaration: a name and the rules tried in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDef {
    pub name: Ident,
    pub rules: Vec<MacroRule>,
}

/// One arm of a macro: `| ⟨matcher⟩ -> { ⟨template⟩ }`.
///
/// Both sides are token trees, and the outer brackets of each are only how it
/// is written: the three brackets mean the same thing at a call, so a matcher
/// written with `( )` matches a call written with `[ ]`. What is matched, and
/// what is spliced in, is what lies inside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroRule {
    pub matcher: Group,
    pub template: Group,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseDecl {
    pub path: Vec<Ident>,
    /// Selected names — `use a.b (x, y)`. Empty for a bare `use a.b`.
    pub names: Vec<Ident>,
    /// Selected macros — `use a.b (vec!)`. Macros have a namespace of their
    /// own, so `vec` and `vec!` are two names and the `!` is what says which
    /// one is meant. Read by expansion, which is over before the rest of a
    /// `use` means anything.
    pub macros: Vec<Ident>,
    /// A trailing `.*` — `use Syntax.Tv.*`, every constructor of a type
    /// unqualified. Never set together with `names`.
    pub glob: bool,
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
    /// `{ name : String, age : Int }`, closed, or `{ name : String | r }`, open
    /// over the rest of its fields -- a structural record, the type a record
    /// literal has, written the way a hover prints one.
    Record(Vec<(Ident, LType)>, Option<Ident>),
}

/// An effect annotation `! <row>`: `! Console`, `! e`, `! { Console, State Int | e }`.
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
    /// What was written above or before it: `@token("+")` on a variant of a
    /// lexer's token type. Nothing in the language reads these -- they are for
    /// whatever `@derive`s over the declaration.
    pub attrs: Vec<Attr>,
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
pub struct TypeAliasDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub ty: LType,
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
    /// `"a ${x} b ${y}"` -- a string literal with expressions in it: its text,
    /// unescaped, and its holes, alternating. There is one more piece of text
    /// than there are holes; any piece may be empty.
    Interp(Vec<InternedString>, Vec<LExpr>),
    Lam(Vec<LPat>, LExpr),
    App(LExpr, Vec<LExpr>),
    Let(Vec<Bind>, LExpr),
    If(LExpr, LExpr, LExpr),
    /// `match e with | p if guard -> body | …`: each arm a pattern, the
    /// condition it is taken on if it has one, and its body.
    Match(LExpr, Vec<(LPat, Option<LExpr>, LExpr)>),
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
    /// `{ e | x = v, y = w }` -- `e`, a record, with the fields named replaced.
    /// Each must be one it has, and keep its type.
    Update(LExpr, Vec<(Ident, LExpr)>),
    /// `e.label`
    Field(LExpr, Ident),
    /// `handle e with { op p k -> …, return x -> … }`
    Handle(LExpr, Vec<HandlerArm>, Option<(LPat, LExpr)>),
    Unit,
    /// A `_` hole in expression position. Only legal inside an operator section
    /// `( … )`, where the parser rewrites the section into a lambda; anywhere
    /// else the resolver reports it.
    Hole,
    /// `vec![1; 2]` -- a macro call standing where an expression goes, until it
    /// is expanded into one.
    MacCall(MacCall),
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
    /// `p as x` -- `p`, with the whole of what it matched also named `x`.
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
    /// A macro call standing where a pattern goes, until it is expanded into
    /// one.
    MacCall(MacCall),
}

pub type Ident = Located<InternedString>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    /// An integer literal. Its type comes from context: any integer type, or
    /// `BigInt` when nothing settles it.
    Int(i64),
    /// A floating-point literal, stored as its IEEE-754 bit pattern so the AST
    /// stays `Eq` / `Hash`; decode with `f64::from_bits`.
    Float(u64),
    String(InternedString),
    /// A single Unicode scalar: `'a'`, `'\n'`.
    Char(char),
}
