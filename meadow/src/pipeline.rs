//! The **build driver**: filesystem package discovery, running
//! [`meadow_compiler::compile_unit`] over the package graph, linking the result,
//! and injecting the embedded `Std` package.
//!
//! The per-unit compile (resolve → infer → lower) itself lives in
//! `meadow-compiler`; [`compile_unit`] / [`compile_str`] / [`CompiledPackage`] are
//! re-exported here for convenience.

use crate::linker::{LinkedProgram, Linker};
use crate::package::{Package, PackageGraph};
use crate::stdlib;
use meadow_compiler::{
    core,
    diagnostics::Diagnostic,
    intern::InternedString,
    lexer::{tokenize, Token},
    parser,
    source::{Source, SourceKind},
    span::Span,
};
use chumsky::error::Rich;
use std::path::Path;

pub use meadow_compiler::{
    compile_str, compile_unit, AstModule, CompiledPackage, Export, TypedModule,
};

pub struct BuildOutput {
    pub linked: Option<LinkedProgram>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Discover, compile and link the package rooted at `entry`.
///
/// The embedded `Std` package (see [`crate::stdlib`]) is compiled first and made
/// an implicit dependency of every package, so the prelude is always in scope.
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

    let (std_pkgs, mut diagnostics) = stdlib::compile_std();

    let mut compiled: Vec<Option<CompiledPackage>> =
        (0..graph.packages.len()).map(|_| None).collect();

    for &pid in graph.order() {
        let pkg = &graph.packages[pid];
        let mut deps: Vec<&CompiledPackage> = std_pkgs.iter().collect();
        deps.extend(
            pkg.deps
                .iter()
                .map(|d| compiled[*d].as_ref().expect("topological order")),
        );
        let (cp, mut d) = compile_package(pkg, &deps);
        diagnostics.append(&mut d);
        compiled[pid] = Some(cp);
    }

    let ordered: Vec<CompiledPackage> = std_pkgs
        .into_iter()
        .chain(
            graph
                .order()
                .iter()
                .map(|&pid| compiled[pid].take().expect("compiled above")),
        )
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

/// Like [`compile_str`], but with the embedded `Std` package as a dependency (so
/// the prelude is in scope) and everything linked into one runnable
/// [`core::Program`]. For tests / experiments that want the standard library.
pub fn compile_str_with_std(name: &str, src: &str) -> (core::Program, Vec<Diagnostic>) {
    let name = InternedString::from(name);
    let source = Source::new(SourceKind::Interactive, InternedString::from(src));
    let lex = tokenize(source);
    let (std_pkgs, mut diags) = stdlib::compile_std();
    diags.extend(lex.errors.clone());
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
    let deps: Vec<&CompiledPackage> = std_pkgs.iter().collect();
    let (cp, unit_diags) = compile_unit(name, std_pkgs.len(), modules, &deps);
    diags.extend(unit_diags);

    let linked = Linker::link(std_pkgs.into_iter().chain(std::iter::once(cp)).collect());
    (linked.program, diags)
}

fn rich_to_diag(src: &Source, e: &Rich<'_, Token, Span>) -> Diagnostic {
    meadow_compiler::diagnostics::from_parse_error(&src.name().to_string(), e)
}
