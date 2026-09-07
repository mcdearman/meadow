//! Hindley–Milner type inference, **Algorithm J**.
//!
//! * Meta variables live in an [`Arena`] of union-find slots. `unify` links slots
//!   with path-compressing [`Arena::prune`]; there is no substitution map.
//! * Generalization is **ranked** (a la OCaml): every unbound meta var carries the
//!   `level` it was created at; entering the rhs of a `let`/`fun` bumps the level,
//!   and on the way out any var still deeper than the current level is quantified.
//!   This replaces the "occurs in the environment" scan.
//! * Records use **row polymorphism**: a record type wraps a *row*, which is a
//!   chain of `RowExtend(label, field, rest)` ending in `RowEmpty` (closed) or an
//!   unbound row meta var (open). `unify` rewrites rows to line up labels.
//!
//! Inference never bails: type errors are collected as [`Diagnostic`]s and a fresh
//! var is used to keep going, so one program yields all its errors and a maximally
//! annotated tree.
//!
//! Output is a [`TypeTable`] mapping every [`hir::NodeId`] to its (zonked) [`Type`],
//! plus a [`Scheme`] per top-level binding.

use crate::{
    diagnostics::Diagnostic,
    hir::{self, NodeId, VarId},
    intern::InternedString,
    rename::PRIMS,
    span::Span,
};
use std::collections::HashMap;
use std::fmt;

// ===========================================================================
// Types
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Meta variable: an index into [`Arena::slots`].
    Var(u32),
    /// Quantified variable: only ever appears inside a [`Scheme`]; `u32` indexes
    /// [`Scheme::quant`]. Keeping schemes in terms of `Bound` (not arena indices)
    /// makes them independent of the arena that produced them, so a dependency
    /// package's schemes can be dropped straight into a fresh inference run.
    Bound(u32),
    /// Type constructor applied to arguments: `Int`, `Bool`, `String`, `Unit`,
    /// `List a`, …
    Con(InternedString, Vec<Type>),
    /// Uncurried function type (the HIR has multi-arg lambdas / applications).
    Fun(Vec<Type>, Box<Type>),
    Tuple(Vec<Type>),
    /// A record over a row (the boxed type is `RowEmpty` / `RowExtend` / a row var).
    Record(Box<Type>),
    RowEmpty,
    RowExtend(InternedString, Box<Type>, Box<Type>),
}

