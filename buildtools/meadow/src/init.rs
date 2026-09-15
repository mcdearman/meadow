//! `meadow init` — write the smallest thing the rest of the tools will accept
//! as a package.
//!
//! Which is not much: a `meadow.toml` naming the package, and a `src/Main.mw`
//! with an entry point in it. [`crate::package`] would in fact accept a bare
//! directory of `.mw` files and name the package after the directory, so what
//! this really buys is the *name* — written down, rather than inferred from
//! wherever the directory happens to sit — and a file that already runs.
//!
//! And a `.gitignore` for the `target` directory builds write into (see
//! [`crate::artifacts`]), since nothing in it belongs in version control.
//!
//! Made inside a workspace, the package joins it: it is added to `members`
//! unless a pattern there already covers it, and gets no `.gitignore`, since
//! it builds into the workspace's `target`. `--workspace` makes the workspace
//! itself -- see [`crate::workspace`].

use crate::workspace::Workspace;
use std::path::{Path, PathBuf};

pub struct Options {
    /// Where to put it. `.` is the usual answer.
    pub path: PathBuf,
    /// Overrides the name taken from the directory.
    pub name: Option<String>,
    /// Make a workspace rather than a package -- see [`crate::workspace`].
    pub workspace: bool,
}

/// What was created, for the caller to report.
#[derive(Debug)]
pub struct Created {
    pub name: String,
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
    /// The root of the workspace the new package was added to, if it was
    /// made inside one that did not already count it a member.
    pub joined: Option<PathBuf>,
}

pub fn run(opts: &Options) -> Result<Created, String> {
    let root = &opts.path;

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

    // The workspace this lands in, which the new package joins: making one
    // inside a workspace means that, as `cargo new` does.
    let parent = std::path::absolute(root)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let workspace = match &parent {
        Some(parent) if !opts.workspace => enclosing_workspace(parent)?,
        _ => None,
    };

    std::fs::create_dir_all(root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;

    if opts.workspace {
        refuse_nesting(root)?;
        return init_workspace(root);
    }

    let name = match &opts.name {
        Some(n) => n.clone(),
        None => directory_name(root)?,
    };
    check_name(&name)?;
    if let Some(ws) = &workspace
        && let Some(m) = ws.member_named(&name)
    {
        return Err(format!(
            "the workspace at {} already has a member called `{name}`, at {}",
            crate::workspace::shown(&ws.root),
            crate::workspace::shown(&m.dir)
        ));
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src)
        .map_err(|e| format!("could not create {}: {e}", src.display()))?;

    let mut files = Vec::new();
    write_new(&root.join("meadow.toml"), &manifest(&name), &mut files)?;
    // Left alone if it is already there: someone running this in a directory
    // that has sources wants the manifest, not a new `main`.
    let main = src.join("Main.mw");
    if !main.exists() {
        write_new(&main, MAIN, &mut files)?;
    }

    // A member builds into its workspace's `target`, which the workspace's
    // own `.gitignore` covers.
    let joined = match &workspace {
        None => {
            ignore_target(&root.join(".gitignore"), &mut files)?;
            None
        }
        Some(ws) => join(ws, root, &mut files)?,
    };

    Ok(Created {
        name,
        root: root.clone(),
        files,
        joined,
    })
}

/// A virtual workspace at `root`: a manifest with an empty `members`, and
/// the `.gitignore` for the one `target` every member builds into.
fn init_workspace(root: &Path) -> Result<Created, String> {
    let mut files = Vec::new();
    write_new(&root.join("meadow.toml"), WORKSPACE, &mut files)?;
    ignore_target(&root.join(".gitignore"), &mut files)?;
    Ok(Created {
        name: directory_name(root)?,
        root: root.to_path_buf(),
        files,
        joined: None,
    })
}

/// Workspaces do not nest: one inside another would leave its members
/// belonging to two.
fn refuse_nesting(root: &Path) -> Result<(), String> {
    let full = std::fs::canonicalize(root)
        .map_err(|e| format!("could not read {}: {e}", root.display()))?;
    match full
        .parent()
        .map(enclosing_workspace)
        .transpose()?
        .flatten()
    {
        Some(ws) => Err(format!(
            "{} is inside the workspace at {}, and workspaces do not nest",
            root.display(),
            crate::workspace::shown(&ws.root)
        )),
        None => Ok(()),
    }
}

/// The workspace at or above `dir`, if there is one. A package being made is
/// not a member yet, so this looks for the `[workspace]` itself rather than
/// asking [`Workspace::find`], which would refuse it for not being one.
fn enclosing_workspace(dir: &Path) -> Result<Option<Workspace>, String> {
    let mut dir = Some(dir);
    while let Some(d) = dir {
        if d.join("meadow.toml").is_file() {
            let manifest = crate::package::Manifest::load(d).ok().flatten();
            if manifest.is_some_and(|m| m.workspace.is_some()) {
                return Workspace::load(d).map(Some);
            }
        }
        dir = d.parent();
    }
    Ok(None)
}

/// Make the package at `root` a member of `ws`, if nothing already makes it
/// one, answering the workspace root when it had to be added.
fn join(ws: &Workspace, root: &Path, files: &mut Vec<PathBuf>) -> Result<Option<PathBuf>, String> {
    let dir = crate::package::canonical(root);
    let reloaded = Workspace::load(&ws.root)?;
    if reloaded.member_at(&dir).is_some() {
        return Ok(None);
    }
    let rel = dir
        .strip_prefix(&ws.root)
        .map_err(|_| format!("{} is not under {}", dir.display(), ws.root.display()))?;
    let rel = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    let manifest = ws.root.join("meadow.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("could not read {}: {e}", manifest.display()))?;
    std::fs::write(&manifest, crate::workspace::add_member(&text, &rel))
        .map_err(|e| format!("could not write {}: {e}", manifest.display()))?;
    files.push(PathBuf::from(crate::workspace::shown(&manifest)));
    Ok(Some(PathBuf::from(crate::workspace::shown(&ws.root))))
}

const WORKSPACE: &str = "\
[workspace]
members = []
";

fn write_new(path: &Path, contents: &str, files: &mut Vec<PathBuf>) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    files.push(path.to_path_buf());
    Ok(())
}

/// The line that keeps build output out of version control.
const IGNORE_TARGET: &str = "/target/";

/// Make `.gitignore` ignore the `target` directory: a new file saying only
/// that, or -- when there is one already, which is someone else's -- the line
/// added to the end of it, unless it already ignores `target`.
fn ignore_target(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return write_new(path, &format!("{IGNORE_TARGET}\n"), files);
        }
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let ignored = existing
        .lines()
        .map(str::trim)
        .any(|l| matches!(l, "target" | "target/" | "/target" | "/target/"));
    if ignored {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    write_new(
        path,
        &format!("{existing}{separator}{IGNORE_TARGET}\n"),
        files,
    )
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
