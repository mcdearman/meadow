# Changelog

What changed in each release of Meadow: the language, the standard library,
`Meadow.toml`, and the tools. Versions follow [semantic versioning](https://semver.org).
While the major version is 0, a minor version may break source and a patch may
not. The promise covers the language, `Std`'s public API and `Meadow.toml`; the
bytecode image, the Cut IR and the runtimes' layouts may change in any release
until 1.0.

Each release has three lists: what **breaks** code that built before, what is
**added**, and what is **fixed**. A nightly build is `master`, and what it has
beyond the last release is under _Unreleased_.

## Unreleased

### Added

- The REPL colours what is typed with the language server's token classes:
  types, constructors and modules each have a colour, by what the session
  has in scope. The `name : type` line an entry prints is coloured too.

### Fixed

- Two builds that fetch the same git dependency at once take turns in its
  cache. One could find the other's unfinished clone and report that the
  repository "has no releases".

## 0.2.0 - 2026-10-04

The first release with a version that does not move. Before it there was one
release, `v0.1.0-alpha`, republished from `master`; from here on a version is
published once, and `master` is the nightly channel.

### Breaking

- `Std.Ffi` is unstable. A package that uses it says so in its `Meadow.toml`,
  `features = ["ffi"]`, and a stable `meadow` refuses it; a nightly accepts it.
- `Std.Ffi.Arg`'s constructors are `Arg.Int`, `Arg.Float`, `Arg.String` and
  `Arg.Ptr`. They were `IntArg`, `FloatArg`, `StringArg` and `PtrArg`.
- `Std.String.Parse.Lexer.IndentOpt`'s constructors are `IndentOpt.None`,
  `IndentOpt.Many` and `IndentOpt.Some`. They were `IndentNone`, `IndentMany`
  and `IndentSome`.
- `meadow fmt` cuts lines longer than 80 columns by default. `--width N` sets
  another width and `--width 0` leaves long lines alone.
- A module under the one being written is a qualifier by its name: after
  `mod Core`, `Core.name` means that module's `name`. A `use … as Core`
  written in the same module still wins.

### Added

- Channels. `meadowup default stable` or `nightly` says which releases
  `meadowup update` follows; `meadowup install 0.2.0` installs a version.
  `meadow --version` says which a build is.
- `features = […]` under `[package]` in `Meadow.toml`: the unstable features a
  package uses.
- A `meadow-toolchain` file beside a `Meadow.toml`, holding `stable`, `nightly`
  or a version: `meadow` refuses to build the project with another toolchain
  and says how to get the one it asks for.
- Modules written inside a file, `mod Name { … }`, which nest. A module under
  the current one is reached by its path, `Core.Expr.name`, and so are its
  types' constructors and its pattern synonyms. `use super (…)` names the
  module a module is written in.
- `Std.Ffi`: opening a C library, calling its functions, and reading and
  writing C's memory.
- `Std.Terminal`: raw mode, reading keys, and the terminal's size.
- `Std.String.Parse.keep` and `skip`, for parsers written as pipelines.
- `meadow build --emit expanded` writes each module with its macros expanded.
- `--nursery SIZE`, and `nursery = "8M"` in a profile: how large Glade's
  nursery may grow.
- Syntax highlighting in the REPL.
- `Std.Json` reads through a byte scanner, about twenty times faster.

### Fixed

- Silo on aarch64 passes nothing on the stack, which works around an LLVM bug
  that overwrote a caller's value and crashed programs built with `-O 0`.
- A build is the same from one run to the next, and Silo reuses the object
  files of modules that did not change.
- A `match` on a run of constructors lowers to one switch at every
  optimisation level.
- `@pub` and `@cfg(…)` written in a macro rule's template mean what they say.
  A declaration a rule marked `@pub` was private.
- A pattern synonym reached through a qualifier, `E.Lit n` after
  `use … as E`, is resolved through it.
