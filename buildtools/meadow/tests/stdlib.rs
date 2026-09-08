//! The embedded `Std` package must compile clean, and be usable (prelude names in
//! scope, `::` sugar, the `Std.*` containers) from a program built against it.

use meadow::{pipeline, stdlib};
use meadow_eval as eval;

#[test]
fn stdlib_compiles_without_diagnostics() {
    let (_pkgs, diags) = stdlib::compile_std(meadow::Options::debug());
    assert!(
        diags.is_empty(),
        "Std did not compile clean:\n{}",
        diags
            .iter()
            .map(|d| format!("  {}: {}", d.filename, d.msg))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn run(src: &str) -> String {
    let (program, diags) = pipeline::compile_str_with_std("test", src, meadow::Options::debug());
    if !diags.is_empty() {
        return format!(
            "compile errors:\n{}",
            diags.iter().map(|d| d.msg.clone()).collect::<Vec<_>>().join("\n")
        );
    }
    match eval::run(&program) {
        Ok(v) => v.to_string(),
        Err(e) => format!("runtime error: {e}"),
    }
}

#[test]
fn prelude_list_functions_are_in_scope() {
    assert_eq!(
        run("def main = sum (map (\\x -> x * x) (range 1 5))\n"),
        "30" // 1 + 4 + 9 + 16
    );
}

#[test]
fn cons_sugar_builds_a_list() {
    assert_eq!(run("def main = 1 :: 2 :: 3 :: Nil\n"), "[1; 2; 3]");
}

#[test]
fn cons_sugar_in_patterns() {
    assert_eq!(
        run("fun swapFirstTwo xs =\n  match xs with\n  | a :: b :: rest -> b :: a :: rest\n  | other -> other\ndef main = swapFirstTwo [1; 2; 3; 4]\n"),
        "[2; 1; 3; 4]"
    );
}

#[test]
fn point_free_sum_with_operator_section() {
    // the motivating example
    assert_eq!(
        run("fun total = foldl (_ + _) 0\ndef main = total (range 1 11)\n"),
        "55"
    );
}

#[test]
fn foldl_and_filter() {
    assert_eq!(
        run("def main = foldl (\\a x -> a + x) 0 (filter (\\n -> n % 2 == 0) (range 0 10))\n"),
        "20" // 0 + 2 + 4 + 6 + 8
    );
}

#[test]
fn option_helpers() {
    assert_eq!(
        run("use Std.Maybe as Maybe\ndef main = Maybe.unwrapOr 0 (Maybe.map (\\x -> x + 1) (Just 41))\n"),
        "42"
    );
}

#[test]
fn std_map_roundtrip() {
    assert_eq!(
        run("use Std.Collections.Map as Map\ndef main =\n  let m = Map.insert 2 \"b\" (Map.insert 1 \"a\" Map.empty) in\n  Map.lookup 2 m\n"),
        "Just(\"b\")"
    );
}

#[test]
fn std_set_dedups() {
    assert_eq!(
        run("use Std.Collections.Set as Set\ndef main = Set.size (Set.fromList [1; 2; 2; 3; 3; 3])\n"),
        "3"
    );
}

#[test]
fn fs_write_read_list_remove() {
    use std::path::PathBuf;
    let dir: PathBuf =
        std::env::temp_dir().join(format!("meadow_fs_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("hello.txt").to_string_lossy().replace('\\', "/");
    let d = dir.to_string_lossy().replace('\\', "/");

    let src = format!(
        "def main =\n\
        \x20 let w = writeString (\"{p}\", \"greetings\") in\n\
        \x20 let back = readToString \"{p}\" in\n\
        \x20 let there = exists \"{p}\" in\n\
        \x20 let names = tryReadDir \"{d}\" in\n\
        \x20 let gone = removeFile \"{p}\" in\n\
        \x20 (back, there, names, exists \"{p}\")\n"
    );
    let got = run(&src);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(got, r#"(Ok("greetings"), true, ["hello.txt"], false)"#);
}

#[test]
fn fs_effect_can_be_handled() {
    // a handler intercepts `Fs` so the runtime never touches the disk
    let src = "def main =\n\
      \x20 handle readToString \"/nope\" with {\n\
      \x20   readToString path k -> k (Ok \"mocked\"),\n\
      \x20   return x -> x\n\
      \x20 }\n";
    assert_eq!(run(src), r#"Ok("mocked")"#);
}

#[test]
fn std_tree_sorts() {
    assert_eq!(
        run("use Std.Collections.Tree as Tree\ndef main = Tree.toList (Tree.fromList [5; 3; 8; 1; 4; 7; 9; 2; 6])\n"),
        "[1; 2; 3; 4; 5; 6; 7; 8; 9]"
    );
}
