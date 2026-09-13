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
            home: default_home(),
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
                    let v = it.next().ok_or("--version needs a tag, e.g. v0.1.0")?;
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

fn default_home() -> PathBuf {
    if let Ok(dir) = std::env::var("MEADOW_HOME") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&base).join(".meadow")
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

/// Move an existing install out of the way, returning where it went.
///
/// Windows will not let you write over a running executable, but it *will* let
/// you rename one — so an upgrade works even while a REPL is open in another
/// window, and that window keeps running the binary it started with.
fn displace(dest: &Path) -> Option<PathBuf> {
    let backup = backup_path(dest);
    // A leftover from an upgrade that could not clean up — either a previous run
    // of this installer, or a `meadow update` that could not delete its own
    // running image. Clear it whether or not we are about to make another.
    let _ = std::fs::remove_file(&backup);
    if !dest.exists() {
        return None;
    }
    std::fs::rename(dest, &backup).ok().map(|()| backup)
}

/// Where a displaced binary is parked. Must match `meadow::update::backup_path`,
/// so the installer and `meadow update` tidy up after each other: the name plus
/// `.old`, appended rather than replacing `.exe`.
fn backup_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
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

/// Whether `tag` is safe to put in a release URL.
///
/// It goes straight into the download path, so a `/` or a `..` walks out of this
/// repository's releases and fetches somebody else's binary — which this program
/// then installs and puts on `PATH`. Release tags look like `v1.2.3`.
fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        // `.` has to be allowed, for `v1.2.3` — which lets `..` through the
        // character check below, and `..` is the thing being guarded against.
        && !tag.contains("..")
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// A private directory to download and unpack into.
///
/// `create_dir`, not `create_dir_all`: the old path was `%TEMP%\meadow-setup-<pid>`,
/// which anyone able to write to `%TEMP%` could create first and have us unpack
/// into — and then install whatever they had left there. Failing when it already
/// exists is the point.
fn scratch_dir() -> Result<PathBuf, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "meadow-setup-{}-{nonce:08x}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
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

    let tmp = scratch_dir()?;
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

fn target_triple() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64-pc-windows-msvc"),
        "aarch64" => Ok("aarch64-pc-windows-msvc"),
        other => Err(format!("unsupported architecture: {other}")),
    }
}

fn version_of(exe: &Path) -> Option<String> {
    let out = std::process::Command::new(exe).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().map(|l| l.trim().to_string())
}

// ===========================================================================
// PATH
// ===========================================================================