impl Type {
    pub fn con(name: &str) -> Type {
        Type::Con(InternedString::from(name), vec![])
    }
    pub fn int() -> Type {
        Type::con("Int")
    }
    pub fn bool() -> Type {
        Type::con("Bool")
    }
    pub fn string() -> Type {
        Type::con("String")
    }
    pub fn unit() -> Type {
        Type::con("Unit")
    }
    pub fn list(elem: Type) -> Type {
        Type::Con(InternedString::from("List"), vec![elem])
    }
    /// A curried function type: `func([a, b], r)` is `a -> b -> r`.
    pub fn func(args: Vec<Type>, ret: Type) -> Type {
        args.into_iter()
            .rev()
            .fold(ret, |acc, a| Type::Fun(vec![a], Box::new(acc)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Type,
    Row,
}

/// A polytype: `quant` lists the kind of each quantified variable, and `ty` refers
/// to them as `Type::Bound(i)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheme {
    pub quant: Vec<VarKind>,
    pub ty: Type,
}

impl Scheme {
    pub fn mono(ty: Type) -> Scheme {
        Scheme { quant: vec![], ty }
    }
}

// ===========================================================================
// The meta-variable arena / union-find
// ===========================================================================

#[derive(Debug, Clone)]
enum Slot {
    Unbound { level: u32, kind: VarKind },
    Bound(Type),
}

#[derive(Debug, Clone)]
enum UnifyError {
    Mismatch(Type, Type),
    Occurs(Type, Type),
    Arity(usize, usize),
    /// A required label is absent from a closed row.
    MissingLabel(InternedString),
}

#[derive(Debug, Clone)]
pub struct Arena {
    slots: Vec<Slot>,
    level: u32,
}

impl Arena {
    fn new() -> Self {
        Arena {
            slots: Vec::new(),
            level: 0,
        }
    }

    fn fresh_in(&mut self, kind: VarKind) -> Type {
        let id = self.slots.len() as u32;
        self.slots.push(Slot::Unbound {
            level: self.level,
            kind,
        });
        Type::Var(id)
    }
    fn fresh(&mut self) -> Type {
        self.fresh_in(VarKind::Type)
    }
    fn fresh_row(&mut self) -> Type {
        self.fresh_in(VarKind::Row)
    }
    fn fresh_of(&mut self, kind: VarKind) -> Type {
        self.fresh_in(kind)
    }

    fn enter_level(&mut self) {
        self.level += 1;
    }
    fn exit_level(&mut self) {
        self.level -= 1;
    }

    fn slot_level(&self, id: u32) -> u32 {
        match &self.slots[id as usize] {
            Slot::Unbound { level, .. } => *level,
            Slot::Bound(_) => u32::MAX,
        }
    }
    fn slot_kind(&self, id: u32) -> VarKind {
        match &self.slots[id as usize] {
            Slot::Unbound { kind, .. } => *kind,
            Slot::Bound(_) => VarKind::Type,
        }
    }

    /// Union-find `find` with path compression.
    fn prune(&mut self, ty: Type) -> Type {
        match ty {
            Type::Var(id) => {
                let resolved = match self.slots[id as usize].clone() {
                    Slot::Bound(inner) => inner,
                    Slot::Unbound { .. } => return Type::Var(id),
                };
                let p = self.prune(resolved);
                self.slots[id as usize] = Slot::Bound(p.clone());
                p
            }
            other => other,
        }
    }

    /// Fully resolve a type against the current union-find state.
    pub fn zonk(&mut self, ty: &Type) -> Type {
        let ty = self.prune(ty.clone());
        match ty {
            Type::Var(_) | Type::Bound(_) | Type::RowEmpty => ty,
            Type::Con(name, args) => {
                Type::Con(name, args.iter().map(|a| self.zonk(a)).collect())
            }
            Type::Fun(args, ret) => Type::Fun(
                args.iter().map(|a| self.zonk(a)).collect(),
                Box::new(self.zonk(&ret)),
            ),
            Type::Tuple(items) => Type::Tuple(items.iter().map(|a| self.zonk(a)).collect()),
            Type::Record(row) => Type::Record(Box::new(self.zonk(&row))),
            Type::RowExtend(label, field, rest) => Type::RowExtend(
                label,
                Box::new(self.zonk(&field)),
                Box::new(self.zonk(&rest)),
            ),
        }
    }

    // --- unification -----------------------------------------------------------

    fn unify(&mut self, a: Type, b: Type) -> Result<(), UnifyError> {
        let a = self.prune(a);
        let b = self.prune(b);
        match (a, b) {
            (Type::Var(i), Type::Var(j)) if i == j => Ok(()),
            (Type::Var(i), t) | (t, Type::Var(i)) => self.bind_var(i, t),

            (Type::Con(n1, a1), Type::Con(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
                for (x, y) in a1.into_iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Type::Fun(a1, r1), Type::Fun(a2, r2)) => {
                if a1.len() != a2.len() {
                    return Err(UnifyError::Arity(a1.len(), a2.len()));
                }
                for (x, y) in a1.into_iter().zip(a2) {
                    self.unify(x, y)?;
                }
                self.unify(*r1, *r2)
            }
            (Type::Tuple(a1), Type::Tuple(a2)) if a1.len() == a2.len() => {
                for (x, y) in a1.into_iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Type::Record(r1), Type::Record(r2)) => self.unify(*r1, *r2),

            (Type::RowEmpty, Type::RowEmpty) => Ok(()),
            (Type::RowExtend(l1, t1, rest1), row2 @ (Type::RowExtend(..) | Type::RowEmpty)) => {
                let (t2, rest2) = self.rewrite_row(row2, l1)?;
                self.unify(*t1, t2)?;
                self.unify(*rest1, rest2)
            }

            (a, b) => Err(UnifyError::Mismatch(a, b)),
        }
    }

    fn bind_var(&mut self, id: u32, ty: Type) -> Result<(), UnifyError> {
        self.occurs_adjust(id, &ty)?;
        self.slots[id as usize] = Slot::Bound(ty);
        Ok(())
    }

    /// Occurs check + level adjustment in one walk. Any unbound var reachable from
    /// `ty` that is deeper than `id`'s level is lifted to `id`'s level, so it stays
    /// generalizable no further out than `id` is.
    fn occurs_adjust(&mut self, id: u32, ty: &Type) -> Result<(), UnifyError> {
        let ty = self.prune(ty.clone());
        match ty {
            Type::Var(j) => {
                if j == id {
                    return Err(UnifyError::Occurs(Type::Var(id), Type::Var(j)));
                }
                let min = self.slot_level(id).min(self.slot_level(j));
                if let Slot::Unbound { level, .. } = &mut self.slots[j as usize] {
                    *level = min;
                }
                Ok(())
            }
            Type::Bound(_) | Type::RowEmpty => Ok(()),
            Type::Con(_, args) | Type::Tuple(args) => {
                for a in &args {
                    self.occurs_adjust(id, a)?;
                }
                Ok(())
            }
            Type::Fun(args, ret) => {
                for a in &args {
                    self.occurs_adjust(id, a)?;
                }
                self.occurs_adjust(id, &ret)
            }
            Type::Record(row) => self.occurs_adjust(id, &row),
            Type::RowExtend(_, field, rest) => {
                self.occurs_adjust(id, &field)?;
                self.occurs_adjust(id, &rest)
            }
        }
    }

    /// Given a row, produce `(field_ty, rest)` such that `row ≡ {label: field_ty | rest}`.
    /// If the row ends in an unbound row var and lacks the label, the var is
    /// extended in place (standard rewrite-row; no lacks-predicates, so distinct
    /// labels are assumed).
    fn rewrite_row(&mut self, row: Type, label: InternedString) -> Result<(Type, Type), UnifyError> {
        let row = self.prune(row);
        match row {
            Type::RowExtend(l, field, rest) if l == label => Ok((*field, *rest)),
            Type::RowExtend(l, field, rest) => {
                let (found, rest2) = self.rewrite_row(*rest, label)?;
                Ok((found, Type::RowExtend(l, field, Box::new(rest2))))
            }
            Type::Var(id) => {
                let field = self.fresh();
                let new_rest = self.fresh_row();
                let ext = Type::RowExtend(
                    label,
                    Box::new(field.clone()),
                    Box::new(new_rest.clone()),
                );
                self.slots[id as usize] = Slot::Bound(ext);
                Ok((field, new_rest))
            }
            Type::RowEmpty => Err(UnifyError::MissingLabel(label)),
            other => Err(UnifyError::Mismatch(other, Type::RowEmpty)),
        }
    }

    // --- generalize / instantiate -------------------------------------------

    fn quantify(
        &self,
        ty: &Type,
        map: &mut HashMap<u32, u32>,
        kinds: &mut Vec<VarKind>,
    ) -> Type {
        match ty {
            Type::Var(id) => {
                if self.slot_level(*id) > self.level {
                    let idx = *map.entry(*id).or_insert_with(|| {
                        kinds.push(self.slot_kind(*id));
                        (kinds.len() - 1) as u32
                    });
                    Type::Bound(idx)
                } else {
                    Type::Var(*id)
                }
            }
            Type::Bound(i) => Type::Bound(*i),
            Type::RowEmpty => Type::RowEmpty,
            Type::Con(name, args) => Type::Con(
                *name,
                args.iter().map(|a| self.quantify(a, map, kinds)).collect(),
            ),
            Type::Fun(args, ret) => Type::Fun(
                args.iter().map(|a| self.quantify(a, map, kinds)).collect(),
                Box::new(self.quantify(ret, map, kinds)),
            ),
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|a| self.quantify(a, map, kinds)).collect())
            }
            Type::Record(row) => Type::Record(Box::new(self.quantify(row, map, kinds))),
            Type::RowExtend(label, field, rest) => Type::RowExtend(
                *label,
                Box::new(self.quantify(field, map, kinds)),
                Box::new(self.quantify(rest, map, kinds)),
            ),
        }
    }

