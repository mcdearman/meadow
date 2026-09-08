//! Compiling one open document, and indexing the result by source position.
//!
//! The compiler is built around whole packages, not editors: it hands back a
//! `CompiledPackage` with a `TypeTable` keyed by `NodeId` and an HIR tree whose
//! nodes carry spans. Everything an editor asks — *what is the type here*, *where
//! is this defined* — is a lookup by position, so this module walks the tree once
//! and builds the indices the requests need.
//!
//! `Std` is compiled once and reused. It takes seconds, and a keystroke cannot
//! wait for it.

use meadow_compiler::{
    diagnostics::Diagnostic,
    hir::{self, VarId},
    infer::TypeTable,
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    span::Span,
    AstModule, CompiledPackage, Options,
};

/// Everything the server knows about one document.
pub struct Analysis {
    pub source: String,
    pub diagnostics: Vec<Diagnostic>,
    /// Every typed node, innermost last — so a position lookup can take the
    /// *smallest* span that contains it by scanning for the tightest match.
    pub typed: Vec<(Span, String)>,
    /// Where each binding is introduced.
    pub defs: std::collections::HashMap<VarId, Span>,
    /// Every mention of a binding, for go-to-definition.
    pub refs: Vec<(Span, VarId)>,
    /// A binder whose type is worth showing after the name: parameters and
    /// `let`s, which carry no annotation in the source.
    pub binders: Vec<(Span, String)>,
    /// Names of top-level bindings, for hover headers.
    pub names: std::collections::HashMap<VarId, String>,
    /// Rendered scheme of each top-level binding.
    pub schemes: std::collections::HashMap<VarId, String>,
    /// Type names and constructor names, so highlighting can tell `Maybe` from
    /// `Just` — both are capitalised, and only resolution knows which is which.
    pub types_in_scope: std::collections::HashSet<String>,
    pub ctors_in_scope: std::collections::HashSet<String>,
}

/// The standard library, compiled once.
pub struct Std {
    packages: Vec<CompiledPackage>,
    types: std::collections::HashSet<String>,
    ctors: std::collections::HashSet<String>,
}

impl Std {
    /// Takes an already-compiled standard library. It is not loaded here on
    /// purpose: `meadow` owns the embedded sources and depends on this crate for
    /// its `lsp` subcommand, so reaching back for them would be a cycle.
    pub fn new(packages: Vec<CompiledPackage>) -> Std {
        let mut types = std::collections::HashSet::new();
        let mut ctors = std::collections::HashSet::new();
        for p in &packages {
            collect_names(&p.data_decls, &mut types, &mut ctors);
        }
        Std {
            packages,
            types,
            ctors,
        }
    }

    /// Compile `text` as a throwaway one-module package against `Std`.
    pub fn analyse(&self, text: &str) -> Analysis {
        let name = InternedString::from("main");
        let source = Source::new(SourceKind::Interactive, text.into());
        let lex = tokenize(source);
        let mut diagnostics = lex.errors;
        let (ast, perrs) = parser::parse(name, source, &lex.tokens);
        for e in &perrs {
            diagnostics.push(meadow_compiler::diagnostics::from_parse_error("main", e));
        }

        let modules = ast
            .map(|ast| {
                vec![AstModule {
                    path: vec![],
                    name,
                    ast,
                }]
            })
            .unwrap_or_default();

        let deps: Vec<&CompiledPackage> = self.packages.iter().collect();
        let (pkg, mut unit_diags) =
            meadow_compiler::compile_unit(name, 1, modules, &deps, Options::debug());
        diagnostics.append(&mut unit_diags);

        let mut a = Analysis {
            source: text.to_string(),
            diagnostics,
            typed: Vec::new(),
            defs: Default::default(),
            refs: Vec::new(),
            binders: Vec::new(),
            names: Default::default(),
            schemes: Default::default(),
            types_in_scope: self.types.clone(),
            ctors_in_scope: self.ctors.clone(),
        };
        collect_names(
            &pkg.data_decls,
            &mut a.types_in_scope,
            &mut a.ctors_in_scope,
        );
        for e in &pkg.exports {
            a.names.insert(e.var, e.name.to_string());
            a.schemes.insert(e.var, e.scheme.to_string());
        }
        for m in &pkg.modules {
            let mut w = Walk {
                types: &pkg.types,
                a: &mut a,
            };
            w.module(&m.hir);
        }
        a
    }
}

