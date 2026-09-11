//! **Pattern coverage**: exhaustiveness of `match`, and irrefutability of the
//! patterns that appear in binding position.
//!
//! The engine is Maranget's *usefulness* algorithm ("Warnings for pattern
//! matching", JFP 2007), in its witness-producing form: given the matrix of a
//! `match`'s patterns, [`Checker::missing`] either proves every value is covered
//! or hands back a concrete value that no row matches, which is what the
//! diagnostic prints.
//!
//! It is **type-directed** — each column carries the inferred [`Type`] of the
//! values in it, so the complete constructor set at that position is known
//! exactly (`Maybe a` is `None | Just _`, `Int` is unbounded, a tuple or a record
//! has one constructor, an opaque type has none it can enumerate).
//!
//! Two checks share the machinery:
//!
//! * **`match` exhaustiveness** — gated on the compiler's `check_exhaustive`
//!   option, so it is off in the debug profile and on in release.
//! * **Irrefutability** of a function parameter, lambda parameter or handler
//!   clause parameter — a single-row matrix. This one is *always* on: those
//!   positions have nowhere to jump on a failed match, so a refutable pattern
//!   there is an error rather than a lint.

use meadow_diagnostics::Diagnostic;
use meadow_hir as hir;
use meadow_infer::{subst_bound, Type, TypeTable, VariantEnv};
use meadow_intern::InternedString;
use meadow_span::Span;

/// Run both checks over one module.
///
/// `check_matches` enables the `match`-exhaustiveness check; irrefutability of
/// binding positions is checked regardless.
pub fn check_module(
    filename: &str,
    module: &hir::LModule,
    types: &TypeTable,
    variants: &VariantEnv,
    check_matches: bool,
) -> Vec<Diagnostic> {
    let mut c = Checker {
        filename: filename.to_string(),
        types,
        variants,
        check_matches,
        errors: Vec::new(),
    };
    for decl in &module.value().decls {
        c.decl(decl);
    }
    c.errors
}

// ===========================================================================
// Patterns, reduced to constructor trees
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
enum P {
    Wild,
    Con(Con, Vec<P>),
}

/// A pattern's head constructor. Two patterns overlap only when their heads are
/// equal, so this is the key the matrix is split on.
#[derive(Debug, Clone, PartialEq)]
enum Con {
    /// A `data` / `record` constructor.
    Variant(InternedString),
    /// An n-tuple — the sole constructor of `(a, b, …)`.
    Tuple(usize),
    /// A structural record — the sole constructor of `{ l : a, … }`, carrying its
    /// fields in canonical (sorted) order.
    Record(Vec<InternedString>),
    Unit,
    Int(i64),
    Str(InternedString),
    Char(char),
    /// A float literal, by bit pattern (so it stays comparable).
    Float(u64),
    /// `#[p, …]` — an array of exactly this length.
    Array(usize),
}

// ===========================================================================
// The checker
// ===========================================================================

