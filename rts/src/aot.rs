//! Running a program compiled ahead of time.
//!
//! A native executable is its machine code and its image, in an object file
//! written by [`crate::codegen::object`], linked with this runtime and a `main`
//! that calls [`meadow_aot_main`]. The image is the program's tables and its
//! bytecode, which the instructions native code hands back to the
//! interpreter still run from; the machine code is every block that has some.

use crate::abi::NativeFn;
use std::ffi::c_char;

/// Defined only by a runtime library built from the sources the code generator
/// was: see `build.rs`, and [`crate::codegen::object::runtime_symbol`].
#[unsafe(export_name = concat!("meadow_rts_", env!("MEADOW_RTS_FINGERPRINT")))]
pub static FINGERPRINT: u8 = 0;

/// Run the program in `data`, with the native code starting at `code`, and
/// say how it went as a process exit status: its answer printed, unless it
/// is `()`, and 0; or its failure, and 1.
///
/// # Safety
///
/// `code` and `data` must be the two symbols of one object file from
/// [`crate::codegen::object::write`], for the architecture this is running
/// on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_aot_main(
    code: *const u8,
    data: *const u8,
    _argc: i32,
    _argv: *const *const c_char,
) -> i32 {
    // Safety: the layout `object::data` writes, which the caller vouches for.
    let (image, blocks) = unsafe {
        let word = |at: usize| (data.add(at) as *const u64).read_unaligned() as usize;
        let (len, count, table) = (word(0), word(8), word(16));
        let image = std::slice::from_raw_parts(data.add(24), len);
        let blocks: Vec<(u32, u32)> = (0..count)
            .map(|i| {
                let at = data.add(table + 8 * i);
                (
                    (at as *const u32).read_unaligned(),
                    (at.add(4) as *const u32).read_unaligned(),
                )
            })
            .collect();
        (image, blocks)
    };
    let program = match meadow_bytecode::image::decode(image) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: the program's image is damaged: {e}");
            return 1;
        }
    };
    let native = crate::jit::Native::ahead_of_time(
        &program,
        blocks.into_iter().map(|(pc, offset)| {
            // Safety: an offset the compiler recorded for the function it
            // emitted there, in code the linker made executable.
            let f: NativeFn = unsafe { std::mem::transmute(code.add(offset as usize)) };
            (pc, f)
        }),
    );
    let Some(entry) = program.entry else {
        eprintln!("error: the program has no entry point");
        return 1;
    };
    let outcome = crate::sched::run_native(
        &program,
        Some(&native),
        entry,
        u64::MAX,
        crate::sched::workers(),
    );
    if crate::abi::traps::on() {
        let all = crate::abi::traps::report();
        let total: u64 = all.iter().map(|(_, c)| c).sum();
        eprintln!("traps: {total} instructions handed to the interpreter, by pc:");
        for ((pc, op), c) in all.iter().take(25) {
            eprintln!(
                "  {c:>12}  {:5.1}%  pc {pc:<6} {op:?}",
                100.0 * *c as f64 / total as f64
            );
        }
    }
    match outcome.result {
        Ok(answer) => {
            if answer != "()" {
                println!("{answer}");
            }
            0
        }
        Err(e) => {
            eprintln!("{}", e.msg);
            1
        }
    }
}
