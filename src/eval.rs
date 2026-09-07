//! A straightforward call-by-value tree-walking interpreter for [`crate::core`].
//!
//! This is the reference semantics: no optimizations, no bytecode. Environments
//! are `Rc`-linked frames; `LetRec` creates a frame the closures close over before
//! it is filled, which is enough for recursive functions (recursive *values* that
//! force each other are not supported).

use crate::{core, intern::InternedString};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(InternedString),
    Unit,
    Tuple(Vec<Value>),
    List(Vec<Value>),
    Record(BTreeMap<InternedString, Value>),
    Ctor(InternedString, Vec<Value>),
    Closure {
        param: core::Var,
        body: Rc<core::Term>,
        env: Env,
    },
    /// A partially applied primitive.
    Builtin {
        op: core::Prim,
        args: Vec<Value>,
    },
}

#[derive(Debug)]
pub struct RuntimeError {
    pub msg: String,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime error: {}", self.msg)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, RuntimeError> {
    Err(RuntimeError { msg: msg.into() })
}

// --- environments ----------------------------------------------------------

#[derive(Debug)]
pub struct Frame {
    slots: RefCell<Vec<(core::Var, Value)>>,
    parent: Option<Env>,
}

pub type Env = Rc<Frame>;

fn root_env() -> Env {
    Rc::new(Frame {
        slots: RefCell::new(Vec::new()),
        parent: None,
    })
}

fn child(parent: &Env) -> Env {
    Rc::new(Frame {
        slots: RefCell::new(Vec::new()),
        parent: Some(parent.clone()),
    })
}

fn define(env: &Env, var: core::Var, val: Value) {
    env.slots.borrow_mut().push((var, val));
}

fn lookup(env: &Env, var: core::Var) -> Option<Value> {
    let mut cur = Some(env.clone());
    while let Some(frame) = cur {
        if let Some((_, v)) = frame.slots.borrow().iter().rev().find(|(k, _)| *k == var) {
            return Some(v.clone());
        }
        cur = frame.parent.clone();
    }
    None
}

// --- driver --------------------------------------------------------------

pub type FieldTable = std::collections::HashMap<InternedString, Vec<InternedString>>;

pub fn run(program: &core::Program) -> Result<Value, RuntimeError> {
    let interp = Interp {
        ctor_fields: &program.ctor_fields,
    };
    let env = root_env();
    // Placeholders first so recursive references resolve; then evaluate in order.
    for def in &program.defs {
        define(&env, def.var, Value::Unit);
    }
    for def in &program.defs {
        let v = interp.eval(&def.term, &env)?;
        define(&env, def.var, v);
    }
    match program.entry {
        Some(entry) => lookup(&env, entry).ok_or_else(|| RuntimeError {
            msg: "entry point not found".into(),
        }),
        None => Ok(Value::Unit),
    }
}

// --- evaluation --------------------------------------------------------------

struct Interp<'a> {
    ctor_fields: &'a FieldTable,
}

