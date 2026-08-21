use crate::source::*;
use clap::Parser;
use std::path::PathBuf;

mod ast;
mod diagnostics;
mod hir;
mod intern;
mod lexer;
mod parser;
mod pipeline;
mod rename;
mod repl;
mod session;
mod source;
mod span;

#[derive(Parser)]
#[command(name = "meadow", about = "Meadow language interpreter")]
struct Cli {
    /// Source file to run (omit to start the REPL)
    file: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();

    match cli.file {
        Some(path) => run_file(&path),
        None => {
            let mut repl = repl::Session::new(SourceKind::Interactive);
            repl.run();
        }
    }
}

fn run_file(path: &std::path::Path) {
    let source = Source::new(
        SourceKind::File(path.display().to_string().into()),
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| {
                eprintln!("Error reading '{}': {e}", path.display());
                std::process::exit(1);
            })
            .into(),
    );

    let mut pipeline = pipeline::Pipeline::new(source);

    if let Err(e) = pipeline.run() {
        eprintln!("Runtime error: {e}");
        std::process::exit(1);
    }
}
