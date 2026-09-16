//! **Macro expansion**: `name!(…)`.
//!
//! A macro call is parsed as a name and an opaque argument -- balanced brackets
//! and nothing more -- so this is where it first means anything. Expansion runs
//! after [`crate::cfg`] and before name resolution, and every call is gone by
//! the time it finishes: replaced by what the macro produced, or reported and
//! replaced by something harmless so the rest of the module still compiles.
//!
//! What a macro produces is **tokens**, which are then parsed with the entry
//! point for the position the call was in ([`meadow_parser::parse_expr`] and
//! friends). Nothing here builds a tree directly. That is what keeps a macro
//! unable to say anything the language could not: if it does not parse, it is
//! an error, exactly as it would be written out by hand.
//!
//! A module's `macro` declarations are read first and are gone before anything
//! else looks: a macro is not a value, so there is nowhere past here to put
//! one. Reading them all up front is also what lets a macro be called above
//! where it is written, as every other top-level name can be.
//!
//! These built-ins are always available, and a `macro` may not take one of
//! their names:
//!
//! | call | answers |
//! |---|---|
//! | `line!()` | the line the call is written on |
//! | `file!()` | the name of the file it is written in |
//! | `stringify!(…)` | its argument, written back as text |
//! | `concat!(a, b, …)` | its literal arguments, joined into one string |

mod hygiene;
mod rules;

use meadow_ast as ast;
use meadow_diagnostics::Diagnostic;
use meadow_intern::InternedString;
use meadow_lexer::{LToken, Token, tt};
use meadow_span::{Located, Span};
use rules::Matcher;
use std::collections::HashMap;

/// How many times a macro may expand into another before we call it a loop.
/// Rust's default is 128; a macro that needs more is almost always recursive
/// without a base case, and the limit is what turns a hang into an error.
const MAX_DEPTH: usize = 128;

/// The macros a built-in name takes, which a `macro` may not.
const BUILT_IN: &[&str] = &["line", "file", "stringify", "concat"];

/// A macro definition, with its matchers read and checked.
struct Macro {
    name: InternedString,
    rules: Vec<(Matcher, tt::Group)>,
}

/// Expand every macro call in `module`.
///
/// `text` is the module's source, for the macros that ask where they are;
/// `filename` names it in diagnostics.
pub fn expand(module: &mut ast::Module, text: &str, filename: &str, diags: &mut Vec<Diagnostic>) {
    let mut ex = Expander {
        text,
        filename,
        diags,
        depth: 0,
        macros: HashMap::new(),
        expansions: 0,
    };
    ex.collect(&mut module.decls);
    ex.decls(&mut module.decls);
}

struct Expander<'a> {
    text: &'a str,
    filename: &'a str,
    diags: &'a mut Vec<Diagnostic>,
    depth: usize,
    macros: HashMap<InternedString, Macro>,
    /// How many expansions have happened, which is where a mark comes from:
    /// two expansions of the same macro must not share one.
    expansions: u32,
}

