//! Finding and fetching a published release.
//!
//! Through `curl` and `tar` rather than an HTTP library: those ship with macOS,
//! essentially every Linux, and Windows 10 1803 and later, and a program whose
//! only job is to install another one should not carry a TLS stack to do it.

use super::{REPO, archive_ext, scratch_dir, valid_tag};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The tag of the newest release.
pub fn latest_tag() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = Command::new(curl())
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json", &url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not ask GitHub for the latest release: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    field(&body, "tag_name").ok_or_else(|| "GitHub named no release".to_string())
}

/// Download and unpack the release `tag` for `target`, answering the directory
/// it was unpacked into.
pub fn fetch(tag: &str, target: &str, into: &str) -> Result<PathBuf, String> {
    if !valid_tag(tag) {
        return Err(format!("`{tag}` is not a release tag"));
    }
    let ext = archive_ext();
    // As `release.yml` names them: the tag is in the URL, not in the file.
    let name = format!("meadow-{target}.{ext}");
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{name}");
    let dir = scratch_dir(into)?;
    let archive = dir.join(&name);

    let out = Command::new(curl())
        .args(["-fsSL", "-o"])
        .arg(&archive)
        .arg(&url)
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "no build of {tag} for {target}.\n\
             Looked for {name}"
        ));
    }
    unpack(&archive, &dir)?;
    Ok(dir)
}

/// Unpack `archive` into `dir`.
fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    // `tar` reads zip files on Windows, where it is bsdtar.
    let out = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .output()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not unpack {}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The file called `name` somewhere under `dir`.
///
/// A release archive may hold the binaries at its root or inside one directory
/// named after the release; looking rather than assuming survives either.
pub fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir()
            && let Some(found) = find(&path, name)
        {
            return Some(found);
        }
    }
    None
}

/// `curl` on this system.
fn curl() -> &'static str {
    if cfg!(windows) { "curl.exe" } else { "curl" }
}

/// One string field of a flat JSON object.
///
/// Enough for the one field wanted out of GitHub's release JSON, and small
/// enough not to want a parser: a release payload has no nested `tag_name`.
fn field(json: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let at = json.find(&key)? + key.len();
    let rest = json[at..].trim_start().strip_prefix(':')?.trim_start();
    let body = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_is_read_out_of_the_release_json() {
        let json = r#"{"url":"x","tag_name":"v0.1.0-alpha","name":"Meadow"}"#;
        assert_eq!(field(json, "tag_name").as_deref(), Some("v0.1.0-alpha"));
    }

    #[test]
    fn an_escape_in_a_field_is_read_rather_than_ending_it() {
        let json = r#"{"name":"a \"quoted\" thing","tag_name":"v1"}"#;
        assert_eq!(field(json, "name").as_deref(), Some("a \"quoted\" thing"));
        assert_eq!(field(json, "tag_name").as_deref(), Some("v1"));
    }

    #[test]
    fn a_field_that_is_not_there_is_none() {
        assert_eq!(field(r#"{"a":"b"}"#, "tag_name"), None);
    }

    #[test]
    fn a_tag_that_could_reach_elsewhere_is_refused() {
        assert!(valid_tag("v0.1.0-alpha"));
        assert!(valid_tag("v1.2.3"));
        assert!(!valid_tag("../../etc/passwd"));
        assert!(!valid_tag("v1/../v2"));
        assert!(!valid_tag(""));
        assert!(!valid_tag("v1 v2"));
    }
}