    fn subst_bound(ty: &Type, fresh: &[Type]) -> Type {
        match ty {
            Type::Bound(i) => fresh[*i as usize].clone(),
            Type::Var(id) => Type::Var(*id),
            Type::RowEmpty => Type::RowEmpty,
            Type::Con(name, args) => Type::Con(
                *name,
                args.iter().map(|a| Self::subst_bound(a, fresh)).collect(),
            ),
            Type::Fun(args, ret) => Type::Fun(
                args.iter().map(|a| Self::subst_bound(a, fresh)).collect(),
                Box::new(Self::subst_bound(ret, fresh)),
            ),
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|a| Self::subst_bound(a, fresh)).collect())
            }
            Type::Record(row) => Type::Record(Box::new(Self::subst_bound(row, fresh))),
            Type::RowExtend(label, field, rest) => Type::RowExtend(
                *label,
                Box::new(Self::subst_bound(field, fresh)),
                Box::new(Self::subst_bound(rest, fresh)),
            ),
        }
    }
}

// ===========================================================================
// Side table: NodeId -> Type
// ===========================================================================

#[derive(Debug, Clone, Default)]
pub struct TypeTable {
    types: Vec<Option<Type>>,
}

impl TypeTable {
    pub fn new(node_count: usize) -> Self {
        TypeTable {
            types: vec![None; node_count],
        }
    }

    fn set(&mut self, id: NodeId, ty: Type) {
        let i = id.0 as usize;
        if i >= self.types.len() {
            self.types.resize(i + 1, None);
        }
        self.types[i] = Some(ty);
    }

    pub fn get(&self, id: NodeId) -> Option<&Type> {
        self.types.get(id.0 as usize).and_then(|t| t.as_ref())
    }

    fn zonk_all(&mut self, arena: &mut Arena) {
        for slot in &mut self.types {
            if let Some(ty) = slot {
                *ty = arena.zonk(ty);
            }
        }
    }

    /// `(NodeId, rendered type)` for every annotated node, using one shared naming
    /// of free variables so the same var reads the same across the whole tree.
    pub fn rendered(&self) -> Vec<(NodeId, String)> {
        let mut namer = Namer::default();
        let mut out = Vec::new();
        for (i, slot) in self.types.iter().enumerate() {
            if let Some(ty) = slot {
                let mut s = String::new();
                let _ = write_type(&mut s, ty, &mut namer, Prec::Top);
                out.push((NodeId(i as u32), s));
            }
        }
        out
    }
}

// ===========================================================================
// The inference pass
// ===========================================================================

/// What one inference run hands back.
pub struct InferResult {
    pub table: TypeTable,
    /// Scheme for each top-level binding produced by this run, keyed by its `VarId`.
    pub schemes: HashMap<VarId, Scheme>,
    pub errors: Vec<Diagnostic>,
}

pub struct Infer {
    filename: String,
    arena: Arena,
    /// `VarId` -> polytype. Populated from the prelude, dependency packages, and
    /// as we walk. `VarId`s are globally unique, so this never needs scoping.
    env: HashMap<VarId, Scheme>,
    table: TypeTable,
    /// `VarId`s bound at module top level by this run (its exports).
    exports: Vec<VarId>,
    /// Data / record constructor schemes, e.g. `Leaf : ∀a. Vector a -> Node a`.
    ctors: HashMap<InternedString, Scheme>,
    /// `tyname -> field -> accessor scheme` (`Person -> name -> ∀. Person -> String`).
    record_fields: HashMap<InternedString, HashMap<InternedString, Scheme>>,
    errors: Vec<Diagnostic>,
}

impl Infer {
    pub fn new(filename: impl Into<String>, node_count: usize) -> Self {
        Infer {
            filename: filename.into(),
            arena: Arena::new(),
            env: HashMap::new(),
            table: TypeTable::new(node_count),
            exports: Vec::new(),
            ctors: HashMap::new(),
            record_fields: HashMap::new(),
            errors: Vec::new(),
        }
    }

    /// Seed the environment with the primitive operators. `prims` must be the
    /// `(name, VarId)` pairs the resolver handed out for [`PRIMS`], in order.
    pub fn load_prelude(&mut self, prims: &[(InternedString, VarId)]) {
        debug_assert_eq!(prims.len(), PRIMS.len(), "prelude binding count mismatch");
        for (name, id) in prims {
            if let Some(scheme) = prim_scheme(name) {
                self.env.insert(*id, scheme);
            }
        }
    }

