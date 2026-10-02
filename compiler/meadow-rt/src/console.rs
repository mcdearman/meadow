//! Whether colour written to the terminal shows as colour.
//!
//! Colour is ANSI escapes in what a program writes, which a terminal on Unix
//! draws as a matter of course. A Windows console draws them only once asked
//! to -- "virtual terminal processing", a flag of the console's output mode,
//! which the console host, Windows Terminal and the terminals built on ConPTY
//! (VS Code's) all support since Windows 10 -- and otherwise prints them as
//! they are. So [`ansi`] asks, for standard output and standard error, once;
//! and what writes colour -- `meadow`'s own status lines, and a program asking
//! `Process.isTerminal` -- writes it only if the answer was yes.

use std::sync::OnceLock;

/// Whether ANSI escapes written to standard output and standard error are
/// drawn rather than printed: always off Windows; on Windows, whether the
/// console took the mode that draws them -- or has no console to take it, a
/// file or a pipe, where escapes are the reader's business.
pub fn ansi() -> bool {
    static ANSI: OnceLock<bool> = OnceLock::new();
    *ANSI.get_or_init(enable)
}

#[cfg(not(windows))]
fn enable() -> bool {
    true
}

#[cfg(windows)]
fn enable() -> bool {
    use std::ffi::c_void;

    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
    }

    [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .all(|which| {
            // Safety: the standard handles, asked about and set as the console
            // API documents; a handle that is not a console fails `GetConsoleMode`
            // and is left alone.
            unsafe {
                let handle = GetStdHandle(which);
                let mut mode = 0;
                if handle.is_null() || GetConsoleMode(handle, &mut mode) == 0 {
                    return true;
                }
                mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                    || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
            }
        })
}
