//! Cut as text: what [`crate::parse`] reads, written. A program printed and
//! read back is the program it was.

use crate::*;
use std::fmt::Write;

impl fmt::Display for Rep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rep::I64 => "i64",
            Rep::F64 => "f64",
            Rep::F32 => "f32",
            Rep::I8 => "i8",
            Rep::I16 => "i16",
            Rep::I32 => "i32",
            Rep::U8 => "u8",
            Rep::U16 => "u16",
            Rep::U32 => "u32",
            Rep::U64 => "u64",
            Rep::Bool => "bool",
            Rep::Char => "char",
            Rep::Unit => "unit",
            Rep::Sym => "sym",
            Rep::Str => "str",
            Rep::Ptr => "ptr",
            Rep::Desc => "desc",
            Rep::Any => "any",
            Rep::Var(a) => return write!(f, "'{a}"),
        })
    }
}

impl fmt::Display for Binder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.rep)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Effect => "Effect",
            Kind::ManyEffect => "ManyEffect",
        })
    }
}

fn commas<T: fmt::Display>(xs: &[T]) -> String {
    xs.iter().map(T::to_string).collect::<Vec<_>>().join(", ")
}

/// `(values; continuations)`, every continuation written as the `ptr` it is.
fn params(values: &[Binder], conts: &[String]) -> String {
    let conts: Vec<String> = conts.iter().map(|k| format!("{k}: ptr")).collect();
    format!("({}; {})", commas(values), conts.join(", "))
}

/// A string literal, escaped as the parser reads one.
pub fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn character(c: char) -> String {
    match c {
        '\'' => "'\\''".to_string(),
        '\\' => "'\\\\'".to_string(),
        '\n' => "'\\n'".to_string(),
        '\t' => "'\\t'".to_string(),
        '\r' => "'\\r'".to_string(),
        '\0' => "'\\0'".to_string(),
        c if c.is_control() => format!("'\\u{{{:x}}}'", c as u32),
        c => format!("'{c}'"),
    }
}

/// A float as a literal that reads back as itself: always with a point.
fn float(x: f64) -> String {
    let s = format!("{x:?}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

struct Printer {
    out: String,
}

impl Printer {
    fn line(&mut self, indent: usize) {
        self.out.push('\n');
        self.out.push_str(&"  ".repeat(indent));
    }

    /// A statement on a line of its own, `indent` levels in.
    fn nested(&mut self, s: &Statement, indent: usize) {
        self.line(indent);
        self.statement(s, indent);
    }

    fn list(&mut self, ps: &[Producer], indent: usize) {
        for (i, p) in ps.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.producer(p, indent);
        }
    }

    /// `(p, ..; c, ..)`
    fn call(&mut self, args: &[Producer], conts: &[Consumer], indent: usize) {
        self.out.push('(');
        self.list(args, indent);
        self.out.push_str("; ");
        for (i, c) in conts.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.consumer(c, indent);
        }
        self.out.push(')');
    }

    fn producer(&mut self, p: &Producer, indent: usize) {
        match p {
            Producer::Var(x) => self.out.push_str(x),
            Producer::Val(s) => {
                let _ = write!(self.out, "{s}");
            }
            Producer::Int(n) => {
                let _ = write!(self.out, "{n}");
            }
            Producer::Float(x) => self.out.push_str(&float(*x)),
            Producer::Char(c) => self.out.push_str(&character(*c)),
            Producer::Str(s) => self.out.push_str(&string(s)),
            Producer::Bool(b) => {
                let _ = write!(self.out, "{b}");
            }
            Producer::Unit => self.out.push_str("unit"),
            Producer::Desc(r) => {
                let _ = write!(self.out, "desc({r})");
            }
            Producer::Con(k, args) => {
                let _ = write!(self.out, "{k}");
                if !args.is_empty() {
                    self.out.push('(');
                    self.list(args, indent);
                    self.out.push(')');
                }
            }
            Producer::Tuple(args) => {
                self.out.push_str("#tuple(");
                self.list(args, indent);
                self.out.push(')');
            }
            Producer::Array(args) => {
                self.out.push('[');
                self.list(args, indent);
                self.out.push(']');
            }
            Producer::Record(fields) => {
                self.out.push_str("record { ");
                for (i, (l, p)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    let _ = write!(self.out, "{l} = ");
                    self.producer(p, indent);
                }
                self.out.push_str(" }");
            }
            Producer::Mu(k, s) => {
                let _ = write!(self.out, "μ {k}.");
                self.nested(s, indent + 1);
            }
            Producer::Cocase(methods) => {
                self.out.push_str("cocase {");
                for (i, m) in methods.iter().enumerate() {
                    if i > 0 {
                        self.out.push(';');
                    }
                    self.line(indent + 1);
                    let _ = write!(self.out, "{}{} =>", m.name, params(&m.params, &m.conts));
                    self.nested(&m.body, indent + 2);
                }
                self.line(indent);
                self.out.push('}');
            }
        }
    }

    fn consumer(&mut self, c: &Consumer, indent: usize) {
        match c {
            Consumer::Var(k) => self.out.push_str(k),
            Consumer::Halt => self.out.push_str("halt"),
            Consumer::MuTilde(x, s) => {
                let _ = write!(self.out, "μ̃ {x}.");
                self.nested(s, indent + 1);
            }
            Consumer::Case(arms) => {
                self.out.push_str("case {");
                for (i, arm) in arms.iter().enumerate() {
                    if i > 0 {
                        self.out.push(';');
                    }
                    self.line(indent + 1);
                    match &arm.pattern {
                        Pattern::Con(k) => {
                            let _ = write!(self.out, "{k}");
                        }
                        Pattern::Tuple => self.out.push_str("#tuple"),
                        Pattern::Default => self.out.push('_'),
                    }
                    if !arm.fields.is_empty() || arm.pattern == Pattern::Tuple {
                        let _ = write!(self.out, "({})", commas(&arm.fields));
                    }
                    self.out.push_str(" =>");
                    self.nested(&arm.body, indent + 2);
                }
                self.line(indent);
                self.out.push('}');
            }
            Consumer::Method(m, args, conts) => {
                self.out.push_str(m);
                self.call(args, conts, indent);
            }
        }
    }

    fn statement(&mut self, s: &Statement, indent: usize) {
        match s {
            Statement::Cut(p, c) => {
                self.out.push('<');
                self.producer(p, indent);
                self.out.push_str(" | ");
                self.consumer(c, indent);
                self.out.push('>');
            }
            Statement::Call(f, args, conts) => {
                let _ = write!(self.out, "{f}");
                self.call(args, conts, indent);
            }
            Statement::Prim(op, args, conts) => {
                let _ = write!(self.out, "prim {op}");
                self.call(args, conts, indent);
            }
            Statement::Let(x, p, body) => {
                let _ = write!(self.out, "let {x} = ");
                self.producer(p, indent);
                self.out.push_str(" in");
                self.nested(body, indent);
            }
            Statement::Perform(op, args, c) => {
                let _ = write!(self.out, "perform {op}");
                self.call(args, std::slice::from_ref(c), indent);
            }
            Statement::Error(msg) => {
                let _ = write!(self.out, "error {}", string(msg));
            }
            Statement::Handle(h) => {
                self.out.push_str("handle {");
                for c in &h.clauses {
                    self.line(indent + 1);
                    let conts = [c.resumption.clone(), c.cont.clone()];
                    let _ = write!(self.out, "{}{} =>", c.op, params(&c.params, &conts));
                    self.nested(&c.body, indent + 2);
                    self.out.push(';');
                }
                let (x, k, body) = &h.ret;
                self.line(indent + 1);
                let _ = write!(
                    self.out,
                    "return{} =>",
                    params(std::slice::from_ref(x), std::slice::from_ref(k))
                );
                self.nested(body, indent + 2);
                self.line(indent);
                let _ = write!(self.out, "}} in μ {}.", h.body_cont);
                self.nested(&h.body, indent + 1);
                self.line(indent);
                self.out.push_str("; ");
                self.consumer(&h.cont, indent);
            }
        }
    }
}

