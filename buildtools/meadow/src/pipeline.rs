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
    Options,
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
pub fn build(entry: &Path, opts: Options) -> BuildOutput {
    build_with(entry, opts, None)
}

/// Text to add to the end of one module before it is compiled -- how a
/// debugger starts a program at a function instead of `main`: it appends a
/// definition that calls it, in the function's own module so that everything
/// the function can see, the definition can too.
pub struct Addition<'a> {
    /// The module's file.
    pub file: &'a Path,
    pub text: &'a str,
}

/// [`build`], with `addition` appended to one of the modules first.
///
/// Diagnostics from the added text point past the end of the file, which is
/// how a caller can tell them from the file's own.
pub fn build_with(entry: &Path, opts: Options, addition: Option<Addition<'_>>) -> BuildOutput {
    let mut graph = match PackageGraph::build(entry) {
        Ok(g) => g,
        Err(d) => {
            return BuildOutput {
                linked: None,
                diagnostics: vec![d],
            }
        }
    };

    // `Std` is embedded and injected below as an implicit dependency, so building
    // the tree on disk would declare every one of its names twice. Say that,
    // rather than emitting a hundred `already defined` errors that name the
    // symptom instead of the cause.
    if graph.packages.iter().any(|p| &*p.name == stdlib::PACKAGE_NAME) {
        return BuildOutput {
            linked: None,
            diagnostics: vec![Diagnostic {
                msg: "`Std` is embedded in the compiler and cannot be built as a package \
                      — its modules are compiled one per unit, which a package build \
                      cannot reproduce. Run `meadow test --std` to run its tests"
                    .to_string(),
                filename: stdlib::PACKAGE_NAME.to_string(),
                label: (String::new(), Span::from(0..0)),
                extra_labels: vec![],
            }],
        };
    }

    if let Some(add) = addition {
        let want = std::fs::canonicalize(add.file).unwrap_or_else(|_| add.file.to_path_buf());
        let module = graph
            .packages
            .iter_mut()
            .flat_map(|p| p.modules.iter_mut())
            .find(|m| match m.source.kind {
                SourceKind::File(name) => {
                    let p = Path::new(&*name);
                    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()) == want
                }
                SourceKind::Interactive => false,
            });
        let Some(module) = module else {
            return BuildOutput {
                linked: None,
                diagnostics: vec![Diagnostic {
                    msg: format!("{} is not a module of this package", add.file.display()),
                    filename: add.file.display().to_string(),
                    label: (String::new(), Span::from(0..0)),
                    extra_labels: vec![],
                }],
            };
        };
        let text = format!("{}\n{}\n", &*module.source.content, add.text);
        module.source = Source::new(module.source.kind, InternedString::from(text));
    }

    let (std_pkgs, mut diagnostics) = stdlib::std_packages(opts);

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
        let (cp, mut d) = compile_package(pkg, &deps, opts);
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
    opts: Options,
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
                source: m.source,
            });
        }
    }

    let (cp, unit_diags) = compile_unit(pkg.name, pkg.id, modules, deps, opts);
    diags.extend(unit_diags);
    (cp, diags)
}

/// Like [`compile_str`], but with the embedded `Std` package as a dependency (so
/// the prelude is in scope) and everything linked into one runnable
/// [`core::Program`]. For tests / experiments that want the standard library.
pub fn compile_str_with_std(
    name: &str,
    src: &str,
    opts: Options,
) -> (core::Program, Vec<Diagnostic>) {
    let name = InternedString::from(name);
    let source = Source::new(SourceKind::Interactive, InternedString::from(src));
    let lex = tokenize(source);
    let (std_pkgs, mut diags) = stdlib::std_packages(opts);
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
                source,
            }]
        })
        .unwrap_or_default();
    let deps: Vec<&CompiledPackage> = std_pkgs.iter().collect();
    let (cp, unit_diags) = compile_unit(name, std_pkgs.len(), modules, &deps, opts);
    diags.extend(unit_diags);

    let linked = Linker::link(std_pkgs.into_iter().chain(std::iter::once(cp)).collect());
    (linked.program, diags)
}

fn rich_to_diag(src: &Source, e: &Rich<'_, Token, Span>) -> Diagnostic {
    meadow_compiler::diagnostics::from_parse_error(&src.name().to_string(), e)
}
