//! Core: a small extended lambda calculus that every front-end feature lowers to.
//!
//! Inference runs on the HIR; once a program type-checks we translate it here. Core
//! is deliberately tiny — single-argument lambdas/applications, explicit recursion,
//! primitive ops already resolved, pattern matching reduced to `Case` + tuple/record
//! projection. Later optimization passes will chew on this, and the `meadow-eval` crate
//! walks it directly.

use meadow_hir as hir;
use meadow_infer::{Type as InferType, TypeTable};
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
    /// `setRef : Ref a -> a -> Unit ! { Mut | e }`
    SetRef,
    /// `String -> Option (Array Int)` -- parse a hex string (either case, no
    /// separators, even length) into bytes. `None` on any malformed input.
    BytesFromHex,
}

impl Prim {
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

#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Wild,
    Var(Var),
    As(Var, Box<Pat>),
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
    Lam(Var, Arc<Term>),
    App(Arc<Term>, Arc<Term>),
    Let(Var, Arc<Term>, Arc<Term>),
    LetRec(Vec<(Var, Term)>, Arc<Term>),
    If(Arc<Term>, Arc<Term>, Arc<Term>),
    Tuple(Vec<Term>),
    Proj(Arc<Term>, usize),
    /// `#[e, …]` — a builtin `Array` literal. The *only* built-in collection:
    /// `List` and `Vector` are ordinary `Std` data types and lower to `Ctor`.
    Array(Vec<Term>),
    Record(Vec<(InternedString, Term)>),
    Sel(Arc<Term>, InternedString),
    Extend(Arc<Term>, InternedString, Arc<Term>),
    Ctor(InternedString, Vec<Term>),
    Case(Arc<Term>, Vec<(Pat, Term)>),
    Prim(Prim, Vec<Term>),
    /// `perform Effect.op arg` — an algebraic-effect operation call.
    Perform(InternedString, InternedString, Arc<Term>),
    /// `handle body with { … }` — an effect handler.
    Handle {
        body: Arc<Term>,
        clauses: Vec<HClause>,
        /// `return x -> e` — defaults to the identity when absent.
        ret: Option<(Var, Arc<Term>)>,
    },
    Error,
}

/// One operation clause of a handler: `op param resume -> body`. `resume` is bound
/// to the (one-shot, deep) continuation.
#[derive(Debug, Clone, PartialEq)]
pub struct HClause {
    pub effect: InternedString,
    pub op: InternedString,
    pub param: Var,
    pub resume: Var,
    pub body: Term,
}

#[derive(Debug, Clone)]
pub struct Def {
    pub var: Var,
    pub name: InternedString,
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
}

