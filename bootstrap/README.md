# MeadowBoot

Meadow's compiler, written in Meadow — the bootstrap. It is built with
[Lingua](https://github.com/mcdearman/Lingua), Meadow's language workbench,
and its lexer is written by [Scythe](https://github.com/mcdearman/Scythe).

Each pass is checked against the Rust compiler's, input by input. A pass
writes what it made as text, a line an item, and the test writes the Rust
pass's output the same way. A file whose two texts differ is a difference
between the compilers, shown at its first line. Nothing moves on to the next
pass until the one before agrees everywhere it is asked.

## Passes

| pass     | module                                   | checked against                        | inputs                                                                   |
| -------- | ---------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------ |
| lex      | `src/Lex.mw`                             | `meadow-lexer`'s `tokenize`            | every `.mw` in the repository, `tests/lex/`                              |
| evaluate | `src/Cek.mw`, `Core.mw`                  | `meadow-eval`, the Rust CEK machine    | the programs `glade/tests/differential.rs` checks every back end with    |
| parse    | `src/Syntax.mw`                          | `meadow-parser`                        | every `.mw` in the repository: each parses (the trees are compared next) |
| group    | `src/Fixity.mw`                          | `meadow-rename`'s `reassociate`        | every `.mw` in the repository, each with its unit's fixities             |
| rename   | `src/Lower.mw`, `Rename.mw`, `Scopes.mw` | `meadow-rename`, as an editor reads it | every `.mw` in the repository, each with its unit and what it depends on |
| infer    | —                                        | `meadow-infer`                         |                                                                          |
| lower    | —                                        | `meadow-core`'s lowering               |                                                                          |

### Lexing

Two Scythe lexers taking turns, as logos's `morph` has them: `Code` for the
program, `Piece` for the text of a string literal, each asked for one token at
a time from wherever the other stopped. That is how a `${…}` hole — code again,
strings and braces and all — is read, which no one pattern can match. What logos
decides by priority, Scythe decides by the order the variants are written in,
so they are written in that order; where logos matches a character, the
patterns match one of UTF-8's. An error spans what the machine read before it
gave up, as logos's does.

### Evaluating

The CEK machine is `meadow_eval`'s, frame for frame: the same continuation,
the same order of evaluation, deep one-shot handlers. It runs `Core`, a Lingua
language (`lang!`): a program is tables, a term a row of them, read with the
patterns `lang!` makes. For now Core is read from the core the Rust compiler
makes — generic definitions copied per number type, types erased — written in
`CoreText`, a grammar Lingua makes the parser of (`syntax!`); the language that
grammar implies, `Printed`, becomes Core by a pass (`pass! read`), which makes
numbers of numerals and a float of its bits. Numbers are Meadow's own: a
`UInt8` in the program is a `UInt8` here, so what it does is what the machine
running MeadowBoot does, which the Rust machine's differential tests already
hold to the same answers. Values print as the Rust machine prints them.

### Parsing

Lingua writes the parser from the grammar in `src/Syntax.mw` (`syntax!`), an
automaton decided when MeadowBoot is compiled: each choice a test of at most
five tokens ahead, and nothing tried and taken back. The Rust parser is a PEG
and does take things back; where it does, the grammar says the same thing a
way looking ahead can decide — patterns in parentheses read as expressions
first, as GHC reads them; a signature and a definition, or a context and a
type, read as one form and told apart after. Words that mean something only
where they are (`pattern`, `rec`, `.*`, `<:`, `:?`) and the names in a macro
call's path are kinds of their own, given by the lexer.

The abstract syntax is not a second tree. `syntax!` makes each rule a pattern
over the lossless tree, `SurfaceIfE cond yes no`, which matches an `if` node and
binds its parts, themselves trees to match further. `src/Parse.mw` reads the
tree with them and writes what `meadow-parser` would have built, desugarings
and spans included: several equations as one `match`, sections as lambdas,
`[a .. b]` as `range`, synonyms as their matcher and builder. What the grammar
reads as one form and the Rust parser does not accept is refused there too: an
expression in a pattern's parentheses that is no pattern, a context on a
binding, a signature with parameters. The parse test compares the two dumps
over every source in the repository and the cases in `tests/parse/`, those in
`tests/parse/rejected/` each one the Rust parser refuses.

### Grouping operators

The parser reads `a + b * c` as a chain; how it groups waits for every fixity
the unit declares. The fixity pass (`pass! regroup`) rewrites the parser's tree
as one of `Grouped`, a tree language (`lang!`) whose chains are `Infix` nodes,
grouped as `meadow-rename` groups them, each byte still where it was.

### Renaming

A unit's modules are lowered from their grouped trees (`src/Lower.mw`) to
`Ast`, a table language shaped as `meadow-parser`'s abstract syntax, read as
the parse test reads a tree. Renaming is a pass from `Ast` to `Hir`
(`pass! rename`), whose context is the scope and whose side table (`Lingua.Table`)
is every binder: what it is called, where it was declared, and every place it
is used. `src/Scopes.mw` works out each module's scope as the Rust resolver
does: locals; then the module's own names and what its `use`s bring, first come
first served; then the base -- the primitives, then the prelude. Nothing else is
in scope: a sibling's names and a dependency's arrive only through a `use`.

What is compared is every name a module writes and where what it names was
declared, `from to kind file@offset`, which the Rust side reads off the resolved
HIR the way the language server does (`meadow_lsp::analysis::mentions`). A unit
is renamed with what the units before it export, so each unit's interface
carries its exports, and Lingua's build (`make!`) compiles them in order.

A macro call is what the Rust compiler expanded it to, for now, as the CEK
machine's core is what it lowered: the expander records every expansion, and
each `@derive`'s, as JSON of its syntax with its spans
(`meadow_compiler::expand::record`); the rename test writes a package's to
`Pkg.json` where `MEADOWBOOT_EXPANSIONS` says, which a unit reads as one of its
files, and `Lower` takes an expansion where its call is. A chain of operators
in one is grouped there by the unit's fixities. A name a template wrote keeps
its hygiene mark, `tmp#3`, and is resolved as the Rust resolver resolves it.
`macro` rules are to be expanded here next, checked against these.

## Running

```sh
meadow run --release bootstrap -- lex FILE…     # tokens, one a line
meadow run --release bootstrap -- eval FILE…    # a core program's value
meadow run --release bootstrap -- parse FILE…   # each file's abstract syntax
meadow run --release bootstrap -- fixity FILE…  # each chain of operators, grouped
meadow run --release bootstrap -- rename FILE…  # each name, and what it names
```

and the differential tests, from `buildtools/`:

```sh
MEADOWBOOT_MEADOW=target/release/meadow cargo test -p meadow --test bootstrap
```

`MEADOWBOOT_MEADOW` names the `meadow` that builds and runs MeadowBoot; the
debug one the test is built with works too, slowly.

## What the Rust compiler learned

Writing a second compiler finds what the first one does that nothing else
checks:

- The CEK machine printed a record's fields in the order their labels were
  interned — the process's order, not the program's. It prints them by label
  now, as every other machine does.
- A recursive group's functions were seeded as bare type variables, so a
  sibling applied to fewer arguments than it takes (`map (f k) xs`) was taken to
  perhaps perform something, and that guess was joined to the caller's effect.
  When the function turned out to be pure until its last argument, the guess
  closed the caller's effect too: `the effect Tick is not allowed here`. A
  function is seeded as the function it is now.
- A native build of a large program named each of its hundreds of objects on
  clang's command line, past the 32K characters Windows allows. They are in a
  response file now.
- Type names were unique across a package, not by their path: two modules'
  private `type Syn` clashed, and one module could resolve the other's. A type
  is known by its fully qualified path now, the standard library's too.
