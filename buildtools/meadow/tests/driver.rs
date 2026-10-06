//! What a program run by `meadow` gets from the command line and gives back to
//! it: its arguments, bytes written as they are, a bytecode image run or linked
//! on its own -- and nothing at all when it does not compile.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-driver-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A package at `dir/name` whose `main` is `main`. The directory is called
/// what this test calls it; the package is called what a package is called.
fn package(dir: &Path, name: &str, main: &str) -> PathBuf {
    let root = dir.join(name);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Meadow.toml"),
        format!(
            "[package]\nname = \"{}\"\n",
            meadow::package::as_package_name(name)
        ),
    )
    .unwrap();
    std::fs::write(root.join("src/Main.mw"), main).unwrap();
    root
}

fn meadow(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the meadow binary runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The last line a run printed.
fn answer(stdout: &str) -> &str {
    stdout.lines().last().unwrap_or("")
}

const ARGS: &str = "use Std.Process\nuse Std.Collections.Vector as V\n\nfun main () = println \"${V.toList (argv ()):?}\"\n";

#[test]
fn a_program_gets_what_follows_the_double_dash() {
    let dir = scratch("args");
    let root = package(&dir, "args", ARGS);
    for engine in [&[][..], &["--backend", "vm"], &["--cek"]] {
        let mut args = vec!["run"];
        args.extend_from_slice(engine);
        args.extend([".", "--", "one", "two words", "--three"]);
        let (ok, out, err) = meadow(&root, &args);
        assert!(ok, "{err}");
        assert_eq!(
            answer(&out),
            r#"["one"; "two words"; "--three"]"#,
            "{engine:?}"
        );
    }
    let (ok, out, err) = meadow(&root, &["run", "."]);
    assert!(ok, "{err}");
    assert_eq!(answer(&out), "[;]", "and nothing, given nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Prints, says why it gives up on standard error, and stops with status 3:
/// giving up, made of the runtime's basics.
const GIVES_UP: &str = "use Std.Process (exit)\n\nfun main () =\n  let _ = println \"before\" in\n  let _ = eprintln \"gave up: no input\" in\n  exit 3\n";

#[test]
fn a_program_gives_up_on_standard_error_on_every_engine() {
    let dir = scratch("gives-up");
    let root = package(&dir, "gives-up", GIVES_UP);
    for engine in [
        &[][..],
        &["--backend", "vm"],
        &["--cek"],
        &["--runtime", "silo"],
    ] {
        let mut args = vec!["run"];
        args.extend_from_slice(engine);
        args.push(".");
        let (ok, out, err) = meadow(&root, &args);
        assert!(!ok, "{engine:?}: exit 3 is a failure");
        assert_eq!(
            answer(&out),
            "before",
            "{engine:?}: stdout keeps what it printed"
        );
        assert!(
            err.contains("gave up: no input\n"),
            "{engine:?}: the message is on stderr: {err}"
        );
        assert!(!out.contains("gave up"), "{engine:?}: and only there");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bytes_are_written_as_they_are() {
    let dir = scratch("bytes");
    let root = package(
        &dir,
        "bytes",
        "use Std.Fs\n\nfun main () =\n  let w = writeBytes (\"out.bin\", #[0xff, 0x00, 0x41, 0x80]) in\n  println \"${(w, readBytes \"out.bin\"):?}\"\n",
    );
    for engine in [&[][..], &["--backend", "vm"], &["--cek"]] {
        let mut args = vec!["run"];
        args.extend_from_slice(engine);
        args.push(".");
        let (ok, out, err) = meadow(&root, &args);
        assert!(ok, "{err}");
        assert_eq!(answer(&out), "(Ok(()), Ok(#[255, 0, 65, 128]))");
        assert_eq!(
            std::fs::read(root.join("out.bin")).unwrap(),
            [0xff, 0, 0x41, 0x80]
        );
        std::fs::remove_file(root.join("out.bin")).unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_image_runs_on_its_own() {
    let dir = scratch("exec");
    let root = package(&dir, "img", ARGS);
    let (ok, _, err) = meadow(&root, &["build", "."]);
    assert!(ok, "{err}");
    let image = root.join("target/debug/bytecode/Img.mbc");
    assert!(image.is_file());
    let image = image.to_string_lossy().into_owned();
    for backend in ["jit", "vm"] {
        let (ok, out, err) = meadow(&dir, &["exec", &image, "--backend", backend, "--", "x"]);
        assert!(ok, "{err}");
        assert_eq!(answer(&out), r#"["x"]"#);
    }
    let (ok, _, err) = meadow(&dir, &["exec", &root.join("Meadow.toml").to_string_lossy()]);
    assert!(!ok);
    assert!(err.contains("not a Meadow image"), "{err}");

    // An executable from it, where there is a runtime library to link.
    let exe = dir
        .join("out")
        .join(format!("img{}", std::env::consts::EXE_SUFFIX));
    let (ok, _, err) = meadow(&dir, &["link", &image, "-o", &exe.to_string_lossy()]);
    if !ok {
        assert!(
            err.contains("runtime"),
            "a link failure other than a missing runtime: {err}"
        );
        eprintln!("skipping the executable: {err}");
    } else {
        let out = Command::new(&exe).args(["a", "b"]).output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains(r#"["a"; "b"]"#));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_that_does_not_compile_is_not_run() {
    let dir = scratch("errors");
    let root = package(&dir, "broken", "fun main () = println (1 + \"one\")\n");
    let (ok, out, err) = meadow(&root, &["run", "."]);
    assert!(!ok);
    assert!(err.contains("mismatch") || err.contains("integer"), "{err}");
    assert!(
        !out.contains("=>") && !err.contains("ill-formed"),
        "{out}{err}"
    );
    let (ok, _, _) = meadow(&root, &["build", "."]);
    assert!(!ok);
    assert!(
        !root.join("target/debug/bytecode/Broken.mbc").exists(),
        "and no image is written for it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `meadow` in `dir`, with `input` on its standard input.
fn meadow_fed(dir: &Path, args: &[&str], input: &str) -> (bool, String) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the meadow binary runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_message_framed_by_its_length_is_read_whole() {
    // The Language Server Protocol's framing: a header, a blank line, and a
    // body that no newline ends -- `readLine` for the first two, `readExact`
    // for the last. The two read through one buffer, so they take turns.
    let dir = scratch("framed");
    let root = package(
        &dir,
        "framed",
        "use Std.Console (readLine, readExact)
use Std.String as S

fun loop (n : Int) =
  match readLine () with
  | None -> println \"${show n} read\"
  | Just header ->
      let blank = readLine () in
      match S.toInt (S.trim (S.drop header 15)) with
      | None -> println \"bad header\"
      | Just k -> (match readExact k with
          | Just body -> (let u = println \"[${body}]\" in loop (n + 1))
          | None -> println \"cut short\")

fun main () = loop 0
",
    );
    let input = "Content-Length: 7\r\n\r\n{\"a\":1}Content-Length: 14\r\n\r\n{\"method\":\"x\"}Content-Length: 50\r\n\r\nshort";
    for backend in ["jit", "vm"] {
        let (ok, out) = meadow_fed(&root, &["run", "--backend", backend], input);
        assert!(ok, "{backend}: {out}");
        assert!(
            out.contains("[{\"a\":1}]\n[{\"method\":\"x\"}]\ncut short\n"),
            "{backend}: {out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// --- deep programs ------------------------------------------------------------------
//
// The compiler walks a program recursively, and ran on a main thread whose
// stack is 1 MB on Windows: a few hundred nested `let`s, three hundred chained
// `+`s or a literal of a few hundred elements killed the process with a stack
// overflow. `meadow` now runs on a thread with room.

/// Build and run `main` with the binary, and answer what it printed last.
#[track_caller]
fn runs(who: &str, main: &str) -> String {
    let dir = scratch(who);
    let root = package(&dir, who, main);
    let (ok, stdout, stderr) = meadow(&root, &["run"]);
    assert!(ok, "{who} failed:\n{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
    answer(&stdout).to_string()
}

#[test]
fn many_nested_lets_compile() {
    let n = 600;
    let mut src = String::from("fun main () =\n");
    for i in 0..n {
        src.push_str(&format!("  let x{i} = {i} in\n"));
    }
    src.push_str(&format!("  println x{}\n", n - 1));
    assert_eq!(runs("nested_lets", &src), format!("{}", n - 1));
}

#[test]
fn a_long_chain_of_additions_compiles() {
    let n = 600;
    let terms: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
    let src = format!("fun main () = println ({})\n", terms.join(" + "));
    assert_eq!(runs("long_sum", &src), format!("{}", n * (n + 1) / 2));
}

#[test]
fn a_long_literal_compiles() {
    // Past the register file: see `meadow_codegen`, "Spilling".
    let n = 800;
    let items: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
    let src = format!(
        "use Std.Collections.Vector as V\n\nfun main () = println (V.len [{}])\n",
        items.join(", ")
    );
    assert_eq!(runs("long_literal", &src), format!("{n}"));
}

#[test]
fn a_macro_a_hundred_calls_deep_compiles() {
    let args: Vec<String> = (1..=120).map(|i| i.to_string()).collect();
    let src = format!(
        "macro sum\n  | ($x) -> {{ $x }}\n  | ($x, $( $r ),+) -> {{ $x + sum!($( $r ),+) }}\n\n\
         fun main () = println (sum!({}))\n",
        args.join(", ")
    );
    assert_eq!(runs("deep_macro", &src), "7260");
}

/// A program as a front end hands one to the back end: data taken apart, an
/// object called, a top-level value, a loop, text printed by the runtime as
/// it goes and a string for an answer.
const CUT: &str = r#"cut 0
entry t:Main/main
answer str

native {
  t:Main/Console.say = Console.writeOutput
}

effect t:Main/Console { say(str) -> unit }

data t:Main/Nat { Z; S(ptr) }

val t:Main/two : ptr =
  <t:Main/Nat.S(t:Main/Nat.S(t:Main/Nat.Z)) | halt>

def t:Main/count (n: ptr, acc: i64; k: ptr) =
  <n | case {
    t:Main/Nat.Z => <acc | k>;
    t:Main/Nat.S(m: ptr) => prim add(acc, 1; μ̃ a: i64. t:Main/count(m, a; k))
  }>

def t:Main/down (n: i64, acc: i64; k: ptr) =
  prim eq(n, 0;
    μ̃ u: unit. prim sub(n, 1; μ̃ m: i64. prim add(acc, 2; μ̃ a: i64. t:Main/down(m, a; k))),
    μ̃ u: unit. <acc | k>)

def t:Main/main (; k: ptr) =
  perform t:Main/Console.say("counting\n"; μ̃ u: unit.
    t:Main/count(t:Main/two, 0; μ̃ c: i64.
      <cocase { apply(x: i64; k1: ptr) => t:Main/down(100000, x; k1) } | apply(c; μ̃ r: i64.
        prim eq(r, 200002; μ̃ w: unit. <"wrong" | k>, μ̃ w: unit. <"200002" | k>))>))
"#;

#[test]
fn a_program_of_cut_runs_on_glade_and_as_an_executable_of_silos() {
    let dir = scratch("cut");
    std::fs::write(dir.join("count.cut"), CUT).unwrap();
    for runtime in [&[][..], &["--runtime", "silo"]] {
        let mut args = vec!["cut", "count.cut"];
        args.extend_from_slice(runtime);
        let (ok, out, err) = meadow(&dir, &args);
        assert!(ok, "{runtime:?}: {err}");
        assert_eq!(out, "counting\n200002", "{runtime:?}");
    }
    // What is not lowered yet is said, and nothing runs.
    std::fs::write(
        dir.join("handles.cut"),
        "cut 0\nentry t:Main/main\nanswer none\n\neffect t:Main/Ask { ask(unit) -> i64 }\n\n\
         def t:Main/main (; k: ptr) =\n  handle {\n    t:Main/Ask.ask(u: unit; r: ptr, h: ptr) => <r | apply(1; h)>;\n    \
         return(x: i64; h: ptr) => <x | h>\n  } in μ b.\n    perform t:Main/Ask.ask(unit; b)\n  ; k\n",
    )
    .unwrap();
    let (ok, out, err) = meadow(&dir, &["cut", "handles.cut"]);
    assert!(!ok && out.is_empty(), "{out}");
    assert!(err.contains("not lowered to AxCut yet"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the REPL prints, given `input` a line at a time.
fn repl(input: &str) -> String {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_meadow"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the meadow binary runs");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(input.as_bytes())
        .expect("written");
    let out = child.wait_with_output().expect("the REPL ends");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_repl_shows_a_definition_in_each_ir() {
    let defined = "fun sumTo (m : Int) : Int = let rec go n acc = if n == 0 then acc else go (n - 1) (acc + n) in go m 0\n";
    let all = repl(&format!("{defined}:ir sumTo\n:q\n"));
    let at = |what: &str| {
        all.find(what)
            .unwrap_or_else(|| panic!("no `{what}` in:\n{all}"))
    };
    // In the order a program passes through them.
    assert!(at("-- core") < at("-- cut") && at("-- cut") < at("-- axcut"));
    // One of them, asked for by name, with no heading: the definition's
    // value, and the definition its local function is lifted to.
    let cut = repl(&format!("{defined}:ir cut sumTo\n:q\n"));
    assert!(
        !cut.contains("-- cut") && !cut.contains("-- axcut"),
        "{cut}"
    );
    let from = cut.find("val meadow:").expect("the definition's value");
    let text = format!("cut 0\n\n{}", &cut[from..]);
    let read = meadow_cut::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(read.vals.len(), 1, "{text}");
    assert_eq!(read.defs.len(), 1, "the local function, at the top: {text}");
    // And something not defined is said to be.
    assert!(repl(":ir nope\n:q\n").contains("nothing called `nope` is defined"));
}
