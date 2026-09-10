//! The standard library, through the whole back end.
//!
//! `meadow_rts`'s own differential tests are hand-written programs, one
//! construct at a time. This is the other end: the largest body of Meadow that
//! exists, compiled the way `meadow test --std` compiles it, and put through
//! `core` → AxCut → bytecode → the VM.
//!
//! It answers three separate questions, and they are worth keeping apart —
//! when one fails, which one it was says where to look.
//!
//! * **Does everything lower?** [`the_whole_standard_library_lowers`] asserts
//!   the `Unsupported` set is empty across the entire library. A statement about
//!   the pass, not about any run, and the one that would catch a `core`
//!   construct nobody thought to test.
//! * **Does it compile?** [`the_whole_standard_library_reaches_bytecode`] runs
//!   the register allocator over every block. It has no spilling, so a block
//!   wanting more than 256 registers is a hard error — this is what would notice.
//! * **Does it mean the same thing?** [`the_vm_agrees_with_the_cek`] runs each
//!   `@test` on both machines and requires the same answer.
//!
//! The last has to allow for the one documented divergence: the VM does not
//! discharge an *unhandled* `Fs`, `Process`, `Random` or `Time` operation, where
//! the CEK reaches the real world. Those are counted and reported rather than
//! ignored, and a floor on the number that *do* agree keeps the allowance from
//! quietly swallowing everything.

use meadow::{linker::Linker, runtime, stdlib, Engine, Options};
use meadow_compiler::core;

struct Std {
    program: core::Program,
    tests: Vec<(String, core::Var)>,
}

fn std_program() -> Std {
    let (packages, diags) = stdlib::std_packages(Options::debug());
    assert!(
        diags.is_empty(),
        "the standard library should compile cleanly: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = Linker::link(packages);
    let tests = linked
        .tests
        .iter()
        .map(|t| (format!("{}.{}", t.package, t.name), t.var))
        .collect();
    Std {
        program: linked.program,
        tests,
    }
}

#[test]
fn the_whole_standard_library_lowers() {
    let std = std_program();
    let lowered = meadow_seq::lower_program(&std.program);
    assert!(
        lowered.unsupported.is_empty(),
        "the standard library uses `core` constructs the lowering does not \
         translate: {:?}",
        lowered.unsupported
    );
    // A guard against the assertion above being vacuous.
    assert!(
        lowered.program.defs.len() > 500,
        "only {} blocks — did the library fail to link?",
        lowered.program.defs.len()
    );
}

#[test]
fn the_whole_standard_library_reaches_bytecode() {
    let std = std_program();
    let image = runtime::compile(&std.program).expect("the standard library should compile");
    assert!(image.code.len() > 10_000, "only {} instructions", image.code.len());
    assert!(
        image.regs as usize <= 256,
        "a block wanted {} registers",
        image.regs
    );
    // Worth printing: it is the number that says whether the missing register
    // allocator is a real problem yet.
    println!(
        "{} instructions, {} registers at the high-water mark",
        image.code.len(),
        image.regs
    );
}

#[test]
fn the_vm_agrees_with_the_cek() {
    let std = std_program();
    let vars: Vec<core::Var> = std.tests.iter().map(|(_, v)| *v).collect();

    let cek = runtime::run_tests(&std.program, &vars, Engine::Cek).expect("the CEK runner");
    let vm = runtime::run_tests(&std.program, &vars, Engine::Vm).expect("the VM runner");

    let mut agreed = 0;
    let mut native = Vec::new();
    let mut disagreed = Vec::new();

    for ((name, _), (a, b)) in std.tests.iter().zip(cek.iter().zip(vm.iter())) {
        match (a, b) {
            (Ok(x), Ok(y)) if x == y => agreed += 1,
            // Both failed. The messages need not match — the CEK reports a
            // failed assertion and the VM may report the same thing a different
            // way — but a test that fails on both is not a translation bug.
            (Err(_), Err(_)) => agreed += 1,
            // The one documented divergence.
            (Ok(_), Err(e)) if e.starts_with("unhandled effect") => {
                native.push(format!("{name}: {e}"));
            }
            (Ok(x), Ok(y)) => disagreed.push(format!("{name}: CEK {x}, VM {y}")),
            (Ok(x), Err(e)) => disagreed.push(format!("{name}: CEK {x}, VM failed: {e}")),
            (Err(e), Ok(y)) => disagreed.push(format!("{name}: CEK failed: {e}, VM {y}")),
        }
    }

    println!(
        "{agreed} agreed, {} needed a native effect, {} disagreed",
        native.len(),
        disagreed.len()
    );
    assert!(
        disagreed.is_empty(),
        "{} of {} standard library tests disagree:\n{}",
        disagreed.len(),
        std.tests.len(),
        disagreed.join("\n")
    );
    // The allowance above must not be doing the work.
    assert!(
        agreed * 4 > std.tests.len() * 3,
        "only {agreed} of {} agreed; {} needed a native effect:\n{}",
        std.tests.len(),
        native.len(),
        native.join("\n")
    );
}

#[test]
fn the_axcut_machine_agrees_too() {
    // The middle of the pipeline, so a disagreement above can be attributed.
    // If this passes and `the_vm_agrees_with_the_cek` does not, the bug is in
    // code generation; if both fail, it is in the lowering.
    let std = std_program();
    let lowered = meadow_seq::lower_program(&std.program);

    let mut checked = 0;
    for (name, var) in &std.tests {
        let program = calling(&std.program, *var);
        let lowered_one = meadow_seq::lower_program(&program);
        let cek = meadow_eval::run(&program);
        let axcut = meadow_seq::machine::Machine::run(&lowered_one.program, 200_000_000);
        match (&cek, &axcut) {
            (Ok(a), Ok(b)) if a.to_string() == b.to_string() => checked += 1,
            (Err(_), Err(_)) => checked += 1,
            (Ok(a), Err(e)) => panic!("{name}: CEK {a}, AxCut failed: {}", e.msg),
            (Ok(a), Ok(b)) => panic!("{name}: CEK {a}, AxCut {b}"),
            (Err(e), Ok(b)) => panic!("{name}: CEK failed: {}, AxCut {b}", e.msg),
        }
    }
    assert_eq!(checked, std.tests.len());
    assert!(!lowered.program.defs.is_empty());
}

/// A program whose entry point calls `test` with `()`, which is what the test
/// runner does.
fn calling(program: &core::Program, test: core::Var) -> core::Program {
    let entry = meadow_compiler::hir::VarId::fresh();
    let mut defs = program.defs.clone();
    defs.push(core::Def {
        var: entry,
        name: "<test>".into(),
        term: core::Term::App(
            std::sync::Arc::new(core::Term::Var(test)),
            std::sync::Arc::new(core::Term::Lit(core::Lit::Unit)),
        ),
    });
    core::Program {
        defs,
        entry: Some(entry),
        ctor_fields: program.ctor_fields.clone(),
    }
}
