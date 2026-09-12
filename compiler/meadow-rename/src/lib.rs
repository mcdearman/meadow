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
use meadow_hir::{self as hir, NodeIdGen, VarIdGen, PRIMS, VarId};
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
    predeclared: HashMap<(Vec<InternedString>, InternedString), VarId>,
    /// Type constructors in scope: name -> arity. Seeded with the builtins.
    tycons: HashMap<InternedString, usize>,
    /// Every constructor known here, keyed by its **canonical** name --
    /// `Type.Ctor`, the one thing about it that is unique across a program.
    ///
    /// Keyed this way rather than by the bare name so that two types may each
    /// have a `Leaf`. Everything downstream -- `ctor_fields`, the back end's
    /// tag table, exhaustiveness -- inherits that uniqueness for free, because
    /// what they receive is this name.
    ctors: HashMap<InternedString, CtorInfo>,
    /// Bare name -> canonical, for the constructors this module may write
    /// unqualified: the ones its own types declare, plus whatever a `use` of a
    /// type brought in. A name absent here must be written `Type.Ctor`.
    ///
    /// Only the *base* layer lives here once the scope is sealed -- the
    /// builtins and what the dependencies flattened -- and a module's own and
    /// `use`d constructors go in [`Resolver::module_ctors`] instead, where one
    /// spelling may name several.
    visible_ctors: HashMap<InternedString, InternedString>,
    /// Bare name -> every constructor of that spelling the current module
    /// declares or explicitly imports. More than one is an overload, which
    /// inference settles by type.
    module_ctors: HashMap<InternedString, Vec<InternedString>>,
    /// Whether [`Resolver::seal_base_scope`] has run: from then on, what comes
    /// into scope is a module's own, and may overload rather than shadow.
    sealed: bool,
    /// Scope length once a module's own items and its `use`s are in: what is
    /// below is the module layer (down to `base_scope`), what is above is local.
    module_scope: usize,
    /// Names written bare that have more than one meaning here -- see
    /// [`hir::Overloads`].
    overloads: hir::Overloads,
    /// The module a dependency's value was imported from, for naming the
    /// candidates of an overload.
    origins: HashMap<VarId, InternedString>,
    /// Where each top-level name of each module was first declared, so that a
    /// second declaration can point at the first.
    decl_spans: HashMap<(Vec<InternedString>, InternedString), Span>,
    /// Where a second declaration of a name was written. Such a declaration is
    /// dropped once reported -- see [`Resolver::resolve_bind`].
    duplicates: std::collections::HashSet<Span>,
    /// Type -> its constructors, bare names, in declaration order. What `use`
    /// of a type consults, and what an exhaustiveness message would list.
    ctors_of: HashMap<InternedString, Vec<InternedString>>,
    /// Declared effects: name -> parameter count.
    effects: HashMap<InternedString, usize>,
    /// Operation name -> the effect it belongs to.
    effect_ops: HashMap<InternedString, InternedString>,
    /// Operation `VarId` -> `(effect, operation)`. Keyed by id, not name: an
    /// ordinary value can share an operation's name and shadow it in scope —
    /// `Std.State.get` against the prelude's `Vector.get`, say — and the lowerer
    /// has to tell a `perform` from a variable by identity, not spelling.
    effect_op_ids: HashMap<VarId, (InternedString, InternedString)>,
    /// What each module of this unit declares, by dotted path.
    ///
    /// A package is one compilation unit — its modules may be mutually
    /// recursive and share a `NodeId` space — but each module is its own
    /// *namespace*. It sees the prims, whatever the prelude flattened, its own
    /// items, and whatever it `use`s; never a sibling's names for free. Rust
    /// draws the line in the same place, and it is what gives `use Pack.Mod`
    /// something to name.
    frames: HashMap<Vec<InternedString>, ModuleFrame>,
    /// What every module of the unit starts from: the builtins plus whatever
    /// the dependencies brought in, snapshotted by [`Resolver::seal_base_scope`].
    /// A module's own declarations are laid on top of this and nothing else.
    base_tycons: HashMap<InternedString, usize>,
    base_ctors: HashMap<InternedString, InternedString>,
    base_effects: HashMap<InternedString, usize>,
    base_effect_ops: HashMap<InternedString, InternedString>,
    /// The module being declared into, or resolved.
    current: Vec<InternedString>,
    /// Scope length before any module's own items: the prims and the prelude,
    /// which every module in the unit sees. `enter_module` truncates to this.
    base_scope: usize,
    /// Type variables of the declaration currently being resolved: a `data` /
    /// `record` / `effect` parameter list, or the ones a pattern annotation
    /// introduced.
    tyvars: Vec<(InternedString, VarId)>,
    /// While resolving a pattern annotation, an unknown type variable is bound
    /// rather than reported.
    ///
    /// `(x : a)` has no parameter list to have declared `a` in, so the
    /// alternative to binding it here is refusing to let anyone write it. It is
    /// scoped to the enclosing declaration, which is what makes the two `a`s in
    /// `fun twice (f : a -> a) (x : a)` the same variable.
    open_tyvars: bool,
    ids: NodeIdGen,
    /// This unit's `VarId`s. Seeded by the driver from a base that clears the
    /// unit's dependencies -- see [`meadow_hir::VarIdGen`].
    vars: VarIdGen,
    /// True only while resolving a module-level `Decl` (not nested in an expr).
    toplevel: bool,
    /// The visibility of the declaration being declared, so that the places
    /// that record a name in its module's frame do not each have to be handed
    /// one. Set by [`Resolver::declare_types`] / [`Resolver::declare_toplevel`].
    vis: Vis,
    /// Export bookkeeping. If `any_vis` stays false — no visibility attribute
    /// anywhere in the unit — everything is exported, which is what a script,
    /// a REPL line and an unannotated package all want. Otherwise only
    /// `@pub(pack)` declarations are, plus `@pub(pack) use` re-exports.
    any_vis: bool,
    pub_vars: std::collections::HashSet<VarId>,
    pub_types: std::collections::HashSet<InternedString>,
    /// References the HIR will not carry — see [`RefSite`]. Collected per
    /// module and taken by the driver after each one is resolved.
    extra_refs: Vec<RefSite>,
    /// `@test` functions, in declaration order — `meadow test` runs these.
    test_vars: Vec<(InternedString, VarId)>,
    /// Active module qualifiers: `Foo` -> its exported value names. Populated by the
    /// driver from this module's `mod` children and `use`d modules; consulted when
    /// resolving `Foo.name`.
    qualifiers: HashMap<InternedString, HashMap<InternedString, VarId>>,
    errors: Vec<Diagnostic>,
}