/// A program as the text [`crate::parse`] reads.
pub fn program(p: &Program) -> String {
    let mut out = format!("cut {}\n", p.version);
    if let Some(entry) = &p.entry {
        let _ = writeln!(out, "entry {entry}");
        let answer = match p.answer {
            Answer::None => "none",
            Answer::Str => "str",
        };
        let _ = writeln!(out, "answer {answer}");
    }
    for d in &p.datas {
        let vars = if d.rep_vars.is_empty() {
            String::new()
        } else {
            let vs: Vec<String> = d.rep_vars.iter().map(|a| format!("'{a}")).collect();
            format!(" <{}>", vs.join(", "))
        };
        let ctors: Vec<String> = d
            .ctors
            .iter()
            .map(|(k, fields)| {
                if fields.is_empty() {
                    segment(k)
                } else {
                    format!("{}({})", segment(k), commas(fields))
                }
            })
            .collect();
        let _ = writeln!(out, "\ndata {}{vars} {{ {} }}", d.symbol, ctors.join("; "));
    }
    if !p.roles.is_empty() {
        out.push_str("\nroles {\n");
        for (role, k) in &p.roles {
            let _ = writeln!(out, "  {role} = {k}");
        }
        out.push_str("}\n");
    }
    if !p.natives.is_empty() {
        out.push_str("\nnative {\n");
        for (op, runtime) in &p.natives {
            let _ = writeln!(out, "  {op} = {runtime}");
        }
        out.push_str("}\n");
    }
    for e in &p.effects {
        let ops: Vec<String> = e
            .ops
            .iter()
            .map(|op| {
                let many = if op.many { "@many " } else { "" };
                format!(
                    "{many}{}({}) -> {}",
                    segment(&op.name),
                    commas(&op.params),
                    op.result
                )
            })
            .collect();
        let _ = writeln!(out, "\neffect {} {{ {} }}", e.symbol, ops.join("; "));
    }
    for v in &p.vals {
        let mut pr = Printer { out: String::new() };
        pr.nested(&v.body, 1);
        let _ = writeln!(out, "\nval {} : {} ={}", v.symbol, v.rep, pr.out);
    }
    for d in &p.defs {
        let mut head = Vec::new();
        if !d.rep_vars.is_empty() {
            let vs: Vec<String> = d
                .rep_vars
                .iter()
                .map(|(a, desc)| format!("'{a} = {desc}"))
                .collect();
            head.push(vs.join(", "));
        }
        if !d.effect_vars.is_empty() {
            let es: Vec<String> = d
                .effect_vars
                .iter()
                .map(|(e, k)| format!("{e}: {k}"))
                .collect();
            head.push(es.join(", "));
        }
        let generics = if head.is_empty() {
            String::new()
        } else {
            format!(" <{}>", head.join("; "))
        };
        let mut pr = Printer { out: String::new() };
        pr.nested(&d.body, 1);
        let _ = writeln!(
            out,
            "\ndef {}{generics} {} ={}",
            d.symbol,
            params(&d.params, &d.conts),
            pr.out
        );
    }
    out
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&program(self))
    }
}
