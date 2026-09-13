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
  main : ()
entry: main
Hello, Meadow!
=> ()
```

`main` has type `()` — it produces nothing useful. Printing is something it
*does* rather than something it returns, and a function's type records that
too; [chapter 9](#9-effects) is about how.

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
| `BigInt` | `42`, `-7`, `0xff`, `0o17`, `0b1011` | arbitrary precision; what an integer literal is by default |
| `Int` | *(same literals)* | 64-bit, wrapping; also spelled `Int64` |
| `Int8` `Int16` `Int32` | *(same literals)* | signed, wrapping at their width |
| `UInt8` `UInt16` `UInt32` `UInt64` | *(same literals)* | unsigned, wrapping at their width |
| `Float` | `3.14`, `42.0` | 64-bit; also spelled `Float64` |
| `Float32` | *(same literals)* | 32-bit |
| `Bool` | `True`, `False` | constructors, capitalised |
| `String` | `"hi"`, `"tab\there"` | a sequence of **bytes**; escapes `\n \t \r \\ \" \0` |
| `Char` | `'a'`, `'é'`, `'\n'` | one Unicode **scalar**, not one byte |
| unit | `()` | one value, written the same way as its type |

`Bool` values *print* as lowercase `true` / `false`, but you always write the
constructors `True` and `False`.

### One set of operators for every integer type

`+ - * / % ^` and `< > <= >=` work on **every** integer type. Both sides must
have the same type, and a literal takes whichever type its context needs. When
nothing settles it, an integer is a `BigInt`, so arithmetic you did not think
about cannot overflow:

```meadow
def big   = 2 ^ 100
def small = toInt 2 ^ 62
def byte  = toUInt8 250 + 6

def main = (big, small, byte)
```

```
=> (1267650600228229401496703205376, 4611686018427387904, 0)
```

The fixed-width types wrap at their width, as `byte` shows. Mixing two of them
is a type error rather than a silent conversion: `toInt 1 + toInt32 1` does not
compile. Say which one you mean with a conversion (below).

Floating point keeps its own operators, `+. -. *. /.` and `<. >. <=. >=.`, which
work on `Float` and `Float32`. A float literal nothing settles is a `Float`.

A function written with the operators works on any integer type. Its type says
so with a variable named `n`, which stands for "some integer type":

```meadow
fun square x = x * x

def main = (square 12, square (toUInt8 20))
```

```
=> (144, 144)
```

`square : forall n. n -> n`, and `20 * 20` wraps to `144` as a `UInt8`. Only
`fun` is generic in this way: a `def` or a `let` has one number type, settled
by how it is used, or `BigInt` if nothing uses it at a particular one.

A `BigInt` costs more than a machine word. In a loop that runs millions of
times, pin the counter to `Int` with an annotation — `fun go (i : Int) acc = ...`
— and every literal it meets follows.

Equality is different again: `==` and `!=` are **structural and work at any
type** — tuples, lists, constructors, records, anything.

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

`toInt` (or `toInt64`), `toInt8`, `toInt16`, `toInt32`, `toUInt8` … `toUInt64` and
`toBigInt` take any integer. Narrowing keeps the low bits, so `toInt8 200` is
`-56`. `toFloat` turns an integer into a `Float`; `toFloat32` and `toFloat64`
convert between the two float types; `floor` goes from a float to an `Int`.

```meadow
def main = (toFloat 3, floor 3.9, toBigInt 5, toInt8 200, toFloat32 (toFloat 3))
```

```
=> (3.0, 3, 5, -56, 3.0)
```

### Booleans and bit twiddling

`and` and `or` short-circuit, and `not` is an ordinary function. The bit
operators `<<`, `>>` and `>>>`, and the primitives `bitAnd`, `bitOr`, `bitXor`,
`bitNot`, `popCount` and `bitWidth`, work on every integer type at that type's
width: `>>` is arithmetic on a signed type and logical on an unsigned one, and
`>>>` is always logical. The amount shifted by is an `Int`. `Std.Num.Bits` adds
helpers on top.

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

There is also `Either a b` in `Std.Either`, for when neither side means
"failure". Its constructors are written `Either.Left` / `Either.Right`, or bare
after `use Std.Either.Either.*`.

Note the `use Std.String as S` there: the prelude's bare `toInt` is the
integer-conversion primitive, not string parsing. When a name feels like it should
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
| use it for | primitives, interop | **most things** | when O(1) access to the head is the point |

`Vector` is the default: the bare `map`, `filter`, `foldl`, `len`, `range` … in the
prelude are `Vector`'s. `List`'s equivalents need an explicit `use` — the prelude
does not activate a `List.` qualifier for you.

Reach for `List` when constant-time access to the head matters more than
anything else: building by consing onto the front, sharing a tail between
versions (an environment of bindings, say), or recursion that takes one element
off at a time. For everything else a `Vector` does more operations well and
keeps its elements close together in memory. The standard library follows the
same rule — its functions take and return vectors, and a `List` appears only in
the explicit `toList` / `fromList` conversions and where an algorithm is a list
algorithm underneath, like `Std.Sort`'s merge sort.

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

`concat` is common enough to have an operator. `a ++ b` is `S.concat a b`, and
it is in the prelude, so it needs no `use`:

```meadow
def main = "n = " ++ show 42 ++ "!"
```

```
=> "n = 42!"
```

`++` binds looser than application and tighter than `==`, and groups to the
right. Unlike the arithmetic operators it is not a primitive but an ordinary
name, bound with `fun (++) a b = ...` or `def (++) = ...`. It is imported,
exported and shadowed like any other name, and written `(++)` wherever a name
goes: `(++) "a" "b"`, `use Std.String ((++))`.

