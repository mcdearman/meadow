//! Runtime values.
//!
//! The same shape as `meadow_eval::Value`, with one difference learned there:
//! every payload that can be *deep* is shared behind an `Rc` from the start. A
//! `List` is a `Cons` chain, so an unshared constructor payload makes walking one
//! quadratic and makes cloning recurse once per element — which in the CEK
//! aborted the process at about two thousand elements before it was fixed.
//!
//! Cloning a `Value` is therefore always O(1) except for tuples and records,
//! which are bounded by their arity.

use crate::stack::Resumption;
use meadow_intern::InternedString;
use num_bigint::BigInt;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    BigInt(Rc<BigInt>),
    Float(f64),
    Bool(bool),
    Str(InternedString),
    Char(char),
    Unit,
    Tuple(Rc<Vec<Value>>),
    Array(Rc<Vec<Value>>),
    Record(Rc<BTreeMap<InternedString, Value>>),
    /// A data constructor and its fields. See [`Fields`] for why the payload is
    /// not a plain `Rc<Vec<Value>>`.
    Ctor(InternedString, Rc<Fields>),
    /// A closure: which function, and what it captured.
    Closure { func: u32, captures: Rc<Vec<Value>> },
    /// A mutable cell — the only value with identity.
    Ref(Rc<std::cell::RefCell<Value>>),
    /// A captured one-shot continuation.
    Cont(Resumption),
}

/// A constructor's fields.
///
/// A newtype purely so [`Drop`] can be implemented on it. Sharing the payload
/// stops *cloning* a `Cons` chain from recursing, but not *dropping* one: the
/// derived glue still descends node by node, and a list of a few thousand
/// overflows the stack. The CEK machine learned this the hard way, in that
/// order, and the fix is the same here.
///
/// It cannot go on `Value` itself — a type that implements `Drop` cannot be
/// destructured by move, which the VM does constantly.
#[derive(Debug, Clone)]
pub struct Fields(Vec<Value>);

impl std::ops::Deref for Fields {
    type Target = Vec<Value>;
    fn deref(&self) -> &Vec<Value> {
        &self.0
    }
}

impl Drop for Fields {
    fn drop(&mut self) {
        // Dismantle with a worklist: move each child out, and if this was the
        // last reference to another node, move its children onto the same list
        // rather than letting the drop nest.
        let mut stack: Vec<Value> = std::mem::take(&mut self.0);
        while let Some(v) = stack.pop() {
            if let Value::Ctor(_, rc) = v
                && let Ok(mut fields) = Rc::try_unwrap(rc)
            {
                stack.append(&mut fields.0);
            }
        }
    }
}

impl Value {
    pub fn ctor(name: impl Into<InternedString>, fields: Vec<Value>) -> Value {
        Value::Ctor(name.into(), Rc::new(Fields(fields)))
    }

    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }

    pub fn bool(b: bool) -> Value {
        Value::Bool(b)
    }

    /// Whether this is the `False` a conditional branches on.
    ///
    /// `Bool` and the `Std.Bool` constructors are both accepted: the compiler
    /// lowers `True`/`False` patterns to `Lit(Bool)`, but a value built by
    /// naming the constructor arrives as a `Ctor`.
    pub fn is_falsey(&self) -> bool {
        match self {
            Value::Bool(b) => !b,
            Value::Ctor(name, fields) => &**name == "False" && fields.is_empty(),
            _ => false,
        }
    }

    /// The fields of a constructor or tuple, for `Op::Field`.
    pub fn fields(&self) -> Option<&[Value]> {
        match self {
            Value::Ctor(_, fs) => Some(fs),
            Value::Tuple(fs) => Some(fs),
            Value::Array(fs) => Some(fs),
            _ => None,
        }
    }

    /// A short tag for introspection — what a stack viewer shows per slot.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::BigInt(_) => "bigint",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Char(_) => "char",
            Value::Unit => "unit",
            Value::Tuple(_) => "tuple",
            Value::Array(_) => "array",
            Value::Record(_) => "record",
            Value::Ctor(..) => "ctor",
            Value::Closure { .. } => "closure",
            Value::Ref(_) => "ref",
            Value::Cont(_) => "cont",
        }
    }
}

