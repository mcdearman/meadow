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

use meadow_compiler::{OptLevel, core};
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
    /// The VM, compiling each block to machine code once it has run often
    /// enough -- see `meadow_rts::jit`.
    Jit,
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Engine::Vm => "vm",
            Engine::Cek => "cek",
            Engine::Jit => "jit",
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
        Engine::Vm | Engine::Jit => {
            let image = compile(program, opt)?;
            let native = native(&image, engine, opt)?;
            let entry = image.entry.ok_or("program has no entry point")?;
            meadow_rts::sched::run_native(
                &image,
                native.as_ref(),
                entry,
                UNBOUNDED,
                meadow_rts::sched::workers(),
            )
            .result
            .map_err(|e| e.msg)
        }
    }
}

/// How long [`run_timed`] spent getting a program ready, and running it.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// Lowering and code generation: `core` to bytecode. `None` on the CEK
    /// machine, which runs `core` as it is.
    pub compile: Option<std::time::Duration>,
    /// The program itself, from its first instruction to its answer -- JIT
    /// compilation included, since that happens as it runs.
    pub run: std::time::Duration,
}

/// [`run`], saying how long each part took.
pub fn run_timed(
    program: &core::Program,
    engine: Engine,
    opt: OptLevel,
) -> (Result<String, String>, Timings) {
    use std::time::Instant;
    match engine {
        Engine::Cek => {
            let start = Instant::now();
            let result = run(program, engine, opt);
            let run = start.elapsed();
            (result, Timings { compile: None, run })
        }
        Engine::Vm | Engine::Jit => {
            let start = Instant::now();
            let image = compile(program, opt);
            let compile = Some(start.elapsed());
            let start = Instant::now();
            let result = image.and_then(|image| {
                let native = native(&image, engine, opt)?;
                let entry = image.entry.ok_or("program has no entry point")?;
                meadow_rts::sched::run_native(
                    &image,
                    native.as_ref(),
                    entry,
                    UNBOUNDED,
                    meadow_rts::sched::workers(),
                )
                .result
                .map_err(|e| e.msg)
            });
            let run = start.elapsed();
            (result, Timings { compile, run })
        }
    }
}

/// A duration the way `Std.Time.formatNanos` writes one: three significant
/// figures in the unit that reads best -- `742ns`, `1.23µs`, `45.7ms`, `3.21s`
/// -- then `2m 05.3s`, `1h 02m 03s` and `3d 04h 05m`.
pub fn format_duration(d: std::time::Duration) -> String {
    let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    let three = |unit: u64| {
        let hundredths = (ns * 100 + unit / 2) / unit;
        if hundredths < 1000 {
            return format!("{}.{:02}", hundredths / 100, hundredths % 100);
        }
        let tenths = (ns * 10 + unit / 2) / unit;
        if tenths < 1000 {
            return format!("{}.{}", tenths / 10, tenths % 10);
        }
        ((ns + unit / 2) / unit).to_string()
    };
    match ns {
        0..1_000 => format!("{ns}ns"),
        1_000..999_500 => format!("{}µs", three(1_000)),
        999_500..999_500_000 => format!("{}ms", three(1_000_000)),
        999_500_000..59_950_000_000 => format!("{}s", three(1_000_000_000)),
        59_950_000_000..3_599_950_000_000 => {
            let tenths = (ns + 50_000_000) / 100_000_000;
            format!(
                "{}m {:02}.{}s",
                tenths / 600,
                tenths % 600 / 10,
                tenths % 10
            )
        }
        3_599_950_000_000..86_399_500_000_000 => {
            let secs = (ns + 500_000_000) / 1_000_000_000;
            format!(
                "{}h {:02}m {:02}s",
                secs / 3600,
                secs % 3600 / 60,
                secs % 60
            )
        }
        _ => {
            let mins = ns / 60_000_000_000 + u64::from(ns % 60_000_000_000 >= 30_000_000_000);
            format!(
                "{}d {:02}h {:02}m",
                mins / 1440,
                mins % 1440 / 60,
                mins % 60
            )
        }
    }
}

