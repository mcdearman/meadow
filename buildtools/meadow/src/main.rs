//! The `meadow` command-line entry point. The real work lives in this crate's
//! library (`lib.rs`) plus the `meadow-compiler` and `meadow-eval` crates; this
//! file only parses arguments, wires things together, and prints results.

mod repl;

use clap::{Parser, Subcommand};
use meadow::{format, pipeline, test, update, Profile};
use meadow_eval as eval;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "meadow", about = "Meadow language compiler", version)]
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
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Build a package, then evaluate its `main` entry point.
    Run {
        path: PathBuf,
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Build a package and run its `@test` functions.
    Test {
        /// Package directory (or a single `.mw` file).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only run tests whose name contains this.
        filter: Option<String>,
        /// Also run the standard library's own tests.
        #[arg(long)]
        std: bool,
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Run the language server, speaking LSP over stdin and stdout.
    ///
    /// Editors start this; there is no reason to run it by hand.
    Lsp,
    /// Re-indent `.mw` sources in place.
    Fmt {
        /// Files or directories to format. Defaults to the current directory.
        paths: Vec<PathBuf>,
        /// List the files that would change and exit 1, without writing.
        #[arg(long)]
        check: bool,
        /// Write the result to stdout instead of back to the file.
        #[arg(long)]
        stdout: bool,
    },
    /// Replace this binary with the latest published release.
    Update {
        /// Install a specific release tag instead of the newest.
        #[arg(long, value_name = "TAG")]
        version: Option<String>,
        /// Re-install even if this is already the latest version.
        #[arg(long)]
        force: bool,
    },
}

/// `--release` / `--debug` — the build profile. Debug is the default: it skips
/// the exhaustiveness check so a half-written `match` still runs.
#[derive(clap::Args)]
#[group(multiple = false)]
struct ProfileArgs {
    /// Build with release checks (`match` must be exhaustive).
    #[arg(long)]
    release: bool,
    /// Build with debug checks (the default).
    #[arg(long)]
    debug: bool,
}

impl ProfileArgs {
    fn profile(&self) -> Profile {
        if self.release {
            Profile::Release
        } else {
            Profile::Debug
        }
    }
}

fn main() {
    match Cli::parse().cmd {
        // No subcommand → interactive REPL.
        None => repl::Session::new().run(),
        Some(Cmd::Build {
            path,
            annotations,
            profile,
        }) => build(&path, false, annotations, profile.profile()),
        Some(Cmd::Run { path, profile }) => build(&path, true, false, profile.profile()),
        Some(Cmd::Test {
            path,
            filter,
            std,
            profile,
        }) => match test::run(&test::Options {
            path,
            filter,
            std,
            profile: profile.profile(),
        }) {
            Ok(true) => {}
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some(Cmd::Lsp) => {
            let (packages, _) = meadow::stdlib::std_packages(meadow::Options::debug());
            if let Err(e) = meadow_lsp::server::run(packages) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some(Cmd::Fmt {
            paths,
            check,
            stdout,
        }) => {
            let paths = if paths.is_empty() {
                vec![PathBuf::from(".")]
            } else {
                paths
            };
            match format::run(&format::Options {
                paths,
                check,
                stdout,
            }) {
                // `--check` is for CI: a file that needs formatting is a failure.
                Ok(changed) => {
                    if check && changed > 0 {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Cmd::Update { version, force }) => {
            if let Err(e) = update::run(&update::Options { version, force }) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Discover, compile and link the package at `path`; optionally evaluate its
/// entry point. Exits non-zero if any diagnostic was produced or evaluation
/// failed.
fn build(path: &std::path::Path, run: bool, annotations: bool, profile: Profile) {
    let out = pipeline::build(path, profile.options());

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
