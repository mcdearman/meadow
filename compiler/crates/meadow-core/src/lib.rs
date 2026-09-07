//! Core: a small extended lambda calculus that every front-end feature lowers to.
//!
//! Inference runs on the HIR; once a program type-checks we translate it here. Core
//! is deliberately tiny — single-argument lambdas/applications, explicit recursion,
//! primitive ops already resolved, pattern matching reduced to `Case` + tuple/record
//! projection. Later optimization passes will chew on this, and the `meadow-eval` crate
//! walks it directly.

use meadow_hir as hir;
use meadow_intern::InternedString;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Str(InternedString),
    Bool(bool),
    Unit,
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
    And,
    Or,
    Neg,
    Not,
    Print,
    Println,
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
            "&&" => Prim::And,
            "||" => Prim::Or,
            "neg" => Prim::Neg,
            "!" => Prim::Not,
            "print" => Prim::Print,
            "println" => Prim::Println,
            _ => return None,
        })
    }

    pub fn arity(self) -> usize {
        match self {
            Prim::Neg | Prim::Not | Prim::Print | Prim::Println => 1,
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
    List(Vec<Pat>),
    /// Built-in list `Nil`.
    ListNil,
    /// Built-in list `Cons head tail`.
    ListCons(Box<Pat>, Box<Pat>),
    Ctor(InternedString, Vec<Pat>),
    Record(Vec<(InternedString, Pat)>),
}

/// Core terms. Recursive positions are `Rc<Term>` (not `Box`) so the CEK
/// interpreter (the `meadow-eval` crate) can share subterms freely — a captured
/// continuation is just a slice of `Rc`-holding stack frames.
#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Var(Var),
    Lit(Lit),
    Lam(Var, Rc<Term>),
    App(Rc<Term>, Rc<Term>),
    Let(Var, Rc<Term>, Rc<Term>),
    LetRec(Vec<(Var, Term)>, Rc<Term>),
    If(Rc<Term>, Rc<Term>, Rc<Term>),
    Tuple(Vec<Term>),
    Proj(Rc<Term>, usize),
    List(Vec<Term>),
    /// Built-in list `Cons head tail` — prepends `head` onto the list `tail`.
    ListCons(Rc<Term>, Rc<Term>),
    Record(Vec<(InternedString, Term)>),
    Sel(Rc<Term>, InternedString),
    Extend(Rc<Term>, InternedString, Rc<Term>),
    Ctor(InternedString, Vec<Term>),
    Case(Rc<Term>, Vec<(Pat, Term)>),
    Prim(Prim, Vec<Term>),
    /// `perform Effect.op arg` — an algebraic-effect operation call.
    Perform(InternedString, InternedString, Rc<Term>),
    /// `handle body with { … }` — an effect handler.
    Handle {
        body: Rc<Term>,
        clauses: Vec<HClause>,
        /// `return x -> e` — defaults to the identity when absent.
        ret: Option<(Var, Rc<Term>)>,
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
            Lit::Str(s) => format!("{:?}", &**s), // the string contents, quoted
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
            Term::List(items) => format!("[{}]", self.terms(items)),
            Term::ListCons(h, t) => format!("(:: {} {})", self.term(h), self.term(t)),
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
            Pat::List(ps) => format!("[{}]", self.pats(ps)),
            Pat::ListNil => "[]".to_string(),
            Pat::ListCons(h, t) => format!("(:: {} {})", self.pat(h), self.pat(t)),
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
    /// Named-field order per constructor, accumulated across `lower_module` calls.
    pub ctor_fields: HashMap<InternedString, Vec<InternedString>>,
}

impl<'a> Lowerer<'a> {
    pub fn new(
        prims: &'a HashMap<Var, Prim>,
        names: &'a HashMap<Var, InternedString>,
        effect_ops: &'a HashMap<Var, (InternedString, InternedString)>,
    ) -> Self {
        Lowerer {
            prims,
            names,
            effect_ops,
            ctor_fields: HashMap::new(),
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

    fn curry_lam(&mut self, params: &[hir::Ident], body: &hir::LExpr) -> Term {
        let mut term = self.lower_expr(body);
        for p in params.iter().rev() {
            term = Term::Lam(*p.value(), Rc::new(term));
        }
        term
    }

    fn lower_expr(&mut self, expr: &hir::LExpr) -> Term {
        match expr.value() {
            hir::Expr::Lit(hir::Lit::Int(i)) => Term::Lit(Lit::Int(*i)),
            hir::Expr::Lit(hir::Lit::String(s)) => Term::Lit(Lit::Str(*s)),
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
                        Rc::new(Term::Perform(eff, opname, Rc::new(Term::Var(x)))),
                    )
                } else {
                    Term::Var(v)
                }
            }

            hir::Expr::Lam(params, body) => {
                let params: Vec<_> = params
                    .iter()
                    .map(|p| match p.value() {
                        hir::Pat::Var(id) => (*id.value(), None),
                        hir::Pat::Wildcard => (hir::VarId::fresh(), None),
                        _ => (hir::VarId::fresh(), Some(p)),
                    })
                    .collect();
                let mut term = self.lower_expr(body);
                for (v, refutable) in params.into_iter().rev() {
                    if let Some(p) = refutable {
                        let mut binds = Vec::new();
                        self.bind_pat(Term::Var(v), p, &mut binds);
                        for (bv, bt) in binds.into_iter().rev() {
                            term = Term::Let(bv, Rc::new(bt), Rc::new(term));
                        }
                    }
                    term = Term::Lam(v, Rc::new(term));
                }
                term
            }

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
                    term = Term::App(Rc::new(term), Rc::new(self.lower_expr(a)));
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
                Rc::new(self.lower_expr(c)),
                Rc::new(self.lower_expr(t)),
                Rc::new(self.lower_expr(e)),
            ),

            hir::Expr::Match(scrut, arms) => {
                let s = self.lower_expr(scrut);
                let arms = arms
                    .iter()
                    .map(|(p, e)| (self.lower_pat(p), self.lower_expr(e)))
                    .collect();
                Term::Case(Rc::new(s), arms)
            }

            hir::Expr::Tuple(items) => {
                Term::Tuple(items.iter().map(|e| self.lower_expr(e)).collect())
            }
            hir::Expr::List(items) => {
                Term::List(items.iter().map(|e| self.lower_expr(e)).collect())
            }
            hir::Expr::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Term> = args.iter().map(|e| self.lower_expr(e)).collect();
                match (&*name, lowered.len()) {
                    ("Nil", 0) => Term::List(vec![]),
                    ("True", 0) => Term::Lit(Lit::Bool(true)),
                    ("False", 0) => Term::Lit(Lit::Bool(false)),
                    ("Cons", 2) => {
                        let mut it = lowered.into_iter();
                        Term::ListCons(Rc::new(it.next().unwrap()), Rc::new(it.next().unwrap()))
                    }
                    // under/over-applied built-in constructor: eta-expand and apply
                    ("Nil" | "Cons", _) => lowered
                        .into_iter()
                        .fold(self.eta_ctor(&name), |f, a| Term::App(Rc::new(f), Rc::new(a))),
                    // unknown constructor (no `data` decls yet)
                    _ => Term::Ctor(name, lowered),
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
                            term = Term::Extend(Rc::new(term), l, Rc::new(e));
                        }
                        term
                    }
                }
            }
            hir::Expr::Field(obj, label) => {
                Term::Sel(Rc::new(self.lower_expr(obj)), *label.value())
            }

            hir::Expr::Handle(body, arms, ret) => {
                let lbody = Rc::new(self.lower_expr(body));
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
                    (v, Rc::new(self.with_pat_prelude(v, refutable, rbody)))
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
        let mut term = self.lower_expr(body);
        if let Some(p) = pat {
            let mut binds = Vec::new();
            self.bind_pat(Term::Var(var), p, &mut binds);
            for (bv, bt) in binds.into_iter().rev() {
                term = Term::Let(bv, Rc::new(bt), Rc::new(term));
            }
        }
        term
    }

    fn lower_let_bind(&mut self, bind: &hir::Bind, body: Term) -> Term {
        match bind {
            hir::Bind::Fun(name, params, fbody) => {
                let term = self.curry_lam(params, fbody);
                Term::LetRec(vec![(*name.value(), term)], Rc::new(body))
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs = self.lower_expr(expr);
                match pat.value() {
                    hir::Pat::Var(id) => {
                        Term::Let(*id.value(), Rc::new(rhs), Rc::new(body))
                    }
                    hir::Pat::Wildcard => {
                        Term::Let(hir::VarId::fresh(), Rc::new(rhs), Rc::new(body))
                    }
                    _ => Term::Case(Rc::new(rhs), vec![(self.lower_pat(pat), body)]),
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
                    self.bind_pat(Term::Proj(Rc::new(scrut.clone()), i), p, out);
                }
            }
            hir::Pat::Record(fields, _) => {
                for (label, p) in fields {
                    self.bind_pat(Term::Sel(Rc::new(scrut.clone()), *label.value()), p, out);
                }
            }
            hir::Pat::Error => {}
            // Refutable in an irrefutable position: fall back to a single-arm Case.
            hir::Pat::Cons(..) | hir::Pat::List(..) => {
                let mut inner = Vec::new();
                collect_pat_vars(pat, &mut inner);
                let core_pat = self.lower_pat(pat);
                for v in inner {
                    out.push((
                        v,
                        Term::Case(
                            Rc::new(scrut.clone()),
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
            hir::Pat::Lit(hir::Lit::Int(i)) => Pat::Lit(Lit::Int(*i)),
            hir::Pat::Lit(hir::Lit::String(s)) => Pat::Lit(Lit::Str(*s)),
            hir::Pat::Tuple(items) => {
                Pat::Tuple(items.iter().map(|p| self.lower_pat(p)).collect())
            }
            hir::Pat::List(items) => {
                Pat::List(items.iter().map(|p| self.lower_pat(p)).collect())
            }
            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Pat> = args.iter().map(|p| self.lower_pat(p)).collect();
                match (&*name, lowered.len()) {
                    ("Nil", 0) => Pat::ListNil,
                    ("True", 0) => Pat::Lit(Lit::Bool(true)),
                    ("False", 0) => Pat::Lit(Lit::Bool(false)),
                    ("Cons", 2) => {
                        let mut it = lowered.into_iter();
                        Pat::ListCons(Box::new(it.next().unwrap()), Box::new(it.next().unwrap()))
                    }
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

    /// A built-in constructor used as a value / under-applied: `Cons` becomes
    /// `\h. \t. cons h t`, `Nil` the empty list, `True`/`False` bool literals.
    fn eta_ctor(&self, name: &str) -> Term {
        match name {
            "Nil" => Term::List(vec![]),
            "True" => Term::Lit(Lit::Bool(true)),
            "False" => Term::Lit(Lit::Bool(false)),
            "Cons" => {
                let h = hir::VarId::fresh();
                let t = hir::VarId::fresh();
                Term::Lam(
                    h,
                    Rc::new(Term::Lam(
                        t,
                        Rc::new(Term::ListCons(
                            Rc::new(Term::Var(h)),
                            Rc::new(Term::Var(t)),
                        )),
                    )),
                )
            }
            _ => Term::Ctor(InternedString::from(name), vec![]),
        }
    }

    /// `\a. \b. prim(a, b)` — used when a primitive is referenced without (or with
    /// the wrong number of) arguments.
    fn eta_prim(&self, op: Prim) -> Term {
        let vars: Vec<Var> = (0..op.arity()).map(|_| hir::VarId::fresh()).collect();
        let body = Term::Prim(op, vars.iter().map(|v| Term::Var(*v)).collect());
        vars.into_iter()
            .rev()
            .fold(body, |acc, v| Term::Lam(v, Rc::new(acc)))
    }
}

fn collect_pat_vars(pat: &hir::LPat, out: &mut Vec<Var>) {
    match pat.value() {
        hir::Pat::Var(id) => out.push(*id.value()),
        hir::Pat::As(id, sub) => {
            out.push(*id.value());
            collect_pat_vars(sub, out);
        }
        hir::Pat::Tuple(items) | hir::Pat::List(items) | hir::Pat::Cons(_, items) => {
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
            "+", "-", "*", "/", "%", "^", "==", "!=", "<", ">", "<=", ">=", "&&", "||", "neg",
            "!", "print", "println",
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
                term: Term::Lam(b, Rc::new(Term::Var(b))),
            }],
            entry: Some(a),
            ..Default::default()
        };
        // `a` is seen first (as the def name) -> v0; `b` -> v1.
        assert_eq!(prog.pretty(), "v0 = (\\v1. v1)\nentry: v0\n");
    }
}