    /// Bring a dependency package's exported schemes into scope.
    pub fn load_deps(&mut self, deps: &[(VarId, Scheme)]) {
        for (id, scheme) in deps {
            self.env.insert(*id, scheme.clone());
        }
    }

    pub fn infer_module(&mut self, module: &hir::LModule) {
        for decl in &module.value().decls {
            match decl.value() {
                hir::Decl::Bind(bind) => self.infer_bind(bind, true),
                hir::Decl::Use(_)
                | hir::Decl::Error
                | hir::Decl::Data(_)
                | hir::Decl::Record(_) => {}
            }
            self.table.set(decl.id, Type::unit());
        }
    }

    pub fn finish(mut self) -> InferResult {
        self.table.zonk_all(&mut self.arena);
        let mut schemes = HashMap::new();
        for id in self.exports.clone() {
            if let Some(scheme) = self.env.get(&id).cloned() {
                schemes.insert(id, self.normalize_scheme(&scheme));
            }
        }
        InferResult {
            table: self.table,
            schemes,
            errors: self.errors,
        }
    }

    /// Re-zonk a scheme's body so exported schemes never leak arena indices from a
    /// var that got bound after generalization (can't happen for a closed
    /// top-level binding, but keeps exports self-contained regardless).
    fn normalize_scheme(&mut self, scheme: &Scheme) -> Scheme {
        Scheme {
            quant: scheme.quant.clone(),
            ty: self.arena.zonk(&scheme.ty),
        }
    }

    // --- bindings ----------------------------------------------------------

    fn infer_bind(&mut self, bind: &hir::Bind, toplevel: bool) {
        match bind {
            hir::Bind::Fun(name, params, body) => {
                let vid = *name.value();
                self.arena.enter_level();

                let mut param_tys = Vec::with_capacity(params.len());
                for p in params {
                    let t = self.arena.fresh();
                    self.env.insert(*p.value(), Scheme::mono(t.clone()));
                    self.table.set(p.id, t.clone());
                    param_tys.push(t);
                }
                let ret = self.arena.fresh();
                // curried: `fun f a b = e` has type `a -> b -> typeof(e)`
                let fn_ty = param_tys
                    .into_iter()
                    .rev()
                    .fold(ret.clone(), |acc, pty| Type::Fun(vec![pty], Box::new(acc)));
                // Bind the name monomorphically first so the body can recurse.
                self.env.insert(vid, Scheme::mono(fn_ty.clone()));
                self.table.set(name.id, fn_ty.clone());

                let body_ty = self.infer_expr(body);
                self.unify_at(body.span, ret, body_ty);
                self.arena.exit_level();

                let scheme = self.generalize(&fn_ty);
                self.table.set(name.id, self.arena.zonk(&fn_ty));
                self.env.insert(vid, scheme);
                if toplevel {
                    self.exports.push(vid);
                }
            }
            hir::Bind::Pat(pat, expr) => {
                // Infer rhs *and* the pattern at the raised level, so unifying them
                // doesn't drag the rhs's fresh vars down out of generalization.
                self.arena.enter_level();
                let rhs = self.infer_expr(expr);
                let mut bound = Vec::new();
                let pty = self.infer_pat(pat, &mut bound);
                self.unify_at(pat.span, pty, rhs);
                self.arena.exit_level();

                for (vid, vty) in bound {
                    let scheme = self.generalize(&vty);
                    self.env.insert(vid, scheme);
                    if toplevel {
                        self.exports.push(vid);
                    }
                }
            }
            hir::Bind::Error => {}
        }
    }

    // --- expressions -----------------------------------------------------------

    fn infer_expr(&mut self, expr: &hir::LExpr) -> Type {
        let ty = self.infer_expr_inner(expr);
        self.table.set(expr.id, ty.clone());
        ty
    }

