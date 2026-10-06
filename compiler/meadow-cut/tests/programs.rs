//! Cut programs as a front end writes them: read, printed and read back the
//! same, and run by the reference interpreter -- and, those the lowering to
//! AxCut takes, by the AxCut machine, to the same answer.

use meadow_cut::interp::{Options, Outcome, run};
use meadow_cut::{parse, print};

/// `text` read, printed and read again -- the same program both times --
/// and run.
#[track_caller]
fn runs(text: &str) -> Outcome {
    let p = parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let printed = print::program(&p);
    let again = parse(&printed).unwrap_or_else(|e| panic!("{e}\n{printed}"));
    assert_eq!(p, again, "printed and read back:\n{printed}");
    let out = run(&p, &Options::default()).unwrap_or_else(|(e, out)| panic!("{e}\n{out:?}"));
    lowered_agrees(&p, &out);
    out
}

/// Where `p` is one the lowering to AxCut takes and its answer is a string,
/// the AxCut machine answers what the interpreter printed.
#[track_caller]
fn lowered_agrees(p: &meadow_cut::Program, out: &Outcome) {
    use meadow_axcut::machine::{Machine, Value};
    let Ok(lowered) = meadow_cut::lower::lower(p) else {
        return;
    };
    if p.answer != meadow_cut::Answer::Str {
        return;
    }
    match Machine::run(&lowered, 100_000_000) {
        Ok(Value::Str(s)) => assert_eq!(&*s, out.output, "what the AxCut machine answers"),
        Ok(v) => panic!("the AxCut machine answered {v}, not a string"),
        Err(e) => panic!("the AxCut machine: {e:?}\n{}", lowered.pretty()),
    }
}

/// What `text` fails with when run.
#[track_caller]
fn fails(text: &str) -> String {
    let p = parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    match run(&p, &Options::default()) {
        Ok(out) => panic!("ran to the end: {out:?}"),
        Err((e, _)) => e,
    }
}

const CONSOLE: &str = "
native {
  idyll:Prelude/Console.putStr = Console.writeOutput
  idyll:Prelude/Console.putErr = Console.writeError
  idyll:Prelude/Process.exit = Process.exit
}

effect idyll:Prelude/Console { putStr(str) -> unit; putErr(str) -> unit }
effect idyll:Prelude/Process { exit(i64) -> unit }
";