/// Constructors that are always available (see `infer::ctor_type` / `core`). The
/// `Std` prelude also declares `data List` / `data Bool` with these variants, so a
/// re-declaration of one of these names is tolerated rather than an error.
/// Constructors the *language* depends on, always in scope unqualified.
///
/// `if` needs `True` / `False`, and `[a; b]` and `::` desugar to `Cons` / `Nil`
/// — so requiring `use Std.Bool (Bool)` before an `if` would be absurd. Every
/// other constructor follows the ordinary rule: qualified, or brought in by a
/// `use` of its type.
const BUILTIN_CTORS: &[&str] = &["Nil", "Cons", "True", "False"];

/// The same, paired with the type that owns them, to seed the scope.
const BUILTIN_CTOR_OWNERS: &[(&str, &str)] =
    &[("List", "Nil"), ("List", "Cons"), ("Bool", "True"), ("Bool", "False")];

/// Type constructors seeded into every resolver. Like [`BUILTIN_CTORS`], the
/// prelude is allowed to (re-)declare `List` / `Bool` without it counting as a
/// duplicate-definition error.
const BUILTIN_TYCONS: &[&str] =
    &["Int", "BigInt", "Float", "String", "Char", "Bool", "Unit", "List", "Array", "Ref"];

/// Split a declaration into its attributes and the bare declaration underneath.
/// The parser only ever nests one `Attributed` layer.
fn peel(d: &ast::LDecl) -> (&[ast::Attr], &ast::LDecl) {
    match d.value() {
        ast::Decl::Attributed(attrs, inner) => (attrs, inner),
        _ => (&[], d),
    }
}

/// How far out of its own module a declaration can be seen.
///
/// Rust's arrangement, with the package in the place of the crate:
///
/// | written            | seen by                                   |
/// |--------------------|-------------------------------------------|
/// | nothing            | its own module and the modules inside it  |
/// | `@pub(super)`      | ...and its parent's subtree               |
/// | `@pub`             | ...and every module of the package        |
/// | `@pub(pack)`       | ...and whoever depends on the package     |
///
/// So `@pub` is about leaving the *module*, and leaving the *package* is a
/// separate, louder thing to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Vis {
    Private,
    Super,
    Package,
    Exported,
}

impl Vis {
    /// Can a declaration in `owner` with this visibility be named from `from`?
    ///
    /// "Inside" is by path prefix: `a.b.c` is inside `a.b`, so a private
    /// declaration is visible to its own module's descendants, and
    /// `@pub(super)` reaches the parent and everything under it.
    fn reaches(self, owner: &[InternedString], from: &[InternedString]) -> bool {
        match self {
            Vis::Exported | Vis::Package => true,
            Vis::Super => {
                let parent = owner.split_last().map(|(_, p)| p).unwrap_or(&[]);
                from.starts_with(parent)
            }
            Vis::Private => from.starts_with(owner),
        }
    }

    /// How to say, in a sentence about a module, what this keeps a name to.
    fn describe_in(self) -> &'static str {
        match self {
            Vis::Private => "private to module",
            Vis::Super => "visible only to the parent of module",
            Vis::Package => "visible only within the package of module",
            Vis::Exported => "public in module",
        }
    }

    /// The attribute that would let one more layer of the program see it.
    fn wider(self) -> &'static str {
        match self {
            Vis::Private => "@pub",
            Vis::Super => "@pub",
            Vis::Package | Vis::Exported => "@pub(pack)",
        }
    }
}

/// A module path as it is written: `Collections.List`, or `the root module`
/// when there is nothing to write.
fn dotted_path(path: &[InternedString]) -> String {
    if path.is_empty() {
        "the root module".to_string()
    } else {
        path.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(".")
    }
}

/// The visibility a declaration's attributes ask for, and the span of the
/// `@pub(…)` argument when it is one this does not understand.
fn vis_of(attrs: &[ast::Attr]) -> (Vis, Option<(InternedString, Span)>) {
    let Some(a) = attrs.iter().find(|a| &**a.name.value() == "pub") else {
        return (Vis::Private, None);
    };
    match a.args.first() {
        None => (Vis::Package, None),
        Some(arg) => match &**arg.value() {
            "pack" => (Vis::Exported, None),
            "super" => (Vis::Super, None),
            _ => (Vis::Package, Some((*arg.value(), arg.span))),
        },
    }
}

fn has_test(attrs: &[ast::Attr]) -> bool {
    attrs.iter().any(|a| &**a.name.value() == "test")
}

/// A name is a constructor iff it starts with an uppercase letter.
fn is_ctor_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

/// The type constructors every module sees without asking.
fn builtin_tycons() -> HashMap<InternedString, usize> {
    [
        ("Int", 0), ("BigInt", 0), ("Float", 0), ("String", 0), ("Bool", 0),
        ("Unit", 0), ("List", 1), ("Array", 1), ("Ref", 1),
    ]
    .into_iter()
    .map(|(n, a)| (InternedString::from(n), a))
    .collect()
}

/// The data constructors every module sees without asking — see
/// [`BUILTIN_CTOR_OWNERS`].
fn builtin_ctors() -> HashMap<InternedString, InternedString> {
    BUILTIN_CTOR_OWNERS
        .iter()
        .map(|(ty, c)| {
            let c = InternedString::from(*c);
            (c, canonical_ctor(InternedString::from(*ty), c))
        })
        .collect()
}

/// A constructor's canonical name: `Type.Ctor`.
///
/// Type names are unique across a program, so this is too -- which is what
/// lets `Vector.Leaf` and `Tree.Leaf` both exist. Every pass after the
/// resolver sees only this form; the bare name is a *scoping* convenience that
/// stops here.
fn canonical_ctor(ty: InternedString, ctor: InternedString) -> InternedString {
    InternedString::from(format!("{ty}.{ctor}"))
}

/// The bare spelling of a canonical name: `Expr.Int` -> `Int`.
fn bare_ctor(canonical: InternedString) -> InternedString {
    match canonical.rsplit_once('.') {
        Some((_, c)) => InternedString::from(c),
        None => canonical,
    }
}

/// A name written in the source that the HIR does not keep.
///
/// Most references survive resolution: a variable becomes a `VarId` in a
/// `hir::Expr::Var`, a type becomes a `hir::TypeExpr::Con`. Two do not — the
/// names listed in a `use`, and the qualifier of a `Expr.Ctor` — because
/// neither means anything after resolution. An editor still has to know they
/// are references to the same thing, or a rename would leave them behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRef {
    Value(VarId),
    Type(InternedString),
    /// A data constructor, by its canonical `Type.Ctor` name.
    Ctor(InternedString),
}

/// Where a [`NameRef`] was written.
#[derive(Debug, Clone, Copy)]
pub struct RefSite {
    pub span: Span,
    pub what: NameRef,
}

/// One module's own declarations, kept apart from every other module's.
#[derive(Debug, Default, Clone)]
struct ModuleFrame {
    /// Top-level bindings, in declaration order.
    values: Vec<(InternedString, VarId, Vis)>,
    /// Type constructors this module declares: name -> arity.
    tycons: Vec<(InternedString, usize, Vis)>,
    /// Data constructors this module declares: bare spelling -> canonical.
    /// A constructor is as visible as the type that owns it.
    ctors: Vec<(InternedString, InternedString, Vis)>,
    /// Effects declared here: name -> parameter count.
    effects: Vec<(InternedString, usize, Vis)>,
    /// Effect operations declared here: operation -> its effect.
    effect_ops: Vec<(InternedString, InternedString, Vis)>,
}

