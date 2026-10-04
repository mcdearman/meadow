# Meadow roadmap

What Meadow is for, what is done, what is under way, and what is planned.
Update it in the commit that changes a task's state.

Each task is marked **Done**, **In progress** (with what is done so far), or
**Planned**. Commit hashes are given where a task landed recently enough to be
worth finding.

## Goals

1. **A small ML-family language that is pleasant to write real programs in.**
   Hindley–Milner inference, row-polymorphic records, traits, algebraic effects
   with handlers, macros, and scoping and visibility that follow Rust's.
2. **Two runtimes behind one compiler.** Glade, a bytecode VM with a JIT, an
   ahead-of-time compiler and a garbage collector, for fast builds and
   interactive use. Silo, native code through LLVM with reference counting, for
   speed and a small runtime.
3. **A shared back end other languages can target.** The Cut IR and AxCut, with
   a reference interpreter that says what a program means, so that Meadow,
   Idyll and others compile through the same runtimes.
4. **A compiler written in Meadow.** MeadowBoot, built with the Lingua language
   workbench and checked pass by pass against the Rust compiler.
5. **Tooling at the level of a mainstream language.** Build system, package
   manager, formatter, language server, debugger, REPL, test runner and
   profilers, all in one `meadow` command.
6. **A standard library and package ecosystem** that exercise the language:
   collections, concurrency, parsing, terminal and C interop in Std, and
   third-party packages in their own repositories.

## Language

| Task                                                                               | State                                                              |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Traits, macros (rules and procedural), user-defined operators                      | Done                                                               |
| Algebraic effects, `St` effect, arenas                                             | Done                                                               |
| Concurrency, STM and parallelism                                                   | Done                                                               |
| Sized numeric types                                                                | Done                                                               |
| String escapes and interpolated strings                                            | Done                                                               |
| Conditional compilation with `@cfg`                                                | Done                                                               |
| Doc comments                                                                       | Planned                                                            |
| Inline `Module.Type.Constructor` paths in expressions and types                    | Planned. Today a type has to be imported by name first.            |
| A parse error that says a word is reserved, where a keyword is used as a name      | Planned                                                            |
| `\uXXXX` escapes in JSON strings (needs surrogate pairing in `Std.String`)         | Planned                                                            |
| Typed `@extern` declarations for C functions, `@repr(C)` records, `meadow bindgen` | Planned. The dynamic interface they would sit on is done: see Std. |

## Compiler front end

| Task                                                                                                        | State                                               |
| ----------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| Per-package incremental compilation, with Std compiled ahead of time                                        | Done                                                |
| `meadow build --emit expanded`: sources with macros expanded, and a per-macro size table (`ecb7725`)        | Done                                                |
| A `match` on constructors with a default arm compiles to one switch at every optimisation level (`324ca76`) | Done                                                |
| Interned strings ordered by text, so two builds of one program are the same program (`d63c191`)             | Done                                                |
| Split `expand` time into running a macro and parsing its output in `MEADOW_TIME_PASSES`                     | Planned                                             |
| Decision trees at O1 for arms that can fail after their tag matches (guards, nested patterns)               | Planned. They still use the closure chain below O2. |
| Parallel or cached front end across packages                                                                | Planned                                             |
| Lossless red/green trees with ungrammar                                                                     | Planned                                             |
| Query-based incremental compilation                                                                         | Planned                                             |
| Printable IRs with a live view in the REPL and language server                                              | Planned                                             |

## Glade (bytecode VM, JIT, ahead-of-time)

