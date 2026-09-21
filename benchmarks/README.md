# Meadow against other languages

Seven tasks, eight languages, one program each. Four of the tasks are
single-threaded and three use every core the machine has.

This is not `benches/`. That directory times Meadow against *itself* — the
bytecode VM against the CEK machine — and is the right tool for asking whether
a change to the compiler helped. This directory asks a different and less
comfortable question: given a task, how long does Meadow take compared to the
languages someone might otherwise have written it in.

```
./run.py                      every task, every language whose toolchain is installed
./run.py --task matmul        one task
./run.py --lang meadow --lang rust --lang c
./run.py --reps 10            more repetitions; the minimum is reported
./run.py --list               what is here, and what is missing
./run.py --json out.json      the raw numbers as well as the table
```

Meadow needs a release build of its own compiler first:

```
cd ../buildtools && cargo build --release -p meadow
```

## The rules

**The same algorithm everywhere.** Not the same code — that would be a
translation exercise and would flatter whichever language the original was
written in. The loop structure, the data layout and the amount of work are
fixed; how each language expresses them is left to that language.

**Idiomatic, lightly optimized.** What a competent person would write knowing
it should be quick, and would not be embarrassed to show a colleague. Not the
benchmark-game entry with hand-unrolled SIMD and a custom allocator. So: `ikj`
loop order in the matrix multiply, because everyone knows to do that;
`STUArray` rather than a list in Haskell, because nobody would write the list;
but no intrinsics, no arena allocators, no unsafe tricks.

**Every program prints one line, and they must all agree.** The harness
compares the checksums before it reports any timing, and refuses to time a task
whose languages disagree. A benchmark that is quietly computing something else
is not a benchmark, and this is the only defence against it.

**The minimum of several runs**, not the mean. The work is deterministic, so
the spread is the operating system, and the shortest run is the one it
interfered with least.

**Wall clock for the whole process**, startup included. That is unfair to the
runtimes that have one, so the table's last row is the same measurement for a
program that only prints — the floor each language cannot go below. Java's 28ms
and Node's 56ms are in every number in their columns.

## The tasks

| | task | what it is for |
|---|---|---|
| | `fib` | recursive calls and nothing else: `fib 32`, 64-bit integers, no allocation |
| | `binarytrees` | allocation and collection: 67 million short-lived nodes, one long-lived tree |
| | `matmul` | 256×256 double matrix multiply, `ikj` order: flat arrays and tight loops |
| | `wordfreq` | a 3MB file, 400k words, a hash map and a sort |
| ⇉ | `mandelbrot` | data parallelism: a 2000×2000 grid of independent float work |
| ⇉ | `contention` | eight threads moving money between sixteen accounts, atomically |
| ⇉ | `pipeline` | message passing: four producers, one channel, 200k messages |

A few of these deserve a note about why they are shaped as they are.

**`binarytrees` gives every tip the iteration number it was built in.** Without
that the tree is the same on every pass of the loop, GHC floats it out and
builds it once, and the benchmark finishes in a fiftieth of the time having
measured nothing. This was not a hypothetical: the first version of this task
had Haskell at 23ms against C's 1.4s, with the right answer. Making each tree
depend on its iteration is the fix, and it is applied in all eight languages so
that none of them is being handicapped.

**`matmul` uses only whole numbers small enough to be exact in a double.** Every
product and every partial sum is an integer below 2^53, so fusing a multiply
and an add gives bit-identical results to doing them separately. Without that
the task would be measuring which compilers emit FMA on this hardware rather
than which ones are fast, and the checksums would not agree. C is built with
`-ffp-contract=off` as a second belt.

**`contention` is transfers, not a counter.** A shared counter is an atomic
add, which several of these languages would do without any mutual exclusion at
all, and then the comparison with Meadow's transactions would be meaningless.
Moving money needs two reads and two writes to happen as one step, which is the
thing transactions are for and the thing atomics cannot do. Because a transfer
is unconditional, the final balances do not depend on the order the threads got
their turns — so the sixteen printed balances are a real check that each of the 150,080
transfers landed exactly once and none of them twice, and not merely a timing.

**`mandelbrot` splits rows between threads by taking every Nth one**, so no
thread gets all the cheap ones, and the total does not depend on how many
threads there were. Every language may therefore use as many as it likes and
still has to produce the same number.

