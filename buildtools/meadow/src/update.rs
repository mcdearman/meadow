//! `meadow update` — replace this binary with the latest published release.
//!
//! The same job `meadow-setup.exe` / `install.sh` do, but from inside the
//! toolchain and for whichever platform is running. Downloading goes through
//! `curl` and `tar`, which ship with macOS, essentially every Linux, and Windows
//! 10 1803 and later — so the compiler gains no HTTP or TLS dependency for a
//! command most people run once in a while.
//!
//! Asset names have to match what `.github/workflows/release.yml` uploads.

use std::path::{Path, PathBuf};

const REPO: &str = "mcdearman/meadow";

/// The version this binary was built as, e.g. `0.1.0`.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

pub struct Options {
    /// A specific release tag; the latest release when `None`.
    pub version: Option<String>,
    /// Re-install even when the running version already matches.
    pub force: bool,
}

pub fn run(opts: &Options) -> Result<(), String> {
    let target = target_triple()?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not locate the running executable: {e}"))?;

    // A previous update on Windows could not delete the binary it displaced,
    // because that image was the process doing the updating. Nothing holds it
    // now, so clear it before making another.
    let _ = std::fs::remove_file(backup_path(&exe));

    let wanted = match &opts.version {
        Some(tag) => tag.clone(),
        None => {
            println!("Checking for a newer release...");
            latest_tag()?
        }
    };
    if !valid_tag(&wanted) {
        return Err(format!("`{wanted}` is not a release tag"));
    }

    if !opts.force && wanted.trim_start_matches('v') == CURRENT {
        println!("Already on {wanted} — the latest release.");
        println!("Re-install anyway with `meadow update --force`.");
        return Ok(());
    }

    let to = wanted.trim_start_matches('v');
    if to == CURRENT {
        println!("Re-installing {CURRENT}");
    } else {
        println!("Updating {CURRENT} -> {to}");
    }

    let tmp = scratch_dir("meadow-update")?;
    let result = install(&wanted, target, &tmp, &exe);
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    println!("Updated. `meadow --version` to confirm.");
    Ok(())
}

fn install(tag: &str, target: &str, tmp: &Path, exe: &Path) -> Result<(), String> {
    let asset = format!("meadow-{target}.{}", archive_ext());
    let url = if tag == "latest" {
        format!("https://github.com/{REPO}/releases/latest/download/{asset}")
    } else {
        format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
    };

    let archive = tmp.join(&asset);
    println!("  downloading {url}");
    let out = std::process::Command::new(curl())
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&url)
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        // 22 is curl's "the server said no", i.e. there is no such asset — the
        // message below already explains that. Anything else is a real network
        // problem and curl's own words are the only clue.
        let detail = if out.status.code() == Some(22) {
            String::new()
        } else {
            format!("{}\n", String::from_utf8_lossy(&out.stderr).trim())
        };
        return Err(format!(
            "{detail}no release build of {tag} for {target} (looked for {asset})"
        ));
    }

    // `tar` reads zip archives too on the Windows build.
    let flags = if cfg!(windows) { "-xf" } else { "-xzf" };
    let status = std::process::Command::new("tar")
        .arg(flags)
        .arg(&archive)
        .arg("-C")
        .arg(tmp)
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !status.success() {
        return Err(format!("could not unpack {asset}"));
    }

    let fresh = tmp.join(exe_name());
    if !fresh.is_file() {
        return Err(format!("{asset} did not contain {}", exe_name()));
    }
    replace(exe, &fresh)
}

/// Swap `fresh` into place at `exe`.
///
/// The running binary is renamed aside rather than written over: Windows refuses
/// to open a running executable for writing, and Linux gives `ETXTBSY`. Renaming
/// is allowed on both, and this process keeps executing the file it started with.
///
/// On Unix the leftover then unlinks cleanly even though it is still running. On
/// Windows it cannot — this very process *is* that image — so it stays until
/// something removes it later: the next `update` (see [`run`]) or an installer,
/// both of which look for the same [`backup_path`].
fn replace(exe: &Path, fresh: &Path) -> Result<(), String> {
    let backup = backup_path(exe);
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(exe, &backup)
        .map_err(|e| format!("could not move {} aside: {e}", exe.display()))?;

    let placed = std::fs::rename(fresh, exe).or_else(|_| {
        // A temp dir on another volume cannot be renamed across; copy instead.
        std::fs::copy(fresh, exe).map(|_| ())
    });
    if let Err(e) = placed {
        // Put the old binary back rather than leaving nothing behind.
        let _ = std::fs::rename(&backup, exe);
        return Err(format!("could not write {}: {e}", exe.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755));
    }

    let _ = std::fs::remove_file(&backup);
    Ok(())
}

