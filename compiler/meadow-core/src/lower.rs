//! **Lowering: HIR to core.**
//!
//! Where the types come from. Inference has already annotated every HIR node
//! and recorded what each binding generalized over
//! ([`meadow_infer::Generalized`]); this pass copies that onto the core term,
//! so that what comes out is checkable on its own — see [`crate::lint`].
//!
//! Two things need explaining, because they are the only places the
//! translation is not a transcription.
//!
//! * **A generalized binding becomes a [`Term::TyLam`].** Its binders are the
//!   arena variables inference quantified over, and the annotations inside its
//!   body mention exactly those, because both come from the same inference run.
//!
//! * **Every mention of a polymorphic name becomes a [`Term::TyApp`].** The
//!   type arguments are not recorded anywhere by inference — instantiation
//!   replaced them with fresh variables and unified those — so they are
//!   recovered by matching the binding's scheme against the type the mention
//!   was inferred at. That is a one-way match, not unification: the occurrence
//!   type is already a substitution instance of the scheme, and the match
//!   reads the substitution back off it.

use crate::*;

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
    /// What each binding generalized over, for the bindings of this unit —
    /// the binders of the `TyLam` it becomes.
    generalized: &'a HashMap<Var, Generalized>,
    /// Every polymorphic name in scope, this unit's and its dependencies',
    /// so a mention of one can be given its type arguments.
    schemes: &'a HashMap<Var, Scheme>,
    /// Named-field order per constructor, accumulated across `lower_module` calls.
    pub ctor_fields: HashMap<InternedString, Vec<InternedString>>,
    /// Desugaring invents variables -- a scrutinee to bind, an eta-expansion's
    /// parameters -- and they belong to the unit being lowered, so this carries
    /// on from where resolution left off rather than starting anywhere.
    vars: hir::VarIdGen,
}

impl<'a> Lowerer<'a> {
    pub fn new(
        prims: &'a HashMap<Var, Prim>,
        names: &'a HashMap<Var, InternedString>,
        effect_ops: &'a HashMap<Var, (InternedString, InternedString)>,
        types: &'a TypeTable,
        ctor_arity: &'a HashMap<InternedString, usize>,
        generalized: &'a HashMap<Var, Generalized>,
        schemes: &'a HashMap<Var, Scheme>,
        vars: hir::VarIdGen,
    ) -> Self {
        Lowerer {
            prims,
            names,
            effect_ops,
            types,
            ctor_arity,
            generalized,
            schemes,
            ctor_fields: HashMap::new(),
            vars,
        }
    }

    /// One past the last `VarId` this unit has handed out, resolution and
    /// lowering together. The unit records it so the next one can stack above.
    pub fn var_end(&self) -> u32 {
        self.vars.end()
    }

    /// Is the node at `id` inferred to have type `BigInt`?
    fn is_bigint(&self, id: hir::NodeId) -> bool {
        matches!(
            self.types.get(id),
            Some(InferType::Con(n, args)) if args.is_empty() && &**n == "BigInt"
        )
    }

    /// The type inference gave the node at `id`.
    ///
    /// A node with no entry is one inference never reached, which happens only
    /// in a unit that already has errors. [`unknown`] stands in, and the
    /// checker lets it pass rather than reporting a second problem caused by
    /// the first.
    fn ty(&self, id: hir::NodeId) -> Ty {
        self.types.get(id).cloned().unwrap_or_else(unknown)
    }

    /// The polytype of a binding of this unit: its `TyLam` binders and the
    /// type over them.
    ///
    /// Inference records a scheme whose quantifiers are numbered, plus the
    /// arena variable behind each one. Core wants the *variables*, because the
    /// annotations inside the binding's body are written in terms of them.
    fn poly_of(&self, v: Var, fallback: hir::NodeId) -> Poly {
        match self.generalized.get(&v) {
            Some(g) => {
                let binders: Vec<TyVar> = g
                    .vars
                    .iter()
                    .zip(&g.scheme.quant)
                    .map(|(id, kind)| TyVar { id: *id, kind: *kind })
                    .collect();
                let map: HashMap<u32, Ty> = binders
                    .iter()
                    .enumerate()
                    .map(|(i, b)| (i as u32, InferType::Var(b.id)))
                    .collect();
                Poly { ty: subst_bound(&g.scheme.ty, &map), binders }
            }
            None => Poly::mono(self.ty(fallback)),
        }
    }

