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
use meadow_infer::{Type, TypeTable, VariantEnv, subst_bound};
use meadow_intern::InternedString;
use meadow_span::Span;
use std::cell::RefCell;
use std::collections::HashMap;

/// Run both checks over one module.
///
/// `check_matches` enables the `match`-exhaustiveness check; irrefutability of
/// binding positions is checked regardless.
///
/// `compacting` names the primitives that copy a value into a compact region,
/// each with which of its parameters is that value: see [`Checker::compacted`].
pub fn check_module(
    filename: &str,
    module: &hir::LModule,
    types: &TypeTable,
    variants: &VariantEnv,
    check_matches: bool,
    compacting: &HashMap<hir::VarId, usize>,
    synonyms: &Synonyms,
) -> Vec<Diagnostic> {
    let mut c = Checker {
        filename: filename.to_string(),
        types,
        variants,
        check_matches,
        compacting,
        synonyms,
        seeing: RefCell::new(Vec::new()),
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
    /// A pattern synonym of a set that covers its type together (`pattern
    /// A | B`), by its matcher, with its name for a witness.
    Syn(hir::VarId, InternedString),
    /// `#[p, …]` — an array of exactly this length.
    Array(usize),
}

/// The functions of `modules` that hand one of their parameters straight to a
/// function in `known` -- one that copies it into a compact region -- added
/// to `known` with which parameter, and answered: `Std.Compact.make x =
/// compact x` is checked at each call to `make`, where the type of `x` is
/// known, as `compact` is. To a fixpoint, so a wrapper of a wrapper is one.
pub fn compacting_wrappers(
    modules: &[&hir::LModule],
    known: &mut HashMap<hir::VarId, usize>,
) -> Vec<(hir::VarId, usize)> {
    let mut found = Vec::new();
    loop {
        let before = found.len();
        for m in modules {
            for d in &m.value().decls {
                let hir::Decl::Bind(b) = d.value() else {
                    continue;
                };
                let (name, params, body) = match b {
                    hir::Bind::Fun(name, params, _, body) => (name, params.as_slice(), body),
                    hir::Bind::Pat(pat, e) => match (strip(pat).value(), e.value()) {
                        (hir::Pat::Var(name), hir::Expr::Lam(params, body)) => {
                            (name, params.as_slice(), body)
                        }
                        _ => continue,
                    },
                    hir::Bind::Error => continue,
                };
                if known.contains_key(name.value()) {
                    continue;
                }
                if let Some(at) = forwards(params, body, known) {
                    known.insert(*name.value(), at);
                    found.push((*name.value(), at));
                }
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

/// A pattern synonym whose use can be seen through: its pattern, and its
/// parameters in the order a use gives them.
pub struct Synonym {
    pat: hir::LPat,
    params: Vec<hir::VarId>,
}

/// What the check knows of a package's pattern synonyms.
#[derive(Default)]
pub struct Synonyms {
    /// The ones that can be seen through, by their matchers.
    seen: HashMap<hir::VarId, Synonym>,
    /// The sets that cover their type together, `pattern A | B`: each
    /// synonym's matcher, name and number of parameters.
    sets: Vec<Vec<(hir::VarId, InternedString, usize)>>,
}

impl Synonyms {
    fn get(&self, matcher: &hir::VarId) -> Option<&Synonym> {
        self.seen.get(matcher)
    }

    /// The set `matcher` is in, if one says so.
    fn set_of(&self, matcher: hir::VarId) -> Option<&[(hir::VarId, InternedString, usize)]> {
        self.sets
            .iter()
            .find(|s| s.iter().any(|(m, ..)| *m == matcher))
            .map(|s| s.as_slice())
    }
}

/// The pattern synonyms of `modules` whose patterns say everything about what
/// they match -- no view, and no `as` -- by their matchers: see the parser's
/// `synonym` for what one is. A use of one covers what its pattern does; a
/// use of any other covers nothing, as a view does.
pub fn synonyms(
    modules: &[&hir::LModule],
    names: &HashMap<hir::VarId, InternedString>,
) -> Synonyms {
    let mut found = Synonyms::default();
    // `$m2P`: `P`, of 2.
    let matcher = |v: &hir::VarId| -> Option<(InternedString, usize)> {
        let rest = names.get(v)?.strip_prefix("$m")?;
        let name = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        let arity = rest[..rest.len() - name.len()].parse().ok()?;
        Some((InternedString::from(name), arity))
    };
    for m in modules {
        for d in &m.value().decls {
            // `pattern A | B`, which is `$complete@… = ($m1A, $m0B)`.
            if let hir::Decl::Bind(hir::Bind::Pat(p, e)) = d.value()
                && let hir::Pat::Var(v) = p.value()
                && names
                    .get(v.value())
                    .is_some_and(|n| n.starts_with("$complete"))
            {
                let members = match e.value() {
                    hir::Expr::Tuple(xs) => xs.iter().collect(),
                    _ => vec![e],
                };
                let set: Vec<_> = members
                    .into_iter()
                    .filter_map(|x| match x.value() {
                        hir::Expr::Var(f) => matcher(f.value()).map(|(n, a)| (*f.value(), n, a)),
                        _ => None,
                    })
                    .collect();
                found.sets.push(set);
                continue;
            }
            let hir::Decl::Bind(hir::Bind::Fun(name, params, _, body)) = d.value() else {
                continue;
            };
            let is_matcher = names.get(name.value()).is_some_and(|n| {
                n.strip_prefix("$m")
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            });
            if !is_matcher || params.len() != 1 {
                continue;
            }
            let hir::Expr::Match(_, arms) = body.value() else {
                continue;
            };
            let Some((pat, None, yes)) = arms.first() else {
                continue;
            };
            let var = |e: &hir::LExpr| match e.value() {
                hir::Expr::Var(v) => Some(*v.value()),
                _ => None,
            };
            let params = match yes.value() {
                hir::Expr::Cons(_, args) if args.is_empty() => Some(Vec::new()),
                hir::Expr::List(xs) => match xs.as_slice() {
                    [only] => match only.value() {
                        hir::Expr::Tuple(vs) => vs.iter().map(var).collect(),
                        _ => var(only).map(|v| vec![v]),
                    },
                    _ => None,
                },
                _ => None,
            };
            if let Some(params) = params
                && transparent(pat)
            {
                found.seen.insert(
                    *name.value(),
                    Synonym {
                        pat: pat.clone(),
                        params,
                    },
                );
            }
        }
    }
    found
}

/// What a use of a pattern synonym of `arity` parameters was given: the
/// pattern its matcher's answer is matched against, `True`, `[a;]` or `[(a,
/// b);]` -- see the parser's `synonym`.
fn answered(p: &hir::LPat, arity: usize) -> Option<Vec<&hir::LPat>> {
    match (p.value(), arity) {
        (hir::Pat::Cons(_, none), 0) if none.is_empty() => Some(Vec::new()),
        (hir::Pat::List(one), 1) if one.len() == 1 => Some(vec![&one[0]]),
        (hir::Pat::List(one), n) if one.len() == 1 => match one[0].value() {
            hir::Pat::Tuple(ps) if ps.len() == n => Some(ps.iter().collect()),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `p` says everything about what it matches: whether it has no
/// view but a pattern synonym's use, and names nothing twice with `as`.
fn transparent(p: &hir::LPat) -> bool {
    match p.value() {
        hir::Pat::As(..) => false,
        // A synonym's use: whether that one can be seen through is asked
        // where it is used.
        hir::Pat::View(f, inner) => matches!(f.value(), hir::Expr::Var(_)) && transparent(inner),
        hir::Pat::Ann(inner, _) => transparent(inner),
        hir::Pat::Tuple(ps) | hir::Pat::Array(ps) | hir::Pat::List(ps) | hir::Pat::Cons(_, ps) => {
            ps.iter().all(transparent)
        }
        hir::Pat::Record(fields, _) => fields.iter().all(|(_, q)| transparent(q)),
        hir::Pat::Wildcard
        | hir::Pat::Var(_)
        | hir::Pat::Lit(_)
        | hir::Pat::Unit
        | hir::Pat::Error => true,
    }
}

/// `p` without the type written on it.
fn strip(p: &hir::LPat) -> &hir::LPat {
    match p.value() {
        hir::Pat::Ann(inner, _) => strip(inner),
        _ => p,
    }
}

/// Which of `params` `body` hands to a compacting function as the value it
/// copies, if it is such a call and one of them is.
fn forwards(
    params: &[hir::LPat],
    body: &hir::LExpr,
    known: &HashMap<hir::VarId, usize>,
) -> Option<usize> {
    let hir::Expr::App(f, args) = body.value() else {
        return None;
    };
    let hir::Expr::Var(g) = f.value() else {
        return None;
    };
    let at = *known.get(g.value())?;
    let hir::Expr::Var(x) = args.get(at)?.value() else {
        return None;
    };
    params
        .iter()
        .position(|p| matches!(strip(p).value(), hir::Pat::Var(v) if v.value() == x.value()))
}

/// What a value of type `ty` could hold that a compact region may not, if
/// anything: looking through tuples, records and the fields of data types,
/// with `ty`'s arguments in place. `seen` is the data types being looked
/// through, so a recursive one is looked at once.
///
/// A variable could be anything, and is taken to be fine: the check is
/// wherever it is known. A `Compact` inside one is kept whole, having been
/// checked when it was made.
fn uncompactable(ty: &Type, variants: &VariantEnv, seen: &mut Vec<Type>) -> Option<&'static str> {
    match ty {
        Type::Fun(..) => Some("a function"),
        Type::Tuple(ts) => ts.iter().find_map(|t| uncompactable(t, variants, seen)),
        Type::Record(row) => {
            let mut row = &**row;
            while let Type::RowExtend(_, field, rest) = row {
                if let Some(what) = uncompactable(field, variants, seen) {
                    return Some(what);
                }
                row = rest;
            }
            None
        }
        Type::Con(name, args) => {
            match &**name {
                "Ref" | "StRef" => return Some("a Ref"),
                "StArray" => return Some("a mutable array"),
                "Task" => return Some("a thread"),
                "Channel" => return Some("a channel"),
                "TVar" => return Some("a TVar"),
                "Compact" => return None,
                _ => {}
            }
            if seen.contains(ty) {
                return None;
            }
            match variants.get(name) {
                // What its constructors hold, with this type's arguments.
                Some(vs) => {
                    seen.push(ty.clone());
                    let found = vs
                        .iter()
                        .flat_map(|v| &v.fields)
                        .find_map(|f| uncompactable(&subst_bound(f, args), variants, seen));
                    seen.pop();
                    found
                }
                // A built-in container -- an `Array`, a `List` -- holds its
                // arguments.
                None => args.iter().find_map(|a| uncompactable(a, variants, seen)),
            }
        }
        Type::Var(_) | Type::Bound(_) | Type::RowEmpty | Type::RowExtend(..) | Type::Error => None,
    }
}

// ===========================================================================
// The checker
// ===========================================================================

struct Checker<'a> {
    filename: String,
    types: &'a TypeTable,
    variants: &'a VariantEnv,
    check_matches: bool,
    compacting: &'a HashMap<hir::VarId, usize>,
    synonyms: &'a Synonyms,
    /// The synonyms being seen through, innermost last: one that is used in
    /// its own pattern is not seen through again.
    seeing: RefCell<Vec<hir::VarId>>,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    // --- walking the tree ---------------------------------------------------

    fn decl(&mut self, decl: &hir::LDecl) {
        match decl.value() {
            hir::Decl::Bind(b) => self.bind(b),
            // A method is a function like any other, and so is a default.
            hir::Decl::Impl(imp) => imp.methods.iter().for_each(|(_, b)| self.bind(b)),
            hir::Decl::Trait(tr) => {
                for m in &tr.methods {
                    if let Some(body) = m.default.as_ref().and_then(|d| d.body.as_ref()) {
                        self.bind(body);
                    }
                }
            }
            _ => {}
        }
    }

    fn bind(&mut self, bind: &hir::Bind) {
        match bind {
            hir::Bind::Fun(_, params, _, body) => {
                for p in params {
                    self.require_irrefutable(p, "function parameter");
                    self.pat(p);
                }
                self.expr(body);
            }
            // `def (a, b) = e` / `let p = e` destructure without a fallback arm,
            // so they must be irrefutable too.
            hir::Bind::Pat(pat, expr) => {
                self.require_irrefutable(pat, "binding");
                self.pat(pat);
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
                    self.pat(p);
                }
                self.expr(body);
            }
            hir::Expr::Match(scrut, arms) => {
                self.expr(scrut);
                for (p, guard, body) in arms {
                    self.pat(p);
                    if let Some(g) = guard {
                        self.expr(g);
                    }
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
                    self.pat(&arm.param);
                    self.expr(&arm.body);
                }
                if let Some((p, b)) = ret {
                    self.require_irrefutable(p, "handler `return` parameter");
                    self.pat(p);
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
            hir::Expr::Update(base, fields) => {
                self.expr(base);
                fields.iter().for_each(|(_, e)| self.expr(e));
            }
            hir::Expr::Var(v) => {
                if let Some(&at) = self.compacting.get(v.value()) {
                    self.compacted(expr, at);
                }
            }
            hir::Expr::Lit(_) | hir::Expr::Unit | hir::Expr::Error => {}
        }
    }

    /// The expressions of the views in `pat`, which may hold matches of their
    /// own.
    fn pat(&mut self, pat: &hir::LPat) {
        match pat.value() {
            hir::Pat::View(f, p) => {
                self.expr(f);
                self.pat(p);
            }
            hir::Pat::Ann(p, _) => self.pat(p),
            hir::Pat::As(_, p) => self.pat(p),
            hir::Pat::Tuple(ps)
            | hir::Pat::Array(ps)
            | hir::Pat::List(ps)
            | hir::Pat::Cons(_, ps) => ps.iter().for_each(|p| self.pat(p)),
            hir::Pat::Record(fields, _) => fields.iter().for_each(|(_, p)| self.pat(p)),
            hir::Pat::Wildcard
            | hir::Pat::Var(_)
            | hir::Pat::Lit(_)
            | hir::Pat::Unit
            | hir::Pat::Error => {}
        }
    }

    /// Whether `pat` says anything definite about what it covers: whether it
    /// holds no view that may fail to match. A view's answer is not a question
    /// about the value's shape, so -- like a guarded arm -- a pattern with one
    /// covers nothing; one whose own pattern matches everything is `_`.
    fn definite(&self, pat: &hir::LPat) -> bool {
        match pat.value() {
            hir::Pat::View(f, p) if self.seen_through(f, p).is_some() => {
                let (_, _, args) = self.seen_through(f, p).expect("just seen");
                args.iter().all(|a| self.definite(a))
            }
            // One of a set that covers its type: as definite as a
            // constructor.
            hir::Pat::View(f, p) if self.of_set(f, p).is_some() => {
                let (_, _, args) = self.of_set(f, p).expect("just seen");
                args.iter().all(|a| self.definite(a))
            }
            hir::Pat::View(_, p) => {
                self.definite(p)
                    && match self.types.get(p.id) {
                        Some(t) => self.missing(&[vec![self.lower(p)]], &[t.clone()]).is_none(),
                        None => true,
                    }
            }
            hir::Pat::Ann(p, _) => self.definite(p),
            hir::Pat::As(_, p) => self.definite(p),
            hir::Pat::Tuple(ps)
            | hir::Pat::Array(ps)
            | hir::Pat::List(ps)
            | hir::Pat::Cons(_, ps) => ps.iter().all(|p| self.definite(p)),
            hir::Pat::Record(fields, _) => fields.iter().all(|(_, p)| self.definite(p)),
            hir::Pat::Wildcard
            | hir::Pat::Var(_)
            | hir::Pat::Lit(_)
            | hir::Pat::Unit
            | hir::Pat::Error => true,
        }
    }

    /// The view `f -> p` as the use of a pattern synonym that can be seen
    /// through, `P a b`: its matcher, the synonym, and what it was given.
    fn seen_through<'p>(
        &self,
        f: &hir::LExpr,
        p: &'p hir::LPat,
    ) -> Option<(hir::VarId, &Synonym, Vec<&'p hir::LPat>)> {
        let hir::Expr::Var(v) = f.value() else {
            return None;
        };
        let v = *v.value();
        let syn = self.synonyms.get(&v)?;
        if self.seeing.borrow().contains(&v) {
            return None;
        }
        // Its pattern may hold views only of synonyms that can be seen
        // through in turn.
        self.seeing.borrow_mut().push(v);
        let definite = self.definite(&syn.pat);
        self.seeing.borrow_mut().pop();
        if !definite {
            return None;
        }
        let args = answered(p, syn.params.len())?;
        Some((v, syn, args))
    }

    /// The view `f -> p` as the use of a pattern synonym in a set that covers
    /// its type, `pattern A | B`: its matcher, its name, and what it was
    /// given.
    fn of_set<'p>(
        &self,
        f: &hir::LExpr,
        p: &'p hir::LPat,
    ) -> Option<(hir::VarId, InternedString, Vec<&'p hir::LPat>)> {
        let hir::Expr::Var(v) = f.value() else {
            return None;
        };
        let v = *v.value();
        let set = self.synonyms.set_of(v)?;
        let (_, name, arity) = set.iter().find(|(m, ..)| *m == v)?;
        Some((v, *name, answered(p, *arity)?))
    }

    /// A mention of `compact` or `compactAdd`, whose parameter `at` is the value
    /// copied into a region: refused here when its type says it can hold what
    /// a region may not -- a function, or anything that changes.
    ///
    /// Every runtime refuses these as it copies, but not all of them can see
    /// all of it: on Silo a function that captures nothing is a word, as a
    /// constructor with no fields is, and nothing tells the two apart. The
    /// type does, wherever it is known -- a call, a pipe, or the primitive
    /// passed as a value. Where it is a variable (`Std.Compact.make` itself)
    /// nothing is known, and the check is at each use of that instead.
    fn compacted(&mut self, var: &hir::LExpr, at: usize) {
        let Some(Type::Fun(params, ret, _)) = self.types.get(var.id) else {
            return;
        };
        // Curried: the parameter `at` is `at` arrows in.
        let (mut params, mut ret) = (params, ret);
        let mut at = at;
        while at >= params.len() {
            at -= params.len();
            match &**ret {
                Type::Fun(p, r, _) => (params, ret) = (p, r),
                _ => return,
            }
        }
        let ty = params[at].clone();
        if let Some(what) = uncompactable(&ty, self.variants, &mut Vec::new()) {
            self.error(
                format!("a compact cannot hold {what}: a compact region holds only immutable data"),
                format!("this would hold {what}"),
                var.span,
            );
        }
    }

    // --- the two checks -----------------------------------------------------

    fn check_match(
        &mut self,
        span: Span,
        scrut: &hir::LExpr,
        arms: &[(hir::LPat, Option<hir::LExpr>, hir::LExpr)],
    ) {
        let Some(ty) = self.types.get(scrut.id).cloned() else {
            return; // untyped (an earlier error) — nothing reliable to say
        };
        // A guarded arm covers nothing: whether it is taken is not a question
        // about the pattern, so the arms after it have to cover its cases too.
        // Nor does one with a view that may not match (see `definite`).
        let rows: Vec<Vec<P>> = arms
            .iter()
            .filter(|(p, guard, _)| guard.is_none() && self.definite(p))
            .map(|(p, _, _)| vec![self.lower(p)])
            .collect();
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
        if matches!(
            pat.value(),
            hir::Pat::Var(_) | hir::Pat::Wildcard | hir::Pat::Error
        ) {
            return;
        }
        let Some(ty) = self.types.get(pat.id).cloned() else {
            return;
        };
        if !self.definite(pat) {
            self.error(
                format!("refutable pattern in {what}: a view in it may not match"),
                format!("a {what} must match every value"),
                pat.span,
            );
            return;
        }
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
        self.lower_in(pat, &HashMap::new())
    }

    /// [`Checker::lower`], with what each of `subst`'s names -- a pattern
    /// synonym's parameters -- stands for.
    fn lower_in(&self, pat: &hir::LPat, subst: &HashMap<hir::VarId, P>) -> P {
        // A pattern inference gave the error type (an unknown constructor) is
        // taken to match anything: whatever it was meant to cover, saying the
        // `match` misses it would only repeat the error.
        if matches!(self.types.get(pat.id), Some(Type::Error)) {
            return P::Wild;
        }
        match pat.value() {
            hir::Pat::Var(v) => subst.get(v.value()).cloned().unwrap_or(P::Wild),
            hir::Pat::Wildcard | hir::Pat::Error => P::Wild,
            // A pattern synonym's use is its pattern, with what it was given
            // for its parameters. Any other view is only ever one that
            // matches everything: see `definite`.
            hir::Pat::View(f, p) => match self.seen_through(f, p) {
                Some((v, syn, args)) => {
                    let given: HashMap<hir::VarId, P> = syn
                        .params
                        .iter()
                        .zip(args)
                        .map(|(x, a)| (*x, self.lower_in(a, subst)))
                        .collect();
                    self.seeing.borrow_mut().push(v);
                    let lowered = self.lower_in(&syn.pat, &given);
                    self.seeing.borrow_mut().pop();
                    lowered
                }
                None => match self.of_set(f, p) {
                    Some((v, name, args)) => P::Con(
                        Con::Syn(v, name),
                        args.iter().map(|a| self.lower_in(a, subst)).collect(),
                    ),
                    None => P::Wild,
                },
            },
            // An annotation constrains the type, never the shape.
            hir::Pat::Ann(inner, _) => self.lower_in(inner, subst),
            hir::Pat::As(_, sub) => self.lower_in(sub, subst),
            hir::Pat::Unit => P::Con(Con::Unit, vec![]),
            hir::Pat::Lit(hir::Lit::Int(i)) => P::Con(Con::Int(*i), vec![]),
            hir::Pat::Lit(hir::Lit::String(s)) => P::Con(Con::Str(*s), vec![]),
            hir::Pat::Lit(hir::Lit::Char(c)) => P::Con(Con::Char(*c), vec![]),
            hir::Pat::Lit(hir::Lit::Float(b)) => P::Con(Con::Float(*b), vec![]),
            hir::Pat::Tuple(ps) => P::Con(
                Con::Tuple(ps.len()),
                ps.iter().map(|p| self.lower_in(p, subst)).collect(),
            ),
            hir::Pat::Array(ps) => P::Con(
                Con::Array(ps.len()),
                ps.iter().map(|p| self.lower_in(p, subst)).collect(),
            ),
            // `[a; b]` is sugar for `Cons a (Cons b Nil)`.
            hir::Pat::List(ps) => ps.iter().rev().fold(
                P::Con(Con::Variant(InternedString::from("List.Nil")), vec![]),
                |tail, p| {
                    P::Con(
                        Con::Variant(InternedString::from("List.Cons")),
                        vec![self.lower_in(p, subst), tail],
                    )
                },
            ),
            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                P::Con(
                    Con::Variant(name),
                    args.iter().map(|p| self.lower_in(p, subst)).collect(),
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
                        Some((_, p)) => self.lower_in(p, subst),
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
                    let arity = vs
                        .iter()
                        .flat_map(|v| &v.fields)
                        .filter_map(max_bound)
                        .max();
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
                    "Bool" => Some(vec![
                        (con("Bool.False"), vec![]),
                        (con("Bool.True"), vec![]),
                    ]),
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
        // A column of the error type (an undefined scrutinee, say) has no
        // constructor set to be exhaustive over. Every row covers it, which
        // drops the column and checks the rest.
        if *ty == Type::Error {
            let rest: Vec<Vec<P>> = rows.iter().map(|row| row[1..].to_vec()).collect();
            let w = self.missing(&rest, &col_types[1..])?;
            let mut out = vec![P::Wild];
            out.extend(w);
            return Some(out);
        }
        let used: Vec<Con> = heads(rows);
        // A column headed by pattern synonyms of a set that covers the type:
        // the set is its constructors, each of whose arguments is covered only
        // by what matches anything. What it leaves uncovered is the witness --
        // unless the type's own constructors, to which the synonyms' rows
        // cover nothing, cover it after all.
        if let Some(set) = used.iter().find_map(|c| match c {
            Con::Syn(v, _) => self.synonyms.set_of(*v),
            _ => None,
        }) {
            let opaque = Type::Con(InternedString::from("$synonym"), Vec::new());
            let mut witness = None;
            for (v, n, arity) in set {
                let con = Con::Syn(*v, *n);
                if !used.contains(&con) {
                    let rest = self.missing(&default_matrix(rows), &col_types[1..]);
                    if let Some(rest) = rest {
                        let mut out = vec![P::Con(con, vec![P::Wild; *arity])];
                        out.extend(rest);
                        witness = Some(out);
                        break;
                    }
                    continue;
                }
                let spec = specialize(rows, &con, *arity);
                let mut tys = vec![opaque.clone(); *arity];
                tys.extend_from_slice(&col_types[1..]);
                if let Some(w) = self.missing(&spec, &tys) {
                    witness = Some(rebuild(con, *arity, w));
                    break;
                }
            }
            let witness = witness?;
            return self.missing_by_type(rows, col_types).map(|_| witness);
        }
        self.missing_by_type(rows, col_types)
    }

    /// [`Checker::missing`], splitting the first column by its type's own
    /// constructors.
    fn missing_by_type(&self, rows: &[Vec<P>], col_types: &[Type]) -> Option<Vec<P>> {
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
            Con::Syn(_, name) if args.is_empty() => name.to_string(),
            Con::Syn(_, name) => {
                let parts: Vec<String> = args.iter().map(|a| render_at(a, true)).collect();
                let s = format!("{name} {}", parts.join(" "));
                if nested { format!("({s})") } else { s }
            }
            Con::Variant(name) if args.is_empty() => bare_ctor(name),
            Con::Variant(name) => {
                let parts: Vec<String> = args.iter().map(|a| render_at(a, true)).collect();
                let s = format!("{} {}", bare_ctor(name), parts.join(" "));
                if nested { format!("({s})") } else { s }
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
        Type::Var(_) | Type::RowEmpty | Type::Error => None,
        Type::Con(_, args) | Type::Tuple(args) => args.iter().filter_map(max_bound).max(),
        Type::Fun(ps, r, e) => ps.iter().chain([&**r, &**e]).filter_map(max_bound).max(),
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
