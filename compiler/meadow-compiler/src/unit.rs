//! Per-compilation-unit driver: **resolve → infer → lower** a set of
//! already-parsed modules, against a set of already-compiled dependency
//! packages, into one typed and lowered [`CompiledPackage`].
//!
//! This is the compiler's headline entry point. Everything *around* it —
//! filesystem package discovery, the package graph, linking, and the embedded
//! standard library — lives in the `meadow` build crate.

use crate::{
    ast, core,
    diagnostics::{from_parse_error, Diagnostic},
    exhaust,
    hir::{self, VarId},
    infer::{Infer, InferResult, Scheme, TypeTable},
    intern::InternedString,
    lexer::tokenize,
    parser,
    rename::{self, NameRef, Resolver},
    scc,
    source::{Source, SourceKind},
    Options,
};
use std::collections::HashMap;

/// One parsed module handed to [`compile_unit`].
pub struct AstModule {
    /// Dotted path from the package's source root; empty for the root module.
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub ast: ast::LModule,
    /// Where the text came from.
    ///
    /// A [`Span`](meadow_span::Span) is a pair of offsets and nothing else — it
    /// carries no record of which file it indexes. That is fine while a
    /// consumer only ever looks at one module, and not fine the moment one has
    /// to *name* a position: an editor answering go-to-definition across
    /// modules reads the file from here.
    pub source: Source,
}

/// A resolved module, paired with its position in the package.
#[derive(Clone)]
pub struct TypedModule {
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub hir: hir::LModule,
    /// Carried through from [`AstModule::source`] — what this module's spans are
    /// offsets into.
    pub source: Source,
    /// Names written in this module that the HIR does not keep: the contents
    /// of its `use` lists, and the type qualifying a `Type.Ctor`. An editor
    /// renaming a name has to rewrite these too.
    pub refs: Vec<rename::RefSite>,
}

/// An exported top-level binding: its name, its `VarId`, its inferred scheme, and
/// the dotted path of the module it was declared in (empty for a single-module
/// package). A dependent reaches it as `<pkg>.<module path>.<name>` via `use`.
#[derive(Clone)]
pub struct Export {
    pub name: InternedString,
    pub var: VarId,
    pub scheme: Scheme,
    pub module: Vec<InternedString>,
}

/// The output of [`compile_unit`]: one package, fully typed and lowered.
#[derive(Clone)]
pub struct CompiledPackage {
    /// Caller-assigned id — the build system uses the package-graph index, the
    /// REPL uses the line number. Not interpreted here.
    pub id: usize,
    /// Types whose constructors a dependent may write **unqualified**.
    ///
    /// A constructor's real name is `Type.Ctor`, so by default a dependent has
    /// to say which type it meant. This is the escape hatch the prelude uses:
    /// `Maybe`, `Result`, `List` and friends are listed here, which is why
    /// `Just` and `Nil` need no qualifier anywhere.
    pub flat_ctor_types: Vec<InternedString>,
    /// The `VarId` range this unit minted: `start..end`.
    ///
    /// A unit starts above everything its dependencies used, so the ranges of
    /// a package and everything under it never overlap. Recorded rather than
    /// recomputed because it is the provenance of an id — which unit owns it —
    /// and because a unit stacked on this one needs `end` to start from.
    pub vars: std::ops::Range<u32>,
    pub name: InternedString,
    /// This package's `main`, if its root module declares one.
    ///
    /// An entry point is not an export: nothing links against `main`, the
    /// runtime calls it. Finding it here rather than among the exports is what
    /// lets a program keep its declarations to itself — before this, adding
    /// `@pub` anywhere in a package meant `main` needed it too or the linker
    /// would report no entry point at all.
    pub entry: Option<VarId>,
    pub modules: Vec<TypedModule>,
    /// Whole-package node -> type table (node ids are dense across the package).
    pub types: TypeTable,
    pub exports: Vec<Export>,
    pub defs: Vec<core::Def>,
    /// Named-field order per constructor declared in this package.
    pub ctor_fields: HashMap<InternedString, Vec<InternedString>>,
    /// This package's resolved `data` / `record` / `effect` declarations,
    /// re-imported by dependents (and by later REPL lines).
    pub data_decls: Vec<hir::LDecl>,
    /// `@test` functions declared in this unit: `(name, its VarId)`, in
    /// declaration order. `meadow test` calls each with `()`.
    pub tests: Vec<(InternedString, VarId)>,
    /// Names a dependent gets **unqualified** automatically (the package's prelude
    /// re-exports). `None` = flat-import everything (REPL prefixes, ad-hoc `deps`);
    /// `Some(list)` = only these are flat, the rest need `use`.
    pub prelude_exports: Option<Vec<InternedString>>,
}

