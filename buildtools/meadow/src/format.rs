//! `meadow fmt` — apply [`meadow_compiler::fmt`] to files on disk.
//!
//! The formatter itself is a pure `&str -> String`; everything here is the file
//! handling around it: which paths to visit, whether to write or just report,
//! and what to print.

use meadow_fmt as fmt;
use std::path::{Path, PathBuf};

pub struct Options {
    /// Files or directories to format. A directory is walked for `.mw` files.
    pub paths: Vec<PathBuf>,
    /// Report what would change and exit non-zero instead of writing.
    pub check: bool,
    /// Print the formatted source to stdout instead of writing it back.
    pub stdout: bool,
}

/// Returns the number of files that were (or would be) changed.
pub fn run(opts: &Options) -> Result<usize, String> {
    let mut files = Vec::new();
    for path in &opts.paths {
        collect(path, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err(format!(
            "no .mw files under {}",
            opts.paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut changed = 0;
    for file in &files {
        let src = std::fs::read_to_string(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let out = fmt::format(&src);

        if opts.stdout {
            print!("{out}");
            continue;
        }
        if out == src {
            continue;
        }
        changed += 1;
        if opts.check {
            println!("would reformat {}", file.display());
        } else {
            std::fs::write(file, &out).map_err(|e| format!("{}: {e}", file.display()))?;
            println!("reformatted {}", file.display());
        }
    }

    if !opts.stdout && changed == 0 {
        println!("{} file(s) already formatted", files.len());
    }
    Ok(changed)
}

/// Add `path` to `out` if it is a `.mw` file, or every `.mw` file beneath it if
/// it is a directory.
fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let entries = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut children: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() || p.extension().is_some_and(|e| e == "mw"))
        .collect();
    children.sort();
    for child in children {
        collect(&child, out)?;
    }
    Ok(())
}