    fn infer_expr_inner(&mut self, expr: &hir::LExpr) -> Type {
        match expr.value() {
            hir::Expr::Lit(hir::Lit::Int(_)) => Type::int(),
            hir::Expr::Lit(hir::Lit::String(_)) => Type::string(),
            hir::Expr::Unit => Type::unit(),

            hir::Expr::Var(ident) => {
                let ty = match self.env.get(&*ident.value()).cloned() {
                    Some(scheme) => self.instantiate(&scheme),
                    None => {
                        // Unresolved name already reported by the resolver; keep going.
                        self.arena.fresh()
                    }
                };
                self.table.set(ident.id, ty.clone());
                ty
            }

            hir::Expr::Lam(params, body) => {
                // Multi-parameter lambdas curry: `\a b -> e` is `\a -> \b -> e`,
                // matching both hand-written `\a -> \b -> e` and n-ary application.
                let mut bound = Vec::new();
                let ptys: Vec<Type> = params.iter().map(|p| self.infer_pat(p, &mut bound)).collect();
                let bty = self.infer_expr(body);
                ptys.into_iter()
                    .rev()
                    .fold(bty, |acc, pty| Type::Fun(vec![pty], Box::new(acc)))
            }

            hir::Expr::App(func, args) => {
                // n-ary application is a fold of single-argument applications.
                let mut fty = self.infer_expr(func);
                for arg in args {
                    let aty = self.infer_expr(arg);
                    let ret = self.arena.fresh();
                    self.unify_at(expr.span, fty, Type::Fun(vec![aty], Box::new(ret.clone())));
                    fty = ret;
                }
                fty
            }

            hir::Expr::Let(binds, body) => {
                for b in binds {
                    self.infer_bind(b, false);
                }
                self.infer_expr(body)
            }

            hir::Expr::If(cond, then_branch, else_branch) => {
                let ct = self.infer_expr(cond);
                self.unify_at(cond.span, ct, Type::bool());
                let tt = self.infer_expr(then_branch);
                let et = self.infer_expr(else_branch);
                self.unify_at(expr.span, tt.clone(), et);
                tt
            }

            hir::Expr::Match(scrut, arms) => {
                let st = self.infer_expr(scrut);
                let result = self.arena.fresh();
                for (pat, arm) in arms {
                    let mut bound = Vec::new();
                    let pt = self.infer_pat(pat, &mut bound);
                    self.unify_at(pat.span, pt, st.clone());
                    let at = self.infer_expr(arm);
                    self.unify_at(arm.span, at, result.clone());
                }
                result
            }

            hir::Expr::Tuple(items) => {
                Type::Tuple(items.iter().map(|e| self.infer_expr(e)).collect())
            }

            hir::Expr::List(items) => {
                let elem = self.arena.fresh();
                for e in items {
                    let t = self.infer_expr(e);
                    self.unify_at(e.span, t, elem.clone());
                }
                Type::list(elem)
            }

            hir::Expr::Cons(label, args) => {
                let name = *label.value();
                match self.ctor_type(name) {
                    Some(mut cty) => {
                        self.table.set(label.id, cty.clone());
                        // apply the constructor to its arguments, one at a time
                        for arg in args {
                            let aty = self.infer_expr(arg);
                            let ret = self.arena.fresh();
                            self.unify_at(
                                arg.span,
                                cty,
                                Type::Fun(vec![aty], Box::new(ret.clone())),
                            );
                            cty = ret;
                        }
                        cty
                    }
                    None => {
                        // Unknown constructor (no `data` decls yet) — infer args, give up.
                        for a in args {
                            self.infer_expr(a);
                        }
                        self.arena.fresh()
                    }
                }
            }

            hir::Expr::Record(fields, base) => {
                let mut row = match base {
                    Some(b) => {
                        let bt = self.infer_expr(b);
                        let r = self.arena.fresh_row();
                        self.unify_at(b.span, bt, Type::Record(Box::new(r.clone())));
                        r
                    }
                    None => Type::RowEmpty,
                };
                for (label, val) in fields.iter().rev() {
                    let vt = self.infer_expr(val);
                    self.table.set(label.id, vt.clone());
                    row = Type::RowExtend(*label.value(), Box::new(vt), Box::new(row));
                }
                Type::Record(Box::new(row))
            }

            hir::Expr::Field(obj, label) => {
                let ot = self.infer_expr(obj);
                let fname = *label.value();
                let pruned = self.arena.zonk(&ot);

                // Nominal record / data type with this field: use its accessor type.
                let nominal = if let Type::Con(tyname, _) = &pruned {
                    self.record_fields
                        .get(tyname)
                        .and_then(|m| m.get(&fname))
                        .cloned()
                } else {
                    None
                };

                let result = match nominal {
                    Some(scheme) => {
                        let accessor = self.instantiate(&scheme);
                        let res = self.arena.fresh();
                        self.unify_at(
                            expr.span,
                            accessor,
                            Type::Fun(vec![pruned], Box::new(res.clone())),
                        );
                        res
                    }
                    None => {
                        // Structural: `obj : { fname : field | rest }`.
                        let field = self.arena.fresh();
                        let rest = self.arena.fresh_row();
                        let want = Type::Record(Box::new(Type::RowExtend(
                            fname,
                            Box::new(field.clone()),
                            Box::new(rest),
                        )));
                        self.unify_at(expr.span, ot, want);
                        field
                    }
                };
                self.table.set(label.id, result.clone());
                result
            }

            hir::Expr::Error => self.arena.fresh(),
        }
    }

    // --- patterns ------------------------------------------------------------

    fn infer_pat(&mut self, pat: &hir::LPat, bound: &mut Vec<(VarId, Type)>) -> Type {
        let ty = self.infer_pat_inner(pat, bound);
        self.table.set(pat.id, ty.clone());
        ty
    }

