use crate::pipeline::InputMode;
use clap::Parser;
use std::path::PathBuf;

mod ast;
mod diagnostics;
mod lexer;
mod parser;
mod pipeline;
mod rename;
mod repl;
mod span;
mod token;

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
        None => repl::run(),
    }
}

fn run_file(path: &std::path::Path) {
    let source = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading '{}': {e}", path.display());
        std::process::exit(1);
    });

    let filename = path.display().to_string();
    let pipeline = pipeline::Pipeline::new(&source, InputMode::File(filename));

    if let Err(e) = pipeline.run() {
        eprintln!("Runtime error: {e}");
        std::process::exit(1);
    }
}
