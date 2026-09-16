//! `meadowup` — installs and updates the Meadow toolchain.
//!
//! The division is rustup's, and for rustup's reasons:
//!
//! * **`meadowup`** looks after the *toolchain* — which version of Meadow is on
//!   this machine, and where. It is a program of its own with no Meadow crates
//!   behind it, because it has to work before Meadow is installed at all.
//! * **`meadow`** is the build system, the way `cargo` is. It builds packages
//!   and knows nothing about installing itself.
//!
//! ```text
//! meadowup install            put the latest Meadow on this machine
//! meadowup update             bring it, and meadowup itself, up to date
//! meadowup show               what is installed, and where
//! meadowup which              the path to the meadow binary
//! meadowup uninstall          remove it and undo the PATH entry
//! ```
//!
//! Everything lives under `$MEADOW_HOME` (`~/.meadow`), and `bin` inside it is
//! what joins the `PATH`.

use meadowup::{bin_dir, exe_name, home, path, release, up_name, version_of};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!();
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    /// A release tag, rather than the newest.
    version: Option<String>,
    /// Re-install even when this version is already here.
    force: bool,
    /// Leave shell profiles and the registry alone.
    modify_path: bool,
    home: PathBuf,
}

fn run(argv: &[String]) -> Result<(), String> {
    let (command, rest) = match argv.split_first() {
        None => {
            help();
            return Ok(());
        }
        Some((c, rest)) => (c.as_str(), rest),
    };
    let args = parse(rest)?;
    match command {
        "install" => install(&args, false),
        "update" => install(&args, true),
        "show" => show(&args),
        "which" => which(&args),
        "uninstall" => uninstall(&args),
        "help" | "--help" | "-h" => {
            help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("meadowup {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => Err(format!(
            "`{other}` is not a meadowup command. Try `meadowup help`."
        )),
    }
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        version: None,
        force: false,
        modify_path: true,
        home: home(),
    };
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => {
                args.version = Some(
                    it.next()
                        .ok_or("`--version` needs a tag, e.g. `--version v0.1.0-alpha`")?
                        .clone(),
                );
            }
            "--force" => args.force = true,
            "--no-modify-path" => args.modify_path = false,
            other => return Err(format!("`{other}` is not an option here")),
        }
    }
    Ok(args)
}

/// Put the toolchain in place.
///
/// `updating` only changes what is *said*: installing and updating do the same
/// thing, which is to make the newest release the one that is here.
fn install(args: &Args, updating: bool) -> Result<(), String> {
    let target = meadowup::target_triple()?;
    let bin = bin_dir(&args.home);
    let meadow = bin.join(exe_name());

    let tag = match &args.version {
        Some(t) => t.clone(),
        None => {
            println!("  Checking for the latest release");
            release::latest_tag()?
        }
    };

    // Already this version: say so rather than downloading it again.
    if !args.force
        && let Some(have) = version_of(&meadow)
        && have.split_whitespace().nth(1) == Some(tag.trim_start_matches('v'))
    {
        println!("  Unchanged {have} is already installed");
        println!("            use `--force` to install it again");
        return Ok(());
    }

    println!(
        "{} {tag} for {target}",
        if updating { " Updating" } else { "Installing" }
    );
    let unpacked = release::fetch(&tag, target, "meadowup-install")?;

    std::fs::create_dir_all(&bin).map_err(|e| format!("could not make {}: {e}", bin.display()))?;

    // The build tool, and this program: a release carries both, and leaving
    // meadowup behind would strand the machine on a version that cannot update
    // itself.
    let put = |name: &str, dest: &Path| -> Result<bool, String> {
        let Some(found) = release::find(&unpacked, name) else {
            return Ok(false);
        };
        let backup = meadowup::displace(dest);
        std::fs::copy(&found, dest)
            .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        make_runnable(dest);
        // The displaced image may still be locked by a running process, in
        // which case leaving it is right and the next install clears it.
        if let Some(backup) = backup {
            let _ = std::fs::remove_file(backup);
        }
        println!("  Installed {}", dest.display());
        Ok(true)
    };

    if !put(exe_name(), &meadow)? {
        return Err(format!(
            "the {tag} archive holds no {}, so there is nothing to install",
            exe_name()
        ));
    }
    // Absent from older releases, which is not an error: those simply predate
    // meadowup, and the next release will carry it.
    let _ = put(up_name(), &bin.join(up_name()))?;
    let _ = std::fs::remove_dir_all(&unpacked);

    if args.modify_path {
        match path::add(&bin) {
            Ok(true) => println!("  Added {} to your PATH", bin.display()),
            Ok(false) => println!("  Unchanged {} is already on your PATH", bin.display()),
            Err(e) => println!("  warning: could not update PATH: {e}"),
        }
    }

    println!();
    match version_of(&meadow) {
        Some(v) => println!("Installed {v}"),
        None => println!("Installed {tag}"),
    }
    println!();
    println!("  meadow                   start the REPL");
    println!("  meadow run <path>        build and run a package");
    println!("  meadow add <url>         add a dependency");
    println!("  meadow test              run a package's @test functions");
    println!("  meadowup update          bring the toolchain up to date");
    println!();
    if args.modify_path {
        if cfg!(windows) {
            println!("Open a new terminal, then run `meadow`.");
        } else {
            println!(
                "Open a new shell, or run:  . \"{}\"",
                meadowup::env_file(&args.home).display()
            );
        }
    } else {
        println!("Add this to your PATH: {}", bin.display());
    }
    Ok(())
}

