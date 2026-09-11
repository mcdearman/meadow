# The Meadow Tutorial

Meadow is a small, strictly-evaluated, statically-typed functional language. Types
are inferred — you never have to write one — and its two more unusual features are
**algebraic effects** (a `handle` block can intercept I/O, state, randomness or the
clock) and **row-polymorphic records**.

This tutorial walks through the language from the ground up. Every example in it
is a complete program that compiles and runs against the version of Meadow in this
repository.

**Contents**

1. [Getting started](#1-getting-started)
2. [Values, types and operators](#2-values-types-and-operators)
3. [Functions](#3-functions)
4. [Bindings and scope](#4-bindings-and-scope)
5. [Pattern matching](#5-pattern-matching)
6. [Your own types](#6-your-own-types)
7. [Sequences: arrays, vectors and lists](#7-sequences-arrays-vectors-and-lists)
8. [Modules and packages](#8-modules-and-packages)
9. [Effects](#9-effects)
10. [Testing](#10-testing)
11. [Tooling](#11-tooling)
12. [Reference](#12-reference)

---

## 1. Getting started

### Installing

Grab a build from the [releases page](https://github.com/mcdearman/meadow/releases/latest)
— `meadow-setup-x86_64.exe` on Windows, or:

```sh
curl -fsSL https://raw.githubusercontent.com/mcdearman/meadow/master/install.sh | sh
```

Either way you get a self-contained `meadow` in `~/.meadow/bin`. From a checkout,
`cargo install --path buildtools/meadow` does the same job.

### Your first program

Meadow source files end in `.mw`. A file is a list of **declarations** — you cannot
write a bare expression at the top level. The entry point is a declaration named
`main`:

```meadow
def main = println "Hello, Meadow!"
```

Save that as `hello.mw` and run it:

```sh
$ meadow run hello.mw
```

`meadow run` prints the type of every top-level binding, then the value `main`
evaluated to. Trimmed to the interesting part:

```
=== package hello ===
  main : Unit ! { io | e }
entry: main
Hello, Meadow!
=> ()
```

`main` has type `Unit ! { io | e }` — it produces nothing useful, and it performs
the `io` effect. More on that in [chapter 9](#9-effects).

### The REPL

Running `meadow` with no arguments starts a REPL:

```
> 1 + 2
_ : Int
= 3

> fun square x = x * x
square : Int -> Int

> square 7
_ : Int
= 49
```

An expression is labelled `_`; a declaration is labelled with its own name. Useful
commands:

| | |
|---|---|
| `:t <expr>` | show the type without evaluating |
| `:module` | list everything in scope |
| `:reset` | forget everything defined so far |
| `:q` | quit |

Tab completes names, and knows whether the cursor wants a value, a type or a `use`
path. For multi-line entries, an unfinished line (open bracket, dangling operator,
`| ...` arms) keeps reading; `Alt+Enter` or `Ctrl+J` forces a newline. **End a
multi-line entry with a blank line.**

### Comments

Only line comments, introduced by `--`. There is no block comment syntax.

```meadow
-- This is a comment.
def main = 1  -- so is this
```

---

## 2. Values, types and operators

### The primitive types

| Type | Literals | Notes |
|---|---|---|
| `Int` | `42`, `-7`, `0xff`, `0o17`, `0b1011` | 64-bit, wrapping |
| `BigInt` | *(no literal)* | arbitrary precision, via `toBigInt` |
| `Float` | `3.14`, `42.0` | 64-bit |
| `Bool` | `True`, `False` | constructors, capitalised |
| `String` | `"hi"`, `"tab\there"` | a sequence of **bytes**; escapes `\n \t \r \\ \" \0` |
| `Char` | `'a'`, `'é'`, `'\n'` | one Unicode **scalar**, not one byte |
| `Unit` | `()` | the empty tuple |

`Bool` values *print* as lowercase `true` / `false`, but you always write the
constructors `True` and `False`.

### Three numeric types, three sets of operators

This is the first thing that surprises people. Meadow has no numeric overloading,
so each numeric type gets its own operators, distinguished by a suffix:

| | `Int` | `Float` | `BigInt` |
|---|---|---|---|
| arithmetic | `+ - * / % ^` | `+. -. *. /.` | `+~ -~ *~ /~ %~ ^~` |
| comparison | `< > <= >=` | `<. >. <=. >=.` | `<~ >~ <=~ >=~` |

```meadow
def ints   = 1 + 2 * 3
def floats = 1.5 +. 2.5
def bigs   = toBigInt 2 ^~ toBigInt 64

def main = (ints, floats, bigs)
```

```
=> (7, 4.0, 18446744073709551616)
```

Equality is the exception: `==` and `!=` are **structural and work at any type** —
tuples, lists, constructors, records, anything.

```meadow
def main = ([1, 2] == [1, 2], Just 1 != None, "a" == "a")
```

```
=> (true, true, true)
```

Integer division truncates, and `%` follows the sign of the *dividend* — so
`(-7) % 3` is `-1`, not `2`. That trips people up when writing `even`/`odd`-style
tests by hand.

### Converting between them

```meadow
def main = (toFloat 3, floor 3.9, toBigInt 5, toInt (toBigInt 5))
```

```
=> (3.0, 3, 5, 5)
```

### Booleans and bit twiddling

`and` and `or` short-circuit, and `not` is an ordinary function. Bit operators
`<<`, `>>` (arithmetic) and `>>>` (unsigned) work on `Int`, alongside the
`Std.Bits` helpers.

```meadow
def main = (1 < 2 and 3 < 4, not True or True, 1 << 4, bitAnd 12 10)
```

```
=> (true, true, 16, 8)
```

### Tuples

```meadow
def point = (1, "north", True)

def main = (fst (1, 2), snd (1, 2), point)
```

```
=> (1, 2, (1, "north", true))
```

`fst` and `snd` only work on pairs. For anything wider, use pattern matching.

---

## 3. Functions

### Defining them

`fun` defines a function; `def` binds a value. There are no type annotations on
either — the types you saw above were inferred.

The two are not interchangeable: **`def` takes no parameters.** It binds a
*pattern* to the value of an expression, so `def square x = x * x` is a parse
error. Write `fun square x = x * x`, or `def square = \x -> x * x`.

```meadow
fun double n = n * 2

fun add a b = a + b

def four = double 2

def main = (double 21, add 1 2, four)
```

```
=> (42, 3, 4)
```

Application is by juxtaposition — `add 1 2`, not `add(1, 2)` — so parentheses are
only for grouping. `double 2 + 1` means `(double 2) + 1`.

### Currying and partial application

Every function of several arguments is really a chain of one-argument functions, so
you can apply one at a time:

```meadow
fun add a b = a + b

def increment = add 1

def main = (increment 41, map (add 10) [1, 2, 3])
```

```
=> (42, [11, 12, 13])
```

### Lambdas

`\x -> body`, with multiple parameters allowed:

```meadow
def main = (
  (\x -> x * 2) 21,
  (\x y -> x + y) 1 2,
  filter (\n -> n % 2 == 0) [1, 2, 3, 4]
)
```

```
=> (42, 3, [2, 4])
```

### Pipelines

`|>` sends a value into a function and `<|` does the reverse. Both make long
chains read left-to-right instead of inside-out.

```meadow
def main =
  [1, 2, 3, 4, 5]
  |> filter (\n -> n % 2 == 1)
  |> map (\n -> n * n)
  |> sum
```

```
=> 35
```

`|>` binds looser than every arithmetic operator, so `x |> f |> g` is `g (f x)` and
`1 + 2 |> double` is `double (1 + 2)`.

### Composing

`compose f g` is "`g` then `f`", and `flip` swaps the first two arguments:

```meadow
def addThenDouble = compose (\n -> n * 2) (\n -> n + 1)

def main = (addThenDouble 5, flip (\a b -> a - b) 3 10, id 7, const 1 "ignored")
```

```
=> (12, 7, 7, 1)
```

### Order does not matter

Top-level declarations may appear in any order. The compiler sorts them by
dependency before type-checking and evaluating, and handles mutual recursion:

```meadow
def main = isEven 10

fun isEven n = if n == 0 then True else isOdd (n - 1)

fun isOdd n = if n == 0 then False else isEven (n - 1)
```

```
=> true
```

---

## 4. Bindings and scope

### `let ... in`

`let` binds a name for the rest of an expression. It is an expression itself, so it
has a value and can be chained:

```meadow
fun hypotenuseSquared a b =
  let aa = a * a in
  let bb = b * b in
  aa + bb

def main = hypotenuseSquared 3 4
```

```
=> 25
```

### `let rec`

A plain `let` cannot refer to itself. For a local recursive helper, use `let rec`:

```meadow
fun countdown n =
  let rec go i acc =
    if i == 0 then acc else go (i - 1) (acc + i)
  in go n 0

def main = countdown 10
```

```
=> 55
```

### `if ... then ... else`

`if` is an expression, both branches must have the same type, and **the `else` is
mandatory** — there is no value for a missing branch to produce.

```meadow
fun classify n =
  if n < 0 then "negative"
  else if n == 0 then "zero"
  else "positive"

def main = (classify (0 - 3), classify 0, classify 3)
```

```
=> ("negative", "zero", "positive")
```

### Shadowing

A `let` may reuse a name that is already in scope, and its right-hand side sees
the *old* binding:

```meadow
fun normalise x =
  let x = abs x in
  let x = min x 100 in
  x

def main = (normalise (0 - 250), let a = 1 in let a = a + 1 in a)
```

```
=> (100, 2)
```

A binding **with parameters** is the exception, and deliberately so: it is
recursive, so its own name refers to itself rather than to anything outer. That is
what makes a local helper work.

```meadow
fun sumTo n =
  let go i acc = if i == 0 then acc else go (i - 1) (acc + i) in
  go n 0

def main = sumTo 10
```

```
=> 55
```

`let rec` is accepted as a spelling of the same thing, for emphasis.

---

## 5. Pattern matching

`match` scrutinises a value against a series of patterns, first match wins.

```meadow
fun describe n =
  match n with
  | 0 -> "zero"
  | 1 -> "one"
  | _ -> "many"

def main = (describe 0, describe 1, describe 9)
```

```
=> ("zero", "one", "many")
```

### What you can match on

Literals, `_`, variables, tuples, constructors, records, and the sequence forms:

```meadow
data Shape = Circle Int | Rect Int Int

fun area s =
  match s with
  | Circle r -> 3 * r * r
  | Rect w h -> w * h

fun originDistance p =
  match p with
  | (0, 0) -> "at the origin"
  | (0, _) -> "on the y axis"
  | (_, 0) -> "on the x axis"
  | (_, _) -> "somewhere else"

def main = (area (Rect 3 4), area (Circle 2), originDistance (0, 5))
```

```
=> (12, 12, "on the y axis")
```

### Patterns in parameters

A function parameter is itself a pattern, so you can destructure on the way in.
Parameter patterns must be **irrefutable** — they must always match — so tuples and
records are fine, but constructors of a multi-case type are not.

```meadow
fun addPair (a, b) = a + b

def main = addPair (1, 2)
```

```
=> 3
```

### Exhaustiveness

By default (`--debug`) a `match` may be incomplete, so half-written code still
runs. `--release` requires every case to be covered and tells you which is missing:

```sh
$ meadow run --release incomplete.mw
incomplete: non-exhaustive patterns: `G` is not matched
```

### What is *not* supported

Two things you may reach for out of habit and will not find:

- **No guards.** `| n if n > 3 -> ...` does not parse. Use nested `if` in the arm.
- **No `as`-patterns.** `| x :: rest as whole -> ...` does not parse. (`as` is only
  used for `use ... as`.) Rebuild the value in the arm instead.

---

## 6. Your own types

### `data` — sum types

A `data` declaration lists alternatives, each with zero or more fields. Type
parameters are lowercase names after the type's own name.

```meadow
data Colour = Red | Green | Blue

data Shrub a = Tip | Fork (Shrub a) a (Shrub a)

fun size t =
  match t with
  | Tip -> 0
  | Fork l x r -> size l + 1 + size r

def sample = Fork (Fork Tip 1 Tip) 2 Tip

def main = (size sample, Red == Red, Red == Blue)
```

```
=> (2, true, false)
```

Constructors live in one global namespace, so `Red` is in scope everywhere the
type is, without a qualifier. **Type names share that namespace too** — which is
why the tree above is a `Shrub`: `Tree` is already taken by
`Std.Collections.Tree`, and a clash is an error even if you never `use` it.

### The two you get for free

`Maybe` and `Result` come from the standard library and are always in scope:

```meadow
use Std.String as S

fun safeDiv a b = if b == 0 then None else Just (a / b)

fun parseAge s =
  match S.toInt s with
  | None -> Err "not a number"
  | Just n -> if n < 0 then Err "negative" else Ok n

def main = (safeDiv 10 2, safeDiv 10 0, parseAge "30", parseAge "x")
```

```
=> (Just(5), None, Ok(30), Err("not a number"))
```

There is also `Either a b` (`Left` / `Right`) in `Std.Either`, for when neither
side means "failure".

Note the `use Std.String as S` there: the prelude's bare `toInt` is the
`BigInt -> Int` primitive, not string parsing. When a name feels like it should
exist, check whether the prelude already means something else by it.

### `record` — named fields

There are two kinds of record, and the difference matters more than you would
expect.

An **anonymous** record needs no declaration. Field selection on it is
*row-polymorphic*: `getX` below reads "any record with at least an `x`", so it
accepts records of different shapes.

```meadow
def anon = { x = 1, y = 2, label = "extra" }

fun getX r = r.x

def main = (anon, getX anon, getX { x = 99 }, getX { x = 5, other = True })
```

```
=> ({ x = 1, y = 2, label = "extra" }, 1, 99, 5)
```

A **nominal** record is declared with `record`, constructed by name, and selected
from with the same `.`:

```meadow
record Point = { x : Int, y : Int }

def origin = Point { x = 0, y = 0 }

def main = (origin, origin.x, origin.y)
```

```
=> (Point(0, 0), 0, 0)
```

> **The catch.** Field selection always infers the *structural* row type
> `{ x : Int | a }`, and that does not unify with a nominal type. So a function
> that selects a field cannot be applied to a nominal record:
>
> ```
> fun magnitudeSquared p = p.x * p.x + p.y * p.y
> magnitudeSquared (Point { x = 3, y = 4 })
>   -- type mismatch: `{ x : Int | a }` vs `Point`
> ```
>
> Write the function by pattern-matching instead — which works, and reads fine:

```meadow
record Point = { x : Int, y : Int }

fun magnitudeSquared p =
  match p with
  | Point { x = a, y = b } -> a * a + b * b

def main = magnitudeSquared (Point { x = 3, y = 4 })
```

```
=> 25
```

In short: use anonymous records when you want lightweight structural data and
generic accessors; use `record` when you want a named type, and reach into it by
matching.

> **Not supported:** there is no record *update* syntax. `{ p | x = 9 }` does not
> parse; rebuild the record explicitly.

---

## 7. Sequences: arrays, vectors and lists

Meadow has three sequence types, and one rule for telling them apart in source:
**a `;` means the linked `List`; brackets without one mean the default `Vector`.**

| | `Array` | `Vector` | `List` |
|---|---|---|---|
| what it is | flat contiguous buffer | RRB tree | cons list |
| type | `#[a]` | `[a]` | `[a;]` |
| empty | `#[]` | `[]` | `[;]` |
| one element | `#[x]` | `[x]` | `[x;]` |
| several | `#[x, y]` | `[x, y]` | `[x; y]` |
| indexing | O(1) | O(log n) | O(n) |
| use it for | primitives, interop | **most things** | recursion, pattern matching |

`Vector` is the default: the bare `map`, `filter`, `foldl`, `len`, `range` … in the
prelude are `Vector`'s. `List`'s equivalents need an explicit `use` — the prelude
does not activate a `List.` qualifier for you.

```meadow
use Std.Collections.List as List

def vec  = [1, 2, 3]
def list = [1; 2; 3]
def arr  = #[1, 2, 3]

def main = (len vec, List.length list, arrayLen arr)
```

```
=> (3, 3, 3)
```

### Ranges

`[a..b]` builds an inclusive `Vector` range. Note that the prelude's `range`
function is **half-open**, which is a genuine trap:

```meadow
def main = ([1..5], range 1 5)
```

```
=> ([1, 2, 3, 4, 5], [1, 2, 3, 4])
```

### Working with vectors

```meadow
def main =
  [1..10]
  |> filter (\n -> n % 2 == 0)
  |> map (\n -> n * n)
  |> foldl (\acc n -> acc + n) 0
```

```
=> 220
```

Other prelude staples: `head`, `last`, `get`, `getOr`, `take`, `drop`, `slice`,
`append`, `reverse`, `zip`, `zipWith`, `unzip`, `partition`, `all`, `any`, `find`,
`elem`, `sum`, `product`, `maximum`, `minimum`, `concat`, `concatMap`, `replicate`,
`takeWhile`, `dropWhile`.

`get` is bounds-checked and returns a `Maybe`:

```meadow
def main = (get [1, 2, 3] 1, get [1, 2, 3] 99, getOr 0 [1, 2, 3] 99)
```

```
=> (Just(2), None, 0)
```

### Working with lists

Lists are what you pattern-match on. `::` is `Cons`, right-associative:

```meadow
use Std.Collections.List as List

fun total xs =
  match xs with
  | [;] -> 0
  | x :: rest -> x + total rest

def main = (total [1; 2; 3], 1 :: 2 :: [;], List.reverse [1; 2; 3])
```

```
=> (6, [1; 2], [3; 2; 1])
```

`Vector.toList` and `Vector.fromList` convert between the two.

### Matching an empty vector

`[]` is a pattern too — it matches the empty `Vector`. A *non-empty* vector has no
structural pattern (it is a balanced tree, not a cons cell), so match on `[]` and
fall through, or use `len`:

```meadow
fun describe v =
  match v with
  | [] -> 0
  | _ -> len v

def main = (describe [], describe [1, 2, 3])
```

```
=> (0, 3)
```

### Strings are bytes; characters are scalars

`Std.String` works in **bytes**, so `byteLength "é"` is 2. A `Char` is one
Unicode **scalar**, so they are different things and the bridge is explicit:

```meadow
use Std.String as S

def main =
  ( S.byteLength "héllo"
  , S.charLength "héllo"
  , S.chars "hi"
  , S.fromChars ['h', 'i']
  , S.charAt "héllo" 1
  )
```

```
=> (6, 5, ['h', 'i'], "hi", Just('é'))
```

The rest of `Std.String` is byte-oriented and ASCII-minded:

```meadow
use Std.String as S

def main = (S.concat "foo" "bar", S.split "," "a,b,c", S.toUpper "hi", S.toInt "42")
```

```
=> ("foobar", ["a", "b", "c"], "HI", Just(42))
```

`Std.Char` classifies and converts single characters. Its predicates are
**ASCII-only** by design — doing it properly means shipping the Unicode
character database — so a non-ASCII character answers `False` rather than being
guessed at:

```meadow
use Std.Char as C

def main =
  ( C.isDigit '7'
  , C.toUpper 'a'
  , C.code 'A'
  , C.fromCode 97
  , C.digitToInt '7'
  , C.isAlpha 'é'
  )
```

```
=> (true, 'A', 65, 'a', Just(7), false)
```

Characters match like any other literal:

```meadow
use Std.Char as C

fun kind c =
  match c with
  | ' ' -> "space"
  | '\n' -> "newline"
  | _ -> if C.isDigit c then "digit" else "other"

def main = (kind ' ', kind '\n', kind '4', kind 'x')
```

```
=> ("space", "newline", "digit", "other")
```

---

## 8. Modules and packages

### A package

A package is a directory with a `meadow.toml` and a `src/`:

```
myapp/
  meadow.toml
  src/
    main.mw
    Math.mw
```

```toml
[package]
name = "myapp"
version = "0.1.0"
```

`meadow run myapp` builds it and evaluates `main`. A single `.mw` file also counts
as a package, which is why `meadow run hello.mw` works.

### Modules inside one package are separate namespaces

The package is the **compilation unit** — every module of it is resolved,
inferred and lowered together, so two modules may refer to each other and even
be mutually recursive. But each module is its own **namespace**, as in Rust: a
sibling's names arrive through a `use`, and never for free.

```meadow
-- src/Math.mw
@pub fun double n = n * 2
```

```meadow
-- src/main.mw  (a second file in the same package)
use myapp.Math (double)       -- the package name, then the module

@pub def main = double 21
```

The path starts with the package's own name, which is what `meadow.toml` says —
`use myapp.Math`. The plain `use Math (double)` works too and means the same
thing; the longer form is the one to write when it is not obvious that `Math` is
next door rather than a dependency.

Every `use` form works on a sibling:

```meadow
use myapp.Math               -- everything it exports, unqualified
use myapp.Math as M          -- M.double, and nothing unqualified
use myapp.Math (double)      -- just `double`
```

Because the namespaces are separate, two modules of one package may both define
`map`, and a module that wants both can take one of them under an alias.

Naming a **type** in a `use` brings its constructors with it — that is how
`Expr.Int` becomes writable as `Int`:

```meadow
-- src/Syntax.mw
@pub data Expr = Int Int | Add Expr Expr
```

```meadow
-- src/Eval.mw
use myapp.Syntax (Expr)      -- the type, and `Int` / `Add` with it

@pub fun eval e = match e with
  | Int n -> n
  | Add a b -> eval a + eval b
```

### `@pub` and a gotcha

`@pub` marks a declaration as exported. The rule to remember:

> As soon as **any** declaration in the package is `@pub`, only `@pub` declarations
> are exported — including `main`.

So the moment you add `@pub` anywhere, `main` needs it too, or the linker will not
find an entry point and you will see `entry: (none)` and a result of `()`.

### `use`

`use` brings a dependency's module into scope. Three forms, and the thing to
remember is that **a qualifier comes only from `as`**:

| Form | Effect |
|---|---|
| `use M` | every exported name, unqualified |
| `use M as C` | `C.name` only — nothing unqualified |
| `use M (a, b)` | just `a` and `b`, unqualified |
| `use M as C (a, b)` | both: `C.name`, plus `a` and `b` unqualified |

```meadow
use Std.Collections.List              -- everything, unqualified
use Std.Collections.Vector as V       -- V.len, and nothing else
use Std.String (concat)               -- just `concat`

def main = (length [1; 2; 3], V.len [1, 2], concat "a" "b")
```

```
=> (3, 2, "ab")
```

A bare `use` shadows anything of the same name already in scope, including the
prelude — which is exactly how you switch a file from `Vector` to `List`:
`length` above is `List`'s, not the prelude's.

### Depending on another package

```toml
[package]
name = "app"
version = "0.1.0"

[dependencies]
util = { path = "../util" }
```

Then `use util` for all of it, `use util (double)` for one name, or
`use util as U` to keep it behind a qualifier. Only `@pub` names cross the
boundary.

---

## 9. Effects

This is Meadow's most distinctive feature. An **effect** is a set of operations a
computation may perform; a **handler** decides what they mean. The type system
tracks which effects an expression can perform, in the `! { ... }` row after the
arrow.

The payoff is worth stating up front, because it is the reason to bother: code
that reads and writes the world is written *once*, and a handler decides whether
"the world" is the real filesystem and the real clock, or a list of strings and
the number 500. Nothing has to be written twice, and nothing has to be injected.

### Declaring and performing

```meadow
effect Log { log : String -> Unit }

fun greet name =
  let ignored = log name in
  "done"

def main =
  handle greet "world" with {
    log message k -> k (),
    return x -> x
  }
```

```
=> "done"
```

`effect` declares the operations. Calling `log` is just a function call — its type
carries `! { Log | e }`, meaning "performs `Log`, and possibly other effects `e`".

### Handlers

`handle expr with { op param k -> ..., return x -> ... }`:

- Each **operation clause** binds the operation's argument (`param`) and the
  continuation `k` — the rest of the computation, from the point `log` was called.
- Calling `k v` resumes with `v`. **Not** calling it abandons the computation,
  which is how early exit works.
- The `return` clause transforms the final value.

Handlers are *deep* (they cover nested calls too) and *one-shot* (resume at most
once).

Because the handler decides, the same code can be run for real or faked:

```meadow
effect Log { log : String -> Unit }

fun work u =
  let a = log "step one" in
  let b = log "step two" in
  42

def collected =
  handle work () with {
    log m k -> m :: k (),
    return x -> [;]
  }

def counted =
  handle work () with {
    log m k -> 1 + k (),
    return x -> 0
  }

def main = (collected, counted)
```

```
=> (["step one"; "step two"], 2)
```

One handler collects the messages, the other counts them, and `work` knows about
neither.

### Not resuming: early exit

A clause that never calls `k` throws the rest of the computation away. That is the
whole mechanism behind `Exn`, `Stream.take`, and any "stop now" you write yourself:

```meadow
use Std.String as S

effect Abort { abort : String -> Unit }

fun search xs =
  let _ = forEach (\x -> if x < 0 then abort (show x) else ()) xs in
  "all non-negative"

def main =
  ( handle search [1, 2, 3] with { abort m k -> S.concat "found " m, return x -> x }
  , handle search [1, -2, 3] with { abort m k -> S.concat "found " m, return x -> x } )
```

```
=> ("all non-negative", "found -2")
```

Note there is no `break` in the language and none is needed: `forEach` does not
know it can be interrupted, and is interrupted anyway.

### Which things are effects, and which are not

A fair question is why `println` is not an effect when `readLine` is. Ask the
compiler:

```meadow
def main = println "x"
```

```sh
$ meadow build hello.mw
  main : Unit
```

No `!` row at all — `print` and `println` are plain primitives. Input is an effect
and output is not, and the asymmetry is deliberate rather than an oversight:

- **Reading** is the thing a test can never be allowed to do for real. A suite
  that blocks waiting for someone to type is a suite that hangs. So `readLine` is
  an operation, and `withInput` supplies a script.
- **Writing** is harmless to let escape. A test that prints is noisy, not broken.
  Making it an effect would put a `! { Console | e }` on the type of nearly every
  function anyone writes, for very little.

So there is no `IO` effect in Meadow, and nothing to import to print. If you do
want to capture output, wrap it in an effect of your own — the `Log` example above
is exactly that, in nine lines.

### The standard library's effects

`Std` ships nine, plus `Mut` for mutable cells. Each pairs a real implementation
with a handler that fakes it — which is the point, since these are exactly the
things that are otherwise hard to test.

| Module | Operations | Unhandled | Handled with |
|---|---|---|---|
| `Std.Ref` (`Mut`) | via `newRef` / `getRef` / `setRef` | real cells | — |
| `Std.State` | `get`, `put` | — | `runState`, `evalState`, `execState` |
| `Std.Console` | `readLine` | real stdin | `withInput` |
| `Std.Exn` | `throw` | aborts | `toResult`, `catch`, `withDefault`, `toMaybe` |
| `Std.Yield` | `yield` | — | everything in `Std.Stream` |
| `Std.Random` | `nextInt`, `intBetween`, … | real entropy | `withSeed`, `withSeedFrom` |
| `Std.Time` | `now`, `monotonic`, `sleep` | real clock | `withClock`, `withTickingClock` |
| `Std.Fs` | `readToString`, `writeString`, … | real filesystem | any `handle` |
| `Std.Process` | `spawn`, `status`, `argv`, … | real subprocesses | any `handle` |
| `Std.Test` | `fail` | fails the test | `didFail` |

The VM does **not** discharge an unhandled `Fs`, `Process`, `Random` or `Time`
operation — those reach the real world only on the CEK machine. In practice that
means a `meadow test` run cannot touch your filesystem by accident.

#### Mut — the one mutable cell

`newRef`, `getRef` and `setRef` are primitives and always in scope. Every one of
them carries the `Mut` effect, so a function that mutates says so in its type and
a pure one still reads as pure:

```meadow
use Std.Ref (modify, repeatN)

fun countUp n =
  let r = newRef 0 in
  let ignored = repeatN n (\u -> modify r (\x -> x + 1)) in
  getRef r

def main = countUp 5
```

```
=> 5
```

`countUp : Int -> Int ! { Mut | e }`. There is no handler for `Mut` — it is an
effect so that it shows up in types, not so that it can be reinterpreted.

That typing also keeps mutation *sound*. Meadow generalizes a binding only when
its right-hand side is pure — an effect-based value restriction rather than ML's
syntactic one — so `def r = newRef []` is never given `forall a. Ref [a]`, and the
classic trick of storing at one type and reading at another does not typecheck.

One surprise worth knowing: a `Ref` has identity. `==` on two of them compares
cells, not contents, so `newRef 1 == newRef 1` is `False`. Nothing else in the
language behaves that way.

#### State — a value threaded for you

Where `Mut` is a real cell, `State` is a value passed along invisibly and handed
back at the end. Nothing is allocated and nothing is shared.

```meadow
use Std.State (get, put, modify, runState, evalState, execState)

fun tick u =
  let n = get () in
  let ignored = put (n + 1) in
  n

def main = runState 0 (\u -> let a = tick () in let b = tick () in get ())
```

```
=> (2, 2)
```

`runState` returns `(value, finalState)`; `evalState` keeps the value, `execState`
the state. `modify f` is `put (f (get ()))` and `gets f` is `f (get ())`.

Reach for `State` when the state is part of what a computation *means* and you
want it out of the signatures; reach for `Mut` when you want a cell with identity,
or speed.

#### Console — input, and why it makes a program testable

`readLine` is the only operation; `prompt` is `print` then `readLine`. `None`
means end of input — a closed pipe, or Ctrl-D — which is not an error, so a loop
ends by matching it rather than by catching anything.

```meadow
use Std.Console (prompt, withInput, readLine)
use Std.String as S

fun greet u =
  match prompt "name: " with
  | Just name -> S.concat "hello, " name
  | None -> "nobody there"

def main = (withInput ["ada";] greet, withInput [;] greet)
```

```
name: name: => ("hello, ada", "nobody there")
```

The two `name: ` are real: `prompt` still *prints*, because printing is not the
part a handler replaced. Only the reading was faked.

`withInput` feeds a list of lines and answers `None` once they run out, so the
same function covers both the interactive and the exhausted case.

The example in `examples/rock-paper-scissors` is the whole point of this in
practice. It is an interactive terminal game — it prompts, it loops, it keeps a
tally — and its test plays a *complete game* with no terminal and no entropy
anywhere near it:

```meadow
@test fun playsAFullRound () =
  let played =
    R.withSeed 7 (\u -> withInput ["rock"; "nonsense"; "paper"; "quit"] game) in
    assertEq played () "a full game plays through to the summary"
```

That runs `game` itself, not a copy of it — the same function `def main` calls. The
`"nonsense"` line exercises the re-prompt path, and `"quit"` exercises the exit.
Two handlers nest: `withSeed` fixes what the machine plays, `withInput` fixes what
you type, and between them the transcript is fully determined.

Handlers compose by nesting, and the inner one sees the outer one's effects:

```meadow
use Std.Console (prompt, withInput)
use Std.Random as R
use Std.String as S

fun guess u =
  let secret = R.between 1 11 in
  match prompt "pick 1-10: " with
  | None -> "no answer"
  | Just typed -> if S.trim typed == show secret then "right" else "wrong"

def main = R.withSeed 1 (\u -> withInput ["3";] guess)
```

```
pick 1-10: => "wrong"
```

#### Exn — failure that unwinds

`raise` abandons the computation and travels to the nearest handler, so nothing in
between has to mention failure:

```meadow
use Std.Exn (raise, toResult, withDefault, toMaybe, ensure)

fun half n = if n % 2 == 0 then n / 2 else raise "odd"

def main =
  ( toResult (\u -> 1 + half 8)
  , toResult (\u -> 1 + half 7)
  , withDefault 0 (\u -> half 7)
  , toMaybe (\u -> half 7) )
```

```
=> (Ok(5), Err("odd"), 0, None)
```

Also: `catch recover act` runs `recover` on the error, `threw act` answers a
`Bool`, and `ensure cond e` / `refute cond e` throw when a condition fails.
`ofResult` and `ofMaybe` go the other way, turning a value back into a throw.

Use `Result` when the caller should inspect the failure; use `Exn` when it should
travel a long way untouched. `toResult` converts at the boundary.

#### Yield and Stream — generators

`Std.Yield` is one operation, `yield : a -> Unit`. A producer performs it; a
consumer decides what it means. `Std.Stream` is the consumers.

```meadow
use Std.Yield (yield)
use Std.Stream as St

fun countdown n =
  if n <= 0 then ()
  else let _ = yield n in countdown (n - 1)

def main = (St.toList (\u -> countdown 5), St.take 2 (\u -> countdown 100))
```

```
=> ([5; 4; 3; 2; 1], [100; 99])
```

`take` is the interesting one: it simply stops resuming, which unwinds the
producer where it stands. So a producer is lazy without being written lazily —
`countdown 100` really does stop after two, and this stops after three rather than
building a million-element anything:

```meadow
use Std.Stream as St

def main =
  ( St.take 3 (\u -> St.range 0 1000000)
  , St.toList (\u -> St.map (\x -> x * x) (\v -> St.range 1 5))
  , St.sum (\u -> St.range 1 101) )
```

```
=> ([0; 1; 2], [1; 4; 9; 16], 5050)
```

Consumers: `toList`, `toVec`, `forEach`, `fold`, `count`, `sum`, `take`,
`takeWhile`, `first`, `any`, `all`, `find`. Transformers, which are producers that
consume: `map`, `filter`. Producers: `ofList`, `ofVec`, `range`, `repeat`,
`iterate`. Every collection has `toStream`.

#### Random — deterministic when you want it

```meadow
use Std.Random as R

def rolls = R.withSeed 42 (\u -> [R.between 1 7; R.between 1 7; R.between 1 7])

def main = (rolls, rolls == R.withSeed 42 (\u -> [R.between 1 7; R.between 1 7; R.between 1 7]))
```

```
=> ([1; 4; 4], true)
```

Same seed, same sequence — which is what makes a shuffle or a simulation testable.
Operations: `nextInt`, `intBetween` (and the friendlier `between lo hi`),
`nextFloat`, `nextSeed`, plus `bool`, `choose` and `shuffle` on top. Unhandled,
`between` uses real entropy.

#### Time — a clock you control

```meadow
use Std.Time as T

def frozen = T.withClock 500 (\u -> (T.now (), T.now ()))

def ticking = T.withTickingClock 1000 10 (\u -> (T.now (), T.now (), T.now ()))

def main = (frozen, ticking)
```

```
=> ((500, 500), (1000, 1010, 1020))
```

`withClock` freezes time; `withTickingClock start step` advances it by `step` on
every read, which is how you test a timeout without waiting for one. `sleep`
returns immediately under either. There are helpers for units (`seconds`,
`minutes`, `hours`, `days`, `toSeconds`) and `timed` / `elapsed` for measuring a
computation with the monotonic clock.

#### Fs — the filesystem, or a pretend one

Every operation answers a `Result`, so failure is a value rather than a throw:

```meadow
use Std.Fs (readToString, writeString, exists, removeFile)

def main =
  handle
    match readToString "config.txt" with
    | Ok text -> text
    | Err e -> e
  with {
    readToString path k -> k (Ok "colour = blue"),
    return x -> x
  }
```

```
=> "colour = blue"
```

No file was touched. `Fs` has no bundled fake — a handler is a few lines and the
one you want depends on the test, so write it inline as above. Operations cover
reading (`readToString`, `readBytes`, `readDir`, `metadata`), writing
(`writeString`, `appendString`, `copy`, `rename`), directories (`createDir`,
`createDirAll`, `removeDir`, `removeDirAll`) and predicates (`exists`, `isFile`,
`isDir`). Convenience: `readToStringOr`, `tryReadDir`, `existsAll`.

#### Process — subprocesses, argv and the environment

A command is built up rather than passed as one lump: `command "git"` then
`withArg`, `withArgs`, `withCwd`, `withEnv`. `run` and `runInherit` are the short
forms.

```meadow
use Std.Process as P

fun versionOf tool =
  match P.run tool ["--version";] with
  | Ok out -> P.outputStdout out
  | Err e -> e

def main =
  handle versionOf "meadow" with {
    spawn cmd k -> k (Ok (0, "meadow 0.2.0", "")),
    return x -> x
  }
```

```
=> "meadow 0.2.0"
```

`spawn` captures stdout and stderr and gives you `(status, out, err)` — pick them
apart with `outputStatus` / `outputStdout` / `outputStderr`, or ask `succeeded`.
`status` inherits the parent's stdio and yields only the exit code. Also here:
`exit`, `currentPid`, `argv`, `getEnv`, `setEnv`, `removeEnv`.

#### Test — an assertion is an effect too

`Std.Test`'s `fail` is an ordinary operation, which is why a failing assertion
stops the test it is in and nothing else. `didFail` handles it, so you can assert
that something *should* fail:

```meadow
use Std.Test (assertEq, didFail)

def main = (didFail (\u -> assertEq 1 1 "same"), didFail (\u -> assertEq 1 2 "different"))
```

```
=> (false, true)
```

### Writing a handler for your own effect

The shape is always the same. Declare the operations, write the code that performs
them without thinking about who answers, then write one handler per interpretation:

- **Resume with a value** — `op x k -> k answer`. The normal case; the
  computation carries on.
- **Resume with something computed from the rest** — `op x k -> f (k ())`. This is
  how `collected` and `counted` above accumulate: the clause gets to see the result
  of everything that follows.
- **Do not resume** — `op x k -> something`. The rest is abandoned; `Exn`, `take`
  and `Abort` all work this way.
- **Return a function** — `op x k -> \state -> ...`, and make the `return` clause
  `\state -> ...` too, then apply the whole `handle` to an initial value. This is
  how `State` threads a value and how `withInput` threads the remaining lines;
  look at `Std.State.runState` for the smallest complete example.

## 10. Testing

Mark a function `@test` and `meadow test` runs it. A test takes one argument — the
runner calls it with `()`.

```meadow
use Std.Test (assertEq, assertTrue)

fun double n = n * 2

@pub def main = double 21

@test fun doubling u = assertEq (double 21) 42 "double 21"

@test fun listsCompare u = assertEq [1; 2] [1; 2] "list equality"

@test fun somethingTrue u = assertTrue (double 2 == 4) "double 2"
```

```sh
$ meadow test .
running 3 tests
test doubling ... ok
test listsCompare ... ok
test somethingTrue ... ok

test result: ok. 3 passed; 0 failed
```

A failure names both values, because `==` and `show` are structural and work at any
type:

```
---- doubling ----
double 21: expected 43, got 42
```

The assertions: `assert`, `assertEq`, `assertNeq`, `assertTrue`, `assertFalse`,
`refuteThat`, `failWith`, plus `didFail` / `assertFails` for checking that
something *does* fail.

A test fails by performing `Std.Test`'s effect rather than returning a value, so an
assertion five calls deep still stops the test and still names itself.

`meadow test <path> <filter>` runs only tests whose name contains `<filter>`, and
`meadow test --std` runs the standard library's own 154 tests.

---

## 11. Tooling

### Commands

| | |
|---|---|
| `meadow` | REPL |
| `meadow run <path>` | build and evaluate `main` |
| `meadow build <path>` | type-check and link |
| `meadow build --annotations <path>` | …and dump every node's type |
| `meadow test [<path>] [<filter>]` | run `@test` functions |
| `meadow fmt <path>` | re-indent in place |
| `meadow fmt --check <path>` | report, exit 1 if anything differs |
| `meadow lsp` | run the language server (editors start this) |
| `meadow update` | replace the binary with the latest release |

`--release` and `--debug` select a profile. Today the only difference is that
release requires every `match` to be exhaustive.

### Editors

`meadow lsp` speaks the Language Server Protocol over stdin and stdout, so any
LSP client can use it. It gives you diagnostics as you type, hover showing the
inferred type and the `--` comment above the definition, go-to-definition, inlay
hints for parameters and `let` bindings, and semantic tokens.

The VS Code extension lives in `editors/vscode`; `editors/vscode/build.sh`
produces a `.vsix`, and every release attaches one. It runs `meadow lsp`, and
falls back to plain syntax highlighting if the executable is not on your `PATH`.

### The formatter

`meadow fmt` is an *indenter*, not a pretty-printer: it fixes leading and trailing
whitespace, tabs and blank-line runs, and never moves a token to another line.
Comments therefore survive exactly as written, and since the grammar is not
layout-sensitive, formatting cannot change what a program means.

Indentation follows structure — brackets, `match` arms lining up with their
`match`, `then`/`else` with their `if`, `in` with its `let`, two units for an arm
body on its own line. A line that merely *continues* the expression above it keeps
the column you chose, so deliberate alignment survives.

---

## 12. Reference

### Operators, loosest to tightest

| Operators | |
|---|---|
| `<\|` | apply, right-associative |
| `\|>` | pipe |
| `or` | short-circuit |
| `and` | short-circuit |
| `==` `!=` `<` `>` `<=` `>=` (and `.` / `~` variants) | comparison |
| `::` | cons, right-associative |
| `+` `-` (and `+.` `-.` `+~` `-~`), `<<` `>>` `>>>` | |
| `*` `/` `%` (and `*.` `/.` `*~` `/~` `%~`) | |
| `^` `^~` | power, right-associative |
| `-` (prefix) | negation |
| *juxtaposition* | function application, tightest |

### Always in scope

Without any `use`, from the prelude:

- **Bool** `not`
- **Ordering** `compare` `isLess` `isEqual` `isGreater`
- **Function** `id` `const` `flip` `compose` `apply` `twice`
- **Tuple** `fst` `snd` `pair` `swap` `mapFst` `mapSnd`
- **Int** `min` `max` `abs` `signum` `clamp` `succ` `pred` `even` `odd` `gcd` `lcm`
- **Bits** `lowMask` `lowBits` `bit` `testBit` `setBit` `clearBit` `flipBit`
  `byteOf` `fromBytes4` `countLeadingZeros` `countTrailingZeros` `rotateLeft`
  `rotateRight`
- **Bytes** `bytesLength` `bytesGet` `bytesSet` `bytesPush` `bytesSlice`
  `bytesConcat`, the `U16`/`U32` accessors, …
- **Fs** `readToString` `writeString` `exists` `readDir` …
- **Process** `command` `run` `exit` `argv` `getEnv` …
- **Vector** `empty` `singleton` `len` `length` `isEmpty` `get` `getOr` `head`
  `last` `nth` `set` `pushBack` `pushFront` `popBack` `popFront` `append`
  `splitAt` `slice` `take` `drop` `map` `filter` `foldl` `foldr` `reverse`
  `updateAt` `forEach` `range` `elem` `notElem` `sum` `product` `sumBy` `all`
  `any` `find` `concat` `concatMap` `replicate` `takeWhile` `dropWhile`
  `partition` `zip` `zipWith` `unzip` `maximum` `minimum` `toArray` `toList`
  `fromArray` `fromList`

Also always available: the primitives (`println`, `print`, `show`, `arrayLen`,
`arrayGet`, `stringToBytes`, `stringToChars`, `charCode`, `bitAnd`, `toFloat`,
`toBigInt`, …) and the
constructors of every `Std` type (`Just`, `None`, `Ok`, `Err`, `True`, `False`,
`Left`, `Right`, `Nil`, `Cons`).

### The standard library

`Bool` `Ordering` `Function` `Tuple` `Int` `Maybe` `Char` `Result` `Either` `Bits`
`Bytes` `Yield` `Collections` (`Vector` `List` `Tree` `Set` `Map`) `State` `Exn`
`Stream` `Random` `Fs` `Process` `String` (`Parse`) `Path` `Json` `Time` `Test`

`Std.String.Parse` is a megaparsec-style parser combinator library; `Std.Json` is
built on it and is worth reading as a worked example.

### Gotchas, collected

- `%` follows the sign of the dividend: `(-7) % 3` is `-1`.
- `[1..5]` is **inclusive**; `range 1 5` is **half-open**.
- `Bool` prints lowercase but is written `True` / `False`.
- Adding `@pub` anywhere means `main` needs it too.
- Modules within a package share one flat namespace; `Mod.name` is for dependencies.
- Exhaustiveness is only checked under `--release`.
- `def` takes no parameters — `def f x = ...` is a parse error; use `fun`.
- A function that selects a field (`p.x`) cannot be applied to a *nominal* record;
  match on it instead.
- No guards, no `as`-patterns, no record update, no block comments.
- `Std.Char`'s predicates are ASCII-only; `String` counts bytes, `Char` counts
  scalars.
- `Std.Test` is not in the prelude — `use Std.Test (assertEq)`.
- A qualifier comes only from `as`: after `use Std.Collections.List`, it is
  `length`, not `List.length`.
