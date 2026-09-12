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

use meadow_diagnostics::Diagnostic;
use meadow_hir::{self as hir, NodeId, PRIMS, VarId};
use meadow_intern::InternedString;
use meadow_span::Span;
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
    /// Function type `arg -> ret ! effect`. Currying keeps the arg list length 1;
    /// the third component is the arrow's **latent effect** — a *row* (`RowEmpty`
    /// for a pure arrow, `RowExtend` / a row var otherwise). See the module docs.
    Fun(Vec<Type>, Box<Type>, Box<Type>),
    Tuple(Vec<Type>),
    /// A record over a row (the boxed type is `RowEmpty` / `RowExtend` / a row var).
    Record(Box<Type>),
    /// Empty row — a closed record `{}` *and* the pure effect.
    RowEmpty,
    /// `label(field) | rest`. For a record row the `field` is the field's type; for
    /// an effect row it is `Tuple([..effect type args..])`.
    RowExtend(InternedString, Box<Type>, Box<Type>),
    /// The type of something already reported as wrong: an undefined name, an
    /// unknown constructor or type. It unifies with anything, and a mismatch
    /// that involves it is not reported, which is what stops one mistake from
    /// turning into a cascade. The same idea as rustc's `{type error}`.
    Error,
}

impl Type {
    /// Whether the error type appears anywhere in this (zonked) type.
    pub fn references_error(&self) -> bool {
        match self {
            Type::Error => true,
            Type::Var(_) | Type::Bound(_) | Type::RowEmpty => false,
            Type::Con(_, args) | Type::Tuple(args) => args.iter().any(Type::references_error),
            Type::Fun(ps, r, e) => {
                ps.iter().any(Type::references_error)
                    || r.references_error()
                    || e.references_error()
            }
            Type::Record(r) => r.references_error(),
            Type::RowExtend(_, f, rest) => f.references_error() || rest.references_error(),
        }
    }

    pub fn con(name: &str) -> Type {
        Type::Con(InternedString::from(name), vec![])
    }
    pub fn int() -> Type {
        Type::con("Int")
    }
    pub fn bigint() -> Type {
        Type::con("BigInt")
    }
    pub fn float() -> Type {
        Type::con("Float")
    }
    pub fn bool() -> Type {
        Type::con("Bool")
    }
    pub fn string() -> Type {
        Type::con("String")
    }
    pub fn char() -> Type {
        Type::con("Char")
    }
    pub fn unit() -> Type {
        Type::con("Unit")
    }
    pub fn list(elem: Type) -> Type {
        Type::Con(InternedString::from("List"), vec![elem])
    }
    /// The builtin `Array a` — a fixed-size, `Rc`-shared contiguous buffer.
    pub fn array(elem: Type) -> Type {
        Type::Con(InternedString::from("Array"), vec![elem])
    }
    /// `Vector a` — the `Std.Collections.Vector` RRB vector that `[…]` builds.
    pub fn vector(elem: Type) -> Type {
        Type::Con(InternedString::from("Vector"), vec![elem])
    }
    /// `Ref a` — the one mutable cell. Reading and writing one carries the `Mut`
    /// effect, so a function that mutates says so in its type.
    pub fn reference(inner: Type) -> Type {
        Type::Con(InternedString::from("Ref"), vec![inner])
    }
    /// A curried **pure** function type: `func([a, b], r)` is `a -> b -> r`.
    pub fn func(args: Vec<Type>, ret: Type) -> Type {
        Type::func_eff(args, ret, Type::RowEmpty)
    }

