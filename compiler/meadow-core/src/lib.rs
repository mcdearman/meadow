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

pub mod erase;
pub mod lint;
pub mod lower;
pub use lower::Lowerer;

use meadow_hir as hir;
use meadow_infer::{Generalized, Scheme, Type as InferType, TypeTable, VarKind};
use meadow_intern::InternedString;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// Fixed-width integer (`Int`, i.e. i64).
    Int(i64),
    /// An integer literal whose inferred type is `BigInt` (context coerced it) —
    /// widened to an arbitrary-precision value at runtime.
    BigInt(i64),
    Float(f64),
    Str(InternedString),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Print,
    Println,
    // --- floating point ---
    AddF,
    SubF,
    MulF,
    DivF,
    LtF,
    GtF,
    LeF,
    GeF,
    /// `Int -> Float`
    ToFloat,
    /// `Float -> Int` (round toward negative infinity)
    Floor,
    // --- arbitrary precision (`BigInt`) ---
    AddB,
    SubB,
    MulB,
    DivB,
    ModB,
    PowB,
    LtB,
    GtB,
    LeB,
    GeB,
    /// `Int -> BigInt`
    ToBig,
    /// `BigInt -> Int` (fails at runtime if out of range)
    ToInt,
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
    // --- bitwise ops on `Int` (i64) ---
    /// `Int -> Int -> Int` — logical left shift (`x << n`, `n` masked to 0..63).
    Shl,
    /// `Int -> Int -> Int` — arithmetic right shift (`x >> n`, sign-extending).
    Shr,
    /// `Int -> Int -> Int` — arithmetic-right-shift by an unsigned reading.
    Ushr,
    BitAnd,
    BitOr,
    BitXor,
    /// `Int -> Int` — bitwise complement.
    BitNot,
    /// `Int -> Int` — number of set bits.
    PopCount,
    // --- bytes ---
    /// `String -> Array Int` — the UTF-8 bytes of a string.
    StringToBytes,
    /// `Array Int -> String` — decode UTF-8, replacing invalid sequences with
    /// U+FFFD (never fails). Non-`Int` / out-of-range elements error at runtime.
    BytesToString,
    /// `Array Int -> String` -- lowercase hex, two chars per byte, no separator.
    /// Non-`Int` / out-of-range (not 0..255) elements error at runtime.
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
    /// `newRef : a -> Ref a ! { Mut | e }` — allocate a mutable cell.
    NewRef,
    /// `getRef : Ref a -> a ! { Mut | e }`
    GetRef,
    /// `setRef : Ref a -> a -> () ! { Mut | e }`
    SetRef,
    /// `String -> Option (Array Int)` -- parse a hex string (either case, no
    /// separators, even length) into bytes. `None` on any malformed input.
    BytesFromHex,
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
                | Lt
                | Gt
                | Le
                | Ge
                | LtF
                | GtF
                | LeF
                | GeF
                | LtB
                | GtB
                | LeB
                | GeB
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
            "print" => Prim::Print,
            "println" => Prim::Println,
            "+." => Prim::AddF,
            "-." => Prim::SubF,
            "*." => Prim::MulF,
            "/." => Prim::DivF,
            "<." => Prim::LtF,
            ">." => Prim::GtF,
            "<=." => Prim::LeF,
            ">=." => Prim::GeF,
            "toFloat" => Prim::ToFloat,
            "floor" => Prim::Floor,
            "+~" => Prim::AddB,
            "-~" => Prim::SubB,
            "*~" => Prim::MulB,
            "/~" => Prim::DivB,
            "%~" => Prim::ModB,
            "^~" => Prim::PowB,
            "<~" => Prim::LtB,
            ">~" => Prim::GtB,
            "<=~" => Prim::LeB,
            ">=~" => Prim::GeB,
            "toBigInt" => Prim::ToBig,
            "toInt" => Prim::ToInt,
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
            "newRef" => Prim::NewRef,
            "getRef" => Prim::GetRef,
            "setRef" => Prim::SetRef,
            _ => return None,
        })
    }

    pub fn arity(self) -> usize {
        match self {
            Prim::Neg
            | Prim::Print
            | Prim::Println
            | Prim::ToFloat
            | Prim::Floor
            | Prim::ToBig
            | Prim::ToInt
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
            | Prim::NewRef
            | Prim::GetRef => 1,
            Prim::ArraySet | Prim::ArraySlice | Prim::ArrayGetOr => 3,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TyVar {
    pub id: u32,
    pub kind: VarKind,
}

/// A polytype — `forall (a : k) …. t` — as a definition or a `let` binder
/// carries it. Monomorphic when `binders` is empty, which is the common case.
#[derive(Debug, Clone, PartialEq)]
pub struct Poly {
    pub binders: Vec<TyVar>,
    pub ty: Ty,
}

impl Poly {
    pub fn mono(ty: Ty) -> Poly {
        Poly { binders: Vec::new(), ty }
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
        InferType::Con(n, args) => InferType::Con(
            *n,
            args.iter().map(|a| subst_rigid(a, map)).collect(),
        ),
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

#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
    /// `case scrut of …` and the type every arm has to produce.
    Case(Arc<Term>, Vec<(Pat, Term)>, Ty),
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
    /// A term that failed to compile. Well-typed at any type, on purpose: it
    /// only exists in a unit that already has errors, and the checker has
    /// nothing useful to say about it.
    Error,
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
#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone)]
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
            InferType::Con(n, args) if args.is_empty() => n.to_string(),
            InferType::Con(n, args) => {
                let parts: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                format!("({n} {})", parts.join(" "))
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

    /// A row, record or effect: `{ x : Int, y : Int }`, `{ io, Mut | e }`.
    ///
    /// An effect's payload is the empty tuple when the effect takes no
    /// parameters, and writing `io : ()` for that would be noise.
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
            Lit::Str(s) => format!("{:?}", &**s), // the string contents, quoted
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
            Term::TyLam(binders, b) => {
                let names: Vec<String> =
                    binders.iter().map(|x| self.tyvar(x.id)).collect();
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
                format!("({n} {} : {t})", self.terms(args))
            }
            Term::Case(s, arms, _) => {
                let parts: Vec<String> = arms
                    .iter()
                    .map(|(p, b)| format!("{} -> {}", self.pat(p), self.term(b)))
                    .collect();
                format!("(case {} of {})", self.term(s), parts.join("; "))
            }
            Term::Prim(op, args, _) => format!("({op:?} {})", self.terms(args)),
            Term::Perform(eff, op, arg, _) => {
                format!("(perform {eff}.{op} {})", self.term(arg))
            }
            Term::Handle { body, clauses, ret, .. } => {
                let mut parts: Vec<String> = clauses
                    .iter()
                    .map(|c| {
                        let p = self.var(c.param);
                        let k = self.var(c.resume);
                        format!("{}.{} {p} {k} -> {}", c.effect, c.op, self.term(&c.body))
                    })
                    .collect();
                if let Some((v, _, b)) = ret {
                    let v = self.var(*v);
                    parts.push(format!("return {v} -> {}", self.term(b)));
                }
                format!("(handle {} with {{ {} }})", self.term(body), parts.join("; "))
            }
            Term::Error => "<error>".to_string(),
        }
    }

    fn terms(&mut self, ts: &[Term]) -> String {
        ts.iter().map(|t| self.term(t)).collect::<Vec<_>>().join(" ")
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
            Pat::Ctor(n, ps) => format!("({n} {})", self.pats(ps)),
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
            "+", "-", "*", "/", "%", "^", "==", "!=", "<", ">", "<=", ">=", "neg", "print",
            "println", "+.", "-.", "*.", "/.", "<.", ">.", "<=.", ">=.", "toFloat", "floor",
        ] {
            assert!(Prim::from_name(name).is_some(), "{name} should be a prim");
        }
        assert_eq!(Prim::from_name("map"), None);
    }

    #[test]
    fn prim_arity() {
        assert_eq!(Prim::Add.arity(), 2);
        assert_eq!(Prim::Neg.arity(), 1);
        assert_eq!(Prim::Println.arity(), 1);
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
            binders: vec![TyVar { id: 7, kind: VarKind::Type }],
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
/// | `O1` | nothing yet | — |
/// | `O2` | `match` compiled to a decision tree | code size, compile time |
///
/// `O1` adding nothing over `O0` today is deliberate rather than an oversight:
/// `O1` is the default, and it is where a pass lands that is worth doing on
/// every keystroke. `O0` exists so that a suspected miscompilation can be
/// bisected against a compiler doing the least it is allowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OptLevel {
    /// Nothing optional at all.
    ///
    /// Which today is the same code as [`OptLevel::O1`]: everything below `O2`
    /// is unconditional, because it is not a trade. A known call becoming a
    /// jump, a literal folding into the instruction that uses it — those make
    /// debug builds smaller and faster and cost nothing to read, so there is
    /// nothing to turn off. `O0` exists for the first pass that changes that.
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
}
