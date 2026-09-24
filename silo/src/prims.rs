//! **The primitives the emitted code does not do inline**, and the effects
//! that reach the world.
//!
//! Ported from `meadow-glade`'s, operation for operation -- the wrapping
//! arithmetic, the zero checks, the clamping in `arraySlice`, the error
//! messages a program can see -- since a difference here is a difference
//! between backends, and the differential tests call it a bug.
//!
//! [`meadow_prim`] is the one entry: the primitive's code
//! (`meadow_core::Prim::code`), and its arguments and their descriptors in
//! memory. **Ownership**, as everywhere in this runtime: arguments are
//! borrowed, and what a primitive answers is owned by the caller -- so a
//! primitive that answers, or stores, a value it was lent shares it first.

use crate::heap::{self, Word};
use crate::show;
use crate::value::{self, Val, val};
use meadow_core::desc;
use meadow_core::{Prim, num};

/// What the runtime keeps for the whole run -- string literals, top-level
/// values -- from which what a leak check does not count is reachable.
pub fn roots() -> Vec<(Word, i64)> {
    // Safety: the running thread's context.
    unsafe { (*crate::ctx::get()).kept.clone() }
}

/// Keep `w` for the rest of the run: see [`roots`].
pub fn keep(w: Word, d: i64) {
    if heap::tracking() {
        // Safety: the running thread's context.
        unsafe { (*crate::ctx::get()).kept.push((w, d)) };
    }
}

fn fail<T>(msg: impl AsRef<str>) -> T {
    crate::fail(msg.as_ref())
}

/// `v`, which was lent, as something the caller owns.
fn lent(v: Val) -> Word {
    let (w, d) = v.bits();
    heap::share(w, d);
    w
}

/// Run primitive `code` on the `n` arguments at `args`, described by `descs`.
///
/// # Safety
///
/// `args` and `descs` must each point at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_prim(
    code: i64,
    n: i64,
    args: *const Word,
    descs: *const i64,
) -> Word {
    // Safety: the caller's.
    let (args, descs) = unsafe {
        (
            std::slice::from_raw_parts(args, n as usize),
            std::slice::from_raw_parts(descs, n as usize),
        )
    };
    let Some(p) = Prim::from_code(code as u16) else {
        return fail(format!("no primitive {code}"));
    };
    if counting() {
        count(p);
    }
    // No allocation for the common case: every primitive but the variadic
    // ones takes three arguments or fewer.
    let mut small = [Val::Unit; 4];
    if args.len() <= small.len() {
        for (i, (w, d)) in args.iter().zip(descs).enumerate() {
            small[i] = val(*w, *d);
        }
        return prim(p, &small[..args.len()], descs);
    }
    let vals: Vec<Val> = args.iter().zip(descs).map(|(w, d)| val(*w, *d)).collect();
    prim(p, &vals, descs)
}

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

fn number(v: Val) -> num::Num {
    value::num(v).unwrap_or_else(|| fail(format!("expected a number, got {}", shown(v))))
}

fn int(v: Val) -> i64 {
    match v {
        Val::Int(i) => i,
        other => fail(format!("expected an Int, got {}", shown(other))),
    }
}

fn index(v: Val) -> usize {
    match int(v) {
        i if i >= 0 => i as usize,
        i => fail(format!("expected a non-negative index, got {i}")),
    }
}

fn shown(v: Val) -> String {
    let (w, d) = v.bits();
    show::show(w, d)
}

fn text(v: Val, what: &str) -> std::borrow::Cow<'static, [u8]> {
    value::text(v).unwrap_or_else(|| fail(format!("{what}: expected a String, got {}", shown(v))))
}

fn string(bytes: &[u8]) -> Word {
    heap::string(bytes)
}

fn ok<T>(r: Result<T, String>) -> T {
    r.unwrap_or_else(|msg| fail(msg))
}

fn num_word(n: num::Num) -> Word {
    value::from_num(n).word()
}

// --- arrays ---------------------------------------------------------------

/// The array `v` is.
fn array(v: Val) -> Word {
    match v {
        Val::Ref(w) if heap::is_block(w) && heap::kind(w) == heap::ARRAY => w,
        other => fail(format!("expected an Array, got {}", shown(other))),
    }
}

fn mut_array(v: Val) -> Word {
    match v {
        Val::Ref(w) if heap::is_block(w) && heap::kind(w) == heap::MUT_ARRAY => w,
        other => fail(format!("expected a mutable array, got {}", shown(other))),
    }
}

/// An array's elements and their one descriptor. The empty array has no
/// block.
pub fn elems(a: Word) -> (Vec<Word>, i64) {
    if !heap::is_block(a) {
        return (Vec::new(), desc::REF);
    }
    let d = heap::field_desc(a, 0);
    ((0..heap::len(a)).map(|i| heap::field(a, i)).collect(), d)
}

/// A new array of `kind` owning `words`, each described by `d`.
pub fn new_array(kind: u64, words: &[Word], d: i64) -> Word {
    let v = heap::build_uniform(kind, 0, words.len(), d);
    for (i, w) in words.iter().enumerate() {
        heap::set_word(v, 2 + i, *w);
    }
    v
}

