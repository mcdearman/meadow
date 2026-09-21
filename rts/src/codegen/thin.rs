//! **What a fat instruction is made of**: the layer between the bytecode and an
//! architecture.
//!
//! The bytecode is an interpreter's instruction set. One `Op::Prim2` covers a
//! hundred primitives, and `Op::MakeData` builds an object, because a dispatch
//! costs more than the work when the work is small -- so the fewer and fatter
//! the instructions, the faster the interpreter.
//!
//! A compiler wants the opposite. It can only compile what it can see through,
//! and a fat instruction is opaque: [`crate::codegen`] either has a method that
//! emits the whole of one, hand-written per architecture, or it hands the
//! instruction back to the interpreter and native code stops being native.
//! That is why a matrix multiply used to spend 62% of its time outside the
//! machine code compiled for it, and now spends under 1%.
//!
//! The two wants are not reconcilable in one instruction set, so this is a
//! second, thinner one underneath. A fat instruction [`expand`]s into a run of
//! [`Step`]s -- loads, stores, arithmetic and guards over the heap and the
//! register file -- written **once**, for every architecture, and read by the
//! backends instead of the fat instruction.
//!
//! # What a step can say
//!
//! Less than you might expect, on purpose. There are no branches and no loops:
//! a run of steps is straight-line, and the only way out of it is a [`Guard`]
//! that does not hold, which abandons the whole run and hands the instruction
//! to the interpreter after all. So a run either does the instruction or does
//! nothing, and an architecture needs no control flow to emit one -- which is
//! what keeps the per-architecture code small enough to be worth having.
//!
//! [`Guard`]: Step::Guard
//!
//! # Why this is safe to add
//!
//! A run of steps is a second implementation of an instruction's meaning, and
//! two implementations disagree sooner or later. So [`Machine`] runs a run of
//! steps directly, on a real heap, and the tests compare it against what the
//! interpreter does with the same instruction and the same heap. The expansion
//! is checked against the thing it is an expansion of, without an assembler in
//! the way.

use crate::heap::{Heap, Kind};
use crate::value::{Addr, Word};
use meadow_bytecode::{Cond, Instr, Program, Reg};

/// Slots of header an object with one descriptor for all its fields has. Such
/// an object -- an array, a mutable array -- is the only kind a step reads, so
/// the header size is a constant here rather than something to compute.
pub const UNIFORM_HEADER: u64 = 2;

/// Where in an object's first word each part lives. See [`crate::object`].
pub const KIND_BITS: u64 = 0xFF;
pub const UNIFORM_BIT: u32 = 12;
pub const LEN_SHIFT: u32 = 32;

/// What a step reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    /// A register of the bytecode machine -- which is to say a position in the
    /// AxCut environment, still in the order the IR put it in.
    Reg(Reg),
    /// A value made earlier in this run. Numbered from zero per run, and dead
    /// at the end of it, so an architecture can keep them wherever it likes.
    Tmp(u8),
    Imm(u64),
}

/// One thin instruction. Everything is words: a step has no idea what a value
/// means, only where its bits are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `t = src`
    Set(u8, Src),
    /// `t = a + b`, wrapping, on raw words.
    Add(u8, Src, Src),
    /// `t = a & mask`
    And(u8, Src, u64),
    /// `t = a >> n`, unsigned.
    Shr(u8, Src, u32),
    /// The heap word at slot `at`. Slots, not bytes: an [`Addr`] indexes the
    /// heap as an array of words, which is what the machine holds it as.
    Load(u8, Src),
    /// `heap[at] = v`
    Store(Src, Src),
    /// `t = ` where slot `at` is in memory: a machine address, found through
    /// the block tables once, from which the rest of the object is an offset.
    /// That holds across every object: one in the nursery, which is one
    /// space; one in a block, which never crosses its end; and one bigger
    /// than a block, which is laid out in a run of blocks in one allocation
    /// (see `crate::old`'s `Mem`).
    Locate(u8, Src),
    /// `t = mem[base + off]`, `base` a machine address from [`Step::Locate`]
    /// and `off` in slots.
    LoadAt(u8, Src, Src),
    /// `mem[base + off] = v`, likewise.
    StoreAt(Src, Src, Src),
    /// Unless `a cond b` as **unsigned** words, abandon the run.
    ///
    /// Unsigned is not an accident. An index arrives as a signed `Int`, and a
    /// negative one read as unsigned is enormous, so a single `u < len` rejects
    /// both a negative index and one past the end.
    Guard(Cond, Src, Src),
    /// `r[dst] = src`: the instruction's answer, and the last step of a run.
    Put(Reg, Src),
}

