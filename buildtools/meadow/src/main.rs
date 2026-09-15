//! The `meadow` command-line entry point. The real work lives in this crate's
//! library (`lib.rs`) plus the `meadow-compiler` and `meadow-eval` crates; this
//! file only parses arguments, wires things together, and prints results.

mod repl;

use clap::{Parser, Subcommand};
use meadow::{
    Backend, Engine, OptLevel, Profile, Resolved, Strictness, aot, artifacts, format, init,
    package::ProfileConfig,
    pipeline, runtime, test, update,
    workspace::{Selected, Selection},
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
        /// Package directory (or a single `.mw` file), or a workspace.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Also print every node's inferred type.
        #[arg(long)]
        annotations: bool,
        #[command(flatten)]
        packages: PackageArgs,
        #[command(flatten)]
        profile: ProfileArgs,
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Build a package, then evaluate its `main` entry point.
    Run {
        /// Package directory (or a single `.mw` file), or a workspace.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// In a workspace: the member to run.
        #[arg(short = 'p', long = "package", value_name = "NAME")]
        package: Option<String>,
        #[command(flatten)]
        profile: ProfileArgs,
        #[command(flatten)]
        engine: EngineArgs,
        /// After running, report what the garbage collector did (VM only): how
        /// many collections, how long they took, and how much they copied.
        #[arg(long)]
        gc_stats: bool,
        /// Which collector the VM uses: `generational` (the default: a nursery,
        /// and an old generation marked concurrently, for short pauses) or
        /// `copying` (one space, copied whole). `MEADOW_GC` sets the same.
        #[arg(long, value_parser = ["generational", "copying"])]
        gc: Option<String>,
        #[command(flatten)]
        target: TargetArgs,
        /// Arguments for the program, after `--`: what `Process.argv` gives
        /// it. `meadow run . -- in.mw -o out` passes `in.mw -o out`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Run a bytecode image: what `meadow build` writes under
    /// `target/<profile>/bytecode/`, or what a compiler written in Meadow
    /// emits.
    Exec {
        /// The `.mbc` file.
        image: PathBuf,
        /// `vm` (the interpreter alone) or `jit` (the default). For an
        /// executable, `meadow link` the image.
        #[arg(long, value_name = "BACKEND", value_parser = backend)]
        backend: Option<Backend>,
        /// Optimization level for the JIT: 0, 1 or 2.
        #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", value_parser = opt_level)]
        opt: Option<OptLevel>,
        /// Arguments for the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Compile a bytecode image to machine code and link it into an
    /// executable.
    Link {
        /// The `.mbc` file.
        image: PathBuf,
        /// The executable to write. The image's name beside it, by default.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// Optimization level: 0, 1 or 2 (the default).
        #[arg(short = 'O', long = "opt-level", value_name = "LEVEL", value_parser = opt_level)]
        opt: Option<OptLevel>,
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Build a package and run its `@test` functions.
    Test {
        /// Package directory (or a single `.mw` file).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only run tests whose name contains this.
        filter: Option<String>,
        /// Only the test whose name is exactly FILTER -- `Module.test`, or the
        /// bare name in the root module.
        #[arg(long, requires = "filter")]
        exact: bool,
        /// Also run the standard library's own tests.
        #[arg(long)]
        std: bool,
        #[command(flatten)]
        packages: PackageArgs,
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
        packages: PackageArgs,
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
    /// Run the debug adapter, speaking the Debug Adapter Protocol over stdin
    /// and stdout.
    ///
    /// Editors start this when you debug a Meadow program; there is no reason
    /// to run it by hand.
    Dap,
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
    /// Create a package: a `meadow.toml` and a `src/Main.mw` that runs.
    ///
    /// Inside a workspace, the new package is added to its `members`.
    Init {
        /// Where to put it, created if it does not exist.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// The package's name. Defaults to the directory's.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Create a workspace instead: a `meadow.toml` with an empty
        /// `[workspace]`, for packages made inside it to join.
        #[arg(long, conflicts_with = "name")]
        workspace: bool,
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

/// `-p`, `--workspace` and `--exclude` -- which members of a workspace a
/// command means.
#[derive(clap::Args)]
struct PackageArgs {
    /// In a workspace: the member to use, by name. Repeat it for more.
    #[arg(short = 'p', long = "package", value_name = "NAME")]
    package: Vec<String>,
    /// Every member of the workspace.
    #[arg(long, visible_alias = "all", conflicts_with = "package")]
    workspace: bool,
    /// With `--workspace`: leave this member out. Repeat it for more.
    #[arg(long, value_name = "NAME", requires = "workspace")]
    exclude: Vec<String>,
}

impl PackageArgs {
    fn selection(&self) -> Selection {
        Selection {
            packages: self.package.clone(),
            workspace: self.workspace,
            exclude: self.exclude.clone(),
        }
    }
}

/// What `selection` means at `path`, exiting with the reason when it means
/// nothing -- after warning about any member profile the workspace ignores.
fn select(selection: &Selection, path: &std::path::Path) -> Selected {
    let selected = selection.select(path).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    if let Some(ws) = &selected.workspace {
        for m in ws.ignored_profiles(&selected.paths) {
            eprintln!(
                "warning: the `[profile]` sections of `{}` are ignored: a workspace member \
                 builds with the profiles in {}",
                m.name,
                meadow::workspace::shown(&ws.root.join("meadow.toml"))
            );
        }
    }
    selected
}

/// `--target` -- what an `aot` build compiles for.
#[derive(clap::Args)]
struct TargetArgs {
    /// The architecture an `aot` build compiles for: `aarch64` or `x86_64`.
    /// The host's by default.
    #[arg(long, value_name = "ARCH")]
    target: Option<String>,
}

impl TargetArgs {
    fn target(&self) -> Result<aot::Target, String> {
        match &self.target {
            Some(name) => aot::Target::named(name),
            None => aot::Target::host(),
        }
    }
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
    /// How the program runs: `vm` (the bytecode interpreter), `jit` (which
    /// compiles what runs often to machine code as it goes) or `aot` (machine
    /// code compiled ahead of time, into an executable). Debug builds default
    /// to `jit` and release builds to `aot`; `backend = "..."` in a
    /// `[profile.<name>]` of `meadow.toml` overrides that, and this overrides
    /// both.
    #[arg(long, value_name = "BACKEND", value_parser = backend, conflicts_with_all = ["jit", "aot"])]
    backend: Option<Backend>,
    /// `--backend jit`.
    #[arg(long, conflicts_with = "aot")]
    jit: bool,
    /// `--backend aot`.
    #[arg(long, visible_alias = "native")]
    aot: bool,
    /// Turn on a flag for `@cfg(…)` to test: `--cfg fast`, `--cfg feature=gpu`.
    /// Repeat it for more; a manifest's `cfg = "…"` adds to these.
    #[arg(long = "cfg", value_name = "FLAG")]
    cfg: Vec<String>,
    /// Compile every definition of the package and its dependencies, not only
    /// what `main` reaches. `prune = false` in a `[profile.<name>]` of
    /// `meadow.toml` does the same.
    #[arg(long)]
    no_prune: bool,
}

fn backend(s: &str) -> Result<Backend, String> {
    Backend::parse(s).ok_or_else(|| format!("expected vm, jit or aot, got `{s}`"))
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
            backend: self
                .backend
                .or(self.jit.then_some(Backend::Jit))
                .or(self.aot.then_some(Backend::Aot)),
            prune: self.no_prune.then_some(false),
            cfg: (!self.cfg.is_empty())
                .then(|| meadow_compiler::intern::InternedString::from(self.cfg.join(","))),
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
/// The VM, with its JIT, is the default. The CEK is the specification of what a Meadow program
/// means, so if the two disagree it is right and the VM has a bug; this flag is
/// what makes that comparison available without a rebuild.
#[derive(clap::Args)]
struct EngineArgs {
    /// Evaluate with the CEK machine rather than the bytecode VM.
    #[arg(long, conflicts_with_all = ["jit", "aot", "backend"])]
    cek: bool,
}

impl EngineArgs {
    /// `profile`, told whether the CEK machine runs the program -- which
    /// `@cfg(backend = "cek")` asks.
    fn resolve(&self, mut profile: Resolved) -> Resolved {
        if self.cek {
            profile.options.cfg.backend = "cek";
        }
        profile
    }

    /// The machine that runs a program in this process, for `backend`. An
    /// `aot` backend's native code runs in a process of its own, so here --
    /// where tests run -- it is the JIT's.
    fn engine(&self, backend: Backend) -> Engine {
        if self.cek {
            return Engine::Cek;
        }
        match backend {
            Backend::Vm => Engine::Vm,
            Backend::Jit | Backend::Aot => Engine::Jit,
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
            packages,
            profile,
            target,
        }) => {
            let selected = select(&packages.selection(), &path);
            let profile = profile.resolve(&path);
            match &selected.paths[..] {
                [one] => build(one, None, annotations, false, profile, &target),
                many => build_many(many, annotations, profile, &target),
            }
        }
        Some(Cmd::Run {
            path,
            package,
            profile,
            engine,
            gc_stats,
            gc,
            target,
            args,
        }) => {
            meadow_compiler::core::args::set(args);
            if let Some(gc) = gc {
                meadow_rts::heap::configure(meadow_rts::heap::GcConfig {
                    collector: match gc.as_str() {
                        "copying" => meadow_rts::heap::Collector::Copying,
                        _ => meadow_rts::heap::Collector::Generational,
                    },
                    ..meadow_rts::heap::GcConfig::from_env()
                });
            }
            let selection = Selection {
                packages: package.into_iter().collect(),
                ..Selection::default()
            };
            let selected = select(&selection, &path);
            let one = selected.one("run").unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
            let profile = engine.resolve(profile.resolve(&path));
            build(
                one,
                Some(engine.engine(profile.backend)),
                false,
                gc_stats,
                profile,
                &target,
            )
        }
        Some(Cmd::Exec {
            image,
            backend,
            opt,
            args,
        }) => {
            meadow_compiler::core::args::set(args);
            exec(
                &image,
                backend.unwrap_or(Backend::Jit),
                opt.unwrap_or(OptLevel::O1),
            );
        }
        Some(Cmd::Link {
            image,
            output,
            opt,
            target,
        }) => link(&image, output, opt.unwrap_or(OptLevel::O2), &target),
        Some(Cmd::Dis {
            path,
            packages,
            profile,
        }) => {
            let selected = select(&packages.selection(), &path);
            disassemble(&selected.paths, profile.resolve(&path))
        }
        Some(Cmd::Test {
            path,
            filter,
            exact,
            std,
            packages,
            profile,
            engine,
        }) => match test::run(&test::Options {
            packages: packages.selection(),
            engine: engine.engine(profile.resolve(&path).backend),
            profile: engine.resolve(profile.resolve(&path)),
            path,
            filter,
            exact,
            std,
        }) {
            Ok(true) => {}
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some(Cmd::Dap) => {
            if let Err(e) = meadow::dap::run() {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
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
            if let Err(e) = meadow_lsp::server::run(
                packages,
                modules,
                src_root,
                Some(meadow::editor::find_package),
            ) {
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
        Some(Cmd::Init {
            path,
            name,
            workspace,
        }) => match init::run(&init::Options {
            path,
            name,
            workspace,
        }) {
            Ok(made) => {
                if workspace {
                    println!("created a workspace at {}", made.root.display());
                } else {
                    println!("created package `{}` at {}", made.name, made.root.display());
                }
                for f in &made.files {
                    println!("  {}", nearby(f).display());
                }
                if let Some(ws) = &made.joined {
                    println!(
                        "added `{}` to the members of {}",
                        made.name,
                        nearby(ws).display()
                    );
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some(Cmd::Update { version, force }) => {
            if let Err(e) = update::run(&update::Options { version, force }) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// `path` as seen from the current directory, when it is inside it: `init`
/// finds a workspace by its full path, and says so more briefly.
fn nearby(path: &std::path::Path) -> PathBuf {
    let here = std::env::current_dir()
        .ok()
        .and_then(|d| std::fs::canonicalize(d).ok());
    let full = std::fs::canonicalize(path).ok();
    match (here, full) {
        (Some(here), Some(full)) => match full.strip_prefix(&here) {
            Ok(rest) if rest.as_os_str().is_empty() => PathBuf::from("."),
            Ok(rest) => rest.to_path_buf(),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

/// Discover, compile and link the package at `path`; with `engine`, also
/// evaluate its entry point. Exits non-zero if any diagnostic was produced or
/// evaluation failed.
fn build(
    path: &std::path::Path,
    engine: Option<Engine>,
    annotations: bool,
    gc_stats: bool,
    profile: Resolved,
    target: &TargetArgs,
) {
    let profile = for_target(profile, target);
    let out = pipeline::build(path, profile.options);

    for d in &out.diagnostics {
        eprintln!("{}: {}", d.filename, d.msg);
    }

    let Some(linked) = out.linked else {
        std::process::exit(1);
    };
    // A program with errors is not run, and nothing is written for it: what
    // it would do is not what was written. What was worked out is still worth
    // showing a build.
    if !out.diagnostics.is_empty() {
        if engine.is_none() {
            print!("{}", linked.dump());
            if annotations {
                print!("{}", linked.annotations());
            }
        }
        std::process::exit(1);
    }
    finish(
        linked,
        &out.package,
        engine,
        annotations,
        gc_stats,
        profile,
        target,
    );
}

/// `meadow exec`: run the image at `path` on `backend`.
fn exec(path: &std::path::Path, backend: Backend, opt: OptLevel) {
    let image = read_image(path);
    let engine = match backend {
        Backend::Vm => Engine::Vm,
        Backend::Jit => Engine::Jit,
        Backend::Aot => {
            eprintln!(
                "error: an image runs on `vm` or `jit`; `meadow link` makes it an executable"
            );
            std::process::exit(1);
        }
    };
    let result = match runtime::native(&image, engine, opt) {
        Ok(jit) => runtime::run_image_with_stats(&image, jit.as_ref()).0,
        Err(e) => Err(e),
    };
    match result {
        Ok(value) => println!("=> {value}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// `meadow link`: the image at `path`, as an executable.
fn link(path: &std::path::Path, output: Option<PathBuf>, opt: OptLevel, target: &TargetArgs) {
    let image = read_image(path);
    let made = target.target().and_then(|target| {
        let exe = output.unwrap_or_else(|| {
            path.with_extension(target.format.exe_suffix().trim_start_matches('.'))
        });
        aot::link_image(&image, opt, target, &exe).map(|()| exe)
    });
    match made {
        Ok(exe) => eprintln!("native: {}", exe.display()),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn read_image(path: &std::path::Path) -> meadow_bytecode::Program {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("error: could not read {}: {e}", path.display());
        std::process::exit(1);
    });
    meadow_bytecode::image::decode(&bytes).unwrap_or_else(|e| {
        eprintln!("error: {}: {e}", path.display());
        std::process::exit(1);
    })
}

/// [`build`] for several packages of a workspace, sharing what they have in
/// common. Nothing runs: `run` takes one package.
fn build_many(paths: &[PathBuf], annotations: bool, profile: Resolved, target: &TargetArgs) {
    let profile = for_target(profile, target);
    let paths: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let out = pipeline::build_each(&paths, profile.options);
    for d in &out.diagnostics {
        eprintln!("{}: {}", d.filename, d.msg);
    }
    if out.each.is_empty() {
        std::process::exit(1);
    }
    if !out.diagnostics.is_empty() {
        for built in &out.each {
            let linked = built.linked.as_ref().expect("each is linked");
            print!("{}", linked.dump());
            if annotations {
                print!("{}", linked.annotations());
            }
        }
        std::process::exit(1);
    }
    for built in out.each {
        let linked = built.linked.expect("each is linked");
        finish(
            linked,
            &built.package,
            None,
            annotations,
            false,
            profile,
            target,
        );
    }
    if !out.diagnostics.is_empty() {
        std::process::exit(1);
    }
}

/// `profile`, told the architecture an executable for another is compiled
/// for: that is the `arch` its `@cfg(…)` sees.
fn for_target(mut profile: Resolved, target: &TargetArgs) -> Resolved {
    if profile.backend == Backend::Aot
        && let Ok(t) = target.target()
    {
        profile.options.cfg.arch = match t.arch {
            meadow_rts::codegen::Arch::Aarch64 => "aarch64",
            meadow_rts::codegen::Arch::X86_64 => "x86_64",
        };
    }
    profile
}

/// Everything [`build`] does once a package is linked: print it, write its
/// image and executable, and -- with `engine` -- run it.
fn finish(
    linked: meadow::linker::LinkedProgram,
    package: &Option<(PathBuf, meadow_compiler::intern::InternedString)>,
    engine: Option<Engine>,
    annotations: bool,
    gc_stats: bool,
    profile: Resolved,
    target: &TargetArgs,
) {
    print!("{}", linked.dump());
    if annotations {
        print!("{}", linked.annotations());
    }

    // What runs: the entry point and what it reaches, unless the profile says
    // to keep everything.
    let program = profile.program(&linked.program);

    // What the VM runs is written under the package's `target` directory
    // whenever it is made: by `build`, and by `run` on the VM.
    let image = match engine {
        None | Some(Engine::Vm) | Some(Engine::Jit) => {
            match runtime::compile(&program, profile.opt()) {
                Ok(image) => {
                    if let Some((root, name)) = package
                        && let Err(e) = artifacts::write_image(root, profile.profile, name, &image)
                    {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                    Some(image)
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Engine::Cek) => None,
    };

    // Machine code, linked into an executable -- and, for `run`, run: what an
    // `aot` backend is, unless `--cek` asked for the CEK machine.
    let aot = profile.backend == Backend::Aot && engine != Some(Engine::Cek);
    if let (true, Some(image)) = (aot, &image) {
        let exe = target.target().and_then(|target| {
            let (root, name) = package
                .as_ref()
                .ok_or("a native executable needs a package to put it in")?;
            aot::build(root, profile.profile, profile.opt(), name, image, target)
        });
        match exe {
            Ok(exe) if engine.is_none() => eprintln!("native: {}", exe.display()),
            Ok(exe) => {
                let status = std::process::Command::new(&exe)
                    .args(meadow_compiler::core::args::get())
                    .status()
                    .unwrap_or_else(|e| {
                        eprintln!("error: could not run {}: {e}", exe.display());
                        std::process::exit(1);
                    });
                std::process::exit(status.code().unwrap_or(1));
            }
            // A release build of a lone file, or on a machine that cannot link
            // one: the JIT runs it, unless `aot` was asked for by name.
            Err(e) if profile.fallback().is_some() => {
                if package.is_some() {
                    eprintln!("warning: no native executable: {e}");
                    if engine.is_some() {
                        eprintln!("note: running on the JIT instead; `--aot` makes this an error");
                    }
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }

    if let Some(engine) = engine {
        let (result, stats) = match &image {
            Some(image) => match runtime::native(image, engine, profile.opt()) {
                Ok(jit) => runtime::run_image_with_stats(image, jit.as_ref()),
                Err(e) => (Err(e), None),
            },
            None => runtime::run_with_stats(&program, engine, profile.opt()),
        };
        if gc_stats {
            match stats {
                Some(stats) => eprintln!("{stats}"),
                None => {
                    eprintln!("gc: the CEK machine reference-counts, so there is nothing to report")
                }
            }
        }
        match result {
            Ok(value) => println!("=> {value}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
}

/// Print the bytecode the VM would run — the back end's output, addresses and
/// all — for each package in `paths`.
fn disassemble(paths: &[PathBuf], profile: Resolved) {
    let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let out = pipeline::build_each(&refs, profile.options);
    for d in &out.diagnostics {
        eprintln!("{}: {}", d.filename, d.msg);
    }
    if out.each.is_empty() {
        std::process::exit(1);
    }
    let several = out.each.len() > 1;
    for built in out.each {
        let linked = built.linked.expect("each is linked");
        if several && let Some((_, name)) = &built.package {
            println!("=== {name} ===");
        }
        match runtime::compile(&profile.program(&linked.program), profile.opt()) {
            Ok(image) => print!("{}", image.disassemble()),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    if !out.diagnostics.is_empty() {
        std::process::exit(1);
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

    /// `init` takes the current directory when told nothing, which is the way
    /// it will usually be run.
    #[test]
    fn init_defaults_to_here() {
        let parsed = Cli::try_parse_from(["meadow", "init"]).expect("bare `init`");
        match parsed.cmd {
            Some(Cmd::Init { path, name, .. }) => {
                assert_eq!(path, PathBuf::from("."));
                assert_eq!(name, None);
            }
            other => panic!("expected `init`, got {:?}", other.is_some()),
        }

        let parsed = Cli::try_parse_from(["meadow", "init", "pkg", "--name", "myPkg"]).unwrap();
        match parsed.cmd {
            Some(Cmd::Init { path, name, .. }) => {
                assert_eq!(path, PathBuf::from("pkg"));
                assert_eq!(name.as_deref(), Some("myPkg"));
            }
            _ => panic!("expected `init`"),
        }
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
