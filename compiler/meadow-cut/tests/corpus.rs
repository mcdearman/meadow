//! Idyll's Cut corpus, run by the reference interpreter: every program in
//! `cut-corpus/` of the Idyll repository, against what its `.expect` says.
//!
//! Where the corpus is comes from `CUT_CORPUS`; without it, the test says so
//! and passes, so that a checkout with no Idyll beside it still builds. Each
//! program is `NAME.cut` and `NAME.expect`: the expect file's `prints:` lines,
//! each with a newline, then its `answer:` text with none, are the program's
//! standard output, and `exit:` its status. A program whose expect file says
//! `nondeterministic` -- it rolls dice or reads the clock -- is held to its
//! status alone.

use meadow_cut::interp::{Options, run};
use meadow_cut::parse;
use std::path::{Path, PathBuf};

struct Expect {
    output: String,
    status: i64,
    /// Only the status is checked.
    nondeterministic: bool,
}

fn expect(text: &str) -> Expect {
    let mut prints = String::new();
    let mut answer = String::new();
    let mut status = 0;
    let mut nondeterministic = false;
    for line in text.lines() {
        if line.trim() == "nondeterministic" {
            nondeterministic = true;
        }
        if let Some(rest) = line.strip_prefix("prints:") {
            prints.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            prints.push('\n');
        } else if let Some(rest) = line.strip_prefix("answer:") {
            answer = rest.strip_prefix(' ').unwrap_or(rest).to_string();
        } else if let Some(rest) = line.strip_prefix("exit:") {
            status = rest.trim().parse().unwrap_or(-1);
        }
    }
    Expect {
        output: prints + &answer,
        status,
        nondeterministic,
    }
}

fn programs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for p in entries.flatten().map(|e| e.path()) {
        if p.is_dir() {
            programs(&p, out);
        } else if p.extension().is_some_and(|e| e == "cut") {
            out.push(p);
        }
    }
}

#[test]
fn idylls_corpus_runs_as_idyll_says_it_does() {
    let Some(root) = std::env::var_os("CUT_CORPUS") else {
        eprintln!("CUT_CORPUS is not set: Idyll's corpus not run");
        return;
    };
    let mut files = Vec::new();
    programs(Path::new(&root), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no `.cut` files under {root:?}");
    let mut bad = Vec::new();
    for cut in &files {
        let name = cut.strip_prefix(&root).unwrap_or(cut).display().to_string();
        let want = match std::fs::read_to_string(cut.with_extension("expect")) {
            Ok(t) => expect(&t),
            Err(_) => {
                bad.push(format!("{name}: no .expect beside it"));
                continue;
            }
        };
        let text = std::fs::read_to_string(cut).expect("a .cut file reads");
        let program = match parse(&text) {
            Ok(p) => p,
            Err(e) => {
                bad.push(format!("{name}: does not read: {e}"));
                continue;
            }
        };
        match run(&program, &Options::default()) {
            Ok(out)
                if out.status == want.status
                    && (want.nondeterministic || out.output == want.output) => {}
            Ok(out) => bad.push(format!(
                "{name}: printed {:?} with status {}, expected {:?} with status {}",
                out.output, out.status, want.output, want.status
            )),
            Err((e, out)) => bad.push(format!(
                "{name}: fails: {e} (after printing {:?})",
                out.output
            )),
        }
    }
    eprintln!(
        "{} of {} programs agree",
        files.len() - bad.len(),
        files.len()
    );
    assert!(
        bad.is_empty(),
        "{} of {} programs disagree:\n{}",
        bad.len(),
        files.len(),
        bad.join("\n")
    );
}