fn show(args: &Args) -> Result<(), String> {
    let bin = bin_dir(&args.home);
    println!("home       {}", args.home.display());
    println!("binaries   {}", bin.display());
    println!(
        "target     {}",
        meadowup::target_triple().unwrap_or("unknown")
    );
    println!();
    for name in [exe_name(), up_name()] {
        let at = bin.join(name);
        match version_of(&at) {
            Some(v) => println!("{v}"),
            None if at.exists() => println!("{name} (would not say its version)"),
            None => println!("{name} is not installed"),
        }
    }
    Ok(())
}

fn which(args: &Args) -> Result<(), String> {
    let at = bin_dir(&args.home).join(exe_name());
    if !at.exists() {
        return Err(format!(
            "meadow is not installed at {}. Run `meadowup install`.",
            at.display()
        ));
    }
    println!("{}", at.display());
    Ok(())
}

fn uninstall(args: &Args) -> Result<(), String> {
    if !args.home.exists() {
        return Err(format!(
            "meadow is not installed at {}",
            args.home.display()
        ));
    }
    if args.modify_path {
        match path::remove(&bin_dir(&args.home)) {
            Ok(true) => println!("  Removed the PATH entry"),
            Ok(false) => println!("  Unchanged there was no PATH entry"),
            Err(e) => println!("  warning: could not update PATH: {e}"),
        }
    }
    std::fs::remove_dir_all(&args.home)
        .map_err(|e| format!("could not remove {}: {e}", args.home.display()))?;
    println!("  Removed {}", args.home.display());
    println!();
    println!("Meadow is uninstalled. Open a new shell for the PATH change to take.");
    Ok(())
}

/// Mark a freshly written binary executable, which matters everywhere but
/// Windows.
#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn make_runnable(_path: &Path) {}

fn help() {
    println!(
        "meadowup {} — installs and updates Meadow",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("USAGE");
    println!("  meadowup <command> [options]");
    println!();
    println!("COMMANDS");
    println!("  install      put the latest Meadow on this machine");
    println!("  update       bring Meadow, and meadowup itself, up to date");
    println!("  show         what is installed, and where");
    println!("  which        the path to the meadow binary");
    println!("  uninstall    remove Meadow and undo the PATH entry");
    println!();
    println!("OPTIONS");
    println!("  --version <TAG>      a particular release, not the newest");
    println!("  --force              install again even if it is already here");
    println!("  --no-modify-path     leave shell profiles and the registry alone");
    println!();
    println!("`meadow` itself is the build system: `meadow build`, `meadow run`,");
    println!("`meadow add`, `meadow update`. This program only looks after which");
    println!("version of it you have.");
    println!();
    println!(
        "MEADOW_HOME overrides where everything goes (now: {}).",
        home().display()
    );
}
