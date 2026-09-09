//! Rendering sequent programs, in the notation the papers use.
//!
//! `⟨p | c⟩` for a cut, `mu a.` and `mu~ x.` for the two binders. Worth having
//! as more than a debugging aid: the point of this IR is that control is
//! visible, and it is only visible if it can be read.

use crate::{Consumer, Producer, Program, Statement};
use std::fmt::{self, Write};

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for def in &self.defs {
            writeln!(f, "def {} (ret a{}) =", def.name, def.ret.0)?;
            let mut body = String::new();
            write_stmt(&mut body, &def.body, 1)?;
            f.write_str(&body)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

fn indent(out: &mut String, depth: usize) -> fmt::Result {
    for _ in 0..depth {
        out.push_str("  ");
    }
    Ok(())
}

fn write_stmt(out: &mut String, s: &Statement, depth: usize) -> fmt::Result {
    indent(out, depth)?;
    match s {
        Statement::Cut(p, c) => {
            out.push('<');
            write_producer(out, p, depth)?;
            out.push_str(" | ");
            write_consumer(out, c, depth)?;
            out.push_str(">\n");
            Ok(())
        }
        Statement::Prim {
            prim,
            args,
            out: o,
            next,
        } => {
            write!(out, "let x{} = {prim:?}(", o.0)?;
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_producer(out, a, depth)?;
            }
            out.push_str(")\n");
            write_stmt(out, next, depth)
        }
        Statement::If { cond, then, els } => {
            out.push_str("if ");
            write_producer(out, cond, depth)?;
            out.push('\n');
            write_stmt(out, then, depth + 1)?;
            indent(out, depth)?;
            out.push_str("else\n");
            write_stmt(out, els, depth + 1)
        }
        Statement::Jump(l, args) => {
            write!(out, "jump L{}(", l.0)?;
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_producer(out, a, depth)?;
            }
            out.push_str(")\n");
            Ok(())
        }
        Statement::LetLabel {
            label,
            params,
            body,
            next,
        } => {
            write!(out, "label L{}(", label.0)?;
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write!(out, "x{}", p.0)?;
            }
            out.push_str(") =\n");
            write_stmt(out, body, depth + 1)?;
            write_stmt(out, next, depth)
        }
        Statement::Perform {
            effect,
            op,
            arg,
            ret,
        } => {
            write!(out, "perform {effect}.{op}(")?;
            write_producer(out, arg, depth)?;
            write!(out, ") -> a{}\n", ret.0)?;
            Ok(())
        }
        Statement::Handle {
            body,
            clauses,
            ret,
            out: o,
        } => {
            write!(out, "handle -> a{}\n", o.0)?;
            write_stmt(out, body, depth + 1)?;
            for c in clauses {
                indent(out, depth)?;
                write!(out, "| {}.{} x{} k{} ->\n", c.effect, c.op, c.param.0, c.resume.0)?;
                write_stmt(out, &c.body, depth + 1)?;
            }
            if let Some((x, body)) = ret {
                indent(out, depth)?;
                write!(out, "| return x{} ->\n", x.0)?;
                write_stmt(out, body, depth + 1)?;
            }
            Ok(())
        }
        Statement::Error => {
            out.push_str("<error>\n");
            Ok(())
        }
    }
}

fn write_producer(out: &mut String, p: &Producer, depth: usize) -> fmt::Result {
    match p {
        Producer::Var(v) => write!(out, "x{}", v.0),
        Producer::Lit(l) => write!(out, "{l:?}"),
        Producer::Ctor(name, args) => {
            write!(out, "{name}")?;
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_producer(out, a, depth)?;
                }
                out.push(')');
            }
            Ok(())
        }
        Producer::Tuple(items) => {
            out.push('(');
            for (i, a) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_producer(out, a, depth)?;
            }
            out.push(')');
            Ok(())
        }
        Producer::Lam { param, ret, body } => {
            write!(out, "\\x{} a{}.\n", param.0, ret.0)?;
            write_stmt(out, body, depth + 1)?;
            indent(out, depth)
        }
        Producer::Mu(a, body) => {
            write!(out, "mu a{}.\n", a.0)?;
            write_stmt(out, body, depth + 1)?;
            indent(out, depth)
        }
    }
}

fn write_consumer(out: &mut String, c: &Consumer, depth: usize) -> fmt::Result {
    match c {
        Consumer::Covar(a) => write!(out, "a{}", a.0),
        Consumer::MuTilde(x, body) => {
            write!(out, "mu~ x{}.\n", x.0)?;
            write_stmt(out, body, depth + 1)?;
            indent(out, depth)
        }
        Consumer::Apply(arg, ret) => {
            out.push_str("apply(");
            write_producer(out, arg, depth)?;
            write!(out, ") -> a{}", ret.0)
        }
        Consumer::Case(branches) => {
            out.push_str("case\n");
            for b in branches {
                indent(out, depth + 1)?;
                write!(out, "| {:?} ->\n", b.pat)?;
                write_stmt(out, &b.body, depth + 2)?;
            }
            indent(out, depth)
        }
        Consumer::Finish => out.write_str("finish"),
    }
}