impl Resolver {
    pub fn new(filename: impl Into<String>, var_base: u32) -> Self {
        let tycons = builtin_tycons();
        Resolver {
            filename: filename.into(),
            scope: Vec::new(),
            names: HashMap::new(),
            predeclared: HashMap::new(),
            tycons,
            ctors: HashMap::new(),
            visible_ctors: builtin_ctors(),
            module_ctors: HashMap::new(),
            sealed: false,
            module_scope: 0,
            overloads: HashMap::new(),
            origins: HashMap::new(),
            decl_spans: HashMap::new(),
            duplicates: std::collections::HashSet::new(),
            ctors_of: HashMap::new(),
            effects: HashMap::new(),
            effect_ops: HashMap::new(),
            effect_op_ids: HashMap::new(),
            frames: HashMap::new(),
            base_tycons: HashMap::new(),
            base_ctors: HashMap::new(),
            base_effects: HashMap::new(),
            base_effect_ops: HashMap::new(),
            current: Vec::new(),
            base_scope: 0,
            tyvars: Vec::new(),
            open_tyvars: false,
            ids: NodeIdGen::new(),
            vars: VarIdGen::starting_at(var_base),
            toplevel: false,
            vis: Vis::Private,
            any_vis: false,
            pub_vars: std::collections::HashSet::new(),
            pub_types: std::collections::HashSet::new(),
            extra_refs: Vec::new(),
            test_vars: Vec::new(),
            qualifiers: HashMap::new(),
            errors: Vec::new(),
        }
    }

    // --- modules ----------------------------------------------------------

    /// Name the file diagnostics from here on point into.
    ///
    /// A unit is resolved as a whole but its modules are separate files, and an
    /// error has to say which one it is in — so the driver moves this along as
    /// it goes.
    pub fn set_filename(&mut self, filename: impl Into<String>) {
        self.filename = filename.into();
    }

    /// Declarations from here on belong to `path`.
    pub fn set_module(&mut self, path: &[InternedString]) {
        self.current = path.to_vec();
        self.frames.entry(self.current.clone()).or_default();
    }

    /// Everything visible to every module — the prims and the prelude — has
    /// been imported; whatever follows is one module's own.
    pub fn seal_base_scope(&mut self) {
        self.base_scope = self.scope.len();
        self.base_tycons = self.tycons.clone();
        self.base_ctors = self.visible_ctors.clone();
        self.base_effects = self.effects.clone();
        self.base_effect_ops = self.effect_ops.clone();
        self.sealed = true;
    }

    /// Begin resolving `path`: reset to the shared base, then lay this
    /// module's own declarations on top. A sibling's names are *not* here;
    /// `use` puts them there.
    pub fn enter_module(&mut self, path: &[InternedString]) {
        self.current = path.to_vec();
        self.scope.truncate(self.base_scope);
        self.qualifiers.clear();
        self.tycons = self.base_tycons.clone();
        self.visible_ctors = self.base_ctors.clone();
        self.module_ctors.clear();
        self.effects = self.base_effects.clone();
        self.effect_ops = self.base_effect_ops.clone();
        let frame = self.frames.get(path).cloned().unwrap_or_default();
        self.admit(&frame, None);
    }

    /// Make a frame's declarations visible in the current module.
    ///
    /// `owner` is the module the frame belongs to: `None` for the current
    /// module's own frame, where visibility does not apply — a declaration is
    /// always visible where it was written.
    fn admit(&mut self, frame: &ModuleFrame, owner: Option<&[InternedString]>) {
        // Copied out first: what follows mutates `self`, and the test only
        // needs where we are and whether the unit talks about visibility.
        let here = self.current.clone();
        let all = !self.any_vis;
        let ok = |vis: Vis| match owner {
            None => true,
            Some(o) => all || vis.reaches(o, &here),
        };
        for (n, a, v) in &frame.tycons {
            if ok(*v) {
                self.tycons.insert(*n, *a);
            }
        }
        for (bare, canonical, v) in &frame.ctors {
            if ok(*v) {
                self.add_ctor(*bare, *canonical);
            }
        }
        for (n, a, v) in &frame.effects {
            if ok(*v) {
                self.effects.insert(*n, *a);
            }
        }
        for (op, eff, v) in &frame.effect_ops {
            if ok(*v) {
                self.effect_ops.insert(*op, *eff);
            }
        }
        for (n, id, v) in &frame.values {
            if ok(*v) {
                self.scope.push((*n, *id));
            }
        }
    }

    /// Can this module name a declaration of `owner`'s with visibility `vis`?
    ///
    /// A unit that never mentions visibility has none: a package with no
    /// `@pub` anywhere is a script, a REPL line or a two-file program, and
    /// making it annotate itself to see across its own files buys nothing.
    /// It is the same rule the export surface uses -- say nothing and
    /// everything is public, say anything and only what you marked is.
    fn sees(&self, vis: Vis, owner: &[InternedString]) -> bool {
        !self.any_vis || vis.reaches(owner, &self.current)
    }

    /// Record a reference the HIR will not keep.
    pub fn note_ref(&mut self, span: Span, what: NameRef) {
        self.extra_refs.push(RefSite { span, what });
    }

    /// Take the references collected since the last call — the driver does
    /// this after each module, which is what pairs them with a file.
    pub fn take_extra_refs(&mut self) -> Vec<RefSite> {
        std::mem::take(&mut self.extra_refs)
    }

    /// Does this unit contain a module at `path`?
    pub fn has_module(&self, path: &[InternedString]) -> bool {
        self.frames.contains_key(path)
    }

