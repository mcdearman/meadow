//! `meadow test` — build a package and run its `@test` functions.
//!
//! A test is `@test fun name u = …`. The runner calls each with `()`, in
//! declaration order, sharing one load of the program: top-level definitions are
//! evaluated once, as they are for `meadow run`.
//!
//! A test fails by performing `Std.Test.fail` — which `Std.Test`'s `assert` /
//! `assertEq` do for you. With no handler in the way the runtime turns that into
//! a stopped test carrying the message, so a failure reads like any other runtime
//! error and one failing test does not stop the others.

use crate::{pipeline, Profile};
use meadow_eval as eval;
use std::path::Path;

pub struct Options {
    /// Package directory (or a single `.mw` file).
    pub path: std::path::PathBuf,
    /// Run only tests whose name contains this.
    pub filter: Option<String>,
    /// Also run the standard library's own tests.
    pub std: bool,
    pub profile: Profile,
}

/// Returns `true` if everything that ran passed.
pub fn run(opts: &Options) -> Result<bool, String> {
    // `--std` is a request to test the standard library, and there need not be a
    // package to hang it off — `meadow test --std` from anywhere should work.
    // Without this, the default path of `.` would sweep every `.mw` file below
    // the working directory into one package and fail in a hundred ways.
    let linked = if opts.std && !is_package(Path::new(&opts.path)) {
        let (packages, diags) = crate::stdlib::compile_std(opts.profile.options());
        for d in &diags {
            eprintln!("{}: {}", d.filename, d.msg);
        }
        if !diags.is_empty() {
            return Err("the standard library did not compile".into());
        }
        crate::linker::Linker::link(packages)
    } else {
        let out = pipeline::build(Path::new(&opts.path), opts.profile.options());
        for d in &out.diagnostics {
            eprintln!("{}: {}", d.filename, d.msg);
        }
        let Some(linked) = out.linked else {
            return Err("could not build the package".into());
        };
        if !out.diagnostics.is_empty() {
            return Err("build failed".into());
        }
        linked
    };

    // The standard library carries its own tests. A package build should not be
    // made to wait on them unless they were asked for.
    let cases: Vec<_> = linked
        .tests
        .iter()
        .filter(|t| opts.std || &*t.package != "Std")
        .filter(|t| {
            opts.filter
                .as_ref()
                .is_none_or(|f| t.name.to_string().contains(f.as_str()))
        })
        .collect();

    let total = cases.len();
    println!("running {total} test{}", if total == 1 { "" } else { "s" });
    if total == 0 {
        println!();
        println!("test result: ok. 0 passed; 0 failed");
        return Ok(true);
    }

    let vars: Vec<_> = cases.iter().map(|t| t.var).collect();
    let results = eval::run_tests(&linked.program, &vars).map_err(|e| e.to_string())?;

    let mut failures = Vec::new();
    for (case, result) in cases.iter().zip(&results) {
        match result {
            Ok(_) => println!("test {} ... ok", case.name),
            Err(e) => {
                println!("test {} ... FAILED", case.name);
                // `msg`, not `to_string`: a failed assertion is not a "runtime error".
                failures.push((case.name, e.msg.clone()));
            }
        }
    }

    if !failures.is_empty() {
        println!();
        println!("failures:");
        println!();
        for (name, msg) in &failures {
            println!("---- {name} ----");
            println!("{msg}");
            println!();
        }
    }

    let passed = total - failures.len();
    println!();
    println!(
        "test result: {}. {passed} passed; {} failed",
        if failures.is_empty() { "ok" } else { "FAILED" },
        failures.len()
    );
    Ok(failures.is_empty())
}

/// Whether `path` names something the build system would recognise: a single
/// `.mw` file, or a directory with a manifest or a `src/`.
fn is_package(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "mw") {
        return true;
    }
    path.join("meadow.toml").is_file() || path.join("src").is_dir()
}

/// Link a set of packages without building from disk — used by the integration
/// tests, which assemble a package in memory. `package` selects whose tests to
/// run (`"Std"` for the standard library's own).
pub fn run_linked_in(
    linked: crate::linker::LinkedProgram,
    package: &str,
) -> Result<Vec<(String, Option<String>)>, String> {
    let cases: Vec<_> = linked
        .tests
        .iter()
        .filter(|t| &*t.package == package)
        .collect();
    let vars: Vec<_> = cases.iter().map(|t| t.var).collect();
    let results = eval::run_tests(&linked.program, &vars).map_err(|e| e.to_string())?;
    Ok(cases
        .iter()
        .zip(results)
        .map(|(c, r)| (c.name.to_string(), r.err().map(|e| e.msg)))
        .collect())
}

/// Every test that is not the standard library's.
pub fn run_linked(linked: crate::linker::LinkedProgram) -> Result<Vec<(String, Option<String>)>, String> {
    let cases: Vec<_> = linked.tests.iter().filter(|t| &*t.package != "Std").collect();
    let vars: Vec<_> = cases.iter().map(|t| t.var).collect();
    let results = eval::run_tests(&linked.program, &vars).map_err(|e| e.to_string())?;
    Ok(cases
        .iter()
        .zip(results)
        .map(|(c, r)| (c.name.to_string(), r.err().map(|e| e.msg)))
        .collect())
}
