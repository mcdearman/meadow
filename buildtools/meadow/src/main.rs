//! The `meadow` command-line entry point. The real work lives in this crate's
//! library (`lib.rs`) plus the `meadow-compiler` and `meadow-eval` crates; this
//! file only parses arguments, wires things together, and prints results.

mod repl;

use clap::{Parser, Subcommand};
use meadow::{
    format, package::ProfileConfig, pipeline, runtime, test, update, Engine, OptLevel, Profile,
    Resolved, Strictness,
};
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
        #[command(flatten)]
        engine: EngineArgs,
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
        #[command(flatten)]
        engine: EngineArgs,
    },
    /// Disassemble a package: the bytecode the VM would run.
    Dis {
        /// Package directory (or a single `.mw` file).
        #[arg(default_value = ".")]
        path: PathBuf,
        #[command(flatten)]
        profile: ProfileArgs,
    },
    /// Run the language server, speaking LSP over stdin and stdout.
    ///
    /// Editors start this; there is no reason to run it by hand.
    Lsp {
        /// Ignored: clients pass this to select the stdio transport, which is
        /// the only one spoken.
        ///
        /// It has to be *accepted* rather than merely ignored. `vscode-languageclient`
        /// appends `--stdio` to the command it was given, and refusing an unknown
        /// argument made the server exit with code 2 before reading a byte — which
        /// reached the user as `write EPIPE`, a message about the editor's failed
        /// write that says nothing about the argument it passed.
        #[arg(long)]
        stdio: bool,
        /// Ignored: some clients name their own process id so a server can exit
        /// when the editor goes away. This one exits when its input closes.
        #[arg(long = "clientProcessId", value_name = "PID")]
        client_process_id: Option<String>,
    },
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

/// `--release` / `--debug` — the build profile — plus the individual switches
/// that override whatever the profile chose.
///
/// Debug is the default: `-O1`, and no exhaustiveness check, so a half-written
/// `match` still runs. Release is `-O2` and strict. Either can be overridden
/// per switch, here or in the package's `meadow.toml`.
#[derive(clap::Args)]
struct ProfileArgs {
    /// Build with the release profile: `-O2`, and `match` must be exhaustive.
    #[arg(long, conflicts_with = "debug")]
    release: bool,
    /// Build with the debug profile (the default).
    #[arg(long)]
    debug: bool,
    /// Optimization level: 0, 1 or 2. Overrides the profile.
    #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", value_parser = opt_level)]
    opt: Option<OptLevel>,
    /// Reject a non-exhaustive `match`, whatever the profile says.
    #[arg(long, conflicts_with = "lenient")]
    strict: bool,
    /// Allow a non-exhaustive `match`, whatever the profile says.
    #[arg(long)]
    lenient: bool,
}

fn opt_level(s: &str) -> Result<OptLevel, String> {
    OptLevel::parse(s).ok_or_else(|| format!("expected 0, 1 or 2, got `{s}`"))
}

impl ProfileArgs {
    fn profile(&self) -> Profile {
        if self.release {
            Profile::Release
        } else {
            Profile::Debug
        }
    }

    /// The switches named on the command line, which win over everything.
    fn overrides(&self) -> ProfileConfig {
        ProfileConfig {
            opt: self.opt,
            strictness: match (self.strict, self.lenient) {
                (true, _) => Some(Strictness::Strict),
                (_, true) => Some(Strictness::Lenient),
                _ => None,
            },
        }
    }

    /// The profile, the package's `meadow.toml`, and these flags, in that order
    /// of increasing authority.
    fn resolve(&self, path: &std::path::Path) -> Resolved {
        Resolved::resolve(self.profile(), path, self.overrides())
    }
}

/// `--cek` — run on the CEK abstract machine instead of the bytecode VM.
///
/// The VM is the default. The CEK is the specification of what a Meadow program
/// means, so if the two disagree it is right and the VM has a bug; this flag is
/// what makes that comparison available without a rebuild. It is also the only
/// way to run a program that reaches the real world through an *unhandled*
/// `Fs`, `Process`, `Random` or `Time` operation, which the VM does not
/// discharge yet.
#[derive(clap::Args)]
struct EngineArgs {
    /// Evaluate with the CEK machine rather than the bytecode VM.
    #[arg(long)]
    cek: bool,
}

