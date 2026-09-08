# Meadow for VS Code

Language support for [Meadow](https://github.com/mcdearman/meadow).

- **Syntax highlighting** — a TextMate grammar, plus semantic tokens from the
  compiler's own lexer once the server is running (which is what tells a type
  from a constructor: `Maybe` and `Just` are both capitalised).
- **Diagnostics** — type and parse errors, live as you type.
- **Hover** — the inferred type, and the `--` doc comment above the definition.
- **Go to definition**.
- **Inlay hints** — inferred types after parameters and `let` bindings.

## Requirements

`meadow` on your `PATH`; the extension runs `meadow lsp`. If it lives elsewhere,
set `meadow.server.path`. Without it you still get syntax highlighting, just
nothing that needs the compiler.

## Settings

| | |
|---|---|
| `meadow.server.path` | path to the `meadow` executable (default `meadow`) |
| `meadow.inlayHints.enabled` | show inferred types inline |
| `meadow.trace.server` | log the LSP traffic, for debugging |