/// `words`, lent, shared once each.
fn share_all(words: &[Word], d: i64) {
    for w in words {
        heap::share(*w, d);
    }
}

fn array_len(v: Val) -> usize {
    let a = array(v);
    heap::len(a)
}

// --- records --------------------------------------------------------------

/// A record's labels and values: fields `2j` and `2j + 1`.
fn record_get(r: Word, label: usize) -> Option<(Word, i64)> {
    (0..heap::len(r) / 2)
        .find(|j| heap::field(r, 2 * j) as usize == label)
        .map(|j| (heap::field(r, 2 * j + 1), heap::field_desc(r, 2 * j + 1)))
}

// --- the primitives ---------------------------------------------------------

fn prim(p: Prim, a: &[Val], d: &[i64]) -> Word {
    use Prim::*;
    let p = p.untyped();
    let arg = |i: usize| a[i];
    match p {
        // --- numbers ---------------------------------------------------
        Add | Sub | Mul | Div | Mod | Pow => match (arg(0), arg(1)) {
            (Val::Int(x), Val::Int(y)) => {
                (match p {
                    Add => x.wrapping_add(y),
                    Sub => x.wrapping_sub(y),
                    Mul => x.wrapping_mul(y),
                    Div if y == 0 => fail("division by zero"),
                    Div => x.wrapping_div(y),
                    Mod if y == 0 => fail("modulo by zero"),
                    Mod => x.wrapping_rem(y),
                    _ => {
                        let e = u32::try_from(y).unwrap_or_else(|_| {
                            fail(format!("`^` exponent must fit in u32, got {y}"))
                        });
                        x.wrapping_pow(e)
                    }
                }) as Word
            }
            (x, y) => num_word(ok(num::int_arith(arith(p), number(x), number(y)))),
        },
        Lt | Gt | Le | Ge => match (arg(0), arg(1)) {
            (Val::Int(x), Val::Int(y)) => Word::from(match p {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                _ => x >= y,
            }),
            (Val::Char(x), Val::Char(y)) => Word::from(match p {
                Lt => x < y,
                Gt => x > y,
                Le => x <= y,
                _ => x >= y,
            }),
            (x, y) => Word::from(ok(num::int_cmp(cmp(p), number(x), number(y)))),
        },
        Neg => match arg(0) {
            Val::Int(x) => x.wrapping_neg() as Word,
            other => num_word(ok(num::int_neg(number(other)))),
        },
        AddF | SubF | MulF | DivF => num_word(ok(num::float_arith(
            arith(p),
            number(arg(0)),
            number(arg(1)),
        ))),
        LtF | GtF | LeF | GeF => {
            Word::from(ok(num::float_cmp(cmp(p), number(arg(0)), number(arg(1)))))
        }
        ToFloat => ok(num::to_float(number(arg(0)))).to_bits(),
        ToFloat32 => u64::from(ok(num::to_float32(number(arg(0)))).to_bits()),
        Floor => ok(num::floor(number(arg(0)))) as Word,
        ToBig | ToInt | ToWord(_) => {
            let target = match p {
                ToBig => num::IntTarget::Big,
                ToInt => num::IntTarget::Int,
                ToWord(w) => num::IntTarget::Word(w),
                _ => unreachable!("matched above"),
            };
            num_word(ok(num::to_int(target, number(arg(0)))))
        }
        Shl | Shr | Ushr | BitAnd | BitOr | BitXor => match (arg(0), arg(1)) {
            (Val::Int(x), Val::Int(y)) => {
                (match p {
                    Shl => x.wrapping_shl(y as u32),
                    Shr => x.wrapping_shr(y as u32),
                    Ushr => (x as u64).wrapping_shr(y as u32) as i64,
                    BitAnd => x & y,
                    BitOr => x | y,
                    _ => x ^ y,
                }) as Word
            }
            (x, y) => num_word(ok(num::int_bits(bits(p), number(x), number(y)))),
        },
        BitNot => num_word(ok(num::int_not(number(arg(0))))),
        PopCount => ok(num::pop_count(number(arg(0)))) as Word,
        BitWidth => ok(num::bit_width(&number(arg(0)))) as Word,

        // --- structural ------------------------------------------------
        Eq => Word::from(equal(arg(0), arg(1))),
        Ne => Word::from(!equal(arg(0), arg(1))),
        Show => string(shown(arg(0)).as_bytes()),
        Display => string(show::displayed(a[0].word(), d[0]).as_bytes()),
        Hash => hash(arg(0)) as Word,

        // --- the builtin Array --------------------------------------------
        ArrayLen => array_len(arg(0)) as Word,
        ArrayGet => {
            let arr = array(arg(0));
            let i = index(arg(1));
            let n = heap::len(arr);
            if i >= n {
                return fail(format!("arrayGet: index {i} out of bounds (len {n})"));
            }
            let x = heap::field(arr, i);
            heap::share(x, heap::field_desc(arr, i));
            x
        }
        ArrayGetOr => {
            let arr = array(arg(1));
            let i = index(arg(2));
            if i < heap::len(arr) {
                let x = heap::field(arr, i);
                heap::share(x, heap::field_desc(arr, i));
                x
            } else {
                lent(arg(0))
            }
        }
        ArraySet => {
            let (mut words, ed) = elems(array(arg(0)));
            let i = index(arg(1));
            if i >= words.len() {
                return fail(format!(
                    "arraySet: index {i} out of bounds (len {})",
                    words.len()
                ));
            }
            let (v, vd) = arg(2).bits();
            share_all(&words, ed);
            heap::erase(words[i], ed);
            heap::share(v, vd);
            words[i] = v;
            new_array(heap::ARRAY, &words, vd)
        }
        ArrayPush => {
            let (mut words, ed) = elems(array(arg(0)));
            share_all(&words, ed);
            let (v, vd) = arg(1).bits();
            heap::share(v, vd);
            words.push(v);
            new_array(heap::ARRAY, &words, vd)
        }
        ArrayPop => {
            let (mut words, ed) = elems(array(arg(0)));
            if words.is_empty() {
                return fail("arrayPop: empty array");
            }
            words.pop();
            share_all(&words, ed);
            new_array(heap::ARRAY, &words, ed)
        }
        ArraySlice => {
            // Only the slice is read: taking the whole array out first made
            // cutting a big one into pieces quadratic.
            let a = array(arg(0));
            let n = heap::len(a) as i64;
            let ed = heap::field_desc(a, 0);
            let from = int(arg(1)).clamp(0, n) as usize;
            let to = int(arg(2)).clamp(from as i64, n) as usize;
            let part: Vec<Word> = (from..to).map(|i| heap::field(a, i)).collect();
            share_all(&part, ed);
            new_array(heap::ARRAY, &part, ed)
        }
        ArrayConcat => {
            let (x, dx) = elems(array(arg(0)));
            let (y, dy) = elems(array(arg(1)));
            if x.is_empty() {
                return lent(arg(1));
            }
            if y.is_empty() {
                return lent(arg(0));
            }
            share_all(&x, dx);
            share_all(&y, dy);
            let mut all = x;
            all.extend(y);
            new_array(heap::ARRAY, &all, dx)
        }

        // --- text and bytes ----------------------------------------------
        StringToBytes => {
            let bytes = text(arg(0), "stringToBytes");
            let words: Vec<Word> = bytes.iter().map(|b| Word::from(*b)).collect();
            new_array(heap::ARRAY, &words, desc::word(num::Width::U8))
        }
        BytesToString => {
            let buf = byte_array(arg(0), "bytesToString");
            string(String::from_utf8_lossy(&buf).as_bytes())
        }
        BytesToHex => {
            let buf = byte_array(arg(0), "bytesToHex");
            let mut s = String::with_capacity(buf.len() * 2);
            for b in buf {
                s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble"));
                s.push(char::from_digit((b & 0xf) as u32, 16).expect("nibble"));
            }
            string(s.as_bytes())
        }
        BytesFromHex => {
            let bytes = text(arg(0), "bytesFromHex");
            let mut out = Vec::with_capacity(bytes.len() / 2);
            let mut good = bytes.len() % 2 == 0;
            if good {
                for pair in bytes.chunks_exact(2) {
                    match (
                        (pair[0] as char).to_digit(16),
                        (pair[1] as char).to_digit(16),
                    ) {
                        (Some(h), Some(l)) => out.push(Word::from((h << 4) | l)),
                        _ => {
                            good = false;
                            break;
                        }
                    }
                }
            }
            if good {
                let arr = new_array(heap::ARRAY, &out, desc::word(num::Width::U8));
                value::data("Maybe.Just", &[Val::Ref(arr)])
            } else {
                value::data("Maybe.None", &[])
            }
        }
        CharCode => match arg(0) {
            Val::Char(c) => c as Word,
            other => fail(format!("charCode: expected a Char, got {}", shown(other))),
        },
        CharFromCode => match arg(0) {
            Val::Int(n) => match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => c as Word,
                None => fail(format!("charFromCode: {n} is not a Unicode scalar value")),
            },
            other => fail(format!(
                "charFromCode: expected an Int, got {}",
                shown(other)
            )),
        },
        StringToChars => {
            let t = text(arg(0), "stringToChars");
            let words: Vec<Word> = String::from_utf8_lossy(&t)
                .chars()
                .map(|c| c as Word)
                .collect();
            new_array(heap::ARRAY, &words, desc::CHAR)
        }
        CharsToString => {
            let (words, ed) = elems(array(arg(0)));
            if !words.is_empty() && ed != desc::CHAR {
                return fail("charsToString: expected an Array of Char");
            }
            let s: String = words
                .iter()
                .map(|w| char::from_u32(*w as u32).unwrap_or('\u{fffd}'))
                .collect();
            string(s.as_bytes())
        }
        ConcatStrings => {
            let (words, ed) = elems(array(arg(0)));
            let mut out = Vec::new();
            for w in words {
                out.extend_from_slice(&text(val(w, ed), "concatStrings"));
            }
            string(&out)
        }
        StringByteLength => text(arg(0), "stringByteLength").len() as Word,
        StringByteAt => {
            let t = text(arg(0), "stringByteAt");
            let i = int(arg(1));
            match usize::try_from(i).ok().filter(|at| *at < t.len()) {
                Some(at) => Word::from(t[at]),
                None => fail(format!(
                    "stringByteAt: index {i} out of bounds (len {})",
                    t.len()
                )),
            }
        }
        StringSlice => {
            let t = text(arg(0), "stringSlice");
            string(meadow_core::text::slice(&t, int(arg(1)), int(arg(2))).as_bytes())
        }
        StringCompare => meadow_core::text::compare(
            &text(arg(0), "stringCompare"),
            &text(arg(1), "stringCompare"),
        ) as Word,
        StringIndexOf => meadow_core::text::index_of(
            &text(arg(0), "stringIndexOf"),
            &text(arg(1), "stringIndexOf"),
            int(arg(2)),
        ) as Word,

        // --- the mutable cell ----------------------------------------------
        NewRef => {
            let (v, vd) = arg(0).bits();
            heap::share(v, vd);
            heap::build(heap::CELL, 0, &[v], &[vd])
        }
        GetRef => match arg(0).block(heap::CELL) {
            Some(r) => {
                let x = heap::field(r, 0);
                heap::share(x, heap::field_desc(r, 0));
                x
            }
            None => fail(format!("getRef: expected a Ref, got {}", shown(arg(0)))),
        },
        SetRef => match arg(0).block(heap::CELL) {
            Some(r) => {
                let (v, vd) = arg(1).bits();
                heap::share(v, vd);
                heap::erase(heap::field(r, 0), heap::field_desc(r, 0));
                heap::set_field(r, 0, v, vd);
                0
            }
            None => fail(format!("setRef: expected a Ref, got {}", shown(arg(0)))),
        },

        // --- the mutable array ----------------------------------------------
        RunSt => fail("runSt reached the runtime; lowering applies its body"),
        StNewArray => {
            let n = match int(arg(0)) {
                n if n >= 0 => n as usize,
                n => fail(format!(
                    "stNewArray: expected a length of zero or more, got {n}"
                )),
            };
            let (v, vd) = arg(1).bits();
            for _ in 0..n {
                heap::share(v, vd);
            }
            new_array(heap::MUT_ARRAY, &vec![v; n], vd)
        }
        StGetArray => {
            let arr = mut_array(arg(0));
            let i = index(arg(1));
            let n = heap::len(arr);
            if i >= n {
                return fail(format!("stGetArray: index {i} out of bounds (len {n})"));
            }
            let x = heap::field(arr, i);
            heap::share(x, heap::field_desc(arr, i));
            x
        }
        StSetArray => {
            let arr = mut_array(arg(0));
            let i = index(arg(1));
            let n = heap::len(arr);
            if i >= n {
                return fail(format!("stSetArray: index {i} out of bounds (len {n})"));
            }
            let (v, vd) = arg(2).bits();
            heap::share(v, vd);
            heap::erase(heap::field(arr, i), heap::field_desc(arr, i));
            heap::set_word(arr, 2 + i, v);
            0
        }
        StArrayLen => heap::len(mut_array(arg(0))) as Word,
        StFreeze => {
            let arr = mut_array(arg(0));
            let (words, ed) = elems(arr);
            share_all(&words, ed);
            new_array(heap::ARRAY, &words, ed)
        }
        StThaw => {
            let (words, ed) = elems(array(arg(0)));
            share_all(&words, ed);
            new_array(heap::MUT_ARRAY, &words, ed)
        }

        // --- compact regions ------------------------------------------------
        //
        // A region is memory outside every heap, holding a copy of what was
        // put in it, and the rest of the runtime treats a compact as one
        // object: one count, freed all at once, never looked inside. See
        // `crate::region`, which holds all of it.
        //
        // The value is borrowed here, as a primitive's arguments are, and the
        // region holds a copy -- so nothing is shared and what was compacted
        // is free to go.
        Compact => {
            let (v, vd) = arg(0).bits();
            match crate::region::compact(v, vd) {
                Ok(c) => c,
                Err(why) => fail(why),
            }
        }
        GetCompact => match arg(0).block(heap::COMPACT) {
            Some(c) => {
                let x = heap::field(c, 0);
                heap::share(x, heap::field_desc(c, 0));
                x
            }
            None => fail(format!("expected a Compact, got {}", shown(arg(0)))),
        },
        CompactAdd => match arg(0).block(heap::COMPACT) {
            // Safety: a `Compact` block, which `block` checked.
            Some(c) => {
                let (v, vd) = arg(1).bits();
                match unsafe { crate::region::add(c, v, vd) } {
                    Ok(c) => c,
                    Err(why) => fail(why),
                }
            }
            None => fail(format!("expected a Compact, got {}", shown(arg(0)))),
        },
        CompactSize => match arg(0).block(heap::COMPACT) {
            // Safety: as above.
            Some(c) => (unsafe { crate::region::bytes(c) }) as Word,
            None => fail(format!("expected a Compact, got {}", shown(arg(0)))),
        },

        // --- resumptions: the one-shot flag ------------------------------------
        Once => heap::build(heap::ONCE, 0, &[0], &[desc::BOOL]),
        TakeOnce => match arg(0).block(heap::ONCE) {
            Some(f) if heap::field(f, 0) == 0 => {
                heap::set_word(f, heap::first_field(f), 1);
                1
            }
            Some(_) => 0,
            None => fail("takeOnce: expected a resumption's flag"),
        },
        Enter | Detach | Reattach => crate::segments::prim(p, a),

        // --- top-level values ---------------------------------------------------
        GlobalReady => Word::from(globals(|g| matches!(g.get(index(arg(0))), Some(Some(_))))),
        GlobalGet => match globals(|g| g.get(index(arg(0))).copied().flatten()) {
            Some((w, wd)) => {
                heap::share(w, wd);
                w
            }
            None => fail("a definition read before it was evaluated"),
        },
        GlobalSet => {
            let i = index(arg(0));
            let (v, vd) = arg(1).bits();
            heap::share(v, vd);
            keep(v, vd);
            globals(|g| {
                if g.len() <= i {
                    g.resize(i + 1, None);
                }
                g[i] = Some((v, vd));
            });
            0
        }

        // --- tail recursion modulo cons ----------------------------------------
        //
        // A constructor's hole filled in place. The object now holds a
        // reference to the value, and none to the placeholder; and the field is
        // described as the value is.
        SetField => {
            let i = index(arg(1));
            match arg(0).block(heap::DATA) {
                Some(obj) if i < heap::len(obj) => {
                    let (v, vd) = arg(2).bits();
                    heap::erase(heap::field(obj, i), heap::field_desc(obj, i));
                    heap::share(v, vd);
                    heap::set_field(obj, i, v, vd);
                    0
                }
                _ => fail(format!(
                    "setField: expected a constructor with a field {i}, got {}",
                    shown(arg(0))
                )),
            }
        }

        // --- threads and transactions ---------------------------------------------
        ThreadSpawn | ThreadAwait | ThreadYield | ChannelNew | ChannelSend | ChannelReceive
        | StmNew | StmRead | StmWrite | StmBegin | StmCommit | StmWait | StmNest | StmMerge
        | StmRollback => crate::sched::prim(p, a),

        IntAdd | IntSub | IntMul | IntDiv | IntMod | IntEq | IntNe | IntLt | IntLe | IntGt
        | IntGe | FloatAdd | FloatSub | FloatMul | FloatDiv | FloatEq | FloatNe | FloatLt
        | FloatLe | FloatGt | FloatGe => unreachable!("made untyped above"),
    }
}

