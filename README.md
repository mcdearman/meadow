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

The tree is four independent Cargo workspaces:

| | |
|---|---|
| `compiler/` | the front end, one crate per pass |
| `eval/` | the CEK machine |
| `meadow/` | build system, CLI and REPL — the binary |
| `installer/` | `meadow-setup.exe`, the Windows installer |

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
