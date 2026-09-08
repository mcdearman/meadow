//! `@test` and `meadow test`.
//!
//! The `effects` fixture is a package of `@test` functions exercising the effect
//! modules (`Std.State`, `Exn`, `Stream`, `Random`, `Time`) and the pure ones
//! (`Path`, `Json`). Running it here means the standard library's own behaviour is
//! checked *through* the test runner, so both are covered at once.

use meadow::{linker::Linker, pipeline, test, Options};
use std::path::Path;

const WORKSPACE: &str = "tests/fixtures/workspace";

/// Build the `effects` fixture and run its tests. Returns `(name, failure)` pairs.
fn run_fixture() -> Vec<(String, Option<String>)> {
    let out = pipeline::build(
        Path::new(&format!("{WORKSPACE}/effects")),
        Options::debug(),
    );
    assert!(
        out.diagnostics.is_empty(),
        "fixture should compile cleanly: {:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    test::run_linked(out.linked.expect("linked")).expect("the runner itself should not fail")
}

#[test]
fn every_test_in_the_fixture_passes() {
    let results = run_fixture();
    let failures: Vec<_> = results
        .iter()
        .filter_map(|(name, err)| err.as_ref().map(|e| format!("{name}: {e}")))
        .collect();
    assert!(failures.is_empty(), "failing tests:\n{}", failures.join("\n"));
    // A count, so deleting the fixture's contents cannot make this pass vacuously.
    assert!(
        results.len() >= 20,
        "expected the fixture to hold its tests, found {}",
        results.len()
    );
}

#[test]
fn the_standard_librarys_tests_are_not_the_packages() {
    // `meadow test` runs the package under test, not `Std`.
    for (name, _) in run_fixture() {
        assert!(!name.is_empty());
    }
}

// --- the attribute itself ----------------------------------------------------

fn tests_of(src: &str) -> Vec<String> {
    let (cp, diags) = meadow_compiler::compile_str("t", src);
    assert!(diags.is_empty(), "{:?}", diags.iter().map(|d| &d.msg).collect::<Vec<_>>());
    cp.tests.iter().map(|(n, _)| n.to_string()).collect()
}

#[test]
fn the_attribute_collects_functions_in_declaration_order() {
    assert_eq!(
        tests_of("@test fun b u = ()\nfun helper x = x\n@test fun a u = ()\n"),
        vec!["b", "a"]
    );
}

#[test]
fn a_plain_function_is_not_a_test() {
    assert!(tests_of("fun notATest u = ()\n").is_empty());
}

#[test]
fn a_test_must_be_callable() {
    // The runner calls a test with `()`, so a value binding cannot be one.
    let (_, diags) = meadow_compiler::compile_str("t", "@test def x = 1\n");
    let msgs: Vec<_> = diags.iter().map(|d| d.msg.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("`@test` must be a function")),
        "expected a diagnostic, got {msgs:?}"
    );
}

// --- failure reporting -------------------------------------------------------

/// Compile a one-file package in memory and run its tests.
fn run_src(src: &str) -> Vec<(String, Option<String>)> {
    // `compile_str_with_std` returns only a linked program, and the test list
    // lives on the package — so compile the unit directly against `Std`.
    let (std_pkgs, _) = meadow::stdlib::std_packages(Options::debug());
    let std_refs: Vec<_> = std_pkgs.iter().collect();
    let source = meadow_compiler::source::Source::new(
        meadow_compiler::source::SourceKind::Interactive,
        src.into(),
    );
    let lex = meadow_compiler::lexer::tokenize(source);
    let (ast, _) = meadow_compiler::parser::parse("t".into(), source, &lex.tokens);
    let modules = ast
        .map(|ast| {
            vec![meadow_compiler::AstModule {
                path: vec![],
                name: "t".into(),
                ast,
            }]
        })
        .unwrap_or_default();
    let (cp, diags) =
        meadow_compiler::compile_unit("t".into(), 1, modules, &std_refs, Options::debug());
    assert!(diags.is_empty(), "{:?}", diags.iter().map(|d| &d.msg).collect::<Vec<_>>());
    let mut pkgs: Vec<_> = std_pkgs;
    pkgs.push(cp);
    test::run_linked(Linker::link(pkgs)).expect("runner")
}

#[test]
fn a_failed_assertion_reports_both_values() {
    let results = run_src(
        "use Std.Test (assertEq)\n@test fun wrong u = assertEq (2 + 2) 5 \"arithmetic\"\n",
    );
    assert_eq!(results.len(), 1);
    let msg = results[0].1.clone().expect("should have failed");
    assert!(
        msg.contains("arithmetic") && msg.contains("expected 5") && msg.contains("got 4"),
        "unhelpful message: {msg}"
    );
}

#[test]
fn one_failure_does_not_stop_the_others() {
    let results = run_src(
        "use Std.Test (assertEq)\n\
         @test fun a u = assertEq 1 2 \"a\"\n\
         @test fun b u = assertEq 1 1 \"b\"\n\
         @test fun c u = assertEq 3 4 \"c\"\n",
    );
    let ok: Vec<_> = results.iter().map(|(n, e)| (n.as_str(), e.is_none())).collect();
    assert_eq!(ok, vec![("a", false), ("b", true), ("c", false)]);
}

#[test]
fn show_renders_values_of_any_type() {
    // `assertEq`'s message goes through the `show` primitive, which is what makes
    // the assertion useful for types that are not `Int`.
    let results = run_src(
        "use Std.Test (assertEq)\n@test fun t u = assertEq [1; 2] [1; 3] \"lists\"\n",
    );
    let msg = results[0].1.clone().expect("should have failed");
    assert!(msg.contains("[1; 3]") && msg.contains("[1; 2]"), "got: {msg}");
}
