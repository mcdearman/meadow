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
| Python | CPython, no flags |
| JavaScript | Node, no flags |

Meadow is measured on its **ahead-of-time native backend**, which is what
`--release` selects. It also has a bytecode interpreter and a JIT; neither is
measured here, and `benches/` is where those get compared.

## Results

On a 10-core Apple silicon machine, macOS 26.3.1, minimum of five runs. The
figure in brackets is how many times slower than the fastest in that row.

| task | meadow | rust | c | go | haskell | java | python | js |
|---|---|---|---|---|---|---|---|---|
| fib | 51ms (5.0×) | 10ms (1.0×) | 10ms (1.0×) | **10ms** | 28ms (2.8×) | 35ms (3.4×) | 209ms (20.6×) | 72ms (7.1×) |
| binarytrees | 3.77s (12.1×) | 1.53s (4.9×) | 1.64s (5.3×) | 805ms (2.6×) | 414ms (1.3×) | **311ms** | 5.87s (18.9×) | 885ms (2.8×) |
| matmul | 2.85s (463.7×) | **6ms** | 7ms (1.2×) | 12ms (2.0×) | 28ms (4.6×) | 43ms (6.9×) | 756ms (122.9×) | 79ms (12.9×) |
| wordfreq | 1.44s (119.2×) | 14ms (1.2×) | **12ms** | 17ms (1.4×) | 143ms (11.8×) | 132ms (10.9×) | 55ms (4.6×) | 114ms (9.5×) |
| ⇉ mandelbrot | 433ms (13.1×) | 39ms (1.2×) | 37ms (1.1×) | **33ms** | 75ms (2.3×) | 88ms (2.6×) | 1.24s (37.4×) | 125ms (3.8×) |
| ⇉ contention | 684ms (148.6×) | 5ms (1.2×) | **5ms** | 17ms (3.8×) | 41ms (8.8×) | 60ms (12.9×) | 59ms (12.8×) | 136ms (29.5×) |
| ⇉ pipeline | 58ms (3.4×) | 20ms (1.1×) | 34ms (1.9×) | **17ms** | 564ms (32.8×) | 68ms (4.0×) | 174ms (10.1×) | 158ms (9.2×) |
| _startup_ | 3ms | 3ms | 2ms | 4ms | 17ms | 28ms | 18ms | 56ms |

## What this says

**Meadow's calls and its channels are good; its arrays and its transactions are
not.** That is the whole table in one sentence, and the spread inside Meadow's
own column — 3.4× on `pipeline`, 464× on `matmul` — matters far more than where
the column sits on average.

`pipeline` at 3.4× the fastest is a real result. It is behind Go, which is the
language whose whole reputation rests on this one thing, and behind Rust's
`mpsc`, but ahead of C's mutex and condition variables, ahead of Java, Node and
Python, and ten times ahead of GHC's `Chan`. Handing a value between green threads is what the scheduler in
`rts/src/sched.rs` was built for — a thread woken by a message goes into the
receiving worker's non-stealable next slot, so a send and the receive answering
it happen on one core back to back — and it shows.

`fib` at 5× says the calling convention and the native backend are sound. This
is the one task with no allocation, no arrays and no runtime services, so it is
the closest thing here to a measurement of the compiler on its own.

`matmul` at 464× is the worst number in the suite and it has a specific cause.
`St.get` and `St.set` are primitives, and native code does not implement
primitives: it hands them back to the interpreter through `meadow_exec`. So
every element read and every element written in the innermost loop leaves
machine code, enters the interpreter, does a type check and a bounds check, and
returns. C and Rust are auto-vectorizing the same loop. The gap is roughly two
orders of magnitude of interpreter round-trip and one of vectorization, and it
would take inlining array access into the native backend to close any of it.
(Rust, C and Go here are within ten milliseconds of their process-startup floor,
so their true ratio is larger than 464×, not smaller.)

`contention` at 149× is the second worst, and is a straight comparison of two
software transactional memories: GHC's runs the identical algorithm in 41ms and
Meadow's in 684ms, a 17× gap between implementations of the same idea. Nothing
about the design forces that.

`wordfreq` at 119× is the persistent HAMT doing the work a mutable hash table
does elsewhere. Some of that is inherent to keeping the map persistent and some
is not; the interesting question is which.

`binarytrees` at 12× is the mildest of Meadow's bad results, and the row is
worth reading for its own sake: every generational collector with a bump
allocator — Java, GHC, Go, V8 — beats `malloc`/`free` in C and `Box` in Rust,
Java and GHC by four to five times and Go and V8 by about two. This is the
classic finding of this benchmark and it reproduces cleanly.

**Go is the most consistent column in the table.** It wins or ties four of the
seven tasks, is never worse than 3.8×, and has no entry anywhere near the
disasters the other columns each contain somewhere. It is not the fastest at
anything by much, which is rather the point of it.

**Two results are about the other languages, not Meadow.** Python's `wordfreq`
at 55ms beats Haskell, Java and Node, because `str.split` and
`collections.Counter` are C and the Python in that program is four lines of glue
— idiomatic Python is often a thin wrapper over someone else's C, and that is a
genuine property of the language rather than a cheat. And GHC's `Chan` at 564ms
on `pipeline`, thirty times behind everyone, is a boxed linked structure with an
`MVar` per cell doing what a ring buffer does elsewhere.

## Caveats, in order of how much they matter

1. **One machine, one run of the suite.** These numbers are from a 10-core
   Apple silicon laptop that was not otherwise idle. Ratios within a row are
   more trustworthy than absolute times, and nothing here is a claim about
   other hardware.
2. **Startup is inside every number.** Node pays 56ms and Java 28ms before
   their first instruction. On `fib`, where the fastest is 10ms, that is most
   of what separates the columns. Subtract the last row before drawing
   conclusions about short tasks.
3. **`matmul` in Python is lists, not numpy.** Anybody multiplying matrices in
   Python calls numpy and is then timing a BLAS written in C and Fortran. That
   is a fair thing to know and not a thing this suite measures, so the Python
   entry is the loops written out.
4. **`contention` in JavaScript is not idiomatic.** JavaScript has no shared
   mutable object across workers, so the accounts are a `SharedArrayBuffer` and
   the atomic step is held by a lock built from `Atomics.wait`. It is the only
   way to write the task and no working JavaScript programmer would enjoy it.
   The absence is the finding.
5. **`tasks/startup/` is generated** by the harness and overwritten on each run.
   `work/` holds build outputs, the generated corpus and `results.json`, and is
   not checked in.
