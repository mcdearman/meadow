//! **A program's command-line arguments**, as `Process.argv` answers them.
//!
//! Both machines answer `argv`, so what it means lives here rather than in
//! either. A native executable is the program, so its arguments are the
//! process's, less its own name. A program run by `meadow run` is not the
//! process -- `meadow` is -- and the driver says what the program was given
//! (what followed `--`) before running it.

use std::sync::OnceLock;

static GIVEN: OnceLock<Vec<String>> = OnceLock::new();

/// The arguments the program running in this process was given, when the
/// process is not the program itself. Only the first call counts.
pub fn set(args: Vec<String>) {
    let _ = GIVEN.set(args);
}

/// The program's arguments, not including its name.
pub fn get() -> Vec<String> {
    match GIVEN.get() {
        Some(args) => args.clone(),
        None => std::env::args().skip(1).collect(),
    }
}
