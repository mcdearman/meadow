//! `meadow-setup` — the Windows installer for the Meadow toolchain.
//!
//! Download it from the releases page and double-click it. It puts `meadow.exe`
//! in `%USERPROFILE%\.meadow\bin`, adds that directory to your user `PATH`, and
//! waits for a keypress so you can read what happened before the window closes.
//!
//! It gets `meadow.exe` from whichever of these it finds first:
//!
//! 1. `--from <path>`, if you passed one.
//! 2. A `meadow.exe` sitting next to the installer — so the release zip can hold
//!    both and install with no network at all.
//! 3. The latest GitHub release, fetched with the `curl.exe` that ships with
//!    Windows 10 1803 and later.
//!
//! On macOS and Linux, `install.sh` does the same job.

// The shared installer: where things go, how a release is fetched, and how
// the bin directory joins the PATH. `meadowup` uses the same code.
use meadowup::{displace, path, scratch_dir, target_triple, valid_tag, version_of};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const REPO: &str = "mcdearman/meadow";
const EXE: &str = "meadow.exe";

fn main() -> ExitCode {
    // Work out whether to hold the window open *before* acting, so every exit
    // path — including a bad flag — honours `--no-pause`. A parse failure has no
    // flags to consult, so it falls back to pausing when we own the console.
    let (wants_pause, result) = match Args::parse() {
        Ok(args) => (args.pause, run(&args)),
        Err(msg) => (true, Err(format!("{msg}\ntry `meadow-setup --help`"))),
    };
    let pause = wants_pause && launched_by_double_click();

    match result {
        Ok(()) => finish(ExitCode::SUCCESS, pause),
        Err(e) => {
            eprintln!();
            eprintln!("error: {e}");
            finish(ExitCode::FAILURE, pause)
        }
    }
}

/// Hold the console open when there is nobody watching a shell prompt.
fn finish(code: ExitCode, pause: bool) -> ExitCode {
    if pause {
        println!();
        print!("Press Enter to close this window...");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    code
}

// ===========================================================================
// Arguments
// ===========================================================================

struct Args {
    home: PathBuf,
    from: Option<PathBuf>,
    version: Option<String>,
    modify_path: bool,
    uninstall: bool,
    pause: bool,
    help: bool,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let mut args = Args {
            home: meadowup::home(),
            from: None,
            version: None,
            modify_path: true,
            uninstall: false,
            pause: true,
            help: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => args.help = true,
                "--uninstall" => args.uninstall = true,
                "--no-modify-path" => args.modify_path = false,
                "--no-pause" => args.pause = false,
                "--dir" => {
                    let v = it.next().ok_or("--dir needs a path")?;
                    args.home = PathBuf::from(v);
                }
                "--from" => {
                    let v = it.next().ok_or("--from needs a path")?;
                    args.from = Some(PathBuf::from(v));
                }
                "--version" => {
                    let v = it
                        .next()
                        .ok_or("--version needs a tag, e.g. v0.1.0-alpha")?;
                    args.version = Some(v);
                }
                other => return Err(format!("unknown option: {other}")),
            }
        }
        Ok(args)
    }
}

fn print_help() {
    println!(
        "\
Install the Meadow toolchain.

USAGE:
    meadow-setup [OPTIONS]

OPTIONS:
        --dir <path>       Install directory (default: %USERPROFILE%\\.meadow)
        --from <path>      Install this meadow.exe instead of downloading one
        --version <tag>    Download a specific release (default: latest)
        --no-modify-path   Do not touch your PATH
        --no-pause         Do not wait for a keypress before exiting
        --uninstall        Remove meadow and its PATH entry
    -h, --help             Print this help"
    );
}

// ===========================================================================
// Install / uninstall
// ===========================================================================