/// The steps a fat instruction is made of, if this is one that has any.
///
/// `None` means the instruction has no expansion and the interpreter should be
/// asked for it, exactly as before. Every instruction starts that way and stops
/// being that way one at a time, each with a test.
pub fn expand(program: &Program, pc: usize, i: Instr) -> Option<Vec<Step>> {
    use meadow_core::Prim;
    match i.op {
        // One argument and a result: a length, or what a cell holds.
        meadow_bytecode::Op::Prim1 => match program.prims.get(i.imm as usize)? {
            Prim::GetRef => Some(ref_get(i.a, i.b)),
            Prim::StArrayLen => Some(length(i.a, i.b, Kind::MutArray)),
            Prim::ArrayLen => Some(length(i.a, i.b, Kind::Array)),
            Prim::StringByteLength => Some(meta_of(i.a, i.b, Kind::Str)),
            _ => None,
        },
        // Reading an element of an array, mutable or not. Both are two
        // arguments and a result, so both are an `Op::Prim2`, and the kind
        // guard is what tells them apart at run time.
        meadow_bytecode::Op::Prim2 => match program.prims.get(i.imm as usize)? {
            Prim::StGetArray => Some(element(i.a, i.b, i.c, Kind::MutArray)),
            Prim::ArrayGet => Some(element(i.a, i.b, i.c, Kind::Array)),
            _ => None,
        },
        // Writing one. Three arguments, so a windowed `Op::Prim` whose
        // registers are `b`, `b + 1`, `b + 2`. A value the compiler knows to
        // be a reference is stored only into a young array; anything else,
        // into an array of non-references.
        meadow_bytecode::Op::Prim if i.c == 3 => match program.prims.get(i.imm as usize)? {
            Prim::StSetArray => {
                let value = program.operands(pc).get(2).copied();
                Some(
                    if value == Some(meadow_core::desc::REF as meadow_bytecode::DescSrc) {
                        element_set_young(i.a, i.b, i.b + 1, i.b + 2)
                    } else {
                        element_set(i.a, i.b, i.b + 1, i.b + 2)
                    },
                )
            }
            _ => None,
        },
        _ => None,
    }
}

/// `r[dst] = ` what the cell in `r[obj]` holds. A cell has one field after a
/// two-word header.
pub fn ref_get(dst: Reg, obj: Reg) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::region::REGION_BASE as u64)),
        Step::Locate(1, Tmp(0)),
        Step::LoadAt(2, Tmp(1), Imm(0)),
        Step::And(2, Tmp(2), KIND_BITS),
        Step::Guard(Cond::Eq, Tmp(2), Imm(Kind::Ref as u64)),
        Step::LoadAt(2, Tmp(1), Imm(2)),
        Step::Put(dst, Tmp(2)),
    ]
}

/// `r[dst] = ` how many elements the `kind` in `r[obj]` has: its header's
/// length. Exact about the kind, as [`element`] is: an array of bytes is
/// packed and its header counts words.
pub fn length(dst: Reg, obj: Reg, kind: Kind) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::region::REGION_BASE as u64)),
        Step::Load(1, Tmp(0)),
        Step::And(2, Tmp(1), KIND_BITS),
        Step::Guard(Cond::Eq, Tmp(2), Imm(kind as u64)),
        Step::Shr(2, Tmp(1), LEN_SHIFT),
        Step::Put(dst, Tmp(2)),
    ]
}

