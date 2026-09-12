//! The Meadow **build system**: filesystem package discovery ([`package`]), the
//! per-unit compile [`pipeline`], the [`linker`], and the embedded standard
//! library ([`stdlib`]).
//!
//! It sits on top of the `meadow-compiler` workspace (front-end passes and the
//! AxCut back end) and the two machines that can run the result: `meadow-rts`,
//! the bytecode VM, and `meadow-eval`, the CEK machine. [`runtime`] picks
//! between them. The `meadow` binary — CLI + REPL — is a thin shell over this
//! crate.

pub mod linker;
pub mod package;
pub mod editor;
pub mod complete;
pub mod format;
pub mod init;
pub mod runtime;
pub mod test;
pub mod pipeline;
pub mod profile;
pub mod update;
pub mod stdlib;

pub use meadow_compiler::{OptLevel, Options, Strictness};
pub use profile::{Profile, Resolved};
pub use runtime::Engine;
