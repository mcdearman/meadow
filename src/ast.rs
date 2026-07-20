use crate::{span::Located, token::Token};

pub type Prog<'src> = Located<Module<'src>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module<'src> {
    pub name: &'src str,
    pub decls: Vec<LDecl<'src>>,
}

pub type LDecl<'src> = Located<Decl<'src>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl<'src> {
    Bind(Bind<'src>),
}

pub type LExpr<'src> = Located<Expr<'src>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr<'src> {
    Var(Ident<'src>),
    Lit(Lit<'src>),
    Lam(Vec<LPat<'src>>, LExpr<'src>),
    App(LExpr<'src>, Vec<LExpr<'src>>),
    Let(Vec<Bind<'src>>, LExpr<'src>),
    If(LExpr<'src>, LExpr<'src>, LExpr<'src>),
    Match(LExpr<'src>, Vec<(LPat<'src>, LExpr<'src>)>),
    UnOp(LUnOp, LExpr<'src>),
    BinOp(LBinOp, LExpr<'src>, LExpr<'src>),
    Tuple(Vec<LExpr<'src>>),
    List(Vec<LExpr<'src>>),
    Cons(Ident<'src>, Vec<LExpr<'src>>),
    Unit,
}

pub type LUnOp = Located<UnOp>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl From<UnOp> for &str {
    fn from(op: UnOp) -> Self {
        match op {
            UnOp::Neg => "-",
            UnOp::Not => "!",
        }
    }
}

impl<'a> From<Token<'a>> for UnOp {
    fn from(token: Token<'a>) -> Self {
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

impl From<BinOp> for &str {
    fn from(op: BinOp) -> Self {
        match op {
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
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind<'src> {
    Pat(LPat<'src>, LExpr<'src>),
    Fun(Ident<'src>, Vec<Ident<'src>>, LExpr<'src>),
}

pub type LPat<'src> = Located<Pat<'src>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat<'src> {
    Wildcard,
    Var(Ident<'src>),
    Lit(Lit<'src>),
    As(Ident<'src>, LPat<'src>),
    Cons(Ident<'src>, Vec<LPat<'src>>),
    Tuple(Vec<LPat<'src>>),
    List(Vec<LPat<'src>>),
    Unit,
}

pub type Ident<'src> = Located<&'src str>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lit<'src> {
    Int(i64),
    String(&'src str),
}