#[test]
fn a_definition_is_called_and_a_test_branches() {
    let out = runs(&format!(
        "cut 0
entry idyll:Main/\"#entry\"
answer none
{CONSOLE}
def idyll:Main/double (n: i64; k: ptr) =
  prim mul(n, 2; k)

def idyll:Main/\"#entry\" (; k: ptr) =
  idyll:Main/double(21; μ̃ n: i64.
    prim lt(n, 50;
      μ̃ u: unit. perform idyll:Prelude/Console.putStr(\"big\\n\"; k),
      μ̃ u: unit. perform idyll:Prelude/Console.putStr(\"small\\n\"; k)))
"
    ));
    assert_eq!(out.output, "small\n");
}

const NAT: &str = "
data idyll:Main/Nat { Z; S(ptr) }

def idyll:Main/plus (x: ptr, m: ptr; k: ptr) =
  <x | case {
    idyll:Main/Nat.Z => <m | k>;
    idyll:Main/Nat.S(n: ptr) =>
      idyll:Main/plus(n, m; μ̃ v: ptr. <idyll:Main/Nat.S(v) | k>)
  }>

def idyll:Main/count (x: ptr; k: ptr) =
  <x | case {
    idyll:Main/Nat.Z => <0 | k>;
    idyll:Main/Nat.S(n: ptr) => idyll:Main/count(n; μ̃ c: i64. prim add(c, 1; k))
  }>
";

#[test]
fn data_is_built_and_taken_apart_and_a_str_is_the_answer() {
    let out = runs(&format!(
        "cut 0
entry idyll:Main/main
answer str
{NAT}
def idyll:Main/main (; k: ptr) =
  idyll:Main/plus(idyll:Main/Nat.S(idyll:Main/Nat.Z), idyll:Main/Nat.S(idyll:Main/Nat.S(idyll:Main/Nat.Z)); μ̃ r: ptr.
    idyll:Main/count(r; μ̃ c: i64.
      prim eq(c, 3; μ̃ u: unit. <\"wrong\" | k>, μ̃ u: unit. <\"three\" | k>)))
"
    ));
    assert_eq!(out.output, "three");
}

#[test]
fn a_deep_handler_resumes_and_the_clause_gets_the_answer() {
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

effect idyll:Main/Ask { ask(unit) -> i64 }

def idyll:Main/main (; k: ptr) =
  handle {
    idyll:Main/Ask.ask(u: unit; r: ptr, k2: ptr) => <r | apply(10; k2)>;
    return(x: i64; k3: ptr) => <x | k3>
  } in μ b.
    perform idyll:Main/Ask.ask(unit; μ̃ a: i64.
      perform idyll:Main/Ask.ask(unit; μ̃ c: i64. prim add(a, c; b)))
  ; μ̃ total: i64. prim eq(total, 20; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <\"yes\" | k>)
",
    );
    assert_eq!(out.output, "yes");
}

#[test]
fn a_many_operation_is_resumed_twice_and_every_path_is_counted() {
    // `choose` resumed with true and with false; each path answers 1 or 2, and
    // the clause adds what the two resumptions came back with.
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

effect idyll:Main/Choose { @many choose(unit) -> bool }

def idyll:Main/main (; k: ptr) =
  handle {
    idyll:Main/Choose.choose(u: unit; r: ptr, k2: ptr) =>
      <r | apply(true; μ̃ a: i64.
        <r | apply(false; μ̃ b2: i64. prim add(a, b2; k2))>)>;
    return(x: i64; k3: ptr) => <x | k3>
  } in μ b.
    perform idyll:Main/Choose.choose(unit; μ̃ c: bool.
      prim if(c; μ̃ u: unit. <2 | b>, μ̃ u: unit. <1 | b>))
  ; μ̃ n: i64. prim eq(n, 3; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <\"three\" | k>)
",
    );
    assert_eq!(out.output, "three");
}

#[test]
fn a_val_is_computed_once_and_a_mu_runs_where_an_argument_is_wanted() {
    let out = runs(&format!(
        "cut 0
entry idyll:Main/main
answer none
{CONSOLE}
val idyll:Main/forty : i64 =
  perform idyll:Prelude/Console.putStr(\"once\\n\"; μ̃ u: unit. prim add(40, 2; halt))

def idyll:Main/id <'a = d> (d: desc, x: 'a; k: ptr) =
  <x | k>

def idyll:Main/main (; k: ptr) =
  idyll:Main/id(desc(i64), μ j. <idyll:Main/forty | j>; μ̃ v: i64.
    idyll:Main/id(desc(i64), idyll:Main/forty; μ̃ w: i64.
      prim add(v, w; μ̃ s: i64.
        prim eq(s, 84;
          μ̃ u: unit. perform idyll:Prelude/Console.putStr(\"no\\n\"; k),
          μ̃ u: unit. perform idyll:Prelude/Console.putStr(\"84\\n\"; k)))))
"
    ));
    assert_eq!(out.output, "once\n84\n", "the val's effect happens once");
}

#[test]
fn an_object_is_called_through_its_method() {
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

def idyll:Main/main (; k: ptr) =
  let one: i64 = 1 in
  <cocase { apply(x: i64; k1: ptr) => prim add(x, one; k1) } | apply(41; μ̃ v: i64.
    prim eq(v, 42; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <\"42\" | k>))>
",
    );
    assert_eq!(out.output, "42");
}

#[test]
fn a_program_gives_up_on_standard_error() {
    let out = runs(&format!(
        "cut 0
entry idyll:Main/main
answer none
{CONSOLE}
def idyll:Main/main (; k: ptr) =
  perform idyll:Prelude/Console.putStr(\"before\\n\"; μ̃ u: unit.
    perform idyll:Prelude/Console.putErr(\"gave up\\n\"; μ̃ u2: unit.
      perform idyll:Prelude/Process.exit(3; μ̃ u3: unit. perform idyll:Prelude/Console.putStr(\"after\\n\"; k))))
"
    ));
    assert_eq!(out.output, "before\n");
    assert_eq!(out.errors, "gave up\n");
    assert_eq!(out.status, 3);
}

#[test]
fn a_loop_of_a_million_turns_runs_in_constant_host_stack() {
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

def idyll:Main/down (n: i64, acc: i64; k: ptr) =
  prim eq(n, 0;
    μ̃ u: unit. prim sub(n, 1; μ̃ m: i64. prim add(acc, 2; μ̃ a: i64. idyll:Main/down(m, a; k))),
    μ̃ u: unit. <acc | k>)

def idyll:Main/main (; k: ptr) =
  idyll:Main/down(1000000, 0; μ̃ r: i64.
    prim eq(r, 2000000; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <\"done\" | k>))
",
    );
    assert_eq!(out.output, "done");
}

#[test]
fn integer_arithmetic_wraps_and_division_by_zero_is_an_error() {
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

def idyll:Main/main (; k: ptr) =
  prim add(9223372036854775807, 1; μ̃ x: i64.
    prim div(-7, 2; μ̃ q: i64.
      prim mod(-7, 2; μ̃ r: i64.
        prim eq(x, -9223372036854775808; μ̃ u: unit. <\"no wrap\" | k>, μ̃ u: unit.
          prim eq(q, -3; μ̃ u: unit. <\"no truncation\" | k>, μ̃ u: unit.
            prim eq(r, -1; μ̃ u: unit. <\"wrong sign\" | k>, μ̃ u: unit. <\"ok\" | k>))))))
",
    );
    assert_eq!(out.output, "ok");
    let e = fails(
        "cut 0
entry idyll:Main/main
answer none

def idyll:Main/main (; k: ptr) =
  prim div(1, 0; k)
",
    );
    assert_eq!(e, "division by zero");
}

#[test]
fn an_unhandled_operation_not_in_the_native_table_is_an_error() {
    let e = fails(
        "cut 0
entry idyll:Main/main
answer none

effect idyll:Main/Ask { ask(unit) -> i64 }

def idyll:Main/main (; k: ptr) =
  perform idyll:Main/Ask.ask(unit; μ̃ n: i64. <unit | k>)
",
    );
    assert!(e.contains("no handler answers it"), "{e}");
}

#[test]
fn the_file_system_is_written_read_and_cleared_through_natives() {
    let dir = std::env::temp_dir().join(format!("meadow-cut-fs-{}", std::process::id()));
    let dir = dir.to_string_lossy().replace('\\', "/");
    let out = runs(&format!(
        "cut 0
entry idyll:Main/main
answer str

data idyll:Prelude/Result {{ Ok(ptr); Err(str) }}
roles {{ ok = idyll:Prelude/Result.Ok, err = idyll:Prelude/Result.Err }}

native {{
  idyll:Prelude/Fs.createDirAll = Fs.createDirAll
  idyll:Prelude/Fs.writeString = Fs.writeString
  idyll:Prelude/Fs.appendString = Fs.appendString
  idyll:Prelude/Fs.readToString = Fs.readToString
  idyll:Prelude/Fs.exists = Fs.exists
  idyll:Prelude/Fs.removeDirAll = Fs.removeDirAll
}}

effect idyll:Prelude/Fs {{ createDirAll(str) -> ptr; writeString(str, str) -> ptr; appendString(str, str) -> ptr;
  readToString(str) -> ptr; exists(str) -> bool; removeDirAll(str) -> ptr }}

def idyll:Main/main (; k: ptr) =
  perform idyll:Prelude/Fs.createDirAll(\"{dir}\"; μ̃ a: ptr.
    perform idyll:Prelude/Fs.writeString(\"{dir}/f.txt\", \"ab\"; μ̃ b: ptr.
      perform idyll:Prelude/Fs.appendString(\"{dir}/f.txt\", \"cd\"; μ̃ c: ptr.
        perform idyll:Prelude/Fs.readToString(\"{dir}/f.txt\"; μ̃ r: ptr.
          <r | case {{
            idyll:Prelude/Result.Ok(text: str) =>
              perform idyll:Prelude/Fs.removeDirAll(\"{dir}\"; μ̃ d: ptr.
                perform idyll:Prelude/Fs.exists(\"{dir}\"; μ̃ there: bool.
                  prim if(there; μ̃ u: unit. <text | k>, μ̃ u: unit. <\"still there\" | k>)));
            idyll:Prelude/Result.Err(why: str) => <why | k>
          }}>))))
"
    ));
    assert_eq!(out.output, "abcd");
}

#[test]
fn quoted_segments_versions_and_comments_read() {
    let p = parse(
        "cut 0 -- the version
data idyll:Prelude/List <'a> { Nil; \"::\"('a, ptr) }
roles { nil = idyll:Prelude/List.Nil, cons = idyll:Prelude/List.\"::\" }
def idyll:Json@1.2.0-alpha.1/parse (s: str; k: ptr) = <s | k>
def idyll:Prelude/Nat.\"x'\" (; k: ptr) = <'c' | k>
",
    )
    .unwrap();
    assert_eq!(p.datas[0].ctors[1].0, "::");
    assert_eq!(p.roles[1].1.to_string(), "idyll:Prelude/List.\"::\"");
    assert_eq!(p.defs[0].symbol.package, "Json@1.2.0-alpha.1");
    assert_eq!(p.defs[1].symbol.path, ["Nat", "x'"]);
}

#[test]
fn a_reader_refuses_what_it_cannot_read() {
    assert!(parse("cut 1\n").unwrap_err().contains("version 1"));
    let e = parse("cut 0\ndef idyll:A/f (; k: ptr) = <idyll:A/K(1) | k>\n").unwrap_err();
    assert!(e.contains("no data declares it"), "{e}");
    let e = parse("cut 0\ndef idyll:A/f (; k: ptr) = <1 | \n").unwrap_err();
    assert!(e.starts_with("3:1:"), "says where: {e}");
}

#[test]
fn what_is_not_lowered_to_axcut_yet_is_refused_and_says_what() {
    let lowered = |text: &str| meadow_cut::lower::lower(&parse(text).expect("a program"));
    let effects = lowered(&format!(
        "cut 0
entry idyll:Main/main
answer none
{CONSOLE}
def idyll:Main/main (; k: ptr) =
  perform idyll:Prelude/Console.putStr(\"hello\\n\"; k)
"
    ));
    assert!(
        effects.as_ref().is_err_and(|e| e.contains("effects")),
        "{effects:?}"
    );
    let generic = lowered(
        "cut 0
entry idyll:Main/main
answer none

def idyll:Main/id <'a = d> (d: desc, x: 'a; k: ptr) =
  <x | k>

def idyll:Main/main (; k: ptr) =
  idyll:Main/id(desc(i64), 1; k)
",
    );
    assert!(
        generic.as_ref().is_err_and(|e| e.contains("generic")),
        "{generic:?}"
    );
}

#[test]
fn a_top_level_value_is_computed_once_and_read_on_the_axcut_machine() {
    // The start block computes each value in the order written, the second
    // of the first, before the entry runs.
    let out = runs(
        "cut 0
entry idyll:Main/main
answer str

val idyll:Main/twenty : i64 =
  prim add(19, 1; halt)

val idyll:Main/pair : ptr =
  <#tuple(idyll:Main/twenty, \"x\") | halt>

def idyll:Main/main (; k: ptr) =
  <idyll:Main/pair | case {
    #tuple(n: i64, s: str) =>
      prim add(n, idyll:Main/twenty; μ̃ m: i64.
        prim eq(m, 40; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <s | k>))
  }>
",
    );
    assert_eq!(out.output, "x");
}
