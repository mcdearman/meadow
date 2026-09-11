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

impl Program {
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        for def in &self.defs {
            let entry = if Some(def.label) == self.entry {
                "  (entry)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "def {} {}{entry}",
                label(def.label),
                params(&def.block.params)
            );
            stmt(&mut out, &def.block.body, 1);
            out.push('\n');
        }
        out
    }
}

fn label(Label(n): Label) -> String {
    format!("#{n}")
}

fn name(n: Name) -> String {
    format!("v{}", n.0)
}

fn names(ns: &[Name]) -> String {
    ns.iter().map(|n| name(*n)).collect::<Vec<_>>().join(", ")
}

fn params(ns: &[Name]) -> String {
    format!("({})", names(ns))
}

fn pad(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn stmt(out: &mut String, s: &Statement, depth: usize) {
    pad(out, depth);
    match s {
        Statement::Substitute(sel, block) => {
            let _ = writeln!(out, "substitute [{}] in {}", names(sel), params(&block.params));
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
            let _ = writeln!(
                out,
                "let {} = {ctor}#{tag}({});",
                name(*n),
                names(fields)
            );
            stmt(out, rest, depth);
        }
        Statement::Switch {
            scrutinee,
            arms,
            default,
        } => {
            let _ = writeln!(out, "switch {} {{", name(*scrutinee));
            for (tag, b) in arms {
                pad(out, depth + 1);
                let _ = writeln!(out, "#{tag} {} =>", params(&b.params));
                stmt(out, &b.body, depth + 2);
            }
            pad(out, depth + 1);
            let _ = writeln!(out, "else {} =>", params(&default.params));
            stmt(out, &default.body, depth + 2);
            pad(out, depth);
            out.push_str("}\n");
        }
        Statement::New {
            name: n,
            captures,
            methods,
            rest,
        } => {
            let _ = writeln!(out, "new {} [{}] {{", name(*n), names(captures));
            for (tag, m) in methods.iter().enumerate() {
                pad(out, depth + 1);
                let _ = writeln!(out, "#{tag} {} =>", params(&m.params));
                stmt(out, &m.body, depth + 2);
            }
            pad(out, depth);
            out.push_str("};\n");
            stmt(out, rest, depth);
        }
        Statement::Invoke(n, tag) => {
            let _ = writeln!(out, "invoke {}#{tag}", name(*n));
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
                Extern::Select(l) => format!("select .{l}"),
                Extern::Extend(l) => format!("extend .{l}"),
                Extern::Array => "array".to_string(),
                Extern::Field(i) => format!("field {i}"),
            };
            // One continuation is a sequence point, not a branch: write it flat.
            if let [only] = &blocks[..] {
                let _ = writeln!(out, "extern {op}({}) -> {};", names(args), params(&only.params));
                stmt(out, &only.body, depth);
            } else {
                let _ = writeln!(out, "extern {op}({}) {{", names(args));
                for (i, b) in blocks.iter().enumerate() {
                    pad(out, depth + 1);
                    let _ = writeln!(out, "#{i} {} =>", params(&b.params));
                    stmt(out, &b.body, depth + 2);
                }
                pad(out, depth);
                out.push_str("}\n");
            }
        }
        Statement::Handle {
            handler,
            ops,
            k,
            rest,
        } => {
            let ops = ops
                .iter()
                .map(|(e, o)| format!("{e}.{o}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "handle {} [{ops}] answering {};",
                name(*handler),
                name(*k)
            );
            stmt(out, rest, depth);
        }
        Statement::Unhandle { k, rest } => {
            let _ = writeln!(out, "unhandle -> {};", name(*k));
            stmt(out, rest, depth);
        }
        Statement::Perform {
            effect,
            op,
            arg,
            k,
        } => {
            let _ = writeln!(
                out,
                "perform {effect}.{op}({}) -> {}",
                name(*arg),
                name(*k)
            );
        }
        Statement::Error(msg) => {
            let _ = writeln!(out, "error {msg:?}");
        }
    }
}

impl std::fmt::Display for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        let _ = writeln!(out, "{} =>", params(&self.params));
        stmt(&mut out, &self.body, 1);
        f.write_str(&out)
    }
}
