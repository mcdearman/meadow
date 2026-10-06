//! A readable rendering of an AxCut program.
//!
//! Statements chain rather than nest — `let`, `new` and a single-continuation
//! `extern` are written as one line followed by what comes next at the same
//! indentation — so a long straight-line function reads as a list instead of a
//! staircase. Only the things that genuinely branch (`switch`, a two-way
//! `extern`) and the block a `substitute` enters are indented.
//!
//! Blocks show their parameters, and those parameters are the *whole
//! environment* at that point, not just what is new. Reading a dump is
//! therefore also reading the register assignment, which is the property the IR
//! exists to have.

use crate::{Block, Extern, Label, Name, Program, Statement};
use std::fmt::Write;

/// A program's text, and where in it each definition and each name is
/// written: for a tool that shows a program beside what it was lowered from,
/// which knows what each label and name came from.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub text: String,
    /// Bytes `start..end` are the definition labelled so, whole.
    pub defs: Vec<(usize, usize, Label)>,
    /// Bytes `start..end` are a mention of the name, or where it is bound.
    pub names: Vec<(usize, usize, Name)>,
}

impl Write for Listing {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.text.push_str(s);
        Ok(())
    }
}

impl Listing {
    fn name(&mut self, n: Name) {
        let start = self.text.len();
        let _ = write!(self.text, "v{}", n.0);
        self.names.push((start, self.text.len(), n));
    }

    fn names(&mut self, ns: &[Name]) {
        for (i, n) in ns.iter().enumerate() {
            if i > 0 {
                self.text.push_str(", ");
            }
            self.name(*n);
        }
    }

    fn params(&mut self, ns: &[Name]) {
        self.text.push('(');
        self.names(ns);
        self.text.push(')');
    }

    fn pad(&mut self, depth: usize) {
        for _ in 0..depth {
            self.text.push_str("  ");
        }
    }
}

impl Program {
    pub fn pretty(&self) -> String {
        self.listing().text
    }

    /// [`Program::pretty`]'s text, and where its definitions and names are.
    pub fn listing(&self) -> Listing {
        let mut out = Listing::default();
        for def in &self.defs {
            let start = out.text.len();
            let _ = write!(out, "def {} ", label(def.label));
            out.params(&def.block.params);
            if Some(def.label) == self.entry {
                out.text.push_str("  (entry)");
            }
            out.text.push('\n');
            stmt(&mut out, &def.block.body, 1);
            out.defs.push((start, out.text.len(), def.label));
            out.text.push('\n');
        }
        out
    }
}

fn label(Label(n): Label) -> String {
    format!("#{n}")
}

fn place(p: &Option<crate::Place>) -> String {
    match p {
        Some(p) => format!(" @{}/{}", p.at, p.of),
        None => String::new(),
    }
}

fn stmt(out: &mut Listing, s: &Statement, depth: usize) {
    out.pad(depth);
    match s {
        Statement::Substitute(sel, block) => {
            out.text.push_str("substitute [");
            out.names(sel);
            out.text.push_str("] in ");
            out.params(&block.params);
            out.text.push('\n');
            stmt(out, &block.body, depth + 1);
        }
        Statement::Jump(l) => {
            let _ = writeln!(out, "jump {}", label(*l));
        }
        Statement::Let {
            name: n,
            tag,
            ctor,
            fields,
            rest,
        } => {
            out.text.push_str("let ");
            out.name(*n);
            let _ = write!(out, " = {ctor}#{tag}(");
            out.names(fields);
            out.text.push_str(");\n");
            stmt(out, rest, depth);
        }
        Statement::Switch {
            scrutinee,
            arms,
            default,
        } => {
            out.text.push_str("switch ");
            out.name(*scrutinee);
            out.text.push_str(" {\n");
            for (tag, b) in arms {
                out.pad(depth + 1);
                let _ = write!(out, "#{tag} ");
                out.params(&b.params);
                out.text.push_str(" =>\n");
                stmt(out, &b.body, depth + 2);
            }
            out.pad(depth + 1);
            out.text.push_str("else ");
            out.params(&default.params);
            out.text.push_str(" =>\n");
            stmt(out, &default.body, depth + 2);
            out.pad(depth);
            out.text.push_str("}\n");
        }
        Statement::New {
            name: n,
            captures,
            methods,
            rest,
        } => {
            out.text.push_str("new ");
            out.name(*n);
            out.text.push_str(" [");
            out.names(captures);
            out.text.push_str("] {\n");
            for (tag, m) in methods.iter().enumerate() {
                out.pad(depth + 1);
                let _ = write!(out, "#{tag} ");
                out.params(&m.params);
                out.text.push_str(" =>\n");
                stmt(out, &m.body, depth + 2);
            }
            out.pad(depth);
            out.text.push_str("};\n");
            stmt(out, rest, depth);
        }
        Statement::Invoke(n, tag) => {
            out.text.push_str("invoke ");
            out.name(*n);
            let _ = writeln!(out, "#{tag}");
        }
        Statement::Extern { op, args, blocks } => {
            let op = match op {
                Extern::Lit(l) => format!("lit {l:?}"),
                Extern::Prim(p) => format!("{p:?}"),
                Extern::PrimK(p, l) => format!("{p:?} .. {l:?}"),
                Extern::Branch => "branch".to_string(),
                Extern::BranchPrim(p) => format!("branch {p:?}"),
                Extern::BranchPrimK(p, l) => format!("branch {p:?} .. {l:?}"),
                Extern::Record(labels) => format!(
                    "record{{{}}}",
                    labels
                        .iter()
                        .map(|l| l.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Extern::Select(l, p) => format!("select .{l}{}", place(p)),
                Extern::Extend(l, p) => format!("extend .{l}{}", place(p)),
                Extern::Array => "array".to_string(),
                Extern::Field(i) => format!("field {i}"),
                Extern::Native(e, o) => format!("native {e}.{o}"),
            };
            let _ = write!(out, "extern {op}(");
            out.names(args);
            // One continuation is a sequence point, not a branch: write it flat.
            if let [only] = &blocks[..] {
                out.text.push_str(") -> ");
                out.params(&only.params);
                out.text.push_str(";\n");
                stmt(out, &only.body, depth);
            } else {
                out.text.push_str(") {\n");
                for (i, b) in blocks.iter().enumerate() {
                    out.pad(depth + 1);
                    let _ = write!(out, "#{i} ");
                    out.params(&b.params);
                    out.text.push_str(" =>\n");
                    stmt(out, &b.body, depth + 2);
                }
                out.pad(depth);
                out.text.push_str("}\n");
            }
        }
        Statement::Error(msg) => {
            let _ = writeln!(out, "error {msg:?}");
        }
        Statement::Mark(loc, inner) => {
            let _ = writeln!(out, "-- at {}..{}", loc.span.start, loc.span.end);
            stmt(out, inner, depth);
        }
    }
}

impl std::fmt::Display for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = Listing::default();
        out.params(&self.params);
        out.text.push_str(" =>\n");
        stmt(&mut out, &self.body, 1);
        f.write_str(&out.text)
    }
}