    fn infer_pat_inner(&mut self, pat: &hir::LPat, bound: &mut Vec<(VarId, Type)>) -> Type {
        match pat.value() {
            hir::Pat::Wildcard => self.arena.fresh(),
            hir::Pat::Unit => Type::unit(),
            hir::Pat::Lit(hir::Lit::Int(_)) => Type::int(),
            hir::Pat::Lit(hir::Lit::String(_)) => Type::string(),

            hir::Pat::Var(ident) => {
                let vid = *ident.value();
                let t = self.arena.fresh();
                self.env.insert(vid, Scheme::mono(t.clone()));
                self.table.set(ident.id, t.clone());
                bound.push((vid, t.clone()));
                t
            }

            hir::Pat::As(ident, sub) => {
                let st = self.infer_pat(sub, bound);
                let vid = *ident.value();
                self.env.insert(vid, Scheme::mono(st.clone()));
                self.table.set(ident.id, st.clone());
                bound.push((vid, st.clone()));
                st
            }

            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                match self.ctor_type(name) {
                    Some(mut cty) => {
                        for sub in args {
                            let sty = self.infer_pat(sub, bound);
                            let ret = self.arena.fresh();
                            self.unify_at(
                                sub.span,
                                cty,
                                Type::Fun(vec![sty], Box::new(ret.clone())),
                            );
                            cty = ret;
                        }
                        self.table.set(label.id, cty.clone());
                        cty
                    }
                    None => {
                        for a in args {
                            self.infer_pat(a, bound);
                        }
                        self.arena.fresh()
                    }
                }
            }

            hir::Pat::Tuple(items) => {
                Type::Tuple(items.iter().map(|p| self.infer_pat(p, bound)).collect())
            }

            hir::Pat::List(items) => {
                let elem = self.arena.fresh();
                for p in items {
                    let t = self.infer_pat(p, bound);
                    self.unify_at(p.span, t, elem.clone());
                }
                Type::list(elem)
            }

            hir::Pat::Record(fields, open) => {
                let mut row = if *open {
                    self.arena.fresh_row()
                } else {
                    Type::RowEmpty
                };
                for (label, sub) in fields.iter().rev() {
                    let st = self.infer_pat(sub, bound);
                    self.table.set(label.id, st.clone());
                    row = Type::RowExtend(*label.value(), Box::new(st), Box::new(row));
                }
                Type::Record(Box::new(row))
            }

            hir::Pat::Error => self.arena.fresh(),
        }
    }

    // --- helpers -------------------------------------------------------------

    /// The (fully instantiated) type of a data constructor: user-declared
    /// constructors first, then the hard-wired built-ins.
    ///
    /// - `Nil  : List a`
    /// - `Cons : a -> List a -> List a`
    /// - `True`, `False : Bool`
    fn ctor_type(&mut self, name: InternedString) -> Option<Type> {
        if let Some(scheme) = self.ctors.get(&name).cloned() {
            return Some(self.instantiate(&scheme));
        }
        Some(match &*name {
            "Nil" => Type::list(self.arena.fresh()),
            "Cons" => {
                let a = self.arena.fresh();
                Type::func(vec![a.clone(), Type::list(a.clone())], Type::list(a))
            }
            "True" | "False" => Type::bool(),
            _ => return None,
        })
    }

    /// Populate `ctors` / `record_fields` from `data` / `record` declarations.
    /// Call before `infer_module`.
    pub fn register_types(&mut self, decls: &[hir::LDecl]) {
        for d in decls {
            match d.value() {
                hir::Decl::Data(dd) => {
                    let params = param_map(&dd.params);
                    let quant = vec![VarKind::Type; dd.params.len()];
                    let head = Type::Con(
                        dd.name,
                        (0..dd.params.len() as u32).map(Type::Bound).collect(),
                    );
                    for v in &dd.variants {
                        let fields: Vec<(Option<InternedString>, Type)> = match &v.fields {
                            hir::VariantFields::Positional(ts) => {
                                ts.iter().map(|t| (None, ty_of(t, &params))).collect()
                            }
                            hir::VariantFields::Named(fs) => fs
                                .iter()
                                .map(|(n, t)| (Some(*n), ty_of(t, &params)))
                                .collect(),
                        };
                        self.record_ctor(dd.name, v.name, &quant, &head, &fields);
                    }
                }
                hir::Decl::Record(rd) => {
                    let params = param_map(&rd.params);
                    let quant = vec![VarKind::Type; rd.params.len()];
                    let head = Type::Con(
                        rd.name,
                        (0..rd.params.len() as u32).map(Type::Bound).collect(),
                    );
                    let fields: Vec<(Option<InternedString>, Type)> = rd
                        .fields
                        .iter()
                        .map(|(n, t)| (Some(*n), ty_of(t, &params)))
                        .collect();
                    self.record_ctor(rd.name, rd.name, &quant, &head, &fields);
                }
                _ => {}
            }
        }
    }

    fn record_ctor(
        &mut self,
        tyname: InternedString,
        ctor: InternedString,
        quant: &[VarKind],
        head: &Type,
        fields: &[(Option<InternedString>, Type)],
    ) {
        let field_tys: Vec<Type> = fields.iter().map(|(_, t)| t.clone()).collect();
        let cty = if field_tys.is_empty() {
            head.clone()
        } else {
            Type::func(field_tys, head.clone())
        };
        self.ctors.insert(
            ctor,
            Scheme {
                quant: quant.to_vec(),
                ty: cty,
            },
        );
        let accessors = self.record_fields.entry(tyname).or_default();
        for (name, t) in fields {
            if let Some(name) = name {
                accessors.insert(
                    *name,
                    Scheme {
                        quant: quant.to_vec(),
                        ty: Type::Fun(vec![head.clone()], Box::new(t.clone())),
                    },
                );
            }
        }
    }

    fn generalize(&mut self, ty: &Type) -> Scheme {
        let z = self.arena.zonk(ty);
        let mut map = HashMap::new();
        let mut kinds = Vec::new();
        let body = self.arena.quantify(&z, &mut map, &mut kinds);
        Scheme {
            quant: kinds,
            ty: body,
        }
    }

    fn instantiate(&mut self, scheme: &Scheme) -> Type {
        let fresh: Vec<Type> = scheme
            .quant
            .iter()
            .map(|k| self.arena.fresh_of(*k))
            .collect();
        Arena::subst_bound(&scheme.ty, &fresh)
    }

    fn unify_at(&mut self, span: Span, a: Type, b: Type) {
        if let Err(err) = self.arena.unify(a, b) {
            let diag = self.unify_diagnostic(span, err);
            self.errors.push(diag);
        }
    }

    fn unify_diagnostic(&mut self, span: Span, err: UnifyError) -> Diagnostic {
        let (msg, label) = match err {
            UnifyError::Mismatch(a, b) => {
                let a = self.arena.zonk(&a);
                let b = self.arena.zonk(&b);
                (
                    format!("type mismatch: `{}` vs `{}`", show(&a), show(&b)),
                    "types do not unify here".to_string(),
                )
            }
            UnifyError::Occurs(a, b) => {
                let a = self.arena.zonk(&a);
                let b = self.arena.zonk(&b);
                (
                    format!("infinite type: `{}` occurs in `{}`", show(&a), show(&b)),
                    "recursive type".to_string(),
                )
            }
            UnifyError::Arity(x, y) => (
                format!("function applied to the wrong number of arguments: expected {x}, found {y}"),
                "arity mismatch".to_string(),
            ),
            UnifyError::MissingLabel(l) => (
                format!("record has no field `{l}`"),
                format!("missing field `{l}`"),
            ),
        };
        Diagnostic {
            msg,
            filename: self.filename.clone(),
            label: (label, span),
            extra_labels: vec![],
        }
    }
}

