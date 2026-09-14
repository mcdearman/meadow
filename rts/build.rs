//! A fingerprint of the sources that decide the native ABI -- the layout of
//! `Vm` and `Heap` the generated code reads, the calls it makes, the image it
//! is handed -- as `MEADOW_RTS_FINGERPRINT`.
//!
//! The runtime library exports a symbol named after it, and the `main` of every
//! program compiled ahead of time refers to that symbol (see
//! `codegen::object::main_c`), so linking a program against a runtime library
//! from other sources fails at link time instead of crashing when it runs.

use std::hash::Hasher;
use std::path::{Path, PathBuf};

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        here.join("src"),
        here.join("../compiler/meadow-bytecode/src"),
        here.join("../compiler/meadow-core/src"),
    ];
    let mut files = Vec::new();
    for root in &roots {
        println!("cargo:rerun-if-changed={}", root.display());
        walk(root, &mut files);
    }
    files.sort();
    // FNV-1a: stable across toolchains, unlike `DefaultHasher`.
    let mut hash = Fnv(0xcbf2_9ce4_8422_2325);
    for file in &files {
        let rel = roots
            .iter()
            .find_map(|r| file.strip_prefix(r).ok())
            .unwrap_or(file);
        hash.write(rel.to_string_lossy().as_bytes());
        hash.write(&std::fs::read(file).unwrap_or_default());
    }
    println!(
        "cargo:rustc-env=MEADOW_RTS_FINGERPRINT={:016x}",
        hash.finish()
    );
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

struct Fnv(u64);

impl Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}
