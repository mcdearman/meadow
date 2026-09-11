# Meadow for VS Code

Language support for [Meadow](https://github.com/mcdearman/meadow).

- **Syntax highlighting** — a TextMate grammar, plus semantic tokens from the
  compiler's own lexer once the server is running (which is what tells a type
  from a constructor: `Maybe` and `Just` are both capitalised).
- **Diagnostics** — type and parse errors, live as you type.
- **Hover** — the inferred type, and the `--` doc comment above the definition.
- **Go to definition** — including into another module of the package, and
  into the standard library.
- **Rename** (F2) — every occurrence of the binding under the cursor, across
  every module of its package. It follows name resolution rather than text, so
  a shadowed `x` elsewhere is left alone and two constructors that share a
  spelling are told apart. A name from the standard library is refused, since
  its definition is not yours to edit.
- **Inlay hints** — inferred types after parameters and `let` bindings.

## Requirements

A `meadow` that supports `meadow lsp` — the extension starts it and talks to it
over stdin and stdout. Without one you still get syntax highlighting; everything
that needs the compiler is off.

It is looked for in this order, so an ordinary install needs no configuration:

1. your `PATH`;
2. `$MEADOW_HOME/bin`, if that is set — both installers honour it;
3. `~/.cargo/bin` (`cargo install`);
4. `~/.meadow/bin` (`install.sh` and the Windows installer).

`PATH` is searched first because what you put there is what you meant, and the
last three are searched at all because a VS Code launched from the Dock or the
Start menu does not inherit your shell's `PATH`.

Each candidate is asked whether it understands `lsp` before it is used, so a
`meadow` left over from before the language server existed is skipped rather
than started and left to fail — and if that is all there is, the extension says
so instead of going quiet.

`meadow.server.path` overrides the search entirely.

## Settings

| | |
|---|---|
| `meadow.server.path` | path to the `meadow` executable (default `meadow`) |
| `meadow.inlayHints.enabled` | show inferred types inline |
| `meadow.trace.server` | log the LSP traffic, for debugging |
