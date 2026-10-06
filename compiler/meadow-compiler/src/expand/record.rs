//! What macros expanded to, written down: for a tool that reads the compiler's
//! expansions rather than making its own -- MeadowBoot, which takes a
//! procedural macro's expansion from here as it takes core from the Rust
//! compiler, and checks the `macro` rules it expands itself against these.
//!
//! Off unless [`start`] turns it on. Then every call the expander replaces is
//! kept, in the module it is in: where the call was, in which position, and
//! what it came to, expanded all the way down -- and each `@derive`'s
//! declarations, under the declaration it is written on. [`take`] answers them
//! and turns recording off.
//!
//! Each is written as JSON, a node an array: its production, where it came
//! from, and its fields -- `["Var", 10, 13, ["Name", 10, 13, "map"]]` -- named
//! and ordered as MeadowBoot's `Ast` language has them. A chain of operators
//! is written as it was parsed, `["Chain", …, first, [ops], [operands]]`,
//! since how it groups is the resolver's to say.

use meadow_ast as ast;
use meadow_span::Span;
use std::sync::Mutex;

/// One expansion: the module it is in, where the call was, what it stood for
/// -- `decls`, `expr`, `pat`, or `derived` for what a declaration's
/// `@derive`s wrote -- and what it came to, as JSON.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub filename: String,
    pub at: Span,
    pub kind: &'static str,
    pub json: String,
}

static RECORDING: Mutex<Option<Vec<Recorded>>> = Mutex::new(None);

/// Start keeping every expansion, from every thread.
pub fn start() {
    *RECORDING.lock().unwrap_or_else(|p| p.into_inner()) = Some(Vec::new());
}

/// Every expansion kept since [`start`], and stop keeping them.
pub fn take() -> Vec<Recorded> {
    RECORDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or_default()
}

fn keep(filename: &str, at: Span, kind: &'static str, json: impl FnOnce() -> String) {
    let mut recording = RECORDING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(kept) = recording.as_mut() {
        kept.push(Recorded {
            filename: filename.to_string(),
            at,
            kind,
            json: json(),
        });
    }
}

pub(crate) fn decls(filename: &str, at: Span, kind: &'static str, ds: &[ast::LDecl]) {
    keep(filename, at, kind, || list(ds.iter().map(decl)));
}

pub(crate) fn expr(filename: &str, at: Span, e: &ast::LExpr) {
    keep(filename, at, "expr", || self::expr_of(e));
}

pub(crate) fn pat(filename: &str, at: Span, p: &ast::LPat) {
    keep(filename, at, "pat", || pat_of(p));
}

// --- what each call produced, as source ----------------------------------------

/// One macro call and the tokens that stood in its place: what `meadow build
/// --emit expanded` writes out, as `cargo expand` does for Rust.
///
/// One level: a call the tokens themselves contain is another of these, and
/// since everything a macro produces carries its call's span, such a call's
/// `call` lies inside the `arg` of the one that produced it.
#[derive(Debug, Clone)]
pub struct Produced {
    pub filename: String,
    /// The macro, as the call names it.
    pub name: String,
    /// The whole call, name and argument.
    pub call: Span,
    /// The call's argument.
    pub arg: Span,
    /// How many tokens the call was given, and how many it produced.
    pub given: usize,
    pub tokens: usize,
    /// What it produced, as source: a declaration to a line where it produced
    /// declarations.
    pub text: String,
}

static PRODUCED: Mutex<Option<Vec<Produced>>> = Mutex::new(None);

/// Start keeping what every macro call produces, from every thread.
pub fn start_produced() {
    *PRODUCED.lock().unwrap_or_else(|p| p.into_inner()) = Some(Vec::new());
}

