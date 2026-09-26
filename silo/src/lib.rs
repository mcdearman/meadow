//! **The runtime of a program compiled by `meadow-llvm`.**
//!
//! `meadow build --runtime silo` compiles a program all the way to machine code
//! with LLVM, straight from AxCut: no bytecode, no interpreter, no collector.
//! Memory is counted by reference, as in the AxCut paper -- Schuster, Müller,
//! Ostermann and Brachthäuser, _Compiling Classical Sequent Calculus to Stock
//! Hardware: The Duality of Compilation_, OOPSLA 2025,
//! <https://doi.org/10.1145/3720507> -- and the program runs on the native
//! stack. `docs/SILO.md` is the design.
//!
//! This library is what such a program links against: the heap ([`heap`]),
//! the primitives the emitted code does not do inline ([`prims`]), printing
//! ([`show`]), and the entry point, [`meadow_run`]. The emitted module refers
//! to [`FINGERPRINT`], so it links only with a runtime built from the sources
//! its compiler was.
//!
//! Every `extern "C"` function here is called by emitted code, and takes and
//! answers words: references are counted as `docs/SILO.md` says, a primitive
//! borrows its arguments, and what one answers is owned by the caller.

pub mod ctx;
pub mod cycles;
pub mod heap;
pub mod native;
pub mod parcel;
pub mod prims;
pub mod region;
pub mod sched;
pub mod segments;
pub mod shadow;
pub mod show;
pub mod value;

use ctx::Ctx;
use heap::Word;
use std::ffi::c_char;

/// Defined only by a library of this runtime built from the sources the
/// compiler was: see `build.rs`, and `meadow_llvm::runtime_symbol`.
#[unsafe(export_name = concat!("meadow_silo_", env!("MEADOW_SILO_FINGERPRINT")))]
pub static FINGERPRINT: u8 = 0;

unsafe extern "C" {
    /// The descriptor of what the program's entry point answers.
    static meadow_result_desc: i64;
}

/// A block of `words` words for a `let` or a `new`, count zero.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_acquire(words: i64) -> Word {
    heap::acquire(words as usize)
}

/// The last reference to the block at `v` was erased. Its fields are erased
/// later, a few at a time: see [`heap`].
#[unsafe(no_mangle)]
pub extern "C" fn meadow_free(v: Word) {
    heap::erase(v, meadow_core::desc::REF);
}

/// The block at `v` had its fields moved out by its last reference: it is
/// reusable.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_clean(v: Word) {
    heap::clean(v);
}

/// Fail with the message at `msg`: an `error`, a failed assertion, a division
/// by zero.
///
/// # Safety
///
/// `msg` must point at `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_fail(msg: *const u8, len: i64) -> ! {
    // Safety: the caller's.
    let text = unsafe { std::slice::from_raw_parts(msg, len as usize) };
    fail(&String::from_utf8_lossy(text))
}

/// Stop the program with `msg` on stderr and a failing status -- or, in a
/// thread other than `main`, just that thread: see [`sched::fail_thread`].
pub fn fail(msg: &str) -> ! {
    sched::fail_thread(msg);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    eprintln!("{msg}");
    std::process::exit(1)
}

/// Field `i` of the data, record or array at `v`, shared.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_field(v: Word, i: i64) -> Word {
    let i = i as usize;
    if !heap::is_block(v) || i >= heap::len(v) {
        fail(&format!("field {i} of a value that has none"));
    }
    let x = heap::field(v, i);
    heap::share(x, heap::field_desc(v, i));
    x
}

/// The string literal at `text`: made once, kept for the rest of the run, and
/// shared with each use.
///
/// # Safety
///
/// `text` must point at `len` bytes that live as long as the program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_text(text: *const u8, len: i64) -> Word {
    // Safety: the running thread's context -- each thread makes its own, in
    // its own heap.
    let lits = unsafe { &mut (*crate::ctx::get()).literals };
    let v = match lits.get(&(text as usize)) {
        Some(v) => *v,
        None => {
            // Safety: the caller's.
            let bytes = unsafe { std::slice::from_raw_parts(text, len as usize) };
            let s = heap::string(bytes);
            prims::keep(s, meadow_core::desc::REF);
            // Safety: as above; `keep` and `string` do not touch the literals.
            unsafe { (*crate::ctx::get()).literals.insert(text as usize, s) };
            s
        }
    };
    heap::share(v, meadow_core::desc::REF);
    v
}

