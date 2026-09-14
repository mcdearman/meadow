//! Vectors built by the runtime itself.
//!
//! `Fs.readDir` and `Process.argv` answer with a `Vector`, which the runtime has
//! to lay out exactly the way `Std.Collections.Vector.fromArray` would -- that
//! module's code is what takes it apart. One chunk holds 32 elements, and 32
//! chunks fill a branch, so the sizes below cover an empty vector, a single
//! chunk, a one-level tree and a two-level one, on both engines.

mod common;
use common::{cek_main_std, eval_main_std};

/// A fresh directory holding `n` empty files named `f0`, `f1`, ….
/// `tag` keeps tests running in parallel out of each other's directories.
fn dir_with(tag: &str, n: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "meadow-native-vectors-{}-{tag}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("f{i}")), b"").unwrap();
    }
    dir
}

/// Read the directory, then use the vector the way ordinary code would: its
/// length, every element through a fold, indexing at both ends, and a push
/// that has to extend the runtime's shape rather than replace it.
fn program(dir: &std::path::Path) -> String {
    format!(
        "use Std.Collections.Vector as V\n\
         use Std.String as S\n\
         def main =\n\
         \x20 match readDir \"{}\" with\n\
         \x20 | Err e -> (0 - 1, 0, False, 0)\n\
         \x20 | Ok names ->\n\
         \x20     let total = V.foldl (\\acc name -> acc + S.byteLength name) 0 names in\n\
         \x20     let ends = V.get names 0 != None and V.get names (V.len names - 1) != None in\n\
         \x20     let pushed = V.pushBack names \"extra\" in\n\
         \x20     (V.len names, total, ends or V.len names == 0, V.len (V.reverse pushed))\n",
        dir.display()
    )
}

fn check(n: usize) {
    let dir = dir_with("dir", n);
    let src = program(&dir);
    let total: usize = (0..n).map(|i| format!("f{i}").len()).sum();
    let want = format!("({n}, {total}, True, {})", n + 1);
    assert_eq!(eval_main_std(&src), want, "VM, {n} entries");
    assert_eq!(cek_main_std(&src), want, "CEK, {n} entries");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_directory_is_an_empty_vector() {
    check(0);
}

#[test]
fn a_directory_that_fits_one_chunk() {
    check(5);
}

#[test]
fn a_directory_past_one_chunk() {
    check(40);
}

#[test]
fn a_directory_past_one_branch() {
    check(1100);
}

#[test]
fn read_bytes_is_an_array() {
    let dir = dir_with("bytes", 0);
    let file = dir.join("bytes");
    std::fs::write(&file, [104u8, 105, 255]).unwrap();
    let src = format!(
        "def main = match readBytes \"{}\" with | Ok b -> (arrayLen b, arrayGet b 2) | Err e -> (0, 0)\n",
        file.display()
    );
    assert_eq!(eval_main_std(&src), "(3, 255)");
    assert_eq!(cek_main_std(&src), "(3, 255)");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn argv_is_a_vector() {
    let src = "use Std.Collections.Vector as V\ndef main = V.len (argv ()) >= 0\n";
    assert_eq!(eval_main_std(src), "True");
    assert_eq!(cek_main_std(src), "True");
}