| Task                                                                                                                                                                                                                                                                                                                                                                                                                                | State                                                                                                              |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| JIT, ahead-of-time executables, profile-guided optimisation                                                                                                                                                                                                                                                                                                                                                                         | Done                                                                                                               |
| Tail recursion modulo cons, in-place reuse                                                                                                                                                                                                                                                                                                                                                                                          | Done                                                                                                               |
| Native code for `setField`, `setRef` and field reads on records of more than eight fields (`1e530dc`)                                                                                                                                                                                                                                                                                                                               | Done                                                                                                               |
| `MEADOW_TRAPS` summary by instruction                                                                                                                                                                                                                                                                                                                                                                                               | Done                                                                                                               |
| Nursery size as a profile key and `--nursery` flag (`7b92801`)                                                                                                                                                                                                                                                                                                                                                                      | Done                                                                                                               |
| Native code for what is still handed to the interpreter on compiler workloads: tag tests, constants, string bytes and slices, array pushes, record selects                                                                                                                                                                                                                                                                          | Planned. 37 million instructions on Lingua's MiniML benchmark.                                                     |
| **Native calls for functions that cannot suspend.** Call a function with a machine `call` and keep its frame on the native stack when the compiler can see it never suspends (no effect that might capture a continuation, no thread operation), as Silo does everywhere. Needs an analysis of which calls qualify, a second calling path in both code generators, and a collector that can find references in native stack frames. | Planned, not soon. `fib` is 1.5× Silo because a return costs about 18 instructions to find its continuation frame. |
| Fix the JIT panic at `-O 2` on MeadowBoot ("r1 holds a value nothing describes")                                                                                                                                                                                                                                                                                                                                                    | Planned                                                                                                            |
| A larger default nursery                                                                                                                                                                                                                                                                                                                                                                                                            | Planned, undecided. 8 MB makes `binarytrees` 23% faster and lengthens nursery pauses.                              |

## Silo (native code through LLVM)

| Task                                                                                                                           | State                                                                                    |
| ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------- |
| Reference counting with reuse, cycle collection, green threads, STM                                                            | Done                                                                                     |
| Nothing passed on the stack on aarch64, working around an LLVM `tailcc` bug (`2e22d8a`)                                        | Done                                                                                     |
| One LLVM unit per source module, named for it (`ef5aadb`)                                                                      | Done                                                                                     |
| Objects kept and reused when a unit's text is unchanged; functions, strings and method indexes named, not numbered (`d63c191`) | Done                                                                                     |
| `MEADOW_SIZES`: LLVM IR per module and per definition                                                                          | Done                                                                                     |
| Stable constructor tags, so using a constructor earlier does not renumber the rest                                             | Planned                                                                                  |
| Count references into a compact region per thread, touching the shared count only at zero                                      | Planned. Today several threads reading one pointer-rich `Compact` contend on every read. |
| Keep Std compiled in full and link against it                                                                                  | Planned, low priority. Std is 6% of a MeadowBoot build.                                  |
| Report the LLVM `tailcc` stack-argument bug upstream                                                                           | Planned. A reproduction is written up; LLVM 15 to trunk are affected.                    |
| A worker-count function a program can read                                                                                     | Planned                                                                                  |

## Shared back end (Cut and AxCut)

| Task                                                                                                                                                     | State                                                                                  |
| -------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Back end independent of Meadow's front end; runtimes ask for roles, not names                                                                            | Done                                                                                   |
| Cut IR specified (`docs/CUT.md`, draft v0), with parser, printer and reference interpreter                                                               | Done                                                                                   |
| Idyll's Cut corpus runs on the reference interpreter                                                                                                     | Done                                                                                   |
| Lower Cut to AxCut                                                                                                                                       | In progress. Not started: the plan is pure programs first, then effects, then `@many`. |
| Runtime changes the spec promises: sequences as plain arrays without vector roles, O(1) freeze for unique mutable arrays, the boxed `any` representation | Planned                                                                                |
| Per-type to-string derived by the compiler, replacing the generic `show` primitive                                                                       | Planned                                                                                |
| Meadow's front end targets Cut                                                                                                                           | Planned                                                                                |
| The shared corpus in CI across the interpreter, AxCut machine, Glade and Silo                                                                            | Planned                                                                                |
| Generate the primitive and operation tables in `CUT.md` from the runtime interface                                                                       | Planned, before freezing v0                                                            |

## MeadowBoot (the compiler in Meadow)

