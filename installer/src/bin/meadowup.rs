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

use meadowup::ui::{self, bold};
use meadowup::{Provenance, bin_dir, exe_name, home, path, release, up_name, version_of};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Double-clicked with no arguments is someone who downloaded this to
    // install Meadow, so that is what it does -- and the window is held open
    // afterwards, or everything printed would vanish with it.
    let clicked = meadowup::launched_by_double_click();
    let argv = if clicked && argv.is_empty() {
        vec!["install".to_string()]
    } else {
        argv
    };

    let code = match run(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ui::error(e);
            ExitCode::FAILURE
        }
    };
    if clicked {
        meadowup::wait_for_enter();
    }
    code
}

struct Args {
    /// A release tag, rather than the newest.
    version: Option<String>,
    /// Re-install even when this version is already here.
    force: bool,
    /// Leave shell profiles and the registry alone.
    modify_path: bool,
    home: PathBuf,
    /// Take the binaries from this directory instead of fetching a release:
    /// what `install.sh` passes after building them from source.
    from: Option<PathBuf>,
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
        from: None,
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
            "--from" => {
                args.from = Some(PathBuf::from(
                    it.next()
                        .ok_or("`--from` needs a directory holding the binaries")?,
                ));
            }
            other => return Err(format!("`{other}` is not an option here")),
        }
    }
    Ok(args)
}

/// The version in what `meadow --version` says: `0.1.0-alpha` of
/// `meadow 0.1.0-alpha`.
fn number(said: &str) -> &str {
    said.split_whitespace().nth(1).unwrap_or(said)
}

/// Put the toolchain in place.
///
/// `updating` only changes what is *said*: installing and updating do the same
/// thing, which is to make the newest release the one that is here.
///
/// What it says follows rustup: what it is about to do, the download as a
/// progress bar, each component as it goes in, and a line at the end saying
/// what changed.
fn install(args: &Args, updating: bool) -> Result<(), String> {
    let bin = bin_dir(&args.home);
    let meadow = bin.join(exe_name());
    let before = version_of(&meadow);

    // `--from` is a directory that already holds the binaries -- built from
    // source, or an unpacked archive. Nothing is looked up and nothing is
    // fetched, so this is the offline install too.
    let (unpacked, fetched, provenance) = match &args.from {
        Some(dir) => {
            if !dir.is_dir() {
                return Err(format!("{} is not a directory", dir.display()));
            }
            ui::info(format!("installing from {}", dir.display()));
            (dir.clone(), false, Provenance::Local)
        }
        None => {
            let target = meadowup::target_triple()?;
            let tag = match &args.version {
                Some(t) => t.clone(),
                None => {
                    ui::info("checking for the latest release");
                    let tag = release::latest_tag()?;
                    ui::info(format!("latest release is {}", bold(&tag)));
                    tag
                }
            };
            let wanted = tag.trim_start_matches('v');
            // Already this version: say so rather than downloading it again.
            if !args.force
                && let Some(have) = &before
                && number(have) == wanted
            {
                println!();
                println!("  {} unchanged - {have}", bold("meadow"));
                // A build from source shares its version with the release it
                // followed, so "unchanged" alone would hide which is here.
                if meadowup::provenance(&args.home) == Some(Provenance::Local) {
                    println!();
                    ui::info(format!(
                        "this is a local build, not the {tag} release; \
                         `meadowup update --force` replaces it with the release"
                    ));
                }
                println!();
                return Ok(());
            }
            match &before {
                Some(have) => ui::info(format!(
                    "{} meadow {} -> {}",
                    if updating { "updating" } else { "replacing" },
                    number(have),
                    bold(wanted)
                )),
                None => ui::info(format!("installing meadow {}", bold(wanted))),
            }
            ui::info(format!("downloading toolchain for {}", bold(target)));
            let (dir, _) = release::fetch(&tag, target, "meadowup-install")?;
            (dir, true, Provenance::Release(tag))
        }
    };

    std::fs::create_dir_all(&bin).map_err(|e| format!("could not make {}: {e}", bin.display()))?;

    // Each component the toolchain has, and where it goes. A release carries
    // both; leaving meadowup behind would strand the machine on a version that
    // cannot update itself.
    let put = |component: &str, file: &str, dest: &Path| -> Result<bool, String> {
        let Some(found) = release::find(&unpacked, file) else {
            return Ok(false);
        };
        let len = std::fs::metadata(&found).map(|m| m.len()).unwrap_or(0);
        ui::info(format!(
            "installing component '{}' ({})",
            bold(component),
            ui::size(len)
        ));
        let backup = meadowup::displace(dest);
        std::fs::copy(&found, dest)
            .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        make_runnable(dest);
        // The displaced image may still be locked by a running process, in
        // which case leaving it is right and the next install clears it.
        if let Some(backup) = backup {
            let _ = std::fs::remove_file(backup);
        }
        Ok(true)
    };

    if !put("meadow", exe_name(), &meadow)? {
        return Err(format!(
            "{} holds no {}, so there is nothing to install",
            unpacked.display(),
            exe_name()
        ));
    }
    // meadowup itself. The release's copy is preferred over the running one --
    // it is the one being installed, and on an update it is the newer. A
    // release that predates meadowup carries none, and then the program that is
    // running puts itself in place instead, which is what makes a downloaded
    // meadowup all anyone needs to fetch.
    if !put("meadowup", up_name(), &bin.join(up_name()))? {
        install_self(&bin)?;
    }
    // Only what was downloaded is cleared away; a `--from` directory is the
    // caller's, and removing it would take their build with it.
    if fetched {
        let _ = std::fs::remove_dir_all(&unpacked);
    }
    if let Err(e) = meadowup::record(&args.home, &provenance) {
        ui::warn(format!("could not record what was installed: {e}"));
    }

    if args.modify_path {
        match path::add(&bin) {
            Ok(true) => ui::info(format!("added {} to your PATH", bin.display())),
            Ok(false) => {}
            Err(e) => ui::warn(format!("could not update PATH: {e}")),
        }
    }

    // What changed, in one line, as rustup ends.
    let after = version_of(&meadow);
    let from = match &provenance {
        Provenance::Local => " (local build)",
        Provenance::Release(_) => "",
    };
    println!();
    match (&before, &after) {
        (Some(b), Some(a)) if number(b) != number(a) => {
            println!(
                "  {} updated - {a}{from} (from {})",
                bold("meadow"),
                number(b)
            )
        }
        (Some(_), Some(a)) => println!("  {} reinstalled - {a}{from}", bold("meadow")),
        (None, Some(a)) => println!("  {} installed - {a}{from}", bold("meadow")),
        (_, None) => println!("  {} installed", bold("meadow")),
    }
    println!();
    if before.is_none() {
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
    }
    Ok(())
}