// ===========================================================================
// data / record declaration -> types
// ===========================================================================

/// Map each type parameter `VarId` to its quantifier index.
fn param_map(params: &[hir::Ident]) -> HashMap<VarId, u32> {
    params
        .iter()
        .enumerate()
        .map(|(i, p)| (*p.value(), i as u32))
        .collect()
}

/// Convert a resolved HIR type expression into an inference [`Type`], with the
/// declaration's parameters as `Bound` variables.
fn ty_of(t: &hir::LTypeExpr, params: &HashMap<VarId, u32>) -> Type {
    match t.value() {
        hir::TypeExpr::Var(v) => match params.get(v.value()) {
            Some(&i) => Type::Bound(i),
            None => Type::unit(), // rename already reported the unbound tyvar
        },
        hir::TypeExpr::Con(name, args) => {
            let args: Vec<Type> = args.iter().map(|a| ty_of(a, params)).collect();
            match &**name {
                "Int" => Type::int(),
                "String" => Type::string(),
                "Bool" => Type::bool(),
                "Unit" => Type::unit(),
                "List" => Type::list(args.into_iter().next().unwrap_or_else(Type::unit)),
                _ => Type::Con(*name, args),
            }
        }
        hir::TypeExpr::Fun(ps, r) => Type::func(
            ps.iter().map(|p| ty_of(p, params)).collect(),
            ty_of(r, params),
        ),
        hir::TypeExpr::Tuple(ts) => Type::Tuple(ts.iter().map(|x| ty_of(x, params)).collect()),
        hir::TypeExpr::List(x) => Type::list(ty_of(x, params)),
    }
}

// ===========================================================================
// Primitive signatures (indexed by `rename::PRIMS`)
// ===========================================================================

fn prim_scheme(name: &str) -> Option<Scheme> {
    use Type::*;
    let s = match name {
        "+" | "-" | "*" | "/" | "%" | "^" => {
            Scheme::mono(Type::func(vec![Type::int(), Type::int()], Type::int()))
        }
        "<" | ">" | "<=" | ">=" => {
            Scheme::mono(Type::func(vec![Type::int(), Type::int()], Type::bool()))
        }
        "&&" | "||" => Scheme::mono(Type::func(vec![Type::bool(), Type::bool()], Type::bool())),
        "neg" => Scheme::mono(Type::func(vec![Type::int()], Type::int())),
        "!" => Scheme::mono(Type::func(vec![Type::bool()], Type::bool())),
        "==" | "!=" => Scheme {
            quant: vec![VarKind::Type],
            ty: Type::func(vec![Bound(0), Bound(0)], Type::bool()),
        },
        "print" | "println" => Scheme {
            quant: vec![VarKind::Type],
            ty: Type::func(vec![Bound(0)], Type::unit()),
        },
        _ => return None,
    };
    Some(s)
}

// ===========================================================================
// Pretty printing
// ===========================================================================

fn show(ty: &Type) -> String {
    let mut namer = Namer::default();
    let mut s = String::new();
    let _ = write_type(&mut s, ty, &mut namer, Prec::Top);
    s
}

#[derive(Default)]
struct Namer {
    names: HashMap<u32, String>,
    next: u32,
}

impl Namer {
    fn name(&mut self, id: u32) -> String {
        if let Some(n) = self.names.get(&id) {
            return n.clone();
        }
        let n = var_name(self.next);
        self.next += 1;
        self.names.insert(id, n.clone());
        n
    }
}

fn var_name(mut n: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, char::from(b'a' + (n % 26) as u8));
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Top,
    Arrow,
    App,
}

fn write_type(out: &mut impl fmt::Write, ty: &Type, namer: &mut Namer, prec: Prec) -> fmt::Result {
    match ty {
        Type::Var(id) => write!(out, "{}", namer.name(*id)),
        Type::Bound(i) => write!(out, "{}", var_name(*i)),
        Type::Con(name, args) if args.is_empty() => write!(out, "{name}"),
        Type::Con(name, args) if &**name == "List" => {
            out.write_char('[')?;
            write_type(out, &args[0], namer, Prec::Top)?;
            out.write_char(']')
        }
        Type::Con(name, args) => {
            let wrap = prec >= Prec::App;
            if wrap {
                out.write_char('(')?;
            }
            write!(out, "{name}")?;
            for a in args {
                out.write_char(' ')?;
                write_type(out, a, namer, Prec::App)?;
            }
            if wrap {
                out.write_char(')')?;
            }
            Ok(())
        }
        Type::Fun(args, ret) => {
            let wrap = prec >= Prec::Arrow;
            if wrap {
                out.write_char('(')?;
            }
            if args.len() == 1 {
                write_type(out, &args[0], namer, Prec::Arrow)?;
            } else {
                out.write_char('(')?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write_type(out, a, namer, Prec::Top)?;
                }
                out.write_char(')')?;
            }
            out.write_str(" -> ")?;
            write_type(out, ret, namer, Prec::Top)?;
            if wrap {
                out.write_char(')')?;
            }
            Ok(())
        }
        Type::Tuple(items) => {
            out.write_char('(')?;
            for (i, a) in items.iter().enumerate() {
                if i > 0 {
                    out.write_str(", ")?;
                }
                write_type(out, a, namer, Prec::Top)?;
            }
            out.write_char(')')
        }
        Type::Record(row) => {
            out.write_str("{ ")?;
            write_row(out, row, namer)?;
            out.write_str(" }")
        }
        Type::RowEmpty => out.write_str("()"),
        Type::RowExtend(..) => {
            out.write_str("(| ")?;
            write_row(out, ty, namer)?;
            out.write_str(" |)")
        }
    }
}