/// The bytes of an `Array UInt8`.
pub fn byte_array(v: Val, what: &str) -> Vec<u8> {
    let (words, ed) = elems(array(v));
    words
        .iter()
        .map(|w| match val(*w, ed) {
            Val::Word(num::Width::U8, b) => b as u8,
            Val::Int(n) if (0..=255).contains(&n) => n as u8,
            other => fail(format!("`{what}`: not a byte: {}", shown(other))),
        })
        .collect()
}

/// This thread's top-level values: each thread computes its own (`crate::ctx`).
fn globals<T>(f: impl FnOnce(&mut Vec<Option<(Word, i64)>>) -> T) -> T {
    // Safety: the running thread's context.
    f(unsafe { &mut (*crate::ctx::get()).globals })
}

// --- equality and hashing -------------------------------------------------

fn is_number(v: Val) -> bool {
    matches!(
        v,
        Val::Int(_) | Val::Word(..) | Val::Float(_) | Val::Float32(_)
    )
}

/// Structural equality, as `==` on values of any type means it.
pub fn equal(a: Val, b: Val) -> bool {
    let mut stack = vec![(a, b)];
    while let Some((a, b)) = stack.pop() {
        match (a, b) {
            (Val::Int(x), Val::Int(y)) if x == y => {}
            (Val::Float(x), Val::Float(y)) if x == y => {}
            (x, y) if is_number(x) || is_number(y) => match (value::num(x), value::num(y)) {
                (Some(p), Some(q)) if num::num_eq(&p, &q) => {}
                _ => return false,
            },
            (Val::Bool(x), Val::Bool(y)) if x == y => {}
            (Val::Char(x), Val::Char(y)) if x == y => {}
            (Val::Sym(x), Val::Sym(y)) if x == y => {}
            (Val::Unit, Val::Unit) => {}
            (Val::Ref(x), Val::Ref(y)) => {
                if x == y {
                    continue;
                }
                // A vector is equal to another of the same elements, whatever
                // the shape of the trees holding them.
                if let (Some(xs), Some(ys)) = (show::vector_elems(x), show::vector_elems(y)) {
                    if xs.len() != ys.len() {
                        return false;
                    }
                    stack.extend(
                        xs.into_iter()
                            .zip(ys)
                            .map(|((p, dp), (q, dq))| (val(p, dp), val(q, dq))),
                    );
                    continue;
                }
                if !heap::is_block(x) || !heap::is_block(y) {
                    // An empty array has no block; neither does a nullary
                    // constructor, which is equal only to itself.
                    let empty = |w: Word| !heap::is_block(w) || heap::len(w) == 0;
                    let arrays = |w: Word| !heap::is_block(w) || heap::kind(w) == heap::ARRAY;
                    if empty(x) && empty(y) && arrays(x) && arrays(y) && x & 1 == 0 && y & 1 == 0 {
                        continue;
                    }
                    return false;
                }
                let (kx, ky) = (heap::kind(x), heap::kind(y));
                if kx != ky {
                    return false;
                }
                match kx {
                    heap::STRING => {
                        if heap::bytes(x) != heap::bytes(y) {
                            return false;
                        }
                    }
                    heap::BIGINT => {
                        if heap::meta(x) != heap::meta(y)
                            || heap::len(x) != heap::len(y)
                            || (0..heap::len(x)).any(|i| heap::field(x, i) != heap::field(y, i))
                        {
                            return false;
                        }
                    }
                    heap::RECORD => {
                        if heap::len(x) != heap::len(y) {
                            return false;
                        }
                        for j in 0..heap::len(x) / 2 {
                            let label = heap::field(x, 2 * j) as usize;
                            match record_get(y, label) {
                                Some((w, wd)) => stack.push((
                                    val(heap::field(x, 2 * j + 1), heap::field_desc(x, 2 * j + 1)),
                                    val(w, wd),
                                )),
                                None => return false,
                            }
                        }
                    }
                    heap::CLOSURE | heap::CELL | heap::MUT_ARRAY | heap::ONCE => return false,
                    // By what they hold, not by the region they share.
                    heap::COMPACT => stack.push((
                        val(heap::field(x, 0), heap::field_desc(x, 0)),
                        val(heap::field(y, 0), heap::field_desc(y, 0)),
                    )),
                    _ => {
                        if heap::meta(x) != heap::meta(y) || heap::len(x) != heap::len(y) {
                            return false;
                        }
                        for i in 0..heap::len(x) {
                            stack.push((
                                val(heap::field(x, i), heap::field_desc(x, i)),
                                val(heap::field(y, i), heap::field_desc(y, i)),
                            ));
                        }
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

/// A structural hash, consistent with [`equal`]: `meadow_core::hash`'s, which
/// every backend uses.
pub fn hash(v: Val) -> i64 {
    use meadow_core::hash::{Hasher, unhashable};
    enum Work {
        Val(Val),
        Label(String),
    }
    let mut h = Hasher::new();
    let mut stack = vec![Work::Val(v)];
    while let Some(w) = stack.pop() {
        let v = match w {
            Work::Label(l) => {
                h.str(&l);
                continue;
            }
            Work::Val(v) => v,
        };
        match v {
            Val::Int(_) | Val::Word(..) | Val::Float(_) | Val::Float32(_) => {
                num::hash_into(&mut h, &number(v));
            }
            Val::Bool(b) => h.bool(b),
            Val::Char(c) => h.char(c),
            Val::Sym(s) => h.str(show::names().syms.get(s).map_or("", |x| x.as_str())),
            Val::Unit => h.unit(),
            Val::Ref(x) => {
                if let Some(xs) = show::vector_elems(x) {
                    h.vector(xs.len());
                    stack.extend(xs.into_iter().rev().map(|(p, d)| Work::Val(val(p, d))));
                    continue;
                }
                if x & 1 == 1 {
                    let name = show::ctor_name((x >> 1) as usize);
                    h.data(&name, 0);
                    continue;
                }
                if x == 0 {
                    return fail(unhashable("a function"));
                }
                let n = heap::len(x);
                let field = |i: usize| val(heap::field(x, i), heap::field_desc(x, i));
                match heap::kind(x) {
                    heap::STRING => {
                        let bytes = heap::bytes(x);
                        h.str_packed(bytes.len(), (0..heap::len(x)).map(|i| heap::field(x, i)));
                    }
                    heap::BIGINT => {
                        num::hash_into(&mut h, &number(v));
                    }
                    heap::DATA => {
                        let name = show::ctor_name(heap::meta(x) as usize);
                        h.data(&name, n);
                        stack.extend((0..n).rev().map(|i| Work::Val(field(i))));
                    }
                    heap::ARRAY => {
                        h.array(n);
                        stack.extend((0..n).rev().map(|i| Work::Val(field(i))));
                    }
                    heap::RECORD => {
                        h.record(n / 2);
                        let mut sorted: Vec<(String, Val)> = (0..n / 2)
                            .map(|j| {
                                let label = show::names()
                                    .syms
                                    .get(heap::field(x, 2 * j) as usize)
                                    .cloned()
                                    .unwrap_or_default();
                                (label, field(2 * j + 1))
                            })
                            .collect();
                        sorted.sort_by(|p, q| p.0.cmp(&q.0));
                        for (label, value) in sorted.into_iter().rev() {
                            stack.push(Work::Val(value));
                            stack.push(Work::Label(label));
                        }
                    }
                    heap::COMPACT => {
                        h.compact();
                        stack.push(Work::Val(field(0)));
                    }
                    heap::CELL => return fail(unhashable("a Ref")),
                    heap::MUT_ARRAY => return fail(unhashable("a mutable array")),
                    heap::CHANNEL => return fail(unhashable("a channel")),
                    heap::TASK => return fail(unhashable("a thread")),
                    heap::TVAR => return fail(unhashable("a TVar")),
                    _ => return fail(unhashable("a function")),
                }
            }
        }
    }
    h.finish()
}

// --- the primitives the emitted code calls straight -------------------------
//
// Everything above goes through [`meadow_prim`], which takes its arguments in
// memory and decodes them. For the few that a program does millions of times,
// that marshalling is most of the cost, so the emitted code calls these
// instead, with the values in registers. See `meadow_llvm::emit::direct`.

/// `hash v`, where `d` describes `v`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_hash(v: Word, d: i64) -> Word {
    hash(val(v, d)) as Word
}

/// `a == b`, structurally.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_equal(a: Word, ad: i64, b: Word, bd: i64) -> Word {
    Word::from(equal(val(a, ad), val(b, bd)))
}

/// `stringIndexOf s sub from`: where `sub` next appears in `s`, or -1.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_string_index_of(s: Word, sub: Word, from: Word) -> Word {
    meadow_core::text::index_of(
        &text(Val::Ref(s), "stringIndexOf"),
        &text(Val::Ref(sub), "stringIndexOf"),
        from as i64,
    ) as Word
}

/// `stringSlice s from to`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_string_slice(s: Word, from: Word, to: Word) -> Word {
    let t = text(Val::Ref(s), "stringSlice");
    string(meadow_core::text::slice(&t, from as i64, to as i64).as_bytes())
}

/// `getRef r`: what the cell holds, shared.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_get_ref(r: Word) -> Word {
    match Val::Ref(r).block(heap::CELL) {
        Some(c) => {
            let x = heap::field(c, 0);
            heap::share(x, heap::field_desc(c, 0));
            x
        }
        None => fail(format!(
            "getRef: expected a Ref, got {}",
            shown(Val::Ref(r))
        )),
    }
}

/// `stSetArray a i x`, where `d` describes `x`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_st_set(a: Word, i: Word, x: Word, d: i64) -> Word {
    let arr = mut_array(Val::Ref(a));
    let i = i as usize;
    let n = heap::len(arr);
    if i >= n {
        return fail(format!("stSetArray: index {i} out of bounds (len {n})"));
    }
    heap::share(x, d);
    heap::erase(heap::field(arr, i), heap::field_desc(arr, i));
    heap::set_word(arr, 2 + i, x);
    0
}

