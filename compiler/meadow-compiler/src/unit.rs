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
    rename::Resolver,
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
}

/// A resolved module, paired with its position in the package.
pub struct TypedModule {
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub hir: hir::LModule,
}

/// An exported top-level binding: its name, its `VarId`, its inferred scheme, and
/// the dotted path of the module it was declared in (empty for a single-module
/// package). A dependent reaches it as `<pkg>.<module path>.<name>` via `use`.
pub struct Export {
    pub name: InternedString,
    pub var: VarId,
    pub scheme: Scheme,
    pub module: Vec<InternedString>,
}

/// The output of [`compile_unit`]: one package, fully typed and lowered.
pub struct CompiledPackage {
    /// Caller-assigned id — the build system uses the package-graph index, the
    /// REPL uses the line number. Not interpreted here.
    pub id: usize,
    pub name: InternedString,
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
    let mut resolver = Resolver::with_prelude(filename.clone());
    // Types / constructors are a single global namespace — always import every dep's.
    for dep in deps {
        resolver.import_types(&dep.data_decls);
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
    // Qualified imports: honour each `use` decl in the modules being compiled.
    for m in &modules {
        for d in &m.ast.value().decls {
            let base = match d.value() {
                ast::Decl::Attributed(_, inner) => inner.value(),
                other => other,
            };
            if let ast::Decl::Use(u) = base {
                apply_use(&mut resolver, pkg, u, deps, &filename, &mut diags);
            }
        }
    }
    for m in &modules {
        resolver.declare_types(&m.ast.value().decls);
    }
    for m in &modules {
        resolver.declare_toplevel(&m.ast.value().decls);
    }
    let mut typed: Vec<TypedModule> = modules
        .iter()
        .map(|m| {
            let mut hir = resolver.resolve_module(&m.ast);
            // Reorder the top-level bindings by dependency and record their
            // groups, so inference (and evaluation) never meets a name before the
            // thing that defines it.
            scc::group_module(&mut hir.value);
            TypedModule {
                path: m.path.clone(),
                name: m.name,
                hir,
            }
        })
        .collect();
    // Same again one level up: a unit's modules are resolved into one flat scope
    // but discovered in alphabetical order, so they need sorting too.
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
        infer.register_types(&m.hir.value().decls);
        // This unit's own effect operations are part of its export surface, so a
        // dependent can write `State.get` rather than relying on them being
        // injected into every scope (where an ordinary value can shadow them).
        infer.export_effect_ops(&m.hir.value().decls);
    }
    for m in &typed {
        infer.infer_module(&m.hir);
    }
    let InferResult {
        table,
        schemes,
        variants,
        errors,
    } = infer.finish();
    diags.extend(errors);

    // --- pattern coverage (needs the types; runs before lowering discards them)
    for m in &typed {
        diags.extend(exhaust::check_module(
            &filename,
            &m.hir,
            &table,
            &variants,
            opts.check_exhaustive,
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
    let mut lowerer = core::Lowerer::new(&prims, &names, &effect_ops, &table, &ctor_arity);
    let mut defs = Vec::new();
    for m in &typed {
        defs.extend(lowerer.lower_module(&m.hir));
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

    (
        CompiledPackage {
            id,
            name: unit_name,
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

    let qualifier = match &u.alias {
        Some(a) => *a.value(),
        None => *segs.last().unwrap(),
    };
    resolver.activate_module(qualifier, map.clone());
    // Only *values* live in `map`. A selected name can also be a type, a data
    // constructor or an effect operation, and those are imported wholesale
    // elsewhere (`import_types`), so a miss here is not an error.
    for n in &u.names {
        if let Some(&id) = map.get(&*n.value()) {
            resolver.import(*n.value(), id);
        }
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
