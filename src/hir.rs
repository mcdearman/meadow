use crate::{intern::InternedString, span::Located};
use itertools::Either;
use std::sync::atomic::AtomicU32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prog {
    File(LModule),
    Interactive(Either<LDecl, LExpr>),
}

pub type LModule = Located<Module>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: InternedString,
    pub decls: Vec<LDecl>,
}

pub type LDecl = Located<Decl>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Bind(Bind),
    Error,
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
    Tuple(Vec<LExpr>),
    List(Vec<LExpr>),
    Cons(Ident, Vec<LExpr>),
    Unit,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    Pat(LPat, LExpr),
    Fun(Ident, Vec<Ident>, LExpr),
    Error,
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
    Error,
}

pub type Ident = Located<VarId>;

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
