//! **Core: a typed lambda calculus that every front-end feature lowers to.**
//!
//! Inference runs on the HIR; once a program type-checks we translate it here.
//! Core is deliberately tiny — single-argument lambdas and applications,
//! explicit recursion, primitives already resolved, pattern matching reduced to
//! `Case` plus tuple and record projection.
//!
//! # Why it is typed
//!
//! It is **System F**, near enough: every binder carries its type, every
//! generalized binding is a [`Term::TyLam`], and every mention of a
//! polymorphic name is a [`Term::TyApp`] carrying the types it was
//! instantiated at. So each term has exactly one type, computable in one
//! bottom-up pass with nothing to infer — which is what makes [`lint`] cheap
//! and total, and what makes an optimisation pass *checkable*.
//!
//! That is the whole reason for the types. An inlining that substitutes the
//! wrong type, a specialization that drops a type argument, a `case` rebuilt
//! with an arm of the wrong type — each is a miscompile that runs, and runs
//! wrong. GHC's answer is a typed core and `-dcore-lint`; this is the same
//! answer, and the lint runs after every pass in a debug build and in the
//! whole test suite.
//!
//! Two conventions are worth knowing before reading the types:
//!
//! * A core type is an [`meadow_infer::Type`] in which **`Var(n)` is a rigid
//!   type variable** — bound by an enclosing `TyLam`, never solved by
//!   anything. `Bound` does not appear; a core polytype ([`Poly`]) writes its
//!   binders down instead of numbering them.
//! * [`unknown`] is the type of something the compiler could not type, which
//!   only happens in a unit that already has errors. The lint accepts it
//!   anywhere.
//!
//! # What is downstream
//!
//! Nothing. [`erase`] drops the type abstractions on the way out, and AxCut,
//! the bytecode machine and the CEK evaluator are all untyped: none of them
//! can ask a question a type would answer.

pub mod args;
pub mod bools;
pub mod compact;
pub mod desc;
pub mod erase;
pub mod globals;
pub mod hash;
pub mod joins;
pub mod lint;
pub mod lower;
pub mod num;
pub mod prune;
pub mod rewrite;
pub mod specialize;
pub mod stm;
pub mod text;
pub mod thread;
pub use lower::Lowerer;

use meadow_hir as hir;
use meadow_infer::{Generalized, Scheme, Type as InferType, TypeTable, VarKind, VariantEnv};
use meadow_intern::InternedString;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Lit {
    /// Fixed-width integer (`Int`, i.e. i64).
    Int(i64),
    /// An integer literal whose inferred type is `BigInt` (context coerced it) —
    /// widened to an arbitrary-precision value at runtime.
    BigInt(i64),
    Float(f64),
    /// An integer literal whose inferred type is a sized one -- `UInt8`,
    /// `Int16`, … -- already wrapped to it.
    Word(num::Width, u64),
    /// A float literal whose inferred type is `Float32`.
    Float32(f32),
    /// An integer literal whose type is still a variable -- inside a function
    /// generic over its integer type, `fun succ n = n + 1` -- and that variable.
    /// [`specialize`] makes it the type a copy of the function is for; where
    /// none can be known it is an `Int` at run time, and the primitives let it
    /// take the type of what it meets ([`num`]). The core checker lets it stand
    /// for any type.
    AnyInt(i64, u32),
    /// The same for a float literal: a `Float` where no type is known.
    AnyFloat(f64, u32),
    Str(InternedString),
    /// An interned name, compared by its word rather than its text: the key
    /// an effect operation is dispatched on. No program writes one -- only
    /// lowering to the bytecode machine makes them -- and its type is
    /// [`desc::SYMBOL_TYPE`], not `String`.
    Sym(InternedString),
    Char(char),
    Bool(bool),
    Unit,
}