Underneath, the bytes of a string are a `#[UInt8]`: `stringToBytes` and
`bytesToString` convert, and `Std.Bytes` works on the array. A byte is a
`UInt8`, so arithmetic on one stays a `UInt8` and wraps. Convert before you
accumulate, or a digit fold quietly keeps only the low eight bits:

```meadow
fun digits acc bytes i =
  if i >= arrayLen bytes then acc
  else digits (acc * 10 + toInt (arrayGet bytes i - 48)) bytes (i + 1)

def main = digits 0 (stringToBytes "1234") 0
```

```
=> 1234
```

Without the `toInt`, `acc` would be a `UInt8` too, and the answer `210`.

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

### Maps keyed by anything

`Std.Collections.HashMap` maps any key `==` can compare to a value: strings,
tuples, records, vectors, constructors. Like `Vector` it is persistent, so an
update gives a new map and leaves the old one as it was:

```meadow
use Std.Collections.HashMap as HashMap

def ages = HashMap.fromVec [("ada", 36), ("grace", 45)]

def older = HashMap.adjust "ada" (\n -> n + 1) ages

def main = (HashMap.lookup "ada" ages, HashMap.lookup "ada" older, HashMap.size older)
```

```
=> (Just(36), Just(37), 2)
```

Underneath it is a hash array mapped trie, as in Rust's `im` or Clojure: a
lookup reads a handful of small nodes and an update copies one per level. Keys
are found by the `hash` primitive, which is structural and agrees with `==` —
`hash [1, 2]` is the same however that vector was built. A `Ref` or a function
cannot be a key: `hash` refuses both. (`Std.Collections.Map` is the older,
`Int`-keyed ordered map, for when the keys should come out sorted.)

---

## 8. Modules and packages

### A package

A package is a directory with a `meadow.toml` and a `src/`:

```
myapp/
  meadow.toml
  src/
    Main.mw
    Math.mw
```

```toml
[package]
name = "myapp"
version = "0.1.0"
```

`meadow run myapp` builds it and evaluates `main`. A single `.mw` file also counts
as a package, which is why `meadow run hello.mw` works.

Module files, and any directories under `src/`, are PascalCase: each is a name
in a `use` path, so `src/Math.mw` is the module `Math`. `Main.mw` (or `Lib.mw`) is
the package's root module. A lower-case module file is an error that says what
to rename it to. A lone file run directly is exempt — its name is the package's.

### Modules inside one package are separate namespaces

The package is the **compilation unit** — every module of it is resolved,
inferred and lowered together, so two modules may refer to each other and even
be mutually recursive. But each module is its own **namespace**, as in Rust: a
sibling's names arrive through a `use`, and never for free.

```meadow
-- src/Math.mw
@pub(pkg) fun double n = n * 2
```

```meadow
-- src/Main.mw  (a second file in the same package)
use myapp.Math (double)       -- the package name, then the module

def main = double 21
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

A constructor lives under its type, as a variant does in Rust. Naming a
**type** in a `use` brings the type and nothing else, so outside the module
that declares it a constructor is written `Expr.Int`:

```meadow
-- src/Syntax.mw
@pub(pkg) data Expr = Int Int | Add Expr Expr
```

```meadow
-- src/Eval.mw
use myapp.Syntax (Expr)      -- the type; its constructors stay `Expr.Int`

@pub(pkg) fun eval e = match e with
  | Expr.Int n -> n
  | Expr.Add a b -> eval a + eval b
```

To write them bare, put the type on the end of the path — Rust's
`use Expr::{Int, Add}`:

```meadow
use myapp.Syntax.Expr            -- just the type, same as `use myapp.Syntax (Expr)`
use myapp.Syntax.Expr (Int, Add) -- those two constructors, unqualified
use myapp.Syntax.Expr.*          -- every constructor of `Expr`, unqualified
```

Nothing else flattens them: not naming the type, and not a bare `use myapp.Syntax`,
which brings the module's values, types and effects but leaves constructors under
their types. `use myapp.Syntax (Int)` is an error that says where `Int` lives.
Inside `Syntax.mw` itself they are always bare. The prelude re-exports
`Just`, `None`, `Ok`, `Err` and `Ordering`'s three with `@pub use ... .*`, which is
the only reason those need no `use` anywhere.

### Visibility: `@pub`, `@pub(pkg)`, `@pub(super)`

Nothing is visible outside the module it is written in until it says so. Three
attributes say so, each one layer wider:

| written | seen by |
|---|---|
| nothing | its own module, and the modules inside it |
| `@pub(super)` | ...and its parent module's subtree |
| `@pub(pkg)` | ...and every module of this package |
| `@pub` | ...and anyone who depends on this package |

This is Rust's arrangement, spellings included, with the package where the crate
goes: a plain `@pub` is Rust's `pub`, and `@pub(pkg)` is `pub(crate)`. As in Rust,
something in the parentheses only ever *narrows* `@pub`. A library's surface is
its `@pub` declarations; `@pub(pkg)` is for the helper that two of your own
modules share and nobody else should.

```meadow
-- src/Math.mw
fun fudge n = n + 1             -- this module only
@pub(pkg) fun double n = n * 2  -- the rest of the package
@pub fun triple n = n * 3       -- and anyone who depends on us
```

A type's constructors are exactly as visible as the type, and an effect's
operations follow its effect.

One escape hatch, for small programs: **a package that never mentions
visibility has none** — every module sees every other, and everything is
exported. The moment any declaration is marked, the rules above apply to all of
them.

`main` is exempt: it is an entry point rather than an export, so it is found
whether or not it is marked.

### `use`

`use` brings a dependency's module into scope. Three forms, and the thing to
remember is that **a qualifier comes only from `as`**:

| Form | Effect |
|---|---|
| `use M` | every exported name, unqualified — constructors stay under their type |
| `use M as C` | `C.name` only — nothing unqualified |
| `use M (a, b)` | just `a` and `b`, unqualified |
| `use M as C (a, b)` | both: `C.name`, plus `a` and `b` unqualified |
| `use M.T` | the type `T`, its constructors written `T.C` |
| `use M.T (C, D)` | constructors `C` and `D` of `T`, unqualified |
| `use M.T.*` | every constructor of `T`, unqualified |

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

This is Meadow's most distinctive feature, and the one most worth reading slowly.
It assumes nothing: if you have never met algebraic effects, or `! { ... }` in a
type looks like line noise, start here.

The payoff first, because it is the reason to bother: code that talks to the
world — asks a question, reads a file, looks at the clock, gives up halfway — is
written *once*, and something outside it decides what "the world" is. The real
filesystem, or a list of strings. The real clock, or the number 500. Nothing is
written twice and nothing is passed in.

### The problem effects solve

Suppose a page greets whoever is signed in, and the name is only known at the
very top of the program. The function that needs the name is three calls deep:

```
page  ──calls──▶  banner  ──calls──▶  greeting  (needs the name)
```

Without effects you have two options, and both are bad. Pass the name as a
parameter through `page` and `banner`, which have no use for it themselves — and
do it again for every other thing `greeting` might ever need. Or keep it in a
global, and give up on calling `page` for two different people in one program.

An effect is a third option. `greeting` *asks* for the name, and does not care
who answers:

```meadow
use Std.String as S

