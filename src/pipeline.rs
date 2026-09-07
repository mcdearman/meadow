//! The compiler driver. It knows exactly one job: **compile a package** (a set of
//! modules) against a set of already-compiled dependency packages, producing typed
//! HIR + lowered [`core`]. It has no idea whether it is serving a batch build or a
//! REPL — the REPL just keeps feeding it one-line packages whose dependencies are
//! the previous lines.

use crate::{
    ast, core,
    diagnostics::Diagnostic,
    hir::{self, VarId},
    infer::{Infer, InferResult, Scheme, TypeTable},
    intern::InternedString,
    lexer::{tokenize, Token},
    linker::{LinkedProgram, Linker},
    package::{Package, PackageGraph, PackageId},
    parser,
    rename::Resolver,
    source::Source,
    span::Span,
};
use chumsky::error::Rich;
use std::collections::HashMap;
use std::path::Path;

/// One parsed module handed to [`compile_unit`].
pub struct AstModule {
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub ast: ast::LModule,
}

pub struct TypedModule {
    pub path: Vec<InternedString>,
    pub name: InternedString,
    pub hir: hir::LModule,
}

pub struct Export {
    pub name: InternedString,
    pub var: VarId,
    pub scheme: Scheme,
}

pub struct CompiledPackage {
    pub id: PackageId,
    pub name: InternedString,
    pub modules: Vec<TypedModule>,
    /// Whole-package node -> type table (node ids are dense across the package).
    pub types: TypeTable,
    pub exports: Vec<Export>,
    pub defs: Vec<core::Def>,
    /// Named-field order per constructor declared in this package.
    pub ctor_fields: std::collections::HashMap<InternedString, Vec<InternedString>>,
    /// This package's resolved `data` / `record` declarations, re-imported by
    /// dependents (and by later REPL lines).
    pub data_decls: Vec<hir::LDecl>,
}

pub struct BuildOutput {
    pub linked: Option<LinkedProgram>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Discover, compile and link the package rooted at `entry`.
pub fn build(entry: &Path) -> BuildOutput {
    let graph = match PackageGraph::build(entry) {
        Ok(g) => g,
        Err(d) => {
            return BuildOutput {
                linked: None,
                diagnostics: vec![d],
            }
        }
    };

    let mut diagnostics = Vec::new();
    let mut compiled: Vec<Option<CompiledPackage>> =
        (0..graph.packages.len()).map(|_| None).collect();

    for &pid in graph.order() {
        let pkg = &graph.packages[pid];
        let deps: Vec<&CompiledPackage> = pkg
            .deps
            .iter()
            .map(|d| compiled[*d].as_ref().expect("topological order"))
            .collect();
        let (cp, mut d) = compile_package(pkg, &deps);
        diagnostics.append(&mut d);
        compiled[pid] = Some(cp);
    }

    let ordered: Vec<CompiledPackage> = graph
        .order()
        .iter()
        .map(|&pid| compiled[pid].take().expect("compiled above"))
        .collect();

    BuildOutput {
        linked: Some(Linker::link(ordered)),
        diagnostics,
    }
}

fn compile_package(
    pkg: &Package,
    deps: &[&CompiledPackage],
) -> (CompiledPackage, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut modules = Vec::new();

    for m in &pkg.modules {
        let lex = tokenize(m.source);
        diags.extend(lex.errors);
        let (ast, perrs) = parser::parse(m.name, m.source, &lex.tokens);
        for e in &perrs {
            diags.push(rich_to_diag(&m.source, e));
        }
        if let Some(ast) = ast {
            modules.push(AstModule {
                path: m.path.clone(),
                name: m.name,
                ast,
            });
        }
    }

    let (cp, unit_diags) = compile_unit(pkg.name, pkg.id, modules, deps);
    diags.extend(unit_diags);
    (cp, diags)
}

/// Compile a single source string as a one-module, dependency-free package.
///
/// Convenience for tests and quick experiments — it runs lex → parse → resolve →
/// infer → lower and returns the [`CompiledPackage`] plus every diagnostic
/// (lex/parse/resolve/type errors are all collected, never fatal).
pub fn compile_str(name: &str, src: &str) -> (CompiledPackage, Vec<Diagnostic>) {
    let name = InternedString::from(name);
    let source = Source::new(
        crate::source::SourceKind::Interactive,
        InternedString::from(src),
    );
    let lex = tokenize(source);
    let mut diags: Vec<Diagnostic> = lex.errors;
    let (ast, perrs) = parser::parse(name, source, &lex.tokens);
    for e in &perrs {
        diags.push(rich_to_diag(&source, e));
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

/// Resolve -> infer -> lower a set of already-parsed modules. Shared by the batch
/// build and the REPL.
pub fn compile_unit(
    unit_name: InternedString,
    id: PackageId,
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
    let mut lowerer = core::Lowerer::new(&prims, &names, &effect_ops);
    let mut defs = Vec::new();
    for m in &typed {
        defs.extend(lowerer.lower_module(&m.hir));
    }

    let mut exports: Vec<Export> = schemes
        .iter()
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
        .filter(|d| {
            matches!(
                d.value(),
                hir::Decl::Data(_) | hir::Decl::Record(_) | hir::Decl::Effect(_)
            )
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
            ctor_fields: lowerer.ctor_fields,
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

fn rich_to_diag(src: &Source, e: &Rich<'_, Token, Span>) -> Diagnostic {
    crate::diagnostics::from_parse_error(&src.name().to_string(), e)
}