/// Render a float the way Meadow prints it — always with a fractional part, so it
/// reads as a float and not an int (`1` becomes `1.0`). Shared by the core
/// pretty-printer and the evaluator's `Value` display.
pub fn fmt_float(x: f64) -> String {
    if x.is_finite() && x == x.trunc() {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Prim {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Neg,
    /// A value as text the way output shows it: a `String` as itself,
    /// anything else as `show` renders it. What `print` and `println` write.
    Display,
    /// A structural hash, consistent with `==` -- see [`hash`].
    Hash,
    // --- floating point: `Float` and `Float32` ---
    AddF,
    SubF,
    MulF,
    DivF,
    LtF,
    GtF,
    LeF,
    GeF,
    /// `toFloat : n -> Float` for any integer type, and `toFloat64 : f -> Float`
    /// for either float type -- one conversion at run time.
    ToFloat,
    /// `toFloat32 : f -> Float32` for either float type.
    ToFloat32,
    /// `floor : f -> Int` (round toward negative infinity)
    Floor,
    // --- integer conversions ---
    /// `toBigInt : n -> BigInt` for any integer type.
    ToBig,
    /// `toInt : n -> Int` for any integer type, keeping the low 64 bits.
    ToInt,
    /// `toInt8 : n -> Int8` and the rest of the sized types, keeping the low
    /// bits likewise.
    ToWord(num::Width),
    /// `bitWidth : n -> Int` -- 64 for `Int`, 8 for `UInt8`; an error for `BigInt`.
    BitWidth,
    // --- the builtin `Array` (a persistent, `Arc`-shared contiguous buffer) ---
    /// `Array a -> Int`
    ArrayLen,
    /// `Array a -> Int -> a` — runtime error if out of bounds.
    ArrayGet,
    /// `a -> Array a -> Int -> a` — total: the default is returned out of bounds.
    ArrayGetOr,
    /// `Array a -> Int -> a -> Array a` — persistent update.
    ArraySet,
    /// `Array a -> a -> Array a` — append one element.
    ArrayPush,
    /// `Array a -> Array a` — drop the last element (runtime error if empty).
    ArrayPop,
    /// `Array a -> Int -> Int -> Array a` — the `[from, to)` slice, clamped.
    ArraySlice,
    /// `Array a -> Array a -> Array a`
    ArrayConcat,
    // --- bitwise ops on every integer type, at that type's width ---
    /// `n -> Int -> n` — left shift (`x << k`).
    Shl,
    /// `n -> Int -> n` — right shift (`x >> k`): sign-extending for a signed
    /// type, zero-filling for an unsigned one.
    Shr,
    /// `n -> Int -> n` — zero-filling right shift (`x >>> k`) whatever the sign.
    Ushr,
    BitAnd,
    BitOr,
    BitXor,
    /// `n -> n` — bitwise complement.
    BitNot,
    /// `n -> Int` — number of set bits (an error for a negative `BigInt`).
    PopCount,
    // --- bytes ---
    /// `String -> #[UInt8]` — the UTF-8 bytes of a string.
    StringToBytes,
    /// `#[UInt8] -> String` — decode UTF-8, replacing invalid sequences with
    /// U+FFFD. An element that is not a byte is an error at run time.
    BytesToString,
    /// `#[UInt8] -> String` -- lowercase hex, two chars per byte, no separator.
    /// An element that is not a byte is an error at run time.
    BytesToHex,
    /// `show : forall a. a -> String` — the runtime's own rendering of a value,
    /// the same one the REPL prints. Structural, so it needs no per-type work.
    Show,
    /// `Char -> Int` — the Unicode scalar value.
    CharCode,
    /// `Int -> Char` — errors at run time on a value that is not a scalar.
    CharFromCode,
    /// `String -> Array Char` — decodes UTF-8.
    StringToChars,
    /// `Array Char -> String`.
    CharsToString,
    /// `concatStrings : Array String -> String` -- every string of the array,
    /// one after another, in one allocation. What an interpolated string
    /// literal is compiled to.
    ConcatStrings,
    /// `stringByteLength : String -> Int` -- the bytes a string takes, without
    /// copying them out.
    StringByteLength,
    /// `stringByteAt : String -> Int -> UInt8` -- one byte; an error at run
    /// time past either end.
    StringByteAt,
    /// `stringSlice : String -> Int -> Int -> String` -- bytes `from` up to
    /// `to`, both clamped to the string. A cut through a character leaves
    /// U+FFFD where its bytes were.
    StringSlice,
    /// `stringIndexOf : String -> String -> Int -> Int` -- where `needle` next
    /// occurs in `hay` at or after byte `from`, or -1.
    StringIndexOf,
    /// `stringCompare : String -> String -> Int` -- -1, 0 or 1 as `a` sorts
    /// before, with or after `b`, byte by byte: for UTF-8, the order of the
    /// code points.
    StringCompare,
    /// `newRef : a -> Ref a ! { Mut | e }` — allocate a mutable cell.
    NewRef,
    /// `getRef : Ref a -> a ! { Mut | e }`
    GetRef,
    /// `setRef : Ref a -> a -> () ! { Mut | e }`
    SetRef,
    /// `runSt : (forall s. () -> a ! { St s | e }) -> a ! e` -- run a
    /// computation whose mutable state cannot outlive it. Its type is the
    /// checker's business (see `meadow_infer`); lowering turns `runSt f` into
    /// `f ()`, so no engine ever meets this.
    RunSt,
    /// `stNewArray : Int -> a -> StArray s a ! { St s | e }` -- `n` copies of
    /// a value, in a mutable array.
    StNewArray,
    /// `stGetArray : StArray s a -> Int -> a ! { St s | e }`
    StGetArray,
    /// `stSetArray : StArray s a -> Int -> a -> () ! { St s | e }` -- in place.
    StSetArray,
    /// `stArrayLen : StArray s a -> Int` -- fixed at allocation, so pure.
    StArrayLen,
    /// `stFreeze : StArray s a -> Array a ! { St s | e }` -- a copy.
    StFreeze,
    /// `stThaw : Array a -> StArray s a ! { St s | e }` -- a copy.
    StThaw,
    // --- compact regions ---
    /// `compact : a -> Compact a` -- copy a value, and all it reaches, into a
    /// new region the bytecode VM's collector neither copies nor scans. An
    /// error if the value reaches a `Ref`, a mutable array or a function.
    Compact,
    /// `getCompact : Compact a -> a` -- the value inside, without copying.
    GetCompact,
    /// `compactAdd : Compact b -> a -> Compact a` -- copy a value into the same
    /// region, sharing whatever of it is there already.
    CompactAdd,
    /// `compactSize : Compact a -> Int` -- bytes the region holds. Engine
    /// dependent: the VM counts its slots, the others estimate.
    CompactSize,
    // --- green threads ---
    /// `threadSpawn : (() -> a ! { Thread, Console, … }) -> Task a ! { Thread | e }`
    /// -- start a green thread with a heap of its own, running a copy of the
    /// function. What it may do is closed: the runtime's own effects, and
    /// nothing a handler in the spawning thread would have to answer.
    ThreadSpawn,
    /// `threadAwait : Task a -> a ! { Thread | e }` -- wait for a thread to
    /// finish, and take a copy of its result; its failure, if it failed.
    ThreadAwait,
    /// `threadYield : () -> () ! { Thread | e }` -- let other threads run.
    ThreadYield,
    /// `channelNew : () -> Channel a ! { Thread | e }`
    ChannelNew,
    /// `channelSend : Channel a -> a -> () ! { Thread | e }` -- a copy of the
    /// value goes in; the sender never waits.
    ChannelSend,
    /// `channelReceive : Channel a -> a ! { Thread | e }` -- the oldest value
    /// sent, waiting for one if there is none.
    ChannelReceive,
    // --- software transactional memory (see `stm`) ---
    /// `stmNew : a -> TVar a ! { Stm | e }`, and `stmNewIO`, the same outside a
    /// transaction with `Thread` instead.
    StmNew,
    /// `stmRead : TVar a -> Maybe a ! { Stm | e }` -- `None` is a conflict.
    StmRead,
    /// `stmWrite : TVar a -> a -> () ! { Stm | e }`, into the log.
    StmWrite,
    /// `stmBegin : () -> () ! { Thread | e }` -- a new, empty log.
    StmBegin,
    /// `stmCommit : () -> Bool ! { Thread | e }` -- publish the log, or say it
    /// conflicted.
    StmCommit,
    /// `stmWait : () -> () ! { Thread | e }` -- wait until something the log
    /// read is written.
    StmWait,
    /// `stmNest : () -> () ! { Stm | e }` -- a nested log, for `orElse`.
    StmNest,
    /// `stmMerge : () -> () ! { Stm | e }` -- keep the nested log's writes.
    StmMerge,
    /// `stmRollback : () -> () ! { Stm | e }` -- drop them, keeping its reads.
    StmRollback,
    // --- top-level definitions, cached (see `globals`; no source name) ---
    /// `Int -> Bool` -- has this machine cached definition `i`?
    GlobalReady,
    /// `Int -> a` -- the cached value of definition `i`.
    GlobalGet,
    /// `Int -> a -> ()` -- cache the value of definition `i`.
    GlobalSet,
    /// `String -> Maybe #[UInt8]` -- parse a hex string (either case, no
    /// separators, even length) into bytes. `None` on any malformed input.
    BytesFromHex,
    // --- resumptions (no source name; made by lowering handlers) ---
    /// `a -> Once` -- a fresh one-shot flag, what a resumption checks. Its
    /// argument is ignored. Not a `Ref`, so that a resumption sent to another
    /// thread or compacted is refused as the continuation it is.
    Once,
    /// `Once -> Bool` -- `true` the first time, and `false` ever after.
    TakeOnce,
    // --- typed arithmetic (no source name) ---
    //
    // The operators above at a type core knows: both operands are `Int`, or
    // both `Float`, so no engine has to look at what it was given to decide
    // what to do. Nothing in core chooses them yet; the bytecode back end
    // picks its typed instructions from representations instead (see
    // `meadow_codegen`). `Int` wraps and dividing it by zero is an error, as `Add`
    // and `Div` on two `Int`s are; `Float` is IEEE.
    IntAdd,
    IntSub,
    IntMul,
    IntDiv,
    IntMod,
    IntEq,
    IntNe,
    IntLt,
    IntLe,
    IntGt,
    IntGe,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatEq,
    FloatNe,
    FloatLt,
    FloatLe,
    FloatGt,
    FloatGe,
}

impl Prim {
    /// A number for this primitive, stable for as long as the list of
    /// primitives is: what an image written to a file names one by. See
    /// [`Prim::from_code`].
    pub const fn code(self) -> u16 {
        match self {
            Prim::Add => 0,
            Prim::Sub => 1,
            Prim::Mul => 2,
            Prim::Div => 3,
            Prim::Mod => 4,
            Prim::Pow => 5,
            Prim::Eq => 6,
            Prim::Ne => 7,
            Prim::Lt => 8,
            Prim::Gt => 9,
            Prim::Le => 10,
            Prim::Ge => 11,
            Prim::Neg => 12,
            Prim::Display => 13,
            Prim::Hash => 14,
            Prim::AddF => 15,
            Prim::SubF => 16,
            Prim::MulF => 17,
            Prim::DivF => 18,
            Prim::LtF => 19,
            Prim::GtF => 20,
            Prim::LeF => 21,
            Prim::GeF => 22,
            Prim::ToFloat => 23,
            Prim::ToFloat32 => 24,
            Prim::Floor => 25,
            Prim::ToBig => 26,
            Prim::ToInt => 27,
            Prim::ToWord(w) => 28 + w as u16,
            Prim::BitWidth => 35,
            Prim::ArrayLen => 36,
            Prim::ArrayGet => 37,
            Prim::ArrayGetOr => 38,
            Prim::ArraySet => 39,
            Prim::ArrayPush => 40,
            Prim::ArrayPop => 41,
            Prim::ArraySlice => 42,
            Prim::ArrayConcat => 43,
            Prim::Shl => 44,
            Prim::Shr => 45,
            Prim::Ushr => 46,
            Prim::BitAnd => 47,
            Prim::BitOr => 48,
            Prim::BitXor => 49,
            Prim::BitNot => 50,
            Prim::PopCount => 51,
            Prim::StringToBytes => 52,
            Prim::BytesToString => 53,
            Prim::BytesToHex => 54,
            Prim::Show => 55,
            Prim::CharCode => 56,
            Prim::CharFromCode => 57,
            Prim::StringToChars => 58,
            Prim::CharsToString => 59,
            Prim::NewRef => 60,
            Prim::GetRef => 61,
            Prim::SetRef => 62,
            Prim::RunSt => 63,
            Prim::StNewArray => 64,
            Prim::StGetArray => 65,
            Prim::StSetArray => 66,
            Prim::StArrayLen => 67,
            Prim::StFreeze => 68,
            Prim::StThaw => 69,
            Prim::Compact => 70,
            Prim::GetCompact => 71,
            Prim::CompactAdd => 72,
            Prim::CompactSize => 73,
            Prim::ThreadSpawn => 74,
            Prim::ThreadAwait => 75,
            Prim::ThreadYield => 76,
            Prim::ChannelNew => 77,
            Prim::ChannelSend => 78,
            Prim::ChannelReceive => 79,
            Prim::StmNew => 80,
            Prim::StmRead => 81,
            Prim::StmWrite => 82,
            Prim::StmBegin => 83,
            Prim::StmCommit => 84,
            Prim::StmWait => 85,
            Prim::StmNest => 86,
            Prim::StmMerge => 87,
            Prim::StmRollback => 88,
            Prim::GlobalReady => 89,
            Prim::GlobalGet => 90,
            Prim::GlobalSet => 91,
            Prim::BytesFromHex => 92,
            Prim::Once => 93,
            Prim::TakeOnce => 94,
            Prim::IntAdd => 95,
            Prim::IntSub => 96,
            Prim::IntMul => 97,
            Prim::IntDiv => 98,
            Prim::IntMod => 99,
            Prim::IntEq => 100,
            Prim::IntNe => 101,
            Prim::IntLt => 102,
            Prim::IntLe => 103,
            Prim::IntGt => 104,
            Prim::IntGe => 105,
            Prim::FloatAdd => 106,
            Prim::FloatSub => 107,
            Prim::FloatMul => 108,
            Prim::FloatDiv => 109,
            Prim::FloatEq => 110,
            Prim::FloatNe => 111,
            Prim::FloatLt => 112,
            Prim::FloatLe => 113,
            Prim::FloatGt => 114,
            Prim::FloatGe => 115,
            Prim::ConcatStrings => 116,
            Prim::StringByteLength => 117,
            Prim::StringByteAt => 118,
            Prim::StringSlice => 119,
            Prim::StringIndexOf => 120,
            Prim::StringCompare => 121,
        }
    }

    /// The primitive [`Prim::code`] numbered `c`, if any.
    pub fn from_code(c: u16) -> Option<Prim> {
        Some(match c {
            0 => Prim::Add,
            1 => Prim::Sub,
            2 => Prim::Mul,
            3 => Prim::Div,
            4 => Prim::Mod,
            5 => Prim::Pow,
            6 => Prim::Eq,
            7 => Prim::Ne,
            8 => Prim::Lt,
            9 => Prim::Gt,
            10 => Prim::Le,
            11 => Prim::Ge,
            12 => Prim::Neg,
            13 => Prim::Display,
            14 => Prim::Hash,
            15 => Prim::AddF,
            16 => Prim::SubF,
            17 => Prim::MulF,
            18 => Prim::DivF,
            19 => Prim::LtF,
            20 => Prim::GtF,
            21 => Prim::LeF,
            22 => Prim::GeF,
            23 => Prim::ToFloat,
            24 => Prim::ToFloat32,
            25 => Prim::Floor,
            26 => Prim::ToBig,
            27 => Prim::ToInt,
            28..=34 => Prim::ToWord(num::Width::ALL[(c - 28) as usize]),
            35 => Prim::BitWidth,
            36 => Prim::ArrayLen,
            37 => Prim::ArrayGet,
            38 => Prim::ArrayGetOr,
            39 => Prim::ArraySet,
            40 => Prim::ArrayPush,
            41 => Prim::ArrayPop,
            42 => Prim::ArraySlice,
            43 => Prim::ArrayConcat,
            44 => Prim::Shl,
            45 => Prim::Shr,
            46 => Prim::Ushr,
            47 => Prim::BitAnd,
            48 => Prim::BitOr,
            49 => Prim::BitXor,
            50 => Prim::BitNot,
            51 => Prim::PopCount,
            52 => Prim::StringToBytes,
            53 => Prim::BytesToString,
            54 => Prim::BytesToHex,
            55 => Prim::Show,
            56 => Prim::CharCode,
            57 => Prim::CharFromCode,
            58 => Prim::StringToChars,
            59 => Prim::CharsToString,
            60 => Prim::NewRef,
            61 => Prim::GetRef,
            62 => Prim::SetRef,
            63 => Prim::RunSt,
            64 => Prim::StNewArray,
            65 => Prim::StGetArray,
            66 => Prim::StSetArray,
            67 => Prim::StArrayLen,
            68 => Prim::StFreeze,
            69 => Prim::StThaw,
            70 => Prim::Compact,
            71 => Prim::GetCompact,
            72 => Prim::CompactAdd,
            73 => Prim::CompactSize,
            74 => Prim::ThreadSpawn,
            75 => Prim::ThreadAwait,
            76 => Prim::ThreadYield,
            77 => Prim::ChannelNew,
            78 => Prim::ChannelSend,
            79 => Prim::ChannelReceive,
            80 => Prim::StmNew,
            81 => Prim::StmRead,
            82 => Prim::StmWrite,
            83 => Prim::StmBegin,
            84 => Prim::StmCommit,
            85 => Prim::StmWait,
            86 => Prim::StmNest,
            87 => Prim::StmMerge,
            88 => Prim::StmRollback,
            89 => Prim::GlobalReady,
            90 => Prim::GlobalGet,
            91 => Prim::GlobalSet,
            92 => Prim::BytesFromHex,
            93 => Prim::Once,
            94 => Prim::TakeOnce,
            95 => Prim::IntAdd,
            96 => Prim::IntSub,
            97 => Prim::IntMul,
            98 => Prim::IntDiv,
            99 => Prim::IntMod,
            100 => Prim::IntEq,
            101 => Prim::IntNe,
            102 => Prim::IntLt,
            103 => Prim::IntLe,
            104 => Prim::IntGt,
            105 => Prim::IntGe,
            106 => Prim::FloatAdd,
            107 => Prim::FloatSub,
            108 => Prim::FloatMul,
            109 => Prim::FloatDiv,
            110 => Prim::FloatEq,
            111 => Prim::FloatNe,
            112 => Prim::FloatLt,
            113 => Prim::FloatLe,
            114 => Prim::FloatGt,
            115 => Prim::FloatGe,
            116 => Prim::ConcatStrings,
            117 => Prim::StringByteLength,
            118 => Prim::StringByteAt,
            119 => Prim::StringSlice,
            120 => Prim::StringIndexOf,
            121 => Prim::StringCompare,
            _ => return None,
        })
    }

    /// The untyped primitive a typed one is an instance of -- what an engine
    /// that does not care to be fast can run instead.
    pub const fn untyped(self) -> Prim {
        use Prim::*;
        match self {
            IntAdd => Add,
            IntSub => Sub,
            IntMul => Mul,
            IntDiv => Div,
            IntMod => Mod,
            IntEq | FloatEq => Eq,
            IntNe | FloatNe => Ne,
            IntLt => Lt,
            IntLe => Le,
            IntGt => Gt,
            IntGe => Ge,
            FloatAdd => AddF,
            FloatSub => SubF,
            FloatMul => MulF,
            FloatDiv => DivF,
            FloatLt => LtF,
            FloatLe => LeF,
            FloatGt => GtF,
            FloatGe => GeF,
            other => other,
        }
    }
}

impl Prim {
    /// Does `p` compare two values and answer a `Bool`?
    ///
    /// Two things rest on this list, and both are about a comparison being the
    /// only kind of primitive that can be fused into a branch. It has to answer
    /// a boolean, or the branch would have nothing to test; and — the reason
    /// the runtime cares — it must not **allocate**, because the machine puts a
    /// fused comparison's result in a register the collector does not scan.
    ///
    /// So this is a claim about `meadow_rts::prims`, not only about types. A
    /// primitive that allocates does not belong here however boolean it looks.
    pub const fn compares(self) -> bool {
        use Prim::*;
        matches!(
            self,
            Eq | Ne
                | IntEq
                | IntNe
                | IntLt
                | IntLe
                | IntGt
                | IntGe
                | FloatEq
                | FloatNe
                | FloatLt
                | FloatLe
                | FloatGt
                | FloatGe
                | Lt
                | Gt
                | Le
                | Ge
                | LtF
                | GtF
                | LeF
                | GeF
        )
    }

    pub fn from_name(name: &str) -> Option<Prim> {
        Some(match name {
            "+" => Prim::Add,
            "-" => Prim::Sub,
            "*" => Prim::Mul,
            "/" => Prim::Div,
            "%" => Prim::Mod,
            "^" => Prim::Pow,
            "==" => Prim::Eq,
            "!=" => Prim::Ne,
            "<" => Prim::Lt,
            ">" => Prim::Gt,
            "<=" => Prim::Le,
            ">=" => Prim::Ge,
            "neg" => Prim::Neg,
            "display" => Prim::Display,
            "hash" => Prim::Hash,
            "+." => Prim::AddF,
            "-." => Prim::SubF,
            "*." => Prim::MulF,
            "/." => Prim::DivF,
            "<." => Prim::LtF,
            ">." => Prim::GtF,
            "<=." => Prim::LeF,
            ">=." => Prim::GeF,
            "toFloat" | "toFloat64" => Prim::ToFloat,
            "toFloat32" => Prim::ToFloat32,
            "floor" => Prim::Floor,
            "toBigInt" => Prim::ToBig,
            "toInt" | "toInt64" => Prim::ToInt,
            "bitWidth" => Prim::BitWidth,
            other => match other.strip_prefix("to").and_then(num::Width::from_type) {
                Some(w) => Prim::ToWord(w),
                None => return Self::from_name_rest(name),
            },
        })
    }

    fn from_name_rest(name: &str) -> Option<Prim> {
        Some(match name {
            "arrayLen" => Prim::ArrayLen,
            "arrayGet" => Prim::ArrayGet,
            "arrayGetOr" => Prim::ArrayGetOr,
            "arraySet" => Prim::ArraySet,
            "arrayPush" => Prim::ArrayPush,
            "arrayPop" => Prim::ArrayPop,
            "arraySlice" => Prim::ArraySlice,
            "arrayConcat" => Prim::ArrayConcat,
            "shl" => Prim::Shl,
            "shr" => Prim::Shr,
            "ushr" => Prim::Ushr,
            "bitAnd" => Prim::BitAnd,
            "bitOr" => Prim::BitOr,
            "bitXor" => Prim::BitXor,
            "bitNot" => Prim::BitNot,
            "popCount" => Prim::PopCount,
            "stringToBytes" => Prim::StringToBytes,
            "bytesToString" => Prim::BytesToString,
            "bytesToHex" => Prim::BytesToHex,
            "bytesFromHex" => Prim::BytesFromHex,
            "show" => Prim::Show,
            "charCode" => Prim::CharCode,
            "charFromCode" => Prim::CharFromCode,
            "stringToChars" => Prim::StringToChars,
            "charsToString" => Prim::CharsToString,
            "concatStrings" => Prim::ConcatStrings,
            "stringByteLength" => Prim::StringByteLength,
            "stringByteAt" => Prim::StringByteAt,
            "stringSlice" => Prim::StringSlice,
            "stringIndexOf" => Prim::StringIndexOf,
            "stringCompare" => Prim::StringCompare,
            "newRef" => Prim::NewRef,
            "getRef" => Prim::GetRef,
            "setRef" => Prim::SetRef,
            // A cell tied to a `runSt` is an ordinary cell at run time; only
            // its type differs.
            "stNewRef" => Prim::NewRef,
            "stGetRef" => Prim::GetRef,
            "stSetRef" => Prim::SetRef,
            "runSt" => Prim::RunSt,
            "stNewArray" => Prim::StNewArray,
            "stGetArray" => Prim::StGetArray,
            "stSetArray" => Prim::StSetArray,
            "stArrayLen" => Prim::StArrayLen,
            "stFreeze" => Prim::StFreeze,
            "stThaw" => Prim::StThaw,
            "compact" => Prim::Compact,
            "getCompact" => Prim::GetCompact,
            "compactAdd" => Prim::CompactAdd,
            "compactSize" => Prim::CompactSize,
            "threadSpawn" => Prim::ThreadSpawn,
            "threadAwait" => Prim::ThreadAwait,
            "threadYield" => Prim::ThreadYield,
            "channelNew" => Prim::ChannelNew,
            "channelSend" => Prim::ChannelSend,
            "channelReceive" => Prim::ChannelReceive,
            "stmNew" | "stmNewIO" => Prim::StmNew,
            "stmRead" => Prim::StmRead,
            "stmWrite" => Prim::StmWrite,
            "stmBegin" => Prim::StmBegin,
            "stmCommit" => Prim::StmCommit,
            "stmWait" => Prim::StmWait,
            "stmNest" => Prim::StmNest,
            "stmMerge" => Prim::StmMerge,
            "stmRollback" => Prim::StmRollback,
            _ => return None,
        })
    }

    pub fn arity(self) -> usize {
        match self {
            Prim::Neg
            | Prim::Display
            | Prim::Hash
            | Prim::ToFloat
            | Prim::ToFloat32
            | Prim::Floor
            | Prim::ToBig
            | Prim::ToInt
            | Prim::ToWord(_)
            | Prim::BitWidth
            | Prim::ArrayLen
            | Prim::ArrayPop
            | Prim::BitNot
            | Prim::PopCount
            | Prim::StringToBytes
            | Prim::BytesToString
            | Prim::BytesToHex
            | Prim::BytesFromHex
            | Prim::Show
            | Prim::CharCode
            | Prim::CharFromCode
            | Prim::StringToChars
            | Prim::CharsToString
            | Prim::ConcatStrings
            | Prim::StringByteLength
            | Prim::NewRef
            | Prim::GetRef
            | Prim::RunSt
            | Prim::StArrayLen
            | Prim::StFreeze
            | Prim::StThaw
            | Prim::Compact
            | Prim::GetCompact
            | Prim::CompactSize
            | Prim::ThreadSpawn
            | Prim::ThreadAwait
            | Prim::ThreadYield
            | Prim::ChannelNew
            | Prim::ChannelReceive
            | Prim::GlobalReady
            | Prim::GlobalGet
            | Prim::StmNew
            | Prim::StmRead
            | Prim::StmBegin
            | Prim::StmCommit
            | Prim::StmWait
            | Prim::StmNest
            | Prim::StmMerge
            | Prim::StmRollback
            | Prim::Once
            | Prim::TakeOnce => 1,
            Prim::ArraySet
            | Prim::ArraySlice
            | Prim::ArrayGetOr
            | Prim::StSetArray
            | Prim::StringSlice
            | Prim::StringIndexOf => 3,
            _ => 2,
        }
    }
}

pub type Var = hir::VarId;

/// A type, as core writes them.
///
/// The same representation inference uses, with one reinterpretation that is
/// the whole of core's type discipline: **`Type::Var(n)` is a rigid type
/// variable**, with an id unique across the compilation unit, bound by an
/// enclosing [`Term::TyLam`] or by a [`Def`]'s [`Poly`]. It is not a
/// unification variable — nothing in core solves anything — and
/// `Type::Bound` never appears here at all, since a core polytype writes its
/// binders down rather than numbering them.
pub type Ty = InferType;

/// A rigid type variable: its id, and what sort of thing it ranges over
/// (an ordinary type, a record row, an effect row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TyVar {
    pub id: u32,
    pub kind: VarKind,
}

/// A polytype — `forall (a : k) …. t` — as a definition or a `let` binder
/// carries it. Monomorphic when `binders` is empty, which is the common case.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Poly {
    pub binders: Vec<TyVar>,
    pub ty: Ty,
}

