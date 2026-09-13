//! Which machine runs the program.
//!
//! Two back ends, one interface. The **bytecode VM** is the default: `core` is
//! lowered to the AxCut IR, compiled to registers, and run by `meadow_rts`. The
//! **CEK machine** (`meadow_eval`) is still there behind `--cek`, and is still
//! the specification — if the two disagree the CEK is right and the VM has a
//! bug, so the flag is the first thing to reach for when a program does
//! something inexplicable.
//!
//! Both are held to that by tests rather than by intention: `tests/vm.rs` runs
//! the whole standard library through both and requires every answer to match.
//!
//! # What the VM cannot do yet
//!
//! Discharge an *unhandled* `Fs`, `Process`, `Random` or `Time` operation. The
//! CEK reaches the real world for those; the VM reports them and stops. A
//! program that handles its effects — which is every test in the standard
//! library, by design — is unaffected, and one that reads a file is not, so that
//! is what `--cek` is for until the natives are ported.

use meadow_compiler::{core, OptLevel};
use std::fmt;

/// Which machine to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// Bytecode, registers and a copying collector.
    #[default]
    Vm,
    /// The CEK abstract machine — small, slow, and the definition of what a
    /// program means.
    Cek,
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Engine::Vm => "vm",
            Engine::Cek => "cek",
        })
    }
}

/// No practical limit. The bound exists so a test harness can ask for one.
const UNBOUNDED: u64 = u64::MAX;

/// Evaluate the program's entry point, and render the result.
///
/// A string rather than a value because the two machines have different value
/// types, and every caller here wanted the rendering — which the two produce
/// identically on purpose.
pub fn run(program: &core::Program, engine: Engine, opt: OptLevel) -> Result<String, String> {
    match engine {
        Engine::Cek => meadow_eval::run(program)
            .map(|v| v.to_string())
            .map_err(|e| e.msg),
        Engine::Vm => {
            let image = compile(program, opt)?;
            meadow_rts::run(&image, UNBOUNDED).map_err(|e| e.msg)
        }
    }
}

/// What the bytecode VM's collector did during a run.
#[derive(Debug, Clone, Copy)]
pub struct GcStats {
    pub collections: u64,
    /// Heap slots allocated and copied by collections, in total.
    pub allocated: u64,
    pub copied: u64,
    pub gc_nanos: u64,
    pub run_nanos: u64,
    /// Semispace size, and slots held in compact regions, at the end.
    pub heap_slots: usize,
    pub region_slots: usize,
}

impl fmt::Display for GcStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = |slots: u64| human_bytes(slots * meadow_rts::heap::SLOT_BYTES as u64);
        let share = if self.run_nanos == 0 {
            0.0
        } else {
            100.0 * self.gc_nanos as f64 / self.run_nanos as f64
        };
        write!(
            f,
            "gc: {} collections, {:.1} ms collecting ({share:.1}% of {:.1} ms)\n\
             gc: {} allocated, {} copied by collections\n\
             gc: heap {}, compact regions {}",
            self.collections,
            self.gc_nanos as f64 / 1e6,
            self.run_nanos as f64 / 1e6,
            bytes(self.allocated),
            bytes(self.copied),
            bytes(self.heap_slots as u64),
            bytes(self.region_slots as u64),
        )
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut x = n as f64;
    let mut unit = 0;
    while x >= 1024.0 && unit + 1 < UNITS.len() {
        x /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{x:.1} {}", UNITS[unit]) }
}

/// [`run`], and on the VM what its collector did. The CEK reference-counts,
/// so it has nothing to report.
pub fn run_with_stats(
    program: &core::Program,
    engine: Engine,
    opt: OptLevel,
) -> (Result<String, String>, Option<GcStats>) {
    match engine {
        Engine::Cek => (run(program, engine, opt), None),
        Engine::Vm => {
            let image = match compile(program, opt) {
                Ok(image) => image,
                Err(e) => return (Err(e), None),
            };
            let Some(entry) = image.entry else {
                return (Err("program has no entry point".to_string()), None);
            };
            let started = std::time::Instant::now();
            let mut vm = meadow_rts::Vm::new(&image);
            let result = vm.run(entry, UNBOUNDED).map(|v| vm.show(v)).map_err(|e| e.msg);
            let heap = vm.heap();
            let stats = GcStats {
                collections: heap.collections,
                allocated: heap.allocated,
                copied: heap.copied,
                gc_nanos: heap.gc_nanos,
                run_nanos: started.elapsed().as_nanos() as u64,
                heap_slots: heap.capacity(),
                region_slots: heap.region_slots(),
            };
            (result, Some(stats))
        }
    }
}

/// Call each of `tests` with `()`, in order, sharing one build.
///
/// Each gets its own result: a failing test does not stop the rest.
pub fn run_tests(
    program: &core::Program,
    tests: &[core::Var],
    engine: Engine,
    opt: OptLevel,
) -> Result<Vec<Result<String, String>>, String> {
    match engine {
        Engine::Cek => Ok(meadow_eval::run_tests(program, tests)
            .map_err(|e| e.msg)?
            .into_iter()
            .map(|r| r.map(|v| v.to_string()).map_err(|e| e.msg))
            .collect()),

        Engine::Vm => {
            // One extra definition per test, whose body applies it to `()`. They
            // are compiled with everything else, so the image is built once and
            // each test is simply a different place to start.
            let base = program.defs.len();
            let mut defs = program.defs.clone();
            for (i, var) in tests.iter().enumerate() {
                defs.push(core::Def {
                    // Invented after compilation, so it belongs to no unit —
                    // indexed rather than counted, so the same tests over the
                    // same program always produce the same image.
                    var: meadow_compiler::hir::VarId::synthetic(i as u32),
                    name: "<test>".into(),
                    // Whatever the test returns; nothing reads it, and this
                    // definition is built after type checking is over.
                    poly: core::Poly::mono(core::unknown()),
                    term: core::Term::App(
                        std::sync::Arc::new(core::Term::Var(*var)),
                        std::sync::Arc::new(core::Term::Lit(core::Lit::Unit)),
                    ),
                });
            }
            let whole = core::Program {
                defs,
                entry: program.entry,
                ctor_fields: program.ctor_fields.clone(),
            };
            let image = compile(&whole, opt)?;

            Ok((0..tests.len())
                .map(|i| {
                    // `lower_program` labels definitions by position and
                    // `entries` is indexed by label, so the nth extra definition
                    // is entry `base + n`.
                    let Some(&entry) = image.entries.get(base + i) else {
                        return Err("a test has no entry point".to_string());
                    };
                    let mut vm = meadow_rts::Vm::new(&image);
                    match vm.run(entry, UNBOUNDED) {
                        Ok(v) => Ok(vm.show(v)),
                        Err(e) => Err(e.msg),
                    }
                })
                .collect())
        }
    }
}

/// `core` → AxCut → bytecode.
pub fn compile(program: &core::Program, opt: OptLevel) -> Result<meadow_bytecode::Program, String> {
    let lowered = meadow_seq::lower_program(program, opt);
    if !lowered.unsupported.is_empty() {
        return Err(format!(
            "the back end cannot translate {:?} yet; try --cek",
            lowered.unsupported
        ));
    }
    meadow_codegen::compile(&lowered.program).map_err(|e| e.msg)
}
