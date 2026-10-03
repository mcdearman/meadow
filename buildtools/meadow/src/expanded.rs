//! `meadow build --emit expanded`: a package's sources with every macro call
//! replaced by what it produced, as `cargo expand` shows a crate.
//!
//! The expander writes down each call and the tokens that took its place
//! (`meadow_compiler::expand::record::Produced`). A module's file is written
//! again under `target/<profile>/expanded/`, at the path it has in the
//! package, with each call it wrote replaced by that text between two
//! comments naming the macro. One level at a time: where what a macro
//! produced calls another macro, that call is left as it was produced, and
//! what *it* produced follows, marked as being within.
//!
//! What a macro produces is tokens, so that is what is written: spaced as
//! `stringify!` spaces them, a declaration to a line. It reads as code, not
//! as formatted code.

use meadow_compiler::expand::record::Produced;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::artifacts;
use crate::profile::Profile;

/// Write `produced` out for the package at `root`, and answer what to say
/// about it: how much each macro wrote, and where the files are.
pub fn write(root: &Path, profile: Profile, produced: &[Produced]) -> Result<String, String> {
    let dir = artifacts::profile_dir(root, profile).join("expanded");
    let _ = std::fs::remove_dir_all(&dir);
    let mut by_file: BTreeMap<&str, Vec<&Produced>> = BTreeMap::new();
    for p in produced {
        by_file.entry(p.filename.as_str()).or_default().push(p);
    }
    let mut files = 0;
    for (filename, calls) in &by_file {
        // Only what is in this package has a file here to write again: a
        // dependency compiled on the way is its own package's to show.
        let Some((rel, text)) = source(root, filename) else {
            continue;
        };
        artifacts::write_text(&dir.join(rel), &expanded(&text, calls))?;
        files += 1;
    }
    Ok(report(produced, files, &dir))
}

/// The module `filename` names, as a path inside the package at `root`, and
/// its text.
fn source(root: &Path, filename: &str) -> Option<(std::path::PathBuf, String)> {
    let path = Path::new(filename);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let (full, root) = (full.canonicalize().ok()?, root.canonicalize().ok()?);
    let rel = full.strip_prefix(&root).ok()?.to_path_buf();
    Some((rel, std::fs::read_to_string(&full).ok()?))
}

/// `text` with each call written in it replaced by what it produced.
fn expanded(text: &str, calls: &[&Produced]) -> String {
    // A call the file wrote is one no other call's argument holds; the rest
    // were produced, by the one whose argument holds them.
    let within = |inner: &Produced, outer: &Produced| {
        !std::ptr::eq(inner, outer)
            && outer.arg.start <= inner.call.start
            && inner.call.end <= outer.arg.end
    };
    let mut written: Vec<&Produced> = calls
        .iter()
        .copied()
        .filter(|c| !calls.iter().any(|o| within(c, o)))
        .collect();
    written.sort_by_key(|c| c.call.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for call in written {
        let (start, end) = (call.call.start as usize, call.call.end as usize);
        // Two records of one call -- a macro run again in a later round --
        // and anything that does not lie in the text are passed over.
        if start < at || end > text.len() || !text.is_char_boundary(start) {
            continue;
        }
        out.push_str(&text[at..start]);
        let _ = writeln!(
            out,
            "-- {}! produced {} tokens from {}:",
            call.name, call.tokens, call.given
        );
        out.push_str(&call.text);
        out.push('\n');
        for inner in calls.iter().filter(|c| within(c, call)) {
            let _ = writeln!(
                out,
                "-- within it, {}! produced {} tokens from {}:",
                inner.name, inner.tokens, inner.given
            );
            out.push_str(&inner.text);
            out.push('\n');
        }
        let _ = write!(out, "-- end of {}!", call.name);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// How much each macro wrote, most first.
fn report(produced: &[Produced], files: usize, dir: &Path) -> String {
    let mut by_macro: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for p in produced {
        let e = by_macro.entry(p.name.as_str()).or_default();
        e.0 += 1;
        e.1 += p.given;
        e.2 += p.tokens;
        e.3 += p.text.len();
    }
    let mut rows: Vec<_> = by_macro.into_iter().collect();
    rows.sort_by_key(|(_, (_, _, tokens, _))| std::cmp::Reverse(*tokens));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<20} {:>7} {:>12} {:>12} {:>10}",
        "macro", "calls", "tokens in", "tokens out", "KB out"
    );
    for (name, (calls, given, tokens, bytes)) in &rows {
        let _ = writeln!(
            out,
            "{:<20} {calls:>7} {given:>12} {tokens:>12} {:>10.1}",
            format!("{name}!"),
            *bytes as f64 / 1024.0
        );
    }
    let _ = writeln!(out, "expanded: {files} files under {}", dir.display());
    out
}
