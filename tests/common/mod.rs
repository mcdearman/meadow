//! Shared helpers for the integration tests.
//!
//! Everything here returns **deterministic** strings: rendered type schemes,
//! diagnostic messages, `Value` display, or the var-normalized core dump. Raw HIR
//! / `VarId`s are deliberately never snapshotted — the id counter is process-wide
//! and tests run in parallel, so those values are not stable.

#![allow(dead_code)]

use meadow::{
    lexer::tokenize,
    linker::Linker,
    parser,
    pipeline::{self, CompiledPackage},
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
    match meadow::eval::run(&program) {
        Ok(v) => v.to_string(),
        Err(e) => format!("{e}"),
    }
}

/// Evaluate a single expression by wrapping it in `def main = <expr>`.
pub fn eval_expr(expr: &str) -> String {
    eval_main(&format!("def main = {expr}\n"))
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