/// Put the running program in `bin`, unless it is already the one there.
///
/// A downloaded `meadowup` installs itself first and the toolchain after, so
/// one download is all it takes -- and an update that goes on to replace this
/// file with a newer one simply overwrites what was just written.
fn install_self(bin: &Path) -> Result<(), String> {
    let Ok(running) = std::env::current_exe() else {
        return Ok(());
    };
    let dest = bin.join(up_name());
    // Already in place: `meadowup update` run from the installed copy.
    if std::fs::canonicalize(&running).ok() == std::fs::canonicalize(&dest).ok() {
        return Ok(());
    }
    let backup = meadowup::displace(&dest);
    std::fs::copy(&running, &dest)
        .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    make_runnable(&dest);
    if let Some(backup) = backup {
        let _ = std::fs::remove_file(backup);
    }
    ui::info(format!("installing component '{}'", bold("meadowup")));
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
    println!(
        "source     {}",
        match meadowup::provenance(&args.home) {
            Some(Provenance::Release(tag)) => format!("release {tag}"),
            Some(Provenance::Local) => "a local build".to_string(),
            None => "unknown".to_string(),
        }
    );
    println!();
    println!("{}", bold("installed components"));
    println!("--------------------");
    for (component, file) in [("meadow", exe_name()), ("meadowup", up_name())] {
        let at = bin.join(file);
        match version_of(&at) {
            Some(v) => println!("{v}"),
            None if at.exists() => println!("{component} (would not say its version)"),
            None => println!("{component} (not installed)"),
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
            Ok(true) => ui::info("removing the PATH entry"),
            Ok(false) => {}
            Err(e) => ui::warn(format!("could not update PATH: {e}")),
        }
    }
    for (component, file) in [("meadow", exe_name()), ("meadowup", up_name())] {
        if bin_dir(&args.home).join(file).exists() {
            ui::info(format!("removing component '{}'", bold(component)));
        }
    }
    std::fs::remove_dir_all(&args.home)
        .map_err(|e| format!("could not remove {}: {e}", args.home.display()))?;
    ui::info(format!("removing {}", args.home.display()));
    println!();
    println!("  {} uninstalled", bold("meadow"));
    println!();
    println!("Open a new shell for the PATH change to take.");
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
    println!("  --from <DIR>         take the binaries from here, fetching nothing");
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
