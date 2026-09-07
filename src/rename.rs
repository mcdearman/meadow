//! Name resolution: `ast` -> `hir`.
//!
//! Turns surface identifiers into [`VarId`]s and stamps every HIR node with a
//! [`hir::NodeId`] (via [`Resolver::node`]) so later passes can hang side tables
//! off the tree.
//!
//! The resolver knows nothing about packages or interactivity. A caller feeds it
//! the modules of one compilation unit; if the unit has several modules that may
//! refer to each other, the caller first [`Resolver::declare_toplevel`]s every
//! module so the bodies resolve against the union of their top-level names.

use crate::{
    ast,
    diagnostics::Diagnostic,
    hir::{self, NodeIdGen, VarId},
    intern::InternedString,
    span::Span,
};
use itertools::Itertools;
use std::collections::HashMap;

/// Primitive operators, in the order their [`VarId`]s are handed out by
/// [`Resolver::with_prelude`]. `infer` builds matching type schemes by index.
pub(crate) const PRIMS: &[&str] = &[
    "print", "println", "+", "-", "*", "/", "%", "^", "==", "!=", "<", ">", "<=", ">=", "&&", "||",
    "neg", "!",
];

/// What the resolver remembers about a data / record constructor, enough to
/// desugar named-field syntax and reject unknown constructors.
#[derive(Debug, Clone)]
pub struct CtorInfo {
    pub arity: usize,
    /// `Some` for record constructors and named `data` variants.
    pub field_names: Option<Vec<InternedString>>,
}

#[derive(Debug, Clone)]
pub struct Resolver {
    filename: String,
    /// Lexical scope stack: `(name, id)`, innermost last.
    scope: Vec<(InternedString, VarId)>,
    /// Every id we've ever bound, for debugging / pretty-printing.
    names: HashMap<VarId, InternedString>,
    /// Top-level names bound ahead of time by [`declare_toplevel`], so mutually
    /// recursive definitions resolve to a single id.
    predeclared: HashMap<InternedString, VarId>,
    /// Type constructors in scope: name -> arity. Seeded with the builtins.
    tycons: HashMap<InternedString, usize>,
    /// Data / record constructors in scope.
    ctors: HashMap<InternedString, CtorInfo>,
    /// Type variables of the `data` / `record` decl currently being resolved.
    tyvars: Vec<(InternedString, VarId)>,
    ids: NodeIdGen,
    /// True only while resolving a module-level `Decl` (not nested in an expr).
    toplevel: bool,
    errors: Vec<Diagnostic>,
}

/// Constructors that are always available (see `infer::builtin`/`core`).
const BUILTIN_CTORS: &[&str] = &["Nil", "Cons", "True", "False"];

impl Resolver {
    pub fn new(filename: impl Into<String>) -> Self {
        let mut tycons = HashMap::new();
        for (name, arity) in [("Int", 0), ("String", 0), ("Bool", 0), ("Unit", 0), ("List", 1)] {
            tycons.insert(InternedString::from(name), arity);
        }
        Resolver {
            filename: filename.into(),
            scope: Vec::new(),
            names: HashMap::new(),
            predeclared: HashMap::new(),
            tycons,
            ctors: HashMap::new(),
            tyvars: Vec::new(),
            ids: NodeIdGen::new(),
            toplevel: false,
            errors: Vec::new(),
        }
    }

    pub fn with_prelude(filename: impl Into<String>) -> Self {
        let mut r = Resolver::new(filename);
        for name in PRIMS {
            r.bind(InternedString::from(*name));
        }
        r
    }

    // --- scope management ---------------------------------------------------

    fn mark(&self) -> usize {
        self.scope.len()
    }

    fn reset(&mut self, mark: usize) {
        self.scope.truncate(mark);
    }

    fn bind(&mut self, name: InternedString) -> VarId {
        let id = VarId::fresh();
        self.names.insert(id, name);
        self.scope.push((name, id));
        id
    }