effect Ask { ask : String -> String }

fun greeting () = S.concat "Hello, " (ask "name")

fun banner () = S.concatAll ["*** ", greeting (), " ***"]

fun page () = S.concat (banner ()) " Welcome back."

def main =
  ( handle page () with { ask question k -> k "Ada" }
  , handle page () with { ask question k -> k "Grace" } )
```

```
=> ("*** Hello, Ada *** Welcome back.", "*** Hello, Grace *** Welcome back.")
```

`banner` and `page` never mention a name, and the same `page` served two people.
The rest of this chapter takes that program apart piece by piece.

### Declaring and performing an operation

```meadow
effect Ask { ask : String -> String }
```

This declares an **effect** called `Ask` with one **operation**, `ask`. The type
says what the operation takes and what it gives back — here a question in, an
answer out — but, unlike a function, there is no body. Nothing here says *how*
a question is answered.

Calling `ask "name"` is called **performing** the operation. It looks exactly
like a function call, and to the code calling it, it is one: it takes a
`String` and evaluates to a `String`. The difference is where the answer comes
from. The call is a request sent *outwards*, to whichever handler is in charge
when it runs.

An effect may declare several operations, and may take type parameters the way
a `data` type does:

```meadow
effect State s { get : () -> s, put : s -> () }
```

An operation takes exactly one argument. For more than one, take a tuple — a
handler can pattern-match it apart, as below.

### Handling: answering the request

```meadow
handle page () with { ask question k -> k "Ada" }
```

`handle` runs the expression between `handle` and `with` — the **body** — and
the braces list what to do about each operation it performs. One entry is a
**clause**, and it reads left to right:

| part | meaning |
|---|---|
| `ask` | which operation this clause answers |
| `question` | a pattern for the operation's argument — here `"name"` |
| `k` | the **continuation**: the rest of the body, waiting for an answer |
| `k "Ada"` | resume the body, with `"Ada"` as the value `ask "name"` returns |

Everything in that table is ordinary except `k`, which is the subject of the
next section. In short: when `greeting` performs `ask`, it stops, the clause
runs, and `k "Ada"` sends `"Ada"` back to the spot where `greeting` stopped, so
`greeting` carries on as if `ask "name"` had simply returned `"Ada"`.

A handler may also have a **`return` clause**, which transforms the body's
final value on its way out. Leave it off and it is `return x -> x`:

```meadow
effect Ask { ask : () -> Int }

def main =
  ( handle ask () + ask () with { ask () k -> k 10 }
  , handle 1 + 2 with { ask () k -> k 10, return x -> x * 100 } )
```

```
=> (20, 300)
```

The first body asks twice and gets `10` both times. The second never asks at
all; the `return` clause still runs, on `3`.

If an operation is performed and nothing handles it, the program stops:

```meadow
effect Log { log : String -> () }

def main = log "nobody is listening"
```

```
unhandled effect Log.log
```

That is a *run-time* error, and it is the one place the types below do not
protect you: the type of a top-level `def` does not list what running it
performs, so nothing checks that `main` handled everything.

### Reading the types

Ask the compiler what the first program's functions are:

```
  ask : forall e. String -> String ! { Ask | e }
  greeting : forall e. () -> String ! { Ask | e }
  banner : forall e. () -> String ! { Ask | e }
  page : forall e. () -> String ! { Ask | e }
```

After a function's result comes `!` and an **effect row** in braces: the effects
calling the function may perform. `String -> String ! { Ask | e }` reads "takes
a `String`, returns a `String`, and along the way may perform `Ask`". Nobody
wrote those rows — they are inferred, and they spread from `ask` to every
caller that does not handle it, which is how `page` ends up saying it needs an
answer even though it never asks.

The `| e` is a **row variable**, and it means "and possibly other effects too".
It is what lets a function that performs `Ask` be called from one that also
performs something else — the two rows are merged, not compared. A function
that performs nothing has no `!` at all. A function performing two effects
lists both:

```meadow
effect Log { log : String -> () }
effect Ask { ask : String -> Int }