/// Whether the array at `a` can be written in place: nobody else holds it, and
/// it is not in a region, whose blocks are never counted.
fn unshared(a: Word) -> bool {
    heap::word(a, 0) & 0xFFFF_FFFF == 0
}

/// Set the length of the array at `a` to `n`, keeping its count, and its
/// elements' descriptor to `d`.
fn set_array_shape(a: Word, n: usize, d: i64) {
    heap::set_word(a, 0, ((n as Word) << 32) | (heap::word(a, 0) & 0xFFFF_FFFF));
    let w1 = heap::word(a, 1);
    let bits = (w1 & !(15 << heap::DESC_SHIFT)) | (((d as u64) & 15) << heap::DESC_SHIFT);
    heap::set_word(a, 1, bits);
}

/// `arrayPush a x`, **consuming** `a`: the linearization gives this entry its
/// own reference to the array, sharing it first where the caller still wants
/// the old one. So when the count says there is no other reference, nobody can
/// see the array change, and `x` goes into the room [`heap::array_room`] left
/// at its end. Only a push that crosses into the next size copies, which makes
/// building an array a push at a time linear rather than quadratic.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_array_push(a: Word, x: Word, d: i64) -> Word {
    let arr = array(Val::Ref(a));
    let n = heap::len(arr);
    heap::share(x, d);
    if unshared(arr) && heap::array_room(n + 1) == heap::array_room(n) {
        heap::set_word(arr, 2 + n, x);
        set_array_shape(arr, n + 1, d);
        return arr;
    }
    let (mut words, ed) = elems(arr);
    share_all(&words, ed);
    words.push(x);
    let v = new_array(heap::ARRAY, &words, d);
    heap::erase(arr, desc::REF);
    v
}