impl Expander<'_> {
    // --- reading the definitions --------------------------------------------

    /// Take every `macro` declaration out of `decls` and read its rules.
    fn collect(&mut self, decls: &mut Vec<ast::LDecl>) {
        let mut kept = Vec::with_capacity(decls.len());
        for d in std::mem::take(decls) {
            // A macro may carry attributes like anything else; `@cfg` has
            // already had its say, and visibility waits for export.
            let def = match &*d.value {
                ast::Decl::Macro(m) => Some(m),
                ast::Decl::Attributed(_, inner) => match &*inner.value {
                    ast::Decl::Macro(m) => Some(m),
                    _ => None,
                },
                _ => None,
            };
            let Some(def) = def else {
                kept.push(d);
                continue;
            };
            self.define(def, d.span);
        }
        *decls = kept;
    }

    /// Read one definition into the table, reporting a rule that could not work
    /// where it is written rather than at every call.
    fn define(&mut self, def: &ast::MacroDef, span: Span) {
        let name = *def.name.value();
        if BUILT_IN.contains(&&*name.to_string()) {
            self.error(
                format!("`{name}!` is a built-in macro"),
                "this name is taken".to_string(),
                def.name.span,
                vec![],
            );
            return;
        }
        if let Some(had) = self.macros.get(&name) {
            let _ = had;
            self.error(
                format!("the macro `{name}!` is defined twice"),
                "this module already has one with this name".to_string(),
                def.name.span,
                vec![],
            );
            return;
        }
        let mut rules = Vec::with_capacity(def.rules.len());
        for rule in &def.rules {
            match Matcher::read(&rule.matcher.trees) {
                Ok(m) => rules.push((m, rule.template.clone())),
                Err(bad) => {
                    self.invalid(bad, span);
                    return;
                }
            }
        }
        self.macros.insert(name, Macro { name, rules });
    }

    /// Report something wrong with how a macro is written.
    fn invalid(&mut self, bad: rules::Invalid, fallback: Span) {
        let span = if bad.span == Span::default() {
            fallback
        } else {
            bad.span
        };
        self.error(bad.msg, bad.label, span, vec![]);
    }

    // --- the built-ins ------------------------------------------------------

    /// Run `call`, answering the tokens it produced, or say why it cannot be
    /// run. `None` means the call is gone and an error has been reported.
    ///
    /// Every built-in answers a single literal, so they all take the call's own
    /// span: what they produced is not written anywhere, and the call is the
    /// nearest thing in the file to point at.
    fn run(&mut self, call: &ast::MacCall) -> Option<Vec<LToken>> {
        let span = call.arg.span();
        let one = |t: Token| Some(vec![LToken::new(t, span)]);
        // A qualified name waits for macros that can be exported, which is what
        // there would be to qualify.
        match call.name().as_str() {
            "line" => {
                self.no_argument(call)?;
                let line = line_of(self.text, call.path_span().start);
                one(Token::Int(line as i64))
            }
            "file" => {
                self.no_argument(call)?;
                one(Token::String(InternedString::from(self.filename)))
            }
            "stringify" => one(Token::String(InternedString::from(tt::render(
                &call.arg.trees,
            )))),
            "concat" => {
                let text = self.concat(call)?;
                one(Token::String(InternedString::from(text)))
            }
            _ => self.run_rules(call),
        }
    }

    /// Run a `macro` this module declared: the first rule whose matcher fits
    /// the call's argument, with its template written out.
    fn run_rules(&mut self, call: &ast::MacCall) -> Option<Vec<LToken>> {
        // A qualified name waits for macros that can be exported, which is what
        // there would be to qualify.
        let name = InternedString::from(call.name().as_str());
        let Some(mac) = self.macros.get(&name) else {
            let known = self.known();
            self.error(
                format!("there is no macro `{}!`", call.name()),
                "unknown macro".to_string(),
                call.path_span(),
                vec![(known, call.path_span())],
            );
            return None;
        };
        // Each expansion gets a mark of its own, so that two of the same macro
        // do not share the locals their templates introduce.
        self.expansions += 1;
        let id = self.expansions;
        let at = call.arg.span();

        // Cloned out of the table: writing the template is `&mut self` work,
        // since a failure in it is reported.
        let rules: Vec<_> = mac.rules.iter().map(|(_, t)| t.clone()).collect();
        let matched = mac
            .rules
            .iter()
            .position(|(m, _)| m.match_trees(&call.arg.trees).is_some());
        let bound = matched.and_then(|i| mac.rules[i].0.match_trees(&call.arg.trees));

        let (Some(i), Some(bound)) = (matched, bound) else {
            self.error(
                format!("no rule of `{}!` matches this call", call.name()),
                "this argument fits none of them".to_string(),
                at,
                vec![],
            );
            return None;
        };
        match rules::substitute(&rules[i].trees, &bound, at, &|t| hygiene::mark(t, id)) {
            Ok(tokens) => Some(tokens),
            Err(bad) => {
                self.invalid(bad, at);
                None
            }
        }
    }

    /// What to suggest when a macro is not found.
    fn known(&self) -> String {
        let mut names: Vec<String> = self
            .macros
            .values()
            .map(|m| format!("`{}!`", m.name))
            .collect();
        names.sort();
        for b in BUILT_IN {
            names.push(format!("`{b}!`"));
        }
        format!("the macros in scope are {}", names.join(", "))
    }

    /// Check that `call` was given nothing, for the macros that take nothing.
    fn no_argument(&mut self, call: &ast::MacCall) -> Option<()> {
        if call.arg.trees.is_empty() {
            return Some(());
        }
        self.error(
            format!("`{}!` takes no arguments", call.name()),
            "this is not read".to_string(),
            call.arg.span(),
            vec![],
        );
        None
    }

    /// `concat!(a, b, c)`: the text of each literal argument, joined.
    fn concat(&mut self, call: &ast::MacCall) -> Option<String> {
        let mut out = String::new();
        let mut bad = false;
        for piece in split(&call.arg.trees, Token::Comma) {
            // A trailing comma leaves an empty piece, which is no argument at
            // all rather than a bad one.
            if piece.is_empty() {
                continue;
            }
            match piece {
                [tt::TokenTree::Token(t)] if let Some(text) = literal_text(t.value()) => {
                    out.push_str(&text)
                }
                _ => {
                    let span = piece
                        .first()
                        .expect("a piece that is not empty")
                        .span()
                        .extend(piece.last().expect("the same piece").span());
                    self.error(
                        "`concat!` takes literals".to_string(),
                        "this is not a literal".to_string(),
                        span,
                        vec![],
                    );
                    bad = true;
                }
            }
        }
        if bad { None } else { Some(out) }
    }

    // --- expanding in each position -----------------------------------------

    /// Expand `call` and parse what it produced as an expression.
    ///
    /// Marks come off the names that are not variables once it is parsed: a
    /// label and a variable are the same token, and only a tree tells them
    /// apart (see [`hygiene`]).
    fn as_expr(&mut self, call: &ast::MacCall, span: Span) -> Option<ast::LExpr> {
        let out = self.run(call)?;
        let (parsed, errs) = meadow_parser::parse_expr(&out, span);
        let mut e = self.parsed(call, span, "an expression", parsed, !errs.is_empty())?;
        hygiene::strip_in_expr(&mut e);
        Some(e)
    }

    fn as_pat(&mut self, call: &ast::MacCall, span: Span) -> Option<ast::LPat> {
        let out = self.run(call)?;
        let (parsed, errs) = meadow_parser::parse_pat(&out, span);
        let mut p = self.parsed(call, span, "a pattern", parsed, !errs.is_empty())?;
        hygiene::strip_in_pat(&mut p);
        Some(p)
    }

    fn as_decls(&mut self, call: &ast::MacCall, span: Span) -> Option<Vec<ast::LDecl>> {
        let out = self.run(call)?;
        let (parsed, errs) = meadow_parser::parse_decls(&out, span);
        let mut ds = self.parsed(call, span, "declarations", parsed, !errs.is_empty())?;
        hygiene::strip_items(&mut ds);
        Some(ds)
    }

    /// Report a macro whose output did not parse where it was called.
    ///
    /// The error is at the call rather than inside the expansion, because the
    /// expansion is not in the file: there is nothing to point at. It names the
    /// macro, which is the part the reader can act on.
    fn parsed<T>(
        &mut self,
        call: &ast::MacCall,
        span: Span,
        wanted: &str,
        parsed: Option<T>,
        failed: bool,
    ) -> Option<T> {
        match parsed {
            Some(t) if !failed => Some(t),
            _ => {
                self.error(
                    format!("`{}!` did not expand to {wanted}", call.name()),
                    format!("this call is where {wanted} belongs"),
                    span,
                    vec![],
                );
                None
            }
        }
    }

    // --- walking --------------------------------------------------------------

    fn decls(&mut self, decls: &mut Vec<ast::LDecl>) {
        // Rebuilt rather than edited in place: one call may expand to several
        // declarations, or to none.
        let mut out = Vec::with_capacity(decls.len());
        for mut d in std::mem::take(decls) {
            match &*d.value {
                ast::Decl::MacCall(call) => {
                    let Some(made) = self.deeper(|ex| ex.as_decls(call, d.span)) else {
                        continue;
                    };
                    let mut made = made;
                    self.decls(&mut made);
                    out.extend(made);
                }
                _ => {
                    self.decl(&mut d);
                    out.push(d);
                }
            }
        }
        *decls = out;
    }

    fn decl(&mut self, d: &mut ast::LDecl) {
        match &mut *d.value {
            ast::Decl::Bind(b) => self.bind(b),
            ast::Decl::Attributed(_, inner) => self.decl(inner),
            // Nothing else holds an expression or a pattern: a type is not a
            // place a macro may be called (see `docs/MACROS.md`).
            ast::Decl::MacCall(_)
            | ast::Decl::Macro(_)
            | ast::Decl::Mod(_)
            | ast::Decl::Use(_)
            | ast::Decl::Data(_)
            | ast::Decl::Record(_)
            | ast::Decl::Effect(_)
            | ast::Decl::TypeAlias(_)
            | ast::Decl::Sig(_, _) => {}
        }
    }

    fn bind(&mut self, b: &mut ast::Bind) {
        match b {
            ast::Bind::Pat(p, e) => {
                self.pat(p);
                self.expr(e);
            }
            ast::Bind::Fun(_, params, _, body) => {
                for p in params {
                    self.pat(p);
                }
                self.expr(body);
            }
        }
    }

    fn pat(&mut self, p: &mut ast::LPat) {
        if let ast::Pat::MacCall(call) = &*p.value {
            let span = p.span;
            match self.deeper(|ex| ex.as_pat(call, span)) {
                // Re-expanded, in case what came out holds a call of its own.
                Some(mut made) => {
                    self.pat(&mut made);
                    *p = made;
                }
                // Left as a wildcard: it matches, binds nothing, and lets the
                // rest of the arm be checked instead of collapsing after one
                // error.
                None => *p = Located::new(ast::Pat::Wildcard, span),
            }
            return;
        }
        match &mut *p.value {
            ast::Pat::Ann(inner, _) => self.pat(inner),
            ast::Pat::As(_, inner) => self.pat(inner),
            ast::Pat::Cons(_, ps)
            | ast::Pat::QualCons(_, _, ps)
            | ast::Pat::Tuple(ps)
            | ast::Pat::Array(ps)
            | ast::Pat::Vector(ps)
            | ast::Pat::List(ps) => {
                for p in ps {
                    self.pat(p);
                }
            }
            ast::Pat::Record(fields, _) => {
                for (_, p) in fields {
                    self.pat(p);
                }
            }
            ast::Pat::MacCall(_)
            | ast::Pat::Wildcard
            | ast::Pat::Var(_)
            | ast::Pat::Lit(_)
            | ast::Pat::Unit => {}
        }
    }

    fn expr(&mut self, e: &mut ast::LExpr) {
        if let ast::Expr::MacCall(call) = &*e.value {
            let span = e.span;
            match self.deeper(|ex| ex.as_expr(call, span)) {
                Some(mut made) => {
                    self.expr(&mut made);
                    *e = made;
                }
                // `()` in its place: the module keeps its shape, so everything
                // around the failed call is still checked.
                None => *e = Located::new(ast::Expr::Unit, span),
            }
            return;
        }
        match &mut *e.value {
            ast::Expr::Lam(ps, body) => {
                for p in ps {
                    self.pat(p);
                }
                self.expr(body);
            }
            ast::Expr::App(f, args) => {
                self.expr(f);
                for a in args {
                    self.expr(a);
                }
            }
            ast::Expr::Let(binds, body) => {
                for b in binds {
                    self.bind(b);
                }
                self.expr(body);
            }
            ast::Expr::If(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            ast::Expr::Match(scrutinee, arms) => {
                self.expr(scrutinee);
                for (p, guard, body) in arms {
                    self.pat(p);
                    if let Some(g) = guard {
                        self.expr(g);
                    }
                    self.expr(body);
                }
            }
            ast::Expr::UnOp(_, x) => self.expr(x),
            ast::Expr::BinOp(_, l, r) => {
                self.expr(l);
                self.expr(r);
            }
            ast::Expr::Interp(_, holes) => {
                for h in holes {
                    self.expr(h);
                }
            }
            ast::Expr::Tuple(xs)
            | ast::Expr::Array(xs)
            | ast::Expr::List(xs)
            | ast::Expr::Cons(_, xs) => {
                for x in xs {
                    self.expr(x);
                }
            }
            ast::Expr::Record(fields, base) => {
                for (_, v) in fields {
                    self.expr(v);
                }
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            ast::Expr::Update(base, fields) => {
                self.expr(base);
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            ast::Expr::Field(o, _) => self.expr(o),
            ast::Expr::Handle(body, arms, ret) => {
                self.expr(body);
                for arm in arms {
                    self.pat(&mut arm.param);
                    self.expr(&mut arm.body);
                }
                if let Some((p, e)) = ret {
                    self.pat(p);
                    self.expr(e);
                }
            }
            ast::Expr::MacCall(_)
            | ast::Expr::Var(_)
            | ast::Expr::Lit(_)
            | ast::Expr::Qual(_, _)
            | ast::Expr::Unit
            | ast::Expr::Hole => {}
        }
    }

    // --- the depth limit --------------------------------------------------

    /// Run `f` one expansion deeper, or report a macro that never stops.
    fn deeper<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let out = f(self);
        self.depth -= 1;
        out
    }

    fn error(&mut self, msg: String, label: String, span: Span, extra: Vec<(String, Span)>) {
        self.diags.push(Diagnostic::new(
            msg,
            self.filename.to_string(),
            (label, span),
            extra,
        ));
    }
}

/// The 1-based line `at` is on in `text`.
fn line_of(text: &str, at: u32) -> usize {
    let at = (at as usize).min(text.len());
    text[..at].bytes().filter(|&b| b == b'\n').count() + 1
}

/// `trees` split on every top-level `sep`, separators dropped. A separator
/// inside a bracket belongs to that group, not to this split.
fn split(trees: &[tt::TokenTree], sep: Token) -> Vec<&[tt::TokenTree]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, t) in trees.iter().enumerate() {
        if matches!(t, tt::TokenTree::Token(t) if *t.value() == sep) {
            out.push(&trees[start..i]);
            start = i + 1;
        }
    }
    out.push(&trees[start..]);
    out
}

/// The text a literal token stands for: what `concat!` joins. `None` for a
/// token that is not a literal.
fn literal_text(t: &Token) -> Option<String> {
    match t {
        // The string's contents, not the quotes around it.
        Token::String(s) => Some(s.to_string()),
        Token::Char(c) => Some(c.to_string()),
        Token::Int(_) | Token::Real(_) => Some(t.text()),
        _ => None,
    }
}
