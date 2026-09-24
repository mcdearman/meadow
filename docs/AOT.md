# The native backend: AxCut to LLVM, with reference counting

`meadow build --runtime aot` compiles a program with a backend of its own, for
production: the program is compiled all the way to machine code by LLVM, runs
on the native stack, and manages its memory by reference counting, as the
AxCut paper does. There is no bytecode, no interpreter and no tracing
collector in the executable.

The memory discipline is the paper's:

> Philipp Schuster, Marius Müller, Klaus Ostermann and Jonathan Immanuel
> Brachthäuser. _Compiling Classical Sequent Calculus to Stock Hardware: The
> Duality of Compilation._ Proc. ACM Program. Lang. 9, OOPSLA1, Article 142
> (April 2025). <https://doi.org/10.1145/3720507>. Artifact:
> <https://github.com/se-tuebingen/oopsla-2025-artifact-axcut>, whose
> `axcut/source/X86_64/Coder.idr` is the reference for what each statement
> emits.

Where this document says "the paper", it means that, and where Meadow departs
from it the reason is given. The default `--runtime rts` is the other way
round -- bytecode, native code for the blocks the code generator can do, the
interpreter for the rest, and the collector the JIT and the debugger share --
and stays as it is, so the two can be measured against each other on the same
program.

This document is the design the implementation follows. Where the
implementation is not there yet, [Stages](#stages) says so.

## Pipeline

```text
core ──meadow_seq::lower──▶ AxCut ──linearize──▶ linear AxCut ──emit──▶ LLVM IR (.ll)
                                                                          │ clang -O2
                                                    libmeadow_aot ──link──▶ executable
```

- `meadow_seq::lower` is shared with the bytecode backend: the same AxCut.
- **Linearize** (`compiler/meadow-llvm/src/linear.rs`) makes every copy and
  every drop of a reference explicit, which is the paper's discipline. See
  [Linearity](#linearity).
- **Emit** (`compiler/meadow-llvm/src/emit.rs`) writes LLVM IR as text. clang
  optimizes, compiles and links it; no LLVM library is linked into `meadow`.
- **The runtime** (`aot/`, the static library `meadow_aot`) is the heap, the
  primitives, the effects that reach the world, stack segments, and the
  scheduler. It does not depend on `meadow-rts`.

Primitives reach the runtime by one of three roads, and which one decides
what a loop costs. **Inline**: arithmetic and comparison, the length of an
array or a string, indexing an array, and reading or writing a `Ref` are
emitted as instructions, with the bounds check where one is needed
(`emit::inline_prim`). **An entry of its own**: `hash`, `==`, `stringIndexOf`,
`stringSlice` and a store of a reference into a mutable array take their
arguments in registers (`emit::direct`). **The generic road**: everything else
goes through `meadow_prim`, which takes its arguments in memory and decodes
them from their descriptors.

Where the line falls is worth measuring rather than guessing. `binarytrees`
written against an arena spends one `Ref` write per node on its bump counter:
as a call into the runtime that benchmark took 1.36s, and emitted inline it
takes 314ms.

## Values

A value is a 64-bit word, represented as `meadow_seq::Rep` says: `Int`, `Float`
(its bits), an immediate (`()`, `Bool`, `Char`, sized integers, `Float32`, an
interned name's key), or a **reference** -- data, a closure, a string, an
array, a record, a `BigInt`, a `Ref`. Only references are counted. A value of a
type variable's type is counted when its descriptor, held in another name at
run time, says it is a reference.

In the paper a value of data or codata type is two words: a memory block,
which is null when there are no fields, and a tag or a method table. So a
nullary constructor and a closure that captures nothing allocate nothing, and
a `switch` reads its tag from a register. Meadow keeps one word per value --
its generic code, its descriptors and the runtime's primitives all assume one
-- and gets the same effect by tagging the word:

| word          | is                                                                                                                        |
| ------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `0`           | nothing: a continuation that returns natively (see [Frames](#frames-are-the-native-stack))                                |
| odd           | an object with no block: `tag << 1 \| 1` for a nullary constructor, `table << 1 \| 1` for a closure that captures nothing |
| even, not `0` | the address of a block                                                                                                    |

Counting skips `0` and odd words, as the paper's `share` and `erase` skip a
null block.

## Blocks

```text
word 0   extra references: u32 (0: this is the only one) | length: u32
word 1   kind: u8 | flags: u8 | spare: u16 | meta: u32
         meta: a data value's tag, a closure's method table, a string's
         length in bytes
words    field descriptors, four bits each, sixteen to a word -- absent for a
         uniform block (an array, a string), whose one descriptor is in flags
fields
```

The count is the paper's: the number of references _besides_ one, so a fresh
block has 0 and "is this the last reference?" is a compare against zero. The
descriptors are what erasing a block needs to know which fields to release,
and what printing needs to know how to show them. A closure's `meta` indexes
the program's method tables.

The paper has one size of block, and chains an object with more fields than a
block holds through the block's last field. Meadow has arrays and strings,
which want indexing in constant time, so blocks come in **size classes**
instead, one object to a block. Literals -- strings, big integers -- are made
once at start-up and held by a global that is never erased.

An array's block has **room to grow**: an array of `n` elements is given
`heap::array_room(n)` slots -- `n` itself up to 8, then the next of 12, 16, 24,
32, 48, ... -- so at most a third is spare. The room is a function of the
length alone, so no block records it and the free path works it out as it
does a block's size. `arrayPush` and `arrayConcat` **consume** the array they
grow (see `emit::consumes`): the linearization gives them a reference of their
own, sharing it first where the old array is still wanted, so a count of zero
inside them means nobody else can see the array, and the new elements go into
its room in place. Building an array a push at a time is linear, where copying
at every push made it quadratic.

### Acquiring and releasing, lazily

Freeing is the paper's, and it is what makes every operation constant work: no
erase ever walks a structure.

- **Erasing** a block with other references decrements its count. Erasing the
  last reference puts the block on the _pending_ list **with its fields still
  in it** -- they are not erased yet.
- **Acquiring** a block for a `let` or a `new` first erases some fields of a
  pending block -- at most a fixed number, so a big array is worked through
  across many acquisitions -- and a pending block whose fields are all erased
  is clean. Then it takes a clean block of its size class if there is one,
  working through more of the pending list while there is not, and fresh
  memory last.
- **Loading** fields in a `switch` or an `invoke` (the paper's `release`):
  when the block's count is 0, the fields move out -- nobody else can see them
  -- and the block is clean at once, with no counts touched; otherwise the
  count is decremented and each field loaded is shared.

The paper erases a block's old fields when the block is reused, which with one
size of block means every freed block's fields are erased in the end. With
size classes a block could wait for a class nothing allocates from, holding
what it points to -- so the pending list is one list for every class, and
every acquisition works on it: the lag between a structure becoming garbage
and its memory being reusable stays bounded, and no single operation does
more than a constant amount.

## Linearity

After linearization every name is used exactly once. In the paper's terms (its section on memory management, and `Coder.idr`'s
`codeWeakeningContraction`): a `substitute` that names a variable _n_ times
shares it _n_ − 1 times, and one that leaves it out erases it -- contraction
and weakening. What each statement then does:

| statement           | consumes                                                                                     |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `substitute [x, y]` | the whole environment: names it repeats are shared, names it leaves out are erased           |
| `let z = K(x, y)`   | its fields, which are stored in an acquired block                                            |
| `switch x`          | the scrutinee: an arm loads its fields (release, above)                                      |
| `new f {…} [x]`     | its captures, stored in an acquired block                                                    |
| `invoke f#m`        | `f`: the method loads its captures (release)                                                 |
| `jump L`            | the environment, handed to `L`                                                               |
| `extern p(x)`       | nothing: a primitive borrows its arguments; ones not needed after are erased once it returns |

Meadow's AxCut reads what it names rather than consuming it (see
`meadow_seq`'s module docs), and its `substitute` does not list every name, so
the linearization pass does the paper's accounting at every statement: it
shares a name where it is used again after something consumes it, and erases
it where it is not used again at all. After the pass the program is linear,
and each statement consumes as in the table.

## Cycles

Counting misses a cycle, and this runtime cannot have the usual backstop: a
tracing collector needs the roots, and the roots are in native frames and
registers with no map saying which slots hold references. What needs no roots
is **trial deletion** -- Bacon and Rajan, _Concurrent Cycle Collection in
Reference Counted Systems_, ECOOP 2001 -- and `aot/src/cycles.rs` implements
it: buffer each block whose count goes down without reaching zero, then mark
its subgraph gray while subtracting the references that come from inside it,
scan to restore everything an outside reference still holds, and free what is
left white.

**Emitted only where it can matter.** A cycle needs a store of a reference
into a block that already exists, which is `setRef` or a write into a mutable
array; everything else Meadow builds is built bottom-up and points only
backwards, and `setField` -- which destination-passing uses to fill a hole --
writes something newer than what it writes into. `emit::ties_knots` looks for
those two primitives _and at what they store_, since `Std` builds every string
it prints in a mutable array of bytes and a byte cannot point at anything. A
program without one defines `@meadow_cycles` as zero, its counting helpers
have no candidate test in them, and its runtime never buffers, colours or
collects anything.

**What a program that does pay, pays.** The test before the call is inline and
is one mask and one compare against a constant: a block is worth keeping only
if it is a `Ref` or a mutable array -- marked as such in word 1 when it is
built -- and only the first time its count goes down, since it stays coloured
until a run looks at it. Both questions live in the same word, so both are
asked at once.

**Pauses.** A run takes 64 candidates and walks at most 6,000 blocks; past
that the mark pass stops and puts back exactly what it took, which is possible
because it recorded every block it coloured and undoing is that walk
backwards. Once started the collector keeps going, one bounded run per
allocation, until the buffer is empty -- the program runs between one run and
the next. That holds a pause under a millisecond: 0.35 ms while freeing 10.4M
blocks of small cyclic garbage, 0.18 ms freeing 1.0M.

A cycle _larger_ than the budget is the exception, and it is inherent: what a
run proves garbage is freed in that run or the proof is lost, so one cycle of
100,000 blocks is one pause of about 11 ms. Such a candidate is set aside and
walked with no budget only once the heap has doubled, so the pause is paid for
by a doubling's worth of allocation and the memory is bounded rather than held
to exit -- 111 MB on a program that makes 640 MB of giant cyclic garbage.
Bounding it properly would mean marking incrementally, which means a write
barrier, which this runtime does not have.

**What is still not caught.** Only a `Ref` and a mutable array are buffered.
A cycle whose `Ref` is let go of while the cycle is still live, and which
becomes garbage later when something that is not a `Ref` is let go of, is
never noticed and leaks as every cycle did before. Buffering every decremented
block -- the published algorithm -- closes that hole and costs 11% of
`wordfreq` and 27% of `binarytrees` on programs that make no cycles at all.

## Calling convention

Every block that is entered from elsewhere -- a definition, a method -- is an
LLVM function in the `ghccc` convention, taking its parameters (the
environment) as `i64`s and returning `i64`. Everything inside it -- a
`switch`'s arms, an `extern`'s continuations, a `substitute` -- is basic blocks
and SSA renaming: `substitute` emits no moves at all.

A `jump` and an `invoke` are `tail call`s. `ghccc` has no callee-saved
registers and passes everything in registers, so LLVM makes every such call a
jump -- on Windows too, where `tailcc` with `musttail` is not supported. It
passes ten arguments in registers; a block with more takes the rest from
`meadow_spill`, a thread-local area the caller fills just before the jump.

Reference counting is calls to small helpers the module defines (`mw.share`,
`mw.erase`, and forms that look at a descriptor first), which LLVM inlines where
it pays: emitting the checks at every site made the standard library's module
several times the size.

A method's function takes the object itself first, then the arguments, and
loads its captures from the object -- releasing it, as above. The method knows
how many captures it has and so how its block is laid out, where the call site
knows neither; the paper's `invoke` is likewise a jump through the method
table, with the loading done by the method's code.

### Frames are the native stack

`meadow_seq` marks the continuation a function makes for its own non-tail call
as a **frame**. Where a frame is used for nothing but the continuation of the
one call it was made for, it is never built: the call is an LLVM `call`, the
frame's captures are values live across it, and the frame's method is the code
after it, with the call's result as its argument. The callee receives a null
continuation -- _return natively_ -- and invoking that is `ret`. A function
reached by a tail call passes its own continuation on, so a `ret` anywhere
returns to the nearest native call.

A frame used any other way -- captured, stored -- is built as an ordinary
object, and invoked through its method table as any other continuation is.

A frame's code is one function (`mw.F<n>`) that each call site tail-calls
after its native `call` returns, so a frame reached from many places is
emitted once.

The native stack runs deep: `main` runs on a segment whose reservation is large
(committed as it is touched), since non-tail recursion over a long list is
ordinary Meadow.

## Effects: stack segments

Effects arrive as evidence passing (see `meadow_seq::lower`): a tail-resumptive
clause is an ordinary call, and a general one uses four primitives -- `Enter`
where `handle` begins, `Detach` where an operation captures the continuation up
to its handler, `Reattach` where the resumption puts it back, and
`Once`/`TakeOnce` for one-shotness.

On the native stack those are switches between **stack segments**:

- `Enter` runs the handled body on a segment of its own. When the body returns,
  the segment is finished and control is back on the handler's.
- `Detach` suspends the body's segment where it is, and runs the rest of the
  operation -- building the resumption, calling the clause -- on the handler's
  segment. The code after it is compiled as a function of its own, which the
  runtime calls there.
- `Reattach` switches back to the suspended segment, and runs the code after it
  there -- which is where invoking the operation's continuation is a `ret` into
  the frames the segment kept.

Switching is `corosensei`'s: stackful coroutines on Windows x64, Linux x64 and
aarch64, with guard pages, and on Windows the thread information block kept in
step so that stack probes work.

The emitted code packs the code after each of the three as a closure of one
argument (what it captures: the names free in it), calls `meadow_enter`,
`meadow_detach` or `meadow_reattach` with it, and returns natively with what
the runtime answers. The runtime calls such a closure back through
`meadow_invoke1`, which the module defines, since methods are in `ghccc`.

**A segment ends where its `handle` does.** In AxCut the code after a `handle`
is part of the continuation the code inside it is given, so a segment told to
run "what follows" would run the rest of the program, and handlers in a loop
would pile up a stack each. What the lowering does say is where the value of
the whole `handle` goes: it puts that continuation in the target `Ref` before
`Enter`. So `meadow_enter` takes it out and leaves `0` -- return natively --
in its place. When the value is produced, whichever stack produces it returns,
the segment is over, and the runtime carries on with the continuation it took,
on the stack the `handle` began on. Before this, a program that handled in a
loop slowed down as it went, with an unfinished segment for every turn.

**An abandoned continuation leaks what its frames hold.** A continuation never
resumed is discarded when its stack object is erased, without unwinding: the
native frames on its segment have no maps saying which slots hold references.
This is bounded by what the aborted computation was holding, as a cycle is.

## Threads

A green thread is a segment too (`aot/src/sched.rs`), and `main` is one. The
scheduler runs the ready threads in turn, each until it waits: on a thread it
awaits, on an empty channel, in a transaction's `retry`, or by yielding. A
thread that waits inside a `handle` suspends every segment it has, out to the
scheduler, as an operation performed further out does, and resumes as one.
A failure in a thread other than `main` fails only that thread; `await` fails
with its message.

Threads run **M:N over OS threads**: a worker per core takes ready threads off
one queue, and a thread runs on whichever worker picks it up. The workers
besides the one `main` starts on begin with the first `spawn`, so a program
that never spawns never has a second OS thread.

**A heap per thread** (`aot/src/ctx.rs`), so counting needs no atomics and no
two workers ever touch the same block. What crosses between threads is copied
(`aot/src/parcel.rs`): the function a thread is spawned with and everything it
captures, a message, a thread's answer, a `TVar`'s value. Sharing inside a
value is kept, so what was a DAG arrives as one. A `Ref`, a mutable array or a
continuation is refused, with the message `meadow-rts` gives.

**Waiting.** A thread that waits says why in its context and suspends, and
the worker files it where what it waits for will find it, or puts it straight
back if that is already there. So a wake-up cannot fall between a thread
deciding to wait and its waiting.

**A lock per thing, not one for everything.** A channel has its own, as a
`TVar` does, and its handle holds the address of it, so a send and the receive
it answers touch that channel and nothing else; the scheduler's lock is taken
only to put a woken thread back on the queue. The order is always the
channel's or the `TVar`'s first and the scheduler's after, never the other way
round, which is the whole of what keeps them from holding each other up --
so a worker files a parked thread before it takes the scheduler's lock, not
under it.

**Transactions** keep a version per `TVar`, which a commit bumps. A read of
one written since the transaction began is a conflict, and the transaction
runs again; a commit checks that every version it read is as it was and
publishes its writes, having locked the cells it touches -- in address order,
so two commits cannot hold each other up. Transactions over different `TVar`s
never meet at all.

A `TVar`'s handle holds the address of the cell, so reading or writing one is
no lookup; a handle copied to another thread carries the same address. What it
holds, when that is a word and nothing more -- an `Int`, a `Bool` -- is kept
as it is rather than as a parcel, so a transaction over numbers allocates
nothing to commit.

**Stacks are pooled.** A segment's or a thread's stack is reserved memory with
guard pages, which is most of what making one costs, so a finished one is kept
to be used again: a few per thread for segments, a couple per core for
threads. A program whose every transaction is a `handle` spent eight times
longer making stacks than working before this.

What a thread's context holds, it reaches through one accessor that is never
inlined (`ctx::get`). A thread that waits can carry on on another OS thread,
and an address that a compiler kept from before the wait would be the old OS
thread's. For the same reason the emitted code asks the runtime for the spill
area rather than reading a thread-local of its own.

`meadow run -j N` (`--threads`) sets how many workers there are, for either
runtime; unset, there is one per core. `MEADOW_THREADS` says the same, and
`meadow run --leaks` asks the `aot` runtime for what the program left behind
at exit, as `--gc-stats` asks the VM about its collector.

**Preemption.** A thread that neither waits nor finishes gives way at a
**safe point**: a byte the emitted code reads at the entry of every definition
and every method, which is to say on every loop, since a loop in AxCut is a
jump to one of them. A timer sets the byte every few milliseconds, and only
while a thread is waiting for a core, so the usual answer is a load and a
branch not taken; the thread that acts on it clears it, and goes to the back
of the queue. The load is `volatile`: LLVM turns a self-tail-call into a loop,
and an ordinary load of a byte nothing in the loop writes would be hoisted out
of it, which is exactly the loop that has to be interrupted.

The thread that starts the program waits for `main`'s answer rather than
running threads itself. Otherwise a worker still inside a thread that never
waits would hold the answer up, and the program would end no sooner than that
thread did.

## A program that cannot spawn pays for none of this

Whether a program can ever have a second thread is plain in the program: it
spawns one somewhere, or it never does, and `meadow_llvm::emit` looks before
it writes anything. A program that never spawns gets:

- **no safe points** -- nothing can be waiting for a core, so the check and
  its branch are not emitted at all;
- **a spill area of its own**, an ordinary global, rather than something the
  runtime looks up per thread;
- **one context in a static**, with no thread-local and no lookup: reading it
  is a load, and it can be inlined, since nothing can move between OS threads;
- **no scheduler**: `main` runs on the thread that called it, and no worker,
  timer or coroutine is made for it.

The emitted module says which it is in `@meadow_threaded`, and the runtime
reads it.

Counting itself is never atomic, in either kind of program: a block belongs to
one thread's heap, and what crosses between threads is copied, so there is no
block two threads can count at once. The only atomics are the scheduler's own
bookkeeping -- what `await` has to hand back, what a channel holds, what a
`TVar` holds -- which is shared by nature.

## Testing

- **Differential:** every program the backend compiles is run by the VM as
  well, and must answer alike -- the same harness `meadow-rts` has, on the
  standard library's tests and the examples.
- **Leaks:** a debug runtime counts live objects, and a program without
  `Ref`s, mutable arrays or `TVar`s must end with none.
- **A/B:** `meadow build --release --runtime rts` against `--runtime aot`, on
  the benchmarks.

## Stages

1. **The core, single-threaded:** linearization; data, closures, frames on the
   native stack; `Int`, `Float` and immediates; the arithmetic and comparison
   primitives; printing an answer.
2. **The rest of the values:** strings, arrays, records, `BigInt`, `Ref`s, and
   the primitives over them.
3. **Effects:** stack segments, `Enter`/`Detach`/`Reattach`, and the effects
   that reach the world (`Console`, `Fs`, `Process`, `Time`, `Random`,
   `Test.fail`).
4. **Threads:** spawning, channels, STM, the scheduler.
5. **Parity:** the standard library's tests and every example agree with the
   VM; then the A/B against `--runtime rts`.

All five are done: the standard library's 281 tests pass under
`meadow test --std --release --runtime aot`, and the examples print what they
print under `--runtime rts`, but for timings and the size of a compact region.

A program is emitted as several LLVM modules of about 2 MB each (see
`emit::Module::units`), compiled in parallel and linked: as one module, the
standard library's tests took clang tens of gigabytes.
