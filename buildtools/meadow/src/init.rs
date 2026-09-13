//! `meadow init` — write the smallest thing the rest of the tools will accept
//! as a package.
//!
//! Which is not much: a `meadow.toml` naming the package, and a `src/Main.mw`
//! with an entry point in it. [`crate::package`] would in fact accept a bare
//! directory of `.mw` files and name the package after the directory, so what
//! this really buys is the *name* — written down, rather than inferred from
//! wherever the directory happens to sit — and a file that already runs.

use std::path::{Path, PathBuf};

pub struct Options {
    /// Where to put it. `.` is the usual answer.
    pub path: PathBuf,
    /// Overrides the name taken from the directory.
    pub name: Option<String>,
}

/// What was created, for the caller to report.
#[derive(Debug)]
pub struct Created {
    pub name: String,
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

pub fn run(opts: &Options) -> Result<Created, String> {
    let root = &opts.path;
    std::fs::create_dir_all(root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;

    let name = match &opts.name {
        Some(n) => n.clone(),
        None => directory_name(root)?,
    };
    check_name(&name)?;

    // Refuse rather than merge. A manifest is the one file that says "this is
    // already a package", and overwriting someone's dependency list to put a
    // template there is not a thing to do by accident.
    for existing in ["meadow.toml", "meadow.pkg"] {
        let p = root.join(existing);
        if p.exists() {
            return Err(format!(
                "{} already exists — this is already a package",
                p.display()
            ));
        }
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("could not create {}: {e}", src.display()))?;

    let mut files = Vec::new();
    write_new(&root.join("meadow.toml"), &manifest(&name), &mut files)?;
    // Left alone if it is already there: someone running this in a directory
    // that has sources wants the manifest, not a new `main`.
    let main = src.join("Main.mw");
    if !main.exists() {
        write_new(&main, MAIN, &mut files)?;
    }

    Ok(Created {
        name,
        root: root.clone(),
        files,
    })
}

fn write_new(path: &Path, contents: &str, files: &mut Vec<PathBuf>) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    files.push(path.to_path_buf());
    Ok(())
}

fn manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
}

const MAIN: &str = "\
def main = println \"Hello, world!\"
";

/// The name a directory implies: what it is called, not the path to it.
fn directory_name(root: &Path) -> Result<String, String> {
    // Canonicalised first, because `.` and `..` are ordinary requests and
    // neither has a file name of its own.
    let full = std::fs::canonicalize(root)
        .map_err(|e| format!("could not read {}: {e}", root.display()))?;
    full.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            format!(
                "cannot tell what to call a package at {} — pass --name",
                root.display()
            )
        })
}

/// Reject a name the language could not refer to.
///
/// A package name is not decoration: it is the first segment of a `use` path
/// (`use Std.Collections.List`), so it has to lex as one identifier. A
/// directory called `my-package` is perfectly ordinary and would produce a
/// package nobody could name, and the moment to say so is now rather than at
/// the first `use`.
///
/// The rule is the lexer's, including the detail that an upper-case identifier
/// admits no underscore.
fn check_name(name: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("`{name}` is not a usable package name: {why}"));

    let Some(first) = name.chars().next() else {
        return bad("it is empty");
    };
    if !first.is_ascii_alphabetic() {
        return bad("a package name starts with a letter");
    }
    let upper = first.is_ascii_uppercase();
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '\'' || (c == '_' && !upper);
        if !ok {
            let hint = if c == '-' {
                " — try `_` or run the words together"
            } else if c == '_' {
                " — a capitalised name takes no underscore"
            } else {
                ""
            };
            return bad(&format!(
                "`{c}` cannot appear in one, and the name is the first segment \
                 of a `use` path{hint}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_name;

    #[test]
    fn a_name_has_to_lex_as_one_identifier() {
        for good in ["app", "myApp", "my_app", "Std", "App2", "x"] {
            assert!(check_name(good).is_ok(), "{good} should be allowed");
        }
        for bad in ["my-app", "2fast", "", "my app", "my.app", "My_App"] {
            assert!(check_name(bad).is_err(), "{bad} should be refused");
        }
    }

    /// The hyphen is the one worth a hint: it is what every other ecosystem
    /// spells a multi-word package with, so it is what someone will type.
    #[test]
    fn a_hyphen_says_what_to_do_instead() {
        let e = check_name("my-app").unwrap_err();
        assert!(e.contains('`'), "{e}");
        assert!(e.contains("try `_`"), "{e}");
    }
}