impl Poly {
    pub fn mono(ty: Ty) -> Poly {
        Poly {
            binders: Vec::new(),
            ty,
        }
    }

    pub fn is_mono(&self) -> bool {
        self.binders.is_empty()
    }

    /// The type this polytype takes at `args`, one per binder.
    ///
    /// Substituting rather than unifying: core says what the arguments are, so
    /// instantiation is a walk.
    pub fn instantiate(&self, args: &[Ty]) -> Ty {
        if self.binders.is_empty() {
            return self.ty.clone();
        }
        let map: HashMap<u32, Ty> = self
            .binders
            .iter()
            .zip(args)
            .map(|(b, a)| (b.id, a.clone()))
            .collect();
        subst_rigid(&self.ty, &map)
    }
}

/// The type of something the compiler could not type: a node inference never
/// reached, or a type argument that could not be recovered. Both only happen
/// in a unit that already has errors, and [`crate::lint`] accepts it anywhere
/// rather than piling a second complaint on the first.
pub fn unknown() -> Ty {
    InferType::Con(InternedString::from("?"), Vec::new())
}

/// Is this the placeholder [`unknown`] type?
pub fn is_unknown(ty: &Ty) -> bool {
    matches!(ty, InferType::Con(n, args) if args.is_empty() && &**n == "?")
}