## The languages

| language | how it is built |
|---|---|
| Meadow | `meadow build --release` — `-O2`, compiled ahead of time to a native executable |
| Rust | `rustc -C opt-level=3`, what `cargo build --release` uses |
| C | `cc -O3 -ffp-contract=off` — `-O3` to match Rust's `opt-level=3`; `-ffp-contract=off` so `matmul` measures the loop and not who emits a fused multiply-add |
| Go | `go build` |
| Haskell | `ghc -O2 -threaded -with-rtsopts=-N` |
| Java | `javac`, default JVM settings |
| OCaml | `ocamlopt -O3`, native code |
| MLton | `mlton`, whole-program compilation |
| Koka | `koka -O2`, Perceus reference counting |
| Python | CPython, no flags |
| JavaScript | Node, no flags |

OCaml, MLton and Koka are here because they are the company Meadow is trying to
keep: compiled, garbage-collected or reference-counted functional languages
rather than C and Rust. Koka is the most pointed comparison of the three --
its Perceus reference counting is the nearest thing in production to the memory
discipline AxCut describes, and `binarytrees` is where that shows.

Three of the seven tasks are missing for all three, and for a reason worth
recording rather than hiding. **OCaml 4.14 has no multicore** (OCaml 5 does);
**MLton has no parallel runtime** (MPL is the fork that does); **Koka's
concurrency is `async`, not threads**. None of them can express the parallel
tasks the way the others do, and a `fork`-and-pipe imitation would measure the
imitation. `wordfreq` is also missing for MLton and Koka: SML's Basis Library
has no hash table, and Koka's `std/data/map` and `std/data/dict` are both stubs
marked "Todo".

Meadow is measured on its **ahead-of-time native backend**, which is what
`--release` selects. It also has a bytecode interpreter and a JIT; neither is
measured here, and `benches/` is where those get compared.

## Results

On a 10-core Apple silicon machine, macOS 26.3.1, minimum of five runs. The
figure in brackets is how many times slower than the fastest in that row.

| task | meadow | rust | c | go | haskell | java | ocaml | koka | python | js |
|---|---|---|---|---|---|---|---|---|---|---|
| fib | 26ms (2.9×) | 9ms (1.0×) | **9ms** | 10ms (1.1×) | 25ms (2.9×) | 37ms (4.2×) | 12ms (1.3×) | 11ms (1.3×) | 219ms (25.1×) | 77ms (8.8×) |
| binarytrees | 1.39s (4.5×) | 1.60s (5.2×) | 1.64s (5.3×) | 819ms (2.6×) | 427ms (1.4×) | **310ms** | 560ms (1.8×) | 346ms (1.1×) | 5.70s (18.4×) | 824ms (2.7×) |
| matmul | 14ms (2.8×) | 6ms (1.3×) | **5ms** | 13ms (2.7×) | 29ms (6.2×) | 39ms (8.2×) | 25ms (5.2×) | 201ms (42.1×) | 758ms (159.2×) | 78ms (16.4×) |
| wordfreq | 384ms (32.0×) | 13ms (1.0×) | **12ms** | 16ms (1.4×) | 132ms (11.0×) | 132ms (11.0×) | 52ms (4.4×) | — | 51ms (4.3×) | 114ms (9.5×) |
| ⇉ mandelbrot | 83ms (2.6×) | **32ms** | 48ms (1.5×) | 32ms (1.0×) | 77ms (2.4×) | 87ms (2.7×) | — | — | 1.20s (37.8×) | 127ms (4.0×) |
| ⇉ contention | 483ms (96.6×) | 6ms (1.1×) | **5ms** | 17ms (3.4×) | 38ms (7.6×) | 60ms (11.8×) | — | — | 59ms (11.6×) | 133ms (26.1×) |
| ⇉ pipeline | 53ms (3.1×) | 19ms (1.1×) | 31ms (1.8×) | **17ms** | 822ms (49.4×) | 62ms (3.7×) | — | — | 175ms (10.5×) | 154ms (9.3×) |
| _startup_ | 4ms | 2ms | 2ms | 4ms | 17ms | 30ms | 4ms | — | 19ms | 55ms |

