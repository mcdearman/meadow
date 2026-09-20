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
| C | `cc -O2 -ffp-contract=off` |
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
| fib | 47ms (4.9×) | 11ms (1.1×) | 10ms (1.1×) | **10ms** | 29ms (3.0×) | 35ms (3.6×) | 13ms (1.3×) | 10ms (1.1×) | 205ms (21.3×) | 72ms (7.5×) |
| binarytrees | 1.87s (6.3×) | 1.51s (5.1×) | 1.59s (5.4×) | 775ms (2.6×) | 414ms (1.4×) | **297ms** | 523ms (1.8×) | 330ms (1.1×) | 5.70s (19.2×) | 833ms (2.8×) |
| matmul | 687ms (146.9×) | 5ms (1.1×) | **5ms** | 13ms (2.8×) | 29ms (6.2×) | 41ms (8.7×) | 25ms (5.3×) | 201ms (43.0×) | 755ms (161.6×) | 77ms (16.5×) |
| wordfreq | 880ms (72.6×) | 14ms (1.1×) | **12ms** | 17ms (1.4×) | 142ms (11.7×) | 130ms (10.7×) | 53ms (4.4×) | — | 54ms (4.5×) | 118ms (9.8×) |
| ⇉ mandelbrot | 176ms (5.6×) | 34ms (1.1×) | 36ms (1.1×) | **32ms** | 76ms (2.4×) | 90ms (2.8×) | — | — | 1.21s (38.1×) | 131ms (4.1×) |
| ⇉ contention | 517ms (108.4×) | 5ms (1.1×) | **5ms** | 17ms (3.5×) | 41ms (8.6×) | 60ms (12.6×) | — | — | 59ms (12.4×) | 136ms (28.6×) |
| ⇉ pipeline | 61ms (4.3×) | 20ms (1.4×) | 31ms (2.2×) | **14ms** | 771ms (54.6×) | 63ms (4.5×) | — | — | 175ms (12.4×) | 160ms (11.3×) |
| _startup_ | 4ms | 2ms | 2ms | 3ms | 16ms | 29ms | 3ms | — | 20ms | 59ms |

**Absolute times move about 20% between runs of the whole suite on this
machine**, so do not read one run against another: this one has Rust, C and Go
all slower than the previous one by roughly that much, which says the machine
was busier and nothing about any compiler. Ratios *within* a row are taken in
the same conditions and are the trustworthy part. Where a change to Meadow is
claimed below, it was measured by alternating two compilers back to back on an
otherwise idle machine rather than by comparing two runs of this table.