fn write_row(out: &mut impl fmt::Write, row: &Type, namer: &mut Namer) -> fmt::Result {
    let mut first = true;
    let mut cur = row;
    loop {
        match cur {
            Type::RowExtend(label, field, rest) => {
                if !first {
                    out.write_str(", ")?;
                }
                first = false;
                write!(out, "{label} : ")?;
                write_type(out, field, namer, Prec::Top)?;
                cur = rest;
            }
            Type::RowEmpty => break,
            Type::Var(id) => {
                if !first {
                    out.write_char(' ')?;
                }
                write!(out, "| {}", namer.name(*id))?;
                break;
            }
            Type::Bound(i) => {
                if !first {
                    out.write_char(' ')?;
                }
                write!(out, "| {}", var_name(*i))?;
                break;
            }
            other => {
                if !first {
                    out.write_str(", ")?;
                }
                write_type(out, other, namer, Prec::Top)?;
                break;
            }
        }
    }
    Ok(())
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut namer = Namer::default();
        write_type(f, self, &mut namer, Prec::Top)
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.quant.is_empty() {
            f.write_str("forall")?;
            for i in 0..self.quant.len() as u32 {
                write!(f, " {}", var_name(i))?;
            }
            f.write_str(". ")?;
        }
        let mut namer = Namer::default();
        write_type(f, &self.ty, &mut namer, Prec::Top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_display() {
        assert_eq!(Type::int().to_string(), "Int");
        assert_eq!(Type::list(Type::int()).to_string(), "[Int]");
        assert_eq!(
            Type::func(vec![Type::int(), Type::int()], Type::bool()).to_string(),
            "Int -> Int -> Bool"
        );
        assert_eq!(
            Type::Tuple(vec![Type::int(), Type::string()]).to_string(),
            "(Int, String)"
        );
        // free (unbound) variables get printed as `a`, `b`, …
        assert_eq!(Type::func(vec![Type::Var(3)], Type::Var(3)).to_string(), "a -> a");
        assert_eq!(Type::func(vec![Type::Var(1)], Type::Var(9)).to_string(), "a -> b");
    }

    #[test]
    fn record_type_display() {
        let row = Type::RowExtend(
            InternedString::from("x"),
            Box::new(Type::int()),
            Box::new(Type::RowExtend(
                InternedString::from("y"),
                Box::new(Type::bool()),
                Box::new(Type::Var(0)),
            )),
        );
        assert_eq!(Type::Record(Box::new(row)).to_string(), "{ x : Int, y : Bool | a }");
    }

    #[test]
    fn scheme_display_names_quantifiers() {
        let s = Scheme {
            quant: vec![VarKind::Type, VarKind::Type],
            ty: Type::func(vec![Type::Bound(0)], Type::Bound(1)),
        };
        assert_eq!(s.to_string(), "forall a b. a -> b");
        assert_eq!(Scheme::mono(Type::int()).to_string(), "Int");
    }

    #[test]
    fn arena_unifies_var_with_concrete() {
        let mut a = Arena::new();
        let v = a.fresh();
        a.unify(v.clone(), Type::int()).unwrap();
        assert_eq!(a.zonk(&v), Type::int());
    }

    #[test]
    fn arena_unifies_two_vars_transitively() {
        let mut a = Arena::new();
        let (x, y) = (a.fresh(), a.fresh());
        a.unify(x.clone(), y.clone()).unwrap();
        a.unify(y, Type::bool()).unwrap();
        assert_eq!(a.zonk(&x), Type::bool());
    }

    #[test]
    fn arena_reports_mismatch() {
        let mut a = Arena::new();
        assert!(a.unify(Type::int(), Type::bool()).is_err());
    }

    #[test]
    fn arena_occurs_check() {
        let mut a = Arena::new();
        let v = a.fresh();
        // v = v -> v  must be rejected
        let recursive = Type::func(vec![v.clone()], v.clone());
        assert!(matches!(a.unify(v, recursive), Err(UnifyError::Occurs(..))));
    }

    #[test]
    fn arena_rewrites_rows_to_align_labels() {
        let mut a = Arena::new();
        // { x : Int | r }  ~  { y : Bool, x : Int }
        let lhs = Type::Record(Box::new(Type::RowExtend(
            InternedString::from("x"),
            Box::new(Type::int()),
            Box::new(a.fresh_row()),
        )));
        let rhs = Type::Record(Box::new(Type::RowExtend(
            InternedString::from("y"),
            Box::new(Type::bool()),
            Box::new(Type::RowExtend(
                InternedString::from("x"),
                Box::new(Type::int()),
                Box::new(Type::RowEmpty),
            )),
        )));
        a.unify(lhs.clone(), rhs).unwrap();
        assert_eq!(a.zonk(&lhs).to_string(), "{ x : Int, y : Bool }");
    }

    #[test]
    fn generalization_respects_levels() {
        // A var created at a deeper level is generalized; one at the current
        // level is not.
        let mut infer = Infer::new("t", 0);
        infer.arena.enter_level();
        let deep = infer.arena.fresh();
        infer.arena.exit_level();
        let shallow = infer.arena.fresh();

        assert_eq!(infer.generalize(&deep).quant.len(), 1);
        assert_eq!(infer.generalize(&shallow).quant.len(), 0);
    }
}
