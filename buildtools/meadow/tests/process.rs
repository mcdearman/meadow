//! `Std.Process`: subprocesses and process-level operations.

mod common;
use common::eval_main_std;

#[test]
fn run_captures_output() {
    insta::assert_snapshot!(eval_main_std(
        "def main =\n\
        \x20 match run \"echo\" [\"hi\"] with\n\
        \x20 | Ok o -> (outputStatus o, outputStdout o, succeeded o)\n\
        \x20 | Err e -> (0 - 1, e, False)\n"
    ), @"(0, \"hi\\n\", True)");
}

#[test]
fn nonexistent_program_is_an_err() {
    insta::assert_snapshot!(eval_main_std(
        "def main = match run \"this-program-does-not-exist-xyz\" [] with | Ok o -> False | Err e -> True\n"
    ), @"True");
}

#[test]
fn effect_can_be_handled() {
    // a handler intercepts `Process` so the runtime never spawns anything
    insta::assert_snapshot!(eval_main_std(
        "def main =\n\
        \x20 handle run \"real\" [] with {\n\
        \x20   spawn cmd k -> k (Ok (0, \"mocked\", \"\")),\n\
        \x20   return x -> x\n\
        \x20 }\n"
    ), @"Ok((0, \"mocked\", \"\"))");
}

#[test]
fn command_builder() {
    insta::assert_snapshot!(eval_main_std(
        "def cmd = withArg \"hi\" (withArg \"-n\" (command \"echo\"))\n\
         def main = match spawn cmd with | Ok o -> outputStdout o | Err e -> e\n"
    ), @"\"hi\"");
}

#[test]
fn current_pid_is_positive() {
    insta::assert_snapshot!(eval_main_std("def main = currentPid () > 0\n"), @"True");
}

#[test]
fn env_roundtrip() {
    insta::assert_snapshot!(eval_main_std(
        "def main =\n\
        \x20 let a = getEnv \"MEADOW_TEST_VAR_DOES_NOT_EXIST\" in\n\
        \x20 let b = setEnv (\"MEADOW_TEST_VAR_DOES_NOT_EXIST\", \"x\") in\n\
        \x20 let c = getEnv \"MEADOW_TEST_VAR_DOES_NOT_EXIST\" in\n\
        \x20 let d = removeEnv \"MEADOW_TEST_VAR_DOES_NOT_EXIST\" in\n\
        \x20 (a, c)\n"
    ), @"(None, Just(\"x\"))");
}

#[test]
fn many_arguments_cross_the_boundary_as_a_vector() {
    // Forty arguments is past one chunk, and `withArg` builds the vector by
    // pushing, so the runtime reads a shape it did not build itself.
    let src = "def cmd = V.foldl (\\c i -> withArg (show i) c) (command \"echo\") (V.range 0 40)\n\
               def main = match spawn cmd with | Ok o -> String.byteLength (outputStdout o) | Err e -> 0\n";
    let src = format!("use Std.Collections.Vector as V\nuse Std.String as String\n{src}");
    // "0 1 ... 39\n": ten one-digit and thirty two-digit numbers, 39 spaces, a newline.
    assert_eq!(eval_main_std(&src), "110");
    assert_eq!(common::cek_main_std(&src), "110");
}