/// The running thread's spill area: where the emitted code puts the
/// arguments of a call that do not fit in registers. See
/// `meadow_llvm::emit::spill`.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_spill_area() -> *mut Word {
    // Safety: the running thread's context.
    unsafe { (*crate::ctx::get()).spill.as_mut_ptr() }
}

/// A `BigInt` literal.
#[unsafe(no_mangle)]
pub extern "C" fn meadow_bigint(n: i64) -> Word {
    value::new_bigint(&num_bigint::BigInt::from(n))
}

/// Run the program's entry point and answer what it returned, with the
/// context its heap is in -- current on this OS thread, for reading the
/// answer.
///
/// A program that never spawns needs none of the scheduler: it runs here, on
/// this thread's stack, with one context and no thread-locals at all. See
/// `ctx::threaded`.
fn run_main(entry: extern "C" fn() -> Word) -> (Box<Ctx>, Word) {
    if ctx::threaded() {
        let (mut c, v) = sched::main(move || entry());
        ctx::set(&mut *c);
        return (c, v);
    }
    let mut c = Ctx::new(0);
    ctx::set(&mut *c);
    let v = entry();
    (c, v)
}

/// Run one test of a test executable: the entry of `tests` numbered by the
/// first argument. A test passes by returning -- status 0 -- and fails by
/// failing, with its message on stderr, as `Test.fail` makes it.
///
/// # Safety
///
/// `tests` must be the emitted module's table of `n` entries, and `argv` the
/// process's arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_run_test(
    tests: *const extern "C" fn() -> Word,
    n: i64,
    _argc: i32,
    _argv: *const *const c_char,
) -> i32 {
    let which = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<usize>().ok())
        .filter(|i| *i < n as usize);
    let Some(i) = which else {
        eprintln!("usage: a test's number, below {n}");
        return 2;
    };
    // Safety: the caller's.
    let entry = unsafe { *tests.add(i) };
    let ran = std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(move || {
            let (c, _) = run_main(entry);
            drop(c);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        })
        .expect("the test's thread starts")
        .join();
    i32::from(ran.is_err())
}

/// Run the program: its entry point, on a stack deep enough for the
/// recursion Meadow programs do, then its answer printed -- unless it is `()`
/// -- and a status for the process.
///
/// # Safety
///
/// `entry` must be the emitted module's `meadow_entry`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meadow_run(
    entry: extern "C" fn() -> Word,
    _argc: i32,
    _argv: *const *const c_char,
) -> i32 {
    // Safety: the emitted module defines it.
    let d = unsafe { meadow_result_desc };
    let run = move || {
        // What `main` answers is in its heap, which stays current here.
        let (_main_ctx, v) = run_main(entry);
        let text = show::show(v, d);
        prims::report();
        cycles::report();
        heap::erase(v, d);
        if std::env::var_os("MEADOW_SILO_LEAKS").is_some() {
            // Every cycle collected and everything pending erased, so that
            // what is counted as left behind really is left behind.
            heap::settle();
            let (left, kinds) = heap::leaked(&prims::roots());
            eprintln!("aot: {left} blocks live at exit");
            eprintln!("aot: {} blocks acquired", heap::acquired());
            eprintln!("aot: {} segments live at exit", segments::live());
            if left > 0 {
                for (n, k, m) in kinds.iter().take(12) {
                    let what = match *k {
                        heap::DATA => show::ctor_name(*m as usize),
                        heap::CLOSURE => format!("closure, methods from {m}"),
                        k => format!("kind {k}"),
                    };
                    eprintln!("aot: {n:>8}  {what}");
                }
            }
        }
        text
    };
    // A native stack for non-tail recursion over a long list: reserved, and
    // committed only as it is touched.
    let answer = std::thread::Builder::new()
        .name("main".into())
        .stack_size(1 << 30)
        .spawn(run)
        .expect("the main thread starts")
        .join();
    match answer {
        Ok(text) => {
            if text != "()" {
                println!("{text}");
            }
            0
        }
        Err(_) => 1,
    }
}