fun work () =
  let _ = log "starting" in
  let n = ask "how many?" in
  n * 2

fun silenced () = handle work () with { log m k -> k () }

fun answered () = handle silenced () with { ask q k -> k 21 }

def main = answered ()
```

```
  work : forall r. () -> Int ! { Log, Ask | r }
  silenced : forall r. () -> Int ! { Ask | r }
  answered : () -> Int
=> 42
```

Each handler takes one label off the row. `silenced` still asks, so its type
says so; `answered` handles the rest, and its type is plain `() -> Int` — the
compiler's guarantee that calling it cannot perform anything.

A function that takes a function takes on that function's effects, whatever they
are:

```meadow
effect Log { log : String -> () }

fun twice f = let _ = f () in f ()

fun logTwice () = twice (\() -> log "hi")

fun sumTwice () = twice (\() -> 1 + 1)
```

```
  twice : forall a e. (() -> a ! e) -> a ! e
  logTwice : forall e. () -> () ! { Log | e }
  sumTwice : forall n. () -> n
```

`twice` performs exactly what `f` performs — the row variable `e` appears on both
sides. So `logTwice` performs `Log` and `sumTwice` performs nothing, and `twice`
was written once. This is why `map`, `foldl` and every other higher-order
function in `Std` works with effectful functions for free.

### Continuations: what `k` is

`k` is the heart of the whole mechanism, so here it is slowly.

Take the body `ask () + 1`. At the moment `ask ()` is performed, the program has
done some of its work and has some left. What is left is: *take whatever `ask`
returns, add 1 to it, and finish the `handle`*. Write that leftover work with a
hole where the answer goes:

```
□ + 1
```

That is the **continuation** — the rest of the computation, from the point the
operation was performed to the end of the `handle` body. `k` is that hole, made
into a function: `k 41` fills the hole with `41` and runs the rest, which here
means `k 41` evaluates to `42`. In `page`, the continuation at the `ask` is "put
the answer after `Hello, `, then finish building the banner, then finish the
page" — three functions' worth of unfinished work, packaged as one value.

It helps to picture the call stack. When `greeting` performs `ask`, the stack
looks like this, innermost at the top:

```
│ greeting   waiting for ask's answer    ┐
│ banner     waiting for greeting        │  this part becomes k
│ page       waiting for banner          ┘
│ handle ... with { ask question k -> k "Ada" }
│ main
```

Performing `ask` searches downward for the nearest `handle` that has an `ask`
clause. The frames above it — everything between the `handle` and the `ask` —
are lifted off the stack and wrapped up as `k`. Then the clause runs, *in place
of the whole `handle` expression*. What the clause evaluates to is what the
`handle` evaluates to.

Calling `k "Ada"` puts those frames back, with the handler still underneath
them, and makes `ask "name"` return `"Ada"` inside `greeting`. The body carries
on from there. When the body finally finishes, its value goes through the
`return` clause, and *that* is what `k "Ada"` returns to the clause.

That last point decides the order things happen in, so watch it happen:

```meadow
effect Ask { ask : () -> Int }

def main =
  handle (
    let _ = println "body: before ask" in
    let x = ask () in
    let _ = println "body: after ask" in
    x + 1
  ) with {
    ask () k ->
      let _ = println "handler: before k" in
      let r = k 41 in
      let _ = println "handler: after k" in
      r,
    return v ->
      let _ = println "return clause" in
      v
  }
```

```
body: before ask
handler: before k
body: after ask
return clause
handler: after k
=> 42
```

Read it as a conversation. The body runs until it asks, then pauses. The
handler runs until it calls `k`, then *it* pauses while the body finishes —
including the `return` clause. Only then does `k 41` return `42` to the handler,
which prints its last line and makes `42` the value of the whole `handle`.

If you know exceptions, that is the one-sentence summary: **performing an
operation is throwing an exception that the handler can choose to resume**. The
jump to the handler is the same. What exceptions cannot do is jump *back*,
and `k` is exactly that ability, handed to the handler as a value.

### What a clause can do with `k`

Because `k` is a value, a clause is free to decide what to do with it, and each
choice is a different kind of program.

**Resume it right away** — `op x k -> k answer`. The body barely notices it was
interrupted. That is `Ask` above, and most handlers.

**Resume it, and use what it returns.** `k` returns the value of the rest of the
body, so the clause can wrap it:

```meadow
effect Log { log : String -> () }

fun work () =
  let a = log "step one" in
  let b = log "step two" in
  42

def collected =
  handle work () with {
    log m k -> pushFront (k ()) m,
    return x -> []
  }

def counted =
  handle work () with {
    log m k -> 1 + k (),
    return x -> 0
  }

def main = (collected, counted)
```

```
=> (["step one", "step two"], 2)
```

In `collected` the first `log` puts `"step one"` in front of whatever the rest of
the run produces, and the rest of the run is the second `log` doing the same
thing, and then the `return` clause throwing `42` away for an empty vector. One
handler collects the messages, the other counts them, and `work` knows about
neither.

**Never resume it.** If the clause does not call `k`, the rest of the body never
runs: its frames are simply dropped, and the clause's value becomes the
`handle`'s value. That is early exit, and it is the whole mechanism behind
`Std.Exn`, `Stream.take` and any "stop now" you write yourself:

```meadow
use Std.String as S

effect Abort { abort : String -> () }

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

There is no `break` in the language and none is needed: `forEach` does not know
it can be interrupted, and is interrupted anyway.