/// Whether [`start_produced`] is in force: a build that wants the expansions
/// has to compile the package again rather than read it back.
pub fn producing() -> bool {
    PRODUCED.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

/// Everything kept since [`start_produced`], and stop keeping it.
pub fn take_produced() -> Vec<Produced> {
    PRODUCED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or_default()
}

pub(crate) fn produced(
    filename: &str,
    name: &str,
    call: Span,
    arg: Span,
    given: usize,
    tokens: &[meadow_lexer::LToken],
) {
    let mut producing = PRODUCED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(kept) = producing.as_mut() {
        kept.push(Produced {
            filename: filename.to_string(),
            name: name.to_string(),
            call,
            arg,
            given,
            tokens: tokens.len(),
            text: source(tokens),
        });
    }
}

/// `tokens` as source text, a new line before each declaration that is not
/// inside brackets: what a macro wrote is otherwise one line however long.
fn source(tokens: &[meadow_lexer::LToken]) -> String {
    use meadow_lexer::tt::{TokenTree, render};
    const DECLS: &[&str] = &[
        "fun", "def", "data", "record", "effect", "trait", "impl", "use", "mod", "type", "macro",
    ];
    let mut out = String::new();
    let mut line: Vec<TokenTree> = Vec::new();
    let mut depth = 0usize;
    // An attribute and the declaration it is on are one line.
    let mut attributed = false;
    for t in tokens {
        let text = t.value().text();
        let starts = depth == 0
            && !line.is_empty()
            && (text.starts_with('@') || (DECLS.contains(&text.as_str()) && !attributed));
        if starts {
            out.push_str(&render(&line));
            out.push('\n');
            line.clear();
        }
        if depth == 0 {
            attributed = text.starts_with('@') || (attributed && !DECLS.contains(&text.as_str()));
        }
        match text.as_str() {
            "(" | "[" | "{" | "#[" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            _ => {}
        }
        line.push(TokenTree::Token(t.clone()));
    }
    out.push_str(&render(&line));
    out
}

// --- JSON ------------------------------------------------------------------------

fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(items: impl Iterator<Item = String>) -> String {
    format!("[{}]", items.collect::<Vec<_>>().join(","))
}

fn opt(x: Option<String>) -> String {
    x.unwrap_or_else(|| "null".to_string())
}

/// A node: its production, where it came from, and its fields.
fn node(tag: &str, span: Span, fields: &[String]) -> String {
    let mut out = format!("[{},{},{}", text(tag), span.start, span.end);
    for f in fields {
        out.push(',');
        out.push_str(f);
    }
    out.push(']');
    out
}

fn join(a: Span, b: Span) -> Span {
    Span::new(a.start.min(b.start), a.end.max(b.end))
}

fn name(n: &ast::Ident) -> String {
    node("Name", n.span, &[text(n.value())])
}

fn named(t: &str, at: Span) -> String {
    node("Name", at, &[text(t)])
}

fn names(ns: &[ast::Ident]) -> String {
    list(ns.iter().map(name))
}

// --- declarations -------------------------------------------------------------------

fn decl(d: &ast::LDecl) -> String {
    let s = d.span;
    match &*d.value {
        ast::Decl::Bind(b) => node("Bind", s, &[bind(b)]),
        ast::Decl::Mod(n) => node("ModDecl", s, &[name(n)]),
        ast::Decl::Module(n, decls) => {
            node("ModuleDecl", s, &[name(n), list(decls.iter().map(decl))])
        }
        ast::Decl::Use(u) => node(
            "UseDecl",
            s,
            &[
                names(&u.path),
                names(&u.names),
                names(&u.macros),
                u.glob.to_string(),
                opt(u.alias.as_ref().map(name)),
            ],
        ),
        ast::Decl::Data(dd) => node(
            "DataDecl",
            s,
            &[
                name(&dd.name),
                names(&dd.params),
                list(dd.variants.iter().map(variant)),
            ],
        ),
        ast::Decl::Record(rd) => node(
            "RecordDecl",
            s,
            &[
                name(&rd.name),
                names(&rd.params),
                list(rd.fields.iter().map(field)),
            ],
        ),
        ast::Decl::Effect(ed) => node(
            "EffectDecl",
            s,
            &[
                name(&ed.name),
                names(&ed.params),
                list(ed.ops.iter().map(field)),
            ],
        ),
        ast::Decl::TypeAlias(ad) => node(
            "AliasDecl",
            s,
            &[name(&ad.name), names(&ad.params), ty(&ad.ty)],
        ),
        ast::Decl::EffectAlias(ad) => node(
            "EffectAliasDecl",
            s,
            &[name(&ad.name), names(&ad.params), row(&ad.row, s)],
        ),
        ast::Decl::Sig(n, t, bs) => {
            node("SigDecl", s, &[name(n), ty(t), list(bs.iter().map(bound))])
        }
        ast::Decl::Trait(td) => node(
            "TraitDecl",
            s,
            &[
                name(&td.name),
                names(&td.params),
                list(td.supers.iter().map(bound)),
                list(
                    td.assocs
                        .iter()
                        .map(|(n, ps)| node("Assoc", n.span, &[name(n), names(ps)])),
                ),
                list(
                    td.sigs
                        .iter()
                        .map(|(n, t)| node("MethodSig", join(n.span, t.span), &[name(n), ty(t)])),
                ),
                list(td.defaults.iter().map(bind)),
            ],
        ),
        ast::Decl::Impl(id) => node(
            "ImplDecl",
            s,
            &[
                name(&id.tr),
                list(id.tys.iter().map(ty)),
                list(id.context.iter().map(bound)),
                list(id.assocs.iter().map(|(n, args, t)| {
                    node(
                        "AssocDef",
                        join(n.span, t.span),
                        &[name(n), list(args.iter().map(ty)), ty(t)],
                    )
                })),
                list(id.methods.iter().map(bind)),
            ],
        ),
        ast::Decl::Fixity(_, _, ops) => node("FixityDecl", s, &[names(ops)]),
        ast::Decl::Attributed(attrs, inner) => node(
            "Attributed",
            s,
            &[list(attrs.iter().map(attr)), decl(inner)],
        ),
        ast::Decl::MacCall(m) => node("MacCallDecl", s, &[mac(m, s)]),
        ast::Decl::Macro(md) => node("MacroDecl", s, &[name(&md.name)]),
    }
}

fn attr(a: &ast::Attr) -> String {
    node(
        "Attr",
        a.name.span,
        &[name(&a.name), names(&a.args), list(a.meta.iter().map(meta))],
    )
}

fn meta(m: &ast::Meta) -> String {
    match m {
        ast::Meta::Word(n) => node("MetaWord", n.span, &[name(n)]),
        ast::Meta::Text(t) => node("MetaText", t.span, &[name(t)]),
        ast::Meta::Value(k, v) => node("MetaValue", join(k.span, v.span), &[name(k), name(v)]),
        ast::Meta::List(n, ms) => node("MetaList", n.span, &[name(n), list(ms.iter().map(meta))]),
    }
}

fn mac(m: &ast::MacCall, at: Span) -> String {
    node("MacCall", at, &[names(&m.path)])
}

fn variant(v: &ast::Variant) -> String {
    let attrs = list(v.attrs.iter().map(attr));
    match &v.fields {
        ast::VariantFields::Positional(ts) => node(
            "Variant",
            v.name.span,
            &[
                attrs,
                name(&v.name),
                "false".to_string(),
                list(ts.iter().map(ty)),
                "[]".to_string(),
            ],
        ),
        ast::VariantFields::Named(fs) => node(
            "Variant",
            v.name.span,
            &[
                attrs,
                name(&v.name),
                "true".to_string(),
                "[]".to_string(),
                list(fs.iter().map(field)),
            ],
        ),
    }
}

fn field(f: &ast::Field) -> String {
    node(
        "Field",
        join(f.name.span, f.ty.span),
        &[list(f.attrs.iter().map(attr)), name(&f.name), ty(&f.ty)],
    )
}

fn bound(b: &ast::Bound) -> String {
    node(
        "Bound",
        b.tr.span,
        &[name(&b.tr), list(b.tys.iter().map(ty))],
    )
}

// --- types --------------------------------------------------------------------------

fn ty(t: &ast::LType) -> String {
    let s = t.span;
    match &*t.value {
        ast::TypeExpr::Var(n) => node("TVar", s, &[name(n)]),
        ast::TypeExpr::Con(n, args) => node("TCon", s, &[name(n), list(args.iter().map(ty))]),
        ast::TypeExpr::Fun(ps, r, eff) => node(
            "TFun",
            s,
            &[
                list(ps.iter().map(ty)),
                ty(r),
                opt(eff.as_ref().map(|e| row(e, s))),
            ],
        ),
        ast::TypeExpr::Row(r) => node("TRow", s, &[row(r, s)]),
        ast::TypeExpr::Tuple(ts) => node("TTuple", s, &[list(ts.iter().map(ty))]),
        ast::TypeExpr::Vector(x) => node("TVector", s, &[ty(x)]),
        ast::TypeExpr::List(x) => node("TList", s, &[ty(x)]),
        ast::TypeExpr::Record(fs, tail) => node(
            "TRecord",
            s,
            &[
                list(
                    fs.iter()
                        .map(|(n, t)| node("TField", join(n.span, t.span), &[name(n), ty(t)])),
                ),
                opt(tail.as_ref().map(name)),
            ],
        ),
    }
}

fn row(r: &ast::EffectRow, at: Span) -> String {
    node(
        "Row",
        at,
        &[
            list(
                r.labels
                    .iter()
                    .map(|(n, args)| node("Label", n.span, &[name(n), list(args.iter().map(ty))])),
            ),
            opt(r.tail.as_ref().map(name)),
        ],
    )
}

// --- bindings and patterns ------------------------------------------------------------

fn bind(b: &ast::Bind) -> String {
    match b {
        ast::Bind::Pat(p, e) => node("PatBind", join(p.span, e.span), &[pat_of(p), expr_of(e)]),
        ast::Bind::Fun(n, ps, ret, body) => node(
            "FunBind",
            join(n.span, body.span),
            &[
                name(n),
                list(ps.iter().map(pat_of)),
                opt(ret.as_ref().map(ty)),
                expr_of(body),
            ],
        ),
    }
}

fn lit(l: &ast::Lit, at: Span) -> String {
    match l {
        ast::Lit::Int(n) => node("LitInt", at, &[text(&n.to_string())]),
        ast::Lit::Float(bits) => node("LitFloat", at, &[text(&f64::from_bits(*bits).to_string())]),
        ast::Lit::String(s) => node("LitString", at, &[text(s)]),
        ast::Lit::Char(c) => node("LitChar", at, &[text(&(*c as u32).to_string())]),
        #[allow(unreachable_patterns)]
        _ => node("LitString", at, &[text("")]),
    }
}

fn pat_of(p: &ast::LPat) -> String {
    let s = p.span;
    let pats = |ps: &[ast::LPat]| list(ps.iter().map(pat_of));
    match &*p.value {
        ast::Pat::Wildcard => node("PWild", s, &[]),
        ast::Pat::Var(n) => node("PVar", s, &[name(n)]),
        ast::Pat::Ann(inner, t) => node("PAnn", s, &[pat_of(inner), ty(t)]),
        ast::Pat::Lit(l) => node("PLit", s, &[lit(l, s)]),
        ast::Pat::As(n, inner) => node("PAs", s, &[name(n), pat_of(inner)]),
        ast::Pat::Cons(n, ps) => node("PCons", s, &[name(n), pats(ps)]),
        ast::Pat::QualCons(q, n, ps) => node("PQualCons", s, &[name(q), name(n), pats(ps)]),
        ast::Pat::Tuple(ps) => node("PTuple", s, &[pats(ps)]),
        ast::Pat::Array(ps) => node("PArray", s, &[pats(ps)]),
        ast::Pat::Vector(ps) => node("PVector", s, &[pats(ps)]),
        ast::Pat::List(ps) => node("PList", s, &[pats(ps)]),
        ast::Pat::Record(fs, open) => node(
            "PRecord",
            s,
            &[
                list(
                    fs.iter()
                        .map(|(n, q)| node("PField", join(n.span, q.span), &[name(n), pat_of(q)])),
                ),
                open.to_string(),
            ],
        ),
        ast::Pat::Unit => node("PUnit", s, &[]),
        ast::Pat::View(f, q) => node("PView", s, &[expr_of(f), pat_of(q)]),
        ast::Pat::MacCall(m) => node("PMacCall", s, &[mac(m, s)]),
    }
}

// --- expressions ----------------------------------------------------------------------

fn expr_of(e: &ast::LExpr) -> String {
    let s = e.span;
    let exprs = |es: &[ast::LExpr]| list(es.iter().map(expr_of));
    let fields = |fs: &[(ast::Ident, ast::LExpr)]| {
        list(
            fs.iter()
                .map(|(n, v)| node("FieldE", join(n.span, v.span), &[name(n), expr_of(v)])),
        )
    };
    match &*e.value {
        ast::Expr::Var(n) => node("Var", s, &[name(n)]),
        ast::Expr::Lit(l) => node("LitE", s, &[lit(l, s)]),
        ast::Expr::Interp(texts, holes) => node(
            "Interp",
            s,
            &[
                list(texts.iter().map(|t| text(t))),
                list(holes.iter().map(|(h, fmt)| {
                    node(
                        "Hole",
                        h.span,
                        &[expr_of(h), matches!(fmt, ast::Fmt::Debug).to_string()],
                    )
                })),
            ],
        ),
        ast::Expr::Lam(ps, body) => node("Lam", s, &[list(ps.iter().map(pat_of)), expr_of(body)]),
        ast::Expr::App(f, args) => node("App", s, &[expr_of(f), exprs(args)]),
        ast::Expr::Let(bs, body) => node("Let", s, &[list(bs.iter().map(bind)), expr_of(body)]),
        ast::Expr::If(c, y, n) => node("If", s, &[expr_of(c), expr_of(y), expr_of(n)]),
        ast::Expr::Match(scrut, arms) => node(
            "Match",
            s,
            &[
                expr_of(scrut),
                list(arms.iter().map(|(p, g, b)| {
                    node(
                        "Arm",
                        join(p.span, b.span),
                        &[pat_of(p), opt(g.as_ref().map(expr_of)), expr_of(b)],
                    )
                })),
            ],
        ),
        ast::Expr::UnOp(op, x) => node("Neg", s, &[named("neg", op.span), expr_of(x)]),
        ast::Expr::BinOp(op, l, r) => {
            let (tag, word) = match op.value() {
                ast::BinOp::And => ("AndE", "and"),
                ast::BinOp::Or => ("OrE", "or"),
            };
            node(tag, s, &[named(word, op.span), expr_of(l), expr_of(r)])
        }
        ast::Expr::Infix(first, rest) => node(
            "Chain",
            s,
            &[
                expr_of(first),
                list(rest.iter().map(|(op, _)| name(op))),
                list(rest.iter().map(|(_, x)| expr_of(x))),
            ],
        ),
        ast::Expr::Tuple(es) => node("Tuple", s, &[exprs(es)]),
        ast::Expr::Array(es) => node("ArrayE", s, &[exprs(es)]),
        ast::Expr::List(es) => node("ListE", s, &[exprs(es)]),
        ast::Expr::Cons(n, args) => node("Cons", s, &[name(n), exprs(args)]),
        ast::Expr::Qual(q, n) => node("Qual", s, &[name(q), name(n)]),
        ast::Expr::Record(fs, base) => {
            node("RecordE", s, &[fields(fs), opt(base.as_ref().map(expr_of))])
        }
        ast::Expr::Update(base, fs) => node("Update", s, &[expr_of(base), fields(fs)]),
        ast::Expr::Field(x, l) => node("Proj", s, &[expr_of(x), name(l)]),
        ast::Expr::Handle(body, arms, ret) => node(
            "Handle",
            s,
            &[
                expr_of(body),
                list(arms.iter().map(|a| {
                    node(
                        "HArm",
                        join(a.op.span, a.body.span),
                        &[
                            name(&a.op),
                            pat_of(&a.param),
                            name(&a.resume),
                            expr_of(&a.body),
                        ],
                    )
                })),
                opt(ret
                    .as_ref()
                    .map(|(p, b)| node("Ret", join(p.span, b.span), &[pat_of(p), expr_of(b)]))),
            ],
        ),
        ast::Expr::Unit => node("UnitE", s, &[]),
        ast::Expr::Hole => node("HoleE", s, &[]),
        ast::Expr::MacCall(m) => node("MacCallE", s, &[mac(m, s)]),
    }
}
