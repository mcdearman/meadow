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
    /// Cut lines longer than this many characters, where there is a place to
    /// cut them: see `meadow_fmt::format_within`. `None` leaves every token
    /// on the line it is on.
    pub width: Option<usize>,
}

/// `src` formatted as `opts` ask, or why it is left alone.
///
/// Cutting a line moves tokens, and must move nothing else: what the
/// compiler's lexer reads out of the result is checked against what it read
/// out of `src`, token for token, and a file where they differ is not
/// written. That holds the one thing layout decides -- a macro call in
/// column 0 -- as well as everything a mistake here could do.
fn formatted(src: &str, opts: &Options) -> Result<String, String> {
    let Some(width) = opts.width else {
        return Ok(fmt::format(src));
    };
    let out = fmt::format_within(src, width);
    let tokens = |text: &str| {
        use meadow_compiler::source::{Source, SourceKind};
        let lexed =
            meadow_compiler::lexer::tokenize(Source::new(SourceKind::Interactive, text.into()));
        lexed
            .tokens
            .iter()
            .map(|t| t.value().clone())
            .collect::<Vec<_>>()
    };
    if tokens(src) == tokens(&out) {
        Ok(out)
    } else {
        // `MEADOW_FMT_WHY=1` says where: the first token that differs, and
        // the few before it, which is what finds the line.
        if std::env::var_os("MEADOW_FMT_WHY").is_some() {
            let (a, b) = (tokens(src), tokens(&out));
            let at = a
                .iter()
                .zip(&b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            eprintln!(
                "first difference at token {at}: {:?} vs {:?}",
                a.get(at),
                b.get(at)
            );
            eprintln!("  before: {:?}", &a[at.saturating_sub(6)..at]);
        }
        Err("cutting its lines would change what it says; left as it is".to_string())
    }
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
        let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let out = match formatted(&src, opts) {
            Ok(out) => out,
            Err(why) => {
                eprintln!("{}: {why}", file.display());
                continue;
            }
        };

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