**Absolute times move about 20% between runs of the whole suite on this
machine**, so do not read one run against another: this one has Rust, C and Go
all slower than the previous one by roughly that much, which says the machine
was busier and nothing about any compiler. Ratios *within* a row are taken in
the same conditions and are the trustworthy part. Where a change to Meadow is
claimed below, it was measured by alternating two compilers back to back on an
otherwise idle machine rather than by comparing two runs of this table.

Where Meadow was when this suite was written, and where it is now, after
the rounds of work [What was fixed](#what-was-fixed) describes -- measured the
same way, on the same tasks:

| task | at the start | now | faster by |
|---|---|---|---|
| fib | 50ms | **26ms** | 1.9× |
| binarytrees | 3.77s | **1.39s** | 2.7× |
| matmul | 2.79s | **14ms** | 199× |
| wordfreq | 1.44s | **384ms** | 3.8× |
| mandelbrot | 433ms | **83ms** | 5.2× |
| contention | 683ms | **483ms** | 1.4× |
| pipeline | 56ms | 53ms | — |

`contention` and `pipeline` move ±20% between runs of the same binary, so
nothing under that is claimed for them.

## What this says

**Meadow's calls, its channels, its float loops and its array loops are
good.** The spread inside Meadow's own column — 2.6× on `mandelbrot`, 97× on
`contention` — matters far more than where the column sits on average.

The three languages worth measuring against are OCaml, Koka and Haskell: compiled
functional languages with a managed heap, which is what Meadow is. Against those,
Meadow is currently **1.0–2.4× on `fib`, 2.5–4.0× on `binarytrees`, 2.9–7.4×
on `wordfreq`, and ahead of every one of them on `matmul`** — 14ms against
OCaml's 25ms and Koka's 201ms, level with Go, and 2.8× off C.

`pipeline` at 3.1× the fastest is the strongest result. It is behind Go, the
language whose reputation rests on this one thing, and behind Rust's `mpsc`, but
ahead of C's mutex and condition variables, ahead of Java and Node, and several
times ahead of GHC's `Chan`. Handing a value between green threads is what the
scheduler in `rts/src/sched.rs` was built for — a thread woken by a message goes
into the receiving worker's non-stealable next slot, so a send and the receive
answering it happen on one core back to back — and it shows.

`fib` at 2.9× says the calling convention and the native backend are sound. It
is the one task with no allocation, no arrays and no runtime services, so it is
the closest thing here to a measurement of the compiler on its own. It was
5.2× until [the sixth round](#the-sixth-round-arguments-in-registers) passed
arguments in registers and let a method load its own captures, which is what
the fourth round had said was left: the continuation on a stack moved `fib`
not at all, and the convention around the call moved it 1.8×. It now ties
GHC and is 2.2× off OCaml; what remains per call is the `live` high-water
mark, the preemption budget and a frame push through a nursery-style bump.

`mandelbrot` at 2.5× is a good number, and it is 5.7× two rounds back. Its inner loop is float arithmetic on eight registers and
nothing else, and since [the fifth round](#the-fifth-round-registers-and-the-inliner)
that arithmetic happens in `d16`–`d19` rather than through a register file in
memory. It is the one row where the machine code is now roughly what a native
compiler would emit, and the 2.3× that remains is the bookkeeping a preemptible
loop keeps — a high-water mark for the collector and a back-edge count — which
is the next thing to take out.

`matmul` at 2.8× has been 366×, 146×, 56×, 36× and 21×, each with a specific
cause. `St.get`
and `St.set` are primitives, and native code did not implement primitives: it
handed them back to the interpreter through `meadow_exec`, and sampling put 62%
of the run inside that call. [The third round](#the-third-round-the-native-back-end)
compiled them inline, and the same sampling put 0.6% there — and the benchmark
barely moved, because `St.get a i` was still a *call*: a one-line wrapper around
the primitive, and a call from a machine with no call stack is a continuation
built, a jump and a return through the generic `invoke`. Three of those per
element. [The fifth round](#the-fifth-round-registers-and-the-inliner) inlines
the wrappers, and the inner loop is now straight-line: two shifts, two adds, two
reads, a multiply, an add, a write, then one closure call and one return per
element for the loop itself; the sixth round made that call and return cheap,
[the eighth](#the-eighth-round-loops) removed them -- `St.forRange` with a
lambda is now a loop in the caller, and the loop no longer writes the
collector's high-water mark on every trip round -- and [the
ninth](#the-ninth-round-two-at-a-time) does the loop two elements at a time
on the vector unit, with every check on the arrays made once before it. What
is left is the 2.8× to a C compiler's own vectorizer, which unrolls further
and keeps its addresses in registers across the whole loop nest. Note that **Koka is 38× off C
here too** — array-heavy numeric code is hard for a reference-counted
functional runtime as well — and Meadow is now ahead of it on this row.

`contention` at 97× is a comparison of two software transactional memories:
GHC's runs the identical algorithm in 38ms. (This row moves ±20% between runs of
the same binary; 403ms and 556ms are both this compiler.) A profile of it is mostly
`__psynch_mutexwait`, which looks like lock overhead and is not: two attempts to
remove it — spinning before waiting, and an `RwLock` per cell so readers need
not take turns — are both measurably *worse*, and the note in `rts/src/stm.rs`
records the numbers. The transactions really do conflict. What is left is the
per-transaction machinery: `World::read`, `World::region_for_write` and
`World::commit` each allocate, and `Std.Stm` runs the control as effect
handlers.

`wordfreq` at 32× was 68× with a persistent HAMT doing the work a mutable
hash table does everywhere else; it now uses `Std.Collections.HashTable`,
which is that table, and the same shape of program as the Rust. What is left
is measured exactly (`MEADOW_TRAPS=1`): 2.7 million calls into the
interpreter per run, seven per word — two `stringIndexOf` and a slice from
`split`, a hash, an equality, and the store of the new count into an array
old enough to need the write barrier. Each is a Rust function that will stay
Rust; what they need is a cheaper way to be called than a round trip through
`meadow_exec`, and a `split` primitive would take three of the seven away.
OCaml's `Hashtbl` at 53ms is the number to aim at.

`binarytrees` is worth reading for its own sake, and it is where **Koka earns its
place in this table**: Perceus reference counting at 346ms is second only to
Java's collector and ahead of GHC, Go and V8, on the benchmark that is nothing
but allocate-walk-discard. Every generational collector with a bump allocator
beats `malloc`/`free` in C and `Box` in Rust by two to five times. The received
wisdom that reference counting cannot compete on allocation-heavy functional code
is not what this row says.

**Go is the most consistent column.** It wins or ties three tasks, is never worse
than 3.6×, and has none of the one-task disasters every other column contains
somewhere.

**Two results are about the other languages, not Meadow.** Python's `wordfreq` at
53ms ties OCaml and beats Haskell, Java and Node, because `str.split` and `collections.Counter`
are C and the Python is four lines of glue. And GHC's `Chan` on `pipeline` is a
boxed linked structure with an `MVar` per cell doing what a ring buffer does
elsewhere; it is also the noisiest measurement in the suite, varying between
44ms and 872ms across runs.

## What was fixed

Changes that came out of profiling this suite, in rounds. Each was measured
before and after, and the numbers are in [Results](#results).

### The ninth round: two at a time

A loop whose body is index arithmetic, reads and writes of arrays that do not
change during it, and float arithmetic on what it read -- `matmul`'s inner
loop, after the eighth round -- is done two elements per trip on the vector
unit (`rts/src/codegen/vector.rs`; `docs/RUNTIME.md`, "Vector loops"). The
plan is made from the bytecode: the induction register and its bound, which
registers are the arrays, that every index is the induction register plus
something invariant (stride one, so the element after is the next word), and
that nothing is carried from one iteration to the next through a register.
Before the loop, a *preheader* checks once what the scalar code checked on
every access -- that each array is the kind expected with `Float` elements,
that every index the whole loop will use is inside its array (the body's own
index arithmetic, run at the first and last values of the induction
register), and that an array written is not one read at a different index
under another name -- and then runs the body on `q` registers while two
iterations remain, falling into the ordinary scalar loop for the odd one out,
or for all of them if a check fails. The scalar loop is never changed, so the
vector version is only ever a faster way of doing the same thing.

Two things had to be true underneath. An object bigger than a block is now
contiguous in memory -- the old generation lays it out in a run of blocks in
one allocation -- so an array is one base and an offset, and the thin steps
reach an element that way (one block-table walk per access, not two). And
the checks had to be made where the access is, not after the body's
arithmetic: lowering reuses the index register for the store's argument
window, and a check made at the end compared an address with a length and
sent every loop to the scalar path.

`matmul` went from 104ms to 14ms: level with Go, ahead of OCaml, 2.8× from C.

### The eighth round: loops

Two things, and the second was found by the first.

`St.forRange lo hi (\i -> ...)` was a recursive function called with a
closure: a call, a frame and a return per iteration, for a body of a few
instructions. The inliner now **specialises** such a definition at a call
that gives a lambda for a parameter it passes through unchanged -- the
lambda substituted, the parameter dropped from the recursion -- and, where
every recursive call is a tail call, the copy is a *join point that jumps to
itself*: lowering makes that a block and a jump, and the loop's body is the
lambda's body. `while` and the folds qualify the same way. The simplifier
also applies a lambda where it stands (`(\i -> e) x` is `let i = x in e`),
which is what lets the substituted lambda disappear. `matmul`'s inner loop
became eight instructions and a jump -- and ran no faster.

The reason was the collector's high-water mark. `live`, the count of
registers a collection scans, was raised at every join, and a loop's header
is a join: five instructions per iteration, a load and a store of the same
word, which serialised every trip round on the one before. A jump into a
loop now declares `live` for the whole loop (the most registers anything in
it writes; raising it is safe, since the collector reads each pc's map for
what a register holds and `live` only bounds it), every other way in raises
to that, and the header knows it. `matmul` went from 178ms to 104ms.

### The seventh round: a mutable table, and the long tail

`wordfreq` was the row the sixth round had not touched, and profiling it said
why: the program was not the same program as the others'. Every other
language counts words in a mutable hash table; Meadow was path-copying a
persistent trie per word. `Std.Collections.HashTable` is the mutable table --
open addressing, linear probing, tombstones, cached hashes, living inside a
`runSt` like a `StArray` -- and `insertWith` is one probe and one write.
That alone took 816ms to 648ms, which was less than it should have been, and
the trap counter said the rest: 8.9 million instructions handed back to the
interpreter, twenty-two per word.

Most were one thing. A closure or a frame with more than eight captures has a
header longer than two words (the field descriptors no longer fit the second
one), and native code neither built such objects nor entered them: every
`insertWith` continuation captured ten to twelve values, so its allocation
*and* its invocation went to the interpreter. The native allocator, the frame
push, the general `invoke` path and the method entries now handle a header of
any length. With that, `getRef`, the array and string lengths as thin steps,
a bare store for a reference into a *young* array (no barrier is needed for
one), and `String.split` cutting into an array of the right size instead of
growing a persistent vector a piece at a time, the count is 2.7 million:
seven primitive calls per word, each one real work in Rust. That is the
long tail, and the shape of the next step -- a direct calling convention for
primitives, so a hash of five bytes does not cost a trip through the
interpreter's dispatch.

### The sixth round: arguments in registers

The calling convention, which the fourth round had measured as the cost of
`fib` and the fifth had left alone.

First a fixed assignment: bytecode registers `r0`–`r12` live in the same
thirteen machine registers in every native function, at every level, instead
of each function choosing which few to pin. Control passing from one function
to another then moves nothing, and the register file in memory is touched
only where the interpreter could look at it. On its own this moved nothing
either -- `fib` stayed at 47ms -- because `Invoke` still rebuilt that file in
memory on every call, word by word, and now reloaded thirteen registers from
it afterwards.

Then the call itself. Every method block gets a native **entry** that knows
its own shape -- the compiler now records, per method table, how many
captures the object holds and how many registers the block takes -- so a call
puts its arguments in `r0..argc` and jumps there, and the entry moves them
past the captures, loads the captures from the object, sets `live` and falls
into the block. A return is the same thing for a frame, which is recognised
by its address alone (everything in a stack chunk is a frame), popped, and
entered through the entry of the block it names. Nothing goes through memory.
The interpreter's `Invoke` did not change, and the two agree by construction
(`docs/RUNTIME.md`, "Method entries and the fixed registers").

`fib` went from 47ms to 26ms, `binarytrees` from 1.70s to 1.28s, `matmul`
from 280ms to 186ms. The bug this round found was not in native code: the Std
tests had never been run at `-O2`, and doing so showed the inliner losing a
value's type when it inlined a release-mode specialized copy, whose type
binders are all `#Ref`; lowering now reads the field types a pattern was
checked at when the value's own type says nothing.

### The fifth round: registers, and the inliner

Two changes, one on each side of the bytecode.

Native code now does its arithmetic *in* the machine registers a function pins,
instead of moving each operand into `x9`, operating there and moving the result
back: `add x3, x4, x5` where there were four instructions. A bytecode register
the function only ever does float arithmetic on is pinned to one of
`d16`–`d31` and stays there, so a `mulf` on two pinned registers is one `fmul`;
and the pool of general pins grew from seven to thirteen (`docs/RUNTIME.md`,
"Bookkeeping native code keeps up"). `mandelbrot`, whose inner loop is float
arithmetic on eight registers and nothing else, went from 179ms to 74ms: its
escape test is now `fmul, fmul, fadd, fcmp, b.le` on `d16`–`d19`. `matmul`
moved 4% on this alone, which said its cost was somewhere else.

It was. The inliner in `compiler/meadow-core/src/inline.rs` — written, tested
and left out of the pipeline because it broke two differential tests — is in
the pipeline for release builds (a debug build keeps every call a call, so the
debugger has frames to show). Neither failure was its own. A record accessor inlined at a record
literal is a `match` on a literal, whose fields lowering typed as nothing; and a
jump to a join point whose argument has to be evaluated first was not keeping
the join's environment alive meanwhile. Both were lowering bugs no program had
produced before, and both are fixed where they belonged. It also had to learn
that a call to a specialized copy carries the original definition's type
arguments. With `St.get` and `St.set` inlined, `matmul`'s inner loop has no
call in it but the loop's own, and 695ms became 280ms — from 146× C to 56× —
with nothing changed in the machine code for any of the instructions involved.
`wordfreq` moved 10% and `pipeline` 14% from the inliner alone.

`fib` and `binarytrees` did not move, as the fourth round said they would not:
their cost is the calling convention itself, which this round did not touch.

### The fourth round: a frame stack

A function's continuation was a heap object; it is now a frame on a per-thread,
chunked stack (`docs/RUNTIME.md`, "The call stack is a frame stack"), with
effect handlers capturing frames by unlinking chunks rather than copying them.
All of it is exercised by the differential suite against the CEK machine, which
never changed.

The measured result is the table above, and it settles a question this suite
had been assuming an answer to. `fib` is 46ms against OCaml's 12ms, and the
working theory was that the gap was the four-word continuation allocated per
non-tail call. It is not: with the continuation on a stack and returned through
by a direct jump, `fib` does not move. A nursery bump was already as cheap as a
push; the collections it caused were 0.5% of the run. What `fib` pays for is
the convention around the call — a register file that lives in memory, so every
argument and every returned value is a store and a load; a generic `invoke` that
checks the object's kind and copies its captures into that file one word at a
time; and a step counter kept per instruction for preemption. None of that is
touched by where the continuation lives. The stack is the prerequisite for
fixing it — a return that leaves its value in a machine register needs a frame
to return *to* — but it is not the fix.

### The third round: the native back end

Everything here came out of one question, answered by measurement instead of
inference: *which instructions does native code hand back to the interpreter,
and how often?* Static counts said 3–9%. A sampler on the executable said
`matmul` spent 62% of its time in `meadow_exec`. A per-pc counter in the
runtime then said exactly which ones.

**Native code could not reach the old generation.** The nursery is one flat
array, so a heap word was one load off its base; the old generation is Immix
blocks, each a separate allocation, so a promoted object's fields were
unreachable from machine code and every instruction touching one bailed to the
interpreter. `binarytrees` keeps a long-lived tree and many medium-lived ones,
all promoted: **55 million** `Field` and `JumpUnlessTag` traps on trees that
native code itself had built. The heap now publishes a **block table** per
generation (`Heap::tables`, at `layout::TABLES`); a heap word is
`tables[a >> 30][(a >> 13) & 0x1FFFF][a & 8191]`, three dependent loads. The
object instructions branch on the generation bit, so a young object still costs
one load. 55 million traps became 4,467; `binarytrees` 2.98s → 1.89s.

**Arrays went the same way**, and were the matmul number. `stGetArray`,
`arrayGet` and `stSetArray` expand into `codegen::thin` steps that the back end
emits inline, with the nursery-only guard replaced by "not a compact region",
and the emitter tightened to use immediate forms instead of building each
constant. `matmul` 1.78s → 692ms; 62% in the interpreter → 0.6%. A store is
only taken for an array whose elements are not references: that is what lets
it skip the marker's mutation lock, the snapshot barrier and the remembered
set, and the reasoning is written beside the expansion.

**Four instructions the machine did not have.** `toFloat` on an `Int` was
16 million traps a run of `mandelbrot` (twice per pixel, for the coordinates);
`popCount` and `>>>` are what a hash trie finds a child with. They are one
instruction each on every machine this targets and now are here too, with
`Ushr`'s folded-constant form.

**Two bugs the tests caught.** The block table is a `Vec` that moves as blocks
are added, and it was republished only when the *nursery* moved: `wordfreq`
read a freed copy and crashed. It is now republished at the single point
control returns to native code, `meadow_exec`, with a regression test that
promotes trees and closures and reads them natively. And a wrong encoding for
`addv` was an illegal instruction — caught by assembling every new encoding
with the system assembler and comparing bytes, which is how they are all
checked now.

### A change to the language: `Int` is the default

An integer literal nothing pins down was a `BigInt`; it is now an `Int`. This
suite is unmoved by it, because every program here annotates its numbers -- the
benchmark was written to measure the compiler, not the default. Unannotated
code is another matter. A program of the shape a person writes first --

```meadow
fun fib n = if n < 2 then n else fib (n - 1) + fib (n - 2)
fun sumTo n acc = if n == 0 then acc else sumTo (n - 1) (acc + n)
def work = [30, 30, 30, 30]
```

-- ran in **6.3s** under the old default and runs in **0.09s** under the new
one: 70×, from having machine words instead of heap-allocated numbers and typed
instructions instead of primitive calls. `benches/`, the VM-against-CEK package,
is unannotated too, and its ratios rose 20–80× for the same reason. What was
given up is that `2 ^ 64` is now `0` unless the program says `BigInt`, which
`docs/TUTORIAL.md` now says in the same place it used to say the opposite.

### The second round: the compiler

**Registers were never reused.** A name got a register when it was bound and
kept it for the rest of the block, so a straight-line block climbed through the
register file even where most of what it held was dead — and every transfer out
of it ended in a permutation putting things back. `move` was the most-retired
instruction in the machine: 27% of `fib`, 35% of `binarytrees`, 18% of
`mandelbrot`. The sequent IR already says where a value dies, so this was a
matter of reading it (`meadow_seq::still_used`) rather than computing liveness.
The case that matters is that **capturing a name is a use of it** — once a
closure holds a copy, what its methods do later is the methods' business — so a
continuation built out of the environment now lands on top of what it closed
over. `fib`'s recursive call went from five instructions to three:

```text
before                          after
closure r2 <- m7 [r0..+2]       closure r1 <- m7 [r0..+2]
subik   r3 <- r0 - 1            subik   r0 <- r0 - 1
move    r0 <- r3                jump    @169
move    r1 <- r2
jump    @169
```

**There was no simplifier.** `meadow_core::simplify` now does case of known
constructor (a `Maybe` built and matched in one expression is neither
allocated nor tested), case of case (the outer alternatives named once in a join
point and jumped to, rather than copied into each branch), and the algebraic
identities. On this suite the first two are **neutral**: these are numeric loops
and channels, not constructor-heavy code. They are kept because they are correct
and because the code they remove is the code inlining produces.

**The machine had no shift instruction**, and that made strength reduction a
pessimization. Turning `i * 256` into `i << 8` looks free — a multiply is three
to five cycles and a shift is one — but `Shl` was not in the typed instruction
table, so it lowered to a generic primitive, and a generic primitive is not
compiled natively: it traps back into the interpreter. `matmul` got **45%
slower** (1.84s to 2.66s). `ShlI`, `ShrI`, `AndI` and their folded-constant forms
now exist in the bytecode, the VM and both native back ends, and with them
strength reduction is neutral on `matmul` and worth 11% on `wordfreq`, which
hashes. The general lesson is the one worth keeping: an instruction is only
cheaper if the back end can see it.

### The first round: the runtime

**The nursery was 256 KB** (`rts/src/heap.rs`), so `binarytrees` collected about
eight thousand times. It is now 2 MB. That is a real trade and not a free win:
a nursery collection costs what *survives* it, so a bigger nursery buys
throughput and spends tail latency.

```text
  nursery   Latency p99   over 1 ms   binarytrees   copied
   256 KiB       115 us           0        3.77 s   926 MiB
     2 MiB       459 us           0        2.75 s     —
     8 MiB      1.31 ms           0        2.14 s   450 MiB
    32 MiB      5.24 ms          79        3.40 s   5.3 GiB
```

2 MiB is the largest that keeps every pause well under a millisecond, which is
the property `docs/RUNTIME.md` claims. At 32 MiB the collector copies 5.3 GiB out
of 8 GiB allocated, because a long-lived object sitting in a large nursery is
recopied at every collection instead of being promoted out of it.
`MEADOW_GC_NURSERY` moves it, in slots.

**Every collection zeroed the whole to-space.** A copying collector writes every
slot below `top` and nothing reads above it, so the memset was pure loss — and it
made a pause cost what the nursery *is* rather than what survived it. Removing it
cut median pause time by a quarter.

**Hashing allocated twice per call.** `Vm::hash_value` built a `Vec` work stack
for values that have no parts, and hashed a heap string by copying its bytes into
a fresh `Vec` first. A `Kind::Str` is already laid out exactly as the hasher
consumes a string — packed eight bytes to a little-endian word — so its words now
go straight in. `wordfreq` hashes a string twice per word; that was two million
allocations it did not need.

**A top-level `def` of a literal was a jump.** A mention of a global lowers to a
jump to its definition, and lowering only fuses a comparison into the branch
that tests it when the comparison cannot transfer control. So a bound written
`def limit : Int = 100` and read in a loop condition cost *two heap-allocated
continuations and an unfused compare per iteration*, where the same loop over a
parameter cost a single `bri`. `globals::inline_literals` puts the literal at
the mention. This was the largest single change in the suite's history, and it
moved every benchmark that reads a size or a bound from a top-level `def`.

**Each `St` array access read the object header three times** — once for the
kind, once for the length, once for the field. Now once. Worth 2%, and worth
less than it looks, because the cost of an array access is the round trip and
not the memory.

**The STM serialised every commit on one mutex**, and looked up each `TVar`
through an `RwLock` and an `Arc` clone on every read and every commit.
`World::tvar` was the largest single cost of a contended transaction. Commit
already locks the union of the read and write sets in sorted order, which is
two-phase locking and is all serializability needs, so the global lock was
redundant.

## Caveats, in order of how much they matter

1. **One machine, one run of the suite.** These numbers are from a 10-core
   Apple silicon laptop that was not otherwise idle. Ratios within a row are
   more trustworthy than absolute times, and nothing here is a claim about
   other hardware. The whole table moves about 20% between runs, so a cell
   compared against the same cell in an older version of this file says
   nothing: every claim in [What was fixed](#what-was-fixed) comes from
   alternating two compilers back to back instead. `contention` and `pipeline`
   vary by ±20% on their own — `contention` gave 403ms, 469ms and 475ms from
   one binary — and `pipeline` in Haskell moves between 44ms and 1.16s, so read
   that cell as an order of magnitude, not a number.
2. **MLton is written but has never been run.** There is no MLton toolchain on
   this machine, so `mlton` has never seen `tasks/*/*.sml`. The programs were
   written to the same specification as the rest and may well have typos.
   `brew install mlton` and `./run.py` will say. Every other number above comes
   from a run that produced the agreed checksum.
3. **Startup is inside every number.** Node pays 56ms and Java 28ms before
   their first instruction. On `fib`, where the fastest is 10ms, that is most
   of what separates the columns. Subtract the last row before drawing
   conclusions about short tasks.
4. **`matmul` in Python is lists, not numpy.** Anybody multiplying matrices in
   Python calls numpy and is then timing a BLAS written in C and Fortran. That
   is a fair thing to know and not a thing this suite measures, so the Python
   entry is the loops written out.
5. **`contention` in JavaScript is not idiomatic.** JavaScript has no shared
   mutable object across workers, so the accounts are a `SharedArrayBuffer` and
   the atomic step is held by a lock built from `Atomics.wait`. It is the only
   way to write the task and no working JavaScript programmer would enjoy it.
   The absence is the finding.
6. **`tasks/startup/` is generated** by the harness and overwritten on each run.
   `work/` holds build outputs, the generated corpus and `results.json`, and is
   not checked in.
