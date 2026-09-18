# Macros

The design for Meadow's macros: what they look like, how they are expanded, and
what is deliberately left out. Written before the implementation, so much of it
describes code that does not exist yet; [section 12](#12-the-order-this-is-built-in)
is the order it is being built in, and says how far that has got.

- [1. What a macro is](#1-what-a-macro-is)
- [2. Calling one](#2-calling-one)
- [3. Defining one](#3-defining-one)
- [4. Fragments and repetition](#4-fragments-and-repetition)
- [5. How expansion runs](#5-how-expansion-runs)
- [6. Hygiene](#6-hygiene)
- [7. Spans and diagnostics](#7-spans-and-diagnostics)
- [8. Exporting, and incremental builds](#8-exporting-and-incremental-builds)
- [9. Procedural macros](#9-procedural-macros)
- [10. Macros that talk to each other](#10-macros-that-talk-to-each-other)
- [11. What is left out](#11-what-is-left-out)
- [12. The order this is built in](#12-the-order-this-is-built-in)

## 1. What a macro is

A macro takes **token trees** and produces token trees. A token tree is a token,
or a bracketed run of token trees — `( … )`, `[ … ]`, `{ … }` — so the only
thing a macro's argument has to satisfy is that its brackets balance. It need
not be an expression, or parse at all.

This is Rust's arrangement, and the reason for it is that the parser stays
fixed. A macro call is parsed as an opaque argument, expanded later, and the
result is parsed with the ordinary grammar. Nothing a macro does can change how
the rest of the file parses, and `meadow fmt` — an indenter that never moves a
token to another line — keeps working inside a macro call it has never heard of.

## 2. Calling one

A call is a name, `!`, and one bracketed argument:

```meadow
assertEq!(got, want, "they differ")
vec![1; 2; 3]
config! { name = "demo", debug = True }
```

The three brackets mean the same thing. Which one to use is a question of how
the call reads, exactly as in Rust.

A call is an **expression**, a **pattern** or a **declaration**, decided by where
it is written. In declaration position it takes attributes like any other
declaration:

```meadow
@pub
derive! { Show for Colour }
```

`name!` cannot be confused with anything else: `!` is otherwise only read after
`->`, as the effect arrow of a function type. One lexical trap is worth knowing:
`foo!=x` is `foo != x`, because `!=` is one token. A call is always followed by
an opening bracket, so this cannot arise in a real call, and `foo! =` should be
reported as the mistake it is.

## 3. Defining one

A macro matches token trees against rules and takes the first that fits — which
is what `match` does, so a macro is written the way a `match` is:

```meadow
macro swap
  | ($a, $b) -> { ($b, $a) }
```

Each rule is `| ⟨matcher⟩ -> { ⟨template⟩ }`. The matcher is a token tree, and
so is the template; the braces around the template are its brackets, not a
record. Rules are tried in order:

```meadow
@pub macro vec
  | ()                 -> { empty }
  | ($x)               -> { push empty $x }
  | ($x, $( $rest ),+) -> { push (vec!($( $rest ),+)) $x }
```

`macro` is a declaration, so everything that applies to declarations applies to
it: `@pub` exports it and `@pub(pkg)` / `@pub(super)` narrow that, `@cfg`
compiles it conditionally, and a doc comment above it is its documentation.

Macros have a **namespace of their own**, so a function `vec` and a macro `vec!`
can both exist, and the `!` is what an import names the macro by:

```meadow
use Std.Collections.Vector (vec!)
```

### `$` is the language's splice

`$name` in a template is the fragment the matcher bound. This is not borrowed
notation: `${…}` is already a hole in an interpolated string, and it is the same
idea. The two compose without a rule saying they must, because what is inside
`${…}` is tokens:

```meadow
macro describe
  | ($x : expr) -> { "value: ${$x}" }
```

Two more forms:

| written | means |
|---|---|
| `$$` | a literal `$` in the output |
| `$pkg` | the package the macro was defined in |

`$pkg` is Rust's `$crate`. An exported macro whose template calls a helper
writes `$pkg.Text.escape`, and that resolves wherever the macro is expanded
rather than wherever it happens to land.

## 4. Fragments and repetition

A matcher binds a metavariable with `$name : kind`, which reads the way a type
annotation does:

```meadow
macro unlessEmpty
  | ($xs : expr, $body : expr) -> {
      match $xs with
      | [;] -> ()
      | _   -> $body
    }
```

| kind | matches |
|---|---|
| `tt` | one token tree |
| `ident` | one identifier |
| `lit` | one literal |
| `expr` | an expression |
| `pat` | a pattern |
| `item` | a declaration |

`tt`, `ident` and `lit` need no parser at all. The rest are matched by calling
the parser on the rest of the tokens, which raises the question of where the
fragment stops.

**Meadow needs stricter follow rules than Rust.** Application is juxtaposition,
so an expression does not end where a Rust one would: in `($f : expr $x : expr)`
the first fragment swallows the second, and no amount of care in the matcher
changes that. So a fragment may only be followed by a token that cannot continue
it: `,` `;` `->` `|` a closing bracket, or one of `then`, `else`, `in`, `with`. A
matcher that puts anything else after an `expr` is rejected where it is written,
not where it is called.

That rule is what says where a fragment *ends*, and matching uses it directly:
the fragment runs to the first such token at the top level of the argument —
brackets are already grouped, so a `,` inside one is not a candidate — and that
run is handed to the parser. A run the parser cannot read is not a fragment of
that kind, so the rule does not match and the next is tried, exactly as a
mismatched token does.

A fragment written into a template comes out **parenthesised**, so that it stays
one thing: `$x` bound to `1 + 2` under `show $x` is `show (1 + 2)`, which is what
was passed, and not `(show 1) + 2`. Rust uses an invisible bracket for the same
purpose; a real one costs nothing and is honest in `stringify!`. Declarations are
not parenthesised, since a declaration is never part of something larger.

The same rule applies inside a repetition, where the separator is what ends each
pass: `$( $e : expr ),*` is fine, and `$( $e : expr )*` is refused, because
nothing says where one expression stops and the next begins.

Repetition is `$( … )` with a separator and a count, as in Rust:

```meadow
macro logAll
  | ($( $x : expr ),*) -> { let _ = [ $( println $x );* ] in () }
```

`*` is zero or more, `+` one or more, `?` zero or one. The separator in the
template need not be the one in the matcher — above, the matcher separates with
`,` and the template with `;`, because the template builds a list.

## 5. How expansion runs

Expansion happens on the AST, before name resolution, in `compile_unit` where
[`cfg::strip`](../compiler/meadow-compiler/src/cfg.rs) already runs.

1. The lexer produces tokens, and a **token-tree builder** groups them by
   bracket. Unbalanced brackets are a lexical error, reported there.
2. The parser produces an AST in which a macro call is an **unexpanded node** —
   `Expr::MacCall`, `Pat::MacCall`, `Decl::MacCall` — holding the macro's path
   and its argument token tree. The parser never looks inside one.
3. The expander loops: strip `@cfg`, find a call, match its argument against the
   macro's rules, substitute into the template, **parse the result with the
   entry point for that position**, and splice it in. Whatever comes out may
   contain calls of its own, so this repeats to a depth limit.

The parser therefore needs an entry point per position — an expression, a
pattern, a declaration list — rather than only a whole module. That is the first
thing the implementation adds, and it is useful on its own: it is what the REPL
and the language server would rather call.

`@cfg` is stripped before expansion and again after it, so a `@cfg` on a call
removes the call, and one in a template is read in the expansion.

## 6. Hygiene

A macro that writes `let tmp = …` in its template must not capture a `tmp` the
caller passed in, and must not be capturable by one the caller defines. Meadow
takes Rust's **mixed-site** position: local bindings are hygienic, and items,
types and constructors are not.

The mechanism is a **mark**. Every identifier a template writes comes out as
`tmp#3`, the number being that expansion's; a `#` cannot be lexed into an
identifier, so no source can spell a marked name and no two expansions share
one. Everything else follows from that:

- a template's `let tmp#3 = …` binds a name the caller cannot write, so the
  caller's `tmp` is a different variable;
- what came from the *call* is spliced in unmarked, so `$x` is still the
  caller's `x`;
- a marked name that nothing bound — `push#3`, where the template meant the
  ordinary `push` — is simply not found, and the resolver looks again without
  the mark. That single fallback is what makes items unhygienic.

The one thing marking must not touch is a lowercase name that is *not* a
variable: a record label, a module member, a type variable, the name of a
declaration. Those are matched against something outside the expansion, so a
mark would break them. A label and a variable are the same token, though, and
only a tree tells them apart — so marks go on at substitution and come off
those slots once the expansion has been parsed.

Two consequences worth stating. A macro name is an item, so it is *stripped*
before the macro is looked up: that is what lets a template call itself, which
is how a recursive macro is written. And a name a template only mentions
resolves where the call is, not where the macro was defined — harmless while a
macro can only be used in its own module, and the thing [`$pkg`](#3-defining-one)
is for once [export](#8-exporting-and-incremental-builds) arrives.

## 7. Spans and diagnostics

A token that came from the call site keeps its own span, so an error in an
argument is reported where the argument was written. A token the template
produced is given the call's span plus an **expansion id**, and a side table
says which macro, which call and which definition it came from. An error in
expanded code is reported at the call, with a note naming the macro it came
from.

This is also what the editor needs: hover and go-to-definition keep working on
the arguments of a call, because those tokens never lost their spans.

## 8. Exporting, and incremental builds

A macro's rules are stored in its `CompiledPackage` as token trees, so a
dependent expands it without re-parsing the dependency's source.

Incremental builds need nothing new for declarative macros. A package's
fingerprint already covers its module sources and its dependencies'
fingerprints ([`incremental.rs`](../buildtools/meadow/src/incremental.rs)), so
changing a macro invalidates everything that used it.

## 9. Procedural macros

A procedural macro is an ordinary Meadow function, `TokenStream -> TokenStream`,
compiled to bytecode and **run on the VM during compilation**. There is no
dynamic library, no ABI between compiler and macro, and no bridge crate: the
compiler already contains a machine that runs Meadow.

Three things follow from that, and they are the reason to do it this way:

- **A proc macro is sandboxed by its type.** Its signature forbids `Fs`,
  `Process`, `Random` and `Time`, and the effect system checks it. A macro
  cannot read a file or the clock.
- **So it is deterministic, and its output is cacheable** on the tokens it was
  given. Impure procedural macros are a standing source of stale-cache bugs
  elsewhere; here the type system rules them out.
- **There is no host/target split.** Bytecode runs anywhere, so cross-compiling
  a program does not mean building its macros twice.

A proc macro lives in a package of its own, as it does in Rust, which with
workspaces is just another member. It is given a fuel budget, so a macro that
loops forever fails the build instead of hanging it.

`Std.Macro` provides the `TokenTree` type, spans, and `quote`, which is built
into the compiler rather than written in the library: it is what attaches
hygiene contexts to the tokens it produces.

`@derive(Show)` is then an attribute proc macro over a `data` declaration, using
the attribute syntax that already exists.

## 10. Macros that talk to each other

A token tree in and a token tree out is enough for `vec!` and `assertEq!`. It is
not enough for an **embedded language**, which is the interesting case:

```meadow
defineLanguage! {
  L0 = e : Var x | Lam x e | App e e
}

definePass! {
  removeLam : L0 -> L1
  | Lam x e -> ...
}
```

`definePass!` cannot be written against token trees alone, because it has to
*know what `L0` is*: which productions exist, so it can check the pass covers
them, generate the traversal for the cases the author did not write, and reject
output that is not an `L1`. `defineLanguage!` knows that, and by the time
`definePass!` runs, it is gone.

This is the thing Lisp has that a token-tree macro system does not, and it is
not really quotation — Meadow's `$` already quotes. It is that a macro can leave
something behind for a later macro to find. Racket calls it a compile-time
binding; nanopass is built on exactly this.

Three pieces make it work, and only the third is hard.

**A compile-time binding.** The namespace macros live in holds values, not just
macros — a `macro` is simply the case where the value is a function from tokens
to tokens.

```meadow
@compileTime
def l0 = Language.of [ ... ]
```

The right-hand side is ordinary Meadow, evaluated during compilation on the VM
that is already there for proc macros, under the same effect bound: no `Fs`, no
`Process`, no `Random`, no `Time`. So a compile-time value is deterministic, and
cacheable on what it was computed from, for the same reason a proc macro is.

**A way to read one.** A proc macro asks for a binding by name:

```meadow
lookup : Ident -> Macro (Maybe a)
```

`defineLanguage!` expands to the `data` declarations for the language *and* a
`@compileTime def` holding its grammar; `definePass!` looks that up and writes
the pass. Nothing is smuggled through a side channel: the grammar is a value,
with a type.

**Expansion has to become demand-driven, and that is the cost.** Today it is a
walk: read the `macro` declarations, then expand calls in order. Once one macro
can read what another produced, expanding a call may first require expanding
whatever defines what it asks for, so expansion becomes a fixpoint over a
dependency graph, with a cycle reported as a cycle rather than as a missing
name. Meadow's top level is already order-independent and already has the
machinery for this shape of problem ([`meadow-scc`](../compiler/meadow-scc)),
which is the reason to think it is affordable — but it is a real change, and it
is where this goes further than Rust, which has no such channel at all.

One more thing an embedded language wants: to match *structured* syntax rather
than counting brackets. So `Std.Macro` should expose the parser's own entry
points — the ones [section 5](#5-how-expansion-runs) added — as library
functions, letting a proc macro ask for a token tree as an expression or a
pattern and work on the tree.

## 11. What is left out

**Macros in type position.** `a -> Foo!{ … }` cannot be told from the effect row
`a -> Foo ! { Console | e }`. Allowing only `Foo!( … )` and `Foo![ … ]` would
work, but a rule of "macros in types, except with braces" is worse than not
having them yet.

**Macros that define macros**, beyond what falls out of a template containing a
`macro` declaration.

**`macro_rules` hygiene for items.** Matching Rust here is a deliberate limit,
not an oversight: full referential transparency is
[sets of scopes](https://users.cs.utah.edu/plt/publications/popl16-f.pdf), and it
is a much larger commitment than the rest of this document.

## 12. The order this is built in

Each step is useful on its own and none commits to the next.

1. **Done.** **Token trees**, a `$` token, and parser entry points for an
   expression, a pattern and a declaration list. No user-visible change: the
   test is that every `Std` module still parses to the same AST.
   ([`tt.rs`](../compiler/meadow-lexer/src/tt.rs))
2. **Done.** **Unexpanded call nodes and the expansion loop**, with built-in
   macros only — `line!`, `file!`, `stringify!`, `concat!`. This exercises
   expansion, re-parsing and spans without a matcher.
   ([`expand.rs`](../compiler/meadow-compiler/src/expand.rs))
3. **Done.** **`macro` declarations**: the matcher, `tt` / `ident` / `lit`,
   repetition, local hygiene, use within one module.
   ([`rules.rs`](../compiler/meadow-compiler/src/expand/rules.rs),
   [`hygiene.rs`](../compiler/meadow-compiler/src/expand/hygiene.rs))
4. **Done.** **`expr` / `pat` / `item` fragments**, with the follow rules
   above, checked where the macro is written.
   ([`rules.rs`](../compiler/meadow-compiler/src/expand/rules.rs))
5. **Export**: the macro namespace, visibility, storage in `CompiledPackage`,
   `$pkg`.
6. **Diagnostics and the editor**: expansion ids, notes naming the macro, and an
   editor that can still hover an argument.
7. **Procedural macros**: `Std.Macro`, `quote`, running on the VM under an
   effect bound and a fuel budget, cached on input — then `@derive`.
8. **Compile-time bindings** ([section 10](#10-macros-that-talk-to-each-other)):
   `@compileTime def`, `lookup`, and demand-driven expansion. This is what an
   embedded language needs, and it is last because it rests on every step
   before it.