**Keep it for later.** `k` can be returned, stored in data, and called long
after the `handle` has finished. Here a job reports progress, and the handler
turns each report into a paused job that someone else decides when to resume:

```meadow
effect Progress { report : Int -> () }

data Job = Finished String | Suspended Int (() -> Job)

fun work () =
  let _ = report 25 in
  let _ = report 50 in
  let _ = report 75 in
  "all done"

fun start () = handle work () with {
  report pct k -> Job.Suspended pct k,
  return result -> Job.Finished result
}

fun drive job seen = match job with
  | Finished result -> (result, seen)
  | Suspended pct resume -> drive (resume ()) (pushBack seen pct)

def main = drive (start ()) []
```

```
=> ("all done", [25, 50, 75])
```

`start` returns as soon as `work` reports 25, with the rest of `work` inside the
`Suspended`. Each `resume ()` runs `work` to its next report — still under the
same handler, which wraps that report up the same way — until the `return`
clause produces `Finished`. That is a generator, a coroutine or an async task,
depending on who is calling `drive`, and `work` is none of them. It just reports.

**Return a function, and thread a value through it.** If every clause and the
`return` clause produce a *function*, the `handle` as a whole is a function too,
and applying it to a starting value passes that value from one operation to the
next:

```meadow
effect Counter { next : () -> Int }

fun job () = let a = next () in let b = next () in let c = next () in [a; b; c]

fun counting act =
  (handle act () with {
    next () k -> \n -> (k n) (n + 1),
    return x -> \n -> x
  }) 0

def main = counting job
```

```
=> [0; 1; 2]
```

`next () k -> \n -> (k n) (n + 1)` says: given the current count `n`, answer
`n`, and hand `n + 1` to whatever comes next. `k n` resumes the body, and the
body's remainder is itself one of these functions, waiting for its count. This
is how `Std.State` threads state with no mutation anywhere, and its type says
exactly what happened to the effect:

```
  counting : forall a e. (() -> a ! { Counter | e }) -> a ! e
```

**Not twice.** A continuation can be resumed at most once:

```meadow
effect Choose { choose : () -> Bool }

def main =
  handle (if choose () then 1 else 2) with {
    choose () k -> k True + k False
  }
```

```
continuation resumed more than once
```

Handlers in Meadow are **one-shot**. Resuming moves the suspended frames back
onto the stack instead of copying them, which is what keeps performing an
operation cheap — and a second resume would find nothing left to move. Programs
that want to explore several answers (backtracking, probability) have to be
written as a loop that performs again rather than one that resumes twice.

### The rules, collected

**The innermost handler wins.** Performing searches outwards and stops at the
first handler with a clause for the operation:

```meadow
effect Ask { ask : () -> Int }

def main =
  handle (
    handle ask () with { ask () k -> k 1 }
  ) with { ask () k -> k 100 }
```

```
=> 1
```

**Handlers are deep.** Resuming `k` puts the body back *with its handler around
it*, so every later operation in that body goes to the same handler — which is
why `collected` saw both logs, and why `drive` kept getting `Suspended` jobs
back. Nothing has to reinstall anything.

**A clause runs outside its own handler.** An operation performed *inside* a
clause goes to the next handler out, not back to the one the clause belongs to,
so a handler can pass things along:

```meadow
effect Log { log : String -> () }

use Std.String as S

def main =
  handle (
    handle log "inner" with {
      log m k -> let _ = log (S.concat "relayed: " m) in k (),
      return x -> "done"
    }
  ) with {
    log m k -> let _ = println (S.concat "outer handler saw: " m) in k (),
    return x -> x
  }
```

```
outer handler saw: relayed: inner
=> "done"
```

**One handler can answer several effects**, and a clause's argument can be any
pattern — which is how an operation with a tuple argument is taken apart:

```meadow
effect Files { write : (String, String) -> () }
effect Log { log : String -> () }

fun save () =
  let _ = log "saving" in
  let _ = write ("a.txt", "hello") in
  write ("b.txt", "world")

def main =
  handle save () with {
    log m k -> k (),
    write (path, text) k -> pushFront (k ()) path,
    return x -> []
  }
```

```
=> ["a.txt", "b.txt"]
```

**An operation without a clause passes through** to the next handler out, and
its effect stays in the type — a handler only takes an effect off the row when
it answers every one of its operations:

```meadow
effect Counter { next : () -> Int, reset : () -> () }

fun job () = let a = next () in let _ = reset () in let b = next () in (a, b)

fun partial () = handle job () with { next () k -> k 7 }

def main = handle partial () with { reset () k -> k () }
```

```
  partial : forall e. () -> (Int, Int) ! { Counter | e }
=> (7, 7)
```

The row names effects, not operations, so the type cannot say "`Counter`, but
only `reset`". It errs toward saying too much.

### Which things are effects, and which are not

Printing is an effect like any other:

```meadow
use Std.Console (withOutput)

fun greet name = println name

def main = withOutput (\() -> let _ = greet "Ada" in greet 42)
```

```
  greet : forall a e. a -> () ! { Console | e }
=> ((), "Ada\n42\n")
```

`print` and `println` are ordinary functions in `Std.Console`, re-exported by the
prelude so they need no `use`. Underneath they perform one operation,
`writeOutput : String -> ()`. Unhandled, that writes to the real terminal; under
`withOutput`, as here, nothing is printed and the text comes back as a value.
A value that is not a `String` is written the way `show` renders it, which is
why `42` came out as `42`. `withOutput` answers only `writeOutput` and not
`readLine`, so by the rule above `Console` stays in the type of anything that
uses it.