/// Replace a scheme's numbered quantifiers by types — the bridge from an
/// inference `Scheme` to a core [`Poly`], whose binders are named.
pub fn subst_bound(ty: &Ty, map: &HashMap<u32, Ty>) -> Ty {
    match ty {
        InferType::Bound(i) => map.get(i).cloned().unwrap_or_else(|| ty.clone()),
        InferType::Var(_) | InferType::RowEmpty | InferType::Error => ty.clone(),
        InferType::Con(n, args) => {
            InferType::Con(*n, args.iter().map(|a| subst_bound(a, map)).collect())
        }
        InferType::Fun(args, ret, eff) => InferType::Fun(
            args.iter().map(|a| subst_bound(a, map)).collect(),
            Box::new(subst_bound(ret, map)),
            Box::new(subst_bound(eff, map)),
        ),
        InferType::Tuple(items) => {
            InferType::Tuple(items.iter().map(|a| subst_bound(a, map)).collect())
        }
        InferType::Record(row) => InferType::Record(Box::new(subst_bound(row, map))),
        InferType::RowExtend(label, field, rest) => InferType::RowExtend(
            *label,
            Box::new(subst_bound(field, map)),
            Box::new(subst_bound(rest, map)),
        ),
    }
}

/// Replace rigid type variables by types.
pub fn subst_rigid(ty: &Ty, map: &HashMap<u32, Ty>) -> Ty {
    match ty {
        InferType::Var(v) => map.get(v).cloned().unwrap_or_else(|| ty.clone()),
        InferType::Bound(_) | InferType::RowEmpty | InferType::Error => ty.clone(),
        InferType::Con(n, args) => {
            InferType::Con(*n, args.iter().map(|a| subst_rigid(a, map)).collect())
        }
        InferType::Fun(args, ret, eff) => InferType::Fun(
            args.iter().map(|a| subst_rigid(a, map)).collect(),
            Box::new(subst_rigid(ret, map)),
            Box::new(subst_rigid(eff, map)),
        ),
        InferType::Tuple(items) => {
            InferType::Tuple(items.iter().map(|a| subst_rigid(a, map)).collect())
        }
        InferType::Record(row) => InferType::Record(Box::new(subst_rigid(row, map))),
        InferType::RowExtend(label, field, rest) => InferType::RowExtend(
            *label,
            Box::new(subst_rigid(field, map)),
            Box::new(subst_rigid(rest, map)),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Pat {
    Wild,
    /// A bound variable and the type it is bound at. Annotated like every other
    /// binder: the checker types an arm's body in an environment built from
    /// this, rather than working the types out from the scrutinee itself.
    Var(Var, Ty),
    As(Var, Ty, Box<Pat>),
    Lit(Lit),
    Tuple(Vec<Pat>),
    /// `#[p, …]` — matches a builtin `Array` of exactly this length.
    Array(Vec<Pat>),
    Ctor(InternedString, Vec<Pat>),
    Record(Vec<(InternedString, Pat)>),
}

/// Core terms. Recursive positions are `Arc<Term>` (not `Box`) so the CEK
/// interpreter (the `meadow-eval` crate) can share subterms freely — a captured
/// continuation is just a slice of `Arc`-holding stack frames.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Term {
    Var(Var),
    Lit(Lit),
    /// `\(x : T) -> e`
    Lam(Var, Ty, Arc<Term>),
    /// `/\(a : k) … -> e` — the type abstraction a generalized binding gets.
    /// Its binders are what the annotations inside `e` refer to.
    TyLam(Vec<TyVar>, Arc<Term>),
    App(Arc<Term>, Arc<Term>),
    /// `e [T, …]` — the instantiation of a polymorphic binding, written out.
    /// Every mention of a polymorphic name is wrapped in one of these.
    TyApp(Arc<Term>, Vec<Ty>),
    Let(Var, Poly, Arc<Term>, Arc<Term>),
    LetRec(Vec<(Var, Poly, Term)>, Arc<Term>),
    If(Arc<Term>, Arc<Term>, Arc<Term>),
    Tuple(Vec<Term>),
    Proj(Arc<Term>, usize),
    /// `#[e, …]` — a builtin `Array` literal. The *only* built-in collection:
    /// `List` and `Vector` are ordinary `Std` data types and lower to `Ctor`.
    /// The type is the element type, which an empty literal could not otherwise
    /// supply.
    Array(Vec<Term>, Ty),
    Record(Vec<(InternedString, Term)>),
    Sel(Arc<Term>, InternedString, Ty),
    Extend(Arc<Term>, InternedString, Arc<Term>),
    /// A saturated constructor application, with the type it builds — from
    /// which the checker instantiates the constructor's field types.
    Ctor(InternedString, Ty, Vec<Term>),
    /// `case scrut of …` and the type every arm has to produce. An arm is its
    /// pattern, a guard -- a `Bool`, in the scope of the pattern's variables --
    /// that must also hold for it to be taken, and its body. An arm whose guard
    /// is false is passed over for the next, as if its pattern had not matched.
    Case(Arc<Term>, Vec<(Pat, Option<Term>, Term)>, Ty),
    Prim(Prim, Vec<Term>, Ty),
    /// `perform Effect.op arg` — an algebraic-effect operation call, and the
    /// type it resumes with.
    Perform(InternedString, InternedString, Arc<Term>, Ty),
    /// `handle body with { … }` — an effect handler.
    Handle {
        body: Arc<Term>,
        clauses: Vec<HClause>,
        /// `return x -> e` — defaults to the identity when absent.
        ret: Option<(Var, Ty, Arc<Term>)>,
        /// What the whole `handle` produces.
        ty: Ty,
    },
    /// `join j (x : T)… = rhs in body` — a binding that is only ever *jumped*
    /// to, never called.
    ///
    /// A join point is a `let` with a promise: every mention of `j` in `body`
    /// is a saturated [`Term::Jump`] in tail position, so `j` never escapes,
    /// never needs a closure, and never needs its free variables captured —
    /// they are still in scope where it is entered. It is a name for a
    /// continuation that several places share.
    ///
    /// That is what makes it worth having. Transformations that push a context
    /// inwards -- case-of-case above all -- otherwise have to choose between
    /// copying the context into every branch, which can square the size of a
    /// program, and building a closure for it, which allocates. A join point
    /// is the third answer: name it once, jump to it from each branch, and let
    /// the back end make it a label. `meadow_seq` does exactly that, and its
    /// IR has had labels all along.
    ///
    /// The promise is not checked by the type system here. It is *established*
    /// by [`crate::joins`], which only makes a join point where it holds, and
    /// preserved by construction because nothing else creates one.
    Join {
        var: Var,
        params: Vec<(Var, Ty)>,
        /// What entering it produces, which is what `body` produces.
        ty: Ty,
        rhs: Arc<Term>,
        body: Arc<Term>,
    },
    /// `jump j a…` — entering a join point. Always saturated, always in tail
    /// position. The type is what it produces, which is the `Join`'s.
    Jump(Var, Vec<Term>, Ty),
    /// A term that failed to compile. Well-typed at any type, on purpose: it
    /// only exists in a unit that already has errors, and the checker has
    /// nothing useful to say about it.
    Error,
    /// `e`, which was written at `loc` -- present only when the unit was
    /// compiled for a debugger, and meaning exactly what `e` means.
    ///
    /// Only placed where a person would want to stop: a call, a `perform`, a
    /// function's body, the branches of an `if` and the arms of a `match`.
    /// Never around a literal or a condition, which the back end inspects to
    /// fold a constant or fuse a comparison into its branch: a debug build
    /// should run the program the ordinary build runs.
    Loc(Loc, Arc<Term>),
}

/// Where a term was written: which source, and where in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Loc {
    /// A [`meadow_source::Source`]'s id -- see `meadow_source::SourceId`.
    pub source: u32,
    pub span: meadow_span::Span,
}

impl Term {
    /// The term under any [`Term::Loc`]s.
    pub fn peel(&self) -> &Term {
        let mut t = self;
        while let Term::Loc(_, inner) = t {
            t = inner;
        }
        t
    }
}

impl Term {
    // --- untyped constructors -------------------------------------------
    //
    // For terms built where no type is available or wanted: a test, or code
    // the driver invents after type checking is over (the entry point the
    // test runner wraps around a `@test` function, say). They annotate with
    // [`unknown`], which [`crate::lint`] accepts anywhere.

    pub fn lam(v: Var, body: Term) -> Term {
        Term::Lam(v, unknown(), Arc::new(body))
    }

    pub fn let_(v: Var, rhs: Term, body: Term) -> Term {
        Term::Let(v, Poly::mono(unknown()), Arc::new(rhs), Arc::new(body))
    }

    pub fn letrec(binds: Vec<(Var, Term)>, body: Term) -> Term {
        Term::LetRec(
            binds
                .into_iter()
                .map(|(v, t)| (v, Poly::mono(unknown()), t))
                .collect(),
            Arc::new(body),
        )
    }

    pub fn prim(op: Prim, args: Vec<Term>) -> Term {
        Term::Prim(op, args, unknown())
    }

    pub fn ctor(name: impl Into<InternedString>, args: Vec<Term>) -> Term {
        Term::Ctor(name.into(), unknown(), args)
    }

    pub fn case(scrut: Term, arms: Vec<(Pat, Term)>) -> Term {
        let arms = arms.into_iter().map(|(p, b)| (p, None, b)).collect();
        Term::Case(Arc::new(scrut), arms, unknown())
    }

    pub fn array(items: Vec<Term>) -> Term {
        Term::Array(items, unknown())
    }

    pub fn sel(rec: Term, label: impl Into<InternedString>) -> Term {
        Term::Sel(Arc::new(rec), label.into(), unknown())
    }

    pub fn perform(
        effect: impl Into<InternedString>,
        op: impl Into<InternedString>,
        arg: Term,
    ) -> Term {
        Term::Perform(effect.into(), op.into(), Arc::new(arg), unknown())
    }
}

impl Pat {
    pub fn var(v: Var) -> Pat {
        Pat::Var(v, unknown())
    }

    pub fn as_(v: Var, sub: Pat) -> Pat {
        Pat::As(v, unknown(), Box::new(sub))
    }
}

impl Program {
    /// The type of calling definition `f` with `()` -- what a test runner's
    /// stand-in for an entry point has, since that is all it does.
    pub fn result_of_calling(&self, f: Var) -> Poly {
        let ty = self
            .defs
            .iter()
            .find(|d| d.var == f)
            .and_then(|d| match &d.poly.ty {
                InferType::Fun(_, ret, _) => Some((**ret).clone()),
                _ => None,
            });
        Poly::mono(ty.unwrap_or_else(unknown))
    }
}

impl Def {
    /// A definition with no type worth stating — see [`Term::lam`] and friends.
    pub fn untyped(var: Var, name: impl Into<InternedString>, term: Term) -> Def {
        Def {
            var,
            name: name.into(),
            poly: Poly::mono(unknown()),
            term,
        }
    }
}

/// One operation clause of a handler: `op param resume -> body`. `resume` is bound
/// to the (one-shot, deep) continuation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HClause {
    pub effect: InternedString,
    pub op: InternedString,
    pub param: Var,
    /// The operation's argument type.
    pub param_ty: Ty,
    pub resume: Var,
    /// `resume`'s type: `a -> r` from the operation's result to the handler's.
    pub resume_ty: Ty,
    pub body: Term,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Def {
    pub var: Var,
    pub name: InternedString,
    /// The definition's type. A polymorphic one's `term` is a [`Term::TyLam`]
    /// binding exactly these binders.
    pub poly: Poly,
    pub term: Term,
}

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub defs: Vec<Def>,
    pub entry: Option<Var>,
    /// Named-field order for each data/record constructor, so `.field` selection
    /// works on `Value::Ctor` at runtime.
    pub ctor_fields: HashMap<InternedString, Vec<InternedString>>,
    /// Every data and record type's constructors and their field types, by type
    /// name: what a backend needs to know what is in a field it did not name.
    pub variants: meadow_infer::VariantEnv,
    /// Variables a pass made as copies of others, and which: what a debugger
    /// needs to call a specialized copy's variables by their names.
    pub origins: HashMap<Var, Var>,
}

