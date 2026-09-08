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

use meadow_ast as ast;
use meadow_diagnostics::Diagnostic;
use meadow_hir::{self as hir, NodeIdGen, PRIMS, VarId};
use meadow_intern::InternedString;
use meadow_span::Span;
use itertools::Itertools;
use std::collections::HashMap;

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
    /// Declared effects: name -> parameter count.
    effects: HashMap<InternedString, usize>,
    /// Operation name -> the effect it belongs to.
    effect_ops: HashMap<InternedString, InternedString>,
    /// Type variables of the `data` / `record` decl currently being resolved.
    tyvars: Vec<(InternedString, VarId)>,
    ids: NodeIdGen,
    /// True only while resolving a module-level `Decl` (not nested in an expr).
    toplevel: bool,
    /// `@pub` bookkeeping. If `any_pub` stays false the unit exports everything
    /// (the historical default); otherwise only `pub_vars` / `pub_types` — plus
    /// `@pub use M (…)` re-exports, which also land in these sets.
    any_pub: bool,
    pub_vars: std::collections::HashSet<VarId>,
    pub_types: std::collections::HashSet<InternedString>,
    /// Active module qualifiers: `Foo` -> its exported value names. Populated by the
    /// driver from this module's `mod` children and `use`d modules; consulted when
    /// resolving `Foo.name`.
    qualifiers: HashMap<InternedString, HashMap<InternedString, VarId>>,
    errors: Vec<Diagnostic>,
}

/// Constructors that are always available (see `infer::ctor_type` / `core`). The
/// `Std` prelude also declares `data List` / `data Bool` with these variants, so a
/// re-declaration of one of these names is tolerated rather than an error.
const BUILTIN_CTORS: &[&str] = &["Nil", "Cons", "True", "False"];

/// Type constructors seeded into every resolver. Like [`BUILTIN_CTORS`], the
/// prelude is allowed to (re-)declare `List` / `Bool` without it counting as a
/// duplicate-definition error.
const BUILTIN_TYCONS: &[&str] = &["Int", "BigInt", "Float", "String", "Bool", "Unit", "List", "Array"];

/// Split a declaration into its attributes and the bare declaration underneath.
/// The parser only ever nests one `Attributed` layer.
fn peel(d: &ast::LDecl) -> (&[ast::Attr], &ast::LDecl) {
    match d.value() {
        ast::Decl::Attributed(attrs, inner) => (attrs, inner),
        _ => (&[], d),
    }
}

fn has_pub(attrs: &[ast::Attr]) -> bool {
    attrs.iter().any(|a| &**a.name.value() == "pub")
}

/// A name is a constructor iff it starts with an uppercase letter.
fn is_ctor_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