/// With [`Engine::Jit`], a JIT for `image`, compiling what gets hot -- after
/// `MEADOW_JIT_THRESHOLD` entries, or the default.
pub fn native(
    image: &meadow_bytecode::Program,
    engine: Engine,
    opt: OptLevel,
) -> Result<Option<meadow_rts::jit::Native<'_>>, String> {
    native_at(
        image,
        engine,
        meadow_rts::jit::Native::threshold_from_env(),
        opt,
    )
}

/// [`native`], compiling a block the `threshold`th time it is entered.
fn native_at(
    image: &meadow_bytecode::Program,
    engine: Engine,
    threshold: u32,
    opt: OptLevel,
) -> Result<Option<meadow_rts::jit::Native<'_>>, String> {
    match engine {
        Engine::Jit => meadow_rts::jit::Native::jit(image, threshold, opt).map(Some),
        _ => Ok(None),
    }
}

/// What the bytecode VM's collector did during a run.
#[derive(Debug, Clone)]
pub struct GcStats {
    /// Green threads that finished, `main` included. Each had a heap of its
    /// own, and the rest of these figures are summed over them.
    pub threads: u64,
    pub collections: u64,
    /// Heap slots allocated and copied by collections, in total.
    pub allocated: u64,
    pub copied: u64,
    pub gc_nanos: u64,
    pub run_nanos: u64,
    /// The main thread's heap -- nursery and old generation -- and slots held in
    /// compact regions, at the end.
    pub heap_slots: usize,
    pub region_slots: usize,
    /// Every pause, across every thread.
    pub pauses: meadow_rts::pauses::Pauses,
    /// Slots promoted to old generations, marking cycles, and the time marking
    /// took on any thread -- mostly not the program's.
    pub promoted: u64,
    pub cycles: u64,
    pub mark_nanos: u64,
    /// Slots and blocks moved out of sparse old blocks.
    pub evacuated: u64,
    pub evacuated_blocks: u64,
    /// The main thread's old generation at the end, and what its last marking
    /// cycle found alive in it.
    pub old_slots: usize,
    pub old_live: usize,
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
            "gc: {} collections across {} thread{}, {:.1} ms paused ({share:.1}% of {:.1} ms)\n\
             gc: {} allocated, {} copied in nurseries, {} promoted\n\
             gc: {} marking cycle{}, {:.1} ms marking, {} evacuated from {} block{}\n\
             gc: old generation {}, {} alive when last marked\n\
             gc: heap {}, compact regions {}\n\
             gc: pauses {}",
            self.collections,
            self.threads,
            if self.threads == 1 { "" } else { "s" },
            self.gc_nanos as f64 / 1e6,
            self.run_nanos as f64 / 1e6,
            bytes(self.allocated),
            bytes(self.copied),
            bytes(self.promoted),
            self.cycles,
            if self.cycles == 1 { "" } else { "s" },
            self.mark_nanos as f64 / 1e6,
            bytes(self.evacuated),
            self.evacuated_blocks,
            if self.evacuated_blocks == 1 { "" } else { "s" },
            bytes(self.old_slots as u64),
            bytes(self.old_live as u64),
            bytes(self.heap_slots as u64),
            bytes(self.region_slots as u64),
            pauses(&self.pauses),
        )
    }
}

/// The pause percentiles worth knowing when latency matters: the typical one,
/// the tail, and the worst.
fn pauses(p: &meadow_rts::pauses::Pauses) -> String {
    if p.count == 0 {
        return "none".to_string();
    }
    format!(
        "p50 {}, p99 {}, p99.9 {}, max {}",
        human_nanos(p.percentile(0.5)),
        human_nanos(p.percentile(0.99)),
        human_nanos(p.percentile(0.999)),
        human_nanos(p.max_nanos),
    )
}

fn human_nanos(n: u64) -> String {
    match n {
        0..=999 => format!("{n} ns"),
        1_000..=999_999 => format!("{:.1} us", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.2} ms", n as f64 / 1e6),
        _ => format!("{:.2} s", n as f64 / 1e9),
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
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{x:.1} {}", UNITS[unit])
    }
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
        Engine::Vm | Engine::Jit => match compile(program, opt) {
            Ok(image) => match native(&image, engine, opt) {
                Ok(jit) => run_image_with_stats(&image, jit.as_ref()),
                Err(e) => (Err(e), None),
            },
            Err(e) => (Err(e), None),
        },
    }
}