fn run(args: &Args) -> Result<(), String> {
    if args.help {
        print_help();
        return Ok(());
    }
    let bin_dir = args.home.join("bin");

    if args.uninstall {
        return uninstall(&args.home, &bin_dir, args.modify_path);
    }

    println!("Installing Meadow to {}", args.home.display());
    println!();

    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("could not create {}: {e}", bin_dir.display()))?;

    let dest = bin_dir.join(EXE);
    let source = locate_binary(args)?;
    let backup = displace(&dest);
    match source {
        Source::Local(path) => {
            println!("  copying {}", path.display());
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        }
        Source::Downloaded(tmp) => {
            std::fs::rename(&tmp, &dest)
                .or_else(|_| std::fs::copy(&tmp, &dest).map(|_| ()))
                .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        }
    }
    // Now that the new binary is in place the old one is dead weight — but it may
    // still be locked by a running REPL, in which case leaving it is the right
    // answer and the next install will clear it.
    if let Some(backup) = backup {
        let _ = std::fs::remove_file(backup);
    }
    println!("  installed {}", dest.display());

    if args.modify_path {
        match path::add(&bin_dir) {
            Ok(true) => println!("  added {} to your PATH", bin_dir.display()),
            Ok(false) => println!("  {} is already on your PATH", bin_dir.display()),
            Err(e) => println!("  warning: could not update PATH: {e}"),
        }
    }

    println!();
    match version_of(&dest) {
        Some(v) => println!("Installed {v}"),
        None => println!("Installed meadow"),
    }
    println!();
    println!("  meadow                   start the REPL");
    println!("  meadow run <path>        build and run a package");
    println!("  meadow build --release   build with release checks");
    println!("  meadow fmt <path>        re-indent .mw sources");
    println!("  meadow test              run a package's @test functions");
    println!("  meadow lsp               the language server (editors start this)");
    println!();
    if args.modify_path {
        println!("Open a new terminal, then run `meadow`.");
    } else {
        println!("Add this to your PATH: {}", bin_dir.display());
    }
    Ok(())
}

fn uninstall(home: &Path, bin_dir: &Path, modify_path: bool) -> Result<(), String> {
    if !home.exists() {
        return Err(format!("meadow is not installed at {}", home.display()));
    }
    if modify_path {
        match path::remove(bin_dir) {
            Ok(true) => println!("Removed {} from your PATH", bin_dir.display()),
            Ok(false) => {}
            Err(e) => println!("warning: could not update PATH: {e}"),
        }
    }
    std::fs::remove_dir_all(home)
        .map_err(|e| format!("could not remove {}: {e}", home.display()))?;
    println!("Removed {}", home.display());
    Ok(())
}

enum Source {
    Local(PathBuf),
    Downloaded(PathBuf),
}

fn locate_binary(args: &Args) -> Result<Source, String> {
    if let Some(from) = &args.from {
        if !from.is_file() {
            return Err(format!("no such file: {}", from.display()));
        }
        return Ok(Source::Local(from.clone()));
    }

    // A meadow.exe shipped alongside the installer: install with no network.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join(EXE);
            if beside.is_file() {
                return Ok(Source::Local(beside));
            }
        }
    }

    download(args.version.as_deref()).map(Source::Downloaded)
}

/// Fetch the release archive with the bundled `curl.exe` and unpack `meadow.exe`
/// from it with `tar.exe` — both ship with Windows 10 1803 and later.
fn download(version: Option<&str>) -> Result<PathBuf, String> {
    let target = target_triple()?;
    let asset = format!("meadow-{target}.zip");
    let url = match version {
        Some(tag) => {
            if !valid_tag(tag) {
                return Err(format!("`{tag}` is not a release tag"));
            }
            format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
        }
        None => format!("https://github.com/{REPO}/releases/latest/download/{asset}"),
    };

    let tmp = scratch_dir("meadow-setup")?;
    let archive = tmp.join(&asset);

    println!("  downloading {url}");
    let out = std::process::Command::new("curl.exe")
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&url)
        .output()
        .map_err(|e| format!("could not run curl.exe: {e}"))?;
    if !out.status.success() {
        // Exit 22 is curl's "the server said no" — for us that means the asset is
        // missing, which the message below already explains. Anything else is a
        // real network or TLS problem, and curl's own words are the only clue.
        let detail = if out.status.code() == Some(22) {
            String::new()
        } else {
            format!("{}\n", String::from_utf8_lossy(&out.stderr).trim())
        };
        return Err(format!(
            "{detail}no prebuilt binary for {target} (looked for {asset}).\n\
             \n\
             Either put a meadow.exe next to this installer and run it again, or\n\
             build from source:\n\
             \n\
             \x20   git clone https://github.com/{REPO}.git\n\
             \x20   cd meadow\n\
             \x20   cargo install --path meadow"
        ));
    }

    // `tar.exe` handles zip archives on Windows.
    let status = std::process::Command::new("tar.exe")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()
        .map_err(|e| format!("could not run tar.exe: {e}"))?;
    if !status.success() {
        return Err(format!("could not unpack {asset}"));
    }

    let unpacked = tmp.join(EXE);
    if !unpacked.is_file() {
        return Err(format!("{asset} did not contain {EXE}"));
    }
    Ok(unpacked)
}

// ===========================================================================
// Console
// ===========================================================================

/// True when this process owns its console, i.e. it was double-clicked rather
/// than run from an existing shell — in which case the window would vanish along
/// with the output unless we hold it open.
#[cfg(windows)]
fn launched_by_double_click() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut pids = [0u32; 4];
    let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    count == 1
}

#[cfg(not(windows))]
fn launched_by_double_click() -> bool {
    false
}
