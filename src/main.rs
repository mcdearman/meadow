#![allow(dead_code)] // the IR / driver surface is intentionally ahead of its first consumer

use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod ast;
mod core;
mod diagnostics;
mod eval;
mod hir;
mod infer;
mod intern;
mod lexer;
mod linker;
mod package;
mod parser;
mod pipeline;
mod rename;
mod repl;
mod source;
mod span;

#[derive(Parser)]
#[command(name = "meadow", about = "Meadow language compiler")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Type-check and link a package, printing the annotated result.
    Build {
        /// Package directory (or a single `.mw` file).
        path: PathBuf,
        /// Also print every node's inferred type.
        #[arg(long)]
        annotations: bool,
    },
    /// Build a package, then evaluate its `main` entry point.
    Run {
        path: PathBuf,
    },
}

fn main() {
    match Cli::parse().cmd {
        None => repl::Session::new().run(),
        Some(Cmd::Build { path, annotations }) => build(&path, false, annotations),
        Some(Cmd::Run { path }) => build(&path, true, false),
    }
}

fn build(path: &std::path::Path, run: bool, annotations: bool) {
    let out = pipeline::build(path);

    for d in &out.diagnostics {
        eprintln!("{}: {}", d.filename, d.msg);
    }

    let Some(linked) = out.linked else {
        std::process::exit(1);
    };

    print!("{}", linked.dump());
    if annotations {
        print!("{}", linked.annotations());
    }

    if run {
        match eval::run(&linked.program) {
            Ok(value) => println!("=> {value}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }

    if !out.diagnostics.is_empty() {
        std::process::exit(1);
    }
}
