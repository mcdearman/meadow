//! The Meadow **build system**: filesystem package discovery ([`package`]), the
//! per-unit compile [`pipeline`], the [`linker`], and the embedded standard
//! library ([`stdlib`]).
//!
//! It sits on top of the `meadow-compiler` workspace (front-end passes) and
//! `meadow-eval` (the evaluator). The `meadow` binary — CLI + REPL — is a thin
//! shell over this crate.

pub mod linker;
pub mod package;
pub mod pipeline;
pub mod stdlib;
