//! The standard library's own `@test` functions.
//!
//! Each `Std` module carries its tests at the bottom of its own file, which is
//! what `meadow test --std` runs. Running them from `cargo test` as well means
//! the library is covered by the normal build, and means the language's test
//! machinery is exercised by something bigger than a fixture.

use meadow::{linker::Linker, stdlib, test, Engine, Options};

fn run() -> Vec<(String, Option<String>)> {
    let (packages, diags) = stdlib::std_packages(Options::debug());
    assert!(
        diags.is_empty(),
        "the standard library should compile cleanly: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    test::run_linked_in(Linker::link(packages), "Std", Engine::default(), Options::debug().opt).expect("the runner should not itself fail")
}

#[test]
fn every_standard_library_test_passes() {
    let results = run();
    let failures: Vec<_> = results
        .iter()
        .filter_map(|(name, err)| err.as_ref().map(|e| format!("  {name}: {e}")))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} failed:\n{}",
        failures.len(),
        results.len(),
        failures.join("\n")
    );
}

#[test]
fn the_library_actually_carries_tests() {
    // A guard against the collection silently breaking: a `@test` that stops
    // being seen would make the assertion above pass on an empty list.
    let results = run();
    assert!(
        results.len() >= 150,
        "expected the standard library's tests to be collected, found {}",
        results.len()
    );
}

#[test]
fn every_module_with_a_public_surface_is_tested() {
    // Not a coverage measure — just a check that no module was left out entirely
    // when tests were added, and that a new one does not quietly arrive untested.
    let (packages, _) = stdlib::std_packages(Options::debug());
    let tested: Vec<String> = packages[0]
        .tests
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    assert!(!tested.is_empty(), "no tests were collected at all");
}

// --- building `Std` as a package ---------------------------------------------

#[test]
fn the_standard_library_cannot_be_built_as_a_package() {
    // Pointing the build system at `lib/Std` used to produce a hundred
    // `already defined` errors — the embedded copy is injected alongside the
    // one on disk. The cause is worth naming; the symptoms are not.
    let out = meadow::pipeline::build(
        std::path::Path::new("../../lib/Std"),
        Options::debug(),
    );
    assert!(out.linked.is_none(), "it should not link");
    let msgs: Vec<_> = out.diagnostics.iter().map(|d| d.msg.clone()).collect();
    assert_eq!(msgs.len(), 1, "one diagnostic, not a hundred: {msgs:?}");
    assert!(
        msgs[0].contains("embedded") && msgs[0].contains("--std"),
        "the message should say why and what to do instead: {}",
        msgs[0]
    );
}