impl Program {
    /// A stable, human-readable rendering of the whole program.
    ///
    /// [`Var`]s (`hir::VarId`) come from a process-wide counter, so their numeric
    /// values are non-deterministic across runs — this printer renumbers them
    /// `v0, v1, …` in first-occurrence order, which makes it safe to snapshot.
    pub fn pretty(&self) -> String {
        let mut p = Printer::default();
        let mut out = String::new();
        for d in &self.defs {
            let v = p.var(d.var);
            out.push_str(&format!("{v} : {}\n", p.poly(&d.poly)));
            out.push_str(&format!("{v} = {}\n", p.term(&d.term)));
        }
        if let Some(e) = self.entry {
            out.push_str(&format!("entry: {}\n", p.var(e)));
        }
        out
    }
}

#[derive(Default)]
struct Printer {
    names: HashMap<Var, String>,
    next: u32,
    /// Rigid type variables, named in first-occurrence order for the same
    /// reason values are: their ids come from the inference arena and mean
    /// nothing to a reader.
    tyvars: HashMap<u32, String>,
    next_ty: u32,
}

impl Printer {
    /// A rigid type variable's printed name: `a`, `b`, … then `a1`, `b1`.
    fn tyvar(&mut self, id: u32) -> String {
        if let Some(s) = self.tyvars.get(&id) {
            return s.clone();
        }
        let n = self.next_ty;
        self.next_ty += 1;
        let letter = (b'a' + (n % 26) as u8) as char;
        let s = if n < 26 {
            letter.to_string()
        } else {
            format!("{letter}{}", n / 26)
        };
        self.tyvars.insert(id, s.clone());
        s
    }