    /// A curried function type whose *innermost* arrow carries `eff`; the outer
    /// arrows (from extra curried params) are pure.
    pub fn func_eff(args: Vec<Type>, ret: Type, eff: Type) -> Type {
        let mut it = args.into_iter().rev();
        let inner = match it.next() {
            Some(last) => Type::Fun(vec![last], Box::new(ret), Box::new(eff)),
            None => return ret, // nullary "function" — just the result
        };
        it.fold(inner, |acc, a| {
            Type::Fun(vec![a], Box::new(acc), Box::new(Type::RowEmpty))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Type,
    /// A record row variable.
    Row,
    /// An effect row variable — structurally a row, tracked separately so error
    /// messages and pretty-printing can tell effects from records.
    Effect,
    /// A numeric-literal variable. Unifies only with `Int`, `BigInt`, a plain
    /// type var (keeping the `Num`), or another `Num`; anything else is a type
    /// error. Never generalized, never printed — any that survive inference
    /// default to `Int` (see `Arena::default_num_vars`).
    Num,
}

/// A polytype: `quant` lists the kind of each quantified variable, and `ty` refers
/// to them as `Type::Bound(i)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheme {
    pub quant: Vec<VarKind>,
    pub ty: Type,
}

/// An operation of an `effect` declaration, with its argument and result types
/// over the effect's parameters (`Bound(0..params)`).
#[derive(Debug, Clone)]
struct EffOp {
    name: InternedString,
    arg: Type,
    ret: Type,
}

#[derive(Debug, Clone)]
struct EffectInfo {
    params: usize,
    ops: Vec<EffOp>,
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
    /// The previous contents of every slot written since the oldest open
    /// [`Snapshot`], so a trial unification can be taken back. Empty, and not
    /// written to, while no snapshot is open.
    trail: Vec<(u32, Slot)>,
    snapshots: usize,
}

/// A point to roll the arena back to -- see [`Arena::snapshot`].
struct Snapshot {
    slots: usize,
    trail: usize,
}

impl Arena {
    fn new() -> Self {
        Arena {
            slots: Vec::new(),
            level: 0,
            trail: Vec::new(),
            snapshots: 0,
        }
    }

    /// Every slot write goes through here, so a snapshot can undo it.
    fn set_slot(&mut self, id: u32, slot: Slot) {
        if self.snapshots > 0 {
            let old = std::mem::replace(&mut self.slots[id as usize], slot);
            self.trail.push((id, old));
        } else {
            self.slots[id as usize] = slot;
        }
    }

    /// The unbound variables in `ty`.
    fn free_vars(&mut self, ty: &Type, out: &mut Vec<u32>) {
        let ty = self.prune(ty.clone());
        match ty {
            Type::Var(id) => {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            Type::Bound(_) | Type::RowEmpty | Type::Error => {}
            Type::Con(_, args) | Type::Tuple(args) => {
                args.iter().for_each(|a| self.free_vars(a, out))
            }
            Type::Fun(args, ret, eff) => {
                args.iter().for_each(|a| self.free_vars(a, out));
                self.free_vars(&ret, out);
                self.free_vars(&eff, out);
            }
            Type::Record(row) => self.free_vars(&row, out),
            Type::RowExtend(_, field, rest) => {
                self.free_vars(&field, out);
                self.free_vars(&rest, out);
            }
        }
    }

    /// Keep every variable still free in `ty` from generalizing past `level`.
    fn hold_back(&mut self, ty: &Type, level: u32) {
        let ty = self.prune(ty.clone());
        match ty {
            Type::Var(id) => {
                if let Slot::Unbound { level: l, kind } = self.slots[id as usize].clone()
                    && l > level
                {
                    self.set_slot(id, Slot::Unbound { level, kind });
                }
            }
            Type::Bound(_) | Type::RowEmpty | Type::Error => {}
            Type::Con(_, args) | Type::Tuple(args) => {
                args.iter().for_each(|a| self.hold_back(a, level))
            }
            Type::Fun(args, ret, eff) => {
                args.iter().for_each(|a| self.hold_back(a, level));
                self.hold_back(&ret, level);
                self.hold_back(&eff, level);
            }
            Type::Record(row) => self.hold_back(&row, level),
            Type::RowExtend(_, field, rest) => {
                self.hold_back(&field, level);
                self.hold_back(&rest, level);
            }
        }
    }

    /// Start recording, for [`Arena::rollback`]. Unifying against each of an
    /// overloaded name's candidates in turn is the one thing that needs it.
    fn snapshot(&mut self) -> Snapshot {
        self.snapshots += 1;
        Snapshot {
            slots: self.slots.len(),
            trail: self.trail.len(),
        }
    }

    /// Put every slot back as it was at `snap`, and drop the variables made since.
    fn rollback(&mut self, snap: Snapshot) {
        while self.trail.len() > snap.trail {
            let (id, old) = self.trail.pop().expect("trail is longer than the snapshot");
            if (id as usize) < self.slots.len() {
                self.slots[id as usize] = old;
            }
        }
        self.slots.truncate(snap.slots);
        self.snapshots -= 1;
        if self.snapshots == 0 {
            self.trail.clear();
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
    fn fresh_effect(&mut self) -> Type {
        self.fresh_in(VarKind::Effect)
    }
    fn fresh_num(&mut self) -> Type {
        self.fresh_in(VarKind::Num)
    }
    fn fresh_of(&mut self, kind: VarKind) -> Type {
        self.fresh_in(kind)
    }

    /// A type variable at the outermost level, so nothing can generalize over it.
    /// The placeholder a forward reference gets — see `Expr::Var`.
    fn fresh_global(&mut self) -> Type {
        let id = self.slots.len() as u32;
        self.slots.push(Slot::Unbound {
            level: 0,
            kind: VarKind::Type,
        });
        Type::Var(id)
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
                self.set_slot(id, Slot::Bound(p.clone()));
                p
            }
            other => other,
        }
    }

    /// Fully resolve a type against the current union-find state.
    pub fn zonk(&mut self, ty: &Type) -> Type {
        let ty = self.prune(ty.clone());
        match ty {
            Type::Var(_) | Type::Bound(_) | Type::RowEmpty | Type::Error => ty,
            Type::Con(name, args) => Type::Con(name, args.iter().map(|a| self.zonk(a)).collect()),
            Type::Fun(args, ret, eff) => Type::Fun(
                args.iter().map(|a| self.zonk(a)).collect(),
                Box::new(self.zonk(&ret)),
                Box::new(self.zonk(&eff)),
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
            (Type::Var(i), Type::Var(j)) => {
                // When a `Num` var meets a plain type var, keep the `Num` (bind
                // the type var to it) so a literal threaded through a polymorphic
                // function stays numeric and later defaults, instead of the plain
                // var winning and freezing the result as `∀a. a`.
                match (self.slot_kind(i), self.slot_kind(j)) {
                    (VarKind::Num, VarKind::Type) => self.bind_var(j, Type::Var(i)),
                    _ => self.bind_var(i, Type::Var(j)),
                }
            }
            // A type variable meeting the error type is poisoned along with
            // it, so later uses of the same variable stay quiet too. Only a
            // plain one: a row or a numeric literal's variable has a shape the
            // error type cannot stand for, and is simply left alone.
            (Type::Var(i), Type::Error) | (Type::Error, Type::Var(i)) => {
                if self.slot_kind(i) == VarKind::Type {
                    self.bind_var(i, Type::Error)
                } else {
                    Ok(())
                }
            }
            (Type::Var(i), t) | (t, Type::Var(i)) => self.bind_var(i, t),
            (Type::Error, _) | (_, Type::Error) => Ok(()),

            (Type::Con(n1, a1), Type::Con(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
                for (x, y) in a1.into_iter().zip(a2) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Type::Fun(a1, r1, e1), Type::Fun(a2, r2, e2)) => {
                if a1.len() != a2.len() {
                    return Err(UnifyError::Arity(a1.len(), a2.len()));
                }
                for (x, y) in a1.into_iter().zip(a2) {
                    self.unify(x, y)?;
                }
                self.unify(*r1, *r2)?;
                // latent effects are rows — falls into the row arm below
                self.unify(*e1, *e2)
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
        if self.slot_kind(id) == VarKind::Num && !Self::num_compatible(&ty) {
            return Err(UnifyError::Mismatch(Type::con("Int"), ty));
        }
        self.set_slot(id, Slot::Bound(ty));
        Ok(())
    }

    /// Can a `Num` (numeric-literal) var legally unify with this type? Only with
    /// `Int` / `BigInt`, or another var (kept unresolved for now).
    fn num_compatible(ty: &Type) -> bool {
        match ty {
            Type::Var(_) => true,
            Type::Con(n, args) if args.is_empty() => matches!(&**n, "Int" | "BigInt"),
            _ => false,
        }
    }

    /// Bind every still-unbound `Num` var to `Int` — a numeric literal that no
    /// context ever pinned to `BigInt`. Run once, at the end of inference.
    fn default_num_vars(&mut self) {
        for slot in &mut self.slots {
            if let Slot::Unbound {
                kind: VarKind::Num, ..
            } = slot
            {
                *slot = Slot::Bound(Type::con("Int"));
            }
        }
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
                if let Slot::Unbound { level, kind } = self.slots[j as usize].clone()
                    && level != min
                {
                    self.set_slot(j, Slot::Unbound { level: min, kind });
                }
                Ok(())
            }
            Type::Bound(_) | Type::RowEmpty | Type::Error => Ok(()),
            Type::Con(_, args) | Type::Tuple(args) => {
                for a in &args {
                    self.occurs_adjust(id, a)?;
                }
                Ok(())
            }
            Type::Fun(args, ret, eff) => {
                for a in &args {
                    self.occurs_adjust(id, a)?;
                }
                self.occurs_adjust(id, &ret)?;
                self.occurs_adjust(id, &eff)
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
    fn rewrite_row(
        &mut self,
        row: Type,
        label: InternedString,
    ) -> Result<(Type, Type), UnifyError> {
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
                let ext =
                    Type::RowExtend(label, Box::new(field.clone()), Box::new(new_rest.clone()));
                self.set_slot(id, Slot::Bound(ext));
                Ok((field, new_rest))
            }
            Type::RowEmpty => Err(UnifyError::MissingLabel(label)),
            Type::Error => Ok((Type::Error, Type::Error)),
            other => Err(UnifyError::Mismatch(other, Type::RowEmpty)),
        }
    }

    // --- generalize / instantiate -------------------------------------------

    fn quantify(&self, ty: &Type, map: &mut HashMap<u32, u32>, kinds: &mut Vec<VarKind>) -> Type {
        self.quantify_from(ty, Some(self.level), map, kinds)
    }

    /// [`Arena::quantify`], over every variable deeper than `level` -- or over
    /// every variable at all, for `None`.
    fn quantify_from(
        &self,
        ty: &Type,
        level: Option<u32>,
        map: &mut HashMap<u32, u32>,
        kinds: &mut Vec<VarKind>,
    ) -> Type {
        match ty {
            Type::Var(id) => {
                // `Num` vars are never generalized — a numeric literal is not
                // polymorphic. Left free here, then defaulted to `Int` in
                // `finish` unless a use site pins it to `BigInt` first.
                if level.is_none() && self.slot_kind(*id) == VarKind::Num {
                    // Closing a type only to print it: a literal nothing has
                    // pinned is going to be an `Int`, so say that.
                    return Type::int();
                }
                if self.slot_kind(*id) != VarKind::Num
                    && level.is_none_or(|l| self.slot_level(*id) > l)
                {
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
            Type::Error => Type::Error,
            Type::Con(name, args) => Type::Con(
                *name,
                args.iter()
                    .map(|a| self.quantify_from(a, level, map, kinds))
                    .collect(),
            ),
            Type::Fun(args, ret, eff) => Type::Fun(
                args.iter()
                    .map(|a| self.quantify_from(a, level, map, kinds))
                    .collect(),
                Box::new(self.quantify_from(ret, level, map, kinds)),
                Box::new(self.quantify_from(eff, level, map, kinds)),
            ),
            Type::Tuple(items) => Type::Tuple(
                items
                    .iter()
                    .map(|a| self.quantify_from(a, level, map, kinds))
                    .collect(),
            ),
            Type::Record(row) => Type::Record(Box::new(self.quantify_from(row, level, map, kinds))),
            Type::RowExtend(label, field, rest) => Type::RowExtend(
                *label,
                Box::new(self.quantify_from(field, level, map, kinds)),
                Box::new(self.quantify_from(rest, level, map, kinds)),
            ),
        }
    }

    fn subst_bound(ty: &Type, fresh: &[Type]) -> Type {
        match ty {
            Type::Bound(i) => fresh[*i as usize].clone(),
            Type::Var(id) => Type::Var(*id),
            Type::RowEmpty => Type::RowEmpty,
            Type::Error => Type::Error,
            Type::Con(name, args) => Type::Con(
                *name,
                args.iter().map(|a| Self::subst_bound(a, fresh)).collect(),
            ),
            Type::Fun(args, ret, eff) => Type::Fun(
                args.iter().map(|a| Self::subst_bound(a, fresh)).collect(),
                Box::new(Self::subst_bound(ret, fresh)),
                Box::new(Self::subst_bound(eff, fresh)),
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

    /// Fold another table's annotations into this one (used when a package is
    /// assembled from several separately-compiled modules). Node ids are only
    /// meaningful per module afterwards — this feeds the diagnostic dump.
    pub fn absorb(&mut self, other: TypeTable) {
        self.types.extend(other.types);
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
                let _ = write_type(
                    &mut s,
                    ty,
                    &mut namer,
                    Prec::Top,
                    &std::collections::HashSet::new(),
                );
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
    /// Every binding this run generalized -- top-level *and* local -- with the
    /// arena variables it quantified over.
    ///
    /// Lowering needs both halves. The scheme says what the binding's type is;
    /// the variables say which of the type annotations scattered through its
    /// body are the *same* variable, which is what a `TyLam` binds and a
    /// `TyApp` supplies. Inference is the only pass that knows this, and it
    /// knows it only while generalizing.
    pub generalized: HashMap<VarId, Generalized>,
    /// Every data / record type's constructors, for the exhaustiveness checker.
    pub variants: VariantEnv,
    /// The candidate chosen for each overloaded name -- see [`hir::Overloads`]
    /// and [`hir::apply_resolutions`].
    pub resolutions: HashMap<NodeId, hir::Alt>,
    pub errors: Vec<Diagnostic>,
}

/// The state of [`Infer::search`].
struct Search {
    /// Every assignment found so far: a candidate index per group member.
    solutions: Vec<Vec<usize>>,
    /// Candidates left to try before giving up.
    budget: usize,
    truncated: bool,
}

/// An overloaded name inference has not chosen a meaning for yet.
#[derive(Clone)]
struct Pending {
    /// The node the name was written at -- the key into [`hir::Overloads`].
    node: NodeId,
    candidates: Vec<hir::Candidate>,
    /// The type the context wants the name to have; what a candidate must
    /// unify with to be chosen.
    ty: Type,
    /// The level the name was mentioned at, which a chosen candidate is
    /// instantiated at -- as it would have been had it been written alone.
    level: u32,
    span: Span,
    filename: String,
    /// Nodes to give the error type if no candidate is ever chosen.
    poison: Vec<NodeId>,
}

/// A generalized binding, as lowering needs to see it.
#[derive(Debug, Clone)]
pub struct Generalized {
    pub scheme: Scheme,
    /// The arena variable behind each quantifier, in quantifier order. A
    /// monomorphic binding has none.
    pub vars: Vec<u32>,
}

/// `type name -> its constructors`, covering this unit *and* its dependencies.
pub type VariantEnv = HashMap<InternedString, Vec<VariantSig>>;

/// One constructor of a data / record type.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantSig {
    pub name: InternedString,
    /// Field types, written over the type's parameters as `Type::Bound(i)` —
    /// instantiate with [`subst_bound`] against the scrutinee's type arguments.
    pub fields: Vec<Type>,
    /// Field labels, for a `record` or a named `data` variant.
    pub labels: Option<Vec<InternedString>>,
}

/// Replace each `Type::Bound(i)` in `ty` with `args[i]`.
pub fn subst_bound(ty: &Type, args: &[Type]) -> Type {
    Arena::subst_bound(ty, args)
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
    /// Every binding generalized (or deliberately not) by this run — see
    /// [`InferResult::generalized`].
    generalized: HashMap<VarId, Generalized>,
    /// Data / record constructor schemes, e.g. `Leaf : ∀a. Vector a -> Node a`.
    ctors: HashMap<InternedString, Scheme>,
    /// The same information grouped by *type*, which is what an exhaustiveness
    /// check needs: given a scrutinee type, what are all its constructors?
    variants: VariantEnv,
    /// `tyname -> field -> accessor scheme` (`Person -> name -> ∀. Person -> String`).
    record_fields: HashMap<InternedString, HashMap<InternedString, Scheme>>,
    /// Declared effects and their operation signatures (for `handle` checking).
    effects: HashMap<InternedString, EffectInfo>,
    /// The effect row of the code region currently being inferred — an open row
    /// var that accumulates every effect performed in the current function body.
    /// Saved/restored around lambda bodies and `let` right-hand sides.
    cur_effect: Type,
    /// The meta variable standing for each type variable a pattern annotation
    /// introduced -- see [`Infer::annotation`]. Never scoped, because the
    /// resolver already scoped the `VarId`s it is keyed by.
    ann_tyvars: HashMap<VarId, Type>,
    /// Names the resolver found several meanings for -- see [`Infer::defer`].
    overloads: hir::Overloads,
    pending: Vec<Pending>,
    resolutions: HashMap<NodeId, hir::Alt>,
    errors: Vec<Diagnostic>,
}

impl Infer {
    /// Name the file diagnostics from here on point into — the driver moves
    /// this along as it walks the unit's modules.
    pub fn set_filename(&mut self, filename: impl Into<String>) {
        self.filename = filename.into();
    }

    pub fn new(filename: impl Into<String>, node_count: usize) -> Self {
        Infer {
            filename: filename.into(),
            arena: Arena::new(),
            env: HashMap::new(),
            generalized: HashMap::new(),
            table: TypeTable::new(node_count),
            exports: Vec::new(),
            ctors: HashMap::new(),
            variants: HashMap::new(),
            ann_tyvars: HashMap::new(),
            overloads: HashMap::new(),
            pending: Vec::new(),
            resolutions: HashMap::new(),
            record_fields: HashMap::new(),
            effects: HashMap::new(),
            cur_effect: Type::RowEmpty,
            errors: Vec::new(),
        }
    }

    /// Record that the current region performs effect `name` with type args `args`
    /// (used by operation calls in later phases).
    #[allow(dead_code)]
    fn emit_effect(&mut self, span: Span, name: InternedString, args: Vec<Type>) {
        let tail = self.arena.fresh_effect();
        let want = Type::RowExtend(name, Box::new(Type::Tuple(args)), Box::new(tail));
        self.unify_at(span, self.cur_effect.clone(), want);
    }

    /// Fold a called function's latent effect `phi` into the current region.
    /// A pure arrow (`phi` = `RowEmpty`) imposes nothing; an open effect row ties
    /// its tail to `cur_effect`, which is how effect polymorphism propagates
    /// (`map`'s effect ends up equal to its function argument's).
    fn join_effect(&mut self, span: Span, phi: Type) {
        if matches!(self.arena.zonk(&phi), Type::RowEmpty) {
            return;
        }
        self.unify_at(span, self.cur_effect.clone(), phi);
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

    /// Infer a module, one binding group at a time.
    ///
    /// `meadow-scc` has already put `decls` in dependency order and filled in
    /// [`hir::Module::groups`], so by the time a group is reached every binding it
    /// refers to is generalized and a mention of it instantiates properly. Within
    /// a recursive group the members stay monomorphic until all of them are
    /// solved — the usual binding-group discipline.
    pub fn infer_module(&mut self, module: &hir::LModule) {
        let module = module.value();
        // A declaration is not itself a value; the node type is only there so the
        // table has no holes.
        for decl in &module.decls {
            self.table.set(decl.id, Type::unit());
        }
        if module.groups.is_empty() {
            // Ungrouped HIR (the SCC pass didn't run): source order is all we have.
            for decl in &module.decls {
                if let hir::Decl::Bind(bind) = decl.value() {
                    self.infer_bind(bind, true);
                }
            }
            return;
        }
        for group in &module.groups {
            self.infer_group(&module.decls, group);
        }
    }

    /// Infer one strongly connected component of the top-level bindings.
    fn infer_group(&mut self, decls: &[hir::LDecl], group: &hir::BindGroup) {
        let binds: Vec<(&hir::Bind, Span)> = group
            .members
            .iter()
            .filter_map(|&i| match decls[i].value() {
                hir::Decl::Bind(bind) => Some((bind, decls[i].span)),
                _ => None,
            })
            .collect();

        // A lone non-recursive binding is just a binding.
        if !group.recursive && binds.len() == 1 {
            self.infer_bind(binds[0].0, true);
            return;
        }

        self.arena.enter_level();

        // Seed every name in the group before inferring any body, so a mention of
        // a sibling resolves to a variable the sibling's own inference constrains
        // rather than to an unrelated fresh one.
        let mut seeds: Vec<Vec<(VarId, Type)>> = Vec::with_capacity(binds.len());
        for (bind, span) in &binds {
            let seeded: Vec<(VarId, Type)> = bind
                .bound_vars()
                .into_iter()
                .map(|vid| {
                    let ty = self.arena.fresh();
                    self.bind_mono(vid, &ty, *span);
                    (vid, ty)
                })
                .collect();
            seeds.push(seeded);
        }

        // Every member of the group has to be pure for any of them to generalize:
        // the value restriction, applied to the group as a whole.
        let mut pure = true;
        for ((bind, _), seed) in binds.iter().zip(&seeds) {
            pure &= self.infer_group_member(bind, seed);
        }

        // A top-level group settles its own names: its type is final once it
        // generalizes, and an overload left open would leave it half-inferred.
        self.solve_overloads(true);
        self.arena.exit_level();

        for seed in &seeds {
            for (vid, ty) in seed {
                let scheme = if pure {
                    self.generalize_named(Some(*vid), ty)
                } else {
                    let s = Scheme::mono(self.arena.zonk(ty));
                    self.record_mono(*vid, &s);
                    s
                };
                self.env.insert(*vid, scheme);
                self.exports.push(*vid);
            }
        }
    }

    /// One member of a recursive group: infer its body and tie the result to the
    /// seed the rest of the group is seeing. Levels and generalization are the
    /// group's business, not this function's. Returns whether it was pure.
    fn infer_group_member(&mut self, bind: &hir::Bind, seed: &[(VarId, Type)]) -> bool {
        match bind {
            hir::Bind::Fun(name, params, declared, body) => {
                let mut bound = Vec::new();
                let param_tys: Vec<Type> = params
                    .iter()
                    .map(|p| self.infer_pat(p, &mut bound))
                    .collect();
                let ret = self.declared_result(declared);
                let body_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, body_eff.clone());

                let fn_ty = Type::func_eff(param_tys, ret.clone(), body_eff);
                if let Some((_, seed_ty)) = seed.first() {
                    self.unify_at(name.span, seed_ty.clone(), fn_ty.clone());
                }
                self.table.set(name.id, fn_ty.clone());

                let body_ty = self.infer_expr(body);
                self.unify_at(body.span, ret, body_ty);
                self.cur_effect = saved;
                self.table.set(name.id, self.arena.zonk(&fn_ty));
                // Defining a function performs no effects.
                true
            }
            hir::Bind::Pat(pat, expr) => {
                let rhs_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, rhs_eff.clone());
                let rhs = self.infer_expr(expr);
                let mut bound = Vec::new();
                let pty = self.infer_pat(pat, &mut bound);
                self.unify_at(pat.span, pty, rhs);
                self.cur_effect = saved;

                // `infer_pat` gave each name a fresh variable of its own; the group
                // has been looking at the seed instead.
                for (vid, vty) in bound {
                    if let Some((_, seed_ty)) = seed.iter().find(|(v, _)| *v == vid) {
                        self.unify_at(pat.span, seed_ty.clone(), vty);
                    }
                }
                matches!(self.arena.zonk(&rhs_eff), Type::RowEmpty | Type::Var(_))
            }
            hir::Bind::Error => true,
        }
    }

    pub fn finish(mut self) -> InferResult {
        // The whole unit has been seen: whatever is still ambiguous stays so.
        self.solve_overloads(true);
        // Any numeric literal context never pinned to `BigInt` is an `Int`.
        self.arena.default_num_vars();
        self.table.zonk_all(&mut self.arena);
        let mut schemes = HashMap::new();
        for id in self.exports.clone() {
            if let Some(scheme) = self.env.get(&id).cloned() {
                schemes.insert(id, self.normalize_scheme(&scheme));
            }
        }
        // Re-zonk: a variable can be solved after the binding that generalized
        // over it was recorded, and core's annotations have to agree with the
        // table's.
        let generalized = self
            .generalized
            .iter()
            .map(|(v, g)| {
                (
                    *v,
                    Generalized {
                        scheme: Scheme {
                            quant: g.scheme.quant.clone(),
                            ty: self.arena.zonk(&g.scheme.ty),
                        },
                        vars: g.vars.clone(),
                    },
                )
            })
            .collect();
        InferResult {
            table: self.table,
            schemes,
            generalized,
            variants: self.variants,
            resolutions: self.resolutions,
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

    /// Bind `vid` to `ty` monomorphically, first tying `ty` to whatever
    /// placeholder an earlier forward reference left behind for the name (see the
    /// `Expr::Var` arm). For everything else — every local, and every top-level
    /// binding the sort managed to order — there is no prior entry and this is a
    /// plain insert.
    fn bind_mono(&mut self, vid: VarId, ty: &Type, span: Span) {
        if let Some(prev) = self.env.get(&vid).cloned()
            && prev.quant.is_empty()
        {
            self.unify_at(span, prev.ty, ty.clone());
        }
        self.env.insert(vid, Scheme::mono(ty.clone()));
    }

    fn infer_bind(&mut self, bind: &hir::Bind, toplevel: bool) {
        match bind {
            hir::Bind::Fun(name, params, declared, body) => {
                let vid = *name.value();
                self.arena.enter_level();

                // Parameters are patterns (`fun f a (x, y) = …`); inferring each
                // binds the variables it introduces.
                let mut bound = Vec::new();
                let param_tys: Vec<Type> = params
                    .iter()
                    .map(|p| self.infer_pat(p, &mut bound))
                    .collect();
                let ret = self.declared_result(declared);
                // The body runs in its own effect region; that region ends up on the
                // function's (innermost) arrow. Defining the function is itself pure,
                // so the outer `cur_effect` is untouched.
                let body_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, body_eff.clone());

                // curried: `fun f a b = e` is `a -> b -> typeof(e) ! <body effect>`
                let fn_ty = Type::func_eff(param_tys, ret.clone(), body_eff);
                // Bind the name monomorphically first so the body can recurse.
                self.bind_mono(vid, &fn_ty, name.span);
                self.table.set(name.id, fn_ty.clone());

                let body_ty = self.infer_expr(body);
                self.unify_at(body.span, ret, body_ty);
                self.cur_effect = saved;
                // A local function may leave a name to the body around it; a
                // top-level one has nothing around it to wait for.
                self.solve_overloads(toplevel);
                self.arena.exit_level();

                let scheme = self.generalize_named(Some(vid), &fn_ty);
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
                let rhs_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, rhs_eff.clone());
                let rhs = self.infer_expr(expr);
                let mut bound = Vec::new();
                let pty = self.infer_pat(pat, &mut bound);
                self.unify_at(pat.span, pty, rhs);
                self.cur_effect = saved;
                self.solve_overloads(toplevel);
                self.arena.exit_level();

                // The value restriction, replaced: generalize a `let`/`def` binding
                // only when its right-hand side is pure. `def r = ref []` is
                // effectful ⇒ `r` stays monomorphic (and can't be misused
                // polymorphically); `def id = \x -> x` is pure ⇒ generalized.
                let pure = matches!(self.arena.zonk(&rhs_eff), Type::RowEmpty | Type::Var(_));
                if !pure && !toplevel {
                    // let the enclosing region see the rhs's effects
                    self.unify_at(pat.span, self.cur_effect.clone(), rhs_eff);
                }

                for (vid, vty) in bound {
                    let scheme = if pure {
                        self.generalize_named(Some(vid), &vty)
                    } else {
                        let s = Scheme::mono(self.arena.zonk(&vty));
                        self.record_mono(vid, &s);
                        s
                    };
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
            hir::Expr::Lit(hir::Lit::Int(_)) => self.arena.fresh_num(),
            hir::Expr::Lit(hir::Lit::Float(_)) => Type::float(),
            hir::Expr::Lit(hir::Lit::String(_)) => Type::string(),
            hir::Expr::Lit(hir::Lit::Char(_)) => Type::char(),
            hir::Expr::Unit => Type::unit(),

            hir::Expr::Var(ident) if self.overloads.contains_key(&ident.id) => {
                let ty = self.defer(ident.id, ident.span, vec![expr.id, ident.id]);
                self.table.set(ident.id, ty.clone());
                ty
            }
            hir::Expr::Var(ident) => {
                let ty = match self.env.get(&*ident.value()).cloned() {
                    Some(scheme) => self.instantiate(&scheme),
                    None => {
                        // Either an unresolved name (the resolver has already said
                        // so) or a top-level binding in a dependency cycle that
                        // `meadow-scc` could not break — mutually recursive
                        // modules, in practice. Give the name one shared
                        // monomorphic placeholder rather than an unrelated fresh
                        // variable per mention: that is what makes the constraints
                        // from the uses meet the eventual definition instead of
                        // being silently discarded. The outermost level keeps any
                        // enclosing binding from generalizing over it, which would
                        // be a promise of polymorphism the definition never made.
                        let ty = self.arena.fresh_global();
                        self.env.insert(*ident.value(), Scheme::mono(ty.clone()));
                        ty
                    }
                };
                self.table.set(ident.id, ty.clone());
                ty
            }

            hir::Expr::Lam(params, body) => {
                // Multi-parameter lambdas curry: `\a b -> e` is `\a -> \b -> e`.
                // The body has its own effect region, which lands on the arrow;
                // building a closure is pure, so the ambient effect is untouched.
                let mut bound = Vec::new();
                let ptys: Vec<Type> = params
                    .iter()
                    .map(|p| self.infer_pat(p, &mut bound))
                    .collect();
                let body_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, body_eff.clone());
                let bty = self.infer_expr(body);
                self.cur_effect = saved;
                Type::func_eff(ptys, bty, body_eff)
            }

            hir::Expr::App(func, args) => {
                // n-ary application is a fold of single-argument applications. Only
                // the final (saturating) call actually runs the function body, so
                // only its latent effect joins the current region; the intermediate
                // arrows of a curried call just build closures and stay pure — which
                // is also how every function type here is constructed (`func_eff`).
                let mut fty = self.infer_expr(func);
                let last = args.len().saturating_sub(1);
                for (i, arg) in args.iter().enumerate() {
                    let aty = self.infer_expr(arg);
                    let ret = self.arena.fresh();
                    let phi = self.arena.fresh_effect();
                    self.unify_at(
                        expr.span,
                        fty,
                        Type::Fun(vec![aty], Box::new(ret.clone()), Box::new(phi.clone())),
                    );
                    if i == last {
                        self.join_effect(expr.span, phi);
                    } else {
                        self.unify_at(expr.span, phi, Type::RowEmpty);
                    }
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

            hir::Expr::Array(items) => {
                let elem = self.arena.fresh();
                for e in items {
                    let t = self.infer_expr(e);
                    self.unify_at(e.span, t, elem.clone());
                }
                Type::array(elem)
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
                let known = if self.overloads.contains_key(&label.id) {
                    Some(self.defer(label.id, label.span, vec![expr.id, label.id]))
                } else {
                    self.ctor_type(name)
                };
                match known {
                    Some(mut cty) => {
                        self.table.set(label.id, cty.clone());
                        // apply the constructor to its arguments, one at a time
                        // (constructors are pure — the fresh effect var unifies away)
                        for arg in args {
                            let aty = self.infer_expr(arg);
                            let ret = self.arena.fresh();
                            let eff = self.arena.fresh_effect();
                            self.unify_at(
                                arg.span,
                                cty,
                                Type::Fun(vec![aty], Box::new(ret.clone()), Box::new(eff)),
                            );
                            cty = ret;
                        }
                        cty
                    }
                    None => {
                        // Unknown constructor, which the resolver has reported.
                        for a in args {
                            self.infer_expr(a);
                        }
                        Type::Error
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
                        let eff = self.arena.fresh_effect();
                        self.unify_at(
                            expr.span,
                            accessor,
                            Type::Fun(vec![pruned], Box::new(res.clone()), Box::new(eff)),
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

            hir::Expr::Handle(body, arms, ret) => {
                // The handled expression runs in its own effect region.
                let body_eff = self.arena.fresh_effect();
                let saved = std::mem::replace(&mut self.cur_effect, body_eff.clone());
                let body_ty = self.infer_expr(body);
                self.cur_effect = saved;

                let result = self.arena.fresh();
                let ename = arms.first().and_then(|a| self.op_effect(a.op));

                match ename.and_then(|n| self.effects.get(&n).cloned().map(|i| (n, i))) {
                    Some((ename, info)) => {
                        let fresh_params: Vec<Type> =
                            (0..info.params).map(|_| self.arena.fresh()).collect();
                        let rho = self.arena.fresh_effect();
                        let handled = Type::RowExtend(
                            ename,
                            Box::new(Type::Tuple(fresh_params.clone())),
                            Box::new(rho.clone()),
                        );
                        self.unify_at(expr.span, body_eff, handled);
                        // effects the handler lets through join the ambient region
                        self.join_effect(expr.span, rho.clone());

                        for arm in arms {
                            let (arg_ty, ret_ty) = match info.ops.iter().find(|o| o.name == arm.op)
                            {
                                Some(o) => (
                                    Arena::subst_bound(&o.arg, &fresh_params),
                                    Arena::subst_bound(&o.ret, &fresh_params),
                                ),
                                None => (self.arena.fresh(), self.arena.fresh()),
                            };
                            let mut bound = Vec::new();
                            let pty = self.infer_pat(&arm.param, &mut bound);
                            self.unify_at(arm.param.span, pty, arg_ty);
                            // resume : op-result -> handler-result ! ρ   (deep)
                            let k_ty = Type::Fun(
                                vec![ret_ty],
                                Box::new(result.clone()),
                                Box::new(rho.clone()),
                            );
                            self.env.insert(*arm.resume.value(), Scheme::mono(k_ty));
                            let at = self.infer_expr(&arm.body);
                            self.unify_at(arm.body.span, at, result.clone());
                        }
                    }
                    None => {
                        for arm in arms {
                            let mut bound = Vec::new();
                            self.infer_pat(&arm.param, &mut bound);
                            let k = self.arena.fresh();
                            self.env.insert(*arm.resume.value(), Scheme::mono(k));
                            self.infer_expr(&arm.body);
                        }
                    }
                }

                match ret {
                    Some((pat, rbody)) => {
                        let mut bound = Vec::new();
                        let pty = self.infer_pat(pat, &mut bound);
                        self.unify_at(pat.span, pty, body_ty);
                        let rt = self.infer_expr(rbody);
                        self.unify_at(rbody.span, rt, result.clone());
                    }
                    None => self.unify_at(expr.span, result.clone(), body_ty),
                }
                result
            }

            hir::Expr::Error => Type::Error,
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
            hir::Pat::Lit(hir::Lit::Int(_)) => self.arena.fresh_num(),
            hir::Pat::Lit(hir::Lit::Float(_)) => Type::float(),
            hir::Pat::Lit(hir::Lit::String(_)) => Type::string(),
            hir::Pat::Lit(hir::Lit::Char(_)) => Type::char(),

            hir::Pat::Var(ident) => {
                let vid = *ident.value();
                let t = self.arena.fresh();
                self.bind_mono(vid, &t, ident.span);
                self.table.set(ident.id, t.clone());
                bound.push((vid, t.clone()));
                t
            }

            // `(p : T)` — infer the pattern, then hold it to the annotation.
            //
            // A type variable written here is *not* one of an enclosing
            // declaration's parameters: there is no enclosing declaration to be
            // a parameter of. Each distinct one is quantified over the
            // annotation alone and then instantiated, so `(x : a)` constrains
            // nothing and `(f : a -> a)` constrains the two ends to agree —
            // which is what writing it twice is for.
            hir::Pat::Ann(inner, ann) => {
                let inferred = self.infer_pat(inner, bound);
                let declared = self.annotation(ann);
                self.unify_at(pat.span, inferred.clone(), declared);
                inferred
            }

            hir::Pat::As(ident, sub) => {
                let st = self.infer_pat(sub, bound);
                let vid = *ident.value();
                self.bind_mono(vid, &st, ident.span);
                self.table.set(ident.id, st.clone());
                bound.push((vid, st.clone()));
                st
            }

            hir::Pat::Cons(label, args) => {
                let name = *label.value();
                let known = if self.overloads.contains_key(&label.id) {
                    Some(self.defer(label.id, label.span, vec![pat.id, label.id]))
                } else {
                    self.ctor_type(name)
                };
                match known {
                    Some(mut cty) => {
                        for sub in args {
                            let sty = self.infer_pat(sub, bound);
                            let ret = self.arena.fresh();
                            let eff = self.arena.fresh_effect();
                            self.unify_at(
                                sub.span,
                                cty,
                                Type::Fun(vec![sty], Box::new(ret.clone()), Box::new(eff)),
                            );
                            cty = ret;
                        }
                        self.table.set(label.id, cty.clone());
                        cty
                    }
                    None => {
                        // Unknown constructor, which the resolver has reported.
                        for a in args {
                            self.infer_pat(a, bound);
                        }
                        Type::Error
                    }
                }
            }

            hir::Pat::Tuple(items) => {
                Type::Tuple(items.iter().map(|p| self.infer_pat(p, bound)).collect())
            }

            hir::Pat::Array(items) => {
                let elem = self.arena.fresh();
                for p in items {
                    let t = self.infer_pat(p, bound);
                    self.unify_at(p.span, t, elem.clone());
                }
                Type::array(elem)
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

            hir::Pat::Error => Type::Error,
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
            "List.Nil" => Type::list(self.arena.fresh()),
            "List.Cons" => {
                let a = self.arena.fresh();
                Type::func(vec![a.clone(), Type::list(a.clone())], Type::list(a))
            }
            "Bool.True" | "Bool.False" => Type::bool(),
            _ => return None,
        })
    }

    /// Export the operations of every `effect` in `decls`, so a dependent can
    /// reach them by qualifier (`State.get`) or name them in a `use`.
    ///
    /// [`register_types`] gives every operation of every dependency a scheme in
    /// the environment, which is what makes them callable unqualified — but that
    /// happens for dependencies too, so it cannot tell which are *this* unit's to
    /// export. The caller knows, and calls this with only the local modules.
    ///
    /// [`register_types`]: Self::register_types
    pub fn export_effect_ops(&mut self, decls: &[hir::LDecl]) {
        for d in decls {
            if let hir::Decl::Effect(ed) = d.value() {
                for (_, op, _) in &ed.ops {
                    self.exports.push(*op.value());
                }
            }
        }
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
                    self.record_ctor(rd.name, rd.ctor, &quant, &head, &fields);
                }
                hir::Decl::Effect(ed) => {
                    let params = param_map(&ed.params);
                    let n = ed.params.len();
                    // `{ EffName p0 .. p{n-1} | e }` where `e = Bound(n)`
                    let head_args: Vec<Type> = (0..n as u32).map(Type::Bound).collect();
                    let mut ops = Vec::new();
                    for (opname, opvar, opty) in &ed.ops {
                        let t = ty_of(opty, &params);
                        let (arg, ret) = match &t {
                            Type::Fun(a, r, _) => (a[0].clone(), (**r).clone()),
                            _ => (Type::unit(), t.clone()),
                        };
                        let mut quant = vec![VarKind::Type; n];
                        quant.push(VarKind::Effect);
                        let eff_row = Type::RowExtend(
                            ed.name,
                            Box::new(Type::Tuple(head_args.clone())),
                            Box::new(Type::Bound(n as u32)),
                        );
                        self.env.insert(
                            *opvar.value(),
                            Scheme {
                                quant,
                                ty: Type::Fun(
                                    vec![arg.clone()],
                                    Box::new(ret.clone()),
                                    Box::new(eff_row),
                                ),
                            },
                        );
                        ops.push(EffOp {
                            name: *opname,
                            arg,
                            ret,
                        });
                    }
                    self.effects.insert(ed.name, EffectInfo { params: n, ops });
                }
                _ => {}
            }
        }
    }

    fn op_effect(&self, op: InternedString) -> Option<InternedString> {
        self.effects
            .iter()
            .find(|(_, info)| info.ops.iter().any(|o| o.name == op))
            .map(|(name, _)| *name)
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
            Type::func(field_tys.clone(), head.clone())
        };
        self.ctors.insert(
            ctor,
            Scheme {
                quant: quant.to_vec(),
                ty: cty,
            },
        );
        let labels: Option<Vec<InternedString>> = fields
            .iter()
            .map(|(n, _)| *n)
            .collect::<Option<Vec<_>>>()
            .filter(|ls| !ls.is_empty());
        let sig = VariantSig {
            name: ctor,
            fields: field_tys,
            labels,
        };
        // `register_types` runs once per unit and again for each dependency, so a
        // re-registered constructor replaces rather than duplicates its entry.
        let group = self.variants.entry(tyname).or_default();
        match group.iter_mut().find(|v| v.name == ctor) {
            Some(existing) => *existing = sig,
            None => group.push(sig),
        }
        let accessors = self.record_fields.entry(tyname).or_default();
        for (name, t) in fields {
            if let Some(name) = name {
                accessors.insert(
                    *name,
                    Scheme {
                        quant: quant.to_vec(),
                        ty: Type::Fun(
                            vec![head.clone()],
                            Box::new(t.clone()),
                            Box::new(Type::RowEmpty),
                        ),
                    },
                );
            }
        }
    }

    /// Generalize, and record what was quantified under `vid`.
    ///
    /// The arena variables are kept, not just their count: a `TyLam` in core
    /// binds them and the annotations inside the binding's body mention them,
    /// so the two have to agree about which variable is which.
    fn generalize_named(&mut self, vid: Option<VarId>, ty: &Type) -> Scheme {
        // A name still waiting on its overload has a type that is not settled,
        // and generalizing over it would let each use pick differently.
        let level = self.arena.level;
        for i in 0..self.pending.len() {
            let t = self.pending[i].ty.clone();
            self.arena.hold_back(&t, level);
        }
        let z = self.arena.zonk(ty);
        let mut map = HashMap::new();
        let mut kinds = Vec::new();
        let body = self.arena.quantify(&z, &mut map, &mut kinds);
        let scheme = Scheme {
            quant: kinds,
            ty: body,
        };
        if let Some(vid) = vid {
            // `map` is arena var -> quantifier index; invert it.
            let mut vars = vec![0u32; scheme.quant.len()];
            for (&var, &idx) in &map {
                vars[idx as usize] = var;
            }
            self.generalized.insert(vid, Generalized { scheme: scheme.clone(), vars });
        }
        scheme
    }

    /// Record a binding that was *not* generalized, so lowering can find every
    /// binding in one place.
    fn record_mono(&mut self, vid: VarId, scheme: &Scheme) {
        self.generalized.insert(
            vid,
            Generalized { scheme: scheme.clone(), vars: Vec::new() },
        );
    }

    // --- overloaded names ------------------------------------------------------

    /// Hand over the names the resolver found several meanings for. Call before
    /// [`Infer::infer_module`].
    pub fn set_overloads(&mut self, overloads: hir::Overloads) {
        self.overloads = overloads;
    }

    /// Give an overloaded name a placeholder type and put off choosing its
    /// meaning until the context has constrained that type.
    ///
    /// Choosing is settled by unification alone: a candidate *fits* when its
    /// type unifies with everything the placeholder has been asked to be. The
    /// set of fitting candidates can only shrink as constraints arrive, so the
    /// moment one is left it is the answer, and the moment none is there never
    /// will be one -- which is what lets [`Infer::solve_overloads`] decide
    /// early, at each binding, rather than only at the end of the unit.
    fn defer(&mut self, node: NodeId, span: Span, poison: Vec<NodeId>) -> Type {
        let ty = self.arena.fresh();
        let candidates = self.overloads.get(&node).cloned().unwrap_or_default();
        self.pending.push(Pending {
            node,
            candidates,
            ty: ty.clone(),
            level: self.arena.level,
            span,
            filename: self.filename.clone(),
            poison,
        });
        ty
    }

    /// Choose what can be chosen. `last` is the end of the unit, when a name
    /// that several candidates still fit is reported as ambiguous.
    fn solve_overloads(&mut self, last: bool) {
        if self.pending.is_empty() {
            return;
        }
        // Choosing one name can constrain another, so go round until nothing
        // changes.
        loop {
            let mut progress = false;
            let mut i = 0;
            while i < self.pending.len() {
                let fits = self.fitting(&self.pending[i].clone());
                match fits.as_slice() {
                    [] => {
                        let p = self.pending.remove(i);
                        self.report_overload(&p, &fits);
                        progress = true;
                    }
                    [only] => {
                        let p = self.pending.remove(i);
                        self.choose(&p, *only);
                        progress = true;
                    }
                    _ => i += 1,
                }
            }
            // One name at a time is stuck. Names whose types are tied together
            // may still have only one way to be read *together* --
            // `size (Just 5)`, with two `size`s and two `Just`s, say.
            if !progress {
                progress = self.solve_jointly();
            }
            if !progress {
                break;
            }
        }
        if last {
            for p in std::mem::take(&mut self.pending) {
                let fits = self.fitting(&p);
                self.report_overload(&p, &fits);
            }
        }
    }

    /// Settle what can be settled by reading tied-together names as a group.
    ///
    /// Pending names that share a type variable form a component, and each
    /// component is searched for every combination of candidates that unifies
    /// all at once. A name every such combination agrees on is decided; a
    /// component with no combination at all is an error. The search backtracks
    /// on snapshots and gives up past a fixed budget -- a component that large
    /// is left to be reported as ambiguous rather than explored.
    ///
    /// Returns whether anything was decided.
    fn solve_jointly(&mut self) -> bool {
        let n = self.pending.len();
        if n < 2 {
            return false;
        }
        // Components, by shared free variables.
        let vars: Vec<Vec<u32>> = (0..n)
            .map(|i| {
                let t = self.pending[i].ty.clone();
                let mut out = Vec::new();
                self.arena.free_vars(&t, &mut out);
                out
            })
            .collect();
        fn find(comp: &mut [usize], i: usize) -> usize {
            if comp[i] != i {
                let r = find(comp, comp[i]);
                comp[i] = r;
            }
            comp[i]
        }
        let mut comp: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in (i + 1)..n {
                if vars[i].iter().any(|v| vars[j].contains(v)) {
                    let (a, b) = (find(&mut comp, i), find(&mut comp, j));
                    comp[a] = b;
                }
            }
        }
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut root_of: HashMap<usize, usize> = HashMap::new();
        for i in 0..n {
            let root = find(&mut comp, i);
            let g = *root_of.entry(root).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[g].push(i);
        }

        let mut decided: Vec<(NodeId, usize)> = Vec::new();
        let mut hopeless: Vec<Vec<NodeId>> = Vec::new();
        for members in &groups {
            if members.len() < 2 {
                continue;
            }
            let group: Vec<Pending> = members.iter().map(|&i| self.pending[i].clone()).collect();
            let mut search = Search { solutions: Vec::new(), budget: 512, truncated: false };
            self.search(&group, &mut Vec::new(), &mut search);
            if search.truncated {
                continue;
            }
            if search.solutions.is_empty() {
                hopeless.push(group.iter().map(|p| p.node).collect());
                continue;
            }
            for (j, p) in group.iter().enumerate() {
                let k = search.solutions[0][j];
                if search.solutions.iter().all(|s| s[j] == k) {
                    decided.push((p.node, k));
                }
            }
        }

        let progress = !decided.is_empty() || !hopeless.is_empty();
        for (node, k) in decided {
            if let Some(i) = self.pending.iter().position(|p| p.node == node) {
                let p = self.pending.remove(i);
                self.choose(&p, k);
            }
        }
        for nodes in hopeless {
            let group: Vec<Pending> = nodes
                .iter()
                .filter_map(|node| {
                    let i = self.pending.iter().position(|p| p.node == *node)?;
                    Some(self.pending.remove(i))
                })
                .collect();
            self.report_combination(&group);
        }
        progress
    }

    /// Depth-first over the group's candidates, recording each full assignment
    /// that unifies, until the budget runs out.
    fn search(&mut self, group: &[Pending], chosen: &mut Vec<usize>, s: &mut Search) {
        if s.truncated {
            return;
        }
        let depth = chosen.len();
        if depth == group.len() {
            s.solutions.push(chosen.clone());
            if s.solutions.len() > 64 {
                s.truncated = true;
            }
            return;
        }
        let p = &group[depth];
        for (k, c) in p.candidates.iter().enumerate() {
            if s.budget == 0 {
                s.truncated = true;
                return;
            }
            s.budget -= 1;
            let snap = self.arena.snapshot();
            let outer = std::mem::replace(&mut self.arena.level, p.level);
            let fits = match self.candidate_type(c.alt) {
                Some(t) => self.arena.unify(p.ty.clone(), t).is_ok(),
                None => true,
            };
            self.arena.level = outer;
            if fits {
                chosen.push(k);
                self.search(group, chosen, s);
                chosen.pop();
            }
            self.arena.rollback(snap);
        }
    }

    /// Several names that each fit alone, but no reading of them fits together.
    fn report_combination(&mut self, group: &[Pending]) {
        let Some(first) = group.first() else { return };
        let mut names: Vec<String> = Vec::new();
        for p in group {
            let name = format!("`{}`", p.candidates.first().map(|c| c.name).unwrap_or_default());
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let mut lines = vec![
            format!("no reading of {} fits here", names.join(" and ")),
            "candidates in scope:".to_string(),
        ];
        for p in group {
            for c in &p.candidates {
                lines.push(self.candidate_line(c, ""));
            }
        }
        self.errors.push(Diagnostic {
            msg: lines.join("\n"),
            filename: first.filename.clone(),
            label: ("no combination of these candidates has a type".to_string(), first.span),
            extra_labels: group[1..].iter().map(|p| ("and this".to_string(), p.span)).collect(),
        });
        for p in group {
            for node in &p.poison {
                self.table.set(*node, Type::Error);
            }
            let _ = self.arena.unify(p.ty.clone(), Type::Error);
        }
    }

    /// One line of a candidate list: how to write it, its type, where it is from.
    fn candidate_line(&self, c: &hir::Candidate, mark: &str) -> String {
        let (spelled, scheme) = match c.alt {
            hir::Alt::Ctor(ctor) => (ctor.to_string(), self.ctors.get(&ctor).cloned()),
            hir::Alt::Value(v) => (c.name.to_string(), self.env.get(&v).cloned()),
        };
        let ty = match scheme {
            Some(s) => show_scheme_body(&s),
            None => "?".to_string(),
        };
        let from = match (c.alt, c.origin) {
            (hir::Alt::Ctor(_), _) => String::new(),
            (_, Some(m)) => format!(", from `{m}`"),
            (_, None) => ", from this module".to_string(),
        };
        format!("  {spelled} : {ty}{from}{mark}")
    }

    /// The candidates of `p` whose type unifies with the one wanted, by index.
    /// Each is tried against a snapshot and rolled back.
    fn fitting(&mut self, p: &Pending) -> Vec<usize> {
        let (want, level) = (p.ty.clone(), p.level);
        let alts: Vec<hir::Alt> = p.candidates.iter().map(|c| c.alt).collect();
        let mut out = Vec::new();
        for (k, alt) in alts.into_iter().enumerate() {
            let snap = self.arena.snapshot();
            let outer = std::mem::replace(&mut self.arena.level, level);
            let fits = match self.candidate_type(alt) {
                Some(t) => self.arena.unify(want.clone(), t).is_ok(),
                // Nothing known about it yet: it cannot be ruled out.
                None => true,
            };
            self.arena.level = outer;
            self.arena.rollback(snap);
            if fits {
                out.push(k);
            }
        }
        out
    }

    /// A candidate's type, freshly instantiated -- or `None` for a value whose
    /// type is not known yet, which only a dependency cycle leaves behind.
    fn candidate_type(&mut self, alt: hir::Alt) -> Option<Type> {
        match alt {
            hir::Alt::Ctor(c) => self.ctor_type(c),
            hir::Alt::Value(v) => {
                let scheme = self.env.get(&v).cloned()?;
                Some(self.instantiate(&scheme))
            }
        }
    }

    /// Commit to candidate `k`: unify for real and remember the choice.
    fn choose(&mut self, p: &Pending, k: usize) {
        let alt = p.candidates[k].alt;
        let outer = std::mem::replace(&mut self.arena.level, p.level);
        let ty = match self.candidate_type(alt) {
            Some(t) => t,
            None => {
                // The same placeholder an unordered reference would get (see
                // `Expr::Var`), so the definition still meets this use.
                let hir::Alt::Value(v) = alt else {
                    unreachable!("a constructor always has a type")
                };
                let t = self.arena.fresh_global();
                self.env.insert(v, Scheme::mono(t.clone()));
                t
            }
        };
        self.arena.level = outer;
        self.unify_at(p.span, p.ty.clone(), ty);
        self.resolutions.insert(p.node, alt);
    }

    /// A wanted type as a person would write it: free variables lettered, and
    /// an effect variable that says nothing left out, as a scheme prints.
    fn show_wanted(&mut self, ty: &Type) -> String {
        let z = self.arena.zonk(ty);
        let (mut map, mut kinds) = (HashMap::new(), Vec::new());
        let body = self.arena.quantify_from(&z, None, &mut map, &mut kinds);
        show_scheme_body(&Scheme {
            quant: kinds,
            ty: body,
        })
    }

    /// Say why a name could not be settled -- no candidate fits (`fits` is
    /// empty), or more than one does -- and poison it so that is all that is said.
    fn report_overload(&mut self, p: &Pending, fits: &[usize]) {
        let want = self.show_wanted(&p.ty);
        let name = p.candidates.first().map(|c| c.name).unwrap_or_default();
        let (msg, label) = if fits.is_empty() {
            (
                format!("no `{name}` in scope has the type needed here, `{}`", want),
                format!("none of the {} `{name}`s in scope fits", p.candidates.len()),
            )
        } else {
            (
                format!(
                    "ambiguous `{name}`: {} of the candidates in scope fit `{}`",
                    fits.len(),
                    want
                ),
                "add a type annotation, or write the name qualified".to_string(),
            )
        };
        let mut lines = vec![msg, "candidates in scope:".to_string()];
        for (k, c) in p.candidates.iter().enumerate() {
            let mark = if !fits.is_empty() && fits.contains(&k) { "  (fits)" } else { "" };
            lines.push(self.candidate_line(c, mark));
        }
        let diag = Diagnostic {
            msg: lines.join("\n"),
            filename: p.filename.clone(),
            label: (label, p.span),
            extra_labels: vec![],
        };
        self.errors.push(diag);
        // Unresolved, the node keeps the resolver's placeholder meaning, which
        // is as likely to be wrong as right: nothing after this should trust it.
        for n in &p.poison {
            self.table.set(*n, Type::Error);
        }
        let _ = self.arena.unify(p.ty.clone(), Type::Error);
    }

    /// The declared result type of a binding, or a fresh variable when there is
    /// none.
    ///
    /// A declared one is an ordinary meta variable that the body is unified
    /// against, so it constrains rather than merely records: writing
    /// `fun f x : Int = x` makes `f` an `Int -> Int` and an incompatible body
    /// an error, instead of generalising over whatever the body happened to be.
    fn declared_result(&mut self, declared: &Option<hir::LTypeExpr>) -> Type {
        match declared {
            Some(t) => self.annotation(t),
            None => self.arena.fresh(),
        }
    }

    /// A pattern annotation as an inference type.
    ///
    /// Each type variable becomes a meta variable, and the *same* one every
    /// time that variable appears — which is why the map is keyed by `VarId`
    /// and not rebuilt per annotation. The resolver gives a declaration's
    /// annotations one scope, so the two `a`s in
    /// `fun twice (f : a -> a) (x : a)` share a `VarId`, and sharing a `VarId`
    /// has to mean sharing a type or the annotation would be decoration.
    ///
    /// Nothing needs clearing between declarations: the resolver clears its own
    /// scope, so a different declaration's `a` is a different `VarId`.
    fn annotation(&mut self, t: &hir::LTypeExpr) -> Type {
        let mut vars = HashMap::new();
        collect_tyvars(t, &mut vars);
        // Index them for `ty_of`, then map each `Bound` to the meta this
        // declaration has already agreed on for that variable.
        let mut fresh = vec![Type::unit(); vars.len()];
        for (var, i) in &vars {
            let meta = match self.ann_tyvars.get(var) {
                Some(t) => t.clone(),
                None => {
                    let t = self.arena.fresh();
                    self.ann_tyvars.insert(*var, t.clone());
                    t
                }
            };
            fresh[*i as usize] = meta;
        }
        Arena::subst_bound(&ty_of(t, &vars), &fresh)
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
        let (whole_a, whole_b) = (a.clone(), b.clone());
        if let Err(err) = self.arena.unify(a, b) {
            // Part of either side is already an error: the mismatch is that
            // error seen again from here, not a new one.
            if self.arena.zonk(&whole_a).references_error()
                || self.arena.zonk(&whole_b).references_error()
            {
                return;
            }
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
                format!(
                    "function applied to the wrong number of arguments: expected {x}, found {y}"
                ),
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
            None => Type::Error, // rename already reported the unbound tyvar
        },
        hir::TypeExpr::Con(name, args) => {
            let args: Vec<Type> = args.iter().map(|a| ty_of(a, params)).collect();
            match &**name.value() {
                "Int" => Type::int(),
                "BigInt" => Type::bigint(),
                "Float" => Type::float(),
                "String" => Type::string(),
                "Bool" => Type::bool(),
                "Unit" => Type::unit(),
                "List" => Type::list(args.into_iter().next().unwrap_or_else(Type::unit)),
                "Array" => Type::array(args.into_iter().next().unwrap_or_else(Type::unit)),
                "Ref" => Type::reference(args.into_iter().next().unwrap_or_else(Type::unit)),
                _ => Type::Con(*name.value(), args),
            }
        }
        hir::TypeExpr::Fun(ps, r, eff) => {
            let effty = match eff {
                Some(row) => eff_of(row, params),
                None => Type::RowEmpty,
            };
            Type::func_eff(
                ps.iter().map(|p| ty_of(p, params)).collect(),
                ty_of(r, params),
                effty,
            )
        }
        hir::TypeExpr::Tuple(ts) => Type::Tuple(ts.iter().map(|x| ty_of(x, params)).collect()),
        // `[T]` type syntax now denotes the RRB `Vector`; write `List T` for a list.
        hir::TypeExpr::Vector(x) => Type::vector(ty_of(x, params)),
        hir::TypeExpr::List(x) => Type::list(ty_of(x, params)),
        hir::TypeExpr::Error => Type::Error,
    }
}

/// Convert a resolved effect row into an effect [`Type`].
fn eff_of(row: &hir::EffectRow, params: &HashMap<VarId, u32>) -> Type {
    let tail = match &row.tail {
        Some(v) => params
            .get(v.value())
            .map(|&i| Type::Bound(i))
            .unwrap_or(Type::RowEmpty),
        None => Type::RowEmpty,
    };
    row.labels.iter().rev().fold(tail, |rest, (name, args)| {
        let argtup = Type::Tuple(args.iter().map(|a| ty_of(a, params)).collect());
        Type::RowExtend(*name, Box::new(argtup), Box::new(rest))
    })
}

// ===========================================================================
// Primitive signatures (indexed by `rename::PRIMS`)
// ===========================================================================

/// The row `{ Mut | Bound(tail) }` — the effect every `Ref` operation carries.
fn mut_row(tail: u32) -> Type {
    Type::RowExtend(
        InternedString::from("Mut"),
        Box::new(Type::Tuple(vec![])),
        Box::new(Type::Bound(tail)),
    )
}

fn prim_scheme(name: &str) -> Option<Scheme> {
    use Type::*;
    // `∀a. <ty>` where `a` is `Bound(0)`.
    let a1 = |ty: Type| Scheme {
        quant: vec![VarKind::Type],
        ty,
    };
    let s = match name {
        "+" | "-" | "*" | "/" | "%" | "^" => {
            Scheme::mono(Type::func(vec![Type::int(), Type::int()], Type::int()))
        }
        "<" | ">" | "<=" | ">=" => {
            Scheme::mono(Type::func(vec![Type::int(), Type::int()], Type::bool()))
        }
        "+." | "-." | "*." | "/." => Scheme::mono(Type::func(
            vec![Type::float(), Type::float()],
            Type::float(),
        )),
        "<." | ">." | "<=." | ">=." => {
            Scheme::mono(Type::func(vec![Type::float(), Type::float()], Type::bool()))
        }
        "+~" | "-~" | "*~" | "/~" | "%~" | "^~" => Scheme::mono(Type::func(
            vec![Type::bigint(), Type::bigint()],
            Type::bigint(),
        )),
        "<~" | ">~" | "<=~" | ">=~" => Scheme::mono(Type::func(
            vec![Type::bigint(), Type::bigint()],
            Type::bool(),
        )),
        "toFloat" => Scheme::mono(Type::func(vec![Type::int()], Type::float())),
        "floor" => Scheme::mono(Type::func(vec![Type::float()], Type::int())),
        "toBigInt" => Scheme::mono(Type::func(vec![Type::int()], Type::bigint())),
        "toInt" => Scheme::mono(Type::func(vec![Type::bigint()], Type::int())),
        "neg" => Scheme::mono(Type::func(vec![Type::int()], Type::int())),
        // --- builtin `Array` (all `∀a. …`) ---
        "arrayLen" => a1(Type::func(vec![Type::array(Bound(0))], Type::int())),
        "arrayGet" => a1(Type::func(
            vec![Type::array(Bound(0)), Type::int()],
            Bound(0),
        )),
        "arrayGetOr" => a1(Type::func(
            vec![Bound(0), Type::array(Bound(0)), Type::int()],
            Bound(0),
        )),
        "arraySet" => a1(Type::func(
            vec![Type::array(Bound(0)), Type::int(), Bound(0)],
            Type::array(Bound(0)),
        )),
        "arrayPush" => a1(Type::func(
            vec![Type::array(Bound(0)), Bound(0)],
            Type::array(Bound(0)),
        )),
        "arrayPop" => a1(Type::func(
            vec![Type::array(Bound(0))],
            Type::array(Bound(0)),
        )),
        "arraySlice" => a1(Type::func(
            vec![Type::array(Bound(0)), Type::int(), Type::int()],
            Type::array(Bound(0)),
        )),
        "arrayConcat" => a1(Type::func(
            vec![Type::array(Bound(0)), Type::array(Bound(0))],
            Type::array(Bound(0)),
        )),
        // --- bitwise `Int` ops ---
        "shl" | "shr" | "ushr" | "bitAnd" | "bitOr" | "bitXor" => {
            Scheme::mono(Type::func(vec![Type::int(), Type::int()], Type::int()))
        }
        "bitNot" | "popCount" => Scheme::mono(Type::func(vec![Type::int()], Type::int())),
        // --- bytes ---
        "stringToBytes" => Scheme::mono(Type::func(vec![Type::string()], Type::array(Type::int()))),
        "bytesToString" => Scheme::mono(Type::func(vec![Type::array(Type::int())], Type::string())),
        "bytesToHex" => Scheme::mono(Type::func(vec![Type::array(Type::int())], Type::string())),
        "show" => a1(Type::func(vec![Bound(0)], Type::string())),
        "charCode" => Scheme::mono(Type::func(vec![Type::char()], Type::int())),
        "charFromCode" => Scheme::mono(Type::func(vec![Type::int()], Type::char())),
        "stringToChars" => {
            Scheme::mono(Type::func(vec![Type::string()], Type::array(Type::char())))
        }
        "charsToString" => {
            Scheme::mono(Type::func(vec![Type::array(Type::char())], Type::string()))
        }
        "bytesFromHex" => Scheme::mono(Type::func(
            vec![Type::string()],
            Type::Con(
                InternedString::from("Maybe"),
                vec![Type::array(Type::int())],
            ),
        )),
        "==" | "!=" => Scheme {
            quant: vec![VarKind::Type],
            ty: Type::func(vec![Bound(0), Bound(0)], Type::bool()),
        },
        // `∀a e. a -> () ! { io | e }`
        // --- the mutable cell ---
        //
        // Every one of these carries `{ Mut | e }`, which is what makes mutation
        // visible in a caller's type and what stops `def r = newRef []` being
        // generalized: the binding's right-hand side is no longer pure, and the
        // effect-based value restriction refuses to quantify it.
        //
        // `Mut` is its own label rather than part of `io`. Rows are for telling
        // effects apart, and "this touches memory" is not "this touches the
        // outside world" — a caller can reasonably care about one and not the
        // other.
        "newRef" => Scheme {
            quant: vec![VarKind::Type, VarKind::Effect],
            ty: Type::func_eff(vec![Bound(0)], Type::reference(Bound(0)), mut_row(1)),
        },
        "getRef" => Scheme {
            quant: vec![VarKind::Type, VarKind::Effect],
            ty: Type::func_eff(vec![Type::reference(Bound(0))], Bound(0), mut_row(1)),
        },
        "setRef" => Scheme {
            quant: vec![VarKind::Type, VarKind::Effect],
            ty: Type::func_eff(
                vec![Type::reference(Bound(0)), Bound(0)],
                Type::unit(),
                mut_row(1),
            ),
        },
        "print" | "println" => Scheme {
            quant: vec![VarKind::Type, VarKind::Effect],
            ty: Type::func_eff(
                vec![Bound(0)],
                Type::unit(),
                Type::RowExtend(
                    InternedString::from("io"),
                    Box::new(Type::Tuple(vec![])),
                    Box::new(Bound(1)),
                ),
            ),
        },
        _ => return None,
    };
    Some(s)
}

// ===========================================================================
// Pretty printing
// ===========================================================================

use std::collections::HashSet;

/// A scheme without its `forall` -- how a candidate reads in a list of them,
/// where the quantifiers are noise.
fn show_scheme_body(s: &Scheme) -> String {
    let full = s.to_string();
    match full.strip_prefix("forall") {
        Some(rest) => rest
            .split_once(". ")
            .map_or(full.clone(), |(_, body)| body.to_string()),
        None => full,
    }
}

fn show(ty: &Type) -> String {
    let mut namer = Namer::default();
    let mut s = String::new();
    let _ = write_type(&mut s, ty, &mut namer, Prec::Top, &HashSet::new());
    s
}

/// `Bound` indices (of effect kind) that a `Scheme` should NOT surface: a lone
/// effect variable that occurs once and isn't the tail of a labelled row is pure
/// noise, so `∀a e. a -> a ! e` prints as `∀a. a -> a`.
fn hidden_effect_vars(scheme: &Scheme) -> HashSet<u32> {
    let mut count: HashMap<u32, u32> = HashMap::new();
    let mut labelled_tail: HashSet<u32> = HashSet::new();
    fn walk(ty: &Type, count: &mut HashMap<u32, u32>, tails: &mut HashSet<u32>) {
        match ty {
            Type::Bound(i) => *count.entry(*i).or_default() += 1,
            Type::Var(_) | Type::RowEmpty | Type::Error => {}
            Type::Con(_, args) | Type::Tuple(args) => {
                args.iter().for_each(|a| walk(a, count, tails))
            }
            Type::Fun(args, ret, eff) => {
                args.iter().for_each(|a| walk(a, count, tails));
                walk(ret, count, tails);
                walk(eff, count, tails);
            }
            Type::Record(row) => walk(row, count, tails),
            Type::RowExtend(_, field, rest) => {
                walk(field, count, tails);
                // if this row has a label and its tail is a Bound var, that var is
                // "visible" (it prints as `{ … | e }`)
                if let Type::Bound(i) = &**rest {
                    tails.insert(*i);
                }
                walk(rest, count, tails);
            }
        }
    }
    walk(&scheme.ty, &mut count, &mut labelled_tail);
    (0..scheme.quant.len() as u32)
        .filter(|&i| scheme.quant[i as usize] == VarKind::Effect)
        .filter(|i| count.get(i).copied().unwrap_or(0) < 2 && !labelled_tail.contains(i))
        .collect()
}

#[derive(Default)]
struct Namer {
    names: HashMap<u32, String>,
    next: u32,
    /// The enclosing scheme's `quant` kinds, so a `Bound(i)` of row/effect kind
    /// prints as `r` / `e` rather than a plain type-variable letter.
    bound_kinds: Vec<VarKind>,
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

    /// Name a bound variable of a scheme by its kind: type vars get `a`, `b`, `c`
    /// …; record-row vars `r`, `r1`, `r2` …; effect-row vars `e`, `e1`, `e2` ….
    fn bound_name(&self, i: u32) -> String {
        let kind = self
            .bound_kinds
            .get(i as usize)
            .copied()
            .unwrap_or(VarKind::Type);
        // Type-ish vars (`Type` / `Num`) share one `a, b, c…` sequence; rows and
        // effects each get their own.
        let same = |k: VarKind| match kind {
            VarKind::Row => k == VarKind::Row,
            VarKind::Effect => k == VarKind::Effect,
            _ => k != VarKind::Row && k != VarKind::Effect,
        };
        let rank = self
            .bound_kinds
            .iter()
            .take(i as usize)
            .filter(|&&k| same(k))
            .count();
        kinded_var_name(kind, rank as u32)
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

/// A display name for the `rank`-th variable of a given kind. Multiple rows /
/// effects are disambiguated with a numeric suffix (`r`, `r1`, `r2`; `e`, `e1`).
fn kinded_var_name(kind: VarKind, rank: u32) -> String {
    let base = match kind {
        VarKind::Row => 'r',
        VarKind::Effect => 'e',
        _ => return var_name(rank),
    };
    if rank == 0 {
        base.to_string()
    } else {
        format!("{base}{rank}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Top,
    Arrow,
    App,
}

fn write_type(
    out: &mut impl fmt::Write,
    ty: &Type,
    namer: &mut Namer,
    prec: Prec,
    hidden: &HashSet<u32>,
) -> fmt::Result {
    match ty {
        Type::Var(id) => write!(out, "{}", namer.name(*id)),
        Type::Bound(i) => write!(out, "{}", namer.bound_name(*i)),
        Type::Error => out.write_str("{error}"),
        // The unit type is written `()` — the same way its one value is, and
        // the same way an empty parameter list is.
        Type::Con(name, args) if args.is_empty() && &**name == "Unit" => out.write_str("()"),
        Type::Con(name, args) if args.is_empty() => write!(out, "{name}"),
        // `[T]` now prints the RRB `Vector`; `List T` prints as a plain application.
        Type::Con(name, args) if &**name == "Vector" && args.len() == 1 => {
            out.write_char('[')?;
            write_type(out, &args[0], namer, Prec::Top, hidden)?;
            out.write_char(']')
        }
        Type::Con(name, args) if &**name == "Array" => {
            out.write_str("#[")?;
            write_type(out, &args[0], namer, Prec::Top, hidden)?;
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
                write_type(out, a, namer, Prec::App, hidden)?;
            }
            if wrap {
                out.write_char(')')?;
            }
            Ok(())
        }
        Type::Fun(args, ret, eff) => {
            let wrap = prec >= Prec::Arrow;
            if wrap {
                out.write_char('(')?;
            }
            if args.len() == 1 {
                write_type(out, &args[0], namer, Prec::Arrow, hidden)?;
            } else {
                out.write_char('(')?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.write_str(", ")?;
                    }
                    write_type(out, a, namer, Prec::Top, hidden)?;
                }
                out.write_char(')')?;
            }
            out.write_str(" -> ")?;
            write_type(out, ret, namer, Prec::Top, hidden)?;
            write_effect_suffix(out, eff, namer, hidden)?;
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
                write_type(out, a, namer, Prec::Top, hidden)?;
            }
            out.write_char(')')
        }
        Type::Record(row) => {
            out.write_str("{ ")?;
            write_row(out, row, namer, hidden)?;
            out.write_str(" }")
        }
        Type::RowEmpty => out.write_str("()"),
        Type::RowExtend(..) => {
            out.write_str("(| ")?;
            write_row(out, ty, namer, hidden)?;
            out.write_str(" |)")
        }
    }
}

/// Print ` ! e` after an arrow when its latent effect is non-pure. Effect rows
/// read as `io`, `State Int`, `{ io, State Int }`, `{ io | e }` — a single closed
/// label needs no braces; a lone `hidden` variable prints nothing.
fn write_effect_suffix(
    out: &mut impl fmt::Write,
    eff: &Type,
    namer: &mut Namer,
    hidden: &HashSet<u32>,
) -> fmt::Result {
    let mut labels: Vec<(InternedString, &[Type])> = Vec::new();
    let mut tail: Option<String> = None;
    let mut cur = eff;
    loop {
        match cur {
            Type::RowEmpty => break,
            Type::RowExtend(name, field, rest) => {
                let args: &[Type] = match &**field {
                    Type::Tuple(a) => a,
                    _ => &[],
                };
                labels.push((*name, args));
                cur = rest;
            }
            Type::Var(id) => {
                tail = Some(namer.name(*id));
                break;
            }
            Type::Bound(i) => {
                if labels.is_empty() && hidden.contains(i) {
                    return Ok(());
                }
                tail = Some(namer.bound_name(*i));
                break;
            }
            _ => break,
        }
    }
    if labels.is_empty() && tail.is_none() {
        return Ok(()); // pure arrow
    }
    if labels.is_empty() {
        return write!(out, " ! {}", tail.unwrap()); // `a -> b ! e`
    }
    out.write_str(" ! ")?;
    let braces = labels.len() != 1 || tail.is_some();
    if braces {
        out.write_str("{ ")?;
    }
    for (i, (name, args)) in labels.iter().enumerate() {
        if i > 0 {
            out.write_str(", ")?;
        }
        write!(out, "{name}")?;
        for a in *args {
            out.write_char(' ')?;
            write_type(out, a, namer, Prec::App, hidden)?;
        }
    }
    if let Some(t) = tail {
        out.write_char(' ')?;
        write!(out, "| {t}")?;
    }
    if braces {
        out.write_str(" }")?;
    }
    Ok(())
}

fn write_row(
    out: &mut impl fmt::Write,
    row: &Type,
    namer: &mut Namer,
    hidden: &HashSet<u32>,
) -> fmt::Result {
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
                write_type(out, field, namer, Prec::Top, hidden)?;
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
                write!(out, "| {}", namer.bound_name(*i))?;
                break;
            }
            other => {
                if !first {
                    out.write_str(", ")?;
                }
                write_type(out, other, namer, Prec::Top, hidden)?;
                break;
            }
        }
    }
    Ok(())
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut namer = Namer::default();
        write_type(f, self, &mut namer, Prec::Top, &HashSet::new())
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hidden = hidden_effect_vars(self);
        let visible: Vec<u32> = (0..self.quant.len() as u32)
            .filter(|i| !hidden.contains(i))
            .collect();
        let mut namer = Namer {
            bound_kinds: self.quant.clone(),
            ..Namer::default()
        };
        if !visible.is_empty() {
            f.write_str("forall")?;
            for i in &visible {
                write!(f, " {}", namer.bound_name(*i))?;
            }
            f.write_str(". ")?;
        }
        write_type(f, &self.ty, &mut namer, Prec::Top, &hidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_display() {
        assert_eq!(Type::int().to_string(), "Int");
        // `[T]` syntax denotes the RRB `Vector`; a plain list prints as `List T`.
        assert_eq!(Type::vector(Type::int()).to_string(), "[Int]");
        assert_eq!(Type::list(Type::int()).to_string(), "List Int");
        assert_eq!(
            Type::func(vec![Type::int(), Type::int()], Type::bool()).to_string(),
            "Int -> Int -> Bool"
        );
        assert_eq!(
            Type::Tuple(vec![Type::int(), Type::string()]).to_string(),
            "(Int, String)"
        );
        // free (unbound) variables get printed as `a`, `b`, …
        assert_eq!(
            Type::func(vec![Type::Var(3)], Type::Var(3)).to_string(),
            "a -> a"
        );
        assert_eq!(
            Type::func(vec![Type::Var(1)], Type::Var(9)).to_string(),
            "a -> b"
        );
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
        assert_eq!(
            Type::Record(Box::new(row)).to_string(),
            "{ x : Int, y : Bool | a }"
        );
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

        assert_eq!(infer.generalize_named(None, &deep).quant.len(), 1);
        assert_eq!(infer.generalize_named(None, &shallow).quant.len(), 0);
    }
}

/// Number the distinct type variables of an annotation, in order of appearance.
///
/// [`ty_of`] maps a variable to `Bound(i)` through this, so the two occurrences
/// of `a` in `(f : a -> a)` become the same `Bound` and therefore, after
/// instantiation, the same meta variable.
fn collect_tyvars(t: &hir::LTypeExpr, out: &mut HashMap<VarId, u32>) {
    match t.value() {
        hir::TypeExpr::Var(v) => {
            let next = out.len() as u32;
            out.entry(*v.value()).or_insert(next);
        }
        hir::TypeExpr::Con(_, args) => args.iter().for_each(|a| collect_tyvars(a, out)),
        hir::TypeExpr::Fun(ps, r, eff) => {
            ps.iter().for_each(|p| collect_tyvars(p, out));
            collect_tyvars(r, out);
            if let Some(row) = eff {
                for (_, args) in &row.labels {
                    args.iter().for_each(|a| collect_tyvars(a, out));
                }
                if let Some(tail) = &row.tail {
                    let next = out.len() as u32;
                    out.entry(*tail.value()).or_insert(next);
                }
            }
        }
        hir::TypeExpr::Tuple(ts) => ts.iter().for_each(|x| collect_tyvars(x, out)),
        hir::TypeExpr::Vector(x) | hir::TypeExpr::List(x) => collect_tyvars(x, out),
        hir::TypeExpr::Error => {}
    }
}

/// Renders several types with **one** variable naming.
///
/// `Type`'s `Display` starts a fresh [`Namer`] each time, so two independent
/// type variables both come out as `a`. That is fine for one type on its own
/// and misleading for several shown together: `fun snd (a, b) = b` would have
/// its parameters hinted `(a : a)` and `(b : a)`, which says they are the same
/// type when nothing has said so.
///
/// Sharing the namer across a declaration's types fixes that — the same
/// variable gets the same letter, and different ones do not.
#[derive(Default)]
pub struct Renderer {
    namer: Namer,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer::default()
    }

    /// A function's *result*, as it would read after the last arrow: the return
    /// type, then the arrow's latent effect if it has one.
    ///
    /// Needed because an effect is a property of the arrow, not of the type it
    /// returns. Rendering the body's type alone loses it, and a result that
    /// silently drops `! { io | e }` is worse than no annotation at all —
    /// it reads as a claim that the function is pure.
    pub fn render_result(&mut self, ret: &Type, eff: &Type) -> String {
        let mut out = self.render(ret);
        let _ = write_effect_suffix(&mut out, eff, &mut self.namer, &HashSet::new());
        out
    }

    pub fn render(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        // Writing into a `String` cannot fail.
        let _ = Wrapper(&mut out).write_type(ty, &mut self.namer);
        out
    }
}

/// A type with every row written in one canonical order.
///
/// A row is a *set* of labels: `{ io, Mut | e }` and `{ Mut, io | e }` are the
/// same effect, and inference is free to build either. Anything that compares
/// two types structurally — a rename check, the core type checker, the
/// matcher below — has to put them in the same order first or it will reject
/// a type for agreeing with itself.
pub fn normalize(ty: &Type) -> Type {
    match ty {
        Type::Var(_) | Type::Bound(_) | Type::RowEmpty | Type::Error => ty.clone(),
        Type::Con(n, args) => Type::Con(*n, args.iter().map(normalize).collect()),
        Type::Fun(args, ret, eff) => Type::Fun(
            args.iter().map(normalize).collect(),
            Box::new(normalize(ret)),
            Box::new(normalize(eff)),
        ),
        Type::Tuple(items) => Type::Tuple(items.iter().map(normalize).collect()),
        Type::Record(row) => Type::Record(Box::new(normalize(row))),
        Type::RowExtend(..) => {
            let (mut labels, tail) = row_parts(ty);
            // By name, and stable — a row may legitimately repeat a label
            // (`{ Test, Test | e }` is what two nested handlers produce), and
            // the repeats have to stay next to each other.
            labels.sort_by(|(a, _), (b, _)| a.to_string().cmp(&b.to_string()));
            labels.into_iter().rev().fold(
                tail.as_ref().map_or(Type::RowEmpty, normalize),
                |acc, (l, f)| Type::RowExtend(l, Box::new(normalize(&f)), Box::new(acc)),
            )
        }
    }
}

/// Are these the same type, rows aside from their order?
pub fn equal(a: &Type, b: &Type) -> bool {
    normalize(a) == normalize(b)
}

/// The type arguments that instantiate `scheme` to `concrete`, in quantifier
/// order — or `None` if `concrete` is not an instance of it.
///
/// One-way matching, not unification: `concrete` came from a use site that
/// inference already checked, so it *is* a substitution instance, and this
/// only has to read the substitution back off. Lowering needs it because
/// instantiation happens by replacing quantifiers with fresh variables and
/// then solving them — the arguments are never written down anywhere, and
/// core has to write them down.
pub fn match_scheme(scheme: &Scheme, concrete: &Type) -> Option<Vec<Type>> {
    let mut out: HashMap<u32, Type> = HashMap::new();
    if !match_ty(&scheme.ty, concrete, &mut out) {
        return None;
    }
    // Some quantifiers can go unmentioned — a phantom row variable, or one that
    // only appears under a part the match skipped. They are unconstrained, so
    // any type will do, and the unit type is the least surprising one.
    Some(
        (0..scheme.quant.len() as u32)
            .map(|i| match (out.get(&i), scheme.quant[i as usize]) {
                (Some(t), _) => t.clone(),
                (None, VarKind::Row) | (None, VarKind::Effect) => Type::RowEmpty,
                (None, _) => Type::unit(),
            })
            .collect(),
    )
}

fn match_ty(pat: &Type, conc: &Type, out: &mut HashMap<u32, Type>) -> bool {
    match (pat, conc) {
        // The one interesting case: a quantifier binds, or has to agree with
        // what it bound earlier.
        (Type::Bound(i), _) => match out.get(i) {
            // Up to row order: the same variable can be matched against the
            // same effect row written two different ways.
            Some(prev) => equal(prev, conc),
            None => {
                out.insert(*i, normalize(conc));
                true
            }
        },
        (Type::Var(a), Type::Var(b)) => a == b,
        (Type::RowEmpty, Type::RowEmpty) => true,
        (Type::Con(n1, a1), Type::Con(n2, a2)) => {
            n1 == n2
                && a1.len() == a2.len()
                && a1.iter().zip(a2).all(|(x, y)| match_ty(x, y, out))
        }
        (Type::Fun(p1, r1, e1), Type::Fun(p2, r2, e2)) => {
            p1.len() == p2.len()
                && p1.iter().zip(p2).all(|(x, y)| match_ty(x, y, out))
                && match_ty(r1, r2, out)
                && match_ty(e1, e2, out)
        }
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| match_ty(x, y, out))
        }
        (Type::Record(a), Type::Record(b)) => match_ty(a, b, out),
        // A row is a set, not a list: the same labels can be written in either
        // order, so find each of the pattern's labels in the concrete row and
        // match what is left over against the pattern's tail.
        (Type::RowExtend(..), _) => match_row(pat, conc, out),
        _ => false,
    }
}

/// Match a row pattern against a concrete row, label by label.
fn match_row(pat: &Type, conc: &Type, out: &mut HashMap<u32, Type>) -> bool {
    let (mut want, ptail) = row_parts(pat);
    let (mut have, ctail) = row_parts(conc);
    want.sort_by_key(|(l, _)| l.to_string());
    have.sort_by_key(|(l, _)| l.to_string());

    let mut leftover: Vec<(InternedString, Type)> = Vec::new();
    for (label, field) in have {
        match want.iter().position(|(l, _)| *l == label) {
            Some(i) => {
                let (_, pf) = want.remove(i);
                if !match_ty(&pf, &field, out) {
                    return false;
                }
            }
            None => leftover.push((label, field)),
        }
    }
    // Labels the pattern demanded and the row does not have.
    if !want.is_empty() {
        return false;
    }
    let rest = leftover
        .into_iter()
        .rev()
        .fold(ctail.unwrap_or(Type::RowEmpty), |acc, (l, f)| {
            Type::RowExtend(l, Box::new(f), Box::new(acc))
        });
    match ptail {
        Some(t) => match_ty(&t, &rest, out),
        None => rest == Type::RowEmpty,
    }
}

/// A row as its labels and its tail (`None` when the row is closed).
fn row_parts(mut ty: &Type) -> (Vec<(InternedString, Type)>, Option<Type>) {
    let mut labels = Vec::new();
    loop {
        match ty {
            Type::RowExtend(l, f, rest) => {
                labels.push((*l, (**f).clone()));
                ty = rest;
            }
            Type::RowEmpty => return (labels, None),
            other => return (labels, Some(other.clone())),
        }
    }
}

/// Bridges `write_type`, which wants a `fmt::Formatter`, to a `String`.
struct Wrapper<'a>(&'a mut String);

impl Wrapper<'_> {
    fn write_type(&mut self, ty: &Type, namer: &mut Namer) -> fmt::Result {
        struct Show<'a>(&'a Type, std::cell::RefCell<&'a mut Namer>);
        impl fmt::Display for Show<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write_type(f, self.0, &mut self.1.borrow_mut(), Prec::Top, &HashSet::new())
            }
        }
        use fmt::Write;
        write!(self.0, "{}", Show(ty, std::cell::RefCell::new(namer)))
    }
}