    /// A sibling module's top-level values, for `use Pack.Mod` — the ones this
    /// module is allowed to see.
    pub fn module_values(&self, path: &[InternedString]) -> HashMap<InternedString, VarId> {
        self.frames
            .get(path)
            .map(|f| {
                f.values
                    .iter()
                    .filter(|(_, _, v)| self.sees(*v, path))
                    .map(|(n, id, _)| (*n, *id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Bring a sibling module's declarations into the current scope.
    ///
    /// `names` empty means all of them, which is what a bare `use Pack.Mod`
    /// asks for. A selected name may be a value, a type (whose constructors
    /// come with it) or a constructor.
    ///
    /// A declaration the asking module is not allowed to see is skipped when
    /// the whole module was asked for, and an error when it was named: saying
    /// `use M (secret)` and silently getting nothing would be reported later
    /// as `undefined variable`, which is true and unhelpful.
    pub fn use_module(&mut self, path: &[InternedString], names: &[ast::Ident]) {
        let Some(frame) = self.frames.get(path).cloned() else {
            return;
        };
        if names.is_empty() {
            self.admit(&frame, Some(path));
            return;
        }
        let here = self.current.clone();
        let all = !self.any_vis;
        for want in names {
            let name = *want.value();
            let mut found = false;
            // What the name *is* here, for the error: a name invisible in one
            // namespace may be perfectly visible in another.
            let mut hidden: Option<Vis> = None;
            let note = |vis: Vis, found: &mut bool, hidden: &mut Option<Vis>| {
                if all || vis.reaches(path, &here) {
                    *found = true;
                    true
                } else {
                    *hidden = Some(hidden.map_or(vis, |h: Vis| h.max(vis)));
                    false
                }
            };
            let mut sites: Vec<RefSite> = Vec::new();
            for (n, id, v) in &frame.values {
                if *n == name && note(*v, &mut found, &mut hidden) {
                    self.scope.push((*n, *id));
                    sites.push(RefSite { span: want.span, what: NameRef::Value(*id) });
                }
            }
            for (n, a, v) in &frame.tycons {
                if *n == name && note(*v, &mut found, &mut hidden) {
                    self.tycons.insert(*n, *a);
                    // Naming a type brings its constructors, as everywhere else.
                    self.use_type_ctors(*n);
                    sites.push(RefSite { span: want.span, what: NameRef::Type(*n) });
                }
            }
            for (n, a, v) in &frame.effects {
                if *n == name && note(*v, &mut found, &mut hidden) {
                    self.effects.insert(*n, *a);
                }
            }
            for (op, eff, v) in &frame.effect_ops {
                if *op == name && note(*v, &mut found, &mut hidden) {
                    self.effect_ops.insert(*op, *eff);
                }
            }
            for (bare, canonical, v) in &frame.ctors {
                if *bare == name && note(*v, &mut found, &mut hidden) {
                    self.add_ctor(*bare, *canonical);
                    sites.push(RefSite { span: want.span, what: NameRef::Ctor(*canonical) });
                }
            }
            self.extra_refs.extend(sites);
            if !found {
                if let Some(vis) = hidden {
                    let module = dotted_path(path);
                    self.error(
                        format!("`{name}` is {} `{module}`", vis.describe_in()),
                        format!("mark it `{}` to use it here", vis.wider()),
                        want.span,
                    );
                }
            }
        }
    }

    pub fn with_prelude(filename: impl Into<String>, var_base: u32) -> Self {
        let mut r = Resolver::new(filename, var_base);
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
        let id = self.vars.fresh();
        self.names.insert(id, name);
        self.scope.push((name, id));
        id
    }

    /// The innermost binding of `name`, whatever layer it is in. Right for an
    /// operator, which only the prelude defines, and for a `use` re-export.
    fn lookup(&self, name: InternedString) -> Option<VarId> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
    }

    /// Every meaning a bare `name` has here, in three layers.
    ///
    /// A local binding shadows everything, as lexical scope does. Below that is
    /// the module layer -- its own top-level names and whatever its `use`s
    /// brought in -- where two meanings of one spelling are not a conflict to
    /// settle by import order but an overload for inference to settle by type.
    /// Below that, the base: the prims and the prelude, which anything above
    /// shadows. Overloading against the prelude would make every module that
    /// defines its own `map` an ambiguity waiting for an untyped use.
    fn lookup_all(&self, name: InternedString) -> Vec<VarId> {
        let (base, module) = if self.sealed {
            (self.base_scope, self.module_scope.max(self.base_scope))
        } else {
            (0, 0)
        };
        let locals = &self.scope[module.min(self.scope.len())..];
        if let Some((_, id)) = locals.iter().rev().find(|(n, _)| *n == name) {
            return vec![*id];
        }
        let mut found: Vec<VarId> = Vec::new();
        for (n, id) in &self.scope[base.min(module)..module.min(self.scope.len())] {
            if *n == name && !found.contains(id) {
                found.push(*id);
            }
        }
        if !found.is_empty() {
            return found;
        }
        self.scope[..base.min(self.scope.len())]
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| vec![*id])
            .unwrap_or_default()
    }

    /// At the top level, reuse a [`declare_toplevel`] id if there is one; otherwise
    /// bind fresh. Nested definitions always bind fresh (so they can shadow).
    fn bind_defn(&mut self, name: InternedString) -> VarId {
        if self.toplevel {
            if let Some(&id) = self.predeclared.get(&(self.current.clone(), name)) {
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

    /// [`Resolver::import`], remembering the module it came from so an
    /// ambiguity involving it can say so.
    pub fn import_from(&mut self, name: InternedString, id: VarId, module: InternedString) {
        self.origins.insert(id, module);
        self.import(name, id);
    }

    /// One past the last `VarId` this unit has handed out.
    pub fn var_end(&self) -> u32 {
        self.vars.end()
    }

    /// The generator itself, to carry on with after resolution — lowering to
    /// `core` invents variables too, and they belong to the same unit.
    pub fn var_gen(&self) -> VarIdGen {
        self.vars
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

    /// Declared arity of every data / record constructor in scope. `Cons` / `Nil`
    /// come from the prelude's `data List`; the two `Bool` constructors lower to
    /// literals and never need an arity.
    pub fn ctor_arities(&self) -> HashMap<InternedString, usize> {
        self.ctors.iter().map(|(n, c)| (*n, c.arity)).collect()
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
    /// Read a declaration's visibility off its attributes and make it the one
    /// the recording machinery uses, reporting an argument that means nothing.
    fn set_decl_vis(&mut self, attrs: &[ast::Attr]) -> Vis {
        let (vis, bad) = vis_of(attrs);
        if let Some((arg, span)) = bad {
            self.error(
                format!("unknown visibility `@pub({arg})`"),
                "write `@pub`, `@pub(pack)` or `@pub(super)`".to_string(),
                span,
            );
        }
        if vis != Vis::Private {
            self.any_vis = true;
        }
        self.vis = vis;
        vis
    }

    pub fn declare_toplevel(&mut self, decls: &[ast::LDecl]) {
        for d in decls {
            let (attrs, base) = peel(d);
            self.set_decl_vis(attrs);
            if let ast::Decl::Bind(b) = base.value() {
                match b {
                    ast::Bind::Fun(name, ..) => {
                        self.predeclare_unique(name);
                    }
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
            let (attrs, base) = peel(d);
            self.set_decl_vis(attrs);
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
                        self.declare_ctor(
                            *dd.name.value(),
                            *v.name.value(),
                            arity,
                            field_names,
                            v.name.span,
                        );
                    }
                }
                ast::Decl::Record(rd) => {
                    self.declare_tycon(*rd.name.value(), rd.params.len(), rd.name.span);
                    self.check_dup_fields(&rd.fields);
                    let fields = rd.fields.iter().map(|f| *f.name.value()).collect();
                    // A record's one constructor shares the type's name: `Person.Person`.
                    self.declare_ctor(
                        *rd.name.value(),
                        *rd.name.value(),
                        rd.fields.len(),
                        Some(fields),
                        rd.name.span,
                    );
                }
                ast::Decl::Effect(ed) => {
                    let name = *ed.name.value();
                    // an effect name is also a type constructor of its parameters
                    self.declare_tycon(name, ed.params.len(), ed.name.span);
                    self.effects.insert(name, ed.params.len());
                    let vis = self.vis;
                    self.frame().effects.push((name, ed.params.len(), vis));
                    for op_field in &ed.ops {
                        let op = *op_field.name.value();
                        let already = self.predeclared.contains_key(&(self.current.clone(), op));
                        if self.effect_ops.insert(op, name).is_some() || already {
                            self.error(
                                format!("operation `{op}` is already defined"),
                                "duplicate operation".to_string(),
                                ed.name.span,
                            );
                        }
                        // ops are top-level values (functions) — predeclare them
                        let id = self.predeclare(op);
                        self.decl_spans
                            .entry((self.current.clone(), op))
                            .or_insert(op_field.name.span);
                        self.frame().effect_ops.push((op, name, vis));
                        self.effect_op_ids.insert(id, (name, op));
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
                        // `v.name` arrives canonical (the resolver that built
                        // this HIR made it so), and `ctors_of` wants the bare
                        // spelling a `use` would let someone write.
                        self.ctors.insert(v.name, CtorInfo { arity, field_names });
                        self.ctors_of.entry(dd.name).or_default().push(bare_ctor(v.name));
                    }
                }
                hir::Decl::Record(rd) => {
                    self.tycons.insert(rd.name, rd.params.len());
                    self.ctors.insert(
                        rd.ctor,
                        CtorInfo {
                            arity: rd.fields.len(),
                            field_names: Some(rd.fields.iter().map(|(n, _)| *n).collect()),
                        },
                    );
                    self.ctors_of.entry(rd.name).or_default().push(rd.name);
                }
                hir::Decl::Effect(ed) => {
                    self.tycons.insert(ed.name, ed.params.len());
                    self.effects.insert(ed.name, ed.params.len());
                    for (opname, op, _) in &ed.ops {
                        self.effect_ops.insert(*opname, ed.name);
                        self.effect_op_ids.insert(*op.value(), (ed.name, *opname));
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
        self.effect_op_ids
            .iter()
            .map(|(&id, &(eff, op))| (id, eff, op))
            .collect()
    }

    fn frame(&mut self) -> &mut ModuleFrame {
        self.frames.entry(self.current.clone()).or_default()
    }

    fn declare_tycon(&mut self, name: InternedString, arity: usize, span: Span) {
        // Recorded on the module, and also in the working set: type names stay
        // unique across a package (a constructor's canonical name is built
        // from one, so two `Foo`s would collide downstream), and the duplicate
        // check needs to see every module's.
        let vis = self.vis;
        self.frame().tycons.push((name, arity, vis));
        if self.tycons.insert(name, arity).is_some() && !BUILTIN_TYCONS.contains(&&*name) {
            self.error(
                format!("type `{name}` is already defined"),
                "duplicate type".to_string(),
                span,
            );
        }
    }

    /// Register a constructor of `owner`.
    ///
    /// Three tables, because a constructor has three separate facts about it:
    /// what it *is* (keyed canonically), what its type's constructors are (for
    /// `use`), and whether this module may write it bare. The last is the only
    /// one that is scoped -- a type's own module always may.
    fn declare_ctor(
        &mut self,
        owner: InternedString,
        name: InternedString,
        arity: usize,
        field_names: Option<Vec<InternedString>>,
        span: Span,
    ) {
        let canonical = canonical_ctor(owner, name);
        if self
            .ctors
            .insert(canonical, CtorInfo { arity, field_names })
            .is_some()
            && !BUILTIN_CTORS.contains(&&*name)
        {
            self.error(
                format!("constructor `{owner}.{name}` is already defined"),
                "duplicate constructor".to_string(),
                span,
            );
        }
        self.ctors_of.entry(owner).or_default().push(name);
        let vis = self.vis;
        self.frame().ctors.push((name, canonical, vis));
        self.add_ctor(name, canonical);
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

    /// The canonical name a bare constructor refers to here, if any.
    /// Every constructor a bare `name` could mean here, best layer first: the
    /// module's own and imported ones if there are any (possibly several),
    /// otherwise the base layer's one.
    fn ctor_candidates(&self, name: InternedString) -> Vec<InternedString> {
        match self.module_ctors.get(&name) {
            Some(cs) if !cs.is_empty() => cs.clone(),
            _ => self.visible_ctors.get(&name).copied().into_iter().collect(),
        }
    }

    /// Make `bare` mean `canonical` in the current scope. Before the scope is
    /// sealed that replaces whatever it meant; after, it adds a meaning.
    fn add_ctor(&mut self, bare: InternedString, canonical: InternedString) {
        if !self.sealed {
            self.visible_ctors.insert(bare, canonical);
            return;
        }
        let cs = self.module_ctors.entry(bare).or_default();
        if !cs.contains(&canonical) {
            cs.push(canonical);
        }
    }

    /// Record that the name at `node` has several meanings.
    fn overload(&mut self, node: hir::NodeId, alts: Vec<hir::Alt>) {
        let candidates = alts
            .into_iter()
            .map(|alt| hir::Candidate {
                alt,
                name: match alt {
                    hir::Alt::Value(id) => self.names.get(&id).copied().unwrap_or_default(),
                    hir::Alt::Ctor(c) => bare_ctor(c),
                },
                origin: self.origin_of(alt),
            })
            .collect();
        self.overloads.insert(node, candidates);
    }

    /// Where a candidate came from, for a person choosing between them.
    fn origin_of(&self, alt: hir::Alt) -> Option<InternedString> {
        let hir::Alt::Value(id) = alt else {
            // A constructor's canonical name already says which type it is.
            return None;
        };
        let home = self
            .frames
            .iter()
            .find(|(_, f)| f.values.iter().any(|(_, v, _)| *v == id))
            .map(|(path, _)| path.clone());
        match home {
            Some(path) if path == self.current => None,
            Some(path) => Some(InternedString::from(dotted_path(&path))),
            None => self.origins.get(&id).copied(),
        }
    }

    /// Take the overloaded names found so far.
    pub fn take_overloads(&mut self) -> hir::Overloads {
        std::mem::take(&mut self.overloads)
    }

    /// The canonical name for an explicitly qualified `Ty.Ctor`.
    fn resolve_qualified_ctor(
        &self,
        ty: InternedString,
        name: InternedString,
    ) -> Option<InternedString> {
        let canonical = canonical_ctor(ty, name);
        self.ctors.contains_key(&canonical).then_some(canonical)
    }

    fn is_known_ctor(&self, name: InternedString) -> bool {
        self.module_ctors
            .get(&name)
            .is_some_and(|cs| !cs.is_empty())
            || self.visible_ctors.contains_key(&name)
            || BUILTIN_CTORS.contains(&&*name)
    }

    fn ctor_field_order(&self, name: InternedString) -> Option<Vec<InternedString>> {
        self.ctors.get(&name).and_then(|c| c.field_names.clone())
    }

    /// Every type name known here, including imported ones.
    pub fn imported_type_names(&self) -> Vec<InternedString> {
        self.ctors_of.keys().copied().collect()
    }

    /// Bring a type's constructors into scope unqualified -- what `use Expr`
    /// does, and what importing a type name from another module does.
    pub fn use_type_ctors(&mut self, ty: InternedString) {
        let ctors = self.ctors_of.get(&ty).cloned().unwrap_or_default();
        for c in ctors {
            self.add_ctor(c, canonical_ctor(ty, c));
        }
    }

    /// Mint the `VarId` for a top-level name of the module being declared.
    ///
    /// The id is minted up front so that mutual recursion works — across
    /// modules too, since the package is one unit — but the *name* goes into
    /// this module's frame rather than into the shared scope. A sibling sees
    /// it only through a `use`.
    fn predeclare(&mut self, name: InternedString) -> VarId {
        let vis = self.vis;
        let key = (self.current.clone(), name);
        if let Some(&id) = self.predeclared.get(&key) {
            return id;
        }
        let id = self.vars.fresh();
        self.names.insert(id, name);
        self.predeclared.insert(key, id);
        self.frames
            .entry(self.current.clone())
            .or_default()
            .values
            .push((name, id, vis));
        id
    }

    /// [`Resolver::predeclare`] a name written in a declaration, reporting it if
    /// this module already declares one -- a second `fun f` would otherwise
    /// silently share the first one's id, and only one body would survive.
    ///
    /// Two *modules* may each have an `f`; that is what overloading is for.
    fn predeclare_unique(&mut self, name: &ast::Ident) -> VarId {
        let key = (self.current.clone(), *name.value());
        if self.predeclared.contains_key(&key) {
            let first = self.decl_spans.get(&key).copied();
            self.errors.push(Diagnostic {
                msg: format!("`{}` is already defined in this module", name.value()),
                filename: self.filename.clone(),
                label: ("defined again here".to_string(), name.span),
                extra_labels: first
                    .map(|s| vec![("first defined here".to_string(), s)])
                    .unwrap_or_default(),
            });
            self.duplicates.insert(name.span);
        } else {
            self.decl_spans.insert(key, name.span);
        }
        self.predeclare(*name.value())
    }

    /// Whether a top-level pattern binds a name [`Resolver::predeclare_unique`]
    /// reported as a duplicate.
    fn binds_duplicate(&self, pat: &ast::LPat) -> bool {
        match pat.value() {
            ast::Pat::Var(n) => self.duplicates.contains(&n.span),
            ast::Pat::As(n, p) => self.duplicates.contains(&n.span) || self.binds_duplicate(p),
            ast::Pat::Tuple(ps) | ast::Pat::List(ps) | ast::Pat::Vector(ps) | ast::Pat::Cons(_, ps) => {
                ps.iter().any(|p| self.binds_duplicate(p))
            }
            ast::Pat::Record(fs, _) => fs.iter().any(|(_, p)| self.binds_duplicate(p)),
            _ => false,
        }
    }

    fn predeclare_pat(&mut self, pat: &ast::LPat) {
        match pat.value() {
            ast::Pat::Var(n) => {
                self.predeclare_unique(n);
            }
            ast::Pat::As(n, p) => {
                self.predeclare_unique(n);
                self.predeclare_pat(p);
            }
            ast::Pat::Tuple(ps)
            | ast::Pat::List(ps)
            | ast::Pat::Vector(ps)
            | ast::Pat::Cons(_, ps) => ps.iter().for_each(|p| self.predeclare_pat(p)),
            ast::Pat::Record(fs, _) => fs.iter().for_each(|(_, p)| self.predeclare_pat(p)),
            _ => {}
        }
    }

    // --- module ------------------------------------------------------------

    /// Resolve a module's bodies. The caller must already have run
    /// [`declare_toplevel`] for this module (and any siblings).
    pub fn resolve_module(&mut self, module: &ast::LModule) -> hir::LModule {
        // Everything in scope by now is the module's own or `use`d; what is
        // bound from here on is local.
        self.module_scope = self.scope.len();
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
                // Filled in by `meadow-scc`, which runs over the resolved module.
                groups: Vec::new(),
            },
            module.span,
        )
    }

    fn resolve_decl(&mut self, decl: &ast::LDecl) -> hir::LDecl {
        let (attrs, base) = peel(decl);
        let vis = self.set_decl_vis(attrs);
        // `@pub(pack) use M (a, b, c)` — re-export names this module can see.
        // Only the package's own surface is re-exportable: a `use` brings a
        // name *here*, and a sibling asks this module for it by name anyway.
        if let ast::Decl::Use(u) = base.value() {
            if vis == Vis::Exported {
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
        if vis == Vis::Exported {
            self.mark_pub(&hir);
        }
        if has_test(attrs) {
            self.mark_test(&hir, base.span);
        }
        hir
    }

    /// Record a `@test` declaration. A test is a function of no interest to
    /// anything but the runner, which calls it with `()` — so it has to *be*
    /// callable, and `@test def x = …` is a mistake worth naming.
    fn mark_test(&mut self, decl: &hir::LDecl, span: Span) {
        match decl.value() {
            hir::Decl::Bind(hir::Bind::Fun(name, params, _, _)) if params.len() == 1 => {
                self.test_vars.push((
                    self.names.get(name.value()).copied().unwrap_or_default(),
                    *name.value(),
                ));
            }
            _ => self.error(
                "`@test` must be a function of one argument".to_string(),
                "write `@test fun name u = …`, which the runner calls with `()`".to_string(),
                span,
            ),
        }
    }

    /// Record a `@pub(pack)` declaration's names in the export sets.
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

    /// `true` if the unit said anything about visibility at all — it then
    /// exports only what is marked `@pub(pack)` instead of everything.
    pub fn has_pub_markers(&self) -> bool {
        self.any_vis
    }

    pub fn is_pub_var(&self, id: VarId) -> bool {
        self.pub_vars.contains(&id)
    }

    /// Every `VarId` marked `@pub` — including `@pub use` re-exports, whose
    /// schemes live in a dependency rather than this unit.
    pub fn pub_var_ids(&self) -> impl Iterator<Item = VarId> + '_ {
        self.pub_vars.iter().copied()
    }

    /// Every `@test` function of this unit, in declaration order.
    pub fn test_vars(&self) -> &[(InternedString, VarId)] {
        &self.test_vars
    }

    pub fn is_pub_type(&self, name: InternedString) -> bool {
        self.pub_types.contains(&name)
    }

    fn resolve_bare_decl(&mut self, decl: &ast::LDecl) -> hir::LDecl {
        match decl.value() {
            ast::Decl::Attributed(_, inner) => self.resolve_bare_decl(inner),
            ast::Decl::Bind(bind) => {
                self.toplevel = true;
                // A type variable an annotation introduces belongs to *this*
                // declaration. Without the clear, `fun f (x : a) = x` and a
                // later `fun g (y : a) = y` would share one variable and be
                // forced to the same type.
                self.tyvars.clear();
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
                            // Canonical from here down: inference, the
                            // exhaustiveness checker and the back end's tag
                            // table all key on this, and all three need two
                            // types to be able to own a `Leaf`.
                            name: canonical_ctor(*dd.name.value(), *v.name.value()),
                            name_span: v.name.span,
                            fields,
                        }
                    })
                    .collect();
                self.tyvars.clear();
                self.node(
                    hir::Decl::Data(hir::DataDecl {
                        name: *dd.name.value(),
                        name_span: dd.name.span,
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
                        ctor: canonical_ctor(*rd.name.value(), *rd.name.value()),
                        name_span: rd.name.span,
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
                            .get(&(self.current.clone(), opname))
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
                        name_span: ed.name.span,
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
                let id = self.vars.fresh();
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
                    .or_else(|| {
                        if self.open_tyvars {
                            let id = self.vars.fresh();
                            self.names.insert(id, name);
                            self.tyvars.push((name, id));
                            return Some(id);
                        }
                        self.error(
                            format!("unbound type variable `{name}`"),
                            "not a parameter of this type".to_string(),
                            n.span,
                        );
                        None
                    });
                match id {
                    Some(id) => {
                        let v = self.node(id, n.span);
                        self.node(hir::TypeExpr::Var(v), t.span)
                    }
                    None => self.node(hir::TypeExpr::Error, t.span),
                }
            }
            ast::TypeExpr::Con(n, args) => {
                let name = *n.value();
                let ok = match self.tycons.get(&name).copied() {
                    Some(arity) if arity == args.len() => true,
                    Some(arity) => {
                        self.error(
                            format!(
                                "type `{name}` takes {arity} argument(s), got {}",
                                args.len()
                            ),
                            "wrong number of type arguments".to_string(),
                            n.span,
                        );
                        false
                    }
                    None => {
                        self.error(
                            format!("unknown type `{name}`"),
                            "not defined".to_string(),
                            n.span,
                        );
                        false
                    }
                };
                // Resolved either way, so a mistake inside the arguments is
                // reported too.
                let rargs = args.iter().map(|a| self.resolve_ty(a)).collect();
                if !ok {
                    return self.node(hir::TypeExpr::Error, t.span);
                }
                let con = self.node(name, n.span);
                self.node(hir::TypeExpr::Con(con, rargs), t.span)
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
            ast::TypeExpr::Vector(x) => {
                let rx = self.resolve_ty(x);
                self.node(hir::TypeExpr::Vector(rx), t.span)
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
                    self.vars.fresh()
                });
            self.node(id, t.span)
        });
        hir::EffectRow { labels, tail }
    }

    fn resolve_bind(&mut self, bind: &ast::Bind) -> hir::Bind {
        // `toplevel` means *this binding's own name* was predeclared, so
        // `bind_defn` should hand back the reserved id rather than a fresh one.
        // It must not leak into the sub-expressions: a `let` inside the body that
        // happens to share a name with a top-level binding is a new local, and
        // reusing the top-level id there would alias the two.
        let top = std::mem::replace(&mut self.toplevel, false);
        match bind {
            ast::Bind::Pat(pat, expr) => {
                // Right-hand side first, so it sees the *enclosing* scope and a
                // `let` can shadow: `let x = x + 1` reads the outer `x` rather
                // than the one being defined. A binding with parameters is
                // `Bind::Fun` below, which does bind its name first — that is
                // what makes `let rec go i = … go …` work, and a value binding
                // has nothing to gain from being self-recursive under eager
                // evaluation anyway.
                //
                // At the top level this changes nothing: `bind_defn` hands back
                // the id `declare_toplevel` already reserved, so both orders
                // resolve to the same binding and mutual recursion still works.
                let rexpr = self.resolve_expr(expr);
                self.toplevel = top;
                let rpat = self.resolve_pat(pat);
                self.toplevel = false;
                if top && self.binds_duplicate(pat) {
                    return hir::Bind::Error;
                }
                hir::Bind::Pat(rpat, rexpr)
            }
            ast::Bind::Fun(name, params, ret, body) => {
                self.toplevel = top;
                let id = self.bind_defn(*name.value());
                self.toplevel = false;
                let name_node = self.node(id, name.span);
                let mark = self.mark();
                let rparams = params.iter().map(|p| self.resolve_pat(p)).collect_vec();
                // Resolved after the parameters, so a type variable a parameter
                // introduced is the same one here: in
                // `fun id (x : a) : a = x` both `a`s are one variable.
                let rret = ret.as_ref().map(|t| {
                    let was = std::mem::replace(&mut self.open_tyvars, true);
                    let r = self.resolve_ty(t);
                    self.open_tyvars = was;
                    r
                });
                let rbody = self.resolve_expr(body);
                self.reset(mark);
                if top && self.duplicates.contains(&name.span) {
                    // Reported already, and it shares the first definition's
                    // id: inferring both would report the clash again as a
                    // type error. Its body was still resolved, for its own.
                    return hir::Bind::Error;
                }
                hir::Bind::Fun(name_node, rparams, rret, rbody)
            }
        }
    }

    // --- expressions -----------------------------------------------------------

    /// Warn if `q` is not an active module qualifier here (a `mod` child or a
    /// `use`d module). Constructor resolution itself is by bare name (constructor
    /// names are unique across a program), so this is only a scoping check.
    /// Does `q.Ctor` name a constructor of the *type* `q`?
    ///
    /// `Expr.Int` and `Mod.Int` look identical, so both readings are tried.
    /// The type reading is tried first: a type owns its constructors, whereas
    /// a module merely happens to contain them, and the qualified form exists
    /// precisely so a constructor can be named by its type.
    fn qualified_type_ctor(
        &mut self,
        q: &ast::Ident,
        name: InternedString,
    ) -> Option<InternedString> {
        let canonical = self.resolve_qualified_ctor(*q.value(), name)?;
        // The qualifier *is* the type, written out — so it is a reference to
        // it, and a rename of the type has to rewrite it.
        self.note_ref(q.span, NameRef::Type(*q.value()));
        Some(canonical)
    }

    fn check_qualifier(&mut self, q: &ast::Ident) {
        if !self.qualifiers.contains_key(&*q.value()) {
            self.error(
                // A bare `use` imports names unqualified; a qualifier comes only
                // from `as`, so that is what to suggest.
                format!(
                    "module `{}` is not in scope here (add `use <path> as {}`)",
                    q.value(),
                    q.value()
                ),
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
        // Bare here, canonical from here on: a constructor's identity downstream
        // is `Type.Ctor`, and the bare spelling is only how this module is
        // allowed to write it.
        let bare = *name.value();
        let cands = self.ctor_candidates(bare);
        let cname = cands.first().copied().unwrap_or(bare);
        if let [only] = args {
            if let ast::Expr::Record(fields, base) = only.value() {
                if base.is_none() {
                    let labels: Vec<InternedString> =
                        fields.iter().map(|(l, _)| *l.value()).collect();
                    if let Some((c, order)) = self.named_ctor(&cands, &labels, name) {
                        return self.resolve_named_ctor(span, c, name.span, fields, &order);
                    }
                }
            }
        }
        if !self.is_known_ctor(bare) {
            self.error(
                format!("unknown constructor `{bare}`"),
                "not a known constructor".to_string(),
                name.span,
            );
        }
        let label = self.node(cname, name.span);
        if cands.len() > 1 {
            self.overload(label.id, cands.into_iter().map(hir::Alt::Ctor).collect());
        }
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
                let ids = self.lookup_all(*name.value());
                if let Some(&first) = ids.first() {
                    let v = self.node(first, name.span);
                    if ids.len() > 1 {
                        self.overload(v.id, ids.into_iter().map(hir::Alt::Value).collect());
                    }
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
                if let Some(canonical) = self.qualified_type_ctor(q, *name.value()) {
                    let label = self.node(canonical, name.span);
                    let ra = args.iter().map(|a| self.resolve_expr(a)).collect_vec();
                    return self.node(hir::Expr::Cons(label, ra), expr.span);
                }
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
                    if let Some(canonical) = self.qualified_type_ctor(q, nn) {
                        let label = self.node(canonical, name.span);
                        return self.node(hir::Expr::Cons(label, vec![]), expr.span);
                    }
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
                            format!(
                                "module `{qn}` is not in scope here (add `use <path> as {qn}`)"
                            )
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
                        let f = ctor(self, "Bool.False");
                        hir::Expr::If(rl, rr, f)
                    }
                    _ => {
                        let t = ctor(self, "Bool.True");
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
        // Bare here, canonical from here on: a constructor's identity downstream
        // is `Type.Ctor`, and the bare spelling is only how this module is
        // allowed to write it.
        let bare = *name.value();
        let cands = self.ctor_candidates(bare);
        let cname = cands.first().copied().unwrap_or(bare);
        if let [only] = args {
            if let ast::Pat::Record(fields, _) = only.value() {
                let labels: Vec<InternedString> = fields.iter().map(|(l, _)| *l.value()).collect();
                if let Some((c, order)) = self.named_ctor(&cands, &labels, name) {
                    return self.resolve_named_ctor_pat(span, c, name.span, fields, &order);
                }
            }
        }
        if !self.is_known_ctor(bare) {
            self.error(
                format!("unknown constructor `{bare}`"),
                "not a known constructor".to_string(),
                name.span,
            );
        }
        let label = self.node(cname, name.span);
        if cands.len() > 1 {
            self.overload(label.id, cands.into_iter().map(hir::Alt::Ctor).collect());
        }
        let ra = args.iter().map(|p| self.resolve_pat(p)).collect_vec();
        self.node(hir::Pat::Cons(label, ra), span)
    }

    /// Which candidate a named-field constructor `C { a = .., b = .. }` is, and
    /// its field order.
    ///
    /// Named fields are reordered here, by the resolver, into the declared
    /// positions -- so unlike a positional constructor this cannot wait for
    /// inference to pick. The field names usually settle it; when they do not,
    /// the constructor has to be written `Type.C`.
    fn named_ctor(
        &mut self,
        cands: &[InternedString],
        labels: &[InternedString],
        name: &ast::Ident,
    ) -> Option<(InternedString, Vec<InternedString>)> {
        let named: Vec<(InternedString, Vec<InternedString>)> = cands
            .iter()
            .filter_map(|c| self.ctor_field_order(*c).map(|o| (*c, o)))
            .collect();
        if named.len() <= 1 {
            return named.into_iter().next();
        }
        let fits: Vec<&(InternedString, Vec<InternedString>)> = named
            .iter()
            .filter(|(_, order)| labels.iter().all(|l| order.contains(l)))
            .collect();
        if let [one] = fits.as_slice() {
            return Some((*one).clone());
        }
        let options = named
            .iter()
            .map(|(c, _)| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ");
        self.error(
            format!(
                "ambiguous constructor `{}`: could be {options}",
                name.value()
            ),
            "write it qualified, as `Type.Ctor`".to_string(),
            name.span,
        );
        named.into_iter().next()
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
            ast::Pat::Ann(inner, t) => {
                // The annotation resolves in the enclosing declaration's scope,
                // so a type variable in it is the same one a sibling annotation
                // means -- and may introduce one that nothing declared.
                let rp = self.resolve_pat(inner);
                let was = std::mem::replace(&mut self.open_tyvars, true);
                let rt = self.resolve_ty(t);
                self.open_tyvars = was;
                self.node(hir::Pat::Ann(Box::new(rp), rt), pat.span)
            }
            ast::Pat::As(name, sub) => {
                let id = self.bind_defn(*name.value());
                let v = self.node(id, name.span);
                let rsub = self.resolve_pat(sub);
                self.node(hir::Pat::As(v, rsub), pat.span)
            }
            ast::Pat::Cons(name, args) => self.resolve_ctor_pat(pat.span, name, args),
            ast::Pat::QualCons(q, name, args) => {
                if let Some(canonical) = self.qualified_type_ctor(q, *name.value()) {
                    let label = self.node(canonical, name.span);
                    let ra = args.iter().map(|p| self.resolve_pat(p)).collect_vec();
                    return self.node(hir::Pat::Cons(label, ra), pat.span);
                }
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
            // `[]` is the empty `Vector`, which is `Vector.Empty` — the library keeps
            // that the only representation of an empty vector (`vNormalize` and
            // `fromArray` both collapse to it). A non-empty vector has no
            // structural form, so say so rather than guessing.
            ast::Pat::Vector(pats) => {
                if pats.is_empty() {
                    let label = self.node(InternedString::from("Vector.Empty"), pat.span);
                    self.node(hir::Pat::Cons(label, vec![]), pat.span)
                } else {
                    self.error(
                        "a `Vector` pattern can only be the empty `[]`".to_string(),
                        format!(
                            "write `[{}]` to match a list, or match on `len` instead",
                            vec!["_"; pats.len()].join("; ")
                        ),
                        pat.span,
                    );
                    pats.iter().for_each(|p| {
                        self.resolve_pat(p);
                    });
                    self.node(hir::Pat::Error, pat.span)
                }
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
                        // The bare spelling: a record's constructor is
                        // canonically `Person.Person`, which would read as a
                        // stutter in a message about `Person`.
                        format!("missing field `{fname}` for `{}`", bare_ctor(name)),
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
            ast::Lit::Char(c) => hir::Lit::Char(*c),
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