    /// Wrap a generalized binding's body in the type abstraction its polytype
    /// promises. A monomorphic one is left alone.
    fn ty_lam(poly: &Poly, term: Term) -> Term {
        if poly.binders.is_empty() {
            term
        } else {
            Term::TyLam(poly.binders.clone(), Arc::new(term))
        }
    }

    /// A mention of `v`, with its type arguments when it is polymorphic.
    ///
    /// The arguments are recovered by matching the binding's scheme against
    /// the type this mention was inferred at — see the module docs.
    fn mention(&self, v: Var, at: hir::NodeId) -> Term {
        let term = Term::Var(v);
        let Some(scheme) = self.schemes.get(&v) else {
            return term;
        };
        if scheme.quant.is_empty() {
            return term;
        }
        let occurrence = self.ty(at);
        let args = meadow_infer::match_scheme(scheme, &occurrence).unwrap_or_else(|| {
            // Every occurrence *is* an instance, so a failure here is a bug in
            // the matcher rather than in the program. Say so where a test will
            // see it, and carry on with something the checker will wave past.
            debug_assert!(
                false,
                "could not recover type arguments for a mention of `{}`\n  scheme: {:?}\n  at: {:?}",
                self.name_of(v),
                scheme,
                occurrence
            );
            scheme.quant.iter().map(|_| unknown()).collect()
        });
        Term::TyApp(Arc::new(term), args)
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
                        .insert(rd.ctor, rd.fields.iter().map(|(n, _)| *n).collect());
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
            hir::Bind::Fun(name, params, _, body) => {
                let v = *name.value();
                let poly = self.poly_of(v, name.id);
                let term = Self::ty_lam(&poly, self.curry_lam(params, body));
                out.push(Def {
                    var: v,
                    name: self.name_of(v),
                    poly,
                    term,
                });
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs = self.lower_expr(expr);
                match pat.value() {
                    hir::Pat::Var(id) => {
                        let v = *id.value();
                        let poly = self.poly_of(v, pat.id);
                        out.push(Def {
                            var: v,
                            name: self.name_of(v),
                            term: Self::ty_lam(&poly, rhs),
                            poly,
                        });
                    }
                    hir::Pat::Wildcard => {
                        let v = self.vars.fresh();
                        out.push(Def {
                            var: v,
                            name: InternedString::from("_"),
                            poly: Poly::mono(self.ty(pat.id)),
                            term: rhs,
                        });
                    }
                    _ => {
                        // `def (a, b) = e` etc.: bind the value once, then project.
                        let scrut = self.vars.fresh();
                        out.push(Def {
                            var: scrut,
                            name: InternedString::from("$bind"),
                            poly: Poly::mono(self.ty(pat.id)),
                            term: rhs,
                        });
                        let mut binds = Vec::new();
                        self.bind_pat(Term::Var(scrut), pat, &mut binds);
                        for (v, ty, t) in binds {
                            out.push(Def {
                                var: v,
                                name: self.name_of(v),
                                poly: Poly::mono(ty),
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
        for (v, ty, structured) in binders.into_iter().rev() {
            term = self.with_pat_prelude_term(v, structured, term);
            term = Term::Lam(v, ty, Arc::new(term));
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
            for (bv, bty, bt) in binds.into_iter().rev() {
                term = Term::Let(bv, Poly::mono(bty), Arc::new(bt), Arc::new(term));
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
                    self.eta_prim(op, self.ty(id.id))
                } else if let Some(&(eff, opname)) = self.effect_ops.get(&v) {
                    // `get` becomes `\x -> perform Effect.get x`
                    let x = self.vars.fresh();
                    let (arg, ret) = match self.ty(id.id) {
                        InferType::Fun(args, ret, _) => {
                            (args.first().cloned().unwrap_or_else(unknown), *ret)
                        }
                        _ => (unknown(), unknown()),
                    };
                    Term::Lam(
                        x,
                        arg,
                        Arc::new(Term::Perform(
                            eff,
                            opname,
                            Arc::new(Term::Var(x)),
                            ret,
                        )),
                    )
                } else {
                    self.mention(v, id.id)
                }
            }

            hir::Expr::Lam(params, body) => self.curry_lam(params, body),

            hir::Expr::App(func, args) => {
                if let hir::Expr::Var(id) = func.value() {
                    if let Some(&op) = self.prims.get(&*id.value()) {
                        if op.arity() == args.len() {
                            let a = args.iter().map(|a| self.lower_expr(a)).collect();
                            return Term::Prim(op, a, self.ty(expr.id));
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
                // Every layer of a `let` produces what the whole expression
                // does, which is what a structural binder's `case` needs.
                let result = self.ty(expr.id);
                let mut inner = self.lower_expr(body);
                for bind in binds.iter().rev() {
                    inner = self.lower_let_bind(bind, inner, &result);
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
                Term::Case(Arc::new(s), arms, self.ty(expr.id))
            }

            hir::Expr::Tuple(items) => {
                Term::Tuple(items.iter().map(|e| self.lower_expr(e)).collect())
            }
            hir::Expr::Array(items) => {
                let elem = match self.ty(expr.id) {
                    InferType::Con(n, args) if &*n == "Array" && args.len() == 1 => {
                        args[0].clone()
                    }
                    _ => unknown(),
                };
                Term::Array(items.iter().map(|e| self.lower_expr(e)).collect(), elem)
            }
            // `[a; b; c]` is sugar for `Cons a (Cons b (Cons c Nil))` — `List` is
            // an ordinary `Std` data type, so it lowers to plain constructors.
            hir::Expr::List(items) => {
                // Every cell of the chain has the type of the literal itself.
                let ty = self.ty(expr.id);
                let nil = Term::Ctor(InternedString::from("List.Nil"), ty.clone(), vec![]);
                items.iter().rev().fold(nil, |acc, e| {
                    let head = self.lower_expr(e);
                    Term::Ctor(
                        InternedString::from("List.Cons"),
                        ty.clone(),
                        vec![head, acc],
                    )
                })
            }
            hir::Expr::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Term> = args.iter().map(|e| self.lower_expr(e)).collect();
                match (&*name, lowered.len()) {
                    ("Bool.True", 0) => Term::Lit(Lit::Bool(true)),
                    ("Bool.False", 0) => Term::Lit(Lit::Bool(false)),
                    _ => self.ctor(name, lowered, self.ty(expr.id)),
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
            hir::Expr::Field(obj, label) => Term::Sel(
                Arc::new(self.lower_expr(obj)),
                *label.value(),
                self.ty(expr.id),
            ),

            hir::Expr::Handle(body, arms, ret) => {
                let lbody = Arc::new(self.lower_expr(body));
                let clauses = arms
                    .iter()
                    .map(|arm| {
                        let (param, param_ty, refutable) = self.pat_binder(&arm.param);
                        let body = self.with_pat_prelude(param, refutable, &arm.body);
                        HClause {
                            effect: arm.effect,
                            op: arm.op,
                            param,
                            param_ty,
                            resume: *arm.resume.value(),
                            resume_ty: self.ty(arm.resume.id),
                            body,
                        }
                    })
                    .collect();
                let ret = ret.as_ref().map(|(pat, rbody)| {
                    let (v, vty, refutable) = self.pat_binder(pat);
                    (v, vty, Arc::new(self.with_pat_prelude(v, refutable, rbody)))
                });
                Term::Handle {
                    body: lbody,
                    clauses,
                    ret,
                    ty: self.ty(expr.id),
                }
            }

            hir::Expr::Error => Term::Error,
        }
    }

    /// `(binder var, its type, Some(pat) if the pattern is refutable /
    /// structured)`.
    ///
    /// The type is the pattern's, whether the binder was written or invented:
    /// `\(a, b) -> e` binds one variable of the pair's type and takes it apart
    /// underneath.
    fn pat_binder<'p>(&mut self, pat: &'p hir::LPat) -> (Var, Ty, Option<&'p hir::LPat>) {
        let ty = self.ty(pat.id);
        match pat.value() {
            hir::Pat::Var(id) => (*id.value(), ty, None),
            hir::Pat::Wildcard => (self.vars.fresh(), ty, None),
            _ => (self.vars.fresh(), ty, Some(pat)),
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

    fn lower_let_bind(&mut self, bind: &hir::Bind, body: Term, result: &Ty) -> Term {
        match bind {
            hir::Bind::Fun(name, params, _, fbody) => {
                let v = *name.value();
                let poly = self.poly_of(v, name.id);
                let term = Self::ty_lam(&poly, self.curry_lam(params, fbody));
                Term::LetRec(vec![(v, poly, term)], Arc::new(body))
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs = self.lower_expr(expr);
                match pat.value() {
                    hir::Pat::Var(id) => {
                        let v = *id.value();
                        let poly = self.poly_of(v, pat.id);
                        let rhs = Self::ty_lam(&poly, rhs);
                        Term::Let(v, poly, Arc::new(rhs), Arc::new(body))
                    }
                    hir::Pat::Wildcard => Term::Let(
                        self.vars.fresh(),
                        Poly::mono(self.ty(pat.id)),
                        Arc::new(rhs),
                        Arc::new(body),
                    ),
                    // A structural binder: one arm, and the `case` produces
                    // whatever the body does.
                    _ => Term::Case(
                        Arc::new(rhs),
                        vec![(self.lower_pat(pat), body)],
                        result.clone(),
                    ),
                }
            }
            hir::Bind::Error => body,
        }
    }

    /// Build `(v, access)` pairs for every variable an irrefutable pattern binds,
    /// where `access` is a term extracting that piece from `scrut`.
    fn bind_pat(&mut self, scrut: Term, pat: &hir::LPat, out: &mut Vec<(Var, Ty, Term)>) {
        match pat.value() {
            hir::Pat::Wildcard | hir::Pat::Unit | hir::Pat::Lit(_) => {}
            hir::Pat::Var(id) => out.push((*id.value(), self.ty(pat.id), scrut)),
            // Types are erased here; the pattern under the annotation is all
            // that binds anything.
            hir::Pat::Ann(inner, _) => self.bind_pat(scrut, inner, out),
            hir::Pat::As(id, sub) => {
                out.push((*id.value(), self.ty(pat.id), scrut.clone()));
                self.bind_pat(scrut, sub, out);
            }
            hir::Pat::Tuple(items) => {
                for (i, p) in items.iter().enumerate() {
                    self.bind_pat(Term::Proj(Arc::new(scrut.clone()), i), p, out);
                }
            }
            hir::Pat::Record(fields, _) => {
                for (label, p) in fields {
                    let field = Term::Sel(
                        Arc::new(scrut.clone()),
                        *label.value(),
                        self.ty(p.id),
                    );
                    self.bind_pat(field, p, out);
                }
            }
            hir::Pat::Error => {}
            // Refutable in an irrefutable position: fall back to a single-arm Case.
            hir::Pat::Cons(..) | hir::Pat::List(..) | hir::Pat::Array(..) => {
                let mut inner = Vec::new();
                collect_pat_vars(pat, &mut inner);
                let core_pat = self.lower_pat(pat);
                for (v, vty) in inner {
                    let ty = self.ty(vty);
                    out.push((
                        v,
                        ty.clone(),
                        Term::Case(
                            Arc::new(scrut.clone()),
                            vec![(core_pat.clone(), Term::Var(v))],
                            ty,
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
            hir::Pat::Var(id) => Pat::Var(*id.value(), self.ty(pat.id)),
            // Types are gone by here; the annotation did its work in inference.
            hir::Pat::Ann(inner, _) => self.lower_pat(inner),
            hir::Pat::As(id, sub) => Pat::As(
                *id.value(),
                self.ty(pat.id),
                Box::new(self.lower_pat(sub)),
            ),
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
                let nil = Pat::Ctor(InternedString::from("List.Nil"), vec![]);
                items.iter().rev().fold(nil, |acc, p| {
                    let head = self.lower_pat(p);
                    Pat::Ctor(InternedString::from("List.Cons"), vec![head, acc])
                })
            }
            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                let lowered: Vec<Pat> = args.iter().map(|p| self.lower_pat(p)).collect();
                match (&*name, lowered.len()) {
                    ("Bool.True", 0) => Pat::Lit(Lit::Bool(true)),
                    ("Bool.False", 0) => Pat::Lit(Lit::Bool(false)),
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
    fn ctor(&mut self, name: InternedString, args: Vec<Term>, ty: Ty) -> Term {
        let arity = self.ctor_arity.get(&name).copied().unwrap_or(args.len());
        if args.len() >= arity {
            return Term::Ctor(name, ty, args);
        }
        // Under-applied: `\(x : T) … -> K … x …`, with the parameter types and
        // the result read off the function type this mention was inferred at.
        let (params, result) = peel_arrows(&ty, arity - args.len());
        let extra: Vec<Var> = (args.len()..arity).map(|_| self.vars.fresh()).collect();
        let mut all = args;
        all.extend(extra.iter().map(|v| Term::Var(*v)));
        let body = Term::Ctor(name, result, all);
        extra
            .into_iter()
            .zip(params)
            .rev()
            .fold(body, |acc, (v, t)| Term::Lam(v, t, Arc::new(acc)))
    }

    /// `\a. \b. prim(a, b)` — used when a primitive is referenced without (or with
    /// the wrong number of) arguments.
    fn eta_prim(&mut self, op: Prim, ty: Ty) -> Term {
        let (params, result) = peel_arrows(&ty, op.arity());
        let vars: Vec<Var> = (0..op.arity()).map(|_| self.vars.fresh()).collect();
        let body = Term::Prim(op, vars.iter().map(|v| Term::Var(*v)).collect(), result);
        vars.into_iter()
            .zip(params)
            .rev()
            .fold(body, |acc, (v, t)| Term::Lam(v, t, Arc::new(acc)))
    }
}

/// Every variable an irrefutable pattern binds, with the node whose inferred
/// type is that variable's.
fn collect_pat_vars(pat: &hir::LPat, out: &mut Vec<(Var, hir::NodeId)>) {
    match pat.value() {
        hir::Pat::Var(id) => out.push((*id.value(), pat.id)),
        hir::Pat::As(id, sub) => {
            out.push((*id.value(), pat.id));
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

/// Split a function type into `n` parameter types and what is left.
///
/// Used where lowering invents a lambda — an under-applied constructor, a
/// primitive mentioned without arguments — and has to annotate parameters it
/// did not get from the source.
fn peel_arrows(ty: &Ty, n: usize) -> (Vec<Ty>, Ty) {
    let mut params = Vec::with_capacity(n);
    let mut rest = ty.clone();
    for _ in 0..n {
        match rest {
            InferType::Fun(args, ret, _) => {
                params.push(args.first().cloned().unwrap_or_else(unknown));
                rest = *ret;
            }
            _ => {
                params.push(unknown());
                rest = unknown();
            }
        }
    }
    (params, rest)
}