/// Structural equality, iteratively — for the same reason the CEK's is
/// iterative: a `Cons` chain is as deep as it is long.
pub fn value_eq(a: &Value, b: &Value) -> bool {
    let mut stack = vec![(a.clone(), b.clone())];
    while let Some((a, b)) = stack.pop() {
        match (&a, &b) {
            (Value::Int(x), Value::Int(y)) if x == y => {}
            (Value::BigInt(x), Value::BigInt(y)) if x == y => {}
            (Value::Float(x), Value::Float(y)) if x == y => {}
            (Value::Bool(x), Value::Bool(y)) if x == y => {}
            (Value::Str(x), Value::Str(y)) if x == y => {}
            (Value::Char(x), Value::Char(y)) if x == y => {}
            (Value::Unit, Value::Unit) => {}
            (Value::Tuple(x), Value::Tuple(y)) | (Value::Array(x), Value::Array(y)) => {
                if Rc::ptr_eq(x, y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().cloned().zip(y.iter().cloned()));
            }
            (Value::Ctor(n1, x), Value::Ctor(n2, y)) => {
                if n1 != n2 {
                    return false;
                }
                if Rc::ptr_eq(x, y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().cloned().zip(y.iter().cloned()));
            }
            (Value::Record(x), Value::Record(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                for (k, v) in x.iter() {
                    match y.get(k) {
                        Some(w) => stack.push((v.clone(), w.clone())),
                        None => return false,
                    }
                }
            }
            (Value::Ref(x), Value::Ref(y)) if Rc::ptr_eq(x, y) => {}
            (Value::Cont(x), Value::Cont(y)) if x.same(y) => {}
            _ => return false,
        }
    }
    true
}

impl std::fmt::Display for Value {
    /// Deliberately the same rendering as the CEK's, so a differential test can
    /// compare two runs by their printed result and see a real difference rather
    /// than a formatting one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::BigInt(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Char(c) => write!(f, "{c:?}"),
            Value::Unit => f.write_str("()"),
            Value::Tuple(items) => {
                f.write_str("(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Array(items) => {
                f.write_str("#[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::Record(map) => {
                f.write_str("{ ")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k} = {v}")?;
                }
                f.write_str(" }")
            }
            Value::Ctor(name, fields) if fields.is_empty() => write!(f, "{name}"),
            Value::Ctor(name, fields) => {
                write!(f, "{name}(")?;
                for (i, v) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Closure { .. } => f.write_str("<closure>"),
            Value::Ref(cell) => write!(f, "ref {}", cell.borrow()),
            Value::Cont(_) => f.write_str("<continuation>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equality_is_structural_except_for_places() {
        assert!(value_eq(&Value::Int(1), &Value::Int(1)));
        assert!(!value_eq(&Value::Int(1), &Value::Int(2)));
        assert!(value_eq(
            &Value::ctor("Just", vec![Value::Int(1)]),
            &Value::ctor("Just", vec![Value::Int(1)])
        ));
        assert!(!value_eq(
            &Value::ctor("Just", vec![Value::Int(1)]),
            &Value::ctor("None", vec![])
        ));
    }

    #[test]
    fn a_cell_is_equal_only_to_itself() {
        let a = Value::Ref(Rc::new(std::cell::RefCell::new(Value::Int(1))));
        let b = Value::Ref(Rc::new(std::cell::RefCell::new(Value::Int(1))));
        assert!(value_eq(&a, &a.clone()));
        assert!(!value_eq(&a, &b), "same contents, different cells");
    }

    #[test]
    fn comparing_a_long_chain_does_not_recurse() {
        // 200_000 deep: this is the shape that aborted the CEK before its
        // equality was made iterative.
        let mut xs = Value::ctor("Nil", vec![]);
        for i in 0..200_000 {
            xs = Value::ctor("Cons", vec![Value::Int(i), xs]);
        }
        assert!(value_eq(&xs, &xs.clone()));
    }

    #[test]
    fn falsiness_accepts_both_spellings_of_false() {
        assert!(Value::Bool(false).is_falsey());
        assert!(Value::ctor("False", vec![]).is_falsey());
        assert!(!Value::Bool(true).is_falsey());
        assert!(!Value::ctor("True", vec![]).is_falsey());
        assert!(!Value::Int(0).is_falsey(), "zero is not false");
    }
}
