use crate::{span::Located, token::Token};
use itertools::Either;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prog {
    File(LModule),
    Interactive(Either<LDecl, LExpr>),
}

pub type LModule = Located<Module>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Located<Decl>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
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
    Unit,
}

pub type Ident = Located<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    Int(i64),
    String(String),
}