/// Run a compiled image on the VM, and say what its collector did.
pub fn run_image_with_stats(
    image: &meadow_bytecode::Program,
    native: Option<&meadow_rts::jit::Native>,
) -> (Result<String, String>, Option<GcStats>) {
    let Some(entry) = image.entry else {
        return (Err("program has no entry point".to_string()), None);
    };
    let started = std::time::Instant::now();
    let outcome = meadow_rts::sched::run_native(
        image,
        native,
        entry,
        UNBOUNDED,
        meadow_rts::sched::workers(),
    );
    let s = outcome.stats;
    let stats = GcStats {
        threads: s.threads,
        collections: s.collections,
        allocated: s.allocated,
        copied: s.copied,
        gc_nanos: s.gc_nanos,
        run_nanos: started.elapsed().as_nanos() as u64,
        heap_slots: s.heap_slots,
        region_slots: s.region_slots,
        pauses: s.pauses,
        promoted: s.promoted,
        cycles: s.cycles,
        mark_nanos: s.mark_nanos,
        evacuated: s.evacuated,
        evacuated_blocks: s.evacuated_blocks,
        old_slots: s.old_slots,
        old_live: s.old_live,
    };
    (outcome.result.map_err(|e| e.msg), Some(stats))
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
    let threshold = meadow_rts::jit::Native::threshold_from_env();
    run_tests_jit_at(program, tests, engine, opt, threshold)
}

/// [`run_tests`], with the JIT compiling a block the `threshold`th time it is
/// entered -- 1 to run all the code that runs natively.
pub fn run_tests_jit_at(
    program: &core::Program,
    tests: &[core::Var],
    engine: Engine,
    opt: OptLevel,
    threshold: u32,
) -> Result<Vec<Result<String, String>>, String> {
    match engine {
        Engine::Cek => Ok(meadow_eval::run_tests(program, tests)
            .map_err(|e| e.msg)?
            .into_iter()
            .map(|r| r.map(|v| v.to_string()).map_err(|e| e.msg))
            .collect()),

        Engine::Vm | Engine::Jit => {
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
                    // Whatever the test returns: what calling it has.
                    poly: program.result_of_calling(*var),
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
                variants: program.variants.clone(),
                origins: Default::default(),
            };
            let image = compile(&whole, opt)?;
            let jit = native_at(&image, engine, threshold, opt)?;

            Ok((0..tests.len())
                .map(|i| {
                    // `lower_program` labels definitions by position and
                    // `entries` is indexed by label, so the nth extra definition
                    // is entry `base + n`.
                    let Some(&entry) = image.entries.get(base + i) else {
                        return Err("a test has no entry point".to_string());
                    };
                    meadow_rts::sched::run_native(
                        &image,
                        jit.as_ref(),
                        entry,
                        UNBOUNDED,
                        meadow_rts::sched::workers(),
                    )
                    .result
                    .map_err(|e| e.msg)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The same cases as `Std.Time`'s own tests of `formatNanos`, so the REPL's
    /// timings and a program's read alike.
    #[test]
    fn durations_format_as_std_time_does() {
        let cases: &[(u64, &str)] = &[
            (0, "0ns"),
            (742, "742ns"),
            (999, "999ns"),
            (1234, "1.23µs"),
            (9995, "10.0µs"),
            (999_499, "999µs"),
            (999_500, "1.00ms"),
            (45_678_000, "45.7ms"),
            (321_000_000, "321ms"),
            (3_210_000_000, "3.21s"),
            (59_950_000_000, "1m 00.0s"),
            (125_300_000_000, "2m 05.3s"),
            (3_723_000_000_000, "1h 02m 03s"),
            (273_900_000_000_000, "3d 04h 05m"),
        ];
        for &(ns, want) in cases {
            assert_eq!(format_duration(Duration::from_nanos(ns)), want, "{ns}ns");
        }
    }
}