struct Checker<'a> {
    filename: String,
    types: &'a TypeTable,
    variants: &'a VariantEnv,
    check_matches: bool,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    // --- walking the tree ---------------------------------------------------

    fn decl(&mut self, decl: &hir::LDecl) {
        match decl.value() {
            hir::Decl::Bind(b) => self.bind(b),
            _ => {}
        }
    }

    fn bind(&mut self, bind: &hir::Bind) {
        match bind {
            hir::Bind::Fun(_, params, _, body) => {
                for p in params {
                    self.require_irrefutable(p, "function parameter");
                }
                self.expr(body);
            }
            // `def (a, b) = e` / `let p = e` destructure without a fallback arm,
            // so they must be irrefutable too.
            hir::Bind::Pat(pat, expr) => {
                self.require_irrefutable(pat, "binding");
                self.expr(expr);
            }
            hir::Bind::Error => {}
        }
    }

    fn expr(&mut self, expr: &hir::LExpr) {
        match expr.value() {
            hir::Expr::Lam(params, body) => {
                for p in params {
                    self.require_irrefutable(p, "lambda parameter");
                }
                self.expr(body);
            }
            hir::Expr::Match(scrut, arms) => {
                self.expr(scrut);
                for (_, body) in arms {
                    self.expr(body);
                }
                if self.check_matches {
                    self.check_match(expr.span, scrut, arms);
                }
            }
            hir::Expr::Handle(body, arms, ret) => {
                self.expr(body);
                for arm in arms {
                    self.require_irrefutable(&arm.param, "handler parameter");
                    self.expr(&arm.body);
                }
                if let Some((p, b)) = ret {
                    self.require_irrefutable(p, "handler `return` parameter");
                    self.expr(b);
                }
            }
            hir::Expr::App(f, args) => {
                self.expr(f);
                args.iter().for_each(|a| self.expr(a));
            }
            hir::Expr::Let(binds, body) => {
                binds.iter().for_each(|b| self.bind(b));
                self.expr(body);
            }
            hir::Expr::If(c, t, e) => {
                self.expr(c);
                self.expr(t);
                self.expr(e);
            }
            hir::Expr::Tuple(xs)
            | hir::Expr::Array(xs)
            | hir::Expr::List(xs)
            | hir::Expr::Cons(_, xs) => xs.iter().for_each(|x| self.expr(x)),
            hir::Expr::Record(fields, base) => {
                fields.iter().for_each(|(_, e)| self.expr(e));
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            hir::Expr::Field(o, _) => self.expr(o),
            hir::Expr::Var(_) | hir::Expr::Lit(_) | hir::Expr::Unit | hir::Expr::Error => {}
        }
    }

    // --- the two checks -----------------------------------------------------

    fn check_match(&mut self, span: Span, scrut: &hir::LExpr, arms: &[(hir::LPat, hir::LExpr)]) {
        let Some(ty) = self.types.get(scrut.id).cloned() else {
            return; // untyped (an earlier error) — nothing reliable to say
        };
        let rows: Vec<Vec<P>> = arms.iter().map(|(p, _)| vec![self.lower(p)]).collect();
        if let Some(w) = self.missing(&rows, &[ty]) {
            let witness = render(&w[0]);
            self.error(
                format!("non-exhaustive patterns: `{witness}` is not matched"),
                "this `match` does not cover every case".to_string(),
                span,
            );
        }
    }

    fn require_irrefutable(&mut self, pat: &hir::LPat, what: &str) {
        // A bare variable or `_` is irrefutable by construction; skip the work.
        if matches!(pat.value(), hir::Pat::Var(_) | hir::Pat::Wildcard | hir::Pat::Error) {
            return;
        }
        let Some(ty) = self.types.get(pat.id).cloned() else {
            return;
        };
        let rows = vec![vec![self.lower(pat)]];
        if let Some(w) = self.missing(&rows, &[ty]) {
            let witness = render(&w[0]);
            self.error(
                format!("refutable pattern in {what}: `{witness}` is not matched"),
                format!("a {what} must match every value"),
                pat.span,
            );
        }
    }

    fn error(&mut self, msg: String, label: String, span: Span) {
        self.errors.push(Diagnostic {
            msg,
            filename: self.filename.clone(),
            label: (label, span),
            extra_labels: vec![],
        });
    }

    // --- hir::Pat -> P ------------------------------------------------------

    fn lower(&self, pat: &hir::LPat) -> P {
        match pat.value() {
            hir::Pat::Wildcard | hir::Pat::Var(_) | hir::Pat::Error => P::Wild,
            // An annotation constrains the type, never the shape.
            hir::Pat::Ann(inner, _) => self.lower(inner),
            hir::Pat::As(_, sub) => self.lower(sub),
            hir::Pat::Unit => P::Con(Con::Unit, vec![]),
            hir::Pat::Lit(hir::Lit::Int(i)) => P::Con(Con::Int(*i), vec![]),
            hir::Pat::Lit(hir::Lit::String(s)) => P::Con(Con::Str(*s), vec![]),
            hir::Pat::Lit(hir::Lit::Char(c)) => P::Con(Con::Char(*c), vec![]),
            hir::Pat::Lit(hir::Lit::Float(b)) => P::Con(Con::Float(*b), vec![]),
            hir::Pat::Tuple(ps) => P::Con(
                Con::Tuple(ps.len()),
                ps.iter().map(|p| self.lower(p)).collect(),
            ),
            hir::Pat::Array(ps) => P::Con(
                Con::Array(ps.len()),
                ps.iter().map(|p| self.lower(p)).collect(),
            ),
            // `[a; b]` is sugar for `Cons a (Cons b Nil)`.
            hir::Pat::List(ps) => ps.iter().rev().fold(
                P::Con(Con::Variant(InternedString::from("List.Nil")), vec![]),
                |tail, p| {
                    P::Con(
                        Con::Variant(InternedString::from("List.Cons")),
                        vec![self.lower(p), tail],
                    )
                },
            ),
            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                P::Con(
                    Con::Variant(name),
                    args.iter().map(|p| self.lower(p)).collect(),
                )
            }
            // Canonicalize against the record's own type, so every arm of a match
            // presents the same field list in the same order.
            hir::Pat::Record(fields, _) => {
                let labels = match self.types.get(pat.id) {
                    Some(t) => record_labels(t),
                    None => None,
                };
                let labels = labels.unwrap_or_else(|| {
                    let mut ls: Vec<InternedString> =
                        fields.iter().map(|(l, _)| *l.value()).collect();
                    ls.sort_by(|a, b| (**a).cmp(&**b));
                    ls
                });
                let args = labels
                    .iter()
                    .map(|l| match fields.iter().find(|(fl, _)| fl.value() == l) {
                        Some((_, p)) => self.lower(p),
                        None => P::Wild,
                    })
                    .collect();
                P::Con(Con::Record(labels), args)
            }
        }
    }

    // --- constructor sets ---------------------------------------------------

    /// Every constructor of `ty`, each with the types of its fields — or `None`
    /// when the set is unbounded (`Int`, `String`, arrays of any length) or simply
    /// unknown (a type variable, an opaque type). `None` means only a wildcard can
    /// cover the column.
    fn ctors_of(&self, ty: &Type) -> Option<Vec<(Con, Vec<Type>)>> {
        match ty {
            Type::Tuple(ts) => Some(vec![(Con::Tuple(ts.len()), ts.clone())]),
            Type::Record(_) => {
                let labels = record_labels(ty)?;
                let row = record_fields(ty);
                let tys = labels
                    .iter()
                    .map(|l| row.get(l).cloned().unwrap_or(Type::RowEmpty))
                    .collect();
                Some(vec![(Con::Record(labels), tys)])
            }
            Type::Con(name, args) => {
                if let Some(vs) = self.variants.get(name) {
                    // A constructor's field types are written over the *type's*
                    // parameters as `Bound(i)`, so substituting them needs an
                    // argument per parameter. A type written with the wrong
                    // number — `x : Maybe`, which the resolver has already
                    // reported — would index past the end, and inference runs
                    // after a resolver error on purpose, to collect more than
                    // one problem per compile. So: treat a type whose arity does
                    // not add up as opaque, and let the reported error stand.
                    let arity = vs.iter().flat_map(|v| &v.fields).filter_map(max_bound).max();
                    if arity.is_some_and(|n| n as usize >= args.len()) {
                        return None;
                    }
                    return Some(
                        vs.iter()
                            .map(|v| {
                                let fields =
                                    v.fields.iter().map(|f| subst_bound(f, args)).collect();
                                (Con::Variant(v.name), fields)
                            })
                            .collect(),
                    );
                }
                // Guarded builtins — known even when the declaring `Std` module is
                // not in scope. `Std.Collections.Vector` matches on `Nil` / `Cons`
                // and is compiled before `Std.Collections.List`.
                let con = |n: &str| Con::Variant(InternedString::from(n));
                match &**name {
                    "Bool" => Some(vec![(con("Bool.False"), vec![]), (con("Bool.True"), vec![])]),
                    "List" => {
                        let elem = args.first().cloned().unwrap_or(Type::RowEmpty);
                        Some(vec![
                            (con("List.Nil"), vec![]),
                            (con("List.Cons"), vec![elem, ty.clone()]),
                        ])
                    }
                    "Unit" => Some(vec![(Con::Unit, vec![])]),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // --- Maranget's algorithm ------------------------------------------------

    /// `None` when `rows` covers every value of `col_types`; otherwise a witness
    /// vector that no row matches.
    fn missing(&self, rows: &[Vec<P>], col_types: &[Type]) -> Option<Vec<P>> {
        // Base case: zero columns. One row (even an empty one) covers the single
        // zero-width value; no rows covers nothing.
        if col_types.is_empty() {
            return if rows.is_empty() { Some(vec![]) } else { None };
        }
        let ty = &col_types[0];
        let used: Vec<Con> = heads(rows);
        let all = self.ctors_of(ty);
        let complete = all
            .as_ref()
            .is_some_and(|cs| !cs.is_empty() && cs.iter().all(|(c, _)| used.contains(c)));

        if complete {
            // Every constructor appears: the match is exhaustive iff it is
            // exhaustive under each one.
            for (con, sub) in all.expect("complete implies Some") {
                let arity = sub.len();
                let spec = specialize(rows, &con, arity);
                let mut tys = sub;
                tys.extend_from_slice(&col_types[1..]);
                if let Some(w) = self.missing(&spec, &tys) {
                    return Some(rebuild(con, arity, w));
                }
            }
            return None;
        }

        // Some constructor is unused (or the set is unbounded): the witness is
        // one of those, followed by a witness for the remaining columns.
        let rest = self.missing(&default_matrix(rows), &col_types[1..])?;
        let head = match all.and_then(|cs| cs.into_iter().find(|(c, _)| !used.contains(c))) {
            Some((con, sub)) => P::Con(con, vec![P::Wild; sub.len()]),
            None => P::Wild,
        };
        let mut out = vec![head];
        out.extend(rest);
        Some(out)
    }
}

// ===========================================================================
// Matrix operations
// ===========================================================================

/// The distinct head constructors of column 0.
fn heads(rows: &[Vec<P>]) -> Vec<Con> {
    let mut out: Vec<Con> = Vec::new();
    for row in rows {
        if let Some(P::Con(c, _)) = row.first() {
            if !out.contains(c) {
                out.push(c.clone());
            }
        }
    }
    out
}

/// `S(con, rows)` — keep the rows that can match `con`, replacing the head with
/// its arguments.
fn specialize(rows: &[Vec<P>], con: &Con, arity: usize) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for row in rows {
        let Some(head) = row.first() else { continue };
        match head {
            P::Con(c, args) if c == con => {
                let mut new: Vec<P> = args.clone();
                new.extend_from_slice(&row[1..]);
                out.push(new);
            }
            P::Con(_, _) => {}
            P::Wild => {
                let mut new = vec![P::Wild; arity];
                new.extend_from_slice(&row[1..]);
                out.push(new);
            }
        }
    }
    out
}

/// `D(rows)` — the rows whose head matches any *unlisted* constructor.
fn default_matrix(rows: &[Vec<P>]) -> Vec<Vec<P>> {
    rows.iter()
        .filter(|row| matches!(row.first(), Some(P::Wild)))
        .map(|row| row[1..].to_vec())
        .collect()
}

/// Put `arity` of `w`'s leading entries back under `con`.
fn rebuild(con: Con, arity: usize, w: Vec<P>) -> Vec<P> {
    let mut it = w.into_iter();
    let args: Vec<P> = (&mut it).take(arity).collect();
    let mut out = vec![P::Con(con, args)];
    out.extend(it);
    out
}

// ===========================================================================
// Types: rows
// ===========================================================================

/// The labels of a record type, in canonical (sorted) order.
fn record_labels(ty: &Type) -> Option<Vec<InternedString>> {
    let Type::Record(row) = ty else { return None };
    let mut ls: Vec<InternedString> = row_fields(row).into_iter().map(|(l, _)| l).collect();
    ls.sort_by(|a, b| (**a).cmp(&**b));
    Some(ls)
}

fn record_fields(ty: &Type) -> std::collections::HashMap<InternedString, Type> {
    match ty {
        Type::Record(row) => row_fields(row).into_iter().collect(),
        _ => Default::default(),
    }
}

fn row_fields(row: &Type) -> Vec<(InternedString, Type)> {
    let mut out = Vec::new();
    let mut cur = row;
    while let Type::RowExtend(label, field, rest) = cur {
        out.push((*label, (**field).clone()));
        cur = rest;
    }
    out
}

// ===========================================================================
// Rendering a witness
// ===========================================================================

fn render(p: &P) -> String {
    render_at(p, false)
}

fn render_at(p: &P, nested: bool) -> String {
    match p {
        P::Wild => "_".to_string(),
        P::Con(con, args) => match con {
            Con::Unit => "()".to_string(),
            Con::Int(i) => i.to_string(),
            Con::Str(s) => format!("{:?}", &**s),
            Con::Char(c) => format!("{c:?}"),
            Con::Float(b) => f64::from_bits(*b).to_string(),
            Con::Tuple(_) => {
                let parts: Vec<String> = args.iter().map(|a| render_at(a, false)).collect();
                format!("({})", parts.join(", "))
            }
            Con::Array(_) => {
                let parts: Vec<String> = args.iter().map(|a| render_at(a, false)).collect();
                format!("#[{}]", parts.join(", "))
            }
            Con::Record(labels) => {
                let parts: Vec<String> = labels
                    .iter()
                    .zip(args)
                    .map(|(l, a)| format!("{l} = {}", render_at(a, false)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            // Bare, like every other place a constructor is shown to a
            // person: the canonical `Maybe.None` exists so the compiler can
            // tell two types' constructors apart, and a reader looking at a
            // missing case already knows which type they are matching on.
            Con::Variant(name) if args.is_empty() => bare_ctor(name),
            Con::Variant(name) => {
                let parts: Vec<String> = args.iter().map(|a| render_at(a, true)).collect();
                let s = format!("{} {}", bare_ctor(name), parts.join(" "));
                if nested {
                    format!("({s})")
                } else {
                    s
                }
            }
        },
    }
}

/// The largest `Bound` index a type mentions, if any.
///
/// Used to check that a type constructor was written with enough arguments to
/// substitute for its parameters — see the note in `ctors_of`.
fn max_bound(t: &Type) -> Option<u32> {
    match t {
        Type::Bound(i) => Some(*i),
        Type::Var(_) | Type::RowEmpty => None,
        Type::Con(_, args) | Type::Tuple(args) => args.iter().filter_map(max_bound).max(),
        Type::Fun(ps, r, e) => ps
            .iter()
            .chain([&**r, &**e])
            .filter_map(max_bound)
            .max(),
        Type::Record(r) => max_bound(r),
        Type::RowExtend(_, f, rest) => [&**f, &**rest].into_iter().filter_map(max_bound).max(),
    }
}

/// The bare spelling of a canonical constructor name, for a message a person
/// reads: `Maybe.None` -> `None`.
fn bare_ctor(name: &InternedString) -> String {
    let n = name.to_string();
    match n.rsplit_once('.') {
        Some((_, c)) => c.to_string(),
        None => n,
    }
}