impl EngineArgs {
    fn engine(&self) -> Engine {
        if self.cek { Engine::Cek } else { Engine::Vm }
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
        }) => build(&path, None, annotations, profile.resolve(&path)),
        Some(Cmd::Run {
            path,
            profile,
            engine,
        }) => build(&path, Some(engine.engine()), false, profile.resolve(&path)),
        Some(Cmd::Dis { path, profile }) => disassemble(&path, profile.resolve(&path)),
        Some(Cmd::Test {
            path,
            filter,
            std,
            profile,
            engine,
        }) => match test::run(&test::Options {
            profile: profile.resolve(&path),
            path,
            filter,
            std,
            engine: engine.engine(),
        }) {
            Ok(true) => {}
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some(Cmd::Lsp { .. }) => {
            // Both shapes of the standard library: the bundle a package depends
            // on, and the modules it was bundled from — which is what lets a
            // `Std` source file be analysed as itself. One compile serves both.
            let (packages, _) = meadow::stdlib::std_packages(meadow::Options::debug());
            let (modules, _) = meadow::stdlib::std_modules(meadow::Options::debug());
            let modules = modules
                .into_iter()
                .map(|(dotted, pkg)| (dotted.to_string(), pkg))
                .collect();
            // And the sources on disk, so a definition inside `Std` is a file
            // the editor can open. Best-effort: without it, navigation stops at
            // the edge of the open document, which is where it used to stop
            // anyway.
            let src_root = meadow::stdlib::extract_sources();
            if let Err(e) = meadow_lsp::server::run(packages, modules, src_root) {
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

/// Discover, compile and link the package at `path`; with `engine`, also
/// evaluate its entry point. Exits non-zero if any diagnostic was produced or
/// evaluation failed.
fn build(path: &std::path::Path, engine: Option<Engine>, annotations: bool, profile: Resolved) {
    let out = pipeline::build(path, profile.options);

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

    if let Some(engine) = engine {
        match runtime::run(&linked.program, engine, profile.opt()) {
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

/// Print the bytecode the VM would run — the back end's output, addresses and
/// all.
fn disassemble(path: &std::path::Path, profile: Resolved) {
    let out = pipeline::build(path, profile.options);
    for d in &out.diagnostics {
        eprintln!("{}: {}", d.filename, d.msg);
    }
    let Some(linked) = out.linked else {
        std::process::exit(1);
    };
    match runtime::compile(&linked.program, profile.opt()) {
        Ok(image) => print!("{}", image.disassemble()),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// The arguments a language client actually launches us with.
    ///
    /// `vscode-languageclient` appends `--stdio` to the command it was given
    /// whenever the transport is stdio, and other clients add
    /// `--clientProcessId=<pid>`. Rejecting either made `meadow lsp` exit with
    /// code 2 before reading a byte, and the editor reported that as
    /// `write EPIPE` — a message about its own failed write, naming neither the
    /// argument nor the exit code. Nothing about the language server itself was
    /// wrong, which is why it took a log file to find.
    #[test]
    fn lsp_accepts_what_an_editor_passes() {
        for args in [
            vec!["meadow", "lsp"],
            vec!["meadow", "lsp", "--stdio"],
            vec!["meadow", "lsp", "--clientProcessId=1234"],
            vec!["meadow", "lsp", "--stdio", "--clientProcessId=1234"],
        ] {
            let parsed = Cli::try_parse_from(&args);
            assert!(
                parsed.is_ok(),
                "`{}` should start the server, got {}",
                args.join(" "),
                parsed.err().map(|e| e.to_string()).unwrap_or_default()
            );
            assert!(matches!(parsed.unwrap().cmd, Some(Cmd::Lsp { .. })));
        }
    }

    #[test]
    fn an_argument_we_do_not_know_is_still_rejected() {
        // Tolerating the two above is deliberate, not a blanket `allow_hyphen_values`.
        assert!(Cli::try_parse_from(["meadow", "lsp", "--socket=9257"]).is_err());
    }
}