struct Walk<'a> {
    types: &'a TypeTable,
    a: &'a mut Analysis,
}

impl Walk<'_> {
    fn record(&mut self, span: Span, id: hir::NodeId) {
        if let Some(ty) = self.types.get(id) {
            self.a.typed.push((span, ty.to_string()));
        }
    }

    /// A binder: remember where it was introduced, and its type if there is one
    /// worth showing.
    fn bind(&mut self, ident: &hir::Ident, id: hir::NodeId, hint: bool) {
        let var = *ident.value();
        self.a.defs.insert(var, ident.span);
        if let Some(ty) = self.types.get(id) {
            let rendered = ty.to_string();
            self.a.typed.push((ident.span, rendered.clone()));
            if hint {
                self.a.binders.push((ident.span, rendered));
            }
        }
    }

    fn module(&mut self, m: &hir::LModule) {
        for d in &m.value().decls {
            self.decl(d);
        }
    }

    fn decl(&mut self, d: &hir::LDecl) {
        match d.value() {
            hir::Decl::Bind(b) => self.bind_decl(b, false),
            hir::Decl::Data(_)
            | hir::Decl::Record(_)
            | hir::Decl::Effect(_)
            | hir::Decl::Mod(_)
            | hir::Decl::Use(_)
            | hir::Decl::Error => {}
        }
    }

    /// `hint` is false at the top level: a `def`'s own name already shows its
    /// scheme on hover, and an inlay there would only repeat the signature.
    fn bind_decl(&mut self, b: &hir::Bind, hint: bool) {
        match b {
            hir::Bind::Fun(name, params, body) => {
                self.bind(name, name.id, false);
                for p in params {
                    self.pat(p, true);
                }
                self.expr(body);
            }
            hir::Bind::Pat(pat, expr) => {
                self.pat(pat, hint);
                self.expr(expr);
            }
            hir::Bind::Error => {}
        }
    }

    fn pat(&mut self, p: &hir::LPat, hint: bool) {
        self.record(p.span, p.id);
        match p.value() {
            hir::Pat::Var(ident) => self.bind(ident, ident.id, hint),
            hir::Pat::As(ident, sub) => {
                self.bind(ident, ident.id, hint);
                self.pat(sub, false);
            }
            hir::Pat::Tuple(ps)
            | hir::Pat::List(ps)
            | hir::Pat::Array(ps)
            | hir::Pat::Cons(_, ps) => ps.iter().for_each(|q| self.pat(q, hint)),
            hir::Pat::Record(fs, _) => fs.iter().for_each(|(_, q)| self.pat(q, hint)),
            hir::Pat::Wildcard | hir::Pat::Lit(_) | hir::Pat::Unit | hir::Pat::Error => {}
        }
    }

    fn expr(&mut self, e: &hir::LExpr) {
        self.record(e.span, e.id);
        match e.value() {
            hir::Expr::Var(ident) => {
                self.a.refs.push((ident.span, *ident.value()));
                self.record(ident.span, ident.id);
            }
            hir::Expr::Lam(params, body) => {
                params.iter().for_each(|p| self.pat(p, true));
                self.expr(body);
            }
            hir::Expr::App(f, args) => {
                self.expr(f);
                args.iter().for_each(|a| self.expr(a));
            }
            hir::Expr::Let(binds, body) => {
                binds.iter().for_each(|b| self.bind_decl(b, true));
                self.expr(body);
            }
            hir::Expr::If(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            hir::Expr::Match(scrut, arms) => {
                self.expr(scrut);
                for (p, arm) in arms {
                    self.pat(p, true);
                    self.expr(arm);
                }
            }
            hir::Expr::Tuple(xs) | hir::Expr::List(xs) | hir::Expr::Array(xs) => {
                xs.iter().for_each(|x| self.expr(x))
            }
            hir::Expr::Cons(_, xs) => xs.iter().for_each(|x| self.expr(x)),
            hir::Expr::Record(fs, base) => {
                fs.iter().for_each(|(_, x)| self.expr(x));
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            hir::Expr::Field(x, _) => self.expr(x),
            hir::Expr::Handle(body, arms, ret) => {
                self.expr(body);
                for arm in arms {
                    self.pat(&arm.param, true);
                    self.bind(&arm.resume, arm.resume.id, true);
                    self.expr(&arm.body);
                }
                if let Some((p, b)) = ret {
                    self.pat(p, true);
                    self.expr(b);
                }
            }
            hir::Expr::Lit(_) | hir::Expr::Unit | hir::Expr::Error => {}
        }
    }
}

// --- queries -----------------------------------------------------------------

impl Analysis {
    /// The type of the smallest node covering `offset`.
    ///
    /// Spans nest, so the tightest one wins: at the `x` in `f x`, that is `x`'s
    /// own type rather than the whole application's.
    pub fn type_at(&self, offset: usize) -> Option<&str> {
        self.typed
            .iter()
            .filter(|(s, _)| covers(*s, offset))
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(_, t)| t.as_str())
    }

    /// The binding mentioned at `offset`, if any.
    ///
    /// The smallest covering reference wins. An operator application records the
    /// operator itself over the *whole* expression — `answer + 1` is a mention of
    /// `+` spanning all of it — so the first match is routinely the wrong one.
    /// Among equally small candidates, prefer one defined in this file, since
    /// that is the one go-to-definition can actually take you to.
    pub fn var_at(&self, offset: usize) -> Option<VarId> {
        self.refs
            .iter()
            .filter(|(s, _)| covers(*s, offset))
            .min_by_key(|(s, v)| (s.end - s.start, !self.defs.contains_key(v)))
            .map(|(_, v)| *v)
            .or_else(|| {
                // Standing on the definition itself counts as referring to it.
                self.defs
                    .iter()
                    .filter(|(_, s)| covers(**s, offset))
                    .min_by_key(|(_, s)| s.end - s.start)
                    .map(|(v, _)| *v)
            })
    }

    /// Where the binding under the cursor was introduced.
    pub fn definition_at(&self, offset: usize) -> Option<Span> {
        self.defs.get(&self.var_at(offset)?).copied()
    }

    /// Markdown for the hover: a signature, then any doc comment above it.
    pub fn hover_at(&self, offset: usize) -> Option<String> {
        let var = self.var_at(offset);
        let signature = match var.and_then(|v| self.schemes.get(&v)) {
            // A top-level binding shows its generalised scheme, which is more
            // informative than the type at this particular use site.
            Some(scheme) => {
                let name = var.and_then(|v| self.names.get(&v));
                match name {
                    Some(n) => format!("{n} : {scheme}"),
                    None => scheme.clone(),
                }
            }
            None => {
                let ty = self.type_at(offset)?;
                match var.and_then(|v| self.names.get(&v)) {
                    Some(n) => format!("{n} : {ty}"),
                    None => ty.to_string(),
                }
            }
        };

        let mut out = format!("```meadow\n{signature}\n```");
        if let Some(doc) = var.and_then(|v| self.defs.get(&v)).and_then(|s| self.doc_above(*s)) {
            out.push_str("\n\n---\n\n");
            out.push_str(&doc);
        }
        Some(out)
    }

    /// The `--` comment block immediately above the line `span` starts on.
    ///
    /// Comments are discarded by the lexer, so documentation is recovered from
    /// the source text — the run of comment lines directly above a definition,
    /// with the leading `--` stripped.
    pub fn doc_above(&self, span: Span) -> Option<String> {
        let before = self.source.get(..span.start as usize)?;
        let mut lines: Vec<&str> = Vec::new();
        // The line the definition starts on is partial; skip it.
        for line in before.lines().rev().skip(1) {
            let t = line.trim();
            if t.is_empty() && lines.is_empty() {
                continue;
            }
            match t.strip_prefix("--") {
                Some(rest) => lines.push(rest.trim()),
                None => break,
            }
        }
        if lines.is_empty() {
            return None;
        }
        lines.reverse();
        Some(lines.join("\n"))
    }
}

fn covers(s: Span, offset: usize) -> bool {
    (s.start as usize) <= offset && offset < (s.end as usize)
}

/// Type and constructor names declared by `decls`.
fn collect_names(
    decls: &[hir::LDecl],
    types: &mut std::collections::HashSet<String>,
    ctors: &mut std::collections::HashSet<String>,
) {
    for d in decls {
        match d.value() {
            hir::Decl::Data(dd) => {
                types.insert(dd.name.to_string());
                ctors.extend(dd.variants.iter().map(|v| v.name.to_string()));
            }
            hir::Decl::Record(rd) => {
                types.insert(rd.name.to_string());
                ctors.insert(rd.name.to_string());
            }
            hir::Decl::Effect(ed) => {
                types.insert(ed.name.to_string());
            }
            _ => {}
        }
    }
}
