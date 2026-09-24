//! **Compile-time bindings**: a value one macro leaves for another.
//!
//! A macro that defines an embedded language and one that writes a pass over
//! it cannot talk through tokens alone -- by the time the second runs, the
//! first is gone. So a macro may `define` a name to stand for a [`Datum`], and
//! a later one may `lookup` it (see `Std.Macro`'s `Expand`).
//!
//! What is stored is data with a fixed shape, not a value of some type of the
//! defining package's: it crosses packages, is cached, and is read by macros
//! compiled against other versions of whatever declared the type. A `Datum`
//! means the same thing to all of them.

use super::Vis;
use meadow_ast as ast;
use meadow_intern::InternedString;
use meadow_lexer::tt;

/// `Std.Macro.Datum`, as the compiler holds it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Datum {
    Sym(String),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Datum>),
    /// Named fields, in the order they were written.
    Rec(Vec<(String, Datum)>),
    /// A constructor, and what it is applied to.
    Tag(String, Vec<Datum>),
    /// Source, as tokens: what a macro that writes code splices later.
    Code(Vec<tt::TokenTree>),
}

/// A name that stands for a [`Datum`], and where it may be seen from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub name: InternedString,
    /// The module it was defined in, as a path within its package.
    pub module: Vec<InternedString>,
    pub package: InternedString,
    pub vis: Vis,
    pub value: Datum,
}

impl Binding {
    /// Whether a module at `path` in package `pkg` may name this.
    pub fn visible_to(&self, pkg: InternedString, path: &[InternedString]) -> bool {
        super::visible(self.vis, self.package, &self.module, pkg, path)
    }
}

/// The datum a hand-written `@compileTime def` stands for: its right-hand side
/// read as data, never evaluated.
///
/// A name is a `Sym`, a literal is itself, `True` and `False` are `Bool`s, a
/// constructor applied to things is a `Tag`, a list, vector or tuple is a
/// `List`, and a record is a `Rec`. Anything else -- an operator, a lambda, a
/// call of a function -- would need running, and is refused.
pub fn of_expr(e: &ast::LExpr) -> Result<Datum, (String, meadow_span::Span)> {
    let refuse = |what: &str| {
        Err((
            format!("a compile-time value is data, and {what} is not"),
            e.span,
        ))
    };
    match &*e.value {
        ast::Expr::Var(n) => Ok(Datum::Sym(n.value().to_string())),
        ast::Expr::Lit(ast::Lit::Int(n)) => Ok(Datum::Int(*n)),
        ast::Expr::Lit(ast::Lit::Float(bits)) => Ok(Datum::Float(f64::from_bits(*bits))),
        ast::Expr::Lit(ast::Lit::String(s)) => Ok(Datum::Str(s.to_string())),
        ast::Expr::Lit(ast::Lit::Char(c)) => Ok(Datum::Str(c.to_string())),
        ast::Expr::Interp(parts, holes) if holes.is_empty() => {
            Ok(Datum::Str(parts.iter().map(|p| p.to_string()).collect()))
        }
        ast::Expr::Unit => Ok(Datum::List(Vec::new())),
        ast::Expr::Cons(name, args) => tag(name.value(), args),
        ast::Expr::App(f, args) => match &*f.value {
            ast::Expr::Cons(name, first) if first.is_empty() => tag(name.value(), args),
            _ => refuse("a call"),
        },
        ast::Expr::Tuple(xs) | ast::Expr::Array(xs) | ast::Expr::List(xs) => Ok(Datum::List(
            xs.iter().map(of_expr).collect::<Result<_, _>>()?,
        )),
        ast::Expr::Record(fields, None) => Ok(Datum::Rec(
            fields
                .iter()
                .map(|(k, v)| Ok((k.value().to_string(), of_expr(v)?)))
                .collect::<Result<_, _>>()?,
        )),
        ast::Expr::UnOp(op, x) if matches!(op.value(), ast::UnOp::Neg) => match of_expr(x)? {
            Datum::Int(n) => Ok(Datum::Int(-n)),
            Datum::Float(f) => Ok(Datum::Float(-f)),
            _ => refuse("a negation of something that is not a number"),
        },
        _ => refuse("this"),
    }
}

fn tag(name: &str, args: &[ast::LExpr]) -> Result<Datum, (String, meadow_span::Span)> {
    match (name, args.is_empty()) {
        ("True", true) => Ok(Datum::Bool(true)),
        ("False", true) => Ok(Datum::Bool(false)),
        _ => Ok(Datum::Tag(
            name.to_string(),
            args.iter().map(of_expr).collect::<Result<_, _>>()?,
        )),
    }
}
