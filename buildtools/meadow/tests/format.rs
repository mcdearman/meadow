//! The formatter, against real source.
//!
//! The standard library is the formatter's specification: it is hand-written in
//! the style the rules are meant to describe, so anything the formatter would
//! change there is a bug in one of the two.

use meadow::stdlib::MODULES;
use meadow_fmt as fmt;

#[test]
fn the_standard_library_is_already_formatted() {
    for (name, src) in MODULES {
        let out = fmt::format(src);
        if out != *src {
            // Show the first line that differs rather than 400 lines of source.
            let (line, want, got) = src
                .lines()
                .zip(out.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| (i + 1, a.to_string(), b.to_string()))
                .unwrap_or((0, "<length>".into(), "<length>".into()));
            panic!("Std.{name} line {line} would be reformatted:\n  is:   {want:?}\n  want: {got:?}");
        }
    }
}

#[test]
fn formatting_the_standard_library_is_idempotent() {
    for (name, src) in MODULES {
        let once = fmt::format(src);
        assert_eq!(fmt::format(&once), once, "Std.{name} is not a fixed point");
    }
}

#[test]
fn indentation_is_recovered_from_a_flattened_file() {
    // Strip every leading space and let the formatter put it back. It cannot
    // recover hand-aligned continuations — nothing structural could — so this
    // checks that the *structural* rules do the work rather than the fallback
    // that preserves whatever the author wrote.
    let mut total = 0usize;
    let mut wrong = 0usize;
    for (_, src) in MODULES {
        let flat: String = src
            .lines()
            .map(|l| format!("{}\n", l.trim_start()))
            .collect();
        for (a, b) in src.lines().zip(fmt::format(&flat).lines()) {
            total += 1;
            if a != b {
                wrong += 1;
            }
        }
    }
    // The stragglers are hand-aligned continuations — most of the bit-twiddling
    // in `Std.Bytes`, and a handful elsewhere. A proportion rather than a count,
    // so the bound keeps its meaning as the library grows: comfortably above the
    // ~1.3% these account for, far below what a broken rule would produce.
    assert!(
        wrong * 50 <= total,
        "{wrong} of {total} lines were not recovered from a flattened standard library"
    );
}
