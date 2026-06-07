use crate::{intern::InternedString, span::Located, token::Token};

pub type Prog = Located<Module>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
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
    Case(LExpr, Vec<(LPat, LExpr)>),
    UnOp(LUnOp, LExpr),
    BinOp(LBinOp, LExpr, LExpr),
    Tuple(Vec<LExpr>),
    List(Vec<LExpr>),
    Unit,
}

pub type LUnOp = Located<UnOp>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl From<UnOp> for InternedString {
    fn from(op: UnOp) -> Self {
        match op {
            UnOp::Neg => "-".into(),
            UnOp::Not => "!".into(),
        }
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
    Eq,
    Neq,
    Lt,
    Gt,
    Leq,
    Geq,
}

impl From<BinOp> for InternedString {
    fn from(op: BinOp) -> Self {
        match op {
            BinOp::Add => "+".into(),
            BinOp::Sub => "-".into(),
            BinOp::Mul => "*".into(),
            BinOp::Div => "/".into(),
            BinOp::Eq => "==".into(),
            BinOp::Neq => "!=".into(),
            BinOp::Lt => "<".into(),
            BinOp::Gt => ">".into(),
            BinOp::Leq => "<=".into(),
            BinOp::Geq => ">=".into(),
        }
    }
}

impl From<Token> for BinOp {
    fn from(token: Token) -> Self {
        match token {
            Token::Plus => BinOp::Add,
            Token::Minus => BinOp::Sub,
            Token::Star => BinOp::Mul,
            Token::Slash => BinOp::Div,
            Token::Eq => BinOp::Eq,
            Token::Neq => BinOp::Neq,
            Token::Lt => BinOp::Lt,
            Token::Gt => BinOp::Gt,
            Token::Leq => BinOp::Leq,
            Token::Geq => BinOp::Geq,
            _ => panic!("Invalid token for binary operator: {:?}", token),
        }
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

pub type Ident = Located<InternedString>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit {
    Int(i64),
    String(String),
}