/// `r[dst] = ` the `meta` of the `kind` in `r[obj]`: the low half of its
/// second header word, which for a string is its length in bytes.
pub fn meta_of(dst: Reg, obj: Reg, kind: Kind) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::region::REGION_BASE as u64)),
        Step::Load(1, Tmp(0)),
        Step::And(2, Tmp(1), KIND_BITS),
        Step::Guard(Cond::Eq, Tmp(2), Imm(kind as u64)),
        Step::Add(2, Tmp(0), Imm(1)),
        Step::Load(2, Tmp(2)),
        Step::And(2, Tmp(2), 0xFFFF_FFFF),
        Step::Put(dst, Tmp(2)),
    ]
}

/// `r[dst] = ` element `r[idx]` of the `kind` in `r[obj]`.
///
/// The guards, in the order they are cheapest to fail: the object is in the
/// heap rather than a compact region, it is the kind expected, it keeps one
/// descriptor for every element, and the index is inside it. A `Kind::Bytes` array keeps eight elements to a
/// word and is not this shape, which the uniform-descriptor guard does not
/// catch -- so the kind guard is exact rather than a range.
pub fn element(dst: Reg, obj: Reg, idx: Reg, kind: Kind) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    // A region is the one thing ruled out by address: its blocks are looked up
    // by a search rather than indexed, and it is immutable anyway. The
    // generations are both reached through the block table an architecture
    // reads a slot with, so neither needs a guard of its own -- which is what
    // made this worth turning on. See `crate::heap::Heap::tables`.
    //
    // Five temporaries, and they are reused: an architecture has only a handful
    // of scratch registers, and a run that wants more of them than there are
    // would have to spill -- which would cost more than the interpreter it is
    // here to avoid. 0 is the object, 1 its header, 3 the length, 4 the index,
    // and 2 is whatever is needed at the time.
    // 0 is the object, 1 where it is in memory, 3 the length, 4 the index,
    // and 2 is whatever is needed at the time.
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::region::REGION_BASE as u64)),
        Step::Locate(1, Tmp(0)),
        Step::LoadAt(2, Tmp(1), Imm(0)),
        Step::Shr(3, Tmp(2), LEN_SHIFT),
        Step::And(2, Tmp(2), KIND_BITS | 1 << UNIFORM_BIT),
        Step::Guard(Cond::Eq, Tmp(2), Imm(kind as u64 | 1 << UNIFORM_BIT)),
        Step::Set(4, R(idx)),
        Step::Guard(Cond::Lt, Tmp(4), Tmp(3)),
        Step::Add(4, Tmp(4), Imm(UNIFORM_HEADER)),
        Step::LoadAt(2, Tmp(1), Tmp(4)),
        Step::Put(dst, Tmp(2)),
    ]
}

/// Where the element descriptor of a uniform object lives in its first header
/// word. See [`crate::object`].
pub const DESC_SHIFT: u32 = 8;
pub const DESC_BITS: u64 = 0xF;

