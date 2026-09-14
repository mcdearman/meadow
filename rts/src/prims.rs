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
use crate::vm::{Error, Vm, err};
use meadow_core::{Prim, num};
use meadow_intern::InternedString;
use num_bigint::{BigInt, Sign};

fn arith(p: Prim) -> num::Arith {
    match p {
        Prim::Add | Prim::AddF => num::Arith::Add,
        Prim::Sub | Prim::SubF => num::Arith::Sub,
        Prim::Mul | Prim::MulF => num::Arith::Mul,
        Prim::Div | Prim::DivF => num::Arith::Div,
        Prim::Mod => num::Arith::Mod,
        _ => num::Arith::Pow,
    }
}

fn cmp(p: Prim) -> num::Cmp {
    match p {
        Prim::Lt | Prim::LtF => num::Cmp::Lt,
        Prim::Gt | Prim::GtF => num::Cmp::Gt,
        Prim::Le | Prim::LeF => num::Cmp::Le,
        _ => num::Cmp::Ge,
    }
}

fn bits(p: Prim) -> num::Bits {
    match p {
        Prim::Shl => num::Bits::Shl,
        Prim::Shr => num::Bits::Shr,
        Prim::Ushr => num::Bits::Ushr,
        Prim::BitAnd => num::Bits::And,
        Prim::BitOr => num::Bits::Or,
        _ => num::Bits::Xor,
    }
}

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

    /// A number, for `meadow_core::num`. A `BigInt` is read off the heap, which
    /// allocates nothing there.
    pub(crate) fn num(&self, v: Value) -> Result<num::Num, Error> {
        match self.try_num(v) {
            Some(n) => Ok(n),
            None => err(format!("expected a number, got {}", self.show(v))),
        }
    }

    /// [`Vm::num`], without building an error for something that is not one.
    pub(crate) fn try_num(&self, v: Value) -> Option<num::Num> {
        Some(match v {
            Value::Int(x) => num::Num::Int(x),
            Value::Word(w, b) => num::Num::Word(w, b),
            Value::Float(x) => num::Num::Float(x),
            Value::Float32(x) => num::Num::Float32(x),
            other => num::Num::Big(self.bigint_at(other)?),
        })
    }

    /// A number as a value. Only a `BigInt` allocates, and `alloc_bigint` makes
    /// its own room -- nothing heap-shaped may be held across this call.
    fn from_num(&mut self, n: num::Num) -> Value {
        match n {
            num::Num::Int(x) => Value::Int(x),
            num::Num::Word(w, b) => Value::Word(w, b),
            num::Num::Big(x) => self.alloc_bigint(x),
            num::Num::Float(x) => Value::Float(x),
            num::Num::Float32(x) => Value::Float32(x),
        }
    }

    fn array(&self, v: Value) -> Result<Addr, Error> {
        match v.addr().filter(|a| self.heap.kind(*a) == Kind::Array) {
            Some(a) => Ok(a),
            None => err(format!("expected an Array, got {}", self.show(v))),
        }
    }

    /// The run's `TVar`s.
    fn world(&self) -> Result<std::sync::Arc<crate::stm::World>, Error> {
        match &self.world {
            Some(w) => Ok(w.clone()),
            None => err("transactions need the scheduler; this machine is running on its own"),
        }
    }

    /// `v` put where every thread can read it: into `into` -- a region, whether
    /// it is new, and how big a fresh copy of what it holds was -- or into a
    /// new region. An immediate needs no region.
    fn share(
        &mut self,
        v: Value,
        into: Option<(std::sync::Arc<crate::region::Region>, bool, usize)>,
    ) -> Result<crate::stm::Shared, Error> {
        if !matches!(v, Value::Obj(_)) {
            return Ok(crate::stm::Shared {
                region: None,
                root: v,
                fresh: 0,
            });
        }
        let (region, fresh_region, fresh) =
            into.unwrap_or_else(|| (crate::region::Region::new(), true, 0));
        self.heap.adopt(&region);
        let root = match self.heap.compact_into(region.id, v) {
            Ok(root) => root,
            Err(why) => return err(meadow_core::stm::unstorable(why.describe())),
        };
        let fresh = if fresh_region { region.used() } else { fresh };
        Ok(crate::stm::Shared {
            region: Some(region),
            root,
            fresh,
        })
    }

    /// The transaction this thread is in, for `op`.
    fn txn(&mut self, op: &str) -> Result<&mut crate::stm::Txn, Error> {
        match &mut self.txn {
            Some(t) => Ok(t),
            None => err(meadow_core::stm::outside(op)),
        }
    }

    /// `v` exported for another thread, or the error saying why it cannot be.
    fn sendable(&self, v: Value) -> Result<crate::heap::Parcel, Error> {
        self.heap
            .export(v)
            .or_else(|why| err(meadow_core::thread::unsendable(why.describe())))
    }

    /// The number of a channel or thread handle.
    fn handle(&self, v: Value, kind: Kind, what: &str) -> Result<u32, Error> {
        match v.addr().filter(|a| self.heap.kind(*a) == kind) {
            Some(a) => Ok(self.heap.meta(a)),
            None => err(format!("expected {what}, got {}", self.show(v))),
        }
    }

    fn compact_handle(&self, v: Value) -> Result<Addr, Error> {
        match v.addr().filter(|a| self.heap.kind(*a) == Kind::Compact) {
            Some(a) => Ok(a),
            None => err(format!("expected a Compact, got {}", self.show(v))),
        }
    }

    fn mut_array(&self, v: Value) -> Result<Addr, Error> {
        match v.addr().filter(|a| self.heap.kind(*a) == Kind::MutArray) {
            Some(a) => Ok(a),
            None => err(format!("expected a mutable array, got {}", self.show(v))),
        }
    }

    fn bytes(&self, v: Value, what: &str) -> Result<Vec<u8>, Error> {
        let a = self.array(v)?;
        let mut out = Vec::with_capacity(self.heap.len(a));
        for i in 0..self.heap.len(a) {
            match self.heap.field(a, i) {
                Value::Word(num::Width::U8, b) => out.push(b as u8),
                // A literal in code generic over its integer type -- see
                // `meadow_core::num`.
                Value::Int(n) if (0..=255).contains(&n) => out.push(n as u8),
                other => {
                    return err(format!("`{what}`: not a byte: {}", self.show(other)));
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

    /// Run `p` on the values in `srcs`, into `dst`.
    ///
    /// Registers rather than values: a primitive that allocates has to re-read
    /// its arguments after making room, because the collector will have moved
    /// them. See the discipline at the top of this file.
    ///
    /// Register *numbers* rather than indices, and that is not incidental:
    /// `[u8; 3]` is passed in a register and `[usize; 3]` is passed on the
    /// stack. This is the innermost call in the machine — every arithmetic
    /// operation the program performs — and widening it cost about 4% across
    /// every benchmark.
    pub(crate) fn run_prim(&mut self, p: Prim, srcs: [u8; 3], dst: u8) -> Result<(), Error> {
        use Prim::*;
        let arg = |vm: &Vm, i: usize| vm.reg(srcs[i]);

        // Typed primitives run as their untyped ones for now; the machine's
        // own instructions for them come with the typed bytecode.
        let p = p.untyped();
        let out = match p {
            // --- numbers ---------------------------------------------------
            //
            // Two `Int`s -- by far the common case -- are handled inline; every
            // other pairing goes through `meadow_core::num`, shared with the
            // other engines.
            Add | Sub | Mul | Div | Mod | Pow => match (arg(self, 0), arg(self, 1)) {
                (Value::Int(x), Value::Int(y)) => Value::Int(match p {
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
                }),
                (a, b) => {
                    let r = num::int_arith(arith(p), self.num(a)?, self.num(b)?);
                    self.from_num(r.map_err(|msg| Error { msg })?)
                }
            },
            // A fused comparison may not allocate (`Prim::compares`), and none
            // of these does: a `BigInt` is read, not built.
            Lt | Gt | Le | Ge => match (arg(self, 0), arg(self, 1)) {
                (Value::Int(x), Value::Int(y)) => Value::Bool(match p {
                    Lt => x < y,
                    Gt => x > y,
                    Le => x <= y,
                    _ => x >= y,
                }),
                (a, b) => Value::Bool(
                    num::int_cmp(cmp(p), self.num(a)?, self.num(b)?)
                        .map_err(|msg| Error { msg })?,
                ),
            },
            Neg => match arg(self, 0) {
                Value::Int(x) => Value::Int(x.wrapping_neg()),
                other => {
                    let r = num::int_neg(self.num(other)?);
                    self.from_num(r.map_err(|msg| Error { msg })?)
                }
            },
            AddF | SubF | MulF | DivF => match (arg(self, 0), arg(self, 1)) {
                (Value::Float(x), Value::Float(y)) => Value::Float(match p {
                    AddF => x + y,
                    SubF => x - y,
                    MulF => x * y,
                    _ => x / y,
                }),
                (a, b) => {
                    let r = num::float_arith(arith(p), self.num(a)?, self.num(b)?);
                    self.from_num(r.map_err(|msg| Error { msg })?)
                }
            },
            LtF | GtF | LeF | GeF => Value::Bool(
                num::float_cmp(cmp(p), self.num(arg(self, 0))?, self.num(arg(self, 1))?)
                    .map_err(|msg| Error { msg })?,
            ),
            ToFloat => {
                Value::Float(num::to_float(self.num(arg(self, 0))?).map_err(|msg| Error { msg })?)
            }
            ToFloat32 => Value::Float32(
                num::to_float32(self.num(arg(self, 0))?).map_err(|msg| Error { msg })?,
            ),
            Floor => Value::Int(num::floor(self.num(arg(self, 0))?).map_err(|msg| Error { msg })?),
            ToBig | ToInt | ToWord(_) => {
                let target = match p {
                    ToBig => num::IntTarget::Big,
                    ToInt => num::IntTarget::Int,
                    ToWord(w) => num::IntTarget::Word(w),
                    _ => unreachable!("matched above"),
                };
                let r = num::to_int(target, self.num(arg(self, 0))?);
                self.from_num(r.map_err(|msg| Error { msg })?)
            }

            // --- structural ----------------------------------------------
            Eq => Value::Bool(self.value_eq(arg(self, 0), arg(self, 1))),
            Ne => Value::Bool(!self.value_eq(arg(self, 0), arg(self, 1))),
            Show => Value::Str(InternedString::from(self.show(arg(self, 0)))),
            Hash => Value::Int(self.hash_value(arg(self, 0))?),

            // A `String` as its text; everything else the way `show` renders
            // it. See `meadow_eval::displayed` for why.
            Display => Value::Str(InternedString::from(self.displayed(arg(self, 0)))),

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
            Shl | Shr | Ushr | BitAnd | BitOr | BitXor => match (arg(self, 0), arg(self, 1)) {
                (Value::Int(x), Value::Int(y)) => Value::Int(match p {
                    Shl => x.wrapping_shl(y as u32),
                    Shr => x.wrapping_shr(y as u32),
                    Ushr => (x as u64).wrapping_shr(y as u32) as i64,
                    BitAnd => x & y,
                    BitOr => x | y,
                    _ => x ^ y,
                }),
                (a, b) => {
                    let r = num::int_bits(bits(p), self.num(a)?, self.num(b)?);
                    self.from_num(r.map_err(|msg| Error { msg })?)
                }
            },
            BitNot => {
                let r = num::int_not(self.num(arg(self, 0))?);
                self.from_num(r.map_err(|msg| Error { msg })?)
            }
            PopCount => {
                Value::Int(num::pop_count(self.num(arg(self, 0))?).map_err(|msg| Error { msg })?)
            }
            BitWidth => {
                Value::Int(num::bit_width(&self.num(arg(self, 0))?).map_err(|msg| Error { msg })?)
            }

            // --- text and bytes -------------------------------------------
            StringToBytes => match arg(self, 0) {
                Value::Str(s) => {
                    let fields: Vec<Value> = s
                        .bytes()
                        .map(|b| Value::Word(num::Width::U8, b as u64))
                        .collect();
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
                            (Some(h), Some(l)) => {
                                out.push(Value::Word(num::Width::U8, ((h << 4) | l) as u64))
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                }
                if !ok {
                    self.ensure(1);
                    self.data("Maybe.None", &[])
                } else {
                    // The array first, then `Just` around it — with room for
                    // both reserved up front, so the array cannot move in
                    // between.
                    self.ensure(2 + out.len() + 1);
                    let arr = Value::Obj(self.heap.alloc(Kind::Array, 0, &out));
                    self.data("Maybe.Just", &[arr])
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
                let a = match arg(self, 0)
                    .addr()
                    .filter(|a| self.heap.kind(*a) == Kind::Array)
                {
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
            GetRef => match arg(self, 0)
                .addr()
                .filter(|a| self.heap.kind(*a) == Kind::Ref)
            {
                Some(a) => self.heap.field(a, 0),
                None => {
                    return err(format!(
                        "getRef: expected a Ref, got {}",
                        self.show(arg(self, 0))
                    ));
                }
            },
            SetRef => match arg(self, 0)
                .addr()
                .filter(|a| self.heap.kind(*a) == Kind::Ref)
            {
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

            // --- the mutable array ----------------------------------------
            //
            // Written in place, like a `Ref`. `runSt` never gets here: lowering
            // applies its body.
            RunSt => return err("runSt reached the VM; lowering applies its body"),
            StNewArray => {
                let n = match self.int(arg(self, 0))? {
                    n if n >= 0 => n as usize,
                    n => {
                        return err(format!(
                            "stNewArray: expected a length of zero or more, got {n}"
                        ));
                    }
                };
                self.ensure(1 + n);
                let fill = vec![arg(self, 1); n];
                Value::Obj(self.heap.alloc(Kind::MutArray, 0, &fill))
            }
            StGetArray => {
                let a = self.mut_array(arg(self, 0))?;
                let i = self.index(arg(self, 1))?;
                let n = self.heap.len(a);
                if i >= n {
                    return err(format!("stGetArray: index {i} out of bounds (len {n})"));
                }
                self.heap.field(a, i)
            }
            StSetArray => {
                let a = self.mut_array(arg(self, 0))?;
                let i = self.index(arg(self, 1))?;
                let n = self.heap.len(a);
                if i >= n {
                    return err(format!("stSetArray: index {i} out of bounds (len {n})"));
                }
                let v = arg(self, 2);
                self.heap.set_field(a, i, v);
                Value::Unit
            }
            StArrayLen => Value::Int(self.heap.len(self.mut_array(arg(self, 0))?) as i64),
            StFreeze => {
                let n = self.heap.len(self.mut_array(arg(self, 0))?);
                self.ensure(1 + n);
                // Re-read: making room may have moved it.
                let a = self.mut_array(arg(self, 0))?;
                let fields = self.heap.fields(a);
                Value::Obj(self.heap.alloc(Kind::Array, 0, &fields))
            }
            StThaw => {
                let n = self.heap.len(self.array(arg(self, 0))?);
                self.ensure(1 + n);
                let a = self.array(arg(self, 0))?;
                let fields = self.heap.fields(a);
                Value::Obj(self.heap.alloc(Kind::MutArray, 0, &fields))
            }

            // --- compact regions --------------------------------------------
            //
            // Copying into a region allocates nothing in the heap, so the
            // one `ensure` is for the handle, made before the copy and taken
            // after it -- nothing can move in between.
            Compact => {
                self.ensure(2);
                let region = self.heap.new_region();
                match self.heap.compact_into(region, arg(self, 0)) {
                    Ok(root) => Value::Obj(self.heap.alloc(Kind::Compact, region, &[root])),
                    Err(why) => {
                        self.heap.free_region(region);
                        return err(meadow_core::compact::uncompactable(why.describe()));
                    }
                }
            }
            GetCompact => self.heap.field(self.compact_handle(arg(self, 0))?, 0),
            CompactAdd => {
                self.ensure(2);
                let region = self.heap.meta(self.compact_handle(arg(self, 0))?);
                match self.heap.compact_into(region, arg(self, 1)) {
                    Ok(root) => Value::Obj(self.heap.alloc(Kind::Compact, region, &[root])),
                    Err(why) => return err(meadow_core::compact::uncompactable(why.describe())),
                }
            }
            CompactSize => {
                let region = self.heap.meta(self.compact_handle(arg(self, 0))?);
                Value::Int((self.heap.region_used(region) * crate::heap::SLOT_BYTES) as i64)
            }

            // --- resumptions --------------------------------------------------
            //
            // The flag is its own kind, not a `Ref`, so a resumption refused
            // passage to another thread or into a region is refused as the
            // continuation it is.
            Once => {
                self.ensure(2);
                Value::Obj(self.heap.alloc(Kind::Resume, 0, &[Value::Bool(false)]))
            }
            TakeOnce => match arg(self, 0)
                .addr()
                .filter(|a| self.heap.kind(*a) == Kind::Resume)
            {
                Some(a) if self.heap.field(a, 0) == Value::Bool(false) => {
                    self.heap.set_field(a, 0, Value::Bool(true));
                    Value::Bool(true)
                }
                Some(_) => Value::Bool(false),
                None => return err("takeOnce: expected a resumption's flag"),
            },

            IntAdd | IntSub | IntMul | IntDiv | IntMod | IntEq | IntNe | IntLt | IntLe | IntGt
            | IntGe | FloatAdd | FloatSub | FloatMul | FloatDiv | FloatEq | FloatNe | FloatLt
            | FloatLe | FloatGt | FloatGe => unreachable!("made untyped above"),

            // --- top-level values, evaluated once per thread -------------------
            GlobalReady => {
                let i = self.index(arg(self, 0))?;
                Value::Bool(matches!(self.globals.get(i), Some(Some(_))))
            }
            GlobalGet => match self.globals.get(self.index(arg(self, 0))?) {
                Some(Some(v)) => *v,
                _ => return err("a definition read before it was evaluated"),
            },
            GlobalSet => {
                let i = self.index(arg(self, 0))?;
                if self.globals.len() <= i {
                    self.globals.resize(i + 1, None);
                }
                self.globals[i] = Some(arg(self, 1));
                Value::Unit
            }

            // --- software transactional memory --------------------------------
            StmNew => {
                let world = self.world()?;
                let shared = self.share(arg(self, 0), None)?;
                let id = world.new_tvar(shared);
                self.ensure(1);
                Value::Obj(self.heap.alloc(Kind::TVar, id, &[]))
            }
            StmRead => {
                let id = self.handle(arg(self, 0), Kind::TVar, "a TVar")?;
                let world = self.world()?;
                let read = world.read(self.txn("readTVar")?, id);
                match read {
                    crate::stm::Read::Conflict => {
                        self.ensure(1);
                        let tag = self.ctor_tag("Maybe.None");
                        Value::Obj(self.heap.alloc(Kind::Data, tag, &[]))
                    }
                    crate::stm::Read::Value(shared) => {
                        // Room first: the region is adopted after, so the
                        // collection that making room may run cannot let it go.
                        self.ensure(2);
                        if let Some(r) = &shared.region {
                            self.heap.adopt(r);
                        }
                        let tag = self.ctor_tag("Maybe.Just");
                        Value::Obj(self.heap.alloc(Kind::Data, tag, &[shared.root]))
                    }
                }
            }
            StmWrite => {
                let id = self.handle(arg(self, 0), Kind::TVar, "a TVar")?;
                let world = self.world()?;
                let into = world.region_for_write(self.txn("writeTVar")?, id);
                let shared = self.share(arg(self, 1), Some(into))?;
                self.txn("writeTVar")?.write(id, shared);
                Value::Unit
            }
            StmBegin => {
                self.txn = Some(self.world()?.begin());
                Value::Unit
            }
            StmNest => {
                self.txn("orElse")?.writes.push(Vec::new());
                Value::Unit
            }
            StmMerge => {
                self.txn("orElse")?.merge();
                Value::Unit
            }
            StmRollback => {
                self.txn("orElse")?.rollback();
                Value::Unit
            }
            StmCommit => {
                self.txn("atomically")?;
                self.request = Some(crate::vm::Request::StmCommit { dst });
                Value::Unit
            }
            StmWait => {
                self.txn("retry")?;
                self.request = Some(crate::vm::Request::StmWait { dst });
                Value::Unit
            }

            // --- green threads ------------------------------------------------
            //
            // None of these acts here. Each leaves a request for the scheduler,
            // which owns the channels and the other threads, and puts a
            // placeholder in `dst` that the answer overwrites. Anything that
            // crosses to another thread is exported first, while this thread
            // still has it -- see `meadow_core::thread`.
            ThreadSpawn | ThreadAwait | ThreadYield | ChannelNew | ChannelSend | ChannelReceive
                if !self.scheduled =>
            {
                return err("green threads need the scheduler; this machine is running on its own");
            }
            ThreadSpawn => {
                let body = self.sendable(arg(self, 0))?;
                self.request = Some(crate::vm::Request::Spawn { body, dst });
                Value::Unit
            }
            ThreadAwait => {
                let task = self.handle(arg(self, 0), Kind::Task, "a thread")?;
                self.request = Some(crate::vm::Request::Await { task, dst });
                Value::Unit
            }
            ThreadYield => {
                self.request = Some(crate::vm::Request::Yield);
                Value::Unit
            }
            ChannelNew => {
                self.request = Some(crate::vm::Request::NewChannel { dst });
                Value::Unit
            }
            ChannelSend => {
                let channel = self.handle(arg(self, 0), Kind::Channel, "a channel")?;
                let message = self.sendable(arg(self, 1))?;
                self.request = Some(crate::vm::Request::Send { channel, message });
                Value::Unit
            }
            ChannelReceive => {
                let channel = self.handle(arg(self, 0), Kind::Channel, "a channel")?;
                self.request = Some(crate::vm::Request::Receive { channel, dst });
                Value::Unit
            }
        };

        self.set(dst, out);
        Ok(())
    }
}
