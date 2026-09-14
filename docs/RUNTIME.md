# How Meadow runs a program

This is a tour of everything below the type checker: the three backends, the
memory model they share, how effects are compiled and run, and how threads run
in parallel. It describes the code as it is, and names the files to read for
the rest. The module docs in those files are the detailed reference; this is
the map.

- [1. One program, three backends](#1-one-program-three-backends)
- [2. From core to bytecode](#2-from-core-to-bytecode)
- [3. The backends](#3-the-backends)
- [4. The memory model](#4-the-memory-model)
- [5. Effects](#5-effects)
- [6. Concurrency and parallelism](#6-concurrency-and-parallelism)
- [7. Where to read next](#7-where-to-read-next)

## 1. One program, three backends

Every backend runs the **same bytecode**. Native code isn't a second compiler
with its own view of the language. It translates the bytecode's instructions
into machine code, does the simple ones itself, and hands the rest back to the
interpreter one instruction at a time. So the heap, the collector, effects and
the scheduler work the same way whichever backend is running. The only
difference is how much of the work happens in machine code.

| backend | what runs | default for |
|---|---|---|
| `vm` | the bytecode interpreter | |
| `jit` | the interpreter, compiling each block to machine code once it has been entered 16 times | `--debug` |
| `aot` | an executable: machine code for every block, the bytecode image, and the runtime, linked together | `--release` |

`--backend`, `--jit` and `--aot` choose a backend for one command, and
`backend = "..."` under `[profile.<name>]` in `meadow.toml` chooses one for a
package. The CEK machine (`--cek`, in `eval/`) is separate. It is the
reference semantics, and every backend is tested against it.

```text
  source ─► parse, rename, infer ─► core
                                     │  meadow-core: specialize, globals, lower
                                     ▼
                                   AxCut (meadow-seq)       seven statements, no expressions
                                     │  meadow-codegen
                                     ▼
                                   bytecode (meadow-bytecode)   8-byte instructions, 256 registers
                                     │
               ┌─────────────────────┼──────────────────────┐
               ▼                     ▼                      ▼
          interpreter        JIT: hot blocks →        AOT: all blocks →
          (rts/src/vm.rs)    machine code in memory   object file + runtime.a
                             (rts/src/jit.rs)         (rts/src/codegen/object.rs)
               └──────── same heap, same scheduler, same natives ────────┘
```

## 2. From core to bytecode

Three decisions made before the runtime sees anything shape everything after.

### There is no call stack

The backend IR is **AxCut**, a sequent calculus with cuts already eliminated
(`compiler/meadow-seq/src/lib.rs`). In AxCut a function receives the
continuation to answer, and *returning is invoking that continuation*. A
closure, a continuation and an effect handler are all the same kind of heap
object: codata with a method table and captured values.

So the bytecode has no `call` or `ret` and no frame pointer. `Op::Invoke` is
the whole calling convention: rebuild the register file as the object's
captures followed by its arguments, then jump to the method. Nothing is pushed
and nothing is popped. Every call is a tail call, and a deep recursion grows the
heap, which is collected, instead of a stack, which would overflow.

The cost is that a call not in tail position has to allocate its continuation
on the heap, at about four words per call. That makes the nursery's bump
allocator the fast path for ordinary function calls.

### A register is a word, and types become descriptors

Registers and object fields hold 64-bit words with no tags. An `Int`, a `Float`
and a heap address look the same. Whoever needs to know what a word is gets
told:

- **Typed instructions** (`AddI`, `CmpF`, `BrIK`, …) are emitted where the
  types are known, and their opcode says what the operands are.
- **Operand descriptors** (`Program::operands`) are recorded beside the
  instructions that need them, such as a generic primitive, `show`, or a
  `halt`.
- **GC maps** (`GcMap`) at every instruction that can collect say which
  registers are live and what each holds: a reference, a scalar, or "whatever
  the descriptor in register *n* says".
- **Object headers** carry a 4-bit descriptor per field (see
  [the object layout](#objects)).

Code that is generic over a type variable doesn't know at compile time whether
an `a` is an address or a number. So in a **debug** build a generic definition
takes one hidden *descriptor* argument per type variable
(`meadow_core::desc`: `REF`, `INT`, `FLOAT`, `STR`, …), passed at each
instantiation. A **release** build specializes instead
(`meadow_core::specialize::release`). It makes one copy of generic code per
*representation*: one for `Int`, one for `Float`, and one shared by every
reference type. After that no descriptors are passed, and arithmetic in
formerly generic code becomes typed instructions. This matters for native code
further down: typed instructions and objects with headers known at compile time
are what the code generator can do inline.

### Effects are gone before codegen

`handle` and `perform` lower to evidence passing: ordinary objects, data and
jumps. The bytecode has exactly one effect instruction, `Op::Native`, for an
operation that no handler answers. [Section 5](#5-effects) covers the details.

## 3. The backends

### The interpreter

`rts/src/vm.rs`. The machine is a program counter, a boxed array of 256
registers plus a scratch area, `live` (how many registers are roots), a heap,
and a pointer to the program. It has no stacks of any kind. Instructions are a
fixed 8 bytes: opcode, three register operands, and a 32-bit immediate.

`Vm::step` decodes and runs one instruction. `Vm::advance`, which is what the
scheduler calls, checks for native code first:

```rust
pub fn advance(&mut self) -> Result<Option<Value>, Error> {
    match self.native.and_then(|n| n.at(self.pc)) {
        Some(f) => crate::abi::enter(self, f),   // run a whole block natively
        None => self.step(),                     // or one instruction
    }
}
```

That one `match` is the whole integration between interpreted and native code.

### Native code: blocks and block functions

`rts/src/abi.rs`, `rts/src/codegen/mod.rs`.

A **block** starts at any pc that control can enter from elsewhere: a
definition, a method entry, or the target of a jump or branch
(`abi::block_entries`). Each block compiles to one native function:

```c
u32 block(Vm *vm);   // System V on every x86-64 OS, AAPCS64 on arm64
```

The function runs from its pc until control leaves, sets `vm->pc`, and returns
a status:

| status | meaning |
|---|---|
| `JUMPED` | control left the block, and `pc` says where |
| `HALTED` | the program finished, and the value is in the machine |
| `FAILED` | it failed, and the error is in the machine |
| `REQUESTED` | a thread operation is waiting for the scheduler |

Native code reaches the machine only through fixed offsets into the `repr(C)`
`Vm` and `Heap` (`codegen::layout`). It reaches the interpreter through a
function pointer stored in the `Vm` (`vm->exec`), and it branches only within
its own function. So **the code needs no relocations**. The same bytes work
whether they sit in an object file or were just copied into memory made
executable. That's what lets the JIT compile one block at a time and put it
anywhere.

#### What is done inline, and what is handed back

| inline, in machine code | handed to the interpreter via `meadow_exec(vm, pc)` |
|---|---|
| `Move`, and `Const` for immediates | `Const` for strings and `BigInt`s |
| typed `Int`/`Float` arithmetic and comparisons | generic `Prim`, `PrimK`, `JumpUnlessPrim(K)` |
| `Jump`, `JumpUnless`, `BrI`/`BrIK`/`BrF` | `Ref` operations, `compact`, STM and thread primitives |
| `JumpUnlessTag`, `Field` and `Invoke` on a **nursery** object | the same on an old-generation or region object |
| `MakeData`/`MakeArray`/`Closure` with a **static header**, if the nursery has room | the same with descriptors from registers, or a full nursery |
| | `MakeRecord`, `Select`, `Extend`, `Native`, `Halt`, `Error` |

The heap instructions use a **fast path with a slow half**. For `field`, for
example, the aarch64 code checks that the address is below `OLD_BASE`, loads
the header, checks the kind and the bounds, and loads the word. Any check that
fails branches to a slow label placed after the function body. There it undoes
the step count, calls `meadow_exec` for that one instruction, and jumps back.
Allocation works the same way: bump `TOP` by the object's size, and if that
passes `CAP` (or the heap has put enough into regions that it wants a
collection first), take the slow path, where the interpreter collects.

`meadow_exec` runs the instruction exactly as the interpreter would and returns
`CONTINUE` if control falls through. Any other status makes the native function
return it at once.

#### Bookkeeping native code keeps up

Native code updates the machine state an instruction changes: the register it
writes, `live` when that write raises it, `pc` when control leaves, and `steps`.
The code generator follows the compiler's `OptLevel`, and every level has the
same observable behavior:

- **O0** is a literal translation, one instruction at a time. Use it to bisect
  a suspected miscompilation.
- **O1** (debug) settles `steps` and `live` only where they can be observed:
  before a call into the interpreter, a return, a branch, or a join
  (`codegen::Book`). A run of arithmetic between two such points costs one
  addition to `steps` and at most one write to `live`. O1 also uses short
  encodings for constant operands.
- **O2** (release) adds two passes. **Regions** pull the loops a block belongs
  to into its function, so a loop that the bytecode lays out as several blocks
  goes round in machine code instead of returning to `advance` at every block.
  **Pins** keep a function's most-used bytecode registers in machine registers
  (x2–x8 on arm64; r8–r11 and r15 on x86-64). Pinned registers are written back
  to memory before every call and return, and reloaded after every call.

A loop inside one function counts its back edges, and after `BACK_EDGES`
(4096) iterations the function returns anyway. That keeps any single call into
native code bounded, so the scheduler always gets control back
([section 6](#preemption)).

Machine registers inside a block function:

| role | arm64 | x86-64 |
|---|---|---|
| the `Vm` | x19 | rbx |
| its register file | x20 | r12 |
| steps not yet added to the `Vm` | x21 | r13 |
| back edges taken this call | x22 | r14 |
| scratch for the current instruction | x9–x17, d0, d1 | rax, rcx, rdx, rsi, rdi, xmm0, xmm1 |
| pinned bytecode registers (O2) | x2–x8 | r8–r11, r15 |

### The JIT

`rts/src/jit.rs`. `Native::jit(program, threshold, opt)` starts with an empty
table: one `AtomicPtr` per pc. Each time `advance` reaches a block entry with
no function yet, it bumps that pc's counter. On the `threshold`th entry
(`MEADOW_JIT_THRESHOLD`, default 16) it compiles the block:

1. It takes the arena lock and checks the table again, since another thread may
   have compiled the block meanwhile.
2. It runs `codegen::compile_block(program, host arch, pc, opt)`.
3. It copies the bytes into executable memory and publishes the address into
   the table with an atomic store.

Code that runs only once, which is most of what a program does at startup, is
never compiled. A loop is compiled within a few iterations. The `opt` level is
the build profile's: O1 for debug, O2 if you run `--release --jit`.

Getting executable memory depends on the platform:

- **macOS** uses one `MAP_JIT` mapping, 1 MiB at a time. Only the thread writing
  it sees it as writable, and only while it writes
  (`pthread_jit_write_protect_np`), so other threads keep running the code
  already there. `sys_icache_invalidate` then flushes the processor's
  instruction cache.
- **Linux** gives each function fresh pages: written, then `mprotect`ed to
  read+execute, with `__clear_cache` on arm64.
- **Windows** does the same with `VirtualAlloc` and `VirtualProtect`, then
  `FlushInstructionCache`.

Code is never patched or freed while the program runs, so a thread executing a
function never races with anything.

### AOT executables

`buildtools/meadow/src/aot.rs`, `rts/src/codegen/object.rs`, `rts/src/aot.rs`.

`meadow build --release` (or `--aot`) compiles every block with
`codegen::compile` and writes an object file (Mach-O, ELF or COFF) with two
symbols:

```text
  meadow_code    the machine code for every block, one after another
  meadow_data    u64 image length │ u64 block count │ u64 table offset
                 the bytecode image
                 (u32 entry pc, u32 code offset) per block
```

A generated `main.c` calls `meadow_aot_main(meadow_code, meadow_data, argc,
argv)`, and the system C compiler links it with `libmeadow_rts.a`.
`meadow_aot_main` decodes the image, fills a `Native` table from the block list
(`Native::ahead_of_time`), and runs the same scheduler as `meadow run`.

So an executable **still contains the interpreter and the bytecode**. Native
code needs them for every instruction it hands back, and for any pc that isn't
a block entry, such as the instruction after a thread operation returns (see
[below](#thread-operations-from-native-code)). What AOT removes is compiling
at run time and interpreting the common instructions.

**The runtime library has to match the code generator.** The generated code
hard-codes the `Vm` layout, so a runtime built from different sources would
corrupt memory. `rts/build.rs` hashes the runtime's sources (plus
`meadow-bytecode` and `meadow-core`), and the library exports a symbol named
`meadow_rts_<hash>`. The generated `main.c` refers to that symbol, so a
mismatched library fails at link time, and `meadow` then tries the next place a
runtime library could be. A release `meadow` embeds its own runtime library and
writes it to `~/.meadow/lib/meadow_rts_<hash>/` the first time it links, so
nothing else needs to be installed.

## 4. The memory model

`rts/src/heap.rs`, `object.rs`, `old.rs`, `mark.rs`, `evacuate.rs`,
`region.rs`.

### One heap per green thread

Every green thread has its **own heap**, and only the OS thread currently
running that green thread touches it. So there is no locking in the allocator
or the collector, and a collection pauses only the thread that owns the heap
([section 6](#no-shared-mutable-heap)). A spawned thread starts with a small
heap (`THREAD_INITIAL`, 1,024 slots) that grows as needed.

Two kinds of memory are outside every heap:

- **Interned strings**, kept for the life of the process. A `String` word is
  the intern id.
- **Compact regions**, which are shared, read-only and reference-counted
  ([below](#compact-regions)).

### Values and addresses

A `Word` is 64 bits. An address is a **slot number**, not a machine pointer,
and its range says which space it points into:

```text
  0 ............ 2^30   nursery          (moves)
  2^30 ......... 2^31   old generation   (Immix blocks, doesn't move while marking)
  2^31 ......... ∞      shared regions   (never move; the same address in every heap)
```

Native code turns a nursery address into memory as `base + addr × 8`, and
checks `addr < 2^30` before using its fast paths.

In Rust, a `Value` is a word together with what it is (`Value::from_bits(word,
desc)` / `value.bits()`). The primitives, `show` and the natives work with
`Value`s. The machine itself stores only words.

<a name="objects"></a>

### Objects

Every slot is one word. An object is a header followed by its fields, and a
field holds only a value's bits:

```text
  a+0   len (32) │ reserved (19) │ uniform (1) │ uniform desc (4) │ kind (8)
  a+1   descriptors of fields 0..8 (32)        │ meta (32)
  a+2   descriptors of fields 8..24, 16 per word -- only when needed
  …
  a+h   field 0
  …
```

`kind` is one of Data, Array, Record, Closure, Ref, MutArray, BigInt, Resume,
Compact, Channel, Task and TVar. What `meta` holds depends on the kind: a
constructor tag, a method table index, a sign, or a region or channel number.

Arrays, mutable arrays and `BigInt`s are **uniform**: one descriptor covers
every element, so the header is two words at any length. Other objects have a
descriptor per field, so the header is two words up to eight fields, plus a
word per sixteen fields after that. Because the collector finds references by
descriptor, one scan loop serves every kind of object. There are no per-kind
tracing rules.

### The one rule: never hold an address across an allocation

Nursery objects move. An address kept anywhere the collector can't see (a Rust
local, or a machine register) is wrong after a collection. So every operation
that allocates follows the same three steps:

1. Measure how much it will allocate.
2. `Vm::ensure(slots)`, which may collect and move everything.
3. Re-read its arguments from registers, which the collector has updated, and
   only then allocate.

The **roots** are registers `r0..live`, filtered by the GC map of the
instruction being run, plus the per-thread globals cache (top-level `def`s
evaluated once per thread, `meadow_core::globals`) and anything a debugger
pinned. `MEADOW_GC_VERIFY=1` overwrites every register the map leaves out, so a
map that forgets a live register fails immediately instead of rarely.

### The nursery

New objects are bump-allocated into the nursery: a bounds check and an
increment. When the nursery fills, it is collected with **Cheney's
algorithm**. The roots are copied to the other half-space, and the copies are
scanned as a queue, copying whatever they reach. The cost is proportional to
what survives, which is usually very little.

- Until a heap reaches its **largest nursery** (`MEADOW_GC_NURSERY`, default
  32,768 slots, or 256 KiB), the nursery simply grows. Most green threads never
  need an old generation and never pay for one.
- After that, an object that has already survived one nursery collection is
  **promoted** to the old generation at the next one. The fixed nursery size
  bounds the nursery pause.
- An object bigger than the whole nursery goes straight into the old
  generation.

### The old generation: Immix, marked concurrently

Promoted objects go into **Immix** blocks of 8,192 slots (64 KiB), each cut into
256 lines of 32 slots (`old.rs`). Space is reclaimed a *line* at a time: after a
marking cycle, any line with no marked object in it is free. Allocation bumps a
cursor through runs of free lines. No object is freed individually, and nothing
is copied to reclaim space.

Each line records an **epoch**, the number of the cycle that last marked it or
allocated into it, instead of a mark bit that would need clearing before every
cycle. So both starting and finishing a cycle cost O(1).

Marking runs **on other OS threads while the program keeps running**
(`mark.rs`), using snapshot-at-the-beginning:

```text
  program thread:  ──run──┤pause 1├──────run──────────────┤pause 2├──run──
                          │ empty nursery,                │ adopt result:
                          │ hand roots to a Job           │ unmarked lines free
  marker pool:            └──────── mark ─────────────────┘
```

- **Allocation is black.** Anything promoted or allocated in the old generation
  during a cycle is marked when it's created.
- **Overwrites are logged.** Only three things are ever mutated: a `Ref`, a
  mutable array element, and a resumption's one-shot flag. Only those writes
  run the SATB barrier, which logs the old value for the marker. Meadow data is
  otherwise immutable, so the barrier almost never runs.
- The marker reads a mutable object's fields under the heap's mutation lock.
- The marker pool is shared by every heap in the process
  (`MEADOW_GC_MARK_THREADS`, default one per four cores). With no pool, a heap
  marks a slice during each nursery pause.

A cycle starts when the old generation has grown by as much as the previous
cycle found alive, or by `MEADOW_GC_TRIGGER` slots (default 262,144), whichever
is larger.

**Evacuation** (`evacuate.rs`) fixes the fragmentation that non-moving marking
leaves behind. After a cycle, the sparsest blocks are chosen, and during the
next cycle every pointer into them is recorded, by the marker and by the
barriers. Between cycles, a pause copies a chosen block's survivors into dense
blocks, rewrites the recorded pointers, and gives the block back.
`MEADOW_GC_EVACUATE=0` turns this off.

**The remembered set.** A nursery collection doesn't scan the old generation,
so it has to be told about old-to-young pointers. There are only three ways to
create one: promoting an object whose field stays young, allocating a large
object directly into the old generation, and writing a `Ref` or mutable array
element or a resumption flag. Each of those records the field.

`MEADOW_GC=copying` swaps in a single growing Cheney space with no generations,
kept as a baseline to compare against.

### Native code and the collector

The JIT and AOT code don't need stack maps, safepoint polls, or any
cooperation with the collector beyond what the interpreter already does:

- **Collection only happens inside the interpreter.** Native allocation bumps
  `TOP` if there's room and otherwise takes the slow path, which calls
  `meadow_exec`, and that call is where a collection can happen. Inline code
  never collects.
- **Before every call into the interpreter**, native code writes pinned
  registers back to the register file and brings `live` up to date. The GC map
  for that pc then describes the roots exactly.
- **After the call**, pinned registers are reloaded from the register file,
  which the collector has updated. Scratch machine registers are never live
  across a call.
- **No native frame survives a block.** A block function returns to `advance`
  whenever control leaves, so the native stack is one frame deep, and a deep
  recursion grows only the heap.

The inline fast paths only read nursery objects and only allocate in the
nursery, so they never need the old generation's mutation lock or barriers.
Anything that writes a mutable object goes to the interpreter, which runs the
barrier.

<a name="compact-regions"></a>

### Compact regions

`Std.Compact` (`region.rs`) copies a structure out of the heap into a
**region**. No heap owns a region, and no collector copies or scans one.
Regions are **closed** (nothing in them points outside) and **immutable once
published**: new objects are only appended, under the region's lock, past
anything an existing address can reach. So any number of threads can read one
without locks or copying.

A heap holds a reference count on each region it has addresses into, and drops
it when a collection finds none left. A region is freed when the last count
goes. `Ref`s, mutable arrays and functions can't be compacted.

### Tuning

| variable | default | effect |
|---|---|---|
| `MEADOW_GC` | `generational` | `copying` for the single-space baseline |
| `MEADOW_GC_NURSERY` | 32768 | largest nursery, in slots, which bounds nursery pauses |
| `MEADOW_GC_TRIGGER` | 262144 | old-generation growth, in slots, before the first cycle |
| `MEADOW_GC_MARK_THREADS` | cores / 4 | marker pool size; 0 means mark in pauses |
| `MEADOW_GC_EVACUATE` | on | `0` turns evacuation off |
| `MEADOW_GC_VERIFY` | off | check every map and every marking cycle (slow) |
| `MEADOW_JIT_THRESHOLD` | 16 | block entries before the JIT compiles a block |
| `MEADOW_THREADS` | cores | OS threads running green threads |

`meadow run --gc-stats` reports collections, pause percentiles, promotion and
marking.

## 5. Effects

`compiler/meadow-seq/src/lower.rs` ("Effects: evidence passing").

**No backend knows effect handlers exist.** The runtime has no handler stack,
no stack capture, and no special instruction for `handle` or `perform`. The
compiler lowers both into closures, data constructors, `Ref`s and jumps. So the
interpreter, the JIT and AOT code all run effects with the same instructions
they use for everything else, and native code accelerates handlers exactly as
much as it accelerates ordinary closures.

### Evidence

Every function takes one hidden parameter: the **evidence**, the list of
handlers in scope where it was called, newest first.

```text
  #ev (key, clause, target, rest)     an ordinary handler clause
  #evt(key, clause, target, rest)     a tail-resumptive clause (see below)
  #evnone                             the empty list
```

- `key` is the operation's name as a string constant, such as `"Log.log"`.
- `clause` is a closure whose one method is the clause body.
- `target` is a `Ref` holding the continuation that the whole `handle`
  expression's value goes to.
- `rest` is the evidence outside this handler.

A continuation doesn't take evidence as a parameter. It captures the evidence
it needs like any other variable, which is why resuming a continuation needs no
evidence restored.

### `handle body with { clauses }`

Answering continuation `k`, this builds:

```text
  target = Ref k                         where the handle's value goes
  for each clause c:
      obj_c   = closure(c's body, capturing its free variables)
      ev      = #ev(key_c, obj_c, target, ev)    (or #evt for a tail clause)
  H = closure: read target; run the return clause (or pass x through) there
  run body with evidence ev, answering H
```

The clauses and `H` capture the evidence from *outside* the handler, so an
effect performed inside a clause skips past this handler, as it should.

### `perform E.op x`

Each `perform` site gets a small lifted block that walks the evidence:

```text
  <perform Log.log>(ev, x, k):
      switch ev
        #ev (key, clause, target, rest) | #evt(…) →
            if key == "Log.log" then dispatch
            else <perform Log.log>(rest, x, k)
        #evnone → Native Log.log x, answering k       ← no handler: the runtime's
```

The innermost handler is almost always the one that matches, and it is one tag
test and one comparison away. The search allocates nothing.

### Dispatch: tail-resumptive clauses

Most clauses resume straight away with a value computed without using the
resumption, as in `ask () k -> k 10`. `tail_resumptive` recognizes the shape
`op x k -> k e`, where `k` isn't free in `e`. For those clauses the entry is
`#evt`, and dispatch simply **invokes the clause with `x` and the performing
code's own continuation `k`**. The clause computes `e` and returns it to `k`.
No resumption is built, no flag is set, and nothing is written, so this costs
the same as calling a closure.

This is sound because `k e` would have sent its result to wherever the clause's
value goes, and that's the same place.

### Dispatch: general clauses and one-shot resumptions

For any other clause, such as `log m k -> 1 + k ()`:

```text
  flag   = Once ()                          a Kind::Resume object: [taken = false]
  R      = closure [flag, k, target] with the method "resume" below
  kh     = GetRef target                    the handle's current destination
  invoke clause(x, R) answering kh
```

and `R`'s method, called as `k v` with its own continuation `c`, does:

```text
  if TakeOnce flag has already been taken → fail "continuation resumed more than once"
  SetRef target := c                        the handle's value now goes back to this resume site
  invoke k(v)                               continue the performing code
```

Writing `c` into `target` is what makes handlers **deep** and makes `k v`
*return* inside the clause. When the resumed body eventually finishes, it
invokes `H`, and `H` reads `target`. That now holds `c`, the rest of the clause
after its `k v`, so `1 + k ()` gets its answer and the clause carries on. This
is safe only because a resumption runs **at most once**. Resuming twice is a
runtime error, and multi-shot continuations aren't supported.

A worked trace for the `counted` handler in the tutorial:

```text
  handle work () with { log m k -> 1 + k (), return x -> 0 }

  target := main's continuation
  work () → perform log "step one"
      → evidence hit (#ev) → R1, clause("step one", R1) answering main
      clause computes 1 + R1 ()  → the "1 + _" continuation c1 is built
          R1: flag taken; target := c1; resume work
  work () → perform log "step two"
      → R2, clause("step two", R2) answering c1 (read from target)
          clause: 1 + R2 () → c2; R2: target := c2; resume work
  work () returns 42 → H: read target = c2, return clause gives 0
      → c2: 1 + 0 = 1 → c1: 1 + 1 = 2 → main
```

The flag and `target` are mutable, so writing them goes through the same
barrier as any `Ref` write ([section 4](#the-old-generation-immix-marked-concurrently)).
That's one of the three kinds of write that ever run it.

### Operations nobody handles

When the evidence runs out, the site falls into `Op::Native`: `r[a] ←
ops[imm](r[b])`. The runtime answers it in `rts/src/native.rs` and `vm.rs`.

| effect | answered by the runtime |
|---|---|
| `Console` | stdin and stdout, or a debugger's protocol stream |
| `Fs` | files, directories and metadata |
| `Process` | running commands, `argv`, the environment, `exit` |
| `Random` | a splitmix64 generator per OS thread, seeded from the clock |
| `Time` | wall and monotonic clocks, `sleep` |
| `Test.fail` | a failure with the shown message |

Anything else is the run-time error `unhandled effect E.op`. A native's result
is often a tree, such as `Ok(Just(#[1, 2, 3]))`, and building one object at a
time would hold addresses across allocations. So a native describes its result
as a `Build` (plain Rust data), and `Vm::build` measures the whole tree, makes
room once, and materializes it bottom-up.

Mutable references are primitives (`NewRef`, `GetRef`, `SetRef`), not handled
operations. `Mut` appears only in types.

### Effects on native code

Everything above is ordinary instructions, so the native backend runs effects
with the fast paths from [section 3](#what-is-done-inline-and-what-is-handed-back):

- The evidence walk's `switch` on `#ev`/`#evt`/`#evnone` is an inline
  `JumpUnlessTag` when the evidence is in the nursery.
- The key comparison is a `JumpUnlessPrimK` against a string constant, which
  goes to the interpreter.
- Building evidence entries, clause closures and resumptions uses inline
  nursery allocation when their headers are static.
- Invoking a clause, `H` or a resumption is an inline `Invoke`.
- `NewRef`/`GetRef`/`SetRef`, `Once`/`TakeOnce` and `Native` go to the
  interpreter.

So a tail-resumptive operation costs, in native code, the evidence walk, one
interpreted string comparison and one closure call. A general clause adds the
flag, the resumption closure, and three interpreted `Ref`-style primitives.
Doing the key comparison and the `Ref` primitives inline would be the obvious
next steps for effect-heavy code.

### Effects and threads

A spawned thread starts with `#evnone` evidence. Its type allows only the
effects the runtime answers (`Console`, `Fs`, `Process`, `Random`, `Time`,
`Test`, `Mut`, `Thread`), and it must handle any others itself. A resumption
can't cross threads: it captures its `Resume` flag first, and sending or
compacting a value refuses a `Resume` object as a continuation.

**STM** (`lib/Std/src/Stm.mw`, `rts/src/stm.rs`) is the same idea split in two.
`atomically`, `retry` and `orElse` are handlers written in Meadow. The runtime
supplies what a handler can't: `TVar`s every thread can see (their values live
in shared regions), a per-transaction read/write log, and a commit that checks
a global clock and publishes all the writes under one lock.

## 6. Concurrency and parallelism

`rts/src/sched.rs`, `lib/Std/src/Thread.mw`.

### A green thread is a value

A green thread is a `Vm`: registers, a pc, `live`, a heap, and its globals
cache. Because the machine has no call stack, that is **all** of it. A thread
that isn't running is a boxed struct sitting in a queue, and any OS thread can
pick it up and continue. Nothing about a thread is tied to the OS thread that
last ran it: no native stack, no thread-local storage, and no suspended native
frames. This holds equally for native code, since a block function has always
returned before its thread is suspended.

### The scheduler: M:N with work stealing

- **Workers.** `MEADOW_THREADS` OS threads (default: one per core). The calling
  thread is worker 0. The others start at the first `spawn`, so a program that
  never spawns never creates an OS thread.
- **Run queues.** Each worker has its own queue. A thread that a worker spawns
  or wakes, or one that used up its time slice, goes on that worker's queue, so
  its heap stays in that core's cache.
- **The `next` slot.** A thread woken by a message or an `await` result goes
  into the worker's unstealable `next` slot and runs as soon as the current
  thread stops. A send and the receive it satisfies then run back to back on
  one core.
- **Stealing.** An idle worker takes half of a random victim's queue, from the
  end that runs last. A worker is woken to steal only when some queue holds more
  than its owner is about to run.
- **Fairness.** A shared global queue is checked first every `GLOBAL_EVERY`
  (61) turns.
- **Locks** are per queue, per channel and per task handle. Workers that aren't
  touching the same things never wait on each other.

```text
  worker 0                 worker 1                 worker 2 (idle)
  ┌────────┐ next: T7      ┌────────┐               ┌────────┐
  │ T1 ▶   │               │ T4 ▶   │               │ steal  │── takes half of
  ├────────┤               ├────────┤               └────────┘   worker 0's queue
  │ T2 T3 T5 T6 …          │ T8                         
  └──────────────          └────                     global: T9 (every 61 turns)
```

<a name="preemption"></a>

### Slices and preemption

A worker runs a thread for `SLICE` (2048) calls to `Vm::advance`, then puts it
at the back of its queue. On the interpreter, a call runs one instruction. On
native code, a call runs one block function: at least one instruction, and at
most a whole loop nest, bounded by the `BACK_EDGES` (4096) iteration limit per
call. Preemption therefore happens only at block boundaries, never in the middle
of machine code. A native slice can run far more instructions than an
interpreted one, but it is always bounded, so a loop that never waits can't
starve other threads.

<a name="thread-operations-from-native-code"></a>

### Thread operations, from any backend

`spawn`, `await`, `yieldNow`, `newChannel`, `send`, `receive`, STM commit and
STM wait are **primitives**. A primitive can't carry out a thread operation
itself, because the channels and other threads belong to the scheduler. So it
leaves a `Request` in the machine and stops:

```text
  native block ──► meadow_exec(pc of `receive`) ──► primitive sets vm.request
       ◄── REQUESTED ◄──────────────────────────────┘
  block function returns REQUESTED ──► advance returns ──► run_slice sees the request
  worker settles it:
      send / spawn / newChannel / commit  → keep running the same thread
      receive on empty channel / await on unfinished thread / retry
                                          → park it with what it waits on
  whoever wakes it attaches a Wake (a parcel, a handle, a Bool) that is
  written into the thread's destination register when it runs again
```

Native code needs no special support for this. The thread operation is just
another instruction handed to the interpreter, and `REQUESTED` is just another
reason to return. When the thread resumes, its pc is the instruction after the
primitive. If that isn't a block entry, the interpreter runs from there until
it reaches one, and native code takes over again.

**Every thread shares one native table.** The JIT's `Native` is borrowed by
every `Vm` in the run, and its table entries are atomics. A block compiled
because one thread made it hot becomes native for all threads, and it is
compiled only once however many threads reach the threshold together.

<a name="no-shared-mutable-heap"></a>

### No shared mutable heap: parcels

Heaps are never shared, so a value moves between threads in exactly three
ways: the function a thread is spawned with, a message on a channel, and the
result `await` returns. Each is a **copy**:

1. The sending thread **exports** the value into a `Parcel`
   (`Heap::export`). The export walks everything reachable, like a Cheney scan,
   and lays the objects out contiguously with addresses relative to the
   parcel. Sharing inside the value is preserved.
2. A parcel holds no heap addresses, so it can sit in a channel or a task
   result without belonging to any heap.
3. The receiving thread **imports** it when it next runs
   (`Heap::import`): one block copy, then rebasing the references.

The export refuses a `Ref`, a mutable array or a continuation (a `Resume` flag)
with a runtime error. Immutable data copies invisibly. **Compact regions and
`TVar` values aren't copied at all**: the parcel keeps their addresses, which
are the same in every heap, and holds a reference count on the region. This is
how `examples/parallel` lets eight threads read one prime table.

The rule has three consequences:

- **No locks in the machine.** Allocation, field reads and writes, and `Ref`
  updates never contend.
- **Independent collection.** Each worker collects the heap of the thread it is
  running, whenever that heap needs it, without stopping other threads. The only
  process-wide GC state is the marker pool, which takes jobs from any heap, and
  region reference counts.
- **The same semantics everywhere.** The CEK machine shares immutable values
  instead of copying them, which no program can distinguish, and every engine
  refuses the same values with the same messages (`meadow_core::thread`).

### Ending, failing and deadlock

- The program ends when `main` does. Threads still running then are dropped.
- A failure in a thread fails only that thread. `await` on it fails with the
  same message, and it can be awaited any number of times.
- The scheduler counts threads that are running or queued. Only a running
  thread can wake a waiting one, so if the count reaches zero while `main` is
  unfinished, the scheduler reports a deadlock.
- A panic inside the runtime stops every worker instead of hanging the run
  (`StopOnPanic`).

### Parallelism, summarized

| layer | how it uses cores |
|---|---|
| green threads | M:N over `MEADOW_THREADS` workers, with work stealing |
| native code | shared and immutable once published; every worker runs the same functions |
| JIT compilation | on the thread that makes a block hot, under one lock |
| allocation and nursery collection | per thread, no locks |
| old-generation marking | a process-wide marker pool, concurrent with the program |
| shared data | compact regions and `TVar`s, read in place by every thread |
| communication | copy-on-send parcels over channels, `await` results |

## 7. Where to read next

| topic | file |
|---|---|
| AxCut, the IR | `compiler/meadow-seq/src/lib.rs` |
| lowering, evidence passing, descriptors | `compiler/meadow-seq/src/lower.rs`, `describe.rs` |
| register allocation, GC maps, typed ops | `compiler/meadow-codegen/src/lib.rs` |
| the instruction set and image format | `compiler/meadow-bytecode/src/lib.rs`, `image.rs` |
| specialization | `compiler/meadow-core/src/specialize.rs` |
| the interpreter | `rts/src/vm.rs`, `prims.rs` |
| the native ABI | `rts/src/abi.rs` |
| the code generator | `rts/src/codegen/mod.rs`, `a64.rs`, `x64.rs` |
| object files | `rts/src/codegen/object.rs` |
| the JIT | `rts/src/jit.rs` |
| AOT entry and linking | `rts/src/aot.rs`, `buildtools/meadow/src/aot.rs`, `rts/build.rs` |
| heap, nursery, remembered set | `rts/src/heap.rs`, `object.rs` |
| old generation, marking, evacuation | `rts/src/old.rs`, `mark.rs`, `evacuate.rs` |
| regions and STM | `rts/src/region.rs`, `stm.rs` |
| the scheduler | `rts/src/sched.rs` |
| unhandled effects | `rts/src/native.rs` |
| what may cross threads | `compiler/meadow-core/src/thread.rs`, `compact.rs`, `stm.rs` |
