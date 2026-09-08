//! Shared helpers for the integration tests.
//!
//! Everything here returns **deterministic** strings: rendered type schemes,
//! diagnostic messages, `Value` display, or the var-normalized core dump. Raw HIR
//! / `VarId`s are deliberately never snapshotted — the id counter is process-wide
//! and tests run in parallel, so those values are not stable.

#![allow(dead_code)]

use meadow::linker::Linker;
use meadow::pipeline::{self, CompiledPackage};
use meadow_compiler::{
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
};

/// Compile a source string; return the package and its diagnostic messages.
pub fn compile(src: &str) -> (CompiledPackage, Vec<String>) {
    let (cp, diags) = pipeline::compile_str("test", src);
    (cp, diags.iter().map(|d| d.msg.clone()).collect())
}

/// `name : scheme` for every top-level binding, in declaration order, followed by
/// any diagnostics as `!! message` lines.
pub fn schemes(src: &str) -> String {
    let (cp, diags) = compile(src);
    let mut out = String::new();
    for e in &cp.exports {
        out.push_str(&format!("{} : {}\n", e.name, e.scheme));
    }
    for d in diags {
        out.push_str(&format!("!! {d}\n"));
    }
    out
}

/// Just the diagnostic messages, newline-joined (for error-path tests).
pub fn errors(src: &str) -> String {
    compile(src).1.join("\n")
}

/// Compile `src` (which must define `main`), link, and evaluate the entry point.
pub fn eval_main(src: &str) -> String {
    let (cp, diags) = compile(src);
    if !diags.is_empty() {
        return format!("compile errors:\n{}", diags.join("\n"));
    }
    let program = Linker::link(vec![cp]).program;
    match meadow_eval::run(&program) {
        Ok(v) => v.to_string(),
        Err(e) => format!("{e}"),
    }
}

/// Evaluate a single expression by wrapping it in `def main = <expr>`.
pub fn eval_expr(expr: &str) -> String {
    eval_main(&format!("def main = {expr}\n"))
}

/// Like [`eval_main`], but links the embedded `Std` package so prelude names
/// (`map`, `lowMask`, `bytesGet`, …) and `Std.*` containers are in scope.
pub fn eval_main_std(src: &str) -> String {
    let (program, diags) = pipeline::compile_str_with_std("test", src);
    if !diags.is_empty() {
        return format!(
            "compile errors:\n{}",
            diags.iter().map(|d| d.msg.clone()).collect::<Vec<_>>().join("\n")
        );
    }
    match meadow_eval::run(&program) {
        Ok(v) => v.to_string(),
        Err(e) => format!("{e}"),
    }
}

/// Evaluate a single expression against `Std` (see [`eval_main_std`]).
pub fn eval_expr_std(expr: &str) -> String {
    eval_main_std(&format!("def main = {expr}\n"))
}

/// `name : scheme` for every top-level binding, compiled against `Std`.
pub fn schemes_std(src: &str) -> String {
    // `compile_str_with_std` only returns a linked `Program`, so re-run the
    // unit compile with the std package as a dep to recover export schemes.
    let (std_pkgs, _) = meadow::stdlib::compile_std();
    let std_refs: Vec<&CompiledPackage> = std_pkgs.iter().collect();
    let source = meadow_compiler::source::Source::new(
        meadow_compiler::source::SourceKind::Interactive,
        src.into(),
    );
    let lex = tokenize(source);
    let (ast, _) = parser::parse("test".into(), source, &lex.tokens);
    let modules = ast
        .map(|ast| {
            vec![meadow_compiler::AstModule {
                path: vec![],
                name: "test".into(),
                ast,
            }]
        })
        .unwrap_or_default();
    let (cp, diags) =
        meadow_compiler::compile_unit("test".into(), 1, modules, &std_refs);
    let mut out = String::new();
    for e in &cp.exports {
        out.push_str(&format!("{} : {}\n", e.name, e.scheme));
    }
    for d in diags {
        out.push_str(&format!("!! {}\n", d.msg));
    }
    out
}

/// The var-normalized core dump of `src`.
pub fn core_ir(src: &str) -> String {
    let (cp, _) = compile(src);
    Linker::link(vec![cp]).program.pretty()
}

/// Pretty-printed AST for `src` (deterministic — the AST carries no `VarId`s).
pub fn parse_ast(src: &str) -> String {
    let source = Source::new(SourceKind::Interactive, src.into());
    let lex = tokenize(source);
    let (ast, errs) = parser::parse("test".into(), source, &lex.tokens);
    match ast {
        Some(m) => format!("{:#?}", m.value),
        None => format!("parse failed: {errs:?}"),
    }
}