/// Element `r[idx]` of the mutable array in `r[obj]` `= r[val]`, and `r[dst]`
/// the unit the primitive answers with.
///
/// One guard more than [`element`]: the elements must not be **references**.
/// That is what makes a bare store safe, and each of the three things it skips
/// needs it:
///
/// * the marker holds the heap's mutation lock while it reads a mutable
///   object's fields, because a mutable object's *descriptors* change as its
///   fields do -- but a uniform array has one descriptor for all of them and
///   [`Heap::set_field_of`] will not let it change, so there is nothing to
///   race over;
/// * the snapshot barrier records the old value of an overwritten reference,
///   and a non-reference is not one;
/// * the remembered set records an old slot that came to hold a young address,
///   and a non-reference is never an address.
///
/// So an array of numbers is written with a store, and an array of anything
/// else goes to the interpreter, which does all three.
pub fn element_set(dst: Reg, obj: Reg, idx: Reg, val: Reg) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::region::REGION_BASE as u64)),
        Step::Locate(1, Tmp(0)),
        Step::LoadAt(2, Tmp(1), Imm(0)),
        Step::Shr(3, Tmp(2), LEN_SHIFT),
        Step::And(0, Tmp(2), KIND_BITS | 1 << UNIFORM_BIT),
        Step::Guard(
            Cond::Eq,
            Tmp(0),
            Imm(Kind::MutArray as u64 | 1 << UNIFORM_BIT),
        ),
        Step::Shr(2, Tmp(2), DESC_SHIFT),
        Step::And(2, Tmp(2), DESC_BITS),
        Step::Guard(Cond::Ne, Tmp(2), Imm(meadow_core::desc::REF as u64)),
        Step::Set(4, R(idx)),
        Step::Guard(Cond::Lt, Tmp(4), Tmp(3)),
        Step::Add(4, Tmp(4), Imm(UNIFORM_HEADER)),
        Step::StoreAt(Tmp(1), Tmp(4), R(val)),
        // `stSetArray` answers unit, whose word is zero.
        Step::Put(dst, Imm(0)),
    ]
}

/// [`element_set`] for a value that is a reference, which may be stored with
/// a bare store into a **young** array, and only there. What the three
/// things a store into an old array has to do are for -- the mutation lock,
/// the snapshot barrier and the remembered set -- is the marker reading old
/// objects while the program writes them, and old slots coming to hold young
/// addresses. A young array is read by no marker (the nursery is a root,
/// walked when a cycle starts and at every nursery collection, never
/// concurrently), and a young slot is not an old one. A uniform array's
/// descriptor does not change with a store either, so there is nothing else
/// to keep in step.
pub fn element_set_young(dst: Reg, obj: Reg, idx: Reg, val: Reg) -> Vec<Step> {
    use Src::{Imm, Reg as R, Tmp};
    vec![
        Step::Set(0, R(obj)),
        Step::Guard(Cond::Lt, Tmp(0), Imm(crate::old::OLD_BASE as u64)),
        Step::Locate(1, Tmp(0)),
        Step::LoadAt(2, Tmp(1), Imm(0)),
        Step::Shr(3, Tmp(2), LEN_SHIFT),
        Step::And(2, Tmp(2), KIND_BITS | 1 << UNIFORM_BIT),
        Step::Guard(
            Cond::Eq,
            Tmp(2),
            Imm(Kind::MutArray as u64 | 1 << UNIFORM_BIT),
        ),
        Step::Set(4, R(idx)),
        Step::Guard(Cond::Lt, Tmp(4), Tmp(3)),
        Step::Add(4, Tmp(4), Imm(UNIFORM_HEADER)),
        Step::StoreAt(Tmp(1), Tmp(4), R(val)),
        Step::Put(dst, Imm(0)),
    ]
}

/// The most temporaries any expansion uses, which is how many scratch
/// registers an architecture has to keep for them.
pub const TEMPORARIES: usize = 5;

/// How many temporaries a run uses, which is what an architecture has to find
/// room for.
pub fn temporaries(steps: &[Step]) -> usize {
    let mut most = 0;
    for s in steps {
        if let Step::Set(t, _)
        | Step::Add(t, _, _)
        | Step::And(t, _, _)
        | Step::Shr(t, _, _)
        | Step::Load(t, _)
        | Step::Locate(t, _)
        | Step::LoadAt(t, _, _) = s
        {
            most = most.max(*t as usize + 1);
        }
    }
    most
}