Where Meadow was when this suite was written, and where the first round of
changes in [What was fixed](#what-was-fixed) took it:

| task | before | after | |
|---|---|---|---|
| mandelbrot | 433ms | **198ms** | 2.19× |
| matmul | 2.79s | **1.67s** | 1.67× |
| contention | 683ms | **498ms** | 1.37× |
| binarytrees | 3.77s | **2.76s** | 1.37× |
| wordfreq | 1.44s | **1.21s** | 1.19× |
| fib, pipeline | — | — | unchanged |

The second round — [register reuse](#what-was-fixed), the simplifier and the
shift instructions — alternated against the compiler before it, two rounds of
five runs each:

| task | before | after | |
|---|---|---|---|
| wordfreq | 1.30s | **1.17s** | 1.11× |
| fib | 50ms | **47ms** | 1.07× |
| mandelbrot | 244ms | **238ms** | 1.03× |
| binarytrees | 3.06s | **3.01s** | 1.02× |
| matmul, contention, pipeline | — | — | unchanged |

Smaller than the first round, and the reason is worth stating: the first round
removed things the machine was doing *per iteration of a loop* — a jump and two
continuations for reading a global, eight thousand collections, two allocations
per hash. This round removes instructions. `contention` and `pipeline` vary by
±20% run to run whatever is compiled, so nothing below that is claimed for them.

The third round — [native code reaching the whole heap](#the-third-round-the-native-back-end)
— alternated the same way:

| task | before | after | |
|---|---|---|---|
| matmul | 1.78s | **692ms** | 2.57× |
| binarytrees | 2.98s | **1.89s** | 1.57× |
| wordfreq | 1.07s | **893ms** | 1.19× |
| mandelbrot | 200ms | **178ms** | 1.13× |
| fib, contention, pipeline | — | — | unchanged |

Back to loop-shaped wins, and it says where they come from: the instructions in
a hot loop that native code was *handing back to the interpreter*. Statically
those were 3–9% of each program; dynamically they were 62% of `matmul`'s time
and 55 million of `binarytrees`' instructions. A counter in the runtime
(`MEADOW_TRAPS=1`, see `docs/RUNTIME.md`) now says exactly which ones, by pc.

## What this says

**Meadow's calls and its channels are good; its arrays are getting there.**
The spread inside Meadow's own column — 4.3× on `pipeline`, 147× on `matmul` —
matters far more than where the column sits on average.

The three languages worth measuring against are OCaml, Koka and Haskell: compiled
functional languages with a managed heap, which is what Meadow is. Against those,
Meadow is currently **3.6–4.7× on `fib`, 3.6–5.7× on `binarytrees`, 17× on
`wordfreq` and 3.4× on `matmul` against Koka** (27× against OCaml, whose arrays
are unboxed and whose loops are native).

`pipeline` at 4.3× the fastest is the strongest result. It is behind Go, the
language whose reputation rests on this one thing, and behind Rust's `mpsc`, but
ahead of C's mutex and condition variables, ahead of Java and Node, and several
times ahead of GHC's `Chan`. Handing a value between green threads is what the
scheduler in `rts/src/sched.rs` was built for — a thread woken by a message goes
into the receiving worker's non-stealable next slot, so a send and the receive
answering it happen on one core back to back — and it shows.

`fib` at 4.9× says the calling convention and the native backend are sound. It
is the one task with no allocation, no arrays and no runtime services, so it is
the closest thing here to a measurement of the compiler on its own — which is
why it is the row register reuse moved most (7%). The gap to OCaml and Koka
(10–13ms) is the cost of allocating a continuation for each of the two
non-tail calls per node.

`matmul` at 147× is still the worst number in the suite, and it used to be 366×
with a specific cause. `St.get` and `St.set` are primitives, and native code did
not implement primitives: it handed them back to the interpreter through
`meadow_exec`. Every element read and written in the innermost loop left machine
code, did a type check and a bounds check, and returned. Sampling the executable
put 62% of its time inside that call. Both are now compiled inline (see [the
third round](#the-third-round-the-native-back-end)), and the same sampling puts
0.6% there. What remains is the quality of the inline code: three array
accesses per iteration, each four guards and two table walks, all of which are
loop-invariant and none of which are hoisted yet. Note that **Koka is 38× off C here too** —
array-heavy numeric code is hard for a reference-counted functional runtime as
well — so the gap Meadow has to close to reach its own weight class is 3.4×,
not 147×.

What is left there is the **interpreter round trip itself**, and that is now
measured rather than assumed. Keeping the arrays in the nursery, so that every
address is a plain index and no block lookup happens at all, moves the benchmark
by 3% (1711ms to 1659ms). The cost is not reaching the memory; it is leaving
native code, dispatching through `Vm::exec` and `run_prim`, building a `Value`
and coming back. Only inlining the access into native code removes it.

`contention` at 108× is a comparison of two software transactional memories:
GHC's runs the identical algorithm in 41ms. (This row moves ±20% between runs of
the same binary; 403ms and 517ms are both this compiler.) A profile of it is mostly
`__psynch_mutexwait`, which looks like lock overhead and is not: two attempts to
remove it — spinning before waiting, and an `RwLock` per cell so readers need
not take turns — are both measurably *worse*, and the note in `rts/src/stm.rs`
records the numbers. The transactions really do conflict. What is left is the
per-transaction machinery: `World::read`, `World::region_for_write` and
`World::commit` each allocate, and `Std.Stm` runs the control as effect
handlers.

`wordfreq` at 73× is the persistent HAMT doing the work a mutable hash table
does elsewhere. OCaml's `Hashtbl` at 53ms is the number to aim at. Its remaining
interpreter traffic is known to the instruction: closures with more than eight
captures (which the native allocator and `invoke` do not yet handle), and the
string primitives, which are Rust and always will be — what they need is a
cheaper way to be called.

`binarytrees` is worth reading for its own sake, and it is where **Koka earns its
place in this table**: Perceus reference counting at 330ms is second only to
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

Changes that came out of profiling this suite, in two rounds. Each was measured
before and after, and the numbers are in [Results](#results).

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