impl Interp<'_> {
    fn eval(&self, term: &core::Term, env: &Env) -> Result<Value, RuntimeError> {
        use core::Term as T;
        match term {
            T::Var(v) => lookup(env, *v).ok_or_else(|| RuntimeError {
                msg: format!("unbound variable {:?}", v),
            }),
            T::Lit(l) => Ok(lit_value(l)),

            T::Lam(param, body) => Ok(Value::Closure {
                param: *param,
                body: Rc::new((**body).clone()),
                env: env.clone(),
            }),

            T::App(f, a) => {
                let func = self.eval(f, env)?;
                let arg = self.eval(a, env)?;
                self.apply(func, arg)
            }

            T::Let(v, rhs, body) => {
                let val = self.eval(rhs, env)?;
                let scope = child(env);
                define(&scope, *v, val);
                self.eval(body, &scope)
            }

            T::LetRec(binds, body) => {
                let scope = child(env);
                for (v, _) in binds {
                    define(&scope, *v, Value::Unit);
                }
                for (v, rhs) in binds {
                    let val = self.eval(rhs, &scope)?;
                    define(&scope, *v, val);
                }
                self.eval(body, &scope)
            }

            T::If(c, t, e) => match self.eval(c, env)? {
                Value::Bool(true) => self.eval(t, env),
                Value::Bool(false) => self.eval(e, env),
                other => err(format!("`if` condition is not a Bool: {}", other)),
            },

            T::Tuple(items) => Ok(Value::Tuple(
                items
                    .iter()
                    .map(|t| self.eval(t, env))
                    .collect::<Result<_, _>>()?,
            )),
            T::List(items) => Ok(Value::List(
                items
                    .iter()
                    .map(|t| self.eval(t, env))
                    .collect::<Result<_, _>>()?,
            )),
            T::ListCons(head, tail) => {
                let head = self.eval(head, env)?;
                match self.eval(tail, env)? {
                    Value::List(mut items) => {
                        items.insert(0, head);
                        Ok(Value::List(items))
                    }
                    other => err(format!("`Cons` tail is not a list: {}", other)),
                }
            }
            T::Proj(t, i) => match self.eval(t, env)? {
                Value::Tuple(items) => {
                    items.into_iter().nth(*i).ok_or_else(|| RuntimeError {
                        msg: format!("tuple projection {i} out of range"),
                    })
                }
                other => err(format!("cannot project field {i} out of {}", other)),
            },

            T::Record(fields) => {
                let mut map = BTreeMap::new();
                for (label, t) in fields {
                    map.insert(*label, self.eval(t, env)?);
                }
                Ok(Value::Record(map))
            }
            T::Sel(t, label) => match self.eval(t, env)? {
                Value::Record(map) => {
                    map.get(label).cloned().ok_or_else(|| RuntimeError {
                        msg: format!("record has no field `{label}`"),
                    })
                }
                // Nominal record / data value: index by the constructor's field order.
                Value::Ctor(cname, vals) => self
                    .ctor_fields
                    .get(&cname)
                    .and_then(|fs| fs.iter().position(|f| f == label))
                    .and_then(|i| vals.into_iter().nth(i))
                    .ok_or_else(|| RuntimeError {
                        msg: format!("`{cname}` has no field `{label}`"),
                    }),
                other => err(format!("cannot select `.{label}` from {}", other)),
            },
            T::Extend(t, label, v) => match self.eval(t, env)? {
                Value::Record(mut map) => {
                    map.insert(*label, self.eval(v, env)?);
                    Ok(Value::Record(map))
                }
                other => err(format!("cannot extend non-record {}", other)),
            },

            T::Ctor(name, args) => Ok(Value::Ctor(
                *name,
                args.iter()
                    .map(|t| self.eval(t, env))
                    .collect::<Result<_, _>>()?,
            )),

            T::Case(scrut, arms) => {
                let value = self.eval(scrut, env)?;
                for (pat, body) in arms {
                    let scope = child(env);
                    if match_pat(pat, &value, &scope) {
                        return self.eval(body, &scope);
                    }
                }
                err("non-exhaustive pattern match")
            }

            T::Prim(op, args) => {
                let vals: Vec<Value> = args
                    .iter()
                    .map(|t| self.eval(t, env))
                    .collect::<Result<_, _>>()?;
                run_prim(*op, vals)
            }

            T::Error => err("evaluating an ill-formed expression"),
        }
    }

    fn apply(&self, func: Value, arg: Value) -> Result<Value, RuntimeError> {
        match func {
            Value::Closure { param, body, env } => {
                let scope = child(&env);
                define(&scope, param, arg);
                self.eval(&body, &scope)
            }
            Value::Builtin { op, mut args } => {
                args.push(arg);
                if args.len() >= op.arity() {
                    run_prim(op, args)
                } else {
                    Ok(Value::Builtin { op, args })
                }
            }
            other => err(format!("{} is not a function", other)),
        }
    }
}

fn lit_value(lit: &core::Lit) -> Value {
    match lit {
        core::Lit::Int(i) => Value::Int(*i),
        core::Lit::Str(s) => Value::Str(*s),
        core::Lit::Bool(b) => Value::Bool(*b),
        core::Lit::Unit => Value::Unit,
    }
}

// --- pattern matching ------------------------------------------------------

fn match_pat(pat: &core::Pat, value: &Value, scope: &Env) -> bool {
    use core::Pat as P;
    match (pat, value) {
        (P::Wild, _) => true,
        (P::Var(v), _) => {
            define(scope, *v, value.clone());
            true
        }
        (P::As(v, sub), _) => {
            define(scope, *v, value.clone());
            match_pat(sub, value, scope)
        }
        (P::Lit(core::Lit::Int(a)), Value::Int(b)) => a == b,
        (P::Lit(core::Lit::Str(a)), Value::Str(b)) => a == b,
        (P::Lit(core::Lit::Bool(a)), Value::Bool(b)) => a == b,
        (P::Lit(core::Lit::Unit), Value::Unit) => true,
        (P::Tuple(ps), Value::Tuple(vs)) if ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::List(ps), Value::List(vs)) if ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::ListNil, Value::List(vs)) => vs.is_empty(),
        (P::ListCons(ph, pt), Value::List(vs)) if !vs.is_empty() => {
            match_pat(ph, &vs[0], scope)
                && match_pat(pt, &Value::List(vs[1..].to_vec()), scope)
        }
        (P::Ctor(name, ps), Value::Ctor(vname, vs)) if name == vname && ps.len() == vs.len() => {
            ps.iter().zip(vs).all(|(p, v)| match_pat(p, v, scope))
        }
        (P::Record(fields), Value::Record(map)) => fields.iter().all(|(label, p)| {
            map.get(label).is_some_and(|v| match_pat(p, v, scope))
        }),
        _ => false,
    }
}

// --- primitives ----------------------------------------------------------

