//! The primitives.
//!
//! Ported operation for operation from `meadow_eval::run_prim` — the wrapping
//! arithmetic, the two zero checks, the clamping in `arraySlice`, the exact
//! error strings where a program can see them. Anything that differs is a
//! difference between the two runtimes rather than a difference in the language,
//! and the differential tests would call it a bug.
//!
//! # Allocation discipline
//!
//! The collector moves objects, so a primitive that allocates cannot be holding
//! an address when it does. Every one of them follows the same three steps:
//!
//! 1. **measure** — read whatever sizes the result depends on;
//! 2. [`Vm::ensure`] — make room, which may collect and move everything;
//! 3. **re-read** the arguments out of registers, which the collector has
//!    updated, and only then allocate.
//!
//! Skipping step 3 is the one mistake this file can make, and it would show up
//! as a value that is subtly the wrong object rather than as a crash — so the
//! places it matters say so.

use crate::heap::Kind;
use crate::value::{Addr, Value};
use crate::vm::{err, Error, Vm};
use meadow_core::Prim;
use meadow_intern::InternedString;
use num_bigint::{BigInt, Sign};
use num_traits::{ToPrimitive, Zero};

impl Vm<'_> {
    pub(crate) fn alloc_bigint(&mut self, n: BigInt) -> Value {
        let (sign, digits) = n.to_u32_digits();
        // Safe to `ensure` here: the number is a Rust value, not a heap address.
        self.ensure(1 + digits.len());
        let meta = match sign {
            Sign::NoSign => 0,
            Sign::Plus => 1,
            Sign::Minus => 2,
        };
        let fields: Vec<Value> = digits.iter().map(|d| Value::Int(*d as i64)).collect();
        Value::Obj(self.heap.alloc(Kind::BigInt, meta, &fields))
    }

    fn int(&self, v: Value) -> Result<i64, Error> {
        match v {
            Value::Int(i) => Ok(i),
            other => err(format!("expected an Int, got {}", self.show(other))),
        }
    }

    fn index(&self, v: Value) -> Result<usize, Error> {
        match self.int(v)? {
            i if i >= 0 => Ok(i as usize),
            i => err(format!("expected a non-negative index, got {i}")),
        }
    }

    fn int2(&self, a: Value, b: Value) -> Result<(i64, i64), Error> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok((x, y)),
            _ => err(format!(
                "expected two Ints, got {} and {}",
                self.show(a),
                self.show(b)
            )),
        }
    }

    fn float2(&self, a: Value, b: Value) -> Result<(f64, f64), Error> {
        match (a, b) {
            (Value::Float(x), Value::Float(y)) => Ok((x, y)),
            _ => err(format!(
                "expected two Floats, got {} and {}",
                self.show(a),
                self.show(b)
            )),
        }
    }

    fn big2(&self, a: Value, b: Value) -> Result<(BigInt, BigInt), Error> {
        match (self.bigint_at(a), self.bigint_at(b)) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => err(format!(
                "expected two BigInts, got {} and {}",
                self.show(a),
                self.show(b)
            )),
        }
    }

    fn array(&self, v: Value) -> Result<Addr, Error> {
        match v.addr().filter(|a| self.heap.kind(*a) == Kind::Array) {
            Some(a) => Ok(a),
            None => err(format!("expected an Array, got {}", self.show(v))),
        }
    }

    fn bytes(&self, v: Value, what: &str) -> Result<Vec<u8>, Error> {
        let a = self.array(v)?;
        let mut out = Vec::with_capacity(self.heap.len(a));
        for i in 0..self.heap.len(a) {
            match self.heap.field(a, i) {
                Value::Int(n) if (0..=255).contains(&n) => out.push(n as u8),
                other => {
                    return err(format!(
                        "`{what}`: not a byte (0..255): {}",
                        self.show(other)
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Build data whose constructor a primitive names rather than the compiler.
    ///
    /// The tag has to be the one the program's `switch` arms were compiled with,
    /// or a `match` on the result would miss.
    fn data(&mut self, name: &str, fields: &[Value]) -> Value {
        let tag = self.ctor_tag(name);
        Value::Obj(self.heap.alloc(Kind::Data, tag, fields))
    }

    /// Run `p` on the arguments at `base`, into `dst`.
    pub(crate) fn run_prim(&mut self, p: Prim, base: u8, argc: u8, dst: u8) -> Result<(), Error> {
        use Prim::*;
        let arg = |vm: &Vm, i: u8| vm.reg(base + i);
        let _ = argc;

        let out = match p {
            // --- Int ------------------------------------------------------
            Add | Sub | Mul | Div | Mod | Pow => {
                let (x, y) = self.int2(arg(self, 0), arg(self, 1))?;
                Value::Int(match p {
                    Add => x.wrapping_add(y),
                    Sub => x.wrapping_sub(y),
                    Mul => x.wrapping_mul(y),
                    Div if y == 0 => return err("division by zero"),
                    Div => x.wrapping_div(y),
                    Mod if y == 0 => return err("modulo by zero"),
                    Mod => x.wrapping_rem(y),
                    _ => {
                        let e = u32::try_from(y).map_err(|_| Error {
                            msg: format!("`^` exponent must fit in u32, got {y}"),
                        })?;
                        x.wrapping_pow(e)
                    }
                })
            }
            Lt | Gt | Le | Ge => {
                let (x, y) = self.int2(arg(self, 0), arg(self, 1))?;
                Value::Bool(match p {
                    Lt => x < y,
                    Gt => x > y,
                    Le => x <= y,
                    _ => x >= y,
                })
            }
            Neg => match arg(self, 0) {
                Value::Int(x) => Value::Int(x.wrapping_neg()),
                other => return err(format!("`neg` expects an Int, got {}", self.show(other))),
            },

            // --- Float ----------------------------------------------------
            AddF | SubF | MulF | DivF => {
                let (x, y) = self.float2(arg(self, 0), arg(self, 1))?;
                Value::Float(match p {
                    AddF => x + y,
                    SubF => x - y,
                    MulF => x * y,
                    _ => x / y,
                })
            }
            LtF | GtF | LeF | GeF => {
                let (x, y) = self.float2(arg(self, 0), arg(self, 1))?;
                Value::Bool(match p {
                    LtF => x < y,
                    GtF => x > y,
                    LeF => x <= y,
                    _ => x >= y,
                })
            }
            ToFloat => match arg(self, 0) {
                Value::Int(x) => Value::Float(x as f64),
                other => {
                    return err(format!("`toFloat` expects an Int, got {}", self.show(other)));
                }
            },
            Floor => match arg(self, 0) {
                Value::Float(x) => {
                    let f = x.floor();
                    if !f.is_finite() {
                        return err(format!("`floor` of a non-finite Float: {x}"));
                    }
                    Value::Int(f as i64)
                }
                other => {
                    return err(format!("`floor` expects a Float, got {}", self.show(other)));
                }
            },

            // --- BigInt ---------------------------------------------------
            AddB | SubB | MulB | DivB | ModB | PowB => {
                let (x, y) = self.big2(arg(self, 0), arg(self, 1))?;
                let r = match p {
                    AddB => x + y,
                    SubB => x - y,
                    MulB => x * y,
                    DivB if y.is_zero() => return err("division by zero"),
                    DivB => x / y,
                    ModB if y.is_zero() => return err("modulo by zero"),
                    ModB => x % y,
                    _ => {
                        let e = y.to_u32().ok_or_else(|| Error {
                            msg: format!("`^~` exponent must fit in u32, got {y}"),
                        })?;
                        x.pow(e)
                    }
                };
                // Nothing heap-shaped is live across this: `r` is a Rust value.
                self.alloc_bigint(r)
            }
            LtB | GtB | LeB | GeB => {
                let (x, y) = self.big2(arg(self, 0), arg(self, 1))?;
                Value::Bool(match p {
                    LtB => x < y,
                    GtB => x > y,
                    LeB => x <= y,
                    _ => x >= y,
                })
            }
            ToBig => match arg(self, 0) {
                Value::Int(x) => self.alloc_bigint(BigInt::from(x)),
                other => {
                    return err(format!(
                        "`toBigInt` expects an Int, got {}",
                        self.show(other)
                    ));
                }
            },
            ToInt => match self.bigint_at(arg(self, 0)) {
                Some(x) => match x.to_i64() {
                    Some(n) => Value::Int(n),
                    None => return err(format!("`toInt`: {x} does not fit in Int")),
                },
                None => {
                    return err(format!(
                        "`toInt` expects a BigInt, got {}",
                        self.show(arg(self, 0))
                    ));
                }
            },

            // --- structural ----------------------------------------------
            Eq => Value::Bool(self.value_eq(arg(self, 0), arg(self, 1))),
            Ne => Value::Bool(!self.value_eq(arg(self, 0), arg(self, 1))),
            Show => Value::Str(InternedString::from(self.show(arg(self, 0)))),

            Print => {
                print!("{}", self.show(arg(self, 0)));
                Value::Unit
            }
            Println => {
                println!("{}", self.show(arg(self, 0)));
                Value::Unit
            }

            // --- the builtin Array ----------------------------------------
            ArrayLen => Value::Int(self.heap.len(self.array(arg(self, 0))?) as i64),
            ArrayGet => {
                let a = self.array(arg(self, 0))?;
                let i = self.index(arg(self, 1))?;
                let n = self.heap.len(a);
                if i >= n {
                    return err(format!("arrayGet: index {i} out of bounds (len {n})"));
                }
                self.heap.field(a, i)
            }
            ArrayGetOr => {
                let a = self.array(arg(self, 1))?;
                let i = self.index(arg(self, 2))?;
                if i < self.heap.len(a) {
                    self.heap.field(a, i)
                } else {
                    arg(self, 0)
                }
            }
            ArraySet => {
                let n = self.heap.len(self.array(arg(self, 0))?);
                let i = self.index(arg(self, 1))?;
                if i >= n {
                    return err(format!("arraySet: index {i} out of bounds (len {n})"));
                }
                self.ensure(1 + n);
                // Re-read: the collection above moved the array.
                let a = self.array(arg(self, 0))?;
                let mut fields = self.heap.fields(a);
                fields[i] = arg(self, 2);
                Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
            }
            ArrayPush => {
                let n = self.heap.len(self.array(arg(self, 0))?);
                self.ensure(2 + n);
                let a = self.array(arg(self, 0))?;
                let mut fields = self.heap.fields(a);
                fields.push(arg(self, 1));
                Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
            }
            ArrayPop => {
                let n = self.heap.len(self.array(arg(self, 0))?);
                if n == 0 {
                    return err("arrayPop: empty array");
                }
                self.ensure(n);
                let a = self.array(arg(self, 0))?;
                let mut fields = self.heap.fields(a);
                fields.pop();
                Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
            }
            ArraySlice => {
                let n = self.heap.len(self.array(arg(self, 0))?) as i64;
                let from = self.int(arg(self, 1))?.clamp(0, n) as usize;
                let to = self.int(arg(self, 2))?.clamp(from as i64, n) as usize;
                self.ensure(1 + (to - from));
                let a = self.array(arg(self, 0))?;
                let fields: Vec<Value> = (from..to).map(|i| self.heap.field(a, i)).collect();
                Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
            }
            ArrayConcat => {
                let x = self.heap.len(self.array(arg(self, 0))?);
                let y = self.heap.len(self.array(arg(self, 1))?);
                if x == 0 {
                    arg(self, 1)
                } else if y == 0 {
                    arg(self, 0)
                } else {
                    self.ensure(1 + x + y);
                    let a = self.array(arg(self, 0))?;
                    let b = self.array(arg(self, 1))?;
                    let mut fields = self.heap.fields(a);
                    fields.extend(self.heap.fields(b));
                    Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
                }
            }

            // --- bitwise --------------------------------------------------
            Shl | Shr | Ushr | BitAnd | BitOr | BitXor => {
                let (x, y) = self.int2(arg(self, 0), arg(self, 1))?;
                Value::Int(match p {
                    Shl => x.wrapping_shl(y as u32),
                    Shr => x.wrapping_shr(y as u32),
                    Ushr => (x as u64).wrapping_shr(y as u32) as i64,
                    BitAnd => x & y,
                    BitOr => x | y,
                    _ => x ^ y,
                })
            }
            BitNot => Value::Int(!self.int(arg(self, 0))?),
            PopCount => Value::Int(self.int(arg(self, 0))?.count_ones() as i64),

            // --- text and bytes -------------------------------------------
            StringToBytes => match arg(self, 0) {
                Value::Str(s) => {
                    let fields: Vec<Value> = s.bytes().map(|b| Value::Int(b as i64)).collect();
                    self.ensure(1 + fields.len());
                    Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
                }
                other => {
                    return err(format!(
                        "`stringToBytes` expects a String, got {}",
                        self.show(other)
                    ));
                }
            },
            BytesToString => {
                let buf = self.bytes(arg(self, 0), "bytesToString")?;
                Value::Str(InternedString::from(
                    String::from_utf8_lossy(&buf).into_owned(),
                ))
            }
            BytesToHex => {
                let buf = self.bytes(arg(self, 0), "bytesToHex")?;
                let mut s = String::with_capacity(buf.len() * 2);
                for b in buf {
                    s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble"));
                    s.push(char::from_digit((b & 0xf) as u32, 16).expect("nibble"));
                }
                Value::Str(InternedString::from(s))
            }
            BytesFromHex => {
                let s = match arg(self, 0) {
                    Value::Str(s) => s,
                    other => {
                        return err(format!(
                            "`bytesFromHex` expects a String, got {}",
                            self.show(other)
                        ));
                    }
                };
                let bytes = s.as_bytes();
                let mut out = Vec::with_capacity(bytes.len() / 2);
                let mut ok = bytes.len() % 2 == 0;
                if ok {
                    for pair in bytes.chunks_exact(2) {
                        match (
                            (pair[0] as char).to_digit(16),
                            (pair[1] as char).to_digit(16),
                        ) {
                            (Some(h), Some(l)) => out.push(Value::Int(((h << 4) | l) as i64)),
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                }
                if !ok {
                    self.ensure(1);
                    self.data("None", &[])
                } else {
                    // The array first, then `Just` around it — with room for
                    // both reserved up front, so the array cannot move in
                    // between.
                    self.ensure(2 + out.len() + 1);
                    let arr = Value::Obj(self.heap.alloc(Kind::Array, 0, &out));
                    self.data("Just", &[arr])
                }
            }
            CharCode => match arg(self, 0) {
                Value::Char(c) => Value::Int(c as i64),
                other => {
                    return err(format!(
                        "charCode: expected a Char, got {}",
                        self.show(other)
                    ));
                }
            },
            CharFromCode => match arg(self, 0) {
                Value::Int(n) => match u32::try_from(n).ok().and_then(char::from_u32) {
                    Some(c) => Value::Char(c),
                    None => {
                        return err(format!("charFromCode: {n} is not a Unicode scalar value"));
                    }
                },
                other => {
                    return err(format!(
                        "charFromCode: expected an Int, got {}",
                        self.show(other)
                    ));
                }
            },
            StringToChars => match arg(self, 0) {
                Value::Str(s) => {
                    let fields: Vec<Value> = s.chars().map(Value::Char).collect();
                    self.ensure(1 + fields.len());
                    Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
                }
                other => {
                    return err(format!(
                        "stringToChars: expected a String, got {}",
                        self.show(other)
                    ));
                }
            },
            CharsToString => {
                let a = match arg(self, 0).addr().filter(|a| self.heap.kind(*a) == Kind::Array) {
                    Some(a) => a,
                    None => {
                        return err(format!(
                            "charsToString: expected an Array, got {}",
                            self.show(arg(self, 0))
                        ));
                    }
                };
                let mut s = String::with_capacity(self.heap.len(a));
                for i in 0..self.heap.len(a) {
                    match self.heap.field(a, i) {
                        Value::Char(c) => s.push(c),
                        other => {
                            return err(format!(
                                "charsToString: expected a Char, got {}",
                                self.show(other)
                            ));
                        }
                    }
                }
                Value::Str(InternedString::from(s))
            }

            // --- the mutable cell -----------------------------------------
            //
            // The only place a value a program can still see is changed in
            // place. Everything else here builds something new.
            NewRef => {
                self.ensure(2);
                let v = arg(self, 0);
                Value::Obj(self.heap.alloc(Kind::Ref, 0, &[v]))
            }
            GetRef => match arg(self, 0).addr().filter(|a| self.heap.kind(*a) == Kind::Ref) {
                Some(a) => self.heap.field(a, 0),
                None => {
                    return err(format!(
                        "getRef: expected a Ref, got {}",
                        self.show(arg(self, 0))
                    ));
                }
            },
            SetRef => match arg(self, 0).addr().filter(|a| self.heap.kind(*a) == Kind::Ref) {
                Some(a) => {
                    let v = arg(self, 1);
                    self.heap.set_field(a, 0, v);
                    Value::Unit
                }
                None => {
                    return err(format!(
                        "setRef: expected a Ref, got {}",
                        self.show(arg(self, 0))
                    ));
                }
            },
        };

        self.set(dst, out);
        Ok(())
    }
}