#[cfg(windows)]
mod path {
    use std::path::Path;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegType};
    use winreg::{RegKey, RegValue};

    /// Add `dir` to the user `PATH`. `Ok(false)` if it was already there.
    ///
    /// The value is read and written **raw**: `PATH` is often a `REG_EXPAND_SZ`
    /// holding entries like `%JAVA_HOME%\bin`, and reading it through an API that
    /// expands those would bake the expansion in permanently.
    pub fn add(dir: &Path) -> Result<bool, String> {
        let dir = dir.to_string_lossy().to_string();
        let (value, kind) = read()?;
        if entries(&value).any(|e| eq_path(e, &dir)) {
            return Ok(false);
        }
        let mut updated: Vec<String> = entries(&value).map(str::to_string).collect();
        updated.push(dir);
        write(&updated.join(";"), kind)?;
        broadcast();
        Ok(true)
    }

    /// Remove `dir` from the user `PATH`. `Ok(false)` if it was not there — in
    /// which case the registry is left completely untouched.
    pub fn remove(dir: &Path) -> Result<bool, String> {
        let dir = dir.to_string_lossy().to_string();
        let (value, kind) = read()?;
        if !entries(&value).any(|e| eq_path(e, &dir)) {
            return Ok(false);
        }
        let kept: Vec<String> = entries(&value)
            .filter(|e| !eq_path(e, &dir))
            .map(str::to_string)
            .collect();
        write(&kept.join(";"), kind)?;
        broadcast();
        Ok(true)
    }

    pub(crate) fn entries(value: &str) -> impl Iterator<Item = &str> {
        value.split(';').filter(|e| !e.is_empty())
    }

    /// Windows paths are case-insensitive, and a trailing slash means nothing.
    pub(crate) fn eq_path(a: &str, b: &str) -> bool {
        let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
        norm(a) == norm(b)
    }

    fn env_key(write: bool) -> Result<RegKey, String> {
        let access = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", access)
            .map_err(|e| format!("could not open HKCU\\Environment: {e}"))
    }

    fn read() -> Result<(String, RegType), String> {
        let key = env_key(false)?;
        match key.get_raw_value("Path") {
            // `get_raw_value` does not expand `%VARS%`, which is what we want.
            Ok(raw) => {
                let text = String::from_utf16_lossy(&as_u16(&raw.bytes))
                    .trim_end_matches('\0')
                    .to_string();
                Ok((text, raw.vtype))
            }
            // No user PATH yet: create one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((String::new(), RegType::REG_EXPAND_SZ))
            }
            // Anything else is a PATH that exists and could not be read.
            // Carrying on as if it were empty would write `bin` back as the
            // user's entire PATH, so refuse, and leave the value as it is.
            Err(e) => Err(format!("could not read PATH: {e}")),
        }
    }

    fn write(value: &str, kind: RegType) -> Result<(), String> {
        // An entry with `%VARS%` only works from a REG_EXPAND_SZ value.
        let kind = if value.contains('%') {
            RegType::REG_EXPAND_SZ
        } else {
            kind
        };
        let mut bytes: Vec<u8> = Vec::with_capacity(value.len() * 2 + 2);
        for unit in value.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let key = env_key(true)?;
        key.set_raw_value("Path", &RegValue { bytes, vtype: kind })
            .map_err(|e| format!("could not write PATH: {e}"))
    }

    fn as_u16(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    /// Tell running programs the environment changed, so a newly opened terminal
    /// picks up the new `PATH` without a logout.
    fn broadcast() {
        use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        const HWND_BROADCAST: HWND = 0xffff as HWND;
        let env: Vec<u16> = "Environment".encode_utf16().chain(std::iter::once(0)).collect();
        let mut result = 0usize;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0 as WPARAM,
                env.as_ptr() as LPARAM,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }
}

#[cfg(not(windows))]
mod path {
    use std::path::Path;

    pub fn add(_dir: &Path) -> Result<bool, String> {
        Err("PATH editing is only implemented on Windows; use install.sh".into())
    }

    pub fn remove(_dir: &Path) -> Result<bool, String> {
        Err("PATH editing is only implemented on Windows; use install.sh".into())
    }
}

// ===========================================================================
// Console
// ===========================================================================

#[cfg(all(test, windows))]
mod tests {
    use super::path::{entries, eq_path};

    #[test]
    fn path_entries_skip_empties() {
        let got: Vec<&str> = entries(r"C:\a;;C:\b;").collect();
        assert_eq!(got, vec![r"C:\a", r"C:\b"]);
    }

    #[test]
    fn path_comparison_ignores_case_and_trailing_slash() {
        assert!(eq_path(r"C:\Users\me\.meadow\bin", r"c:\users\me\.meadow\bin"));
        assert!(eq_path(r"C:\a\bin\", r"C:\a\bin"));
        assert!(eq_path(r"C:\a\bin/", r"C:\a\bin"));
        assert!(!eq_path(r"C:\a\bin", r"C:\a\binary"));
    }

    #[test]
    fn unexpanded_variables_are_left_alone() {
        // The whole point of reading the registry raw: an entry like this must
        // survive an add/remove round trip untouched.
        let path = r"%JAVA_HOME%\bin;C:\Windows";
        let kept: Vec<&str> = entries(path).filter(|e| !eq_path(e, r"C:\Windows")).collect();
        assert_eq!(kept, vec![r"%JAVA_HOME%\bin"]);
    }
}

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
