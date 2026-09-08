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

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/mcdearman/meadow/master/install.ps1 | iex
```

Both drop a self-contained `meadow` into `~/.meadow/bin` and add it to your
PATH. They download a prebuilt binary when one exists for your platform and
otherwise build from source, which needs a [Rust toolchain](https://rustup.rs).

<details>
<summary>Options</summary>

```sh
install.sh --version v0.1.0   # pin a release
install.sh --from-source      # always build from source
install.sh --no-modify-path   # leave shell profiles alone
install.sh --uninstall
```

```powershell
.\install.ps1 -Version v0.1.0
.\install.ps1 -FromSource
.\install.ps1 -NoModifyPath
.\install.ps1 -Uninstall
```

`MEADOW_HOME` overrides the install directory.
</details>

## Use

```sh
meadow                          # REPL
meadow run examples/euler       # build a package and run `main`
meadow build path/to/pkg        # type-check and link
meadow build --release pkg      # ...with release checks
```

A package is a directory with a `meadow.toml` and a `src/`; `meadow run` also
takes a single `.mw` file. The `Std` library is embedded in the binary, so
there is nothing else to install.

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

The tree is three independent Cargo workspaces:

| | |
|---|---|
| `compiler/` | the front end, one crate per pass |
| `eval/` | the CEK machine |
| `meadow/` | build system, CLI and REPL — the binary |

```sh
cargo install --path meadow      # install the CLI
cd compiler && cargo test        # per-workspace tests
cd eval     && cargo test
cd meadow   && cargo test
```

## Releasing

Push a tag; `.github/workflows/release.yml` cross-builds for Linux, macOS and
Windows (x86-64 and arm64) and attaches the archives the installers look for.

```sh
git tag v0.1.0 && git push origin v0.1.0
```