Reading works the same way — `Console.readLine`, answered by `withInput` — and
so do the filesystem, the clock, randomness and subprocesses. Anything that
touches the world is an operation a handler can stand in for, which is what
lets a test run a program that prompts, prints and rolls dice without a
terminal or an unpredictable number anywhere near it.

The one exception is `Mut`, for `Ref` cells. `newRef`, `getRef` and `setRef`
carry it so that a function that mutates says so and a pure one still reads as
pure, but it has no operations and nothing handles it: a cell is a cell. It is
covered under the standard library below.

### The standard library's effects

`Std` ships nine, plus `Mut` for mutable cells and `St` for mutation kept local. Each pairs a real implementation
with a handler that fakes it — which is the point, since these are exactly the
things that are otherwise hard to test.

| Module | Operations | Unhandled | Handled with |
|---|---|---|---|
| `Std.Ref` (`Mut`) | via `newRef` / `getRef` / `setRef` | real cells | — |
| `Std.St` (`St s`) | via `stNewRef`, `stNewArray`, … | — | `runSt` |
| `Std.State` | `get`, `put` | — | `runState`, `evalState`, `execState` |
| `Std.Console` | `writeOutput`, `readLine` | real stdout and stdin | `withOutput`, `withInput` |
| `Std.Exn` | `throw` | aborts | `toResult`, `catch`, `withDefault`, `toMaybe` |
| `Std.Yield` | `yield` | — | everything in `Std.Stream` |
| `Std.Random` | `nextInt`, `intBetween`, … | real entropy | `withSeed`, `withSeedFrom` |
| `Std.Time` | `now`, `monotonic`, `sleep` | real clock | `withClock`, `withTickingClock` |
| `Std.Fs` | `readToString`, `writeString`, … | real filesystem | any `handle` |
| `Std.Process` | `spawn`, `status`, `argv`, … | real subprocesses | any `handle` |
| `Std.Test` | `fail` | fails the test | `didFail` |

Both engines answer an unhandled `Console`, `Fs`, `Process`, `Random` or `Time`
operation for real, so a test that should not touch the filesystem has to handle
`Fs` — which, for the same reason, is all it takes.

#### Mut — the one mutable cell

`newRef`, `getRef` and `setRef` are primitives and always in scope. Every one of
them carries the `Mut` effect, so a function that mutates says so in its type and
a pure one still reads as pure:

```meadow
use Std.Ref (modify, repeatN)

fun countUp n =
  let r = newRef 0 in
  let ignored = repeatN n (\() -> modify r (\x -> x + 1)) in
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
cells, not contents, so `newRef 1 == newRef 1` is `False`. Only mutable things
behave that way: `Ref`s, and the arrays in the next section.

#### St — mutation that stays inside

`Mut` never goes away: a function that uses a `Ref` for its own private
bookkeeping still shows `! { Mut | e }` to every caller, forever. `runSt` is the
way out. Mutation done inside it — on cells from `stNewRef`, or mutable arrays
from `stNewArray` — cannot be seen from outside it, so the type of the whole
thing is as pure as the result:

```meadow
use Std.St as St

fun sumTo n =
  runSt (\() ->
    let total = St.newRef 0 in
    let _ = St.forRange 1 (n + 1) (\i -> St.modifyRef total (\t -> t + i)) in
    St.getRef total)

fun fibs n =
  runSt (\() ->
    let a = St.newArray n 0 in
    let _ = if n > 1 then St.set a 1 1 else () in
    let _ = St.forRange 2 n (\i -> St.set a i (St.get a (i - 1) + St.get a (i - 2))) in
    St.toVec a)

def main = (sumTo 100, fibs 10)
```

```
  sumTo : forall n. n -> n
  fibs : forall n. Int -> [n]
=> (5050, [0, 1, 1, 2, 3, 5, 8, 13, 21, 34])
```

No `Mut`, and no `St` either. What makes that safe is a check on the types, not
on what the code happens to do. Each `runSt` gets its own state type `s`, which
the checker invents for that `runSt` and nobody can name; everything made inside
carries it (`StRef s Int`, `StArray s Int`), and so does every operation's effect
(`! { St s | e }`). `runSt` takes the `St s` away, and in exchange nothing that
mentions `s` may leave:

```meadow
def cell = runSt (\() -> stNewRef 0)
```

```
state from inside a `runSt` escapes it
```

The same goes for a closure that reads a cell, a cell written into some `Ref`
from outside, or a cell used after its `runSt` has returned. Everything that
does not mention `s` passes through as usual: an inner `runSt` may use an outer
one's cells, a `Log` performed inside stays in the type, and a callback handed
in from outside keeps its own effects and nothing more. `Std.Sort.sortBy` works
this way — it sorts a mutable array in place and is still
`(a -> a -> Ordering ! e) -> [a] -> [a] ! e`.

A `StArray` is written in place, so `St.set` is O(1) where `arraySet` copies the
whole `Array`. `St.freeze` and `St.thaw` copy between the two, and `St.fromVec` /
`St.toVec` do the same for vectors. `runSt` has to be applied to its body
directly, `runSt (\() -> ...)`: that application is where the special typing
happens.

#### State — a value threaded for you

Where `Mut` is a real cell, `State` is a value passed along invisibly and handed
back at the end. Nothing is allocated and nothing is shared.

```meadow
use Std.State (get, put, modify, runState, evalState, execState)

fun tick () =
  let n = get () in
  let ignored = put (n + 1) in
  n