| Task                                                                                | State   |
| ----------------------------------------------------------------------------------- | ------- |
| Lex, parse, group, expand, rename and infer, each checked against the Rust compiler | Done    |
| Built with Lingua's current master, with its smaller generated code (`2671e14`)     | Done    |
| Debug builds on Glade, release on Silo (`85a70a6`)                                  | Done    |
| Lowering to core, checked against `meadow-core`                                     | Planned |
| Reduce `MeadowBoot.Prims.prims`, one definition producing 19.5 MB of LLVM IR        | Planned |
| A formatter for Meadow written with Lingua's `format!`                              | Planned |
| A self-hosted REPL with Lingua's `repl!`                                            | Planned |

## Tooling

| Task                                                                                                                                                                                               | State                                                                                                                                          |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Workspaces, package management and lock files                                                                                                                                                      | Done                                                                                                                                           |
| Parallel tests by default                                                                                                                                                                          | Done                                                                                                                                           |
| `--emit` for bytecode and native code as text                                                                                                                                                      | Done                                                                                                                                           |
| Format on save                                                                                                                                                                                     | Done                                                                                                                                           |
| REPL syntax highlighting from the compiler's lexer (`46c3e8f`)                                                                                                                                     | Done                                                                                                                                           |
| `meadow fmt` keeps lines within 80 columns                                                                                                                                                         | In progress. The line-cutting pass is written (`ce9e1fe`) and is the default; the repository's sources are reformatted with it and under test. |
| **Cut a `match` whose scrutinee is the long part.** The arms line up under the `match`, which the indenter finds from the line ending in `with`, so it has to learn that a `match` can span lines. | Planned. About 90 lines here and in Lingua are left long for it.                                                                               |
| Wrap prose comments longer than the width, leaving aligned ones alone                                                                                                                              | Planned, undecided                                                                                                                             |
| Rules for the bodies of `lang!` and `syntax!` calls                                                                                                                                                | Planned, undecided                                                                                                                             |
| Fix the benchmark harness's stale startup template (`def main = …`)                                                                                                                                | Planned                                                                                                                                        |
| Fix "already defined" errors when go-to-definition opens Std                                                                                                                                       | Planned                                                                                                                                        |
| Fix the handler continuation parameter's inlay hint                                                                                                                                                | Planned                                                                                                                                        |
| Launch the REPL inside a project, with live reloading and third-party packages                                                                                                                     | Planned                                                                                                                                        |
| Hoogle-like fuzzy search for declarations                                                                                                                                                          | Planned                                                                                                                                        |
| Cross-compilation                                                                                                                                                                                  | Planned                                                                                                                                        |
| ABI backwards compatibility                                                                                                                                                                        | Planned                                                                                                                                        |
| A pull request to GitHub Linguist                                                                                                                                                                  | Planned                                                                                                                                        |

## Standard library and packages

| Task                                                                                                  | State                                                                                                                                                         |
| ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Vector-based collections, hash map                                                                    | Done                                                                                                                                                          |
| Parser combinators with effects; `keep` and `skip` for pipeline-style parsers (`87a1608`)             | Done                                                                                                                                                          |
| `Std.Json` with a byte scanner in front of its grammar (`aa26e3b`)                                    | Done                                                                                                                                                          |
| `Std.Terminal`: raw mode, key events, size (`67c310b`)                                                | In progress. Done on Unix on both runtimes; Windows answers "not a terminal", and the resize event is untested.                                               |
| `Std.Ffi`: open a library, find a function, call it, read and write C memory (`6f5e135`)              | In progress. The dynamic, unchecked interface is done on Unix. Typed `@extern` declarations, more than eight arguments, structs by value and Windows are not. |
| Pin a green thread to its OS thread for the length of a call, for C libraries with thread-local state | Planned                                                                                                                                                       |
| Third-party packages: ports of Rust crates, and Fuzzy, Doodle and LineEditor                          | Done, in their own repositories                                                                                                                               |
| SDL3 bindings and a GUI library                                                                       | Planned, after `@extern` and `bindgen`                                                                                                                        |
