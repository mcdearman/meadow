//! `Std.Process`: subprocesses and process-level operations.

mod common;
use common::eval_main_std;

#[test]
fn run_captures_output() {
    insta::assert_snapshot!(eval_main_std(
        "def main =\n\
        \x20 match run \"echo\" (\"hi\" :: Nil) with\n\
        \x20 | Ok o -> (outputStatus o, outputStdout o, succeeded o)\n\
        \x20 | Err e -> (0 - 1, e, False)\n"
    ), @"(0, \"hi\\n\", true)");
}

#[test]
fn nonexistent_program_is_an_err() {
    insta::assert_snapshot!(eval_main_std(
        "def main = match run \"this-program-does-not-exist-xyz\" Nil with | Ok o -> False | Err e -> True\n"
    ), @"true");
}

#[test]
fn effect_can_be_handled() {
    // a handler intercepts `Process` so the runtime never spawns anything
    insta::assert_snapshot!(eval_main_std(
        "def main =\n\
        \x20 handle run \"real\" Nil with {\n\
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
    insta::assert_snapshot!(eval_main_std("def main = currentPid () > 0\n"), @"true");
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