def main = runState 0 (\() -> let a = tick () in let b = tick () in get ())
```

```
=> (2, 2)
```

`runState` returns `(value, finalState)`; `evalState` keeps the value, `execState`
the state. `modify f` is `put (f (get ()))` and `gets f` is `f (get ())`.

Reach for `State` when the state is part of what a computation *means* and you
want it out of the signatures; reach for `Mut` when you want a cell with identity,
or speed.

#### Console — the terminal, and why it makes a program testable

Two operations: `writeOutput`, which `print` and `println` are built on, and
`readLine`. `prompt` is `print` then `readLine`. `None` from `readLine` means
end of input — a closed pipe, or Ctrl-D — which is not an error, so a loop ends
by matching it rather than by catching anything.

```meadow
use Std.Console (prompt, withInput, readLine)
use Std.String as S

fun greet () =
  match prompt "name: " with
  | Just name -> S.concat "hello, " name
  | None -> "nobody there"

def main = (withInput ["ada"] greet, withInput [] greet)
```

```
name: name: => ("hello, ada", "nobody there")
```

The two `name: ` are real: `withInput` answers only `readLine`, so the `print`
inside `prompt` went past it to the terminal. Add `withOutput` and the whole
conversation stays inside the program:

```meadow
use Std.Console (prompt, withInput, withOutput)

def main = withOutput (\() -> withInput ["ada"] (\() -> prompt "name: "))
```

```
=> (Just("ada"), "name: ")
```

`withInput` feeds a vector of lines and answers `None` once they run out, so the
same function covers both the interactive and the exhausted case.

The example in `examples/rock-paper-scissors` is the whole point of this in
practice. It is an interactive terminal game — it prompts, it loops, it keeps a
tally — and its test plays a *complete game* with no terminal and no entropy
anywhere near it:

```meadow
@test fun playsAFullRound () =
  let played =
    R.withSeed 7 (\() -> withInput ["rock", "nonsense", "paper", "quit"] game) in
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

fun guess () =
  let secret = R.between 1 11 in
  match prompt "pick 1-10: " with
  | None -> "no answer"
  | Just typed -> if S.trim typed == show secret then "right" else "wrong"

def main = R.withSeed 1 (\() -> withInput ["3"] guess)
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
  ( toResult (\() -> 1 + half 8)
  , toResult (\() -> 1 + half 7)
  , withDefault 0 (\() -> half 7)
  , toMaybe (\() -> half 7) )
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

`Std.Yield` is one operation, `yield : a -> ()`. A producer performs it; a
consumer decides what it means. `Std.Stream` is the consumers.

```meadow
use Std.Yield (yield)
use Std.Stream as St

fun countdown n =
  if n <= 0 then ()
  else let _ = yield n in countdown (n - 1)

def main = (St.toVec (\() -> countdown 5), St.take 2 (\() -> countdown 100))
```

```
=> ([5, 4, 3, 2, 1], [100, 99])
```

`take` is the interesting one: it simply stops resuming, which unwinds the
producer where it stands. So a producer is lazy without being written lazily —
`countdown 100` really does stop after two, and this stops after three rather than
building a million-element anything:

```meadow
use Std.Stream as St

def main =
  ( St.take 3 (\() -> St.range 0 1000000)
  , St.toVec (\() -> St.map (\x -> x * x) (\() -> St.range 1 5))
  , St.sum (\() -> St.range 1 101) )
```

```
=> ([0, 1, 2], [1, 4, 9, 16], 5050)
```

Consumers: `toVec`, `toList`, `forEach`, `fold`, `count`, `sum`, `take`,
`takeWhile`, `first`, `any`, `all`, `find` — the ones that collect build a
`Vector`, except `toList`. Transformers, which are producers that
consume: `map`, `filter`. Producers: `ofList`, `ofVec`, `range`, `repeat`,
`iterate`. Every collection has `toStream`.

#### Random — deterministic when you want it

```meadow
use Std.Random as R

def rolls = R.withSeed 42 (\() -> [R.between 1 7; R.between 1 7; R.between 1 7])

def main = (rolls, rolls == R.withSeed 42 (\() -> [R.between 1 7; R.between 1 7; R.between 1 7]))
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

def frozen = T.withClock 500 (\() -> (T.now (), T.now ()))

def ticking = T.withTickingClock 1000 10 (\() -> (T.now (), T.now (), T.now ()))

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
`isDir`). Convenience: `readToStringOr`, `tryReadDir`, `existsAll`. `readDir`
answers a `Vector` of names; `readBytes` answers a `#[UInt8]`, the byte-array
type `Std.Bytes` works on.

#### Process — subprocesses, argv and the environment

A command is built up rather than passed as one lump: `command "git"` then
`withArg`, `withArgs`, `withCwd`, `withEnv`. `run` and `runInherit` are the short
forms.

```meadow
use Std.Process as P

fun versionOf tool =
  match P.run tool ["--version"] with
  | Ok out -> P.outputStdout out
  | Err e -> e

def main =
  handle versionOf "meadow" with {
    spawn cmd k -> k (Ok (0, "meadow 0.1.0-alpha", "")),
    return x -> x
  }
```

```
=> "meadow 0.1.0-alpha"
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

def main = (didFail (\() -> assertEq 1 1 "same"), didFail (\() -> assertEq 1 2 "different"))
```

```
=> (false, true)
```

### Writing a handler for your own effect