impl Resolver {
    pub fn new(filename: impl Into<String>) -> Self {
        let mut tycons = HashMap::new();
        for (name, arity) in [("Int", 0), ("BigInt", 0), ("Float", 0), ("String", 0), ("Bool", 0), ("Unit", 0), ("List", 1), ("Array", 1)] {
            tycons.insert(InternedString::from(name), arity);
        }
        Resolver {
            filename: filename.into(),
            scope: Vec::new(),
            names: HashMap::new(),
            predeclared: HashMap::new(),
            tycons,
            ctors: HashMap::new(),
            effects: HashMap::new(),
            effect_ops: HashMap::new(),
            tyvars: Vec::new(),
            ids: NodeIdGen::new(),
            toplevel: false,
            any_pub: false,
            pub_vars: std::collections::HashSet::new(),
            pub_types: std::collections::HashSet::new(),
            qualifiers: HashMap::new(),
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

    /// Continue node-id numbering from `base`, so every module of a package shares
    /// one dense id space (and one package-wide type table).
    pub fn set_id_base(&mut self, base: usize) {
        self.ids = NodeIdGen::starting_at(base);
    }

    /// Make module `qualifier` reachable as `qualifier.name` in the module about to
    /// be resolved. `values` are its exported bindings. (Types / constructors are
    /// registered globally elsewhere — their names are unique across a program.)
    pub fn activate_module(
        &mut self,
        qualifier: InternedString,
        values: HashMap<InternedString, VarId>,
    ) {
        for (&n, &id) in &values {
            self.names.insert(id, n);
        }
        self.qualifiers.insert(qualifier, values);
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
            let (_, base) = peel(d);
            if let ast::Decl::Bind(b) = base.value() {
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
            let (_, base) = peel(d);
            match base.value() {
                ast::Decl::Data(dd) => {
                    self.declare_tycon(*dd.name.value(), dd.params.len(), dd.name.span);
                    for v in &dd.variants {
                        let (arity, field_names) = match &v.fields {
                            ast::VariantFields::Positional(ts) => (ts.len(), None),
                            ast::VariantFields::Named(fs) => {
                                self.check_dup_fields(fs);
                                (fs.len(), Some(fs.iter().map(|f| *f.name.value()).collect()))
                            }
                        };
                        self.declare_ctor(*v.name.value(), arity, field_names, v.name.span);
                    }
                }
                ast::Decl::Record(rd) => {
                    self.declare_tycon(*rd.name.value(), rd.params.len(), rd.name.span);
                    self.check_dup_fields(&rd.fields);
                    let fields = rd.fields.iter().map(|f| *f.name.value()).collect();
                    self.declare_ctor(*rd.name.value(), rd.fields.len(), Some(fields), rd.name.span);
                }
                ast::Decl::Effect(ed) => {
                    let name = *ed.name.value();
                    // an effect name is also a type constructor of its parameters
                    self.declare_tycon(name, ed.params.len(), ed.name.span);
                    self.effects.insert(name, ed.params.len());
                    for op_field in &ed.ops {
                        let op = *op_field.name.value();
                        if self.effect_ops.insert(op, name).is_some() || self.predeclared.contains_key(&op) {
                            self.error(
                                format!("operation `{op}` is already defined"),
                                "duplicate operation".to_string(),
                                ed.name.span,
                            );
                        }
                        // ops are top-level values (functions) — predeclare them
                        self.predeclare(op);
                    }
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
                hir::Decl::Effect(ed) => {
                    self.tycons.insert(ed.name, ed.params.len());
                    self.effects.insert(ed.name, ed.params.len());
                    for (opname, op, _) in &ed.ops {
                        self.effect_ops.insert(*opname, ed.name);
                        // bring the operation value into scope under its own id
                        self.import(*opname, *op.value());
                    }
                }
                _ => {}
            }
        }
    }

    /// `(op VarId, effect name, op name)` for every declared operation — the
    /// lowerer turns a reference to one into `\x -> perform Effect.op x`.
    pub fn effect_op_vars(&self) -> Vec<(VarId, InternedString, InternedString)> {
        self.effect_ops
            .iter()
            .filter_map(|(op, eff)| {
                self.predeclared
                    .get(op)
                    .or_else(|| self.scope.iter().rev().find(|(n, _)| n == op).map(|(_, id)| id))
                    .map(|id| (*id, *eff, *op))
            })
            .collect()
    }

    fn declare_tycon(&mut self, name: InternedString, arity: usize, span: Span) {
        if self.tycons.insert(name, arity).is_some() && !BUILTIN_TYCONS.contains(&&*name) {
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
            && !BUILTIN_CTORS.contains(&&*name)
        {
            self.error(
                format!("constructor `{name}` is already defined"),
                "duplicate constructor".to_string(),
                span,
            );
        }
    }

    fn check_dup_fields(&mut self, fields: &[ast::Field]) {
        let mut seen = std::collections::HashSet::new();
        for f in fields {
            if !seen.insert(*f.name.value()) {
                self.error(
                    format!("duplicate field `{}`", f.name.value()),
                    "already declared".to_string(),
                    f.name.span,
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
        let (attrs, base) = peel(decl);
        let is_pub = has_pub(attrs);
        if is_pub {
            self.any_pub = true;
        }
        // `@pub use M (a, b, c)` — re-export names already visible in the (flat)
        // package scope.
        if let ast::Decl::Use(u) = base.value() {
            if is_pub {
                for n in &u.names {
                    let name = *n.value();
                    if let Some(id) = self.lookup(name) {
                        self.pub_vars.insert(id);
                    } else if self.tycons.contains_key(&name) {
                        self.pub_types.insert(name);
                    } else {
                        self.error(
                            format!("cannot re-export `{name}`: not found in this scope"),
                            "not defined".to_string(),
                            n.span,
                        );
                    }
                }
            }
        }

        let hir = self.resolve_bare_decl(base);
        if is_pub {
            self.mark_pub(&hir);
        }
        hir
    }

    /// Record a `@pub` declaration's names in the export sets.
    fn mark_pub(&mut self, decl: &hir::LDecl) {
        match decl.value() {
            hir::Decl::Bind(hir::Bind::Fun(name, ..)) => {
                self.pub_vars.insert(*name.value());
            }
            hir::Decl::Bind(hir::Bind::Pat(pat, _)) => {
                let mut ids = Vec::new();
                collect_hir_pat_vars(pat, &mut ids);
                self.pub_vars.extend(ids);
            }
            hir::Decl::Data(dd) => {
                self.pub_types.insert(dd.name);
            }
            hir::Decl::Record(rd) => {
                self.pub_types.insert(rd.name);
            }
            hir::Decl::Effect(ed) => {
                self.pub_types.insert(ed.name);
                for (_, op, _) in &ed.ops {
                    self.pub_vars.insert(*op.value());
                }
            }
            _ => {}
        }
    }

    /// `true` if any `@pub` was seen — the unit then exports only its `@pub`
    /// declarations (and `@pub use` re-exports) instead of everything.
    pub fn has_pub_markers(&self) -> bool {
        self.any_pub
    }

    pub fn is_pub_var(&self, id: VarId) -> bool {
        self.pub_vars.contains(&id)
    }

    /// Every `VarId` marked `@pub` — including `@pub use` re-exports, whose
    /// schemes live in a dependency rather than this unit.
    pub fn pub_var_ids(&self) -> impl Iterator<Item = VarId> + '_ {
        self.pub_vars.iter().copied()
    }

    pub fn is_pub_type(&self, name: InternedString) -> bool {
        self.pub_types.contains(&name)
    }

    fn resolve_bare_decl(&mut self, decl: &ast::LDecl) -> hir::LDecl {
        match decl.value() {
            ast::Decl::Attributed(_, inner) => self.resolve_bare_decl(inner),
            ast::Decl::Bind(bind) => {
                self.toplevel = true;
                let b = self.resolve_bind(bind);
                self.toplevel = false;
                self.node(hir::Decl::Bind(b), decl.span)
            }
            ast::Decl::Use(u) => {
                let segs = u.path.iter().map(|s| *s.value()).collect();
                self.node(hir::Decl::Use(segs), decl.span)
            }
            ast::Decl::Mod(name) => self.node(hir::Decl::Mod(*name.value()), decl.span),
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
                                    .map(|f| (*f.name.value(), self.resolve_ty(&f.ty)))
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
                    .map(|f| (*f.name.value(), self.resolve_ty(&f.ty)))
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
            ast::Decl::Effect(ed) => {
                let params = self.bind_tyvars(&ed.params);
                let ops = ed
                    .ops
                    .iter()
                    .map(|f| {
                        // `declare_types` predeclared the op name as a top-level value
                        let opname = *f.name.value();
                        let id = self
                            .predeclared
                            .get(&opname)
                            .copied()
                            .unwrap_or_else(|| self.bind(opname));
                        let rty = self.resolve_ty(&f.ty);
                        (opname, self.node(id, f.name.span), rty)
                    })
                    .collect();
                self.tyvars.clear();
                self.node(
                    hir::Decl::Effect(hir::EffectDecl {
                        name: *ed.name.value(),
                        params,
                        ops,
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
            ast::TypeExpr::Fun(ps, r, eff) => {
                let rps = ps.iter().map(|p| self.resolve_ty(p)).collect();
                let rr = self.resolve_ty(r);
                let reff = eff.as_ref().map(|e| self.resolve_effect_row(e));
                self.node(hir::TypeExpr::Fun(rps, rr, reff), t.span)
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

    fn resolve_effect_row(&mut self, row: &ast::EffectRow) -> hir::EffectRow {
        let labels = row
            .labels
            .iter()
            .map(|(name, args)| {
                let n = *name.value();
                match self.effects.get(&n).copied() {
                    Some(arity) if arity == args.len() => {}
                    Some(arity) => self.error(
                        format!("effect `{n}` takes {arity} argument(s), got {}", args.len()),
                        "wrong number of effect arguments".to_string(),
                        name.span,
                    ),
                    None => self.error(
                        format!("unknown effect `{n}`"),
                        "not declared".to_string(),
                        name.span,
                    ),
                }
                (n, args.iter().map(|a| self.resolve_ty(a)).collect())
            })
            .collect();
        let tail = row.tail.as_ref().map(|t| {
            let name = *t.value();
            let id = self
                .tyvars
                .iter()
                .rev()
                .find(|(nm, _)| *nm == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| {
                    self.error(
                        format!("unbound effect variable `{name}`"),
                        "not a parameter of this declaration".to_string(),
                        t.span,
                    );
                    VarId::fresh()
                });
            self.node(id, t.span)
        });
        hir::EffectRow { labels, tail }
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

    /// Warn if `q` is not an active module qualifier here (a `mod` child or a
    /// `use`d module). Constructor resolution itself is by bare name (constructor
    /// names are unique across a program), so this is only a scoping check.
    fn check_qualifier(&mut self, q: &ast::Ident) {
        if !self.qualifiers.contains_key(&*q.value()) {
            self.error(
                format!("module `{}` is not in scope here (add `use {}`)", q.value(), q.value()),
                "unknown module".to_string(),
                q.span,
            );
        }
    }

    /// Resolve a constructor application `Name arg…` (bare or the tail of a
    /// qualified `Mod.Name arg…`), including the `Name { field = e, … }` form.
    fn resolve_ctor_app(
        &mut self,
        span: Span,
        name: &ast::Ident,
        args: &[ast::LExpr],
    ) -> hir::LExpr {
        let cname = *name.value();
        if let [only] = args {
            if let ast::Expr::Record(fields, base) = only.value() {
                if base.is_none() {
                    if let Some(order) = self.ctor_field_order(cname) {
                        return self.resolve_named_ctor(span, cname, name.span, fields, &order);
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
        self.node(hir::Expr::Cons(label, ra), span)
    }

    fn resolve_expr(&mut self, expr: &ast::LExpr) -> hir::LExpr {
        self.toplevel = false;
        match expr.value() {
            ast::Expr::Lit(lit) => {
                let l = self.resolve_lit(lit);
                self.node(hir::Expr::Lit(l), expr.span)
            }
            ast::Expr::Unit => self.node(hir::Expr::Unit, expr.span),
            ast::Expr::Hole => {
                self.error(
                    "`_` can only appear inside an operator section, e.g. `(_ + 1)`"
                        .to_string(),
                    "stray hole".to_string(),
                    expr.span,
                );
                self.node(hir::Expr::Error, expr.span)
            }
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
            // `Mod.Ctor a b` — a qualified constructor application.
            ast::Expr::App(func, args) if matches!(func.value(), ast::Expr::Qual(_, n) if is_ctor_name(n.value())) =>
            {
                let ast::Expr::Qual(q, name) = func.value() else { unreachable!() };
                self.check_qualifier(q);
                self.resolve_ctor_app(expr.span, name, args)
            }
            ast::Expr::App(func, args) => {
                let rf = self.resolve_expr(func);
                let ra = args.iter().map(|a| self.resolve_expr(a)).collect_vec();
                self.node(hir::Expr::App(rf, ra), expr.span)
            }
            // `Mod.name` — a value from a `use`d / `mod` child module.
            ast::Expr::Qual(q, name) => {
                let qn = *q.value();
                let nn = *name.value();
                if is_ctor_name(&nn) {
                    self.check_qualifier(q);
                    return self.resolve_ctor_app(expr.span, name, &[]);
                }
                match self.qualifiers.get(&qn).and_then(|m| m.get(&nn).copied()) {
                    Some(id) => {
                        let v = self.node(id, name.span);
                        self.node(hir::Expr::Var(v), expr.span)
                    }
                    None => {
                        let msg = if self.qualifiers.contains_key(&qn) {
                            format!("`{nn}` is not exported by module `{qn}`")
                        } else {
                            format!("module `{qn}` is not in scope here (add `use {qn}`)")
                        };
                        self.error(msg, "unresolved".to_string(), expr.span);
                        self.node(hir::Expr::Error, expr.span)
                    }
                }
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
            // `and` / `or` short-circuit: desugar to `if` rather than a prim call.
            ast::Expr::BinOp(op, lhs, rhs)
                if matches!(op.value(), ast::BinOp::And | ast::BinOp::Or) =>
            {
                let rl = self.resolve_expr(lhs);
                let rr = self.resolve_expr(rhs);
                let ctor = |this: &mut Self, name: &str| {
                    let label = this.node(InternedString::from(name), op.span);
                    this.node(hir::Expr::Cons(label, vec![]), op.span)
                };
                let node = match op.value() {
                    ast::BinOp::And => {
                        let f = ctor(self, "False");
                        hir::Expr::If(rl, rr, f)
                    }
                    _ => {
                        let t = ctor(self, "True");
                        hir::Expr::If(rl, t, rr)
                    }
                };
                self.node(node, expr.span)
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
            ast::Expr::Array(exprs) => {
                let res = exprs.iter().map(|e| self.resolve_expr(e)).collect_vec();
                self.node(hir::Expr::Array(res), expr.span)
            }
            ast::Expr::List(exprs) => {
                let res = exprs.iter().map(|e| self.resolve_expr(e)).collect_vec();
                self.node(hir::Expr::List(res), expr.span)
            }
            ast::Expr::Cons(name, args) => self.resolve_ctor_app(expr.span, name, args),
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
            ast::Expr::Handle(scrut, arms, ret) => {
                let rs = self.resolve_expr(scrut);
                let rarms = arms
                    .iter()
                    .map(|arm| {
                        let opname = *arm.op.value();
                        let effect = self.effect_ops.get(&opname).copied().unwrap_or_else(|| {
                            self.error(
                                format!("unknown operation `{opname}`"),
                                "not an effect operation".to_string(),
                                arm.op.span,
                            );
                            InternedString::default()
                        });
                        let mark = self.mark();
                        let rparam = self.resolve_pat(&arm.param);
                        let rid = self.bind(*arm.resume.value());
                        let rresume = self.node(rid, arm.resume.span);
                        let rbody = self.resolve_expr(&arm.body);
                        self.reset(mark);
                        hir::HandlerArm {
                            effect,
                            op: opname,
                            param: rparam,
                            resume: rresume,
                            body: rbody,
                        }
                    })
                    .collect_vec();
                let rret = ret.as_ref().map(|(pat, body)| {
                    let mark = self.mark();
                    let rp = self.resolve_pat(pat);
                    let rb = self.resolve_expr(body);
                    self.reset(mark);
                    (rp, rb)
                });
                self.node(hir::Expr::Handle(rs, rarms, rret), expr.span)
            }
        }
    }

    // --- patterns ------------------------------------------------------------

    /// Resolve a constructor pattern `Name p…` (bare or the tail of `Mod.Name p…`).
    fn resolve_ctor_pat(
        &mut self,
        span: Span,
        name: &ast::Ident,
        args: &[ast::LPat],
    ) -> hir::LPat {
        let cname = *name.value();
        if let [only] = args {
            if let ast::Pat::Record(fields, _) = only.value() {
                if let Some(order) = self.ctor_field_order(cname) {
                    return self.resolve_named_ctor_pat(span, cname, name.span, fields, &order);
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
        self.node(hir::Pat::Cons(label, ra), span)
    }

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
            ast::Pat::Cons(name, args) => self.resolve_ctor_pat(pat.span, name, args),
            ast::Pat::QualCons(q, name, args) => {
                self.check_qualifier(q);
                self.resolve_ctor_pat(pat.span, name, args)
            }
            ast::Pat::Tuple(pats) => {
                let rp = pats.iter().map(|p| self.resolve_pat(p)).collect_vec();
                self.node(hir::Pat::Tuple(rp), pat.span)
            }
            ast::Pat::Array(pats) => {
                let rp = pats.iter().map(|p| self.resolve_pat(p)).collect_vec();
                self.node(hir::Pat::Array(rp), pat.span)
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
            ast::Lit::Float(b) => hir::Lit::Float(*b),
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

/// Every `VarId` an already-resolved (irrefutable) pattern binds.
fn collect_hir_pat_vars(pat: &hir::LPat, out: &mut Vec<VarId>) {
    match pat.value() {
        hir::Pat::Var(id) => out.push(*id.value()),
        hir::Pat::As(id, sub) => {
            out.push(*id.value());
            collect_hir_pat_vars(sub, out);
        }
        hir::Pat::Tuple(ps) | hir::Pat::List(ps) | hir::Pat::Cons(_, ps) => {
            ps.iter().for_each(|p| collect_hir_pat_vars(p, out))
        }
        hir::Pat::Record(fs, _) => fs.iter().for_each(|(_, p)| collect_hir_pat_vars(p, out)),
        _ => {}
    }
}
