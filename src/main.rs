//! The `meadow` command-line entry point. All the real work lives in the library
//! crate (`lib.rs`); this file only parses arguments and prints results.

use clap::{Parser, Subcommand};
use meadow::{eval, pipeline, repl};
use std::path::PathBuf;

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
    Run { path: PathBuf },
}

fn main() {
    match Cli::parse().cmd {
        // No subcommand → interactive REPL.
        None => repl::Session::new().run(),
        Some(Cmd::Build { path, annotations }) => build(&path, false, annotations),
        Some(Cmd::Run { path }) => build(&path, true, false),
    }
}

/// Discover, compile and link the package at `path`; optionally evaluate its
/// entry point. Exits non-zero if any diagnostic was produced or evaluation
/// failed.
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