The shape is always the same. Declare the operations; write the code that
performs them without thinking about who answers; then write one handler per
interpretation — the real one, and the one a test wants. Each clause is one of
the choices from [What a clause can do with `k`](#what-a-clause-can-do-with-k):

- **Resume with an answer** — `op x k -> k answer`. The computation carries on.
- **Resume, then use the result** — `op x k -> f (k ())`. The clause sees
  everything that followed; `collected` and `counted` work this way.
- **Do not resume** — `op x k -> value`. The rest is abandoned; `Exn`, `take` and
  `Abort` work this way.
- **Keep `k`** — store it or return it, and resume it later from somewhere else.
  `Job.Suspended` works this way.
- **Return a function** — `op x k -> \state -> ...`, with the `return` clause
  `\state -> ...` too, then apply the whole `handle` to a starting value. This is
  how `State` threads a value and how `withInput` threads the remaining lines;
  `Std.State.runState` is the smallest complete example.

And whichever you choose, resume at most once.

## 10. Testing

Mark a function `@test` and `meadow test` runs it. A test takes one argument — the
runner calls it with `()`.

```meadow
use Std.Test (assertEq, assertTrue)

fun double n = n * 2

def main = double 21

@test fun doubling () = assertEq (double 21) 42 "double 21"

@test fun listsCompare () = assertEq [1; 2] [1; 2] "list equality"

@test fun somethingTrue () = assertTrue (double 2 == 4) "double 2"
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

In a package of several modules a test is named by its module — `Parser.parses`
rather than `parses` — since two modules may each have a test of that name.
`--exact` runs only the test whose name is the filter, so
`meadow test . Parser.parses --exact` is one test and never `Parser.parsesInts`.
In VS Code, the **▶ Test** link above a `@test` runs exactly that.

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
inferred type and the `--` comment above the definition, go-to-definition,
rename, inlay hints for parameters and `let` bindings, and semantic tokens.

A file inside a package is analysed as part of that package, so a `use` of a
sibling module resolves exactly as it does in a build, and go-to-definition and
rename both cross between modules. **Rename** follows name resolution rather
than text: it changes the binding under the cursor and not something else
spelled the same, and it refuses a name whose definition is outside the package
(the standard library's, say) rather than editing half of it.

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
| `==` `!=` `<` `>` `<=` `>=` (and `<.` `>.` `<=.` `>=.`) | comparison |
| `::` `++` | cons, string concatenation; right-associative |
| `+` `-` (and `+.` `-.`), `<<` `>>` `>>>` | |
| `*` `/` `%` (and `*.` `/.`) | |
| `^` | power, right-associative |
| `-` (prefix) | negation |
| *juxtaposition* | function application, tightest |

### Always in scope

Without any `use`, from the prelude:

- **Bool** `not`
- **Ordering** `compare` `isLess` `isEqual` `isGreater`
- **Function** `id` `const` `flip` `compose` `apply` `twice`
- **Tuple** `fst` `snd` `pair` `swap` `mapFst` `mapSnd`
- **Num.Int** `min` `max` `abs` `signum` `clamp` `succ` `pred` `even` `odd` `gcd` `lcm`
- **Num.Bits** `lowMask` `lowBits` `bit` `testBit` `setBit` `clearBit` `flipBit`
  `byteOf` `fromBytes4` `countLeadingZeros` `countTrailingZeros` `rotateLeft`
  `rotateRight`
- **Bytes** `bytesLength` `bytesGet` `bytesSet` `bytesPush` `bytesSlice`
  `bytesConcat`, the `U16`/`U32`/`U64` accessors, …
- **Fs** `readToString` `writeString` `exists` `readDir` …
- **Process** `command` `run` `exit` `argv` `getEnv` …
- **Vector** `empty` `singleton` `len` `length` `isEmpty` `get` `getOr` `head`
  `last` `nth` `set` `pushBack` `pushFront` `popBack` `popFront` `append`
  `splitAt` `slice` `take` `drop` `map` `filter` `foldl` `foldr` `reverse`
  `updateAt` `forEach` `range` `elem` `notElem` `sum` `product` `sumBy` `all`
  `any` `find` `concat` `concatMap` `replicate` `takeWhile` `dropWhile`
  `partition` `zip` `zipWith` `unzip` `maximum` `minimum` `toArray` `toList`
  `fromArray` `fromList`

Also always available: `++` (`Std.String.concat`); `print` and `println`; `runSt`; the primitives (`show`, `display`, `hash`,
`arrayLen`, `arrayGet`, `stringToBytes`, `stringToChars`, `charCode`, `bitAnd`,
`toInt`, `toUInt8`, `toFloat`, `toBigInt`, …); and the constructors `Just`, `None`, `Ok`, `Err`,
`Less`, `Equal`, `Greater`, `True`, `False`, `Nil` and `Cons`.

### The standard library

`Bool` `Ordering` `Function` `Tuple` `Num` (`Int` `Bits`) `Maybe` `Char` `Result`
`Either` `Bytes` `Yield` `Collections` (`Vector` `List` `Tree` `Set` `Map` `HashMap`) `State` `St` `Exn`
`Stream` `Random` `Fs` `Process` `String` (`Parse`) `Path` `Json` `Time` `Test`

`Std.String.Parse` is a megaparsec-style parser combinator library; `Std.Json` is
built on it and is worth reading as a worked example.

### Gotchas, collected

- `%` follows the sign of the dividend: `(-7) % 3` is `-1`.
- An integer literal nothing pins down is a `BigInt`; annotate a hot loop's
  counter as `Int`.
- A byte is a `UInt8`, and so is arithmetic on it: `toInt (b - 48)` before
  accumulating digits.
- `+` is for integers and `+.` for floats; two different integer types never mix
  without a conversion.
- `[1..5]` is **inclusive**; `range 1 5` is **half-open**.
- `Bool` prints lowercase but is written `True` / `False`.
- `@pub` is public, as in Rust; `@pub(pkg)` stops at the package, like `pub(crate)`.
- A package that marks nothing has no visibility rules at all — mark one thing
  and every module has to say what it shares.
- Each module of a package is its own namespace; a sibling's names come by `use`.
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