impl Printer {
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
            Term::Lam(v, b) => {
                let v = self.var(*v);
                format!("(\\{v}. {})", self.term(b))
            }
            Term::App(f, a) => format!("({} {})", self.term(f), self.term(a)),
            Term::Let(v, r, b) => {
                let v = self.var(*v);
                format!("(let {v} = {} in {})", self.term(r), self.term(b))
            }
            Term::LetRec(binds, b) => {
                let parts: Vec<String> = binds
                    .iter()
                    .map(|(v, t)| {
                        let v = self.var(*v);
                        format!("{v} = {}", self.term(t))
                    })
                    .collect();
                format!("(letrec {} in {})", parts.join("; "), self.term(b))
            }
            Term::If(c, t, e) => {
                format!("(if {} {} {})", self.term(c), self.term(t), self.term(e))
            }
            Term::Tuple(items) => format!("(tup {})", self.terms(items)),
            Term::Proj(t, i) => format!("({}.{i})", self.term(t)),
            Term::Array(items) => format!("#[{}]", self.terms(items)),
            Term::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|(l, t)| format!("{l} = {}", self.term(t)))
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            Term::Sel(t, l) => format!("({}.{l})", self.term(t)),
            Term::Extend(t, l, v) => {
                format!("({} with {l} = {})", self.term(t), self.term(v))
            }
            Term::Ctor(n, args) => format!("({n} {})", self.terms(args)),
            Term::Case(s, arms) => {
                let parts: Vec<String> = arms
                    .iter()
                    .map(|(p, b)| format!("{} -> {}", self.pat(p), self.term(b)))
                    .collect();
                format!("(case {} of {})", self.term(s), parts.join("; "))
            }
            Term::Prim(op, args) => format!("({op:?} {})", self.terms(args)),
            Term::Perform(eff, op, arg) => {
                format!("(perform {eff}.{op} {})", self.term(arg))
            }
            Term::Handle { body, clauses, ret } => {
                let mut parts: Vec<String> = clauses
                    .iter()
                    .map(|c| {
                        let p = self.var(c.param);
                        let k = self.var(c.resume);
                        format!("{}.{} {p} {k} -> {}", c.effect, c.op, self.term(&c.body))
                    })
                    .collect();
                if let Some((v, b)) = ret {
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
            Pat::Var(v) => self.var(*v),
            Pat::As(v, sub) => {
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

// ===========================================================================
// Lowering: hir -> core
// ===========================================================================

pub struct Lowerer<'a> {
    prims: &'a HashMap<Var, Prim>,
    names: &'a HashMap<Var, InternedString>,
    /// Operation `VarId` -> `(effect, op)` — a reference to one lowers to
    /// `\x -> perform Effect.op x`.
    effect_ops: &'a HashMap<Var, (InternedString, InternedString)>,
    /// Inferred types, keyed by `NodeId` — consulted so an integer literal whose
    /// context coerced it to `BigInt` lowers to [`Lit::BigInt`], not [`Lit::Int`].
    types: &'a TypeTable,
    /// Declared arity per data constructor, so an under-applied one can be
    /// eta-expanded into a function.
    ctor_arity: &'a HashMap<InternedString, usize>,
    /// Named-field order per constructor, accumulated across `lower_module` calls.
    pub ctor_fields: HashMap<InternedString, Vec<InternedString>>,
}

impl<'a> Lowerer<'a> {
    pub fn new(
        prims: &'a HashMap<Var, Prim>,
        names: &'a HashMap<Var, InternedString>,
        effect_ops: &'a HashMap<Var, (InternedString, InternedString)>,
        types: &'a TypeTable,
        ctor_arity: &'a HashMap<InternedString, usize>,
    ) -> Self {
        Lowerer {
            prims,
            names,
            effect_ops,
            types,
            ctor_arity,
            ctor_fields: HashMap::new(),
        }
    }

    /// Is the node at `id` inferred to have type `BigInt`?
    fn is_bigint(&self, id: hir::NodeId) -> bool {
        matches!(
            self.types.get(id),
            Some(InferType::Con(n, args)) if args.is_empty() && &**n == "BigInt"
        )
    }

    /// Lower an integer literal, choosing `Int` vs `BigInt` from its inferred type.
    fn int_lit(&self, id: hir::NodeId, value: i64) -> Lit {
        if self.is_bigint(id) {
            Lit::BigInt(value)
        } else {
            Lit::Int(value)
        }
    }

    pub fn lower_module(&mut self, module: &hir::LModule) -> Vec<Def> {
        let mut defs = Vec::new();
        for decl in &module.value().decls {
            match decl.value() {
                hir::Decl::Bind(bind) => self.lower_bind_toplevel(bind, &mut defs),
                hir::Decl::Data(dd) => {
                    for v in &dd.variants {
                        if let hir::VariantFields::Named(fs) = &v.fields {
                            self.ctor_fields
                                .insert(v.name, fs.iter().map(|(n, _)| *n).collect());
                        }
                    }
                }
                hir::Decl::Record(rd) => {
                    self.ctor_fields
                        .insert(rd.name, rd.fields.iter().map(|(n, _)| *n).collect());
                }
                _ => {}
            }
        }
        defs
    }

    fn name_of(&self, v: Var) -> InternedString {
        self.names.get(&v).copied().unwrap_or_default()
    }

    fn lower_bind_toplevel(&mut self, bind: &hir::Bind, out: &mut Vec<Def>) {
        match bind {
            hir::Bind::Fun(name, params, body) => {
                let v = *name.value();
                let term = self.curry_lam(params, body);
                out.push(Def {
                    var: v,
                    name: self.name_of(v),
                    term,
                });
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs = self.lower_expr(expr);
                match pat.value() {
                    hir::Pat::Var(id) => {
                        let v = *id.value();
                        out.push(Def {
                            var: v,
                            name: self.name_of(v),
                            term: rhs,
                        });
                    }
                    hir::Pat::Wildcard => {
                        let v = hir::VarId::fresh();
                        out.push(Def {
                            var: v,
                            name: InternedString::from("_"),
                            term: rhs,
                        });
                    }
                    _ => {
                        // `def (a, b) = e` etc.: bind the value once, then project.
                        let scrut = hir::VarId::fresh();
                        out.push(Def {
                            var: scrut,
                            name: InternedString::from("$bind"),
                            term: rhs,
                        });
                        let mut binds = Vec::new();
                        self.bind_pat(Term::Var(scrut), pat, &mut binds);
                        for (v, t) in binds {
                            out.push(Def {
                                var: v,
                                name: self.name_of(v),
                                term: t,
                            });
                        }
                    }
                }
            }
            hir::Bind::Error => {}
        }
    }

    /// `\p1 p2 -> body` for irrefutable parameter patterns: each parameter binds
    /// one fresh variable, and anything structural is destructured by `let`s at
    /// the top of the body.
    fn curry_lam(&mut self, params: &[hir::LPat], body: &hir::LExpr) -> Term {
        let binders: Vec<_> = params.iter().map(|p| self.pat_binder(p)).collect();
        let mut term = self.lower_expr(body);
        for (v, structured) in binders.into_iter().rev() {
            term = self.with_pat_prelude_term(v, structured, term);
            term = Term::Lam(v, Arc::new(term));
        }
        term
    }

    /// Prefix `term` with the `let`s that destructure `var` according to `pat`.
    fn with_pat_prelude_term(
        &mut self,
        var: Var,
        pat: Option<&hir::LPat>,
        mut term: Term,
    ) -> Term {
        if let Some(p) = pat {
            let mut binds = Vec::new();
            self.bind_pat(Term::Var(var), p, &mut binds);
            for (bv, bt) in binds.into_iter().rev() {
                term = Term::Let(bv, Arc::new(bt), Arc::new(term));
            }
        }
        term
    }

    fn lower_expr(&mut self, expr: &hir::LExpr) -> Term {
        match expr.value() {
            hir::Expr::Lit(hir::Lit::Int(i)) => Term::Lit(self.int_lit(expr.id, *i)),
            hir::Expr::Lit(hir::Lit::Float(b)) => Term::Lit(Lit::Float(f64::from_bits(*b))),
            hir::Expr::Lit(hir::Lit::String(s)) => Term::Lit(Lit::Str(*s)),
            hir::Expr::Lit(hir::Lit::Char(c)) => Term::Lit(Lit::Char(*c)),
            hir::Expr::Unit => Term::Lit(Lit::Unit),

            hir::Expr::Var(id) => {
                let v = *id.value();
                if let Some(&op) = self.prims.get(&v) {
                    self.eta_prim(op)
                } else if let Some(&(eff, opname)) = self.effect_ops.get(&v) {
                    // `get` becomes `\x -> perform Effect.get x`
                    let x = hir::VarId::fresh();
                    Term::Lam(
                        x,
                        Arc::new(Term::Perform(eff, opname, Arc::new(Term::Var(x)))),
                    )
                } else {
                    Term::Var(v)
                }
            }

            hir::Expr::Lam(params, body) => self.curry_lam(params, body),

            hir::Expr::App(func, args) => {
                if let hir::Expr::Var(id) = func.value() {
                    if let Some(&op) = self.prims.get(&*id.value()) {
                        if op.arity() == args.len() {
                            let a = args.iter().map(|a| self.lower_expr(a)).collect();
                            return Term::Prim(op, a);
                        }
                    }
                }
                let mut term = self.lower_expr(func);
                for a in args {
                    term = Term::App(Arc::new(term), Arc::new(self.lower_expr(a)));
                }
                term
            }

            hir::Expr::Let(binds, body) => {
                let mut inner = self.lower_expr(body);
                for bind in binds.iter().rev() {
                    inner = self.lower_let_bind(bind, inner);
                }
                inner
            }

            hir::Expr::If(c, t, e) => Term::If(
                Arc::new(self.lower_expr(c)),
                Arc::new(self.lower_expr(t)),
                Arc::new(self.lower_expr(e)),
            ),

            hir::Expr::Match(scrut, arms) => {
                let s = self.lower_expr(scrut);
                let arms = arms
                    .iter()
                    .map(|(p, e)| (self.lower_pat(p), self.lower_expr(e)))
                    .collect();
                Term::Case(Arc::new(s), arms)
            }

            hir::Expr::Tuple(items) => {
                Term::Tuple(items.iter().map(|e| self.lower_expr(e)).collect())
            }
            hir::Expr::Array(items) => {
                Term::Array(items.iter().map(|e| self.lower_expr(e)).collect())
            }
            // `[a; b; c]` is sugar for `Cons a (Cons b (Cons c Nil))` — `List` is
            // an ordinary `Std` data type, so it lowers to plain constructors.
            hir::Expr::List(items) => {
                let nil = Term::Ctor(InternedString::from("Nil"), vec![]);
                items.iter().rev().fold(nil, |acc, e| {
                    let head = self.lower_expr(e);
                    Term::Ctor(InternedString::from("Cons"), vec![head, acc])
                })
            }
            hir::Expr::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Term> = args.iter().map(|e| self.lower_expr(e)).collect();
                match (&*name, lowered.len()) {
                    ("True", 0) => Term::Lit(Lit::Bool(true)),
                    ("False", 0) => Term::Lit(Lit::Bool(false)),
                    _ => self.ctor(name, lowered),
                }
            }

            hir::Expr::Record(fields, base) => {
                let lowered: Vec<(InternedString, Term)> = fields
                    .iter()
                    .map(|(l, e)| (*l.value(), self.lower_expr(e)))
                    .collect();
                match base {
                    None => Term::Record(lowered),
                    Some(b) => {
                        let mut term = self.lower_expr(b);
                        for (l, e) in lowered {
                            term = Term::Extend(Arc::new(term), l, Arc::new(e));
                        }
                        term
                    }
                }
            }
            hir::Expr::Field(obj, label) => {
                Term::Sel(Arc::new(self.lower_expr(obj)), *label.value())
            }

            hir::Expr::Handle(body, arms, ret) => {
                let lbody = Arc::new(self.lower_expr(body));
                let clauses = arms
                    .iter()
                    .map(|arm| {
                        let (param, refutable) = self.pat_binder(&arm.param);
                        let body = self.with_pat_prelude(param, refutable, &arm.body);
                        HClause {
                            effect: arm.effect,
                            op: arm.op,
                            param,
                            resume: *arm.resume.value(),
                            body,
                        }
                    })
                    .collect();
                let ret = ret.as_ref().map(|(pat, rbody)| {
                    let (v, refutable) = self.pat_binder(pat);
                    (v, Arc::new(self.with_pat_prelude(v, refutable, rbody)))
                });
                Term::Handle {
                    body: lbody,
                    clauses,
                    ret,
                }
            }

            hir::Expr::Error => Term::Error,
        }
    }

    /// `(binder var, Some(pat) if the pattern is refutable / structured)`.
    fn pat_binder<'p>(&self, pat: &'p hir::LPat) -> (Var, Option<&'p hir::LPat>) {
        match pat.value() {
            hir::Pat::Var(id) => (*id.value(), None),
            hir::Pat::Wildcard => (hir::VarId::fresh(), None),
            _ => (hir::VarId::fresh(), Some(pat)),
        }
    }

    /// Lower `body`, prefixing `let`s that destructure `var` per `pat`.
    fn with_pat_prelude(
        &mut self,
        var: Var,
        pat: Option<&hir::LPat>,
        body: &hir::LExpr,
    ) -> Term {
        let term = self.lower_expr(body);
        self.with_pat_prelude_term(var, pat, term)
    }

    fn lower_let_bind(&mut self, bind: &hir::Bind, body: Term) -> Term {
        match bind {
            hir::Bind::Fun(name, params, fbody) => {
                let term = self.curry_lam(params, fbody);
                Term::LetRec(vec![(*name.value(), term)], Arc::new(body))
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs = self.lower_expr(expr);
                match pat.value() {
                    hir::Pat::Var(id) => {
                        Term::Let(*id.value(), Arc::new(rhs), Arc::new(body))
                    }
                    hir::Pat::Wildcard => {
                        Term::Let(hir::VarId::fresh(), Arc::new(rhs), Arc::new(body))
                    }
                    _ => Term::Case(Arc::new(rhs), vec![(self.lower_pat(pat), body)]),
                }
            }
            hir::Bind::Error => body,
        }
    }

    /// Build `(v, access)` pairs for every variable an irrefutable pattern binds,
    /// where `access` is a term extracting that piece from `scrut`.
    fn bind_pat(&mut self, scrut: Term, pat: &hir::LPat, out: &mut Vec<(Var, Term)>) {
        match pat.value() {
            hir::Pat::Wildcard | hir::Pat::Unit | hir::Pat::Lit(_) => {}
            hir::Pat::Var(id) => out.push((*id.value(), scrut)),
            hir::Pat::As(id, sub) => {
                out.push((*id.value(), scrut.clone()));
                self.bind_pat(scrut, sub, out);
            }
            hir::Pat::Tuple(items) => {
                for (i, p) in items.iter().enumerate() {
                    self.bind_pat(Term::Proj(Arc::new(scrut.clone()), i), p, out);
                }
            }
            hir::Pat::Record(fields, _) => {
                for (label, p) in fields {
                    self.bind_pat(Term::Sel(Arc::new(scrut.clone()), *label.value()), p, out);
                }
            }
            hir::Pat::Error => {}
            // Refutable in an irrefutable position: fall back to a single-arm Case.
            hir::Pat::Cons(..) | hir::Pat::List(..) | hir::Pat::Array(..) => {
                let mut inner = Vec::new();
                collect_pat_vars(pat, &mut inner);
                let core_pat = self.lower_pat(pat);
                for v in inner {
                    out.push((
                        v,
                        Term::Case(
                            Arc::new(scrut.clone()),
                            vec![(core_pat.clone(), Term::Var(v))],
                        ),
                    ));
                }
            }
        }
    }

    fn lower_pat(&mut self, pat: &hir::LPat) -> Pat {
        match pat.value() {
            hir::Pat::Wildcard => Pat::Wild,
            hir::Pat::Unit => Pat::Lit(Lit::Unit),
            hir::Pat::Var(id) => Pat::Var(*id.value()),
            hir::Pat::As(id, sub) => Pat::As(*id.value(), Box::new(self.lower_pat(sub))),
            hir::Pat::Lit(hir::Lit::Int(i)) => Pat::Lit(self.int_lit(pat.id, *i)),
            hir::Pat::Lit(hir::Lit::Float(b)) => Pat::Lit(Lit::Float(f64::from_bits(*b))),
            hir::Pat::Lit(hir::Lit::String(s)) => Pat::Lit(Lit::Str(*s)),
            hir::Pat::Lit(hir::Lit::Char(c)) => Pat::Lit(Lit::Char(*c)),
            hir::Pat::Tuple(items) => {
                Pat::Tuple(items.iter().map(|p| self.lower_pat(p)).collect())
            }
            hir::Pat::Array(items) => {
                Pat::Array(items.iter().map(|p| self.lower_pat(p)).collect())
            }
            // `[a; b; c]` — the same `Cons`/`Nil` chain as the expression form.
            hir::Pat::List(items) => {
                let nil = Pat::Ctor(InternedString::from("Nil"), vec![]);
                items.iter().rev().fold(nil, |acc, p| {
                    let head = self.lower_pat(p);
                    Pat::Ctor(InternedString::from("Cons"), vec![head, acc])
                })
            }
            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Pat> = args.iter().map(|p| self.lower_pat(p)).collect();
                match (&*name, lowered.len()) {
                    ("True", 0) => Pat::Lit(Lit::Bool(true)),
                    ("False", 0) => Pat::Lit(Lit::Bool(false)),
                    _ => Pat::Ctor(name, lowered),
                }
            }
            hir::Pat::Record(fields, _) => Pat::Record(
                fields
                    .iter()
                    .map(|(l, p)| (*l.value(), self.lower_pat(p)))
                    .collect(),
            ),
            hir::Pat::Error => Pat::Wild,
        }
    }

    /// Build a constructor application, eta-expanding an under-applied one so a
    /// bare `Cons` / `Just` can still be passed around as a function.
    fn ctor(&self, name: InternedString, args: Vec<Term>) -> Term {
        let arity = self.ctor_arity.get(&name).copied().unwrap_or(args.len());
        if args.len() >= arity {
            return Term::Ctor(name, args);
        }
        let extra: Vec<Var> = (args.len()..arity).map(|_| hir::VarId::fresh()).collect();
        let mut all = args;
        all.extend(extra.iter().map(|v| Term::Var(*v)));
        let body = Term::Ctor(name, all);
        extra
            .into_iter()
            .rev()
            .fold(body, |acc, v| Term::Lam(v, Arc::new(acc)))
    }

    /// `\a. \b. prim(a, b)` — used when a primitive is referenced without (or with
    /// the wrong number of) arguments.
    fn eta_prim(&self, op: Prim) -> Term {
        let vars: Vec<Var> = (0..op.arity()).map(|_| hir::VarId::fresh()).collect();
        let body = Term::Prim(op, vars.iter().map(|v| Term::Var(*v)).collect());
        vars.into_iter()
            .rev()
            .fold(body, |acc, v| Term::Lam(v, Arc::new(acc)))
    }
}

fn collect_pat_vars(pat: &hir::LPat, out: &mut Vec<Var>) {
    match pat.value() {
        hir::Pat::Var(id) => out.push(*id.value()),
        hir::Pat::As(id, sub) => {
            out.push(*id.value());
            collect_pat_vars(sub, out);
        }
        hir::Pat::Tuple(items)
        | hir::Pat::List(items)
        | hir::Pat::Array(items)
        | hir::Pat::Cons(_, items) => {
            items.iter().for_each(|p| collect_pat_vars(p, out))
        }
        hir::Pat::Record(fields, _) => {
            fields.iter().for_each(|(_, p)| collect_pat_vars(p, out))
        }
        _ => {}
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
    fn pretty_renumbers_variables() {
        let a = hir::VarId::fresh();
        let b = hir::VarId::fresh();
        let prog = Program {
            defs: vec![Def {
                var: a,
                name: "f".into(),
                term: Term::Lam(b, Arc::new(Term::Var(b))),
            }],
            entry: Some(a),
            ..Default::default()
        };
        // `a` is seen first (as the def name) -> v0; `b` -> v1.
        assert_eq!(prog.pretty(), "v0 = (\\v1. v1)\nentry: v0\n");
    }
}
