# Meadow

A small ML-family language: Hindley–Milner inference with row-polymorphic
records, algebraic effects with deep one-shot handlers, and a register bytecode
VM with a copying collector.

```
fun classify n =
  match compare n 0 with
  | Less    -> "negative"
  | Equal   -> "zero"
  | Greater -> "positive"

def main = classify 42
```

## Install

**Windows** — download `meadow-setup-x86_64.exe` from the
[latest release](https://github.com/mcdearman/meadow/releases/latest) and run
it. (Take `meadow-setup-aarch64.exe` on an ARM machine.)

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/install.sh | sh
```

Either way you get a self-contained `meadow` in `~/.meadow/bin`, added to your
`PATH` — open a new terminal and run `meadow`.

<details>
<summary>Options</summary>

`meadow-setup.exe` installs the `meadow.exe` sitting next to it if there is one,
so you can also unzip a release and install offline.

```
meadow-setup.exe --dir <path>       install somewhere else
meadow-setup.exe --from <path>      install a specific meadow.exe
meadow-setup.exe --version v0.1.0-alpha   pin a release
meadow-setup.exe --no-modify-path   leave PATH alone
meadow-setup.exe --uninstall
```

```sh
install.sh --version v0.1.0-alpha   # pin a release
install.sh --from-source      # always build from source (needs cargo)
install.sh --no-modify-path   # leave shell profiles alone
install.sh --uninstall
```

`MEADOW_HOME` overrides the install directory for both.
</details>

A full walkthrough of the language lives in [docs/TUTORIAL.md](docs/TUTORIAL.md).
How programs run -- the backends, the memory model, effects and threads -- is
in [docs/RUNTIME.md](docs/RUNTIME.md).

## Examples

Each is a package: `meadow run examples/<name>` runs it, `meadow test
examples/<name>` runs its tests.

| | |
|---|---|
| [`tour`](examples/tour) | the language in one program: data types, records, matching, numbers, modules, local mutation |
| [`effects`](examples/effects) | one program under different handlers: logging, configuration, failure, state, a seeded random game, a tested interactive greeter |
| [`streams`](examples/streams) | generators with `yield`: infinite streams, `take`, `filter`, `map` |
| [`concurrency`](examples/concurrency) | green threads and channels: a worker pool, a pipeline, fan-out and fan-in |
| [`parallel`](examples/parallel) | splitting work across every core, and sharing a compacted table between threads |
| [`stm`](examples/stm) | transactional memory: a bank whose total never wavers, transfers that wait for funds, bounded queues |
| [`mini-ml`](examples/mini-ml) | a small ML with Hindley-Milner inference and an interpreter, over four modules |
| [`euler`](examples/euler) | Project Euler problems |
| [`rock-paper-scissors`](examples/rock-paper-scissors) | an interactive game, tested by feeding it input |

## Use

```sh
meadow                          # REPL
meadow init                     # start a package here, named after the directory
meadow init pkg --name myPkg    # ...or elsewhere, under a name you choose
meadow run examples/euler       # build a package and run `main`, on the VM and its JIT
meadow run --release pkg        # ...optimized, as an executable compiled ahead of time
meadow run --backend vm pkg     # ...on the bytecode VM alone (or `jit`, `aot`)
meadow run --cek pkg            # ...on the CEK machine
meadow build path/to/pkg        # type-check, link, and write the bytecode image
meadow build --release pkg      # ...optimized, `match` exhaustive, and an executable
meadow build --aot --target x86_64 pkg  # an executable for another architecture
meadow run -O2 pkg              # ...or just the optimization level
meadow dis pkg                  # disassemble: the bytecode the VM would run
meadow run pkg -- in.txt -v     # pass the program arguments, which `Process.argv` reads
meadow exec target/debug/bytecode/pkg.mbc   # run a bytecode image
meadow link pkg.mbc -o pkg      # ...or compile one into an executable
meadow fmt src                  # re-indent .mw sources in place
meadow fmt --check src          # ...or just report, and exit 1 if any differ
meadow test                     # run the package's `@test` functions
meadow test . parse             # ...only those whose name contains "parse"
meadow test . Parser.parse --exact  # ...or exactly one
meadow init --workspace shop    # a workspace; `meadow init` inside it adds a member
meadow run -p app               # in a workspace: a member, by name
meadow test --workspace         # ...or every member (`--exclude NAME` leaves one out)
```

A package is a directory with a `meadow.toml` and a `src/`, which `meadow init`
writes for you; `meadow run` also takes a single `.mw` file. The `Std` library
is embedded in the binary, so there is nothing else to install.

What a build makes goes in the package's own `target/` directory, one directory
per profile: the bytecode image at `target/debug/bytecode/<name>.mbc`, and
native object code and executables under `target/<profile>/native/`. `meadow
init` writes a `.gitignore` that ignores it.

Builds are **incremental**. Every package that compiles cleanly, and the
embedded `Std`, is kept under `target/<profile>/incremental/` with a
fingerprint of its sources, the compiler and its options, and the packages it
depends on. The next build reads back each one whose fingerprint still matches,
so an edit recompiles only the package changed and those downstream of it.
`MEADOW_INCREMENTAL=0` turns this off.

Packages developed together can be a **workspace**, as in Cargo: a root
`meadow.toml` with `[workspace] members = ["app", "libs/*"]`, whose members
share one `target/`, the root's `[profile.*]`, and a version and dependencies
declared once under `[workspace.package]` and `[workspace.dependencies]`
(`util = { workspace = true }` in a member). The
[tutorial](docs/TUTORIAL.md#workspaces) has the details.

### Native code

Meadow compiles its bytecode to machine code itself, for **aarch64** and
**x86-64**, with no LLVM or Cranelift: typed arithmetic, comparisons, branches,
moves, constants and the common allocations run as machine instructions, and
everything else calls back into the runtime. There are three backends, and the
same code generator serves the two that use it:

| backend | | default for |
|---|---|---|
| `jit` | the VM, compiling each block to machine code once it has run 16 times (`MEADOW_JIT_THRESHOLD`) | `--debug` |
| `aot` | an executable: a Mach-O or ELF object holding the code and the program's image, linked by the system's C compiler against the runtime | `--release` |
| `vm` | the bytecode interpreter alone | |

`--backend <vm|jit|aot>` (or `--jit`, `--aot`) picks one for a command, and a
package can pick one per profile in its manifest:

```toml
# meadow.toml
[profile.debug]
backend = "vm"

[profile.release]
backend = "jit"
```

`aot` links against the runtime built as a static library: `cargo build
--release` in `rts`, or with `--target x86_64-apple-darwin` for the other Mac
architecture; `MEADOW_RUNTIME` can name one. It has to be built from the same
sources as `meadow`, and a program will not link against one that is not. When
`aot` is only the release default and no executable can be made -- a lone `.mw`
file, which has no `target/` to put one in, or no runtime library -- the JIT
runs the program instead, with a warning; an `aot` that was asked for is an
error. `meadow test` runs in-process, so it uses the JIT for both.

A package's name is the first segment of a `use` path, so it has to lex as one
identifier — `meadow init` says so rather than letting a directory called
`my-pkg` produce a package nothing can refer to.

### Two machines

Programs run on a **register bytecode VM** with a generational garbage
collector: each green thread's heap is collected in pauses of tens of
microseconds, however much it keeps alive, because the old generation is marked
on another OS thread while the program runs.
Behind it, the compiler lowers to a sequent-calculus IR (AxCut) and then to
straight-line instructions over a flat register file — with no call stack, since
in that IR returning from a function is entering the continuation it was given.

The older **CEK abstract machine** is still there, and is still the definition of
what a Meadow program means. `--cek` on `run` and `test`, or `:cek` in the REPL,
switches to it; if the two disagree the CEK is right and the VM has a bug, which
makes the flag the first thing to reach for when a program does something
inexplicable. Both run the whole standard library test suite on every CI build,
so a disagreement should not survive long enough for you to find one.

### Formatting

`meadow fmt` is an indenter, not a pretty-printer: it fixes leading and trailing
whitespace, tabs and blank-line runs, and never moves a token to another line.
Comments therefore survive exactly as written, and since Meadow's grammar is not
layout-sensitive, formatting can never change what a program means.

Indentation comes from structure — brackets, `match` arms lining up with their
`match`, `then`/`else` with their `if`, `in` with its `let`, two units for an arm
body on its own line. A line that only *continues* the expression above it has no
structural anchor, and there `fmt` keeps the column you chose, so deliberate
alignment like this is left alone:

```
bitOr (bitOr (bytesGetOr 0 b i)
             (bytesGetOr 0 b (i + 1) << 8))
      (bitOr (bytesGetOr 0 b (i + 2) << 16)
             (bytesGetOr 0 b (i + 3) << 24))
```

The REPL uses the same rules to indent continuation lines as you type them, plus
the width of the `> ` prompt — which only the first line carries — so what you
see lines up the way it would in a file:

```
> fun f x =
    match x with
    | A ->
        1
```

### Testing

Mark a function `@test` and `meadow test` runs it, cargo-style:

```
use Std.Test (assertEq)

@test fun doubling () = assertEq (double 21) 42 "double 21"
```

```sh
$ meadow test
running 2 tests
test doubling ... ok
test thisOneFails ... FAILED

failures:

---- thisOneFails ----
double 21: expected 43, got 42

test result: FAILED. 1 passed; 1 failed
```

`meadow test --std` also runs the standard library's own tests, which live at
the bottom of each `Std` module.

A test takes one argument (the runner calls it with `()`) and fails by
performing `Std.Test`'s `Test` effect, which `assert` / `assertEq` do for you —
so an assertion five calls deep still stops the test and still names itself.
Both `==` and `show` are structural, so `assertEq` works at any type and prints
what it actually got.

A test is named by its module — `Parser.handlesEmpty`, or just `handlesEmpty`
in the package's root module — because two modules may each declare a test of
the same name. `meadow test <path> <filter>` runs the tests whose name contains
`<filter>`; add `--exact` to run only the one whose name *is* it, which is what
the editor's **▶ Test** link above each `@test` does.

### Effects

`Std` leans on algebraic effects for the things that are usually hardest to
test. Each has a real implementation *and* a handler that fakes it:

| module | with no handler | handled |
|---|---|---|
| `Std.Random` | real entropy | `withSeed 42` — a pure SplitMix64, same numbers every run |
| `Std.Time` | the real clock | `withClock t`, `withTickingClock t tick` |
| `Std.State` | — | `runState`, `evalState`, `execState` |
| `Std.Exn` | — | `toResult`, `catch`, `withDefault`, `toMaybe` |
| `Std.Stream` | — | `toList`, `toVec`, `fold`, `take`, `find`, `map`, `filter` |
| `Std.Fs`, `Std.Process` | real I/O | any `handle` |

`Std.Stream` is generators: a producer performs `yield` and a consumer decides
what that means. `take` simply doesn't resume, which unwinds the producer — the
part a lazy list gives you for free and an eager language otherwise cannot do:

```
def firstThree = Stream.take 3 (\() -> Stream.range 0 1000000)
```

Every collection has `toStream`, and `Std.Stream` has `ofList` / `ofVec` for the
other direction.

### Sequences

There are two sequence types and one rule for telling them apart: **a `;` means
the linked `List`; brackets without one mean the default RRB `Vector`.** It holds
for types, literals and patterns alike.

| | `Vector` | `List` |
|---|---|---|
| type | `[a]` | `[a;]` |
| empty | `[]` | `[;]` |
| one element | `[x]` | `[x;]` |
| several | `[x, y, z]` | `[x; y; z]` |

`#[x, y]` is the third, lower-level one: the builtin `Array`, which both are
built on.

Patterns follow the same rule, with `::` as the usual `Cons` sugar:

```
fun total xs = match xs with
  | [;] -> 0
  | x :: rest -> x + total rest
```

`[]` matches an empty `Vector` — the library keeps `Vector.Empty` the only
representation of one, so it matches a vector emptied at run time too. A
*non-empty* `Vector` has no structural pattern (it is a balanced tree, not a
cons list); match on `Vector.len` or convert with `Vector.toList`.

### Editor support

`meadow lsp` is a language server — diagnostics as you type, hover with the
inferred type and the doc comment above the definition, go-to-definition,
rename across a whole package, inlay hints, and semantic highlighting from the
compiler's own lexer. A file inside a package is analysed as part of it, so a
`use` of a sibling module resolves the way it does in a build. Any LSP client
can drive it; it speaks the protocol over stdin and stdout.

For VS Code, build and install the extension:

```sh
editors/vscode/build.sh
code --install-extension editors/vscode/meadow-0.1.0-alpha.vsix
```

Each release also attaches a built `.vsix`.

The extension runs `meadow lsp`, and finds it on your `PATH`, then in
`$MEADOW_HOME/bin`, `~/.cargo/bin` and `~/.meadow/bin` — the last three because
an editor launched from the Dock or the Start menu does not inherit your shell's
`PATH`. It checks that whatever it finds actually understands `lsp`, so an old
`meadow` is skipped rather than started and left to fail. `meadow.server.path`
overrides all of it. Without a server you still get syntax highlighting from the
bundled TextMate grammar.

### Debugging

`meadow dap` is a debug adapter, and the VS Code extension registers it: open a
`.mw` file and press F5, or add a `meadow` launch configuration naming a
`program` (a package directory or a single file). You get breakpoints, step
in / over / out, the call stack, and three views of a stopped program:

- **Locals** — the names in scope with their inferred types, and values you can
  open up: a `Just [1; 2]` expands into its list, a record into its fields, a
  closure into what it captured.
- **Registers** — the VM's raw register file, with the names the compiler put
  in each, and the program counter and instruction count under **Heap**.
- **Handlers** — the effect handlers installed, and what each one covers.

A program need not start at `main`. Each top-level definition has a **▶ Debug**
link above it (and **Meadow: Debug Function…** on the right-click menu) that asks
for its arguments as Meadow expressions — showing its type while you type them,
and remembering what you gave last time — then debugs a call to it, stopped as
the function is entered. The call is compiled inside the function's own module,
so a private helper can be debugged as easily as an exported one, and an
argument of the wrong type is a compile error before anything runs. In
`launch.json` the same thing is an `entry`:

```json
{
  "type": "meadow",
  "request": "launch",
  "name": "Debug eval",
  "program": "${workspaceFolder}/examples/mini-ml",
  "entry": {
    "module": "${workspaceFolder}/examples/mini-ml/src/Eval.mw",
    "expression": "eval [;] (Expr.Int 1)",
    "function": "eval"
  }
}
```

The machine has no call stack — returning is invoking a continuation on the
heap — so the adapter reconstructs one from the continuation chain, using what
the compiler records about which name is a function's return continuation. A
call in tail position replaces its caller's frame, as it does at run time. The
program's `print` output goes to the Debug Console; `Console.readLine` sees the
end of input, since the adapter's own stdin is the protocol.

A debug session compiles at `-O1` with source positions kept all the way to the
bytecode. That changes where the compiler remembers things, not what the
program does: the standard library's test suite runs identically either way,
and recording the positions changes no instruction.

### Profiles

`--debug` (the default) and `--release` are names for a bundle of compiler
options, not options themselves. There are two:

| | `--debug` | `--release` |
|---|---|---|
| optimization level | `-O1` | `-O2` |
| non-exhaustive `match` | allowed | an error |
| backend | `jit` | `aot` |

```sh
$ meadow run --release missing.mw
missing: non-exhaustive patterns: `None` is not matched
```

Irrefutability of *binding* positions — function and lambda parameters, `def`
and `let` destructuring — is checked at both, since those have no fallback arm.

`-O2` compiles a `match` to a decision tree rather than trying its arms in turn.
Everything below it is unconditional: a known call becoming a jump, a literal
folding into the instruction that uses it, a comparison fusing into the branch
that tests it. Those cost nothing to read and nothing to compile, so a debug
build gets them too — `-O0` exists for the first pass that changes that, and is
`-O1` today.

Either axis can be set on its own, and a package can change what the profiles
mean for it:

```sh
meadow run -O2 pkg          # optimize, but still allow a half-written `match`
meadow build --strict pkg   # check exhaustiveness without optimizing
```

```toml
# meadow.toml
[profile.debug]
opt-level = 2               # this package is slow to run, not slow to build

[profile.release]
strictness = "lenient"      # "lenient" | "strict"
backend = "jit"             # "vm" | "jit" | "aot"
```

A flag beats the manifest, and the manifest beats the profile's built-in
meaning. A key that is absent is simply not overridden, so a `[profile.debug]`
that names only `opt-level` leaves everything else as debug.

## Building from a checkout

The tree is five independent Cargo workspaces:

| | |
|---|---|
| `compiler/` | the front end and back end, one crate per pass — through `meadow-seq` (the AxCut IR and its reference machine) and `meadow-codegen` to `meadow-bytecode` |
| `eval/` | the CEK machine — the specification of what a program means |
| `rts/` | the runtime: a register bytecode VM, green threads, and a low-pause generational collector (a copying nursery, and an Immix old generation marked concurrently and evacuated a block at a time). It loads an image and knows nothing about the IR that produced it |
| `buildtools/` | the tools you point at Meadow source: `meadow` (build system, CLI and REPL — the binary) and `meadow-fmt` (the formatter) |
| `installer/` | `meadow-setup.exe`, the Windows installer |

```
core ──▶ AxCut ──▶ bytecode ──▶ VM
          │
          └─▶ the AxCut machine, and the CEK machine beside it: two
              independent checks that the pipeline preserved meaning
```

```sh
scripts/check.sh                         # everything CI runs
scripts/check.sh --strict                # ...plus rustfmt and clippy
scripts/bench.sh                         # the benchmarks, on both runtimes
scripts/bench-compact.sh                 # what compacting a large live value saves the collector
scripts/install-local.sh                 # install this checkout the way a release installs
scripts/install-local.sh --no-extension  # ...just `meadow`, not the VS Code extension
cargo install --path buildtools/meadow   # install the CLI
```

`scripts/check.sh` is the single entry point: `ci.yml` runs it on every push and
`release.yml` runs it before publishing, so a green run locally is a green run
there. It covers all five workspaces — there is no one `cargo test --workspace`
that does, which is the reason it exists.

## Releasing

Push a tag; `.github/workflows/release.yml` cross-builds for Linux, macOS and
Windows (x86-64 and arm64) and attaches the archives the installers look for.

Until the language settles there is one release, `v0.1.0-alpha`, and a new
build replaces it rather than adding another. Move the tag to the commit to
publish and push it again; the workflow deletes the old release before making
the new one:

```sh
git tag -f v0.1.0-alpha && git push -f origin v0.1.0-alpha
```

The tag is gated on `scripts/check.sh --version <tag>`, which fails if the tag
disagrees with the version in any of the five `Cargo.toml`s — the release assets
carry no version, so nothing downstream would catch that.
