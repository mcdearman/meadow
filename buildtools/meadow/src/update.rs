//! `meadow update` — bring dependencies forward.
//!
//! A manifest says which *reference* a dependency follows -- a branch, a tag,
//! or a commit. The lockfile says which commit that was when it was last
//! looked at. This looks again:
//!
//! ```text
//! meadow update              every dependency
//! meadow update json text    only these
//! meadow update --dry-run    say what would change, change nothing
//! ```
//!
//! What changes is the lockfile, never the manifest: `{ tag = "v1.2.0" }` still
//! means that tag afterwards. A dependency pinned with `rev` therefore cannot
//! move at all, which is the point of pinning one.
//!
//! Updating the *toolchain* -- which version of Meadow you have -- is
//! `meadowup update`, a different job and a different program. This one is
//! about the package in front of you.

use crate::git;
use crate::lock::{self, Lock};
use crate::package::{PackageGraph, Resolver};
use crate::status;
use std::path::{Path, PathBuf};

pub struct Options {
    /// The package, or a workspace member -- the lockfile is found from it.
    pub dir: PathBuf,
    /// Only these dependencies, by name. Empty means all of them.
    pub only: Vec<String>,
    /// Report what would change without changing it.
    pub dry_run: bool,
}

/// One dependency's commit, before and after.
struct Moved {
    name: String,
    source: String,
    from: Option<String>,
    to: String,
    /// The releases, when the dependency asked for a version: what a person
    /// reads instead of two commits.
    was: Option<String>,
    now: Option<String>,
}

pub fn run(opts: &Options) -> Result<(), String> {
    // Run somewhere that is not a package at all, this is most likely someone
    // reaching for what `meadow update` used to mean. Saying where that went
    // helps more than complaining about missing modules.
    if crate::package::manifest_path(&opts.dir).is_none() {
        return Err(format!(
            "{} is not a package: it has no Meadow.toml.\n\
             \n\
             `meadow update` brings a package's dependencies forward. To update\n\
             the toolchain itself, that is `meadowup update`.",
            crate::workspace::shown(&opts.dir)
        ));
    }
    let lock_dir = lock::dir_for(&opts.dir);
    let before = Lock::load(&lock_dir);

    // Everything a manifest names is checked, so that a dependency named on the
    // command line is looked for and reported if it is not there.
    let mut resolver = Resolver::for_entry(&lock_dir);
    resolver.update = true;
    resolver.only = opts.only.clone();
    resolver.net = git::Net::Allowed;
    resolver.locked = false;

    let entry: &Path = &opts.dir;
    let graph = PackageGraph::build_all_with(&[entry], &mut resolver)
        .map_err(|d| format!("{}: {}", d.filename, d.msg))?;
    let _ = graph;

    if !opts.only.is_empty() {
        let known: Vec<&str> = resolver.seen.iter().map(|(n, _)| n.as_str()).collect();
        for name in &opts.only {
            if !known.iter().any(|n| n == name) {
                return Err(format!(
                    "`{name}` is not a git dependency of this package.\n\
                     Its dependencies are: {}",
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
            }
        }
    }

    let moved = changes(&before, &resolver.lock);
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    status::status(
        "Locking",
        format!(
            "{} package{} to the latest commit{}",
            moved.len(),
            plural(moved.len()),
            plural(moved.len())
        ),
    );
    if moved.is_empty() {
        status::note("Unchanged", "every git dependency is already current");
        return Ok(());
    }

    for m in &moved {
        match &m.from {
            Some(from) => status::status(
                "Updating",
                format!(
                    "{} {} -> {} ({})",
                    m.name,
                    m.was
                        .clone()
                        .unwrap_or_else(|| git::short(from).to_string()),
                    m.now
                        .clone()
                        .unwrap_or_else(|| git::short(&m.to).to_string()),
                    lock::describes(&m.source)
                ),
            ),
            None => status::status(
                "Adding",
                format!(
                    "{} {} ({})",
                    m.name,
                    m.now
                        .clone()
                        .unwrap_or_else(|| git::short(&m.to).to_string()),
                    lock::describes(&m.source)
                ),
            ),
        }
    }

    if opts.dry_run {
        status::warning("not updating the lockfile, as this is a dry run");
        return Ok(());
    }
    resolver.lock.retain(&resolver.seen);
    resolver
        .lock
        .save(&lock_dir)
        .map_err(|e| format!("could not write {}: {e}", lock::FILE))?;
    Ok(())
}

/// What `after` pins that `before` did not, or pinned differently.
fn changes(before: &Lock, after: &Lock) -> Vec<Moved> {
    let mut out: Vec<Moved> = after
        .packages
        .iter()
        .filter_map(|now| {
            let had = before.find(&now.name, &now.source);
            match had {
                Some(had) if had.rev == now.rev => None,
                had => Some(Moved {
                    name: now.name.clone(),
                    source: now.source.clone(),
                    from: had.map(|h| h.rev.clone()),
                    to: now.rev.clone(),
                    was: had.and_then(|h| h.version.clone()),
                    now: now.version.clone(),
                }),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::Locked;

    fn locked(name: &str, rev: &str) -> Locked {
        Locked {
            name: name.to_string(),
            source: format!("git+https://e.com/{name}?branch=main"),
            rev: rev.to_string(),
            tree: "t".to_string(),
            version: None,
        }
    }

    #[test]
    fn a_commit_that_did_not_move_is_not_a_change() {
        let mut before = Lock::default();
        before.insert(locked("json", "aaa"));
        let mut after = Lock::default();
        after.insert(locked("json", "aaa"));
        assert!(changes(&before, &after).is_empty());
    }

    #[test]
    fn a_commit_that_moved_is_reported_both_ways() {
        let mut before = Lock::default();
        before.insert(locked("json", "aaa"));
        let mut after = Lock::default();
        after.insert(locked("json", "bbb"));
        let moved = changes(&before, &after);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].from.as_deref(), Some("aaa"));
        assert_eq!(moved[0].to, "bbb");
    }

    #[test]
    fn a_dependency_that_was_not_pinned_before_is_an_addition() {
        let before = Lock::default();
        let mut after = Lock::default();
        after.insert(locked("json", "aaa"));
        let moved = changes(&before, &after);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].from, None);
    }

    #[test]
    fn changes_are_reported_in_a_settled_order() {
        let before = Lock::default();
        let mut after = Lock::default();
        after.insert(locked("text", "bbb"));
        after.insert(locked("json", "aaa"));
        let moved = changes(&before, &after);
        let names: Vec<&str> = moved.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["json", "text"]);
    }

    #[test]
    fn one_name_changing_leaves_the_others_alone() {
        let mut before = Lock::default();
        before.insert(locked("json", "aaa"));
        before.insert(locked("text", "bbb"));
        let mut after = Lock::default();
        after.insert(locked("json", "ccc"));
        after.insert(locked("text", "bbb"));
        let moved = changes(&before, &after);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].name, "json");
    }
}
