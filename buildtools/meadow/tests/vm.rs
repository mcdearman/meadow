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
    let lowered = meadow_seq::lower_program(&std.program, Options::debug().opt);
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
    let image = runtime::compile(&std.program, Options::debug().opt).expect("the standard library should compile");
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

    let cek = runtime::run_tests(&std.program, &vars, Engine::Cek, Options::debug().opt).expect("the CEK runner");
    let vm = runtime::run_tests(&std.program, &vars, Engine::Vm, Options::debug().opt).expect("the VM runner");

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
    //
    // At both levels, because the levels differ *here* — `-O2`'s decision trees
    // are a lowering decision, and this is the test that can say so.
    for opt in [meadow::OptLevel::O1, meadow::OptLevel::O2] {
        let std = std_program();
        let lowered = meadow_seq::lower_program(&std.program, opt);

        let mut checked = 0;
        for (name, var) in &std.tests {
            let program = calling(&std.program, *var);
            let lowered_one = meadow_seq::lower_program(&program, opt);
            let cek = meadow_eval::run(&program);
            let axcut = meadow_seq::machine::Machine::run(&lowered_one.program, 200_000_000);
            let at = opt.name();
            match (&cek, &axcut) {
                (Ok(a), Ok(b)) if a.to_string() == b.to_string() => checked += 1,
                (Err(_), Err(_)) => checked += 1,
                (Ok(a), Err(e)) => panic!("{name} at {at}: CEK {a}, AxCut failed: {}", e.msg),
                (Ok(a), Ok(b)) => panic!("{name} at {at}: CEK {a}, AxCut {b}"),
                (Err(e), Ok(b)) => panic!("{name} at {at}: CEK failed: {}, AxCut {b}", e.msg),
            }
        }
        assert_eq!(checked, std.tests.len());
        assert!(!lowered.program.defs.is_empty());
    }
}

/// A program whose entry point calls `test` with `()`, which is what the test
/// runner does.
fn calling(program: &core::Program, test: core::Var) -> core::Program {
    let entry = meadow_compiler::hir::VarId::synthetic(0);
    let mut defs = program.defs.clone();
    defs.push(core::Def {
        var: entry,
        name: "<test>".into(),
        poly: core::Poly::mono(core::unknown()),
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

/// Every optimization level computes the same answers.
///
/// The back end's optional passes are the ones nothing else checks: a program
/// only reaches `-O2` when someone builds for release, and a decision tree that
/// picks the wrong arm is a wrong answer rather than a crash. So the standard
/// library's own tests are run at each level and required to agree with the CEK
/// machine, which has no levels at all.
#[test]
fn every_opt_level_agrees_with_the_cek() {
    use meadow::OptLevel;

    let std = std_program();
    let vars: Vec<core::Var> = std.tests.iter().map(|(_, v)| *v).collect();
    let cek = runtime::run_tests(&std.program, &vars, Engine::Cek, OptLevel::O1)
        .expect("the CEK runner");

    for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
        let vm = runtime::run_tests(&std.program, &vars, Engine::Vm, opt)
            .unwrap_or_else(|e| panic!("the VM runner at {}: {e}", opt.name()));

        let mut agreed = 0;
        let mut disagreed = Vec::new();
        for ((name, _), (a, b)) in std.tests.iter().zip(cek.iter().zip(vm.iter())) {
            match (a, b) {
                (Ok(x), Ok(y)) if x == y => agreed += 1,
                (Err(_), Err(_)) => agreed += 1,
                // The documented divergence: no native effects in the VM.
                (Ok(_), Err(e)) if e.starts_with("unhandled effect") => {}
                (Ok(x), Ok(y)) => disagreed.push(format!("{name}: CEK {x}, {} {y}", opt.name())),
                (Ok(x), Err(e)) => {
                    disagreed.push(format!("{name}: CEK {x}, {} failed: {e}", opt.name()))
                }
                (Err(e), Ok(y)) => {
                    disagreed.push(format!("{name}: CEK failed: {e}, {} {y}", opt.name()))
                }
            }
        }
        assert!(
            disagreed.is_empty(),
            "{} of {} standard library tests disagree at {}:\n{}",
            disagreed.len(),
            std.tests.len(),
            opt.name(),
            disagreed.join("\n")
        );
        assert!(
            agreed * 4 > std.tests.len() * 3,
            "only {agreed} of {} agreed at {}",
            std.tests.len(),
            opt.name()
        );
    }
}

/// `-O2` compiles a `match` to one `switch` rather than one per arm.
///
/// Counting `switch` statements is the observable difference, and it is what
/// would notice the gate being wired to nothing — which is the way an option
/// like this usually fails.
#[test]
fn case_trees_are_what_o2_turns_on() {
    use meadow::OptLevel;

    let std = std_program();
    let counts: Vec<usize> = [OptLevel::O1, OptLevel::O2]
        .iter()
        .map(|opt| {
            let lowered = meadow_seq::lower_program(&std.program, *opt);
            lowered
                .program
                .defs
                .iter()
                .map(|d| switches(&d.block.body))
                .sum()
        })
        .collect();

    // A tree replaces N single-arm switches with one N-arm switch, so the
    // *count* falls even though the same tags are tested.
    assert!(
        counts[1] < counts[0],
        "O1 has {} switches and O2 has {} — the gate is doing nothing",
        counts[0],
        counts[1]
    );
}

/// How many `switch` statements a block contains, counting into every branch.
fn switches(s: &meadow_seq::Statement) -> usize {
    use meadow_seq::Statement::*;
    match s {
        Substitute(_, b) => switches(&b.body),
        Jump(_) => 0,
        Let { rest, .. } => switches(rest),
        Switch { arms, default, .. } => {
            1 + arms.iter().map(|(_, b)| switches(&b.body)).sum::<usize>()
                + switches(&default.body)
        }
        New { methods, rest, .. } => {
            methods.iter().map(|b| switches(&b.body)).sum::<usize>() + switches(rest)
        }
        Invoke(..) => 0,
        Extern { blocks, .. } => blocks.iter().map(|b| switches(&b.body)).sum(),
        Handle { rest, .. } | Unhandle { rest, .. } => switches(rest),
        Perform { .. } => 0,
        Error(_) => 0,
    }
}