/// Compile a single source string as a one-module, dependency-free package.
///
/// Convenience for tests and quick experiments — lex → parse → resolve → infer →
/// lower, with every diagnostic collected (never fatal). Use the `meadow` build
/// crate's `compile_str_with_std` / `build` when the standard library or a
/// package graph is needed.
pub fn compile_str(name: &str, src: &str) -> (CompiledPackage, Vec<Diagnostic>) {
    compile_str_with(name, src, Options::default())
}

/// [`compile_str`] under an explicit set of compiler [`Options`].
pub fn compile_str_with(
    name: &str,
    src: &str,
    opts: Options,
) -> (CompiledPackage, Vec<Diagnostic>) {
    let name = InternedString::from(name);
    let source = Source::new(SourceKind::Interactive, InternedString::from(src));
    let lex = tokenize(source);
    let mut diags: Vec<Diagnostic> = lex.errors;
    let (ast, perrs) = parser::parse(name, source, &lex.tokens);
    for e in &perrs {
        diags.push(from_parse_error(&source.name().to_string(), e));
    }
    let modules = ast
        .map(|ast| {
            vec![AstModule {
                path: vec![],
                name,
                ast,
                source,
            }]
        })
        .unwrap_or_default();
    let (cp, unit_diags) = compile_unit(name, 0, modules, &[], opts);
    diags.extend(unit_diags);
    (cp, diags)
}

/// Resolve → infer → lower a set of already-parsed modules. Shared by the batch
/// build and the REPL. Modules within a unit may be mutually recursive.
pub fn compile_unit(
    unit_name: InternedString,
    id: usize,
    modules: Vec<AstModule>,
    deps: &[&CompiledPackage],
    opts: Options,
) -> (CompiledPackage, Vec<Diagnostic>) {
    compile_unit_in_package(unit_name, unit_name, id, modules, deps, opts)
}

