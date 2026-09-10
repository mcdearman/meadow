//! Printing and structural equality.
//!
//! Both have to agree with `meadow_eval` exactly. Printing, because `show` is a
//! primitive and a program can therefore observe it, and because the differential
//! tests compare rendered results. Equality, because `==` is structural at every
//! type and is one of the most used operations in the standard library.
//!
//! Two library types get special treatment, for the same reasons they do in the
//! CEK machine:
//!
//! * a **`List`** is a `Cons` chain, and prints as `[1; 2; 3]`;
//! * a **`Vector`** is a balanced tree, so two equal ones need not have equal
//!   shapes. Both printing and equality flatten it to the sequence it denotes.
//!
//! Everything here walks the heap iteratively where the structure can be deep,
//! which is the same reason the CEK's versions are iterative — a list is as deep
//! as it is long.

use crate::heap::Kind;
use crate::value::Value;
use crate::vm::Vm;

use num_bigint::{BigInt, Sign};
use std::fmt::Write;

fn is_vector_ctor(name: &str) -> bool {
    matches!(name, "VEmpty" | "VSingle" | "VFull")
}

impl Vm<'_> {
    /// The tag a constructor has in this program, for the primitives that build
    /// one. A name the program never mentions gets a tag no `switch` arm has,
    /// which is the same thing the AxCut machine does.
    pub(crate) fn ctor_tag(&self, name: &str) -> u32 {
        self.program
            .ctors
            .iter()
            .position(|c| &**c == name)
            .map(|i| i as u32)
            .unwrap_or(u32::MAX)
    }

    /// A `Cons` chain as its elements, or `None` if it is not one.
    pub(crate) fn list_items(&self, v: Value) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        let mut cur = v;
        loop {
            let a = cur.addr()?;
            if self.heap.kind(a) != Kind::Data {
                return None;
            }
            let name = self.program.ctor(self.heap.meta(a))?;
            match (&*name, self.heap.len(a)) {
                ("Nil", 0) => return Some(out),
                ("Cons", 2) => {
                    out.push(self.heap.field(a, 0));
                    cur = self.heap.field(a, 1);
                }
                _ => return None,
            }
        }
    }

    /// A `Vector` flattened to its elements, in order.
    pub(crate) fn vector_elems(&self, v: Value) -> Option<Vec<Value>> {
        let a = v.addr()?;
        if self.heap.kind(a) != Kind::Data {
            return None;
        }
        let name = self.program.ctor(self.heap.meta(a))?;
        let n = self.heap.len(a);
        match (&*name, n) {
            ("VEmpty", 0) => Some(Vec::new()),
            ("VSingle", 1) => self.array_elems(self.heap.field(a, 0)),
            ("VFull", 7) => {
                let mut out = Vec::new();
                for i in [2usize, 3] {
                    out.extend(self.array_elems(self.heap.field(a, i))?);
                }
                self.vector_node(self.heap.field(a, 4), &mut out)?;
                for i in [5usize, 6] {
                    out.extend(self.array_elems(self.heap.field(a, i))?);
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn vector_node(&self, v: Value, out: &mut Vec<Value>) -> Option<()> {
        let a = v.addr()?;
        if self.heap.kind(a) != Kind::Data {
            return None;
        }
        let name = self.program.ctor(self.heap.meta(a))?;
        match (&*name, self.heap.len(a)) {
            ("VLeaf", 1) => {
                out.extend(self.array_elems(self.heap.field(a, 0))?);
                Some(())
            }
            ("VBranch", 2) => {
                for kid in self.array_elems(self.heap.field(a, 1))? {
                    self.vector_node(kid, out)?;
                }
                Some(())
            }
            _ => None,
        }
    }

    fn array_elems(&self, v: Value) -> Option<Vec<Value>> {
        let a = v.addr()?;
        if self.heap.kind(a) != Kind::Array {
            return None;
        }
        Some(self.heap.fields(a))
    }

    pub(crate) fn bigint_at(&self, v: Value) -> Option<BigInt> {
        let a = v.addr()?;
        if self.heap.kind(a) != Kind::BigInt {
            return None;
        }
        let sign = match self.heap.meta(a) {
            0 => Sign::NoSign,
            1 => Sign::Plus,
            _ => Sign::Minus,
        };
        let digits: Vec<u32> = (0..self.heap.len(a))
            .map(|i| match self.heap.field(a, i) {
                Value::Int(n) => n as u32,
                _ => 0,
            })
            .collect();
        Some(BigInt::from_slice(sign, &digits))
    }

    /// How `print` and `println` render a value: a `String` as its text,
    /// everything else as [`Vm::show`] gives it.
    ///
    /// `println "hello"` used to write `"hello"`, quotes and all, because these
    /// are polymorphic and fell through to the display used for data.
    pub fn displayed(&self, v: Value) -> String {
        match v {
            Value::Str(s) => s.to_string(),
            other => self.show(other),
        }
    }

    /// Render a value the way the REPL prints it.
    pub fn show(&self, v: Value) -> String {
        let mut out = String::new();
        self.render(&mut out, v);
        out
    }

    fn render(&self, out: &mut String, v: Value) {
        match v {
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(x) => out.push_str(&meadow_core::fmt_float(x)),
            Value::Bool(b) => {
                let _ = write!(out, "{b}");
            }
            Value::Str(s) => {
                let _ = write!(out, "{:?}", &*s);
            }
            Value::Char(c) => {
                let _ = write!(out, "{c:?}");
            }
            Value::Unit => out.push_str("()"),
            Value::Obj(a) => match self.heap.kind(a) {
                Kind::BigInt => {
                    let _ = write!(out, "{}", self.bigint_at(v).expect("a bigint"));
                }
                Kind::Array => {
                    out.push_str("#[");
                    self.join(out, &self.heap.fields(a), ", ");
                    out.push(']');
                }
                Kind::Record => {
                    out.push_str("{ ");
                    for j in 0..self.heap.len(a) / 2 {
                        if j > 0 {
                            out.push_str(", ");
                        }
                        if let Value::Str(l) = self.heap.field(a, 2 * j) {
                            let _ = write!(out, "{l} = ");
                        }
                        self.render(out, self.heap.field(a, 2 * j + 1));
                    }
                    out.push_str(" }");
                }
                // The contents, not the address: what a person debugging one
                // wants to see.
                Kind::Ref => {
                    out.push_str("ref ");
                    self.render(out, self.heap.field(a, 0));
                }
                Kind::Closure => out.push_str("<closure>"),
                Kind::Resume => out.push_str("<continuation>"),
                Kind::Data => self.render_data(out, v, a),
            },
        }
    }

    fn render_data(&self, out: &mut String, v: Value, a: crate::value::Addr) {
        let name = match self.program.ctor(self.heap.meta(a)) {
            Some(n) => n,
            None => {
                let _ = write!(out, "#{}(..)", self.heap.meta(a));
                return;
            }
        };
        match &*name {
            "#tuple" => {
                out.push('(');
                self.join(out, &self.heap.fields(a), ", ");
                out.push(')');
            }
            "Nil" | "Cons" => match self.list_items(v) {
                Some(xs) => {
                    out.push('[');
                    self.join(out, &xs, "; ");
                    out.push(']');
                }
                None => {
                    let _ = write!(out, "{name}(..)");
                }
            },
            n if is_vector_ctor(n) => match self.vector_elems(v) {
                Some(xs) => {
                    out.push('[');
                    self.join(out, &xs, ", ");
                    out.push(']');
                }
                None => {
                    let _ = write!(out, "{name}(..)");
                }
            },
            _ if self.heap.len(a) == 0 => {
                let _ = write!(out, "{name}");
            }
            _ => {
                let _ = write!(out, "{name}(");
                self.join(out, &self.heap.fields(a), ", ");
                out.push(')');
            }
        }
    }

    fn join(&self, out: &mut String, vs: &[Value], sep: &str) {
        for (i, v) in vs.iter().enumerate() {
            if i > 0 {
                out.push_str(sep);
            }
            self.render(out, *v);
        }
    }

    /// Structural equality.
    ///
    /// Iterative, because a `Cons` chain is as deep as it is long. The address
    /// check at the top is what makes `xs == xs` on a long list O(1) rather than
    /// O(n) — the same short-circuit the CEK gets from `Rc::ptr_eq`, and here it
    /// is simply integer equality.
    pub fn value_eq(&self, a: Value, b: Value) -> bool {
        let mut stack = vec![(a, b)];
        while let Some((a, b)) = stack.pop() {
            match (a, b) {
                (Value::Int(x), Value::Int(y)) if x == y => {}
                (Value::Float(x), Value::Float(y)) if x == y => {}
                (Value::Bool(x), Value::Bool(y)) if x == y => {}
                (Value::Char(x), Value::Char(y)) if x == y => {}
                (Value::Str(x), Value::Str(y)) if x == y => {}
                (Value::Unit, Value::Unit) => {}
                (Value::Obj(x), Value::Obj(y)) => {
                    if x == y {
                        continue;
                    }
                    let (kx, ky) = (self.heap.kind(x), self.heap.kind(y));
                    match (kx, ky) {
                        (Kind::Data, Kind::Data) => {
                            let nx = self.program.ctor(self.heap.meta(x));
                            let ny = self.program.ctor(self.heap.meta(y));
                            // A `Vector` is a tree, so compare the sequences the
                            // two denote rather than their shapes.
                            let vectors = matches!((nx, ny), (Some(nx), Some(ny))
                                if is_vector_ctor(&nx) && is_vector_ctor(&ny));
                            if vectors {
                                match (self.vector_elems(a), self.vector_elems(b)) {
                                    (Some(xs), Some(ys)) if xs.len() == ys.len() => {
                                        stack.extend(xs.into_iter().zip(ys));
                                        continue;
                                    }
                                    _ => return false,
                                }
                            }
                            if self.heap.meta(x) != self.heap.meta(y)
                                || self.heap.len(x) != self.heap.len(y)
                            {
                                return false;
                            }
                            for i in 0..self.heap.len(x) {
                                stack.push((self.heap.field(x, i), self.heap.field(y, i)));
                            }
                        }
                        (Kind::Array, Kind::Array) => {
                            if self.heap.len(x) != self.heap.len(y) {
                                return false;
                            }
                            for i in 0..self.heap.len(x) {
                                stack.push((self.heap.field(x, i), self.heap.field(y, i)));
                            }
                        }
                        (Kind::Record, Kind::Record) => {
                            if self.heap.len(x) != self.heap.len(y) {
                                return false;
                            }
                            for j in 0..self.heap.len(x) / 2 {
                                let Value::Str(label) = self.heap.field(x, 2 * j) else {
                                    return false;
                                };
                                match self.record_get(y, label) {
                                    Some(w) => stack.push((self.heap.field(x, 2 * j + 1), w)),
                                    None => return false,
                                }
                            }
                        }
                        (Kind::BigInt, Kind::BigInt) => {
                            if self.bigint_at(a) != self.bigint_at(b) {
                                return false;
                            }
                        }
                        // A `Ref` is a place, and two cells holding the same
                        // thing are still two cells. The address check above is
                        // the only way one compares equal.
                        _ => return false,
                    }
                }
                _ => return false,
            }
        }
        true
    }
}
