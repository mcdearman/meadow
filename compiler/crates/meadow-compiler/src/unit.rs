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
    hir::{self, VarId},
    infer::{Infer, InferResult, Scheme, TypeTable},
    intern::InternedString,
    lexer::tokenize,
    parser,
    rename::Resolver,
    source::{Source, SourceKind},
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

/// An exported top-level binding: its name, its `VarId`, and its inferred scheme.
pub struct Export {
    pub name: InternedString,
    pub var: VarId,
    pub scheme: Scheme,
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
}

/// Compile a single source string as a one-module, dependency-free package.
///
/// Convenience for tests and quick experiments — lex → parse → resolve → infer →
/// lower, with every diagnostic collected (never fatal). Use the `meadow` build
/// crate's `compile_str_with_std` / `build` when the standard library or a
/// package graph is needed.
pub fn compile_str(name: &str, src: &str) -> (CompiledPackage, Vec<Diagnostic>) {
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
    let (cp, unit_diags) = compile_unit(name, 0, modules, &[]);
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
) -> (CompiledPackage, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let filename = unit_name.to_string();

    // --- name resolution (whole unit at once, so modules may be mutually recursive)
    let mut resolver = Resolver::with_prelude(filename.clone());
    for dep in deps {
        for e in &dep.exports {
            resolver.import(e.name, e.var);
        }
        resolver.import_types(&dep.data_decls);
    }
    for m in &modules {
        resolver.declare_types(&m.ast.value().decls);
    }
    for m in &modules {
        resolver.declare_toplevel(&m.ast.value().decls);
    }
    let typed: Vec<TypedModule> = modules
        .iter()
        .map(|m| TypedModule {
            path: m.path.clone(),
            name: m.name,
            hir: resolver.resolve_module(&m.ast),
        })
        .collect();
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
    }
    for m in &typed {
        infer.infer_module(&m.hir);
    }
    let InferResult {
        table,
        schemes,
        errors,
    } = infer.finish();
    diags.extend(errors);

    // --- lower to core
    let prims = prim_map(&resolver);
    let names = resolver.names().clone();
    let effect_ops: HashMap<VarId, (InternedString, InternedString)> = resolver
        .effect_op_vars()
        .into_iter()
        .map(|(id, eff, op)| (id, (eff, op)))
        .collect();
    let mut lowerer = core::Lowerer::new(&prims, &names, &effect_ops, &table);
    let mut defs = Vec::new();
    for m in &typed {
        defs.extend(lowerer.lower_module(&m.hir));
    }
    // Drop the `&table` borrow held by `lowerer` before `table` is moved below.
    let ctor_fields = lowerer.ctor_fields;

    // Export surface. If the unit used `@pub` anywhere, only the `@pub`
    // declarations (and `@pub use` re-exports) are exported; otherwise everything.
    let gated = resolver.has_pub_markers();
    let mut exports: Vec<Export> = schemes
        .iter()
        .filter(|(var, _)| !gated || resolver.is_pub_var(**var))
        .map(|(var, scheme)| Export {
            name: names.get(var).copied().unwrap_or_default(),
            var: *var,
            scheme: scheme.clone(),
        })
        .collect();
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
        },
        diags,
    )
}

fn prim_map(resolver: &Resolver) -> HashMap<VarId, core::Prim> {
    resolver
        .prelude_bindings()
        .into_iter()
        .filter_map(|(name, id)| core::Prim::from_name(&name).map(|p| (id, p)))
        .collect()
}