/// `arrayConcat a b`, consuming `a` as [`meadow_array_push`] does and lending
/// `b`: `b`'s elements go into `a`'s room when it has enough.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_array_concat(a: Word, b: Word) -> Word {
    let x = array(Val::Ref(a));
    let y = array(Val::Ref(b));
    let (n, m) = (heap::len(x), heap::len(y));
    if m == 0 {
        return x;
    }
    let dy = heap::field_desc(y, 0);
    for i in 0..m {
        heap::share(heap::field(y, i), dy);
    }
    if n == 0 {
        heap::erase(x, desc::REF);
        return new_array(heap::ARRAY, &elems(y).0, dy);
    }
    if unshared(x) && heap::array_room(n + m) == heap::array_room(n) {
        for i in 0..m {
            heap::set_word(x, 2 + n + i, heap::field(y, i));
        }
        set_array_shape(x, n + m, heap::field_desc(x, 0));
        return x;
    }
    let (mut words, dx) = elems(x);
    share_all(&words, dx);
    words.extend(elems(y).0);
    let v = new_array(heap::ARRAY, &words, dx);
    heap::erase(x, desc::REF);
    v
}
// --- records, arrays and fields the emitted code builds ------------------------

/// A record of the `n` values at `args`, described by `descs`, labelled by
/// the symbols at `labels` -- already in the order records keep. Owns the
/// values.
///
/// # Safety
///
/// Each pointer must point at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_record(
    n: i64,
    args: *const Word,
    descs: *const i64,
    labels: *const i64,
) -> Word {
    let n = n as usize;
    // Safety: the caller's.
    let (args, descs, labels) = unsafe {
        (
            std::slice::from_raw_parts(args, n),
            std::slice::from_raw_parts(descs, n),
            std::slice::from_raw_parts(labels, n),
        )
    };
    let mut words = Vec::with_capacity(2 * n);
    let mut ds = Vec::with_capacity(2 * n);
    for i in 0..n {
        heap::share(args[i], descs[i]);
        words.push(labels[i] as Word);
        ds.push(desc::STR);
        words.push(args[i]);
        ds.push(descs[i]);
    }
    heap::build(heap::RECORD, 0, &words, &ds)
}

