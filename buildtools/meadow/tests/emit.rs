//! `--emit`: a build's bytecode and native code written as text, in place of
//! the binaries -- and the listings themselves.

use meadow::listing;
use meadow::{OptLevel, Options, pipeline, runtime};
use meadow_rts::codegen::{self, Arch};
use std::path::{Path, PathBuf};
use std::process::Command;

fn image(src: &str) -> meadow_bytecode::Program {
    let (program, diags) = pipeline::compile_str_with_std("test", src, Options::debug());
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    // What `main` reaches, as a build compiles: not all of `Std`.
    let program = meadow_compiler::core::prune::prune(&program);
    runtime::compile(&program, OptLevel::O1).unwrap()
}

const PROGRAM: &str = "fun fact n = if n == 0 then 1 else n * fact (n - 1)\n\
                       def main = fact 10\n";

/// Every function gets its label, where its code starts; every byte of the
/// code is on some line; and what comes out reads as each architecture's
/// instructions.
#[test]
fn a_listing_accounts_for_all_the_code() {
    let image = image(PROGRAM);
    for (arch, ret) in [(Arch::X86_64, "ret"), (Arch::Aarch64, "ret")] {
        let compiled = codegen::compile(&image, arch, OptLevel::O2);
        let text = listing::listing(&compiled, OptLevel::O2);
        let labels: std::collections::HashSet<&str> =
            text.lines().filter_map(|l| l.strip_suffix(':')).collect();
        assert!(!labels.is_empty());
        for &(pc, at) in &compiled.blocks {
            // Two blocks can share an offset; one of them names it.
            assert!(
                compiled
                    .blocks
                    .iter()
                    .any(|&(p, a)| a == at && labels.contains(format!("fn_pc{p}").as_str())),
                "{arch:?}: no label for pc {pc}"
            );
        }
        let bytes: usize = text
            .lines()
            .filter(|l| l.starts_with("  "))
            .map(|l| {
                l.split_whitespace()
                    .skip(1)
                    .take_while(|t| t.len() == 2 && t.bytes().all(|b| b.is_ascii_hexdigit()))
                    .count()
            })
            .sum();
        assert_eq!(
            bytes,
            compiled.code.len(),
            "{arch:?}: every byte listed once"
        );
        assert!(
            text.lines().any(|l| l.trim_end().ends_with(ret)),
            "{arch:?}:\n{text}"
        );
        assert!(
            !text.contains(".word"),
            "{arch:?}: undecodable words:\n{text}"
        );
    }
}

/// A branch says where it goes as a place in a function: `fact` calling itself
/// jumps back into its own code.
#[test]
fn a_branch_is_named_by_where_it_lands() {
    let image = image(PROGRAM);
    let text = listing::asm(&image, Arch::Aarch64, OptLevel::O2);
    assert!(
        text.lines()
            .any(|l| l.contains(" b ") && l.contains("; fn_pc")),
        "no branch names its place:\n{text}"
    );
    let text = listing::asm(&image, Arch::X86_64, OptLevel::O2);
    assert!(
        text.lines()
            .any(|l| l.contains("jmp") && l.contains(" fn_pc")),
        "no jump names its place:\n{text}"
    );
}

fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-emit-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meadow(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the meadow binary runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn build_writes_text_in_place_of_binaries() {
    let dir = scratch("build");
    let root = dir.join("fact");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("meadow.toml"), "[package]\nname = \"fact\"\n").unwrap();
    std::fs::write(root.join("src/Main.mw"), PROGRAM).unwrap();

    let (ok, err) = meadow(
        &root,
        &["build", "--emit", "bytecode,asm", "--target", "aarch64"],
    );
    assert!(ok, "{err}");
    let debug = root.join("target/debug");
    assert!(
        !debug.join("bytecode/fact.mbc").exists(),
        "the image was written though only text was asked for"
    );
    let bytecode = std::fs::read_to_string(debug.join("bytecode/fact.mbc.txt")).unwrap();
    assert_eq!(bytecode, image_text(&root));
    let native = std::fs::read_dir(debug.join("native"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("aarch64's code beside the host's, under its triple");
    let asm = std::fs::read_to_string(native.join("fact.s")).unwrap();
    assert!(
        asm.starts_with("; meadow native code: aarch64, -O1"),
        "{asm}"
    );
    assert!(err.contains("bytecode: ") && err.contains("asm: "), "{err}");

    // Both kinds of `--emit`, and the image an ordinary build writes.
    let (ok, err) = meadow(&root, &["build", "--emit", "image", "--emit", "bytecode"]);
    assert!(ok, "{err}");
    assert!(debug.join("bytecode/fact.mbc").exists());

    // `link` takes an image to text as it would to an executable.
    let (ok, err) = meadow(
        &root,
        &[
            "link",
            "--emit",
            "asm",
            "--target",
            "x86_64",
            "target/debug/bytecode/fact.mbc",
        ],
    );
    assert!(ok, "{err}");
    let asm = std::fs::read_to_string(debug.join("bytecode/fact.s")).unwrap();
    assert!(
        asm.starts_with("; meadow native code: x86_64, -O2"),
        "{asm}"
    );

    let (ok, err) = meadow(&root, &["build", "--emit", "elf"]);
    assert!(
        !ok && err.contains("expected image, bytecode, asm or exe"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// What `meadow dis` prints for the package at `root`, which is the text an
/// `--emit bytecode` writes.
fn image_text(root: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(root)
        .args(["dis"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_lone_file_has_nowhere_to_emit_to() {
    let dir = scratch("lone");
    std::fs::write(dir.join("one.mw"), PROGRAM).unwrap();
    let (ok, err) = meadow(&dir, &["build", "--emit", "asm", "one.mw"]);
    assert!(!ok, "{err}");
    assert!(err.contains("a lone file has none"), "{err}");
    let out = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(&dir)
        .args(["dis", "--asm", "--target", "x86_64", "one.mw"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("; meadow native code: x86_64"));
    let _ = std::fs::remove_dir_all(&dir);
}
