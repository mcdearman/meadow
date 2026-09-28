//! MeadowBoot -- the compiler written in Meadow, in `bootstrap/` -- checked
//! against this one, pass by pass.
//!
//! Each pass of MeadowBoot writes what it made as text, one item a line, and
//! the same pass here is written the same way; a file whose two texts differ
//! is a difference between the compilers, shown at its first line. The inputs
//! are every Meadow source in the repository and `bootstrap/tests/`, where the
//! cases nothing else exercises are kept.
//!
//! MeadowBoot is run by the `meadow` this test is built with. A debug `meadow`
//! builds it slowly; `MEADOWBOOT_MEADOW` names another -- a release build --
//! to run it with instead.

use meadow_compiler::core::{self, Lit, Pat, Term};
use meadow_compiler::lexer::{Token, tokenize};
use meadow_compiler::source::{Source, SourceKind};
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository")
}

fn meadow() -> PathBuf {
    std::env::var_os("MEADOWBOOT_MEADOW")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_meadow")))
}

/// What MeadowBoot prints for `args`, run with `meadow run --release`: offline
/// when its dependencies are fetched already, and fetching them when not.
fn meadowboot(args: &[String]) -> String {
    // One at a time: two tests building MeadowBoot at once write the same
    // files, and one of them links a half-written executable.
    static BUILDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo().join("bootstrap");
    let run = |offline: bool| {
        let mut c = Command::new(meadow());
        c.arg("run").arg("--release");
        if offline {
            c.arg("--offline");
        }
        c.arg(&dir).arg("--").args(args);
        c.output().expect("meadow runs")
    };
    let mut out = run(true);
    // Online again only for what offline could not have: a dependency not
    // fetched yet. Any other failure -- a build that did not finish -- is the
    // answer, and running it twice doubles what it cost.
    if !out.status.success()
        && out.stdout.is_empty()
        && String::from_utf8_lossy(&out.stderr).contains("not in the cache")
    {
        out = run(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !stdout.is_empty(),
        "MeadowBoot printed nothing:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Every `.mw` file under `dirs`, but what a build wrote.
fn sources(dirs: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "mw") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(&repo().join(d), &mut out);
    }
    out
}

/// MeadowBoot's output for one command over `files`, split by the `== path`
/// line it writes before each: in batches, so a command line stays short.
fn per_file(command: &str, files: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for batch in files.chunks(40) {
        let mut args = vec![command.to_string()];
        args.extend(batch.iter().map(|p| p.display().to_string()));
        let text = meadowboot(&args);
        // A line of a dump never starts with `== `: its lines start with a
        // number, or `! `.
        let mut sections: Vec<String> = Vec::new();
        for line in text.split_inclusive('\n') {
            if line.starts_with("== ") {
                sections.push(String::new());
            } else if let Some(s) = sections.last_mut() {
                s.push_str(line);
            }
        }
        let mut sections = sections.into_iter();
        for p in batch {
            out.push((p.clone(), sections.next().unwrap_or_default()));
        }
    }
    out
}

/// The files whose two texts differ, each at its first differing line.
fn differences(files: &[(PathBuf, String)], reference: impl Fn(&Path) -> String) -> Vec<String> {
    let mut bad = Vec::new();
    for (path, theirs) in files {
        let ours = reference(path);
        if &ours == theirs {
            continue;
        }
        let (a, b): (Vec<&str>, Vec<&str>) = (ours.lines().collect(), theirs.lines().collect());
        let at = a
            .iter()
            .zip(&b)
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        let show = |ls: &[&str]| {
            ls.get(at.saturating_sub(2)..(at + 3).min(ls.len()))
                .unwrap_or(&[])
                .join("\n    ")
        };
        bad.push(format!(
            "{}: first difference at line {}\n  Rust:\n    {}\n  MeadowBoot:\n    {}",
            path.display(),
            at + 1,
            show(&a),
            show(&b)
        ));
    }
    bad
}

// --- lexing --------------------------------------------------------------------

/// `text`'s tokens as `meadow-lexer` reads them, written as MeadowBoot's `lex`
/// writes its own: `from to Kind payload` a line, then `! from to message`.
fn lexed(text: &str) -> String {
    let source = Source::new(SourceKind::Interactive, text.into());
    let result = tokenize(source);
    let mut out = String::new();
    for t in &result.tokens {
        let (from, to) = (t.span.start as usize, t.span.end as usize);
        out.push_str(&format!(
            "{from} {to} {}\n",
            described(t.value(), &text[from..to])
        ));
    }
    for d in &result.errors {
        let span = d.label.1;
        out.push_str(&format!(
            "! {} {} {}\n",
            span.start,
            span.end,
            escape(&d.msg)
        ));
    }
    out
}

fn described(t: &Token, text: &str) -> String {
    match t {
        Token::Int(n) => format!("Int {n}"),
        // Its text: what it is as a number is the parser's to check.
        Token::Real(_) => format!("Real {text}"),
        Token::String(s) => format!("String {}", escape(s)),
        Token::InterpStart(s) => format!("InterpStart {}", escape(s)),
        Token::InterpMid(s) => format!("InterpMid {}", escape(s)),
        Token::InterpEnd(s) => format!("InterpEnd {}", escape(s)),
        Token::Char(c) => format!("Char {}", *c as u32),
        Token::LowerIdent(s) => format!("LowerIdent {s}"),
        Token::UpperIdent(s) => format!("UpperIdent {s}"),
        Token::OpIdent(s) => format!("OpIdent {s}"),
        Token::ConOpIdent(s) => format!("ConOpIdent {s}"),
        other => format!("{other:?}"),
    }
}

/// Every control byte and backslash as `\xNN`, as MeadowBoot's `escape` has it.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let n = c as u32;
        if n < 32 || n == 127 || c == '\\' {
            out.push_str(&format!("\\x{n:02X}"));
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn every_source_lexes_as_meadow_lexer_lexes_it() {
    let files = sources(&["lib", "examples", "benches", "bootstrap", "glade", "silo"]);
    assert!(files.len() > 50, "only {} sources found", files.len());
    let theirs = per_file("lex", &files);
    let bad = differences(&theirs, |p| {
        lexed(&std::fs::read_to_string(p).unwrap_or_default())
    });
    assert!(
        bad.is_empty(),
        "{} of {} files lex differently:\n\n{}",
        bad.len(),
        files.len(),
        bad.iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

// --- parsing -------------------------------------------------------------------

use meadow_compiler::ast::{
    Attr, Bind, Decl, EffectRow, Expr, Field, HandlerArm, Ident, LDecl, LExpr, LPat, LType,
    Lit as ALit, MacCall, Meta, Pat as APat, TypeExpr, VariantFields,
};
use meadow_compiler::lexer::tt::{Delim, Group, TokenTree};
use meadow_compiler::span::Span;

/// `text`'s tree as `meadow-parser` reads it, written as MeadowBoot's `parse`
/// writes its own: a node a line, its children indented under it, each with
/// the bytes it spans -- or `parse failed`, when it does not parse.
fn parsed(text: &str) -> String {
    let source = Source::new(SourceKind::Interactive, text.into());
    let lexed = tokenize(source);
    let (ast, _) = meadow_compiler::parser::parse("boot".into(), source, &lexed.tokens);
    match ast {
        None => "parse failed\n".to_string(),
        Some(m) => {
            let mut w = Tree {
                out: String::new(),
                depth: 0,
                text,
            };
            w.node("Module", m.span, |w| {
                for d in &m.value.decls {
                    w.decl(d);
                }
            });
            w.out
        }
    }
}

/// A tree being written: a line a node, indented two spaces a level.
struct Tree<'t> {
    out: String,
    depth: usize,
    /// The source, for what a float literal says.
    text: &'t str,
}

fn at(s: Span) -> String {
    format!("{}..{}", s.start, s.end)
}

impl Tree<'_> {
    fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn under(&mut self, head: &str, f: impl FnOnce(&mut Self)) {
        self.line(head);
        self.depth += 1;
        f(self);
        self.depth -= 1;
    }

    fn node(&mut self, tag: &str, span: Span, f: impl FnOnce(&mut Self)) {
        self.under(&format!("{tag} {}", at(span)), f);
    }

    fn ident(&mut self, i: &Ident) {
        self.line(&format!("ident {} {}", escape(i.value()), at(i.span)));
    }

    fn idents(&mut self, name: &str, is: &[Ident]) {
        self.under(&format!("{name} {}", is.len()), |w| {
            for i in is {
                w.ident(i);
            }
        });
    }

    fn opt<T>(&mut self, name: &str, x: Option<&T>, f: impl FnOnce(&mut Self, &T)) {
        match x {
            None => self.line(&format!("{name} none")),
            Some(x) => self.under(name, |w| f(w, x)),
        }
    }

    fn list<T>(&mut self, name: &str, xs: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.under(&format!("{name} {}", xs.len()), |w| {
            for x in xs {
                f(w, x);
            }
        });
    }

    fn decl(&mut self, d: &LDecl) {
        let s = d.span;
        match d.value() {
            Decl::Bind(b) => self.node("Bind", s, |w| w.bind(b)),
            Decl::Mod(n) => self.node("Mod", s, |w| w.ident(n)),
            Decl::Use(u) => self.node("Use", s, |w| {
                w.idents("path", &u.path);
                w.line(&format!("glob {}", u.glob));
                w.opt("alias", u.alias.as_ref(), |w, a| w.ident(a));
                w.idents("names", &u.names);
                w.idents("macros", &u.macros);
            }),
            Decl::Data(d) => self.node("Data", s, |w| {
                w.ident(&d.name);
                w.idents("params", &d.params);
                w.list("variants", &d.variants, |w, v| {
                    w.under("Variant", |w| {
                        w.list("attrs", &v.attrs, |w, a| w.attr(a));
                        w.ident(&v.name);
                        match &v.fields {
                            VariantFields::Positional(ts) => {
                                w.list("positional", ts, |w, t| w.ty(t))
                            }
                            VariantFields::Named(fs) => w.list("named", fs, |w, f| w.field(f)),
                        }
                    })
                });
            }),
            Decl::Record(r) => self.node("Record", s, |w| {
                w.ident(&r.name);
                w.idents("params", &r.params);
                w.list("fields", &r.fields, |w, f| w.field(f));
            }),
            Decl::Effect(e) => self.node("Effect", s, |w| {
                w.ident(&e.name);
                w.idents("params", &e.params);
                w.list("ops", &e.ops, |w, f| w.field(f));
            }),
            Decl::TypeAlias(t) => self.node("TypeAlias", s, |w| {
                w.ident(&t.name);
                w.idents("params", &t.params);
                w.ty(&t.ty);
            }),
            Decl::Sig(n, t, bounds) => self.node("Sig", s, |w| {
                w.ident(n);
                w.ty(t);
                w.list("bounds", bounds, |w, b| w.bound(b));
            }),
            Decl::Trait(t) => self.node("Trait", s, |w| {
                w.ident(&t.name);
                w.idents("params", &t.params);
                w.list("supers", &t.supers, |w, b| w.bound(b));
                w.list("assocs", &t.assocs, |w, (n, ps)| {
                    w.under("Assoc", |w| {
                        w.ident(n);
                        w.idents("params", ps);
                    })
                });
                w.list("sigs", &t.sigs, |w, (n, ty)| {
                    w.under("Sig", |w| {
                        w.ident(n);
                        w.ty(ty);
                    })
                });
                w.list("defaults", &t.defaults, |w, b| {
                    w.under("Default", |w| w.bind(b))
                });
            }),
            Decl::Impl(i) => self.node("Impl", s, |w| {
                w.ident(&i.tr);
                w.list("tys", &i.tys, |w, t| w.ty(t));
                w.list("context", &i.context, |w, b| w.bound(b));
                w.list("assocs", &i.assocs, |w, (n, args, ty)| {
                    w.under("Assoc", |w| {
                        w.ident(n);
                        w.list("args", args, |w, t| w.ty(t));
                        w.ty(ty);
                    })
                });
                w.list("methods", &i.methods, |w, b| {
                    w.under("Method", |w| w.bind(b))
                });
            }),
            Decl::Fixity(assoc, n, ops) => self.node(&format!("Fixity {assoc:?} {n}"), s, |w| {
                w.idents("ops", ops)
            }),
            Decl::Attributed(attrs, d) => self.node("Attributed", s, |w| {
                w.list("attrs", attrs, |w, a| w.attr(a));
                w.decl(d);
            }),
            Decl::MacCall(m) => self.node("MacCall", s, |w| w.mac(m)),
            Decl::Macro(m) => self.node("Macro", s, |w| {
                w.ident(&m.name);
                w.list("rules", &m.rules, |w, r| {
                    w.under("Rule", |w| {
                        w.group(&r.matcher);
                        w.group(&r.template);
                    })
                });
            }),
        }
    }

    fn field(&mut self, f: &Field) {
        self.under("Field", |w| {
            w.list("attrs", &f.attrs, |w, a| w.attr(a));
            w.ident(&f.name);
            w.ty(&f.ty);
        });
    }

    fn attr(&mut self, a: &Attr) {
        self.under("Attr", |w| {
            w.ident(&a.name);
            w.idents("args", &a.args);
            w.list("meta", &a.meta, |w, m| w.meta(m));
        });
    }

    fn meta(&mut self, m: &Meta) {
        match m {
            Meta::Word(n) => self.under("Word", |w| w.ident(n)),
            Meta::Text(n) => self.under("Text", |w| w.ident(n)),
            Meta::Value(n, v) => self.under("Value", |w| {
                w.ident(n);
                w.ident(v);
            }),
            Meta::List(n, ms) => self.under("List", |w| {
                w.ident(n);
                w.list("meta", ms, |w, m| w.meta(m));
            }),
        }
    }

    fn bound(&mut self, b: &meadow_compiler::ast::Bound) {
        self.under("Bound", |w| {
            w.ident(&b.tr);
            w.list("tys", &b.tys, |w, t| w.ty(t));
        });
    }

    fn bind(&mut self, b: &Bind) {
        match b {
            Bind::Pat(p, e) => self.under("PatBind", |w| {
                w.pat(p);
                w.expr(e);
            }),
            Bind::Fun(n, ps, ret, body) => self.under("FunBind", |w| {
                w.ident(n);
                w.list("params", ps, |w, p| w.pat(p));
                w.opt("ret", ret.as_ref(), |w, t| w.ty(t));
                w.expr(body);
            }),
        }
    }

    fn ty(&mut self, t: &LType) {
        let s = t.span;
        match t.value() {
            TypeExpr::Var(n) => self.node("TVar", s, |w| w.ident(n)),
            TypeExpr::Con(n, args) => self.node("TCon", s, |w| {
                w.ident(n);
                w.list("args", args, |w, t| w.ty(t));
            }),
            TypeExpr::Fun(params, ret, eff) => self.node("TFun", s, |w| {
                w.list("params", params, |w, t| w.ty(t));
                w.ty(ret);
                w.opt("effect", eff.as_ref(), |w, e| w.row(e));
            }),
            TypeExpr::Tuple(ts) => self.node("TTuple", s, |w| w.list("items", ts, |w, t| w.ty(t))),
            TypeExpr::Vector(t) => self.node("TVector", s, |w| w.ty(t)),
            TypeExpr::List(t) => self.node("TList", s, |w| w.ty(t)),
            TypeExpr::Record(fields, tail) => self.node("TRecord", s, |w| {
                w.list("fields", fields, |w, (n, t)| {
                    w.under("TField", |w| {
                        w.ident(n);
                        w.ty(t);
                    })
                });
                w.opt("tail", tail.as_ref(), |w, r| w.ident(r));
            }),
        }
    }

    fn row(&mut self, e: &EffectRow) {
        self.under("Row", |w| {
            w.list("labels", &e.labels, |w, (n, args)| {
                w.under("Label", |w| {
                    w.ident(n);
                    w.list("args", args, |w, t| w.ty(t));
                })
            });
            w.opt("tail", e.tail.as_ref(), |w, r| w.ident(r));
        });
    }

    fn lit(&mut self, l: &ALit, span: Span) {
        let s = match l {
            ALit::Int(i) => format!("Int {i}"),
            // What the literal says, and its sign: its value, less a parse.
            ALit::Float(_) => {
                let text = &self.text[span.start as usize..span.end as usize];
                let minuses = text.chars().filter(|c| *c == '-').count()
                    - text
                        .trim_start_matches(|c: char| c == '-' || c.is_whitespace())
                        .chars()
                        .filter(|c| *c == '-')
                        .count();
                let digits = text.trim_start_matches(|c: char| c == '-' || c.is_whitespace());
                format!("Float {}{digits}", if minuses % 2 == 1 { "-" } else { "" })
            }
            ALit::String(s) => format!("String {}", escape(s)),
            ALit::Char(c) => format!("Char {}", *c as u32),
        };
        self.line(&s);
    }

    fn pat(&mut self, p: &LPat) {
        let s = p.span;
        match p.value() {
            APat::Wildcard => self.node("PWild", s, |_| {}),
            APat::Var(n) => self.node("PVar", s, |w| w.ident(n)),
            APat::Ann(inner, t) => self.node("PAnn", s, |w| {
                w.pat(inner);
                w.ty(t);
            }),
            APat::Lit(l) => self.node("PLit", s, |w| w.lit(l, s)),
            APat::As(n, inner) => self.node("PAs", s, |w| {
                w.ident(n);
                w.pat(inner);
            }),
            APat::Cons(n, ps) => self.node("PCons", s, |w| {
                w.ident(n);
                w.list("args", ps, |w, p| w.pat(p));
            }),
            APat::QualCons(q, n, ps) => self.node("PQualCons", s, |w| {
                w.ident(q);
                w.ident(n);
                w.list("args", ps, |w, p| w.pat(p));
            }),
            APat::Tuple(ps) => self.node("PTuple", s, |w| w.list("items", ps, |w, p| w.pat(p))),
            APat::Array(ps) => self.node("PArray", s, |w| w.list("items", ps, |w, p| w.pat(p))),
            APat::Vector(ps) => self.node("PVector", s, |w| w.list("items", ps, |w, p| w.pat(p))),
            APat::List(ps) => self.node("PList", s, |w| w.list("items", ps, |w, p| w.pat(p))),
            APat::Record(fields, open) => self.node(&format!("PRecord {open}"), s, |w| {
                w.list("fields", fields, |w, (n, p)| {
                    w.under("PField", |w| {
                        w.ident(n);
                        w.pat(p);
                    })
                })
            }),
            APat::Unit => self.node("PUnit", s, |_| {}),
            APat::View(e, p) => self.node("PView", s, |w| {
                w.expr(e);
                w.pat(p);
            }),
            APat::MacCall(m) => self.node("PMacCall", s, |w| w.mac(m)),
        }
    }

    fn expr(&mut self, e: &LExpr) {
        let s = e.span;
        match e.value() {
            Expr::Var(n) => self.node("Var", s, |w| w.ident(n)),
            Expr::Lit(l) => self.node("Lit", s, |w| w.lit(l, s)),
            Expr::Interp(texts, holes) => self.node("Interp", s, |w| {
                w.list("texts", texts, |w, t| {
                    w.line(&format!("text {}", escape(t)))
                });
                w.list("holes", holes, |w, (e, fmt)| {
                    w.under(&format!("Hole {fmt:?}"), |w| w.expr(e))
                });
            }),
            Expr::Lam(ps, body) => self.node("Lam", s, |w| {
                w.list("params", ps, |w, p| w.pat(p));
                w.expr(body);
            }),
            Expr::App(f, args) => self.node("App", s, |w| {
                w.expr(f);
                w.list("args", args, |w, a| w.expr(a));
            }),
            Expr::Let(binds, body) => self.node("Let", s, |w| {
                w.list("binds", binds, |w, b| w.bind(b));
                w.expr(body);
            }),
            Expr::If(c, a, b) => self.node("If", s, |w| {
                w.expr(c);
                w.expr(a);
                w.expr(b);
            }),
            Expr::Match(scrut, arms) => self.node("Match", s, |w| {
                w.expr(scrut);
                w.list("arms", arms, |w, (p, g, b)| {
                    w.under("Arm", |w| {
                        w.pat(p);
                        w.opt("guard", g.as_ref(), |w, g| w.expr(g));
                        w.expr(b);
                    })
                });
            }),
            Expr::UnOp(op, x) => self.node("UnOp", s, |w| {
                w.line(&format!("op {} {}", op.value().to_string(), at(op.span)));
                w.expr(x);
            }),
            Expr::BinOp(op, a, b) => self.node("BinOp", s, |w| {
                w.line(&format!("op {} {}", op.value().to_string(), at(op.span)));
                w.expr(a);
                w.expr(b);
            }),
            Expr::Infix(first, rest) => self.node("Infix", s, |w| {
                w.expr(first);
                w.list("rest", rest, |w, (op, x)| {
                    w.under("Op", |w| {
                        w.ident(op);
                        w.expr(x);
                    })
                });
            }),
            Expr::Tuple(xs) => self.node("Tuple", s, |w| w.list("items", xs, |w, x| w.expr(x))),
            Expr::Array(xs) => self.node("Array", s, |w| w.list("items", xs, |w, x| w.expr(x))),
            Expr::List(xs) => self.node("List", s, |w| w.list("items", xs, |w, x| w.expr(x))),
            Expr::Cons(n, xs) => self.node("Cons", s, |w| {
                w.ident(n);
                w.list("args", xs, |w, x| w.expr(x));
            }),
            Expr::Qual(q, n) => self.node("Qual", s, |w| {
                w.ident(q);
                w.ident(n);
            }),
            Expr::Record(fields, base) => self.node("RecordE", s, |w| {
                w.list("fields", fields, |w, (n, x)| {
                    w.under("FieldE", |w| {
                        w.ident(n);
                        w.expr(x);
                    })
                });
                w.opt("base", base.as_ref(), |w, b| w.expr(b));
            }),
            Expr::Update(base, fields) => self.node("Update", s, |w| {
                w.expr(base);
                w.list("fields", fields, |w, (n, x)| {
                    w.under("FieldE", |w| {
                        w.ident(n);
                        w.expr(x);
                    })
                });
            }),
            Expr::Field(x, n) => self.node("Field", s, |w| {
                w.expr(x);
                w.ident(n);
            }),
            Expr::Handle(x, arms, ret) => self.node("Handle", s, |w| {
                w.expr(x);
                w.list("arms", arms, |w, a| w.harm(a));
                w.opt("ret", ret.as_ref(), |w, (p, b)| {
                    w.pat(p);
                    w.expr(b);
                });
            }),
            Expr::Unit => self.node("Unit", s, |_| {}),
            Expr::Hole => self.node("Hole", s, |_| {}),
            Expr::MacCall(m) => self.node("MacCallE", s, |w| w.mac(m)),
        }
    }

    fn harm(&mut self, a: &HandlerArm) {
        self.under("HArm", |w| {
            w.ident(&a.op);
            w.pat(&a.param);
            w.ident(&a.resume);
            w.expr(&a.body);
        });
    }

    fn mac(&mut self, m: &MacCall) {
        self.idents("path", &m.path);
        self.group(&m.arg);
    }

    fn group(&mut self, g: &Group) {
        let delim = match g.delim {
            Delim::Paren => "Paren",
            Delim::Brack => "Brack",
            Delim::Brace => "Brace",
        };
        self.under(
            &format!("Group {delim} {} {}", at(g.open), at(g.close)),
            |w| {
                for t in &g.trees {
                    match t {
                        TokenTree::Group(g) => w.group(g),
                        TokenTree::Token(t) => {
                            let (from, to) = (t.span.start as usize, t.span.end as usize);
                            let text = &w.text[from..to];
                            w.line(&format!(
                                "tok {} {}",
                                described(t.value(), text),
                                at(t.span)
                            ));
                        }
                    }
                }
            },
        );
    }
}

#[test]
fn every_source_parses_as_meadow_parser_parses_it() {
    let files = sources(&["lib", "examples", "benches", "bootstrap", "glade", "silo"]);
    assert!(files.len() > 50, "only {} sources found", files.len());
    let theirs = per_file("parse", &files);
    let bad = differences(&theirs, |p| {
        parsed(&std::fs::read_to_string(p).expect("a source"))
    });
    assert!(
        bad.is_empty(),
        "{} of {} files parse differently:\n\n{}",
        bad.len(),
        files.len(),
        bad.iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

// --- evaluating: the CEK machine ------------------------------------------------

/// The programs `glade/tests/differential.rs` checks every back end with: the
/// string literal given to each `agree(…)` there. Read from that file, so a
/// case added there is one here.
fn glade_programs() -> Vec<String> {
    let text = std::fs::read_to_string(repo().join("glade/tests/differential.rs"))
        .expect("the glade cases");
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("agree(") {
        rest = &rest[at + "agree(".len()..];
        if let Some((lit, after)) = rust_string(rest.trim_start()) {
            out.push(lit);
            rest = after;
        }
    }
    out
}

/// The Rust string literal `s` starts with -- `"…"` or `r#"…"#` -- and what
/// follows it; `None` when it starts with something else.
fn rust_string(s: &str) -> Option<(String, &str)> {
    if let Some(raw) = s.strip_prefix('r') {
        let hashes = raw.bytes().take_while(|b| *b == b'#').count();
        let body = raw[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = body.find(&close)?;
        return Some((body[..end].to_string(), &body[end + close.len()..]));
    }
    let body = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &body[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                'x' => {
                    let hex: String = (0..2)
                        .filter_map(|_| chars.next().map(|(_, h)| h))
                        .collect();
                    out.push(u8::from_str_radix(&hex, 16).ok()? as char);
                }
                'u' => {
                    chars.next(); // `{`
                    let mut hex = String::new();
                    for (_, h) in chars.by_ref() {
                        if h == '}' {
                            break;
                        }
                        hex.push(h);
                    }
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                // A line continued: the break and the next line's indentation
                // are not in the string.
                '\n' => {
                    while chars.peek().is_some_and(|(_, w)| w.is_whitespace()) {
                        chars.next();
                    }
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

/// `src` compiled with no library, as the glade cases are: its definitions,
/// entered at `main`.
fn compiled(src: &str) -> Result<core::Program, String> {
    let (pkg, diags) = meadow_compiler::compile_str("boot", src);
    if !diags.is_empty() {
        return Err(diags
            .iter()
            .map(|d| d.msg.clone())
            .collect::<Vec<_>>()
            .join("; "));
    }
    let entry = pkg
        .exports
        .iter()
        .find(|e| &*e.name == "main")
        .map(|e| e.var);
    Ok(core::Program {
        defs: pkg.defs.clone(),
        entry,
        ctor_fields: pkg.ctor_fields.clone(),
        variants: pkg.variants.clone(),
        origins: Default::default(),
    })
}

/// What the Rust CEK machine answers for `p`, as a line: its value as the
/// machine displays it, or `error: why`.
fn cek(p: &core::Program) -> String {
    match meadow_eval::run(p) {
        Ok(v) => v.to_string(),
        Err(e) => format!("error: {}", e.msg),
    }
}

/// `p` as MeadowBoot's `eval` reads it: the program the CEK machine runs --
/// generic definitions copied per number type, and types gone -- written in
/// `CoreText`, the grammar in `bootstrap/src/Core.mw`. A word says what each
/// thing is and comes first, a list is in brackets, a number is a number (a
/// float its bits), and names and text are quoted, with `\xNN` for a control
/// byte, a backslash or a quote.
fn core_text(p: &core::Program) -> String {
    let p = core::erase::program(&core::specialize::program(p));
    let mut out = format!("program {}\ndefs", p.entry.map_or(-1, |v| v.0 as i64));
    for d in &p.defs {
        out.push_str(&format!("\n {} ", d.var.0));
        term(&d.term, &mut out);
    }
    out.push_str("\nfields");
    let mut fields: Vec<_> = p.ctor_fields.iter().collect();
    fields.sort_by_key(|(k, _)| k.to_string());
    for (ctor, fs) in fields {
        out.push_str(&format!(" {} [", quoted(ctor)));
        for f in fs {
            out.push_str(&format!(" {}", quoted(f)));
        }
        out.push_str(" ]");
    }
    out.push('\n');
    out
}

fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        let n = c as u32;
        if n < 32 || n == 127 || c == '\\' || c == '"' {
            out.push_str(&format!("\\x{n:02X}"));
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}

/// A literal, exactly: a float is its bits, since text would have to be parsed
/// back, and a parser rounds.
fn lit(l: &Lit, out: &mut String) {
    let s = match l {
        Lit::Int(i) | Lit::AnyInt(i, _) => format!("int {i}"),
        Lit::BigInt(i) => format!("bigint {i}"),
        Lit::Float(x) | Lit::AnyFloat(x, _) => format!("f64 {}", x.to_bits() as i64),
        Lit::Float32(x) => format!("f32 {}", x.to_bits()),
        Lit::Word(w, b) => format!("word {} {}", quoted(&format!("{w:?}")), *b as i64),
        Lit::Str(s) | Lit::Sym(s) => format!("str {}", quoted(s)),
        Lit::Char(c) => format!("char {}", *c as u32),
        Lit::Bool(b) => b.to_string(),
        Lit::Unit => "unit".to_string(),
    };
    out.push_str(&s);
}

fn pats(ps: &[Pat], out: &mut String) {
    out.push_str(" [");
    for q in ps {
        out.push(' ');
        pat(q, out);
    }
    out.push_str(" ]");
}

fn pat(p: &Pat, out: &mut String) {
    match p {
        Pat::Wild => out.push_str("wild"),
        Pat::Var(v, _) => out.push_str(&format!("pvar {}", v.0)),
        Pat::As(v, _, sub) => {
            out.push_str(&format!("pas {} ", v.0));
            pat(sub, out);
        }
        Pat::Lit(l) => {
            out.push_str("plit ");
            lit(l, out);
        }
        Pat::Tuple(ps) => {
            out.push_str("ptuple");
            pats(ps, out);
        }
        Pat::Array(ps) => {
            out.push_str("parray");
            pats(ps, out);
        }
        Pat::Ctor(name, ps) => {
            out.push_str(&format!("pctor {}", quoted(name)));
            pats(ps, out);
        }
        Pat::Record(fields) => {
            out.push_str("precord [");
            for (l, q) in fields {
                out.push_str(&format!(" {} ", quoted(l)));
                pat(q, out);
            }
            out.push_str(" ]");
        }
    }
}

/// `head`, then each of `ts`, a space before each.
fn node(head: &str, ts: &[&Term], out: &mut String) {
    out.push_str(head);
    for t in ts {
        out.push(' ');
        term(t, out);
    }
}

/// `head`, then `ts` in brackets.
fn list(head: &str, ts: &[Term], out: &mut String) {
    out.push_str(head);
    out.push_str(" [");
    for t in ts {
        out.push(' ');
        term(t, out);
    }
    out.push_str(" ]");
}

fn term(t: &Term, out: &mut String) {
    match t {
        Term::Var(v) => out.push_str(&format!("var {}", v.0)),
        Term::Lit(l) => {
            out.push_str("const ");
            lit(l, out);
        }
        Term::Loc(_, inner) | Term::TyLam(_, inner) | Term::TyApp(inner, _) => term(inner, out),
        Term::Lam(v, _, body) => node(&format!("lam {}", v.0), &[body], out),
        Term::App(f, a) => node("app", &[f, a], out),
        Term::Let(v, _, rhs, body) => node(&format!("let {}", v.0), &[rhs, body], out),
        Term::LetRec(binds, body) => {
            out.push_str("letrec [");
            for (v, _, rhs) in binds {
                node(&format!(" {}", v.0), &[rhs], out);
            }
            out.push_str(" ]");
            node("", &[body], out);
        }
        Term::If(c, a, b) => node("if", &[c, a, b], out),
        Term::Tuple(xs) => list("tuple", xs, out),
        Term::Array(xs, _) => list("array", xs, out),
        Term::Proj(x, i) => {
            node("proj", &[x], out);
            out.push_str(&format!(" {i}"));
        }
        Term::Record(fields) => {
            out.push_str("record [");
            for (l, x) in fields {
                node(&format!(" {}", quoted(l)), &[x], out);
            }
            out.push_str(" ]");
        }
        Term::Sel(x, l, _) => {
            node("sel", &[x], out);
            out.push_str(&format!(" {}", quoted(l)));
        }
        Term::Extend(x, l, v) => {
            node("extend", &[x], out);
            node(&format!(" {}", quoted(l)), &[v], out);
        }
        Term::Ctor(name, _, xs) => list(&format!("ctor {}", quoted(name)), xs, out),
        Term::Prim(op, xs, _) => list(&format!("prim {}", quoted(&format!("{op:?}"))), xs, out),
        Term::Case(s, arms, _) => {
            node("case", &[s], out);
            out.push_str(" [");
            for (p, g, b) in arms {
                out.push(' ');
                pat(p, out);
                if let Some(g) = g {
                    node(" when", &[g], out);
                }
                node("", &[b], out);
            }
            out.push_str(" ]");
        }
        Term::Perform(effect, op, arg, _) => node(
            &format!("perform {} {}", quoted(effect), quoted(op)),
            &[arg],
            out,
        ),
        Term::Handle {
            body, clauses, ret, ..
        } => {
            node("handle", &[body], out);
            out.push_str(" [");
            for c in clauses {
                node(
                    &format!(
                        " {} {} {} {}",
                        quoted(&c.effect),
                        quoted(&c.op),
                        c.param.0,
                        c.resume.0
                    ),
                    &[&c.body],
                    out,
                );
            }
            out.push_str(" ]");
            if let Some((v, _, b)) = ret {
                node(&format!(" return {}", v.0), &[b], out);
            }
        }
        Term::Join {
            var,
            params,
            rhs,
            body,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(|(v, _)| format!(" {}", v.0)).collect();
            node(
                &format!("join {} [{} ]", var.0, ps.concat()),
                &[rhs, body],
                out,
            );
        }
        Term::Jump(j, args, _) => list(&format!("jump {}", j.0), args, out),
        Term::Error => out.push_str("error"),
    }
}

#[test]
fn every_glade_case_evaluates_as_the_cek_machine_evaluates_it() {
    let dir = std::env::temp_dir().join(format!("meadowboot-cek-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let mut files = Vec::new();
    let mut wants = std::collections::HashMap::new();
    for (i, src) in glade_programs().iter().enumerate() {
        let p = match compiled(src) {
            Ok(p) => p,
            Err(e) => {
                if std::env::var_os("MEADOWBOOT_VERBOSE").is_some() {
                    eprintln!("case {i} does not compile: {e}\n{src}\n");
                }
                continue;
            }
        };
        if p.entry.is_none() {
            continue;
        }
        let path = dir.join(format!("case{i:03}.core"));
        std::fs::write(&path, core_text(&p)).expect("a core file");
        wants.insert(path.clone(), cek(&p));
        files.push(path);
    }
    assert!(
        files.len() > 90,
        "only {} glade cases compiled",
        files.len()
    );
    let theirs = per_file("eval", &files);
    let bad = differences(&theirs, |p| format!("{}\n", wants[p]));
    // `MEADOWBOOT_KEEP` keeps the core files, to run MeadowBoot on by hand.
    if std::env::var_os("MEADOWBOOT_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        eprintln!("the core files are in {}", dir.display());
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases evaluate differently:\n\n{}",
        bad.len(),
        files.len(),
        bad.iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}
