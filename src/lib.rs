//! # Meadow
//!
//! A small, embeddable ML. This crate is both the `meadow` binary and a library
//! (so integration tests under `tests/` can drive individual passes).
//!
//! ## The pipeline
//!
//! Source text flows through a fixed sequence of passes. Each pass has one module:
//!
//! ```text
//!   &str
//!    │  lexer::tokenize            logos-based; skips whitespace/comments
//!    ▼
//!   Vec<LToken>
//!    │  parser::parse              chumsky; pratt-parsed operators
//!    ▼
//!   ast::Module                   surface syntax, names still strings
//!    │  rename::Resolver          ast → hir
//!    ▼
//!   hir::Module                   every node stamped with a hir::NodeId;
//!    │                            identifiers resolved to hir::VarId;
//!    │                            data/record decls validated
//!    │  infer::Infer              Algorithm J (see infer.rs)
//!    ▼
//!   infer::TypeTable              NodeId → Type, plus a Scheme per top-level binding
//!    │  core::Lowerer             hir → core (an extended lambda calculus)
//!    ▼
//!   Vec<core::Def>
//!    │  linker::Linker            concatenate packages, find `main`
//!    ▼
//!   core::Program
//!    │  eval::run                 call-by-value tree-walking interpreter
//!    ▼
//!   eval::Value
//! ```
//!
//! ## Compilation units
//!
//! The compiler only ever compiles a **package** (a directory of modules with a
//! `meadow.pkg` manifest) against a set of already-compiled dependency packages.
//! Packages form a DAG — [`package::PackageGraph::build`] topologically sorts them
//! and rejects cycles. Modules *within* a package may be mutually recursive.
//!
//! [`pipeline::compile_unit`] is the shared per-unit driver (resolve → infer →
//! lower). [`pipeline::build`] wraps it with filesystem discovery and linking.
//!
//! ## The REPL
//!
//! The compiler has no notion of interactivity. [`repl::Session`] fakes it: each
//! entry is compiled as a throwaway one-module package whose dependencies are all
//! the previous entries. The "repl prefix" is literally handed back to the
//! compiler as ordinary dependency packages, so a `def` / `data` on one line is in
//! scope on the next.
//!
//! ## Identifiers
//!
//! [`hir::VarId`]s are globally unique (a process-wide atomic counter), so ids
//! minted by separately compiled packages / REPL lines never collide and the
//! linker can key on them. [`hir::NodeId`]s are dense per resolver, so type
//! inference can use a `Vec`-backed side table.

#![allow(dead_code)] // the IR / driver surface is intentionally ahead of its first consumer

pub mod ast;
pub mod core;
pub mod diagnostics;
pub mod eval;
pub mod hir;
pub mod infer;
pub mod intern;
pub mod lexer;
pub mod linker;
pub mod package;
pub mod parser;
pub mod pipeline;
pub mod rename;
pub mod repl;
pub mod source;
pub mod span;