/// Like [`compile_unit`], but `pkg` names the umbrella package (so a `use pkg.a.b`
/// from a sibling sub-module resolves against a peer named `a.b`). The embedded
/// stdlib compiles each of its modules as its own unit with `pkg = "Std"`.
pub fn compile_unit_in_package(
    pkg: InternedString,
    unit_name: InternedString,
    id: usize,
    modules: Vec<AstModule>,
    deps: &[&CompiledPackage],
    opts: Options,
) -> (CompiledPackage, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let filename = unit_name.to_string();

    // --- name resolution (whole unit at once, so modules may be mutually recursive)
    // Start above every dependency, so no two units can mint the same id and
    // an id can be traced back to the unit that owns it.
    let var_base = deps.iter().map(|d| d.vars.end).max().unwrap_or(0);
    let mut resolver = Resolver::with_prelude(filename.clone(), var_base);
    // Every dependency's *types* are known here, so they can be named in an
    // annotation and their constructors written `Type.Ctor`. Which of those
    // constructors may be written *bare* is a separate question, and the
    // answer is `flat_ctor_types` -- the prelude's list, plus anything a `use`
    // brings in later.
    for dep in deps {
        resolver.import_types(&dep.data_decls);
    }
    for dep in deps {
        match &dep.prelude_exports {
            // A REPL prefix or an ad-hoc dep: everything is flat, ctors too.
            None => {
                for ty in resolver.imported_type_names() {
                    resolver.use_type_ctors(ty);
                }
            }
            Some(_) => {
                for ty in &dep.flat_ctor_types {
                    resolver.use_type_ctors(*ty);
                }
            }
        }
    }
    // Flat value imports: a dep with `prelude_exports = None` (a REPL prefix, an
    // ad-hoc `compile_str` dep) contributes *everything*; one with `Some(list)`
    // contributes only `list` unqualified, the rest reachable via `use`.
    for dep in deps {
        match &dep.prelude_exports {
            None => {
                for e in &dep.exports {
                    resolver.import(e.name, e.var);
                }
            }
            Some(flat) => {
                for e in &dep.exports {
                    if e.module.is_empty() && flat.contains(&e.name) {
                        resolver.import(e.name, e.var);
                    }
                }
            }
        }
    }
    // Everything above is shared by every module of the unit. What follows
    // belongs to one module at a time.
    resolver.seal_base_scope();

    // Declare first, all modules, so that a `use` can name a sibling and so
    // that mutual recursion across modules keeps working — the package is one
    // compilation unit, and only the *namespaces* are per module.
    for m in &modules {
        resolver.set_filename(module_filename(&filename, m.source));
        resolver.set_module(&m.path);
        resolver.declare_types(&m.ast.value().decls);
    }
    for m in &modules {
        resolver.set_filename(module_filename(&filename, m.source));
        resolver.set_module(&m.path);
        resolver.declare_toplevel(&m.ast.value().decls);
    }

    let mut typed: Vec<TypedModule> = modules
        .iter()
        .map(|m| {
            // Its own declarations and nothing else, then whatever it asked
            // for. A sibling's names are never free.
            let here = module_filename(&filename, m.source);
            resolver.set_filename(here.clone());
            resolver.enter_module(&m.path);
            for d in &m.ast.value().decls {
                let base = match d.value() {
                    ast::Decl::Attributed(_, inner) => inner.value(),
                    other => other,
                };
                if let ast::Decl::Use(u) = base {
                    apply_use(&mut resolver, pkg, u, deps, &here, &mut diags);
                }
            }
            let mut hir = resolver.resolve_module(&m.ast);
            // Reorder the top-level bindings by dependency and record their
            // groups, so inference (and evaluation) never meets a name before the
            // thing that defines it.
            scc::group_module(&mut hir.value);
            TypedModule {
                path: m.path.clone(),
                name: m.name,
                hir,
                source: m.source,
                refs: resolver.take_extra_refs(),
            }
        })
        .collect();
    // Same again one level up: a unit's modules are discovered in alphabetical
    // order, which says nothing about what depends on what, so they need
    // sorting too.
    let order = scc::module_order(typed.iter().map(|m| m.hir.value()));
    typed = permute(typed, &order);
    diags.extend(resolver.take_errors());

    // --- type inference (one arena for the whole unit + dependency schemes)
    let mut infer = Infer::new(filename.clone(), resolver.id_count());
    infer.load_prelude(&resolver.prelude_bindings());
    let dep_schemes: Vec<(VarId, Scheme)> = deps
        .iter()
        .flat_map(|d| d.exports.iter().map(|e| (e.var, e.scheme.clone())))
        .collect();
    infer.load_deps(&dep_schemes);
    for dep in deps {
        infer.register_types(&dep.data_decls);
    }
    for m in &typed {
        infer.set_filename(module_filename(&filename, m.source));
        infer.register_types(&m.hir.value().decls);
        // This unit's own effect operations are part of its export surface, so a
        // dependent can write `State.get` rather than relying on them being
        // injected into every scope (where an ordinary value can shadow them).
        infer.export_effect_ops(&m.hir.value().decls);
    }
    for m in &typed {
        infer.set_filename(module_filename(&filename, m.source));
        infer.infer_module(&m.hir);
    }
    let InferResult {
        table,
        schemes,
        generalized,
        variants,
        errors,
    } = infer.finish();
    diags.extend(errors);

    // --- pattern coverage (needs the types; runs before lowering discards them)
    for m in &typed {
        diags.extend(exhaust::check_module(
            &module_filename(&filename, m.source),
            &m.hir,
            &table,
            &variants,
            opts.check_exhaustive(),
        ));
    }

    // --- lower to core
    let prims = prim_map(&resolver);
    let names = resolver.names().clone();
    let effect_ops: HashMap<VarId, (InternedString, InternedString)> = resolver
        .effect_op_vars()
        .into_iter()
        .map(|(id, eff, op)| (id, (eff, op)))
        .collect();
    let ctor_arity = resolver.ctor_arities();
    // Every polymorphic name a mention could refer to: this unit's bindings,
    // and the ones its dependencies exported. Lowering needs them to give each
    // mention its type arguments.
    let mut all_schemes: HashMap<VarId, Scheme> = dep_schemes.iter().cloned().collect();
    for (var, g) in &generalized {
        all_schemes.insert(*var, g.scheme.clone());
    }
    let mut lowerer = core::Lowerer::new(
        &prims,
        &names,
        &effect_ops,
        &table,
        &ctor_arity,
        &generalized,
        &all_schemes,
        resolver.var_gen(),
    );
    let mut defs = Vec::new();
    for m in &typed {
        defs.extend(lowerer.lower_module(&m.hir));
    }
    let var_end = lowerer.var_end();

    // --- lint (debug builds and tests only)
    //
    // Core is typed so that a pass over it can be checked; this is where the
    // checking happens. A failure is a compiler bug, not a program error, so
    // it is loud, and it costs a release build nothing.
    //
    // Only for a unit that compiled cleanly: the core of a program with type
    // errors in it is ill-typed, and saying so twice helps nobody.
    if cfg!(debug_assertions) && diags.is_empty() {
        let program = core::Program {
            defs: defs.clone(),
            entry: None,
            ctor_fields: lowerer.ctor_fields.clone(),
        };
        let imported: HashMap<VarId, Scheme> = dep_schemes.iter().cloned().collect();
        let problems = core::lint::check(&program, &variants, &imported);
        assert!(
            problems.is_empty(),
            "core lint failed after lowering `{filename}`:\n{}",
            problems.join("\n")
        );
    }
    // Drop the `&table` borrow held by `lowerer` before `table` is moved below.
    let ctor_fields = lowerer.ctor_fields;

    // Export surface. If the unit used `@pub` anywhere, only the `@pub`
    // declarations (and `@pub use` re-exports) are exported; otherwise everything.
    let gated = resolver.has_pub_markers();
    // Which module each top-level `VarId` was declared in (for `Export.module`).
    let mut var_module: HashMap<VarId, Vec<InternedString>> = HashMap::new();
    for m in &typed {
        collect_toplevel_vars(&m.hir, &m.path, &mut var_module);
    }
    let mut exports: Vec<Export> = Vec::new();
    let mut exported: std::collections::HashSet<VarId> = std::collections::HashSet::new();
    for (var, scheme) in &schemes {
        if gated && !resolver.is_pub_var(*var) {
            continue;
        }
        exported.insert(*var);
        exports.push(Export {
            name: names.get(var).copied().unwrap_or_default(),
            var: *var,
            scheme: scheme.clone(),
            module: var_module.get(var).cloned().unwrap_or_default(),
        });
    }
    // `@pub use M (x)` re-exports: `x` is defined in a dependency, so its scheme
    // comes from `dep_schemes`. These carry an empty module path — they are the
    // package's unqualified (prelude) surface.
    if gated {
        let dep_scheme_map: HashMap<VarId, Scheme> = dep_schemes.iter().cloned().collect();
        for var in resolver.pub_var_ids() {
            if exported.contains(&var) {
                continue;
            }
            if let Some(scheme) = dep_scheme_map.get(&var) {
                exports.push(Export {
                    name: names.get(&var).copied().unwrap_or_default(),
                    var,
                    scheme: scheme.clone(),
                    module: Vec::new(),
                });
            }
        }
    }
    exports.sort_by_key(|e| e.var.0);

    let data_decls: Vec<hir::LDecl> = typed
        .iter()
        .flat_map(|m| m.hir.value().decls.iter())
        .filter(|d| match d.value() {
            hir::Decl::Data(dd) => !gated || resolver.is_pub_type(dd.name),
            hir::Decl::Record(rd) => !gated || resolver.is_pub_type(rd.name),
            hir::Decl::Effect(ed) => !gated || resolver.is_pub_type(ed.name),
            _ => false,
        })
        .cloned()
        .collect();

    // Types whose constructors a dependent may write bare.
    //
    // Deliberately narrow: a type re-exported by `@pub use` is the package
    // saying "this is part of my unqualified surface", which is exactly what
    // the prelude does for `Maybe`, `Result` and `List`. An ungated package
    // (no `@pub` anywhere) exports everything, so its own types go too.
    let mut flat_ctor_types: Vec<InternedString> = modules
        .iter()
        .flat_map(|m| m.ast.value().decls.iter())
        .filter_map(|d| match d.value() {
            ast::Decl::Attributed(attrs, inner) => match inner.value() {
                // `@pub(pack) use M (T)`: the package's *unqualified* surface,
                // which is a claim about what a dependent may write bare.
                ast::Decl::Use(u)
                    if attrs.iter().any(|a| {
                        &**a.name.value() == "pub"
                            && a.args.first().is_some_and(|x| &**x.value() == "pack")
                    }) =>
                {
                    Some(u.names.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .map(|n| *n.value())
        .filter(|n| n.chars().next().is_some_and(|c| c.is_uppercase()))
        .collect();
    if !gated {
        for d in &data_decls {
            match d.value() {
                hir::Decl::Data(dd) => flat_ctor_types.push(dd.name),
                hir::Decl::Record(rd) => flat_ctor_types.push(rd.name),
                _ => {}
            }
        }
    }
    flat_ctor_types.sort_by_key(|n| n.to_string());
    flat_ctor_types.dedup();

    // The entry point, from the root module alone: a `main` in a submodule is
    // an ordinary function that happens to be called `main`.
    let main = InternedString::from("main");
    let entry = typed
        .iter()
        .filter(|m| m.path.is_empty())
        .flat_map(|m| m.hir.value().decls.iter())
        .filter_map(|d| match d.value() {
            hir::Decl::Bind(bind) => bind
                .bound_vars()
                .into_iter()
                .find(|id| names.get(id) == Some(&main)),
            _ => None,
        })
        .next();

    (
        CompiledPackage {
            id,
            flat_ctor_types,
            vars: var_base..var_end,
            name: unit_name,
            entry,
            modules: typed,
            types: table,
            exports,
            defs,
            ctor_fields,
            data_decls,
            tests: resolver.test_vars().to_vec(),
            prelude_exports: None,
        },
        diags,
    )
}

/// Reorder `items` so that the element at `order[k]` ends up `k`th.
fn permute<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    debug_assert_eq!(items.len(), order.len(), "permutation must cover every item");
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .map(|&i| slots[i].take().expect("each index appears once"))
        .collect()
}

/// Record which module each top-level binding lives in.
fn collect_toplevel_vars(
    module: &hir::LModule,
    path: &[InternedString],
    out: &mut HashMap<VarId, Vec<InternedString>>,
) {
    for decl in &module.value().decls {
        if let hir::Decl::Bind(bind) = decl.value() {
            for id in bind.bound_vars() {
                out.entry(id).or_insert_with(|| path.to_vec());
            }
        }
    }
}

/// Resolve one `use` decl against the dependency packages and register the
/// resulting module qualifier (+ any explicitly named unqualified imports).
/// Bring the module a `use` names into scope, and report it if there is no such
/// module (or no such exported name).
fn apply_use(
    resolver: &mut Resolver,
    pkg: InternedString,
    u: &ast::UseDecl,
    deps: &[&CompiledPackage],
    filename: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let segs: Vec<InternedString> = u.path.iter().map(|s| *s.value()).collect();
    if segs.is_empty() {
        return;
    }
    let path_span = u
        .path
        .iter()
        .fold(u.path[0].span, |acc, s| acc.extend(s.span));

    // `use Pack.Mod` — a sibling namespace of this very unit. The package is
    // the compilation unit and modules are namespaces inside it, so a sibling
    // is reached through the resolver's frames, not through `deps`.
    let local: Vec<InternedString> = if segs[0] == pkg {
        segs[1..].to_vec()
    } else {
        segs.clone()
    };
    if !local.is_empty() && resolver.has_module(&local) {
        match &u.alias {
            Some(a) => {
                let values = resolver.module_values(&local);
                resolver.activate_module(*a.value(), values);
            }
            None => resolver.use_module(&local, &u.names),
        }
        return;
    }

    let Resolved { map, found } = resolve_module(pkg, &segs, deps);

    if !found {
        let path = dotted(&segs);
        let mut msg = format!("no module `{path}`");
        if let Some(suggestion) = suggest_module(&segs, deps) {
            msg.push_str(&format!(" — did you mean `{suggestion}`?"));
        }
        diags.push(Diagnostic {
            msg,
            filename: filename.to_string(),
            label: ("not found".to_string(), path_span),
            extra_labels: vec![],
        });
        return;
    }

    match &u.alias {
        // `use M as C` — a qualifier and nothing else. `C.name` reaches the
        // module's exports; none of them are in scope unqualified.
        Some(a) => resolver.activate_module(*a.value(), map.clone()),
        // `use M` — bring every exported name into scope unqualified. Qualifying
        // is what `as` is for, so a bare `use` does not introduce one.
        None if u.names.is_empty() => {
            // Sorted, so the scope is built the same way on every run.
            let mut all: Vec<(InternedString, VarId)> =
                map.iter().map(|(n, id)| (*n, *id)).collect();
            all.sort_by_key(|(n, _)| n.to_string());
            for (name, id) in all {
                resolver.import(name, id);
            }
        }
        None => {}
    }
    // Only *values* live in `map`. A selected name can also be a type, a data
    // constructor or an effect operation, and those are imported wholesale
    // elsewhere (`import_types`), so a miss here is not an error.
    for n in &u.names {
        if let Some(&id) = map.get(&*n.value()) {
            resolver.import(*n.value(), id);
            resolver.note_ref(n.span, NameRef::Value(id));
        } else if n.value().chars().next().is_some_and(|c| c.is_uppercase()) {
            // A type (or an effect): no id to carry, so it is named.
            resolver.note_ref(n.span, NameRef::Type(*n.value()));
        }
        // `use M (Expr)` names a *type*, and naming a type brings its
        // constructors into scope unqualified -- which is the only way to
        // write `Int` rather than `Expr.Int`.
        resolver.use_type_ctors(*n.value());
    }
}

/// What a `use` path names: whether the module exists at all, and the values it
/// exports.
pub struct Resolved {
    /// Exported **values**, by name. Types, constructors and effect operations
    /// are imported wholesale elsewhere and never appear here.
    pub map: HashMap<InternedString, VarId>,
    /// Whether the module exists — which is not the same as `!map.is_empty()`,
    /// since `Std.Collections` is nothing but `mod` declarations.
    pub found: bool,
}

/// Look up the module a `use` path names among `deps`. Shared by `use`
/// resolution and by the REPL's completer, so the two cannot disagree about
/// what is in scope.
pub fn resolve_module(
    pkg: InternedString,
    segs: &[InternedString],
    deps: &[&CompiledPackage],
) -> Resolved {
    let mut map = HashMap::new();
    let mut found = false;
    if segs.is_empty() {
        return Resolved { map, found };
    }
    // `use Pkg.a.b` inside package `Pkg` refers to the local module `a.b`.
    let local: &[InternedString] = if segs[0] == pkg { &segs[1..] } else { segs };

    for dep in deps {
        // intra-batch sub-module: dep is named by its dotted module path
        let dep_is_local_module =
            dotted(local) == &*dep.name.to_string() || dotted(segs) == &*dep.name.to_string();
        if dep_is_local_module {
            found = true;
            for e in &dep.exports {
                map.insert(e.name, e.var);
            }
            continue;
        }
        // external package: first segment is the package name, rest is module path
        if segs[0] == dep.name {
            let want = &segs[1..];
            if dep.modules.iter().any(|m| m.path == want) {
                found = true;
            }
            for e in &dep.exports {
                if e.module == want {
                    map.insert(e.name, e.var);
                }
            }
        }
    }
    Resolved { map, found }
}

/// A module elsewhere whose last segment matches the one asked for, rendered as
/// the full path it should have been written as — so `use List` can point at
/// `Std.Collections.List`.
fn suggest_module(segs: &[InternedString], deps: &[&CompiledPackage]) -> Option<String> {
    let last = segs.last()?;
    for dep in deps {
        for m in &dep.modules {
            if m.path.last() == Some(last) && m.path.as_slice() != segs {
                let mut full = vec![dep.name];
                full.extend(m.path.iter().copied());
                return Some(dotted(&full));
            }
        }
    }
    None
}

/// What a diagnostic from `source` should call its file.
///
/// A unit is compiled as a whole, but its modules are separate files and an
/// error has to name the one it is in. Interactive text — the REPL, a
/// `compile_str` — has no file, so it keeps the unit's name.
fn module_filename(unit: &str, source: Source) -> String {
    match source.kind {
        SourceKind::File(name) => name.to_string(),
        SourceKind::Interactive => unit.to_string(),
    }
}

fn dotted(segs: &[InternedString]) -> String {
    segs.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

fn prim_map(resolver: &Resolver) -> HashMap<VarId, core::Prim> {
    resolver
        .prelude_bindings()
        .into_iter()
        .filter_map(|(name, id)| core::Prim::from_name(&name).map(|p| (id, p)))
        .collect()
}