fn run_prim(op: core::Prim, args: Vec<Value>) -> Result<Value, RuntimeError> {
    use core::Prim::*;

    let int2 = |a: &Value, b: &Value| -> Result<(i64, i64), RuntimeError> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok((*x, *y)),
            _ => err(format!("expected two Ints, got {} and {}", a, b)),
        }
    };

    match op {
        Add | Sub | Mul | Div | Mod | Pow => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Add => x.wrapping_add(y),
                Sub => x.wrapping_sub(y),
                Mul => x.wrapping_mul(y),
                Div => {
                    if y == 0 {
                        return err("division by zero");
                    }
                    x / y
                }
                Mod => {
                    if y == 0 {
                        return err("modulo by zero");
                    }
                    x % y
                }
                Pow => x.pow(y.max(0) as u32),
                _ => unreachable!(),
            };
            Ok(Value::Int(r))
        }
        Lt | Gt | Le | Ge => {
            let (x, y) = int2(&args[0], &args[1])?;
            let r = match op {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                Ge => x >= y,
                _ => unreachable!(),
            };
            Ok(Value::Bool(r))
        }
        Eq => Ok(Value::Bool(value_eq(&args[0], &args[1]))),
        Ne => Ok(Value::Bool(!value_eq(&args[0], &args[1]))),
        And | Or => {
            let a = as_bool(&args[0])?;
            let b = as_bool(&args[1])?;
            Ok(Value::Bool(if matches!(op, And) { a && b } else { a || b }))
        }
        Neg => match &args[0] {
            Value::Int(x) => Ok(Value::Int(-x)),
            other => err(format!("`neg` expects an Int, got {}", other)),
        },
        Not => Ok(Value::Bool(!as_bool(&args[0])?)),
        Print => {
            print!("{}", args[0]);
            Ok(Value::Unit)
        }
        Println => {
            println!("{}", args[0]);
            Ok(Value::Unit)
        }
    }
}

fn as_bool(v: &Value) -> Result<bool, RuntimeError> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => err(format!("expected a Bool, got {}", other)),
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::Tuple(x), Value::Tuple(y)) | (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| value_eq(p, q))
        }
        (Value::Ctor(n1, x), Value::Ctor(n2, y)) => {
            n1 == n2 && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| value_eq(p, q))
        }
        (Value::Record(x), Value::Record(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| value_eq(v, w)))
        }
        _ => false,
    }
}

// --- display -------------------------------------------------------------

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Str(s) => write!(f, "{s}"),
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
            Value::List(items) => {
                f.write_str("[")?;
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
            Value::Ctor(name, args) if args.is_empty() => write!(f, "{name}"),
            Value::Ctor(name, args) => {
                write!(f, "{name}(")?;
                for (i, v) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str(")")
            }
            Value::Closure { .. } => f.write_str("<closure>"),
            Value::Builtin { op, .. } => write!(f, "<builtin {op:?}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Lit, Prim, Program, Term};

    fn run_term(term: Term) -> Value {
        let program = Program {
            defs: vec![core::Def {
                var: crate::hir::VarId::fresh(),
                name: "main".into(),
                term,
            }],
            entry: None,
            ..Default::default()
        };
        // no entry -> `run` returns Unit, so evaluate the single def directly
        let interp = Interp {
            ctor_fields: &program.ctor_fields,
        };
        interp.eval(&program.defs[0].term, &root_env()).unwrap()
    }

    #[test]
    fn prim_arithmetic() {
        assert_eq!(
            run_prim(Prim::Add, vec![Value::Int(2), Value::Int(3)]).unwrap().to_string(),
            "5"
        );
        assert_eq!(
            run_prim(Prim::Mul, vec![Value::Int(4), Value::Int(5)]).unwrap().to_string(),
            "20"
        );
    }

    #[test]
    fn prim_division_by_zero_errors() {
        assert!(run_prim(Prim::Div, vec![Value::Int(1), Value::Int(0)]).is_err());
    }

    #[test]
    fn prim_equality_is_structural() {
        let a = Value::Tuple(vec![Value::Int(1), Value::List(vec![Value::Int(2)])]);
        let b = Value::Tuple(vec![Value::Int(1), Value::List(vec![Value::Int(2)])]);
        assert!(value_eq(&a, &b));
        assert!(!value_eq(&a, &Value::Int(1)));
    }

    #[test]
    fn evaluates_arithmetic_term() {
        // (\x -> x + 1) 41
        let x = crate::hir::VarId::fresh();
        let body = Term::Prim(Prim::Add, vec![Term::Var(x), Term::Lit(Lit::Int(1))]);
        let term = Term::App(
            Box::new(Term::Lam(x, Box::new(body))),
            Box::new(Term::Lit(Lit::Int(41))),
        );
        assert_eq!(run_term(term).to_string(), "42");
    }

    #[test]
    fn list_cons_prepends() {
        let term = Term::ListCons(
            Box::new(Term::Lit(Lit::Int(0))),
            Box::new(Term::List(vec![Term::Lit(Lit::Int(1)), Term::Lit(Lit::Int(2))])),
        );
        assert_eq!(run_term(term).to_string(), "[0, 1, 2]");
    }

    #[test]
    fn value_display_forms() {
        assert_eq!(Value::Unit.to_string(), "()");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(
            Value::Ctor("Some".into(), vec![Value::Int(3)]).to_string(),
            "Some(3)"
        );
        assert_eq!(Value::Ctor("None".into(), vec![]).to_string(), "None");
    }
}
