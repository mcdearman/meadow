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

### Breaking

- `meadow fmt` cuts lines longer than 100 columns by default, as rustfmt does.
  It was 80. `--width 80` asks for the old width.

### Added

- `meadow fmt` sets a record out as rustfmt sets out a struct: its brace on the
  line of what it belongs to, a field to a line, and the closing brace back
  under the start of that line.
- `meadow fmt` puts what is written over several lines back on one when it fits
  the width, as rustfmt does: a definition and its body, a call and its
  arguments, an arm and what it answers, a record and its fields, an `if` and
  its branches. A `match`'s arms, a `data`'s variants, the items of a `trait`
  or an `impl`, and the steps of a `let … in` sequence keep a line each.
- `meadow fmt` puts what a `let` binds back on the `let`'s line when it fits
  the width there, and the `in` after it when that fits too.
- Formatting on save in an editor does what `meadow fmt` does: it cuts and
  joins lines as well as indenting. It only indented before.
- `meadow fmt` puts a space between a name and a `{` written against it,
  `Ctor{` to `Ctor {`, and indents what follows a bracket that ends its line
  by one level.
- The REPL's banner and the tutorial say which keys force a new line on macOS:
  `Ctrl+J` always, and Option+Return once the terminal sends Option as Meta.
- The REPL colours what is typed as the language server would: the entry is
  compiled against the session as it is typed, so types, constructors,
  modules, functions and variables each have a colour, by what each name
  resolved to and its type. The `name : type` line an entry prints is
  coloured too.
- The language server's semantic tokens mark a name whose type is a function
  as `function`; every lower-case name was `variable`.

### Fixed

- `meadow fmt` puts the line after a `let … in` level with the `let`. A second
  `let` written further in stayed there, and its `in` with it, so the body
  under them was out of line with both.
- `meadow fmt` places the `in` of a `let` written in brackets. It sent that
  `in` to the left margin and lost its place in the bracket for the lines
  after. A bracketed `let` is now laid out with its value four in, its `in`
  two in, and its body on the `in`'s line when they fit; and a line that
  carries another on moves with it when that line is moved.
- The finder reads `[a;]` in a type as a list. It dropped the `;` and looked
  for a vector, so `(a -> b) -> [a;] -> [b;]` did not find a `map` over lists.
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
