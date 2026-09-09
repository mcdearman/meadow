| `compiler/` | the front end, one crate per pass |
| `eval/` | the CEK machine — the specification of what a program means |
| `rts/` | the runtime system: a register bytecode VM (early) |
# Meadow

A small ML-family language: Hindley–Milner inference with row-polymorphic
records, algebraic effects with deep one-shot handlers, and a CEK evaluator.

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
meadow-setup.exe --version v0.1.0   pin a release
meadow-setup.exe --no-modify-path   leave PATH alone
meadow-setup.exe --uninstall
```

```sh
install.sh --version v0.1.0   # pin a release
install.sh --from-source      # always build from source (needs cargo)
install.sh --no-modify-path   # leave shell profiles alone
install.sh --uninstall
```

`MEADOW_HOME` overrides the install directory for both.
</details>

A full walkthrough of the language lives in [docs/TUTORIAL.md](docs/TUTORIAL.md).

## Use

```sh
meadow                          # REPL
meadow run examples/euler       # build a package and run `main`
meadow build path/to/pkg        # type-check and link
meadow build --release pkg      # ...with release checks
meadow fmt src                  # re-indent .mw sources in place
meadow fmt --check src          # ...or just report, and exit 1 if any differ
meadow test                     # run the package's `@test` functions
meadow test . parse             # ...only those whose name contains "parse"
```

A package is a directory with a `meadow.toml` and a `src/`; `meadow run` also
takes a single `.mw` file. The `Std` library is embedded in the binary, so
there is nothing else to install.

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

@test fun doubling u = assertEq (double 21) 42 "double 21"
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
what it actually got. `meadow test <path> <filter>` narrows by name.

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
def firstThree = Stream.take 3 (\u -> Stream.range 0 1000000)
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

`[]` matches an empty `Vector` — the library keeps `VEmpty` the only
representation of one, so it matches a vector emptied at run time too. A
*non-empty* `Vector` has no structural pattern (it is a balanced tree, not a
cons list); match on `Vector.len` or convert with `Vector.toList`.

### Editor support

`meadow lsp` is a language server — diagnostics as you type, hover with the
inferred type and the doc comment above the definition, go-to-definition, inlay
hints, and semantic highlighting from the compiler's own lexer. Any LSP client
can drive it; it speaks the protocol over stdin and stdout.

For VS Code, build and install the extension:

```sh
editors/vscode/build.sh
code --install-extension editors/vscode/meadow-0.1.0.vsix
```

Each release also attaches a built `.vsix`. The extension runs `meadow lsp`, so
it needs `meadow` on your `PATH` (or `meadow.server.path` set); without it you
still get syntax highlighting from the bundled TextMate grammar.

### Profiles

`--debug` (the default) and `--release` are bundles of compiler flags. Today the
only difference is that release requires every `match` to be exhaustive:

```sh
$ meadow run --release missing.mw
missing: non-exhaustive patterns: `None` is not matched
```

Irrefutability of *binding* positions — function and lambda parameters, `def`
and `let` destructuring — is checked in both profiles, since those have no
fallback arm.

## Building from a checkout

The tree is five independent Cargo workspaces:

| | |
|---|---|
| `compiler/` | the front end, one crate per pass |
| `eval/` | the CEK machine — the specification of what a program means |
| `rts/` | the runtime system: a register bytecode VM, checked against `eval` (early) |
| `buildtools/` | the tools you point at Meadow source: `meadow` (build system, CLI and REPL — the binary) and `meadow-fmt` (the formatter) |
| `installer/` | `meadow-setup.exe`, the Windows installer |

```sh
scripts/check.sh                         # everything CI runs
scripts/check.sh --strict                # ...plus rustfmt and clippy
cargo install --path buildtools/meadow   # install the CLI
```

`scripts/check.sh` is the single entry point: `ci.yml` runs it on every push and
`release.yml` runs it before publishing, so a green run locally is a green run
there. It covers all four workspaces — there is no one `cargo test --workspace`
that does, which is the reason it exists.

## Releasing

Push a tag; `.github/workflows/release.yml` cross-builds for Linux, macOS and
Windows (x86-64 and arm64) and attaches the archives the installers look for.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The tag is gated on `scripts/check.sh --version <tag>`, which fails if the tag
disagrees with the version in any of the four `Cargo.toml`s — the release assets
carry no version, so nothing downstream would catch that.