    fn ty(&mut self, t: &Ty) -> String {
        match t {
            InferType::Var(v) => self.tyvar(*v),
            InferType::Bound(i) => format!("?{i}"),
            InferType::RowEmpty => "{}".to_string(),
            InferType::Error => "{error}".to_string(),
            InferType::Con(n, args) if args.is_empty() => hir::spelling(n).to_string(),
            InferType::Con(n, args) => {
                let parts: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                format!("({} {})", hir::spelling(n), parts.join(" "))
            }
            InferType::Fun(args, ret, eff) => {
                let parts: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                let e = match &**eff {
                    InferType::RowEmpty => String::new(),
                    other => format!(" ! {}", self.ty(other)),
                };
                format!("({} -> {}{e})", parts.join(" "), self.ty(ret))
            }
            InferType::Tuple(items) => {
                let parts: Vec<String> = items.iter().map(|a| self.ty(a)).collect();
                format!("({})", parts.join(", "))
            }
            InferType::Record(row) => self.row(row),
            InferType::RowExtend(..) => self.row(t),
        }
    }

    /// A row, record or effect: `{ x : Int, y : Int }`, `{ Console, Mut | e }`.
    ///
    /// An effect's payload is the empty tuple when the effect takes no
    /// parameters, and writing `Console : ()` for that would be noise.
    fn row(&mut self, t: &Ty) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = t;
        loop {
            match cur {
                InferType::RowExtend(label, field, rest) => {
                    parts.push(match &**field {
                        InferType::Tuple(xs) if xs.is_empty() => label.to_string(),
                        other => format!("{label} : {}", self.ty(other)),
                    });
                    cur = rest;
                }
                InferType::RowEmpty => {
                    return format!("{{{}}}", parts.join(", "));
                }
                tail => {
                    let tail = self.ty(tail);
                    return if parts.is_empty() {
                        format!("{{| {tail}}}")
                    } else {
                        format!("{{{} | {tail}}}", parts.join(", "))
                    };
                }
            }
        }
    }

    fn poly(&mut self, p: &Poly) -> String {
        if p.binders.is_empty() {
            return self.ty(&p.ty);
        }
        let names: Vec<String> = p.binders.iter().map(|b| self.tyvar(b.id)).collect();
        format!("forall {}. {}", names.join(" "), self.ty(&p.ty))
    }

    fn tys(&mut self, ts: &[Ty]) -> String {
        ts.iter()
            .map(|t| format!("@{}", self.ty(t)))
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn var(&mut self, v: Var) -> String {
        if let Some(s) = self.names.get(&v) {
            return s.clone();
        }
        let s = format!("v{}", self.next);
        self.next += 1;
        self.names.insert(v, s.clone());
        s
    }

    fn lit(l: &Lit) -> String {
        match l {
            Lit::Int(i) => i.to_string(),
            Lit::BigInt(i) => i.to_string(),
            Lit::Float(x) => fmt_float(*x),
            Lit::Word(w, b) => format!("{}{}", w.value(*b), w.name()),
            Lit::Float32(x) => format!("{}f32", num::fmt_float32(*x)),
            Lit::AnyInt(i, _) => format!("{i}?"),
            Lit::AnyFloat(x, _) => format!("{}?", fmt_float(*x)),
            Lit::Str(s) => format!("{:?}", &**s), // the string contents, quoted
            Lit::Sym(s) => format!("#{s}"),
            Lit::Char(c) => format!("{c:?}"),
            Lit::Bool(b) => b.to_string(),
            Lit::Unit => "()".to_string(),
        }
    }

    fn term(&mut self, t: &Term) -> String {
        match t {
            Term::Var(v) => self.var(*v),
            Term::Lit(l) => Self::lit(l),
            Term::Lam(v, t, b) => {
                let v = self.var(*v);
                let t = self.ty(t);
                format!("(\\({v} : {t}). {})", self.term(b))
            }
            Term::Join {
                var,
                params,
                rhs,
                body,
                ..
            } => {
                let j = self.var(*var);
                let ps: Vec<String> = params
                    .iter()
                    .map(|(v, t)| format!("({} : {})", self.var(*v), self.ty(t)))
                    .collect();
                let rhs = self.term(rhs);
                format!("(join {j} {} = {rhs} in {})", ps.join(" "), self.term(body))
            }
            Term::Jump(j, args, _) => {
                let j = self.var(*j);
                let args: Vec<String> = args.iter().map(|a| self.term(a)).collect();
                format!("(jump {j} {})", args.join(" "))
            }
            Term::TyLam(binders, b) => {
                let names: Vec<String> = binders.iter().map(|x| self.tyvar(x.id)).collect();
                format!("(/\\{}. {})", names.join(" "), self.term(b))
            }
            Term::TyApp(f, args) => {
                format!("({} {})", self.term(f), self.tys(args))
            }
            Term::App(f, a) => format!("({} {})", self.term(f), self.term(a)),
            Term::Let(v, p, r, b) => {
                let v = self.var(*v);
                let p = self.poly(p);
                format!("(let {v} : {p} = {} in {})", self.term(r), self.term(b))
            }
            Term::LetRec(binds, b) => {
                let parts: Vec<String> = binds
                    .iter()
                    .map(|(v, p, t)| {
                        let v = self.var(*v);
                        let p = self.poly(p);
                        format!("{v} : {p} = {}", self.term(t))
                    })
                    .collect();
                format!("(letrec {} in {})", parts.join("; "), self.term(b))
            }
            Term::If(c, t, e) => {
                format!("(if {} {} {})", self.term(c), self.term(t), self.term(e))
            }
            Term::Tuple(items) => format!("(tup {})", self.terms(items)),
            Term::Proj(t, i) => format!("({}.{i})", self.term(t)),
            Term::Array(items, t) => {
                let t = self.ty(t);
                format!("#[{} : {t}]", self.terms(items))
            }
            Term::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(l, t)| format!("{l} = {}", self.term(t)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            Term::Sel(t, l, _) => format!("({}.{l})", self.term(t)),
            Term::Extend(t, l, v) => {
                format!("({} with {l} = {})", self.term(t), self.term(v))
            }
            Term::Ctor(n, t, args) => {
                let t = self.ty(t);
                format!("({} {} : {t})", hir::spelling(n), self.terms(args))
            }
            Term::Case(s, arms, _) => {
                let parts: Vec<String> = arms
                    .iter()
                    .map(|(p, g, b)| match g {
                        Some(g) => {
                            format!("{} if {} -> {}", self.pat(p), self.term(g), self.term(b))
                        }
                        None => format!("{} -> {}", self.pat(p), self.term(b)),
                    })
                    .collect();
                format!("(case {} of {})", self.term(s), parts.join("; "))
            }
            Term::Prim(op, args, _) => format!("({op:?} {})", self.terms(args)),
            Term::Perform(eff, op, arg, _) => {
                format!("(perform {eff}.{op} {})", self.term(arg))
            }
            Term::Handle {
                body, clauses, ret, ..
            } => {
                let mut parts: Vec<String> = clauses
                    .iter()
                    .map(|c| {
                        let p = self.var(c.param);
                        let k = self.var(c.resume);
                        let effect = hir::spelling(&c.effect.to_string()).to_string();
                        format!("{effect}.{} {p} {k} -> {}", c.op, self.term(&c.body))
                    })
                    .collect();
                if let Some((v, _, b)) = ret {
                    let v = self.var(*v);
                    parts.push(format!("return {v} -> {}", self.term(b)));
                }
                format!(
                    "(handle {} with {{ {} }})",
                    self.term(body),
                    parts.join("; ")
                )
            }
            // Transparent: a dump of a debug build reads like any other.
            Term::Loc(_, inner) => self.term(inner),
            Term::Error => "<error>".to_string(),
        }
    }

    fn terms(&mut self, ts: &[Term]) -> String {
        ts.iter()
            .map(|t| self.term(t))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn pat(&mut self, p: &Pat) -> String {
        match p {
            Pat::Wild => "_".to_string(),
            Pat::Var(v, _) => self.var(*v),
            Pat::As(v, _, sub) => {
                let v = self.var(*v);
                format!("{v}@{}", self.pat(sub))
            }
            Pat::Lit(l) => Self::lit(l),
            Pat::Tuple(ps) => format!("(tup {})", self.pats(ps)),
            Pat::Array(ps) => format!("#[{}]", self.pats(ps)),
            Pat::Ctor(n, ps) => format!("({} {})", hir::spelling(n), self.pats(ps)),
            Pat::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(l, p)| format!("{l} = {}", self.pat(p)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }

    fn pats(&mut self, ps: &[Pat]) -> String {
        ps.iter().map(|p| self.pat(p)).collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prim_name_roundtrip() {
        for name in [
            "+", "-", "*", "/", "%", "^", "==", "!=", "<", ">", "<=", ">=", "neg", "display", "+.",
            "-.", "*.", "/.", "<.", ">.", "<=.", ">=.", "toFloat", "floor",
        ] {
            assert!(Prim::from_name(name).is_some(), "{name} should be a prim");
        }
        assert_eq!(Prim::from_name("map"), None);
        assert_eq!(
            Prim::from_name("toUInt8"),
            Some(Prim::ToWord(num::Width::U8))
        );
        assert_eq!(Prim::from_name("toInt64"), Some(Prim::ToInt));
        assert_eq!(Prim::from_name("toString"), None);
        assert_eq!(Prim::from_name("+~"), None, "BigInt shares `+` now");
    }

    #[test]
    fn every_primitive_has_a_code_that_comes_back() {
        for c in 0..u16::MAX {
            match Prim::from_code(c) {
                Some(p) => assert_eq!(p.code(), c, "{p:?}"),
                None => {
                    assert!(Prim::from_code(c + 1).is_none(), "codes are dense");
                    break;
                }
            }
        }
    }

    #[test]
    fn prim_arity() {
        assert_eq!(Prim::Add.arity(), 2);
        assert_eq!(Prim::Neg.arity(), 1);
        assert_eq!(Prim::Display.arity(), 1);
        assert_eq!(Prim::Eq.arity(), 2);
    }

    #[test]
    fn pretty_renumbers_variables_and_shows_types() {
        let mut vars = hir::VarIdGen::starting_at(0);
        let a = vars.fresh();
        let b = vars.fresh();
        // `forall t. t -> t`, with `t` a rigid variable — as `id` lowers.
        let t = InferType::Var(7);
        let poly = Poly {
            binders: vec![TyVar {
                id: 7,
                kind: VarKind::Type,
            }],
            ty: InferType::Fun(
                vec![t.clone()],
                Box::new(t.clone()),
                Box::new(InferType::RowEmpty),
            ),
        };
        let prog = Program {
            defs: vec![Def {
                var: a,
                name: "id".into(),
                poly: poly.clone(),
                term: Term::TyLam(
                    poly.binders.clone(),
                    Arc::new(Term::Lam(b, t, Arc::new(Term::Var(b)))),
                ),
            }],
            entry: Some(a),
            ..Default::default()
        };
        // `a` is seen first (as the def name) -> v0; `b` -> v1; the one rigid
        // type variable -> a.
        assert_eq!(
            prog.pretty(),
            "v0 : forall a. (a -> a)\nv0 = (/\\a. (\\(v1 : a). v1))\nentry: v0\n"
        );
    }
}

/// How hard the compiler works to make the program fast.
///
/// **`O0` is not "no optimisation".** The compiler is never gratuitously bad:
/// it does not build a closure for a subexpression that cannot transfer control,
/// a saturated call to a known function is a jump rather than three allocations,
/// and a primitive names its operand registers instead of gathering them. Those
/// cost nothing — no code size, no compile time worth measuring, no fidelity —
/// so turning them off would only make a debug build ten times slower for no
/// benefit to anyone. Nothing on this ladder controls them.
///
/// What the ladder controls is the work that *trades* something.
///
/// | | adds | costs |
/// |---|---|---|
/// | `O0` | nothing | — |
/// | `O1` | native code keeps the machine's books only where they are observed, with short encodings | — |
/// | `O2` | `match` compiled to a decision tree; generic code specialized; native loops kept inside one function, hot registers in machine registers | code size, compile time |
///
/// The native rows are the code generator's (`meadow_rts::codegen`), which the
/// JIT and ahead-of-time executables share. `O1` is the default, and it is
/// where a pass lands that is worth doing on every keystroke. `O0` exists so
/// that a suspected miscompilation can be bisected against a compiler doing
/// the least it is allowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OptLevel {
    /// Nothing optional at all.
    ///
    /// The bytecode is the same as at [`OptLevel::O1`]: everything below `O2`
    /// there is unconditional, because it is not a trade. A known call becoming
    /// a jump, a literal folding into the instruction that uses it — those make
    /// debug builds smaller and faster and cost nothing to read, so there is
    /// nothing to turn off. Native code differs: at `O0` it is the literal
    /// translation of each instruction.
    O0,
    /// The default: everything cheap enough to want while editing.
    #[default]
    O1,
    /// Everything, including passes that trade code size for speed.
    O2,
}
impl OptLevel {
    /// Parse `0`, `1`, `2` — or `O0`, `o1`, as a `-O` flag is usually written.
    pub fn parse(s: &str) -> Option<OptLevel> {
        match s.trim().trim_start_matches(['O', 'o']) {
            "0" => Some(OptLevel::O0),
            "1" => Some(OptLevel::O1),
            "2" => Some(OptLevel::O2),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            OptLevel::O0 => "O0",
            OptLevel::O1 => "O1",
            OptLevel::O2 => "O2",
        }
    }

    /// Compile `match` to a decision tree rather than a chain of failure
    /// continuations.
    ///
    /// The chain retests what an earlier arm already tested, so a wide `match`
    /// does more work than it needs to; a tree tests each scrutinee once. It is
    /// gated because it is the trade the chain was avoiding — a tree duplicates
    /// the arms it shares, so the code grows.
    pub const fn case_trees(self) -> bool {
        matches!(self, OptLevel::O2)
    }

    /// Copy generic code per representation it is used at, rather than pass
    /// descriptors at run time -- see [`crate::specialize::release`]. The
    /// program grows; generic code runs as fast as the code it was written
    /// for. Gated because a debug build is the one being rebuilt constantly.
    pub const fn specializes(self) -> bool {
        matches!(self, OptLevel::O2)
    }
}

// ===========================================================================
// Free variables
// ===========================================================================

/// Adds the free variables of `t` to `out`.
pub fn free_vars_into(t: &Term, out: &mut std::collections::HashSet<Var>) {
    fn go(t: &Term, bound: &mut Vec<Var>, out: &mut std::collections::HashSet<Var>) {
        match t {
            Term::TyLam(_, b) | Term::TyApp(b, _) | Term::Loc(_, b) => go(b, bound, out),
            Term::Var(v) => {
                if !bound.contains(v) {
                    out.insert(*v);
                }
            }
            Term::Lit(_) | Term::Error => {}
            Term::Join {
                var,
                params,
                rhs,
                body,
                ..
            } => {
                let depth = bound.len();
                bound.extend(params.iter().map(|(v, _)| *v));
                go(rhs, bound, out);
                bound.truncate(depth);
                bound.push(*var);
                go(body, bound, out);
                bound.truncate(depth);
            }
            Term::Jump(j, args, _) => {
                if !bound.contains(j) {
                    out.insert(*j);
                }
                for a in args {
                    go(a, bound, out);
                }
            }
            Term::Lam(p, _, b) => {
                bound.push(*p);
                go(b, bound, out);
                bound.pop();
            }
            Term::App(f, a) => {
                go(f, bound, out);
                go(a, bound, out);
            }
            Term::Let(x, _, r, b) => {
                go(r, bound, out);
                bound.push(*x);
                go(b, bound, out);
                bound.pop();
            }
            Term::LetRec(binds, body) => {
                for (v, _, _) in binds {
                    bound.push(*v);
                }
                for (_, _, t) in binds {
                    go(t, bound, out);
                }
                go(body, bound, out);
                for _ in binds {
                    bound.pop();
                }
            }
            Term::If(a, b, c) => {
                go(a, bound, out);
                go(b, bound, out);
                go(c, bound, out);
            }
            Term::Tuple(xs) | Term::Array(xs, _) => {
                for x in xs {
                    go(x, bound, out);
                }
            }
            Term::Ctor(_, _, xs) | Term::Prim(_, xs, _) => {
                for x in xs {
                    go(x, bound, out);
                }
            }
            Term::Proj(t, _) | Term::Sel(t, _, _) => go(t, bound, out),
            Term::Extend(t, _, u) => {
                go(t, bound, out);
                go(u, bound, out);
            }
            Term::Record(fs) => {
                for (_, t) in fs {
                    go(t, bound, out);
                }
            }
            Term::Perform(_, _, a, _) => go(a, bound, out),
            Term::Case(s, arms, _) => {
                go(s, bound, out);
                for (p, g, t) in arms {
                    let before = bound.len();
                    pat_vars(p, bound);
                    if let Some(g) = g {
                        go(g, bound, out);
                    }
                    go(t, bound, out);
                    bound.truncate(before);
                }
            }
            Term::Handle {
                body, clauses, ret, ..
            } => {
                go(body, bound, out);
                for c in clauses {
                    bound.push(c.param);
                    bound.push(c.resume);
                    go(&c.body, bound, out);
                    bound.pop();
                    bound.pop();
                }
                if let Some((v, _, t)) = ret {
                    bound.push(*v);
                    go(t, bound, out);
                    bound.pop();
                }
            }
        }
    }
    go(t, &mut Vec::new(), out);
}

/// The variables a pattern binds.
pub fn pat_vars(p: &Pat, out: &mut Vec<Var>) {
    match p {
        Pat::Wild | Pat::Lit(_) => {}
        Pat::Var(v, _) => out.push(*v),
        Pat::As(v, _, sub) => {
            out.push(*v);
            pat_vars(sub, out);
        }
        Pat::Tuple(ps) | Pat::Array(ps) | Pat::Ctor(_, ps) => {
            for p in ps {
                pat_vars(p, out);
            }
        }
        Pat::Record(fs) => {
            for (_, p) in fs {
                pat_vars(p, out);
            }
        }
    }
}