/// Whether `tag` is safe to put in a release URL.
///
/// The tag goes straight into the download path, so a `/` or a `..` in it walks
/// out of this repository's releases: `--version ../../someone/else/releases/\
/// download/v1` would fetch a stranger's binary and install it over this one.
/// Release tags look like `v1.2.3`, so anything outside this alphabet is either
/// a typo or an attempt.
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
/// Deliberately `create_dir` rather than `create_dir_all`: on a shared machine
/// the old predictable path (`/tmp/meadow-update-<pid>`) could be pre-created by
/// somebody else — or made a symlink somewhere else — and we would unpack into
/// it and then install what we found there. Failing when it already exists is
/// the point, and the nonce is what makes guessing it impractical.
fn scratch_dir(prefix: &str) -> Result<std::path::PathBuf, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{nonce:08x}",
        std::process::id()
    ));
    std::fs::create_dir(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The newest release tag, from the GitHub API.
fn latest_tag() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = std::process::Command::new(curl())
        .args(["-fsSL", &url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not reach the releases API — no releases published yet?\n\
             You can name one explicitly: `meadow update --version v0.1.0`"
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    field(&body, "tag_name")
        .ok_or_else(|| "the releases API returned no `tag_name`".to_string())
}

/// Pull `"<name>": "<value>"` out of a JSON blob.
///
/// A whole JSON parser would be a lot of dependency for one string, and the
/// shape here is fixed by GitHub.
fn field(json: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let rest = &json[json.find(&key)? + key.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn curl() -> &'static str {
    if cfg!(windows) { "curl.exe" } else { "curl" }
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "meadow.exe" } else { "meadow" }
}

fn archive_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

/// The Rust target triple this binary was built for, which is what names the
/// release asset. Must agree with the workflow's build matrix.
fn target_triple() -> Result<&'static str, String> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        (os, arch) => {
            return Err(format!(
                "no release builds for {os}/{arch} — build from source instead:\n\
                 \x20   cargo install --path meadow"
            ))
        }
    };
    Ok(triple)
}

/// Where a displaced binary is parked: the executable's name plus `.old`.
///
/// Appended rather than substituted, so this is `meadow.exe.old` on Windows and
/// `meadow.old` elsewhere — `Path::with_extension` would *replace* `.exe` and
/// give a name the installers do not recognise.
pub fn backup_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_field_extraction() {
        let body = r#"{"url":"x","tag_name":"v1.2.3","name":"Release"}"#;
        assert_eq!(field(body, "tag_name").as_deref(), Some("v1.2.3"));
        assert_eq!(field(body, "name").as_deref(), Some("Release"));
        assert_eq!(field(body, "missing"), None);
    }

    #[test]
    fn json_field_tolerates_whitespace() {
        let body = "{\n  \"tag_name\" : \"v0.1.0\"\n}";
        assert_eq!(field(body, "tag_name").as_deref(), Some("v0.1.0"));
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        // `meadow-<triple>.tar.gz` / `.zip` — see .github/workflows/release.yml.
        let target = target_triple().expect("this platform is in the matrix");
        let asset = format!("meadow-{target}.{}", archive_ext());
        assert!(asset.starts_with("meadow-"));
        assert!(asset.ends_with(if cfg!(windows) { ".zip" } else { ".tar.gz" }));
    }
    #[test]
    fn ordinary_release_tags_are_accepted() {
        for tag in ["v0.1.0", "0.1.0", "v1.2.3-rc.1", "v1.0.0+build.2", "nightly_2024"] {
            assert!(valid_tag(tag), "should accept {tag}");
        }
    }

    #[test]
    fn a_tag_cannot_walk_out_of_the_releases_path() {
        // The tag is interpolated into the download URL. A `/` or a `..` in it
        // reaches another repository's releases, and whatever is downloaded
        // replaces the running binary — so this is the check that matters.
        for tag in [
            "../../someone/else/releases/download/v1",
            "v1/../../evil",
            "..",
            "a/b",
            "%2e%2e%2f",
            r"v1\..\..",
        ] {
            assert!(!valid_tag(tag), "should reject {tag}");
        }
    }

    #[test]
    fn nothing_that_could_confuse_a_url_gets_through() {
        for tag in ["", "v1 2", "v1?x=y", "v1#frag", "v1@host", "v1:80", "v1\n"] {
            assert!(!valid_tag(tag), "should reject {tag:?}");
        }
        // And nothing absurdly long.
        assert!(!valid_tag(&"v".repeat(65)));
    }
}