/// A run of steps, carried out directly.
///
/// Not for running programs -- the interpreter and the compiled code do that.
/// This is what the tests compare an expansion against, so that a run of steps
/// is checked against the instruction it expands, with no assembler in the way.
pub struct Machine<'h> {
    pub heap: &'h mut Heap,
    pub regs: &'h mut [Word],
    tmps: [Word; 16],
}

impl<'h> Machine<'h> {
    pub fn new(heap: &'h mut Heap, regs: &'h mut [Word]) -> Self {
        Machine {
            heap,
            regs,
            tmps: [0; 16],
        }
    }

    fn read(&self, s: Src) -> Word {
        match s {
            Src::Reg(r) => self.regs[r as usize],
            Src::Tmp(t) => self.tmps[t as usize],
            Src::Imm(w) => w,
        }
    }

    /// Run `steps`. `false` means a guard did not hold and nothing was written
    /// to a register -- the caller should ask the interpreter instead.
    pub fn run(&mut self, steps: &[Step]) -> bool {
        for s in steps {
            match *s {
                Step::Set(t, a) => self.tmps[t as usize] = self.read(a),
                Step::Add(t, a, b) => {
                    self.tmps[t as usize] = self.read(a).wrapping_add(self.read(b))
                }
                Step::And(t, a, m) => self.tmps[t as usize] = self.read(a) & m,
                Step::Shr(t, a, n) => self.tmps[t as usize] = self.read(a) >> n,
                Step::Load(t, at) => {
                    self.tmps[t as usize] = self.heap.word_at(self.read(at) as Addr)
                }
                Step::Store(at, v) => {
                    let (at, v) = (self.read(at) as Addr, self.read(v));
                    self.heap.put_word_at(at, v);
                }
                Step::Locate(t, at) => {
                    self.tmps[t as usize] = self.heap.machine_addr(self.read(at) as Addr) as u64
                }
                // Safety: a machine address `Locate` answered, and an offset
                // within the object there -- which is what the guards before
                // the step establish, and what the tests hand it.
                Step::LoadAt(t, base, off) => {
                    let p = (self.read(base) as *const Word).wrapping_add(self.read(off) as usize);
                    self.tmps[t as usize] = unsafe { *p };
                }
                Step::StoreAt(base, off, v) => {
                    let p = (self.read(base) as *mut Word).wrapping_add(self.read(off) as usize);
                    let v = self.read(v);
                    unsafe { *p = v };
                }
                Step::Guard(c, a, b) => {
                    let (x, y) = (self.read(a), self.read(b));
                    let holds = match c {
                        Cond::Eq => x == y,
                        Cond::Ne => x != y,
                        Cond::Lt => x < y,
                        Cond::Le => x <= y,
                        Cond::Gt => x > y,
                        Cond::Ge => x >= y,
                    };
                    if !holds {
                        return false;
                    }
                }
                Step::Put(r, a) => self.regs[r as usize] = self.read(a),
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    /// Run `steps` against a heap, answering the word they put in `r[dst]`, or
    /// `None` if a guard sent the run to the interpreter.
    fn run(heap: &mut Heap, regs: &mut [Word], steps: &[Step], dst: Reg) -> Option<Word> {
        let taken = Machine::new(heap, regs).run(steps);
        taken.then(|| regs[dst as usize])
    }

    /// A mutable array of `n` ints, `i` holding `i * 7`.
    fn ints(heap: &mut Heap, n: usize) -> Addr {
        let fields: Vec<Value> = (0..n).map(|i| Value::Int(i as i64 * 7)).collect();
        heap.reserve(Heap::size_of(Kind::MutArray, n));
        heap.alloc(Kind::MutArray, 0, &fields)
    }

    /// The expansion answers what `Heap::field` answers, for every element.
    ///
    /// This is the whole point of [`Machine`]: the steps are checked against
    /// the thing they are an expansion of, on a real heap, with no assembler
    /// in the way.
    #[test]
    fn an_element_is_what_the_heap_says_it_is() {
        let mut heap = Heap::new();
        let n = 40;
        let a = ints(&mut heap, n);
        let steps = element(2, 0, 1, Kind::MutArray);
        for i in 0..n {
            let mut regs = [a as Word, i as Word, 0];
            let got = run(&mut heap, &mut regs, &steps, 2).expect("inside the array");
            let want = heap.field(a, i).bits();
            assert_eq!(got, want, "element {i}");
        }
    }

    /// A heap whose nursery is too small for the arrays below, so that an
    /// array allocated in it goes straight to the old generation -- which is
    /// what happens to a real one of any size.
    fn old_heap() -> Heap {
        Heap::with_config(
            64,
            crate::heap::GcConfig {
                nursery: 64,
                ..crate::heap::GcConfig::from_env()
            },
        )
    }

    fn in_old(heap: &mut Heap, n: usize) -> Addr {
        let a = ints(heap, n);
        assert!(a >= crate::old::OLD_BASE, "not in the old generation");
        a
    }

    /// The case this was switched off for. An old object is not in the flat
    /// array the nursery is, so reading one used to be the one thing a run of
    /// steps could not do -- and the arrays a matrix multiply is made of are
    /// exactly the ones too big to stay young.
    #[test]
    fn an_element_of_a_promoted_array_is_what_the_heap_says_it_is() {
        let mut heap = old_heap();
        let a = in_old(&mut heap, 64);
        let steps = element(0, 1, 2, Kind::MutArray);
        for i in 0..64u64 {
            let mut regs = [0, a as Word, i];
            assert_eq!(
                run(&mut heap, &mut regs, &steps, 0),
                Some(heap.field(a, i as usize).bits()),
                "element {i}"
            );
        }
    }

    /// An array larger than a block takes several, and they are separate
    /// allocations with nothing contiguous about them -- so the block is
    /// looked up per slot rather than once per object, and an element in the
    /// third block of an array has to come out right.
    #[test]
    fn an_array_of_several_blocks_is_read_a_block_at_a_time() {
        let n = 3 * crate::old::BLOCK;
        let mut heap = old_heap();
        let a = in_old(&mut heap, n);
        let steps = element(0, 1, 2, Kind::MutArray);
        // The first element of each block, and the last of the array.
        for i in [0, crate::old::BLOCK, 2 * crate::old::BLOCK, n - 1] {
            let mut regs = [0, a as Word, i as Word];
            assert_eq!(
                run(&mut heap, &mut regs, &steps, 0),
                Some(heap.field(a, i).bits()),
                "element {i}"
            );
        }
    }

    /// A store writes what the heap would have written, and nothing else in
    /// the object moves.
    #[test]
    fn a_written_element_is_what_the_heap_would_have_written() {
        for mut heap in [Heap::new(), old_heap()] {
            let a = ints(&mut heap, 64);
            let steps = element_set(0, 1, 2, 3);
            for i in 0..64u64 {
                let mut regs = [9, a as Word, i, (i as Word) * 1000 + 1];
                assert_eq!(
                    run(&mut heap, &mut regs, &steps, 0),
                    Some(0),
                    "answers unit"
                );
            }
            for i in 0..64 {
                assert_eq!(heap.field(a, i), Value::Int(i as i64 * 1000 + 1));
            }
        }
    }

    /// An array of references is the case a bare store may not take: the
    /// collector follows those fields, so overwriting one needs the barriers
    /// the interpreter runs and a step does not.
    #[test]
    fn an_array_of_references_is_left_to_the_interpreter() {
        let mut heap = Heap::new();
        let inner = ints(&mut heap, 2);
        let fields = vec![Value::Obj(inner); 4];
        heap.reserve(Heap::size_of(Kind::MutArray, 4));
        let a = heap.alloc(Kind::MutArray, 0, &fields);
        let steps = element_set(0, 1, 2, 3);
        let mut regs = [9, a as Word, 1, inner as Word];
        assert_eq!(run(&mut heap, &mut regs, &steps, 0), None, "gives up");
    }

    /// An index outside the array abandons the run rather than reading
    /// something else. A negative index is a very large unsigned one, which is
    /// why the guard is unsigned and why this covers both.
    #[test]
    fn an_index_outside_the_array_gives_up() {
        let mut heap = Heap::new();
        let n = 8;
        let a = ints(&mut heap, n);
        let steps = element(2, 0, 1, Kind::MutArray);
        for i in [n as i64, n as i64 + 1, 1_000_000, -1, -1000, i64::MIN] {
            let mut regs = [a as Word, i as Word, 0xDEAD];
            assert_eq!(run(&mut heap, &mut regs, &steps, 2), None, "index {i}");
            assert_eq!(regs[2], 0xDEAD, "nothing written for index {i}");
        }
    }

    /// The kind guard is exact. A `Kind::Array` is not a `Kind::MutArray`, and
    /// the run that expects one gives up rather than reading the other.
    #[test]
    fn another_kind_gives_up() {
        let mut heap = Heap::new();
        let n = 4;
        let mutable = ints(&mut heap, n);
        heap.reserve(Heap::size_of(Kind::Array, n));
        let frozen = heap.alloc(Kind::Array, 0, &[Value::Int(1); 4]);

        let want_mut = element(2, 0, 1, Kind::MutArray);
        let want_arr = element(2, 0, 1, Kind::Array);
        let mut regs = [frozen as Word, 0, 0];
        assert_eq!(run(&mut heap, &mut regs, &want_mut, 2), None);
        let mut regs = [mutable as Word, 0, 0];
        assert_eq!(run(&mut heap, &mut regs, &want_arr, 2), None);

        // And each reads its own.
        let mut regs = [frozen as Word, 0, 0];
        assert!(run(&mut heap, &mut regs, &want_arr, 2).is_some());
        let mut regs = [mutable as Word, 0, 0];
        assert!(run(&mut heap, &mut regs, &want_mut, 2).is_some());
    }

    /// A string keeps eight elements to a word, so it is not the shape a step
    /// reads however uniform its descriptor is. The kind guard catches it.
    #[test]
    fn a_packed_array_gives_up() {
        let mut heap = Heap::new();
        heap.reserve(64);
        let s = heap.alloc_bytes(b"abcdefghij");
        let steps = element(2, 0, 1, Kind::Array);
        let mut regs = [s as Word, 0, 0xDEAD];
        assert_eq!(run(&mut heap, &mut regs, &steps, 2), None);
        assert_eq!(regs[2], 0xDEAD);
    }

    /// Whatever the expansion is, an architecture has to know how many
    /// temporaries to find room for, and they are numbered from zero.
    #[test]
    fn a_run_says_how_many_temporaries_it_needs() {
        let steps = element(2, 0, 1, Kind::MutArray);
        let most = temporaries(&steps);
        assert!(most > 0 && most <= TEMPORARIES, "{most}");
        for s in &steps {
            if let Step::Set(t, _) | Step::Load(t, _) = s {
                assert!((*t as usize) < most);
            }
        }
    }

    /// Descriptors are the collector's business, not a step's: a step moves
    /// bits. This holds the expansion to that -- an array of addresses reads
    /// back the same bits as an array of integers does.
    #[test]
    fn a_step_moves_bits_and_does_not_read_them() {
        let mut heap = Heap::new();
        let inner = ints(&mut heap, 2);
        heap.reserve(Heap::size_of(Kind::MutArray, 3));
        let refs = heap.alloc(Kind::MutArray, 0, &[Value::Obj(inner); 3]);

        let steps = element(2, 0, 1, Kind::MutArray);
        let mut regs = [refs as Word, 1, 0];
        let got = run(&mut heap, &mut regs, &steps, 2).expect("inside");
        assert_eq!(got, inner as Word, "the address, as bits");
    }
}
