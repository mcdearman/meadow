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
| fib | 49ms (5.3×) | **9ms** | 10ms (1.1×) | 11ms (1.1×) | 30ms (3.3×) | 36ms (3.9×) | 11ms (1.2×) | 10ms (1.1×) | 210ms (22.8×) | 72ms (7.8×) |
| binarytrees | 2.72s (9.1×) | 1.54s (5.1×) | 1.62s (5.4×) | 779ms (2.6×) | 413ms (1.4×) | **299ms** | 550ms (1.8×) | 333ms (1.1×) | 5.73s (19.2×) | 868ms (2.9×) |
| matmul | 2.74s (523.1×) | 6ms (1.1×) | **5ms** | 12ms (2.4×) | 29ms (5.6×) | 40ms (7.7×) | 25ms (4.9×) | 201ms (38.4×) | 764ms (146.1×) | 80ms (15.3×) |
| wordfreq | 1.28s (105.4×) | 14ms (1.2×) | **12ms** | 17ms (1.4×) | 135ms (11.2×) | 130ms (10.8×) | 51ms (4.2×) | — | 53ms (4.4×) | 116ms (9.6×) |
| ⇉ mandelbrot | 425ms (13.8×) | **31ms** | 36ms (1.2×) | 32ms (1.0×) | 76ms (2.5×) | 89ms (2.9×) | — | — | 1.21s (39.1×) | 127ms (4.1×) |
| ⇉ contention | 581ms (123.0×) | 7ms (1.5×) | **5ms** | 17ms (3.6×) | 41ms (8.6×) | 59ms (12.6×) | — | — | 59ms (12.4×) | 138ms (29.2×) |
| ⇉ pipeline | 61ms (3.4×) | 18ms (1.0×) | 32ms (1.8×) | **18ms** | 280ms (15.9×) | 62ms (3.5×) | — | — | 175ms (9.9×) | 160ms (9.1×) |
| _startup_ | 4ms | 2ms | 2ms | 4ms | 18ms | 30ms | 4ms | — | 18ms | 56ms |

Where Meadow was when this suite was first written, and where three runtime
changes have taken it (see [What was fixed](#what-was-fixed)):

| task | before | after | |
|---|---|---|---|
| binarytrees | 3.77s | **2.72s** | 1.39× |
| contention | 683ms | **581ms** | 1.18× |
| wordfreq | 1.44s | **1.28s** | 1.13× |
| matmul | 2.79s | 2.74s | 1.02× |
| mandelbrot | 433ms | 425ms | 1.02× |
| fib, pipeline | — | — | unchanged |

## What this says

**Meadow's calls and its channels are good; its arrays are not.** The spread
inside Meadow's own column — 3.4× on `pipeline`, 523× on `matmul` — matters far
more than where the column sits on average.

The three languages worth measuring against are OCaml, Koka and Haskell: compiled
functional languages with a managed heap, which is what Meadow is. Against those,
Meadow is currently **4–5× on `fib`, 5–8× on `binarytrees`, 25× on `wordfreq` and
100× on `matmul`.**

`pipeline` at 3.4× the fastest is the strongest result. It is behind Go, the
language whose reputation rests on this one thing, and behind Rust's `mpsc`, but
ahead of C's mutex and condition variables, ahead of Java and Node, and several
times ahead of GHC's `Chan`. Handing a value between green threads is what the
scheduler in `rts/src/sched.rs` was built for — a thread woken by a message goes
into the receiving worker's non-stealable next slot, so a send and the receive
answering it happen on one core back to back — and it shows.

`fib` at 5× says the calling convention and the native backend are sound. It is
the one task with no allocation, no arrays and no runtime services, so it is the
closest thing here to a measurement of the compiler on its own. The gap to OCaml
and Koka (both 10–11ms) is the cost of allocating a continuation for each of the
two non-tail calls per node.

`matmul` at 523× is the worst number in the suite and has a specific cause.
`St.get` and `St.set` are primitives, and native code does not implement
primitives: it hands them back to the interpreter through `meadow_exec`. Every
element read and written in the innermost loop leaves machine code, does a type
check and a bounds check, and returns. Note that **Koka is 38× off C here too** —
array-heavy numeric code is hard for a reference-counted functional runtime as
well — so the gap Meadow has to close to reach its own weight class is 14×, not
500×.

`contention` at 123× is a comparison of two software transactional memories:
GHC's runs the identical algorithm in 41ms. The remaining cost is now mutex
parking — threads blocking on cell locks whose critical sections are a few
nanoseconds — which wants an adaptive spin-then-park lock.

`wordfreq` at 105× is the persistent HAMT doing the work a mutable hash table
does elsewhere. OCaml's `Hashtbl` at 51ms is the number to aim at.

`binarytrees` is worth reading for its own sake, and it is where **Koka earns its
place in this table**: Perceus reference counting at 333ms is second only to
Java's collector and ahead of GHC, Go and V8, on the benchmark that is nothing
but allocate-walk-discard. Every generational collector with a bump allocator
beats `malloc`/`free` in C and `Box` in Rust by two to five times. The received
wisdom that reference counting cannot compete on allocation-heavy functional code
is not what this row says.

**Go is the most consistent column.** It wins or ties three tasks, is never worse
than 3.6×, and has none of the one-task disasters every other column contains
somewhere.

**Two results are about the other languages, not Meadow.** Python's `wordfreq` at
53ms beats Haskell, Java and Node, because `str.split` and `collections.Counter`
are C and the Python is four lines of glue. And GHC's `Chan` on `pipeline` is a
boxed linked structure with an `MVar` per cell doing what a ring buffer does
elsewhere; it is also the noisiest measurement in the suite, varying between
44ms and 872ms across runs.

## What was fixed

Three changes to the runtime came out of profiling this suite. Each was measured
before and after, and the numbers are in [Results](#results).

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
   other hardware. `pipeline` in Haskell is the one measurement that moves a
   lot between runs — 44ms to 872ms — so read that cell as an order of
   magnitude, not a number.
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