/// `r.label`: of a record, or of a `record` type's data, whose fields the
/// program's table names.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_select(r: Word, label: i64) -> Word {
    let label = label as usize;
    if heap::is_block(r) && heap::kind(r) == heap::RECORD {
        if let Some((w, d)) = record_get(r, label) {
            heap::share(w, d);
            return w;
        }
    }
    if heap::is_block(r) && heap::kind(r) == heap::DATA {
        let name = show::names().syms.get(label).cloned().unwrap_or_default();
        if let Some(fields) = show::ctor_fields(heap::meta(r) as usize)
            && let Some(i) = fields.iter().position(|f| *f == name)
            && i < heap::len(r)
        {
            let w = heap::field(r, i);
            heap::share(w, heap::field_desc(r, i));
            return w;
        }
    }
    let name = show::names().syms.get(label).cloned().unwrap_or_default();
    fail(format!("no field `{name}` in {}", show::show(r, desc::REF)))
}

/// `{ r | label = v }`: a new record, `label` replaced or added in its place.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_extend(r: Word, label: i64, v: Word, vd: i64) -> Word {
    if !heap::is_block(r) || heap::kind(r) != heap::RECORD {
        return fail(format!(
            "expected a record, got {}",
            show::show(r, desc::REF)
        ));
    }
    let rank = |l: Word| show::sym_rank(l as usize);
    let mut pairs: Vec<(Word, Word, i64)> = (0..heap::len(r) / 2)
        .map(|j| {
            (
                heap::field(r, 2 * j),
                heap::field(r, 2 * j + 1),
                heap::field_desc(r, 2 * j + 1),
            )
        })
        .filter(|(l, _, _)| *l != label as Word)
        .collect();
    for (_, w, d) in &pairs {
        heap::share(*w, *d);
    }
    heap::share(v, vd);
    pairs.push((label as Word, v, vd));
    pairs.sort_by_key(|(l, _, _)| rank(*l));
    let mut words = Vec::with_capacity(2 * pairs.len());
    let mut ds = Vec::with_capacity(2 * pairs.len());
    for (l, w, d) in pairs {
        words.push(l);
        ds.push(desc::STR);
        words.push(w);
        ds.push(d);
    }
    heap::build(heap::RECORD, 0, &words, &ds)
}

