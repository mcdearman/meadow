use crate::{intern::InternedString, lexer::Token, span::Located};

pub type LModule = Located<Module>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub name: InternedString,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Located<Decl>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
    /// `use a.b.c`
    Use(Vec<Ident>),
    /// `data Node a = Leaf (Vector a) | Internal { ... }`
    Data(DataDecl),
    /// `record Person = { name : String }`
    Record(RecordDecl),
}

pub type LType = Located<TypeExpr>;

/// A type expression as written in a `data` / `record` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    /// lowercase type variable: `a`
    Var(Ident),
    /// type-constructor application: `Int`, `Vector a`, `Maybe (Vector Int)`
    Con(Ident, Vec<LType>),
    /// `a -> b` (curried when lowered)
    Fun(Vec<LType>, LType),
    /// `(a, b)`
    Tuple(Vec<LType>),
    /// `[a]`
    List(LType),
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
    Named(Vec<(Ident, LType)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDecl {
    pub name: Ident,
    pub params: Vec<Ident>,
    pub fields: Vec<(Ident, LType)>,
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
    List(Vec<LExpr>),
    Cons(Ident, Vec<LExpr>),
    /// `{ x = e, y = e | base }` — trailing expr is the record being extended.
    Record(Vec<(Ident, LExpr)>, Option<LExpr>),
    /// `e.label`
    Field(LExpr, Ident),
    Unit,
}

pub type LUnOp = Located<UnOp>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl ToString for UnOp {
    fn to_string(&self) -> String {
        match self {
            UnOp::Neg => "neg",
            UnOp::Not => "!",
        }
        .to_string()
    }
}

impl From<Token> for UnOp {
    fn from(token: Token) -> Self {
        match token {
            Token::Minus => UnOp::Neg,
            Token::Bang => UnOp::Not,
            _ => panic!("Invalid token for unary operator: {:?}", token),
        }
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
        }
        .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    Pat(LPat, LExpr),
    Fun(Ident, Vec<Ident>, LExpr),
}

pub type LPat = Located<Pat>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Wildcard,
    Var(Ident),
    Lit(Lit),
    As(Ident, LPat),
    Cons(Ident, Vec<LPat>),
    Tuple(Vec<LPat>),
    List(Vec<LPat>),
    /// `{ x, y = p | _ }` — `open` (the trailing `| _`) is the bool.
    Record(Vec<(Ident, LPat)>, bool),
    Unit,
}

pub type Ident = Located<InternedString>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    Int(i64),
    String(InternedString),
}