    fn lookup(&self, name: InternedString) -> Option<VarId> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
    }

    /// At the top level, reuse a [`declare_toplevel`] id if there is one; otherwise
    /// bind fresh. Nested definitions always bind fresh (so they can shadow).
    fn bind_defn(&mut self, name: InternedString) -> VarId {
        if self.toplevel {
            if let Some(&id) = self.predeclared.get(&name) {
                return id;
            }
        }
        self.bind(name)
    }

    /// Bring a dependency's exported binding into scope under its own id.
    pub fn import(&mut self, name: InternedString, id: VarId) {
        self.names.insert(id, name);
        self.scope.push((name, id));
    }

    fn node<T>(&mut self, value: T, span: Span) -> hir::Node<T> {
        hir::Node::new(self.ids.fresh(), value, span)
    }

    // --- public accessors ------------------------------------------------------

    pub fn id_count(&self) -> usize {
        self.ids.count()
    }

    /// The `(name, id)` pairs bound for [`PRIMS`], in declaration order. `infer`
    /// and `core` use these to attach primitive schemes / lower operator calls.
    pub fn prelude_bindings(&self) -> Vec<(InternedString, VarId)> {
        self.scope.iter().take(PRIMS.len()).copied().collect()
    }

    pub fn names(&self) -> &HashMap<VarId, InternedString> {
        &self.names
    }

    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }

    pub fn take_errors(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.errors)
    }

    // --- declaration pre-pass ------------------------------------------------

    /// Pre-bind the top-level names of a module so definitions (in this module or a
    /// sibling module of the same package) can refer to each other regardless of
    /// order. Call once per module before resolving any bodies.
    pub fn declare_toplevel(&mut self, decls: &[ast::LDecl]) {
        for d in decls {
            if let ast::Decl::Bind(b) = d.value() {
                match b {
                    ast::Bind::Fun(name, ..) => self.predeclare(*name.value()),
                    ast::Bind::Pat(pat, _) => self.predeclare_pat(pat),
                }
            }
        }
    }

    /// Register the type constructors and data/record constructors of a module so
    /// bodies (and other modules of the same unit) can refer to them, and so type
    /// expressions can be checked. Call before [`resolve_module`].
    pub fn declare_types(&mut self, decls: &[ast::LDecl]) {
        for d in decls {
            match d.value() {
                ast::Decl::Data(dd) => {
                    self.declare_tycon(*dd.name.value(), dd.params.len(), dd.name.span);
                    for v in &dd.variants {
                        let (arity, field_names) = match &v.fields {
                            ast::VariantFields::Positional(ts) => (ts.len(), None),
                            ast::VariantFields::Named(fs) => {
                                self.check_dup_fields(fs);
                                (fs.len(), Some(fs.iter().map(|(n, _)| *n.value()).collect()))
                            }
                        };
                        self.declare_ctor(*v.name.value(), arity, field_names, v.name.span);
                    }
                }
                ast::Decl::Record(rd) => {
                    self.declare_tycon(*rd.name.value(), rd.params.len(), rd.name.span);
                    self.check_dup_fields(&rd.fields);
                    let fields = rd.fields.iter().map(|(n, _)| *n.value()).collect();
                    self.declare_ctor(*rd.name.value(), rd.fields.len(), Some(fields), rd.name.span);
                }
                _ => {}
            }
        }
    }

    /// Re-register the type + data constructors of an already-resolved dependency
    /// (or an earlier REPL line). Trusted, so no duplicate diagnostics.
    pub fn import_types(&mut self, decls: &[hir::LDecl]) {
        for d in decls {
            match d.value() {
                hir::Decl::Data(dd) => {
                    self.tycons.insert(dd.name, dd.params.len());
                    for v in &dd.variants {
                        let (arity, field_names) = match &v.fields {
                            hir::VariantFields::Positional(ts) => (ts.len(), None),
                            hir::VariantFields::Named(fs) => {
                                (fs.len(), Some(fs.iter().map(|(n, _)| *n).collect()))
                            }
                        };
                        self.ctors.insert(v.name, CtorInfo { arity, field_names });
                    }
                }
                hir::Decl::Record(rd) => {
                    self.tycons.insert(rd.name, rd.params.len());
                    self.ctors.insert(
                        rd.name,
                        CtorInfo {
                            arity: rd.fields.len(),
                            field_names: Some(rd.fields.iter().map(|(n, _)| *n).collect()),
                        },
                    );
                }
                _ => {}
            }
        }
    }

    fn declare_tycon(&mut self, name: InternedString, arity: usize, span: Span) {
        if self.tycons.insert(name, arity).is_some() {
            self.error(
                format!("type `{name}` is already defined"),
                "duplicate type".to_string(),
                span,
            );
        }
    }

    fn declare_ctor(
        &mut self,
        name: InternedString,
        arity: usize,
        field_names: Option<Vec<InternedString>>,
        span: Span,
    ) {
        if self
            .ctors
            .insert(name, CtorInfo { arity, field_names })
            .is_some()
        {
            self.error(
                format!("constructor `{name}` is already defined"),
                "duplicate constructor".to_string(),
                span,
            );
        }
    }

    fn check_dup_fields(&mut self, fields: &[(ast::Ident, ast::LType)]) {
        let mut seen = std::collections::HashSet::new();
        for (n, _) in fields {
            if !seen.insert(*n.value()) {
                self.error(
                    format!("duplicate field `{}`", n.value()),
                    "already declared".to_string(),
                    n.span,
                );
            }
        }
    }

    fn is_known_ctor(&self, name: InternedString) -> bool {
        self.ctors.contains_key(&name) || BUILTIN_CTORS.contains(&&*name)
    }

    fn ctor_field_order(&self, name: InternedString) -> Option<Vec<InternedString>> {
        self.ctors.get(&name).and_then(|c| c.field_names.clone())
    }

    fn predeclare(&mut self, name: InternedString) {
        if !self.predeclared.contains_key(&name) {
            let id = self.bind(name);
            self.predeclared.insert(name, id);
        }
    }

    fn predeclare_pat(&mut self, pat: &ast::LPat) {
        match pat.value() {
            ast::Pat::Var(n) => self.predeclare(*n.value()),
            ast::Pat::As(n, p) => {
                self.predeclare(*n.value());
                self.predeclare_pat(p);
            }
            ast::Pat::Tuple(ps) | ast::Pat::List(ps) | ast::Pat::Cons(_, ps) => {
                ps.iter().for_each(|p| self.predeclare_pat(p))
            }
            ast::Pat::Record(fs, _) => fs.iter().for_each(|(_, p)| self.predeclare_pat(p)),
            _ => {}
        }
    }

    // --- module ------------------------------------------------------------

    /// Resolve a module's bodies. The caller must already have run
    /// [`declare_toplevel`] for this module (and any siblings).
    pub fn resolve_module(&mut self, module: &ast::LModule) -> hir::LModule {
        let decls = module
            .value()
            .decls
            .iter()
            .map(|decl| self.resolve_decl(decl))
            .collect_vec();
        self.node(
            hir::Module {
                name: module.value().name,
                decls,
            },
            module.span,
        )
    }

    fn resolve_decl(&mut self, decl: &ast::LDecl) -> hir::LDecl {
        match decl.value() {
            ast::Decl::Bind(bind) => {
                self.toplevel = true;
                let b = self.resolve_bind(bind);
                self.toplevel = false;
                self.node(hir::Decl::Bind(b), decl.span)
            }
            ast::Decl::Use(path) => {
                let segs = path.iter().map(|s| *s.value()).collect();
                self.node(hir::Decl::Use(segs), decl.span)
            }
            ast::Decl::Data(dd) => {
                let params = self.bind_tyvars(&dd.params);
                let variants = dd
                    .variants
                    .iter()
                    .map(|v| {
                        let fields = match &v.fields {
                            ast::VariantFields::Positional(ts) => hir::VariantFields::Positional(
                                ts.iter().map(|t| self.resolve_ty(t)).collect(),
                            ),
                            ast::VariantFields::Named(fs) => hir::VariantFields::Named(
                                fs.iter()
                                    .map(|(n, t)| (*n.value(), self.resolve_ty(t)))
                                    .collect(),
                            ),
                        };
                        hir::Variant {
                            name: *v.name.value(),
                            fields,
                        }
                    })
                    .collect();
                self.tyvars.clear();
                self.node(
                    hir::Decl::Data(hir::DataDecl {
                        name: *dd.name.value(),
                        params,
                        variants,
                    }),
                    decl.span,
                )
            }
            ast::Decl::Record(rd) => {
                let params = self.bind_tyvars(&rd.params);
                let fields = rd
                    .fields
                    .iter()
                    .map(|(n, t)| (*n.value(), self.resolve_ty(t)))
                    .collect();
                self.tyvars.clear();
                self.node(
                    hir::Decl::Record(hir::RecordDecl {
                        name: *rd.name.value(),
                        params,
                        fields,
                    }),
                    decl.span,
                )
            }
        }
    }

    // --- type expressions ---------------------------------------------------

    fn bind_tyvars(&mut self, params: &[ast::Ident]) -> Vec<hir::Ident> {
        params
            .iter()
            .map(|p| {
                let name = *p.value();
                let id = VarId::fresh();
                self.names.insert(id, name);
                self.tyvars.push((name, id));
                self.node(id, p.span)
            })
            .collect()
    }

    fn resolve_ty(&mut self, t: &ast::LType) -> hir::LTypeExpr {
        match t.value() {
            ast::TypeExpr::Var(n) => {
                let name = *n.value();
                let id = self
                    .tyvars
                    .iter()
                    .rev()
                    .find(|(nm, _)| *nm == name)
                    .map(|(_, id)| *id)
                    .unwrap_or_else(|| {
                        self.error(
                            format!("unbound type variable `{name}`"),
                            "not a parameter of this type".to_string(),
                            n.span,
                        );
                        VarId::fresh()
                    });
                let v = self.node(id, n.span);
                self.node(hir::TypeExpr::Var(v), t.span)
            }
            ast::TypeExpr::Con(n, args) => {
                let name = *n.value();
                match self.tycons.get(&name).copied() {
                    Some(arity) if arity == args.len() => {}
                    Some(arity) => self.error(
                        format!(
                            "type `{name}` takes {arity} argument(s), got {}",
                            args.len()
                        ),
                        "wrong number of type arguments".to_string(),
                        n.span,
                    ),
                    None => self.error(
                        format!("unknown type `{name}`"),
                        "not defined".to_string(),
                        n.span,
                    ),
                }
                let rargs = args.iter().map(|a| self.resolve_ty(a)).collect();
                self.node(hir::TypeExpr::Con(name, rargs), t.span)
            }
            ast::TypeExpr::Fun(ps, r) => {
                let rps = ps.iter().map(|p| self.resolve_ty(p)).collect();
                let rr = self.resolve_ty(r);
                self.node(hir::TypeExpr::Fun(rps, rr), t.span)
            }
            ast::TypeExpr::Tuple(ts) => {
                let rts = ts.iter().map(|x| self.resolve_ty(x)).collect();
                self.node(hir::TypeExpr::Tuple(rts), t.span)
            }
            ast::TypeExpr::List(x) => {
                let rx = self.resolve_ty(x);
                self.node(hir::TypeExpr::List(rx), t.span)
            }
        }
    }

    fn resolve_bind(&mut self, bind: &ast::Bind) -> hir::Bind {
        match bind {
            ast::Bind::Pat(pat, expr) => {
                let rpat = self.resolve_pat(pat);
                let rexpr = self.resolve_expr(expr);
                hir::Bind::Pat(rpat, rexpr)
            }
            ast::Bind::Fun(name, params, body) => {
                let id = self.bind_defn(*name.value());
                let name_node = self.node(id, name.span);
                let mark = self.mark();
                let rparams = params
                    .iter()
                    .map(|p| {
                        let pid = self.bind(*p.value());
                        self.node(pid, p.span)
                    })
                    .collect_vec();
                let rbody = self.resolve_expr(body);
                self.reset(mark);
                hir::Bind::Fun(name_node, rparams, rbody)
            }
        }
    }

    // --- expressions -----------------------------------------------------------

    fn resolve_expr(&mut self, expr: &ast::LExpr) -> hir::LExpr {
        self.toplevel = false;
        match expr.value() {
            ast::Expr::Lit(lit) => {
                let l = self.resolve_lit(lit);
                self.node(hir::Expr::Lit(l), expr.span)
            }
            ast::Expr::Unit => self.node(hir::Expr::Unit, expr.span),
            ast::Expr::Var(name) => {
                if let Some(id) = self.lookup(*name.value()) {
                    let v = self.node(id, name.span);
                    self.node(hir::Expr::Var(v), expr.span)
                } else {
                    self.error(
                        format!("undefined variable: {}", name.value()),
                        "not found in this scope".to_string(),
                        name.span,
                    );
                    self.node(hir::Expr::Error, expr.span)
                }
            }
            ast::Expr::Lam(params, body) => {
                let mark = self.mark();
                let rparams = params.iter().map(|p| self.resolve_pat(p)).collect_vec();
                let rbody = self.resolve_expr(body);
                self.reset(mark);
                self.node(hir::Expr::Lam(rparams, rbody), expr.span)
            }
            ast::Expr::App(func, args) => {
                let rf = self.resolve_expr(func);
                let ra = args.iter().map(|a| self.resolve_expr(a)).collect_vec();
                self.node(hir::Expr::App(rf, ra), expr.span)
            }
            ast::Expr::Let(binds, body) => {
                let mark = self.mark();
                let rbinds = binds.iter().map(|b| self.resolve_bind(b)).collect_vec();
                let rbody = self.resolve_expr(body);
                self.reset(mark);
                self.node(hir::Expr::Let(rbinds, rbody), expr.span)
            }
            ast::Expr::If(cond, then_branch, else_branch) => {
                let rc = self.resolve_expr(cond);
                let rt = self.resolve_expr(then_branch);
                let re = self.resolve_expr(else_branch);
                self.node(hir::Expr::If(rc, rt, re), expr.span)
            }
            ast::Expr::Match(scrut, arms) => {
                let rs = self.resolve_expr(scrut);
                let rarms = arms
                    .iter()
                    .map(|(pat, arm)| {
                        let mark = self.mark();
                        let rp = self.resolve_pat(pat);
                        let ra = self.resolve_expr(arm);
                        self.reset(mark);
                        (rp, ra)
                    })
                    .collect_vec();
                self.node(hir::Expr::Match(rs, rarms), expr.span)
            }
            ast::Expr::UnOp(op, operand) => {
                let sym = InternedString::from(op.value().to_string());
                let f = self.lookup(sym).expect("prelude never truncated");
                let ro = self.resolve_expr(operand);
                let fv = self.node(f, op.span);
                let callee = self.node(hir::Expr::Var(fv), op.span);
                self.node(hir::Expr::App(callee, vec![ro]), expr.span)
            }
            ast::Expr::BinOp(op, lhs, rhs) => {
                let sym = InternedString::from(op.value().to_string());
                let f = self.lookup(sym).expect("prelude never truncated");
                let rl = self.resolve_expr(lhs);
                let rr = self.resolve_expr(rhs);
                let fv = self.node(f, op.span);
                let callee = self.node(hir::Expr::Var(fv), op.span);
                self.node(hir::Expr::App(callee, vec![rl, rr]), expr.span)
            }
            ast::Expr::Tuple(exprs) => {
                let res = exprs.iter().map(|e| self.resolve_expr(e)).collect_vec();
                self.node(hir::Expr::Tuple(res), expr.span)
            }
            ast::Expr::List(exprs) => {
                let res = exprs.iter().map(|e| self.resolve_expr(e)).collect_vec();
                self.node(hir::Expr::List(res), expr.span)
            }
            ast::Expr::Cons(name, args) => {
                let cname = *name.value();
                // `Ctor { f = e, ... }` — reorder named fields to declared order.
                if let [only] = &args[..] {
                    if let ast::Expr::Record(fields, base) = only.value() {
                        if base.is_none() {
                            if let Some(order) = self.ctor_field_order(cname) {
                                return self
                                    .resolve_named_ctor(expr.span, cname, name.span, fields, &order);
                            }
                        }
                    }
                }
                if !self.is_known_ctor(cname) {
                    self.error(
                        format!("unknown constructor `{cname}`"),
                        "not a known constructor".to_string(),
                        name.span,
                    );
                }
                let label = self.node(cname, name.span);
                let ra = args.iter().map(|a| self.resolve_expr(a)).collect_vec();
                self.node(hir::Expr::Cons(label, ra), expr.span)
            }
            ast::Expr::Record(fields, base) => {
                let rfields = fields
                    .iter()
                    .map(|(label, val)| {
                        let l = self.node(*label.value(), label.span);
                        let v = self.resolve_expr(val);
                        (l, v)
                    })
                    .collect_vec();
                let rbase = base.as_ref().map(|b| self.resolve_expr(b));
                self.node(hir::Expr::Record(rfields, rbase), expr.span)
            }
            ast::Expr::Field(obj, label) => {
                let o = self.resolve_expr(obj);
                let l = self.node(*label.value(), label.span);
                self.node(hir::Expr::Field(o, l), expr.span)
            }
        }
    }

    // --- patterns ------------------------------------------------------------

    fn resolve_pat(&mut self, pat: &ast::LPat) -> hir::LPat {
        match pat.value() {
            ast::Pat::Wildcard => self.node(hir::Pat::Wildcard, pat.span),
            ast::Pat::Unit => self.node(hir::Pat::Unit, pat.span),
            ast::Pat::Var(name) => {
                let id = self.bind_defn(*name.value());
                let v = self.node(id, name.span);
                self.node(hir::Pat::Var(v), pat.span)
            }
            ast::Pat::Lit(lit) => {
                let l = self.resolve_lit(lit);
                self.node(hir::Pat::Lit(l), pat.span)
            }
            ast::Pat::As(name, sub) => {
                let id = self.bind_defn(*name.value());
                let v = self.node(id, name.span);
                let rsub = self.resolve_pat(sub);
                self.node(hir::Pat::As(v, rsub), pat.span)
            }
            ast::Pat::Cons(name, args) => {
                let cname = *name.value();
                // `Ctor { f, g = p }` — reorder to declared order, missing => wildcard.
                if let [only] = &args[..] {
                    if let ast::Pat::Record(fields, _) = only.value() {
                        if let Some(order) = self.ctor_field_order(cname) {
                            return self
                                .resolve_named_ctor_pat(pat.span, cname, name.span, fields, &order);
                        }
                    }
                }
                if !self.is_known_ctor(cname) {
                    self.error(
                        format!("unknown constructor `{cname}`"),
                        "not a known constructor".to_string(),
                        name.span,
                    );
                }
                let label = self.node(cname, name.span);
                let ra = args.iter().map(|p| self.resolve_pat(p)).collect_vec();
                self.node(hir::Pat::Cons(label, ra), pat.span)
            }
            ast::Pat::Tuple(pats) => {
                let rp = pats.iter().map(|p| self.resolve_pat(p)).collect_vec();
                self.node(hir::Pat::Tuple(rp), pat.span)
            }
            ast::Pat::List(pats) => {
                let rp = pats.iter().map(|p| self.resolve_pat(p)).collect_vec();
                self.node(hir::Pat::List(rp), pat.span)
            }
            ast::Pat::Record(fields, open) => {
                let rfields = fields
                    .iter()
                    .map(|(label, p)| {
                        let l = self.node(*label.value(), label.span);
                        let rp = self.resolve_pat(p);
                        (l, rp)
                    })
                    .collect_vec();
                self.node(hir::Pat::Record(rfields, *open), pat.span)
            }
        }
    }

    /// `Ctor { f2 = e2, f1 = e1 }` -> positional `Ctor e1 e2` in declared order.
    fn resolve_named_ctor(
        &mut self,
        span: Span,
        name: InternedString,
        name_span: Span,
        fields: &[(ast::Ident, ast::LExpr)],
        order: &[InternedString],
    ) -> hir::LExpr {
        let mut provided: HashMap<InternedString, &ast::LExpr> = HashMap::new();
        for (label, val) in fields {
            if provided.insert(*label.value(), val).is_some() {
                self.error(
                    format!("field `{}` given twice", label.value()),
                    "duplicate field".to_string(),
                    label.span,
                );
            }
        }
        let mut exprs = Vec::with_capacity(order.len());
        for fname in order {
            match provided.remove(fname) {
                Some(e) => exprs.push(self.resolve_expr(e)),
                None => {
                    self.error(
                        format!("missing field `{fname}` for `{name}`"),
                        "required here".to_string(),
                        span,
                    );
                    exprs.push(self.node(hir::Expr::Error, span));
                }
            }
        }
        for extra in provided.keys() {
            self.error(
                format!("`{name}` has no field `{extra}`"),
                "unknown field".to_string(),
                name_span,
            );
        }
        let label = self.node(name, name_span);
        self.node(hir::Expr::Cons(label, exprs), span)
    }

    fn resolve_named_ctor_pat(
        &mut self,
        span: Span,
        name: InternedString,
        name_span: Span,
        fields: &[(ast::Ident, ast::LPat)],
        order: &[InternedString],
    ) -> hir::LPat {
        let mut provided: HashMap<InternedString, &ast::LPat> = HashMap::new();
        for (label, p) in fields {
            if provided.insert(*label.value(), p).is_some() {
                self.error(
                    format!("field `{}` bound twice", label.value()),
                    "duplicate field".to_string(),
                    label.span,
                );
            }
        }
        let mut pats = Vec::with_capacity(order.len());
        for fname in order {
            match provided.remove(fname) {
                Some(p) => pats.push(self.resolve_pat(p)),
                None => pats.push(self.node(hir::Pat::Wildcard, span)),
            }
        }
        for extra in provided.keys() {
            self.error(
                format!("`{name}` has no field `{extra}`"),
                "unknown field".to_string(),
                name_span,
            );
        }
        let label = self.node(name, name_span);
        self.node(hir::Pat::Cons(label, pats), span)
    }

    fn resolve_lit(&self, lit: &ast::Lit) -> hir::Lit {
        match lit {
            ast::Lit::Int(i) => hir::Lit::Int(*i),
            ast::Lit::String(s) => hir::Lit::String(*s),
        }
    }

    fn error(&mut self, msg: String, label: String, span: Span) {
        self.errors.push(Diagnostic {
            msg,
            filename: self.filename.clone(),
            label: (label, span),
            extra_labels: vec![],
        });
    }
}