/// An array of the `n` values at `args`, all described alike by the first of
/// `descs`. Owns them.
///
/// # Safety
///
/// Each pointer must point at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_array(n: i64, args: *const Word, descs: *const i64) -> Word {
    let n = n as usize;
    if n == 0 {
        return new_array(heap::ARRAY, &[], desc::REF);
    }
    // Safety: the caller's.
    let (args, descs) = unsafe {
        (
            std::slice::from_raw_parts(args, n),
            std::slice::from_raw_parts(descs, n),
        )
    };
    share_all(args, descs[0]);
    new_array(heap::ARRAY, args, descs[0])
}

// --- counting what goes through here ---------------------------------------
//
// `MEADOW_SILO_PRIMS=1`: at exit, how many times each primitive was called
// through `meadow_prim` rather than done inline -- what to inline next.

fn counting() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MEADOW_SILO_PRIMS").is_some())
}

static COUNTS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

fn count(p: Prim) {
    let mut c = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let i = p.code() as usize;
    if c.len() <= i {
        c.resize(i + 1, 0);
    }
    c[i] += 1;
}

/// The counts, most first, when counting.
pub fn report() {
    if !counting() {
        return;
    }
    let c = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut all: Vec<(u64, Prim)> = c
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .filter_map(|(i, n)| Some((*n, Prim::from_code(i as u16)?)))
        .collect();
    all.sort_by(|a, b| b.0.cmp(&a.0));
    for (n, p) in all.iter().take(20) {
        eprintln!("aot: {n:>12}  {p:?}");
    }
}
