//! **Linear AxCut to LLVM IR**, as text.
//!
//! One LLVM function per block entered from elsewhere -- a definition, a
//! method -- in LLVM's GHC convention, taking the environment as `i64`s and
//! answering an `i64`. Everything else is basic blocks: a `switch`'s arms and a
//! primitive's continuations branch, and a renaming emits nothing. `jump` and
//! `invoke` are tail calls with every argument in a register: see [`REGS`] for
//! why that convention, and what becomes of an argument past the tenth.
//!
//! A **frame** (`docs/SILO.md`, "Frames are the native stack") is not built
//! while it is only waiting to be the continuation of a call: the call is an
//! LLVM `call` with a null continuation, and the frame's method is emitted
//! after it. Anything else done with a frame -- sharing it, storing it --
//! builds it then, as the object it would have been.
//!
//! Memory is the paper's (see `docs/SILO.md`): a block's first word counts the
//! references besides one; sharing adds to it, erasing takes from it or, at
//! zero, hands the block to the runtime's free list; loading fields out of a
//! block whose count is zero takes them without touching a count.

use crate::linear::{L, LBlock, SwitchArm};
use meadow_core::desc;
use meadow_core::{Lit, Prim};
use meadow_seq::{Extern, Name, Program, Rep, VarId};
use std::collections::HashMap;
use std::fmt::Write;
use std::rc::Rc;

/// What a block's second word says it is. Shared with the runtime, which
/// must agree: see `silo/src/heap.rs`.
pub mod kind {
    pub const DATA: u64 = 1;
    pub const CLOSURE: u64 = 2;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { msg: msg.into() })
}

/// A name's value, where the emitted code has it.
#[derive(Debug, Clone)]
enum V {
    /// An `i64` operand: a register (`%t3`, `%a0`) or a constant.
    Val(String),
    /// A frame not built (see the module docs).
    Frame(Rc<Frame>),
}

#[derive(Debug)]
struct Frame {
    /// Which, so that its code is written once however it is entered.
    id: usize,
    captured: Vec<V>,
    method: LBlock,
}

/// A descriptor, as the emitted code has it.
#[derive(Debug, Clone, PartialEq)]
enum D {
    Known(i64),
    /// In this register, at run time: a value of a type variable's type.
    Dyn(String),
}

/// Whether the program spawns a thread anywhere. Known before anything is
/// emitted, because a program that cannot spawn is emitted differently: no
/// safe points, and a spill area of its own. See [`Module::units`].
fn spawns(program: &Program) -> bool {
    uses(program, &[Prim::ThreadSpawn])
}

/// Whether the program can tie a knot: whether it stores into a block that
/// already exists. Everything else Meadow builds is built bottom-up and
/// points only at what was there before it, so no cycle can come of it --
/// `setRef` and a write into a mutable array are the two ways to make one,
/// and a program with neither needs no cycle collector at all (see
/// `silo/src/cycles.rs`).
///
/// `setField`, which destination-passing uses to fill a hole in a structure
/// being built, is not one of them: what it writes is always newer than what
/// it writes into, so it cannot point backwards.
///
/// What is stored matters as much as where. A store of an `Int`, a `Float` or
/// a `Word` cannot make a cycle whatever it is stored into, and that is not a
/// detail: `Std` builds every string it prints in a mutable array of bytes,
/// so taking the representation into account is what lets an ordinary program
/// -- one that prints, and ties no knots -- have no collector in it at all.
fn ties_knots(program: &Program) -> bool {
    knots(program, &|p, args| {
        let value = match p {
            // `setRef r v`, `stSetArray a i v`.
            Prim::SetRef => args.get(1),
            Prim::StSetArray => args.get(2),
            _ => return false,
        };
        // No representation known is a reference as far as this is concerned:
        // the question is only ever asked to leave the collector out, so what
        // is not known says nothing.
        value.is_none_or(|n| {
            !matches!(
                program.reps.get(n),
                Some(Rep::Int | Rep::Float | Rep::Bits(_))
            )
        })
    })
}

/// Whether the program can ever hold a value inside a compact region, which
/// is whether it makes one: a region comes from `compact` and nowhere else,
/// and nothing can be read out of one, sent between threads or added to that
/// did not start there. A program with no `compact` in it therefore never
/// meets a block whose count says it is in a region, and its counting helpers
/// do not ask -- see `silo/src/region.rs`, which is where the asking is paid
/// for.
fn makes_regions(program: &Program) -> bool {
    uses(program, &[Prim::Compact, Prim::CompactAdd])
}

/// Does the program use any of these primitives anywhere?
fn uses(program: &Program, want: &[Prim]) -> bool {
    knots(program, &|p, _| want.contains(p))
}

/// Is there anywhere in the program a primitive `want` says yes to, given
/// what it is applied to?
fn knots(program: &Program, want: &dyn Fn(&Prim, &[Name]) -> bool) -> bool {
    fn in_statement(s: &meadow_seq::Statement, want: &dyn Fn(&Prim, &[Name]) -> bool) -> bool {
        use meadow_seq::Statement::*;
        match s {
            Substitute(_, b) => in_statement(&b.body, want),
            Jump(_) | Invoke(..) | Error(_) => false,
            Let { rest, .. } => in_statement(rest, want),
            Switch { arms, default, .. } => {
                arms.iter().any(|(_, b)| in_statement(&b.body, want))
                    || in_statement(&default.body, want)
            }
            New { methods, rest, .. } => {
                methods.iter().any(|b| in_statement(&b.body, want)) || in_statement(rest, want)
            }
            Extern { op, args, blocks } => {
                let here = match op {
                    meadow_seq::Extern::Prim(p) => want(p, args),
                    meadow_seq::Extern::PrimK(p, _) => want(p, args),
                    meadow_seq::Extern::BranchPrim(p) => want(p, args),
                    _ => false,
                };
                here || blocks.iter().any(|b| in_statement(&b.body, want))
            }
            Mark(_, s) => in_statement(s, want),
        }
    }
    program
        .defs
        .iter()
        .any(|d| in_statement(&d.block.body, want))
}

/// The module being written.
pub struct Module<'p> {
    program: &'p Program,
    /// Every function written, as text, with how many parameters it takes
    /// in registers -- for declaring it in the other units: see
    /// [`Module::units`].
    funs: Vec<(String, String, usize)>,
    /// Every method's function, in method-table order: a closure's `meta` is
    /// the index of its first method here.
    methods: Vec<String>,
    /// Methods still to be written: their function's name, and the block.
    pending: Vec<(String, LBlock, usize)>,
    next_method: usize,
    /// Interned names, as the small integers a `Sym` literal loads.
    syms: Vec<meadow_intern::InternedString>,
    /// Constant strings: their global's name.
    strings: HashMap<String, String>,
    string_defs: String,
    /// Each string's global and its size, for declaring it elsewhere.
    string_sizes: Vec<(String, usize)>,
    /// Frames: how many made, and each one's function and method table, once
    /// written -- see [`Module::frame_fn`].
    next_frame: usize,
    frame_fns: HashMap<usize, String>,
    frame_tables: HashMap<usize, usize>,
    pending_frames: Vec<(String, Rc<Frame>)>,
    /// Whether the program can ever have a second thread: whether it
    /// spawns one anywhere. If it cannot, it pays for none of what threads
    /// need -- see [`Module::units`].
    threaded: bool,
    /// Whether it can make a cycle: see [`ties_knots`]. If it cannot, its
    /// counting helpers keep no candidates and its runtime collects none.
    cycles: bool,
    /// Whether it can make a compact region: see [`makes_regions`]. If it
    /// cannot, no value it ever holds is inside one, so its counting helpers
    /// do not ask.
    regions: bool,
}

impl<'p> Module<'p> {
    pub fn new(program: &'p Program) -> Module<'p> {
        Module {
            program,
            funs: Vec::new(),
            methods: Vec::new(),
            pending: Vec::new(),
            next_method: 0,
            syms: Vec::new(),
            strings: HashMap::new(),
            string_defs: String::new(),
            string_sizes: Vec::new(),
            next_frame: 0,
            frame_fns: HashMap::new(),
            frame_tables: HashMap::new(),
            pending_frames: Vec::new(),
            threaded: spawns(program),
            cycles: ties_knots(program),
            regions: makes_regions(program),
        }
    }

    /// A global holding `text`'s bytes, and its length.
    fn cstr(&mut self, text: &str) -> (String, usize) {
        if let Some(g) = self.strings.get(text) {
            return (g.clone(), text.len());
        }
        let g = format!("@s{}", self.strings.len());
        let mut bytes = String::new();
        for b in text.as_bytes() {
            match b {
                b' '..=b'~' if *b != b'"' && *b != b'\\' => bytes.push(*b as char),
                _ => {
                    let _ = write!(bytes, "\\{b:02X}");
                }
            }
        }
        let _ = writeln!(
            self.string_defs,
            "{g} = hidden constant [{} x i8] c\"{bytes}\\00\"",
            text.len() + 1
        );
        self.string_sizes.push((g.clone(), text.len() + 1));
        self.strings.insert(text.to_string(), g.clone());
        (g, text.len())
    }

    fn sym(&mut self, s: meadow_intern::InternedString) -> usize {
        match self.syms.iter().position(|x| *x == s) {
            Some(i) => i,
            None => {
                self.syms.push(s);
                self.syms.len() - 1
            }
        }
    }

    /// A **safe point**: where a thread that has had its turn gives way, so
    /// that one that never waits does not keep a core to itself. It stands at
    /// the entry of every definition and every method, which is to say on
    /// every loop, since a loop in AxCut is a jump to one of them.
    ///
    /// The flag is a byte in the runtime, set by the scheduler's timer only
    /// while there is a thread waiting for a core, so the usual answer is a
    /// load and a branch not taken. A program that cannot spawn has it as a
    /// constant zero of its own instead, and LLVM folds all of this away: see
    /// [`Module::units`].
    fn safe_point(&self, f: &mut Fun) {
        // A program that cannot spawn has nothing to give way to.
        if !self.threaded {
            return;
        }
        let (give, on) = (f.b(), f.b());
        let flag = f.t();
        f.i(format!("{flag} = load volatile i8, ptr @meadow_preempt"));
        let c = f.t();
        f.i(format!("{c} = icmp ne i8 {flag}, 0"));
        f.i(format!("br i1 {c}, label %{give}, label %{on}"));
        f.label(&give);
        f.i("call void @meadow_preempted()".to_string());
        f.i(format!("br label %{on}"));
        f.label(&on);
    }

    /// A definition: its function.
    pub fn def(&mut self, label: meadow_seq::Label, b: &LBlock) -> Result<(), Error> {
        let params: Vec<String> = (0..b.params.len()).map(|i| format!("%a{i}")).collect();
        let mut f = Fun::new(format!("@mw.L{}", label.0), params.clone());
        self.safe_point(&mut f);
        let mut env = HashMap::new();
        for (n, p) in b.params.iter().zip(&params) {
            env.insert(*n, V::Val(p.clone()));
        }
        self.stmt(&b.body, &mut env, &mut f)?;
        self.finish(f);
        self.drain()
    }

    /// Write the methods queued while writing functions, and theirs.
    fn drain(&mut self) -> Result<(), Error> {
        loop {
            if let Some((name, m, ncap)) = self.pending.pop() {
                self.method_fn(&name, &m, ncap)?;
            } else if let Some((name, fr)) = self.pending_frames.pop() {
                self.frame_body(&name, &fr)?;
            } else {
                return Ok(());
            }
        }
    }

    /// A frame's code, as a function of its captures and its argument:
    /// written once, and called wherever the frame is entered -- by the call
    /// it was made for returning, or by an `invoke` of it. Internal, so that
    /// LLVM inlines the ones entered from one place, within its own limits.
    ///
    /// Emitting the code at each place instead copied it there, and a frame
    /// entered from both arms of a branch -- a join -- inside another did so
    /// again: the standard library came out a gigabyte.
    ///
    /// A capture that is itself a frame not built is taken as *its* captures,
    /// and is a frame not built again inside: `f (g (h x))` returns into `g`'s
    /// continuation with `f`'s still waiting on the native stack, rather than
    /// building `f`'s as an object to hand across. Which captures are frames
    /// is fixed by where the frame was made, so it is the same at every entry.
    fn frame_body(&mut self, name: &str, fr: &Frame) -> Result<(), Error> {
        fn rebuild(caps: &[V], next: &mut usize) -> Vec<V> {
            caps.iter()
                .map(|c| match c {
                    V::Val(_) => {
                        *next += 1;
                        V::Val(format!("%a{}", *next - 1))
                    }
                    V::Frame(inner) => V::Frame(Rc::new(Frame {
                        id: inner.id,
                        captured: rebuild(&inner.captured, next),
                        method: inner.method.clone(),
                    })),
                })
                .collect()
        }
        let m = &fr.method;
        let mut next = 0;
        let captured = rebuild(&fr.captured, &mut next);
        let nargs = m.params.len() - fr.captured.len();
        let params: Vec<String> = (0..next + nargs).map(|i| format!("%a{i}")).collect();
        let mut f = Fun::new(name.to_string(), params.clone());
        let mut env = HashMap::new();
        for (n, v) in m.params.iter().zip(captured) {
            env.insert(*n, v);
        }
        for (n, p) in m.params[fr.captured.len()..].iter().zip(&params[next..]) {
            env.insert(*n, V::Val(p.clone()));
        }
        self.stmt(&m.body, &mut env, &mut f)?;
        self.finish_as(f, "hidden");
        Ok(())
    }

    /// The function a frame's code is: see [`Module::frame_body`].
    fn frame_fn(&mut self, fr: &Rc<Frame>) -> String {
        if let Some(n) = self.frame_fns.get(&fr.id) {
            return n.clone();
        }
        let name = format!("@mw.F{}", fr.id);
        self.frame_fns.insert(fr.id, name.clone());
        self.pending_frames.push((name.clone(), fr.clone()));
        name
    }

    /// What frame `fr` is entered with before its arguments: its captures,
    /// with a frame not built among them flattened into its own.
    fn flat_captures(caps: &[V], out: &mut Vec<String>) {
        for c in caps {
            match c {
                V::Val(v) => out.push(v.clone()),
                V::Frame(inner) => Self::flat_captures(&inner.captured, out),
            }
        }
    }

    /// Enter frame `fr` with `args` after its captures: a tail call to its
    /// code.
    fn call_frame(&mut self, fr: &Rc<Frame>, args: Vec<String>, f: &mut Fun) -> Result<(), Error> {
        let mut ops = Vec::with_capacity(fr.captured.len() + args.len());
        Self::flat_captures(&fr.captured, &mut ops);
        ops.extend(args);
        let callee = self.frame_fn(fr);
        spill(f, &ops);
        let list = ops
            .iter()
            .take(REGS)
            .map(|o| format!("i64 {o}"))
            .collect::<Vec<_>>()
            .join(", ");
        let r = f.t();
        f.i(format!("{r} = tail call ghccc i64 {callee}({list})"));
        f.i(format!("ret i64 {r}"));
        Ok(())
    }

    fn finish(&mut self, f: Fun) {
        self.finish_as(f, "hidden");
    }

    /// Every block's function is `noinline`: they tail-call one another, so
    /// a program is a few enormous strongly connected components of the call
    /// graph, which LLVM's inliner revisits until it has taken minutes and
    /// gigabytes on the standard library's tests. What is worth inlining --
    /// the counting helpers -- is `alwaysinline` instead.
    fn finish_as(&mut self, f: Fun, linkage: &str) {
        let regs = f.params.len().min(REGS);
        let text = format!(
            "define {linkage} ghccc i64 {}({}) noinline {{\nentry:\n{}{}{}}}\n\n",
            f.name,
            f.params
                .iter()
                .take(REGS)
                .map(|p| format!("i64 {p}"))
                .collect::<Vec<_>>()
                .join(", "),
            spilled_params(&f.params),
            f.allocas,
            f.body
        );
        self.funs.push((f.name, text, regs));
    }

    /// A method's function: the object first, then the arguments. It loads
    /// its `ncap` captures from the object, releasing it.
    fn method_fn(&mut self, name: &str, m: &LBlock, ncap: usize) -> Result<(), Error> {
        let nargs = m.params.len() - ncap;
        let mut params = vec!["%obj".to_string()];
        params.extend((0..nargs).map(|i| format!("%a{i}")));
        let mut f = Fun::new(name.to_string(), params);
        self.safe_point(&mut f);
        let mut env = HashMap::new();
        let caps = &m.params[..ncap];
        let loaded = self.load_fields(&mut f, "%obj", ncap);
        self.release(
            &mut f,
            "%obj",
            caps,
            &loaded,
            &env_with(&env, caps, &loaded),
        )?;
        for (n, v) in caps.iter().zip(&loaded) {
            env.insert(*n, V::Val(v.clone()));
        }
        for (i, n) in m.params[ncap..].iter().enumerate() {
            env.insert(*n, V::Val(format!("%a{i}")));
        }
        self.stmt(&m.body, &mut env, &mut f)?;
        self.finish(f);
        Ok(())
    }

    /// Queue `methods` as functions, and answer the method-table index of the
    /// first.
    fn methods_for(&mut self, methods: &[LBlock], ncap: usize) -> usize {
        let base = self.methods.len();
        for m in methods {
            let name = format!("@mw.M{}", self.next_method);
            self.next_method += 1;
            self.methods.push(name.clone());
            self.pending.push((name, m.clone(), ncap));
        }
        base
    }

    // --- descriptors and counting ----------------------------------------

    fn desc(&self, n: Name, env: &HashMap<Name, V>) -> D {
        // A reuse token is a block nobody counts -- its fields already taken
        // -- or nothing: never shared, and given back only by `Clean`, or by
        // erasing the frame that carries it (`erase_frame`). A frame that
        // carries one and is built as an object, then dropped unentered,
        // leaks the block: the one thing a token cannot be is erased as a
        // reference.
        if crate::linear::is_token(n) {
            return D::Known(desc::INT);
        }
        match self.program.reps.get(&n) {
            Some(Rep::Ref) | None => D::Known(desc::REF),
            Some(Rep::Int) => D::Known(desc::INT),
            Some(Rep::Float) => D::Known(desc::FLOAT),
            Some(Rep::Str) => D::Known(desc::STR),
            Some(Rep::Bits(d)) => D::Known(*d),
            Some(Rep::Var(d)) if *d == meadow_seq::NO_DESC => D::Known(desc::ANY),
            Some(Rep::Var(d)) => match env.get(&VarId(*d)) {
                Some(V::Val(v)) => D::Dyn(v.clone()),
                _ => D::Known(desc::ANY),
            },
            Some(Rep::Unknown) => D::Known(desc::ANY),
        }
    }

    /// Share `v`, described by `d`, `times` times: a call to one of the
    /// module's helpers ([`HELPERS`]), which LLVM inlines where it judges it
    /// worth it -- the inline sequence at every one of the many places a
    /// linear program shares made the module several times the size.
    fn share(&self, f: &mut Fun, v: &str, d: &D, times: usize) {
        match d {
            D::Known(k) if *k == desc::REF => {
                f.i(format!("call void @mw.share(i64 {v}, i32 {times})"));
            }
            D::Known(_) => {}
            D::Dyn(r) => f.i(format!(
                "call void @mw.share_d(i64 {v}, i64 {r}, i32 {times})"
            )),
        }
    }

    /// Erase `v`, described by `d`: see [`Module::share`].
    fn erase(&self, f: &mut Fun, v: &str, d: &D) {
        match d {
            D::Known(k) if *k == desc::REF => f.i(format!("call void @mw.erase(i64 {v})")),
            D::Known(_) => {}
            D::Dyn(r) => f.i(format!("call void @mw.erase_d(i64 {v}, i64 {r})")),
        }
    }

    /// Load `n` fields out of the non-uniform block `v`.
    fn load_fields(&self, f: &mut Fun, v: &str, n: usize) -> Vec<String> {
        if n == 0 {
            return Vec::new();
        }
        let first = 2 + n.div_ceil(16);
        let p = f.ptr(v);
        (0..n)
            .map(|i| {
                let q = f.t();
                f.i(format!(
                    "{q} = getelementptr i64, ptr {p}, i64 {}",
                    first + i
                ));
                let x = f.t();
                f.i(format!("{x} = load i64, ptr {q}"));
                x
            })
            .collect()
    }

    /// The paper's `release`: `v`'s fields `loaded` (named `names`) are ours
    /// now. If `v` was the last reference the block goes back to the runtime
    /// clean, and no count moves; otherwise its count goes down and each field
    /// is shared.
    fn release(
        &self,
        f: &mut Fun,
        v: &str,
        names: &[Name],
        loaded: &[String],
        env: &HashMap<Name, V>,
    ) -> Result<(), Error> {
        if names.is_empty() {
            // Nothing loaded: a nullary constructor, or a closure with no
            // captures -- which has no block, or one that is simply erased.
            self.erase(f, v, &D::Known(desc::REF));
            return Ok(());
        }
        let p = f.ptr(v);
        let rc = f.t();
        f.i(format!("{rc} = load i32, ptr {p}"));
        let (clean, shared, done) = (f.b(), f.b(), f.b());
        let last = f.t();
        f.i(format!("{last} = icmp eq i32 {rc}, 0"));
        f.i(format!("br i1 {last}, label %{clean}, label %{shared}"));
        f.label(&clean);
        f.i(format!("call void @meadow_clean(i64 {v})"));
        f.i(format!("br label %{done}"));
        f.label(&shared);
        let rc2 = f.t();
        f.i(format!("{rc2} = sub i32 {rc}, 1"));
        f.i(format!("store i32 {rc2}, ptr {p}"));
        // Loading the fields of a block inside a compact region is a
        // reference into that region given up, like any other: see
        // `silo/src/region.rs`.
        if self.regions {
            let inreg = f.t();
            let (gone, on) = (f.b(), f.b());
            f.i(format!("{inreg} = icmp ugt i32 {rc}, 536870912"));
            f.i(format!("br i1 {inreg}, label %{gone}, label %{on}"));
            f.label(&gone);
            f.i(format!("call void @meadow_region_erased(i64 {v})"));
            f.i(format!("br label %{on}"));
            f.label(&on);
        }
        for (n, x) in names.iter().zip(loaded) {
            let d = self.desc(*n, env);
            self.share(f, x, &d, 1);
        }
        f.i(format!("br label %{done}"));
        f.label(&done);
        Ok(())
    }

    /// [`Self::release`], keeping the block: the answer is a reuse token -- `v`
    /// itself when it was the last reference, its fields now ours and its
    /// memory free to build in, and `0` when it was shared, which is released
    /// as usual. See `linear`'s module docs.
    fn release_reuse(
        &self,
        f: &mut Fun,
        v: &str,
        meta: u64,
        names: &[Name],
        loaded: &[String],
        env: &HashMap<Name, V>,
    ) -> Result<String, Error> {
        let p = f.ptr(v);
        let rc = f.t();
        f.i(format!("{rc} = load i32, ptr {p}"));
        let (mine, shared, shared_end, done) = (f.b(), f.b(), f.b(), f.b());
        let last = f.t();
        f.i(format!("{last} = icmp eq i32 {rc}, 0"));
        f.i(format!("br i1 {last}, label %{mine}, label %{shared}"));
        f.label(&mine);
        f.i(format!("br label %{done}"));
        f.label(&shared);
        let rc2 = f.t();
        f.i(format!("{rc2} = sub i32 {rc}, 1"));
        f.i(format!("store i32 {rc2}, ptr {p}"));
        if self.regions {
            let inreg = f.t();
            let (gone, on) = (f.b(), f.b());
            f.i(format!("{inreg} = icmp ugt i32 {rc}, 536870912"));
            f.i(format!("br i1 {inreg}, label %{gone}, label %{on}"));
            f.label(&gone);
            f.i(format!("call void @meadow_region_erased(i64 {v})"));
            f.i(format!("br label %{on}"));
            f.label(&on);
        }
        for (n, x) in names.iter().zip(loaded) {
            let d = self.desc(*n, env);
            self.share(f, x, &d, 1);
        }
        f.i(format!("br label %{shared_end}"));
        f.label(&shared_end);
        f.i(format!("br label %{done}"));
        f.label(&done);
        let token = f.t();
        f.i(format!(
            "{token} = phi i64 [ {v}, %{mine} ], [ 0, %{shared_end} ]"
        ));
        f.origins.insert(
            token.clone(),
            Origin {
                meta,
                fields: loaded.to_vec(),
                descs: names.iter().map(|n| self.desc(*n, env)).collect(),
            },
        );
        Ok(token)
    }

    /// Erase `v`, keeping its block as a reuse token when this was the last
    /// reference: then its references to its fields, `names`, are given up
    /// -- the kept arm that loaded them holds its own -- and the block is the
    /// token; otherwise the token is `0`.
    fn drop_reuse(
        &self,
        f: &mut Fun,
        v: &str,
        names: &[Name],
        env: &HashMap<Name, V>,
    ) -> Result<String, Error> {
        let p = f.ptr(v);
        let rc = f.t();
        f.i(format!("{rc} = load i32, ptr {p}"));
        let (mine, shared, done) = (f.b(), f.b(), f.b());
        let last = f.t();
        f.i(format!("{last} = icmp eq i32 {rc}, 0"));
        f.i(format!("br i1 {last}, label %{mine}, label %{shared}"));
        f.label(&mine);
        let loaded = self.load_fields(f, v, names.len());
        for (n, x) in names.iter().zip(&loaded) {
            let d = self.desc(*n, env);
            self.erase(f, x, &d);
        }
        let mine_end = f.b();
        f.i(format!("br label %{mine_end}"));
        f.label(&mine_end);
        f.i(format!("br label %{done}"));
        f.label(&shared);
        self.erase(f, v, &D::Known(desc::REF));
        let shared_end = f.b();
        f.i(format!("br label %{shared_end}"));
        f.label(&shared_end);
        f.i(format!("br label %{done}"));
        f.label(&done);
        let token = f.t();
        f.i(format!(
            "{token} = phi i64 [ {v}, %{mine_end} ], [ 0, %{shared_end} ]"
        ));
        Ok(token)
    }

    /// Give reuse token `token` back unused: its block, if it holds one.
    fn clean_token(&self, f: &mut Fun, token: &str) {
        let (give, on) = (f.b(), f.b());
        let has = f.t();
        f.i(format!("{has} = icmp ne i64 {token}, 0"));
        f.i(format!("br i1 {has}, label %{give}, label %{on}"));
        f.label(&give);
        f.i(format!("call void @meadow_clean(i64 {token})"));
        f.i(format!("br label %{on}"));
        f.label(&on);
    }

    /// A block of `kind` with `meta`, whose fields are `vals`, described by
    /// `descs`. No fields at all is no block: `meta << 1 | 1`.
    fn build(&self, f: &mut Fun, kind: u64, meta: u64, vals: &[String], descs: &[D]) -> String {
        self.build_in(f, kind, meta, vals, descs, None)
    }

    /// [`Self::build`], in reuse token `reuse`'s block when it holds one --
    /// the size of this one, since only a `let` of as many fields is given one
    /// -- and in a new block when it does not.
    ///
    /// A new block has every word written. A reused one has only what differs
    /// from what it held (Perceus's *reuse specialization*): its first word
    /// already says zero references and as many fields; its constructor and
    /// descriptors are left when they are the same; and a field is left when
    /// its new value is what was loaded out of that very slot -- the `l` and
    /// `r` of `Node c l k v r` rebuilt with a new colour, say. A token that
    /// came into this function as an argument has no known origin, and is
    /// written whole.
    fn build_in(
        &self,
        f: &mut Fun,
        kind: u64,
        meta: u64,
        vals: &[String],
        descs: &[D],
        reuse: Option<&str>,
    ) -> String {
        if vals.is_empty() {
            return format!("{}", (meta << 1) | 1);
        }
        let n = vals.len();
        let dw = n.div_ceil(16);
        match reuse {
            None => {
                let b = f.t();
                f.i(format!(
                    "{b} = call i64 @meadow_acquire(i64 {})",
                    2 + dw + n
                ));
                self.write_block(f, &b, kind, meta, vals, descs, None);
                b
            }
            Some(token) => {
                let origin = f.origins.get(token).cloned();
                let (keep, fresh, join) = (f.b(), f.b(), f.b());
                let has = f.t();
                f.i(format!("{has} = icmp ne i64 {token}, 0"));
                f.i(format!("br i1 {has}, label %{keep}, label %{fresh}"));
                f.label(&keep);
                self.write_block(f, token, kind, meta, vals, descs, Some(origin.as_ref()));
                let keep_end = f.b();
                f.i(format!("br label %{keep_end}"));
                f.label(&keep_end);
                f.i(format!("br label %{join}"));
                f.label(&fresh);
                let a = f.t();
                f.i(format!(
                    "{a} = call i64 @meadow_acquire(i64 {})",
                    2 + dw + n
                ));
                self.write_block(f, &a, kind, meta, vals, descs, None);
                let fresh_end = f.b();
                f.i(format!("br label %{fresh_end}"));
                f.label(&fresh_end);
                f.i(format!("br label %{join}"));
                f.label(&join);
                let b = f.t();
                f.i(format!(
                    "{b} = phi i64 [ {token}, %{keep_end} ], [ {a}, %{fresh_end} ]"
                ));
                b
            }
        }
    }

    /// Write block `b` as [`Self::build_in`] builds it. `reused` is `None`
    /// for a new block, and `Some` for a reuse token's -- with where the
    /// token came from, when that is known.
    #[allow(clippy::too_many_arguments)]
    fn write_block(
        &self,
        f: &mut Fun,
        b: &str,
        kind: u64,
        meta: u64,
        vals: &[String],
        descs: &[D],
        reused: Option<Option<&Origin>>,
    ) {
        let n = vals.len();
        let dw = n.div_ceil(16);
        let same = match reused {
            Some(Some(o)) if o.meta == meta && kind == kind::DATA && o.fields.len() == n => Some(o),
            _ => None,
        };
        let p = f.ptr(b);
        if reused.is_none() {
            f.i(format!("store i64 {}, ptr {p}", (n as u64) << 32));
        }
        if same.is_none() {
            let w1 = f.t();
            f.i(format!("{w1} = getelementptr i64, ptr {p}, i64 1"));
            f.i(format!("store i64 {}, ptr {w1}", kind | (meta << 32)));
        }
        let same_descs = same.is_some_and(|o| {
            o.descs.as_slice() == descs && descs.iter().all(|d| matches!(d, D::Known(_)))
        });
        for w in 0..dw {
            if same_descs {
                break;
            }
            let mut known: u64 = 0;
            let mut dynamic: Vec<(usize, String)> = Vec::new();
            for (j, d) in descs.iter().enumerate().skip(16 * w).take(16) {
                let shift = 4 * (j - 16 * w);
                match d {
                    D::Known(k) => known |= ((*k as u64) & 15) << shift,
                    D::Dyn(r) => dynamic.push((shift, r.clone())),
                }
            }
            let mut acc = format!("{known}");
            for (shift, r) in dynamic {
                let m = f.t();
                f.i(format!("{m} = and i64 {r}, 15"));
                let s = f.t();
                f.i(format!("{s} = shl i64 {m}, {shift}"));
                let o = f.t();
                f.i(format!("{o} = or i64 {acc}, {s}"));
                acc = o;
            }
            let q = f.t();
            f.i(format!("{q} = getelementptr i64, ptr {p}, i64 {}", 2 + w));
            f.i(format!("store i64 {acc}, ptr {q}"));
        }
        for (i, v) in vals.iter().enumerate() {
            if same.is_some_and(|o| o.fields[i] == *v) {
                continue;
            }
            let q = f.t();
            f.i(format!(
                "{q} = getelementptr i64, ptr {p}, i64 {}",
                2 + dw + i
            ));
            f.i(format!("store i64 {v}, ptr {q}"));
        }
    }

    // --- statements -------------------------------------------------------

    /// `n`'s value as an operand, building it if it is a frame not built.
    fn val(&mut self, n: Name, env: &mut HashMap<Name, V>, f: &mut Fun) -> Result<String, Error> {
        match env.get(&n).cloned() {
            Some(V::Val(v)) => Ok(v),
            Some(V::Frame(fr)) => {
                let v = self.materialize(&fr, env, f)?;
                env.insert(n, V::Val(v.clone()));
                Ok(v)
            }
            None => err(format!("{n:?} is not in scope in the native backend")),
        }
    }

    /// Build a frame as the object it would have been.
    fn materialize(
        &mut self,
        fr: &Frame,
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<String, Error> {
        let ncap = fr.captured.len();
        let base = match self.frame_tables.get(&fr.id) {
            Some(b) => *b,
            None => {
                let b = self.methods_for(std::slice::from_ref(&fr.method), ncap);
                self.frame_tables.insert(fr.id, b);
                b
            }
        };
        let mut vals = Vec::with_capacity(ncap);
        let mut descs = Vec::with_capacity(ncap);
        for (i, c) in fr.captured.iter().enumerate() {
            let v = match c {
                V::Val(v) => v.clone(),
                V::Frame(inner) => self.materialize(inner, env, f)?,
            };
            vals.push(v);
            descs.push(self.desc(fr.method.params[i], env));
        }
        Ok(self.build(f, kind::CLOSURE, base as u64, &vals, &descs))
    }

    fn stmt(&mut self, s: &L, env: &mut HashMap<Name, V>, f: &mut Fun) -> Result<(), Error> {
        match s {
            L::Share(n, rest) => {
                let v = self.val(*n, env, f)?;
                let d = self.desc(*n, env);
                self.share(f, &v, &d, 1);
                self.stmt(rest, env, f)
            }
            L::Erase(n, rest) => {
                match env.get(n).cloned() {
                    // A frame nobody will call: what it captured goes.
                    Some(V::Frame(fr)) => self.erase_frame(&fr, env, f)?,
                    _ => {
                        let v = self.val(*n, env, f)?;
                        let d = self.desc(*n, env);
                        self.erase(f, &v, &d);
                    }
                }
                env.remove(n);
                self.stmt(rest, env, f)
            }
            L::Rename(binds, rest) => {
                let vals: Vec<(Name, V)> = binds
                    .iter()
                    .map(|(to, from)| {
                        env.get(from)
                            .cloned()
                            .map(|v| (*to, v))
                            .ok_or_else(|| Error {
                                msg: format!("{from:?} is not in scope in the native backend"),
                            })
                    })
                    .collect::<Result<_, _>>()?;
                for (to, v) in vals {
                    env.insert(to, v);
                }
                self.stmt(rest, env, f)
            }
            L::Let {
                name,
                tag,
                fields,
                reuse,
                rest,
            } => {
                let mut vals = Vec::with_capacity(fields.len());
                let mut descs = Vec::with_capacity(fields.len());
                for x in fields {
                    vals.push(self.val(*x, env, f)?);
                    descs.push(self.desc(*x, env));
                }
                let token = match reuse {
                    Some(t) => {
                        let tok = self.val(*t, env, f)?;
                        env.remove(t);
                        Some(tok)
                    }
                    None => None,
                };
                let v = self.build_in(f, kind::DATA, *tag as u64, &vals, &descs, token.as_deref());
                env.insert(*name, V::Val(v));
                self.stmt(rest, env, f)
            }
            L::Clean(t, rest) => {
                let tok = self.val(*t, env, f)?;
                self.clean_token(f, &tok);
                env.remove(t);
                self.stmt(rest, env, f)
            }
            L::DropReuse {
                name,
                fields,
                token,
                rest,
            } => {
                let v = self.val(*name, env, f)?;
                let tok = self.drop_reuse(f, &v, fields, env)?;
                env.remove(name);
                env.insert(*token, V::Val(tok));
                self.stmt(rest, env, f)
            }
            L::Switch {
                scrutinee,
                arms,
                default,
                keep_default,
            } => self.switch(*scrutinee, arms, default, *keep_default, env, f),
            L::New {
                name,
                captures,
                methods,
                frame,
                rest,
            } => {
                let one_result =
                    methods.len() == 1 && methods[0].params.len() == captures.len() + 1;
                if *frame && one_result {
                    let captured = captures
                        .iter()
                        .map(|c| env.get(c).cloned())
                        .collect::<Option<Vec<V>>>()
                        .ok_or_else(|| Error {
                            msg: "a frame captures a name not in scope".into(),
                        })?;
                    env.insert(
                        *name,
                        V::Frame(Rc::new(Frame {
                            id: {
                                self.next_frame += 1;
                                self.next_frame
                            },
                            captured,
                            method: methods[0].clone(),
                        })),
                    );
                } else {
                    let base = self.methods_for(methods, captures.len());
                    let mut vals = Vec::with_capacity(captures.len());
                    let mut descs = Vec::with_capacity(captures.len());
                    for c in captures {
                        vals.push(self.val(*c, env, f)?);
                        descs.push(self.desc(*c, env));
                    }
                    let v = self.build(f, kind::CLOSURE, base as u64, &vals, &descs);
                    env.insert(*name, V::Val(v));
                }
                self.stmt(rest, env, f)
            }
            L::Jump { label, args } => {
                let callee = format!("@mw.L{}", label.0);
                self.transfer(&callee, None, args, env, f)
            }
            L::Invoke { target, tag, args } => {
                if let Some(V::Frame(fr)) = env.get(target).cloned() {
                    // Our own frame, entered directly: its code, here.
                    return self.enter_frame(&fr, args, env, f);
                }
                let t = self.val(*target, env, f)?;
                // A continuation of one argument may be the null one: return
                // natively.
                if *tag == 0 && args.len() == 1 {
                    let a = self.val(args[0], env, f)?;
                    let (ret, call) = (f.b(), f.b());
                    let z = f.t();
                    f.i(format!("{z} = icmp eq i64 {t}, 0"));
                    f.i(format!("br i1 {z}, label %{ret}, label %{call}"));
                    f.label(&ret);
                    f.i(format!("ret i64 {a}"));
                    f.label(&call);
                }
                let fp = self.method_ptr(f, &t, *tag);
                self.transfer(&fp, Some(t), args, env, f)
            }
            L::Extern { op, args, blocks } => self.extern_op(op, args, blocks, env, f),
            L::Error(msg) => {
                let (g, len) = self.cstr(msg);
                f.i(format!("call void @meadow_fail(ptr {g}, i64 {len})"));
                f.i("unreachable".to_string());
                Ok(())
            }
        }
    }

    fn erase_frame(
        &mut self,
        fr: &Frame,
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<(), Error> {
        for (i, c) in fr.captured.iter().enumerate() {
            match c {
                V::Frame(inner) => self.erase_frame(inner, env, f)?,
                V::Val(v) if crate::linear::is_token(fr.method.params[i]) => {
                    self.clean_token(f, v);
                }
                V::Val(v) => {
                    let d = self.desc(fr.method.params[i], env);
                    self.erase(f, v, &d);
                }
            }
        }
        Ok(())
    }

    /// A frame's method, emitted here with `args` after its captures.
    fn enter_frame(
        &mut self,
        fr: &Rc<Frame>,
        args: &[Name],
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<(), Error> {
        let vals = args
            .iter()
            .map(|a| self.val(*a, env, f))
            .collect::<Result<Vec<_>, _>>()?;
        self.call_frame(fr, vals, f)
    }

    /// The function pointer of method `tag` of the object in `t`.
    fn method_ptr(&self, f: &mut Fun, t: &str, tag: u32) -> String {
        let (imm, blk, join) = (f.b(), f.b(), f.b());
        let low = f.t();
        f.i(format!("{low} = and i64 {t}, 1"));
        let odd = f.t();
        f.i(format!("{odd} = icmp ne i64 {low}, 0"));
        f.i(format!("br i1 {odd}, label %{imm}, label %{blk}"));
        f.label(&imm);
        let m1 = f.t();
        f.i(format!("{m1} = lshr i64 {t}, 1"));
        f.i(format!("br label %{join}"));
        f.label(&blk);
        let p = f.ptr(t);
        let q = f.t();
        f.i(format!("{q} = getelementptr i64, ptr {p}, i64 1"));
        let w1 = f.t();
        f.i(format!("{w1} = load i64, ptr {q}"));
        let m2 = f.t();
        f.i(format!("{m2} = lshr i64 {w1}, 32"));
        f.i(format!("br label %{join}"));
        f.label(&join);
        let m = f.t();
        f.i(format!("{m} = phi i64 [ {m1}, %{imm} ], [ {m2}, %{blk} ]"));
        let idx = f.t();
        f.i(format!("{idx} = add i64 {m}, {tag}"));
        let slot = f.t();
        f.i(format!(
            "{slot} = getelementptr ptr, ptr @meadow_methods, i64 {idx}"
        ));
        let fp = f.t();
        f.i(format!("{fp} = load ptr, ptr {slot}"));
        fp
    }

    /// Hand control to `callee` -- after `first`, the object of an invoke --
    /// with `args`. A tail call; unless one of the arguments is a frame not
    /// built, in which case it is a native call whose continuation is null,
    /// and the frame's code follows it with the answer.
    fn transfer(
        &mut self,
        callee: &str,
        first: Option<String>,
        args: &[Name],
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<(), Error> {
        // The frame to return into natively: the last one among the
        // arguments. Any other is built.
        let native = args
            .iter()
            .rposition(|a| matches!(env.get(a), Some(V::Frame(_))));
        let mut ops: Vec<String> = first.into_iter().collect();
        let mut frame = None;
        for (i, a) in args.iter().enumerate() {
            if Some(i) == native {
                if let Some(V::Frame(fr)) = env.get(a).cloned() {
                    frame = Some(fr);
                }
                ops.push("0".into());
            } else {
                ops.push(self.val(*a, env, f)?);
            }
        }
        spill(f, &ops);
        let list = ops
            .iter()
            .take(REGS)
            .map(|o| format!("i64 {o}"))
            .collect::<Vec<_>>()
            .join(", ");
        match frame {
            None => {
                let r = f.t();
                f.i(format!("{r} = tail call ghccc i64 {callee}({list})"));
                f.i(format!("ret i64 {r}"));
                Ok(())
            }
            Some(fr) => {
                let r = f.t();
                f.i(format!("{r} = call ghccc i64 {callee}({list})"));
                self.call_frame(&fr, vec![r], f)
            }
        }
    }

    fn switch(
        &mut self,
        scrutinee: Name,
        arms: &[SwitchArm],
        default: &L,
        keep_default: bool,
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<(), Error> {
        let s = self.val(scrutinee, env, f)?;
        let (imm, blk, dispatch) = (f.b(), f.b(), f.b());
        let low = f.t();
        f.i(format!("{low} = and i64 {s}, 1"));
        let odd = f.t();
        f.i(format!("{odd} = icmp ne i64 {low}, 0"));
        f.i(format!("br i1 {odd}, label %{imm}, label %{blk}"));
        f.label(&imm);
        let t1 = f.t();
        f.i(format!("{t1} = lshr i64 {s}, 1"));
        f.i(format!("br label %{dispatch}"));
        f.label(&blk);
        let p = f.ptr(&s);
        let q = f.t();
        f.i(format!("{q} = getelementptr i64, ptr {p}, i64 1"));
        let w1 = f.t();
        f.i(format!("{w1} = load i64, ptr {q}"));
        let t2 = f.t();
        f.i(format!("{t2} = lshr i64 {w1}, 32"));
        f.i(format!("br label %{dispatch}"));
        f.label(&dispatch);
        let tag = f.t();
        f.i(format!(
            "{tag} = phi i64 [ {t1}, %{imm} ], [ {t2}, %{blk} ]"
        ));
        let def = f.b();
        let labels: Vec<String> = arms.iter().map(|_| f.b()).collect();
        let mut sw = format!("switch i64 {tag}, label %{def} [");
        let mut seen = std::collections::HashSet::new();
        for (a, l) in arms.iter().zip(&labels) {
            if seen.insert(a.tag) {
                let _ = write!(sw, " i64 {}, label %{l}", a.tag);
            }
        }
        sw.push_str(" ]");
        f.i(sw);
        for (a, l) in arms.iter().zip(&labels) {
            f.label(l);
            let mut inner = env.clone();
            let loaded = self.load_fields(f, &s, a.fields.len());
            for (n, x) in a.fields.iter().zip(&loaded) {
                inner.insert(*n, V::Val(x.clone()));
            }
            // An arm that keeps the scrutinee borrows its fields: its body
            // shares what it uses of them, and nothing here is given up.
            match (a.keep, a.reuse) {
                (true, _) => {}
                (false, Some(t)) => {
                    let token =
                        self.release_reuse(f, &s, a.tag as u64, &a.fields, &loaded, &inner)?;
                    inner.insert(t, V::Val(token));
                }
                (false, None) => self.release(f, &s, &a.fields, &loaded, &inner)?,
            }
            self.stmt(&a.body, &mut inner, f)?;
        }
        f.label(&def);
        let mut inner = env.clone();
        if !keep_default {
            self.erase(f, &s, &D::Known(desc::REF));
        }
        self.stmt(default, &mut inner, f)
    }

    // --- primitives -------------------------------------------------------

    fn extern_op(
        &mut self,
        op: &Extern,
        args: &[Name],
        blocks: &[(Vec<Name>, L)],
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<(), Error> {
        if op.is_branch() {
            let cond = match op {
                Extern::Branch => {
                    let v = self.val(args[0], env, f)?;
                    let c = f.t();
                    f.i(format!("{c} = icmp ne i64 {v}, 0"));
                    c
                }
                Extern::BranchPrim(p) => {
                    let (a, b) = (self.val(args[0], env, f)?, self.val(args[1], env, f)?);
                    let r = self.prim2(*p, &a, &b, args[0], env, f)?;
                    let c = f.t();
                    f.i(format!("{c} = icmp ne i64 {r}, 0"));
                    c
                }
                Extern::BranchPrimK(p, lit) => {
                    let a = self.val(args[0], env, f)?;
                    let k = self.literal(lit, f)?;
                    let r = self.prim2(*p, &a, &k, args[0], env, f)?;
                    let c = f.t();
                    f.i(format!("{c} = icmp ne i64 {r}, 0"));
                    c
                }
                _ => unreachable!("is_branch"),
            };
            let [(_, on_false), (_, on_true)] = blocks else {
                return err("a branch needs two continuations");
            };
            let (t, e) = (f.b(), f.b());
            f.i(format!("br i1 {cond}, label %{t}, label %{e}"));
            f.label(&t);
            self.stmt(on_true, &mut env.clone(), f)?;
            f.label(&e);
            return self.stmt(on_false, &mut env.clone(), f);
        }
        let [(results, body)] = blocks else {
            return err("a primitive that answers needs one continuation");
        };
        // The stack-segment primitives take the code after them: see
        // `silo/src/segments.rs`. It is packed as a closure of one argument
        // -- what it binds -- and the runtime answers what that code finally
        // returns natively, which this returns.
        if let Extern::Prim(p @ (Prim::Enter | Prim::Detach | Prim::Reattach)) = op {
            let caps: Vec<Name> = crate::linear::free(body)
                .into_iter()
                .filter(|n| !results.contains(n))
                .collect();
            let mut params = caps.clone();
            params.extend(results.iter().copied());
            let method = LBlock {
                params,
                body: body.clone(),
            };
            let base = self.methods_for(std::slice::from_ref(&method), caps.len());
            let mut vals = Vec::with_capacity(caps.len());
            let mut descs = Vec::with_capacity(caps.len());
            for c in &caps {
                vals.push(self.val(*c, env, f)?);
                descs.push(self.desc(*c, env));
            }
            let code = self.build(f, kind::CLOSURE, base as u64, &vals, &descs);
            let a = self.val(args[0], env, f)?;
            let entry = match p {
                Prim::Enter => "meadow_enter",
                Prim::Detach => "meadow_detach",
                _ => "meadow_reattach",
            };
            let r = f.t();
            f.i(format!("{r} = call i64 @{entry}(i64 {a}, i64 {code})"));
            f.i(format!("ret i64 {r}"));
            return Ok(());
        }
        let v = match op {
            Extern::Lit(l) => self.literal(l, f)?,
            Extern::Prim(p) => {
                let vals = args
                    .iter()
                    .map(|a| self.val(*a, env, f))
                    .collect::<Result<Vec<_>, _>>()?;
                let result = results.first().copied();
                if let Some(r) = self.inline_prim(*p, args, &vals, result, env, f) {
                    let mut inner = env.clone();
                    if let Some(n) = result {
                        inner.insert(n, V::Val(r));
                    }
                    return self.stmt(body, &mut inner, f);
                }
                match vals.as_slice() {
                    [a, b] if inline2(*p, self.rep(args[0])) => {
                        self.prim2(*p, a, b, args[0], env, f)?
                    }
                    [a] if *p == Prim::Neg && self.rep(args[0]) == Some(Rep::Int) => {
                        let r = f.t();
                        f.i(format!("{r} = sub i64 0, {a}"));
                        r
                    }
                    // The few a program does millions of times have an entry
                    // of their own in the runtime, taking their arguments in
                    // registers: see `direct`.
                    _ if direct(*p).is_some() => {
                        let (name, descs) = direct(*p).expect("checked");
                        let last = args.len() - 1;
                        let mut ops = Vec::new();
                        for (i, (a, v)) in args.iter().zip(&vals).enumerate() {
                            ops.push(format!("i64 {v}"));
                            let wanted =
                                descs == Descs::Each || (descs == Descs::Last && i == last);
                            if wanted {
                                ops.push(format!(
                                    "i64 {}",
                                    match self.desc(*a, env) {
                                        D::Known(k) => k.to_string(),
                                        D::Dyn(x) => x,
                                    }
                                ));
                            }
                        }
                        let r = f.t();
                        f.i(format!("{r} = call i64 @{name}({})", ops.join(", ")));
                        r
                    }
                    // A thread's function answers what the thread does, and
                    // the runtime keeps that answer for `await`: it is told
                    // how it is represented, as the VM is.
                    _ if *p == Prim::ThreadSpawn => {
                        let answer = match result.and_then(|r| self.program.threads.get(&r)) {
                            Some(Rep::Var(d)) if *d != meadow_seq::NO_DESC => {
                                match env.get(&VarId(*d)) {
                                    Some(V::Val(v)) => D::Dyn(v.clone()),
                                    _ => D::Known(desc::ANY),
                                }
                            }
                            Some(rep) => D::Known(rep.desc().unwrap_or(desc::ANY)),
                            None => D::Known(desc::ANY),
                        };
                        let a = match answer {
                            D::Known(k) => k.to_string(),
                            D::Dyn(x) => x.clone(),
                        };
                        self.generic(*p, args, &vals, &[(a, D::Known(desc::INT))], env, f)
                    }
                    _ => self.generic(*p, args, &vals, &[], env, f),
                }
            }
            Extern::PrimK(p, lit) => {
                let a = self.val(args[0], env, f)?;
                if inline2(*p, self.rep(args[0])) {
                    let k = self.literal(lit, f)?;
                    self.prim2(*p, &a, &k, args[0], env, f)?
                } else {
                    let k = self.literal(lit, f)?;
                    let kd = D::Known(lit_desc(lit));
                    self.generic(*p, args, &[a], &[(k, kd)], env, f)
                }
            }
            Extern::Field(i) => {
                let a = self.val(args[0], env, f)?;
                let r = f.t();
                f.i(format!("{r} = call i64 @meadow_field(i64 {a}, i64 {i})"));
                r
            }
            Extern::Array | Extern::Record(_) => {
                let vals = args
                    .iter()
                    .map(|a| self.val(*a, env, f))
                    .collect::<Result<Vec<_>, _>>()?;
                // A record keeps its labels in one order, the interned names'
                // -- which is decided here, in the compiler, as it is for the
                // other backends.
                let mut order: Vec<usize> = (0..args.len()).collect();
                let mut labels = Vec::new();
                if let Extern::Record(ls) = op {
                    order.sort_by_key(|i| ls[*i]);
                    labels = order.iter().map(|i| self.sym(ls[*i])).collect();
                }
                let n = args.len().max(1);
                let (av, dv) = (f.t(), f.t());
                f.alloca(format!("{av} = alloca [{n} x i64]"));
                f.alloca(format!("{dv} = alloca [{n} x i64]"));
                for (k, i) in order.iter().enumerate() {
                    let (q, r) = (f.t(), f.t());
                    f.i(format!("{q} = getelementptr i64, ptr {av}, i64 {k}"));
                    f.i(format!("store i64 {}, ptr {q}", vals[*i]));
                    f.i(format!("{r} = getelementptr i64, ptr {dv}, i64 {k}"));
                    let d = match self.desc(args[*i], env) {
                        D::Known(x) => x.to_string(),
                        D::Dyn(x) => x,
                    };
                    f.i(format!("store i64 {d}, ptr {r}"));
                }
                let r = f.t();
                if labels.is_empty() && matches!(op, Extern::Array) {
                    f.i(format!(
                        "{r} = call i64 @meadow_array(i64 {}, ptr {av}, ptr {dv})",
                        args.len()
                    ));
                } else {
                    let lv = f.t();
                    f.alloca(format!("{lv} = alloca [{n} x i64]"));
                    for (k, l) in labels.iter().enumerate() {
                        let q = f.t();
                        f.i(format!("{q} = getelementptr i64, ptr {lv}, i64 {k}"));
                        f.i(format!("store i64 {l}, ptr {q}"));
                    }
                    f.i(format!(
                        "{r} = call i64 @meadow_record(i64 {}, ptr {av}, ptr {dv}, ptr {lv})",
                        args.len()
                    ));
                }
                r
            }
            Extern::Select(label) => {
                let a = self.val(args[0], env, f)?;
                let l = self.sym(*label);
                let r = f.t();
                f.i(format!("{r} = call i64 @meadow_select(i64 {a}, i64 {l})"));
                r
            }
            Extern::Extend(label) => {
                let rec = self.val(args[0], env, f)?;
                let v = self.val(args[1], env, f)?;
                let d = match self.desc(args[1], env) {
                    D::Known(x) => x.to_string(),
                    D::Dyn(x) => x,
                };
                let l = self.sym(*label);
                let r = f.t();
                f.i(format!(
                    "{r} = call i64 @meadow_extend(i64 {rec}, i64 {l}, i64 {v}, i64 {d})"
                ));
                r
            }
            Extern::Native(effect, op) => {
                let a = self.val(args[0], env, f)?;
                let d = match self.desc(args[0], env) {
                    D::Known(k) => k.to_string(),
                    D::Dyn(x) => x,
                };
                let (ge, _) = self.cstr(effect);
                let (go, _) = self.cstr(op);
                let r = f.t();
                f.i(format!(
                    "{r} = call i64 @meadow_native(ptr {ge}, ptr {go}, i64 {a}, i64 {d})"
                ));
                r
            }
            other => {
                let (g, len) = self.cstr(&format!("the native backend cannot do {other:?} yet"));
                f.i(format!("call void @meadow_fail(ptr {g}, i64 {len})"));
                f.i("unreachable".into());
                return Ok(());
            }
        };
        let mut inner = env.clone();
        if let Some(r) = results.first() {
            inner.insert(*r, V::Val(v));
        }
        self.stmt(body, &mut inner, f)
    }

    fn rep(&self, n: Name) -> Option<Rep> {
        self.program.reps.get(&n).copied()
    }

    /// A literal's word. `Str` is a string, made by the runtime once and kept.
    fn literal(&mut self, l: &Lit, f: &mut Fun) -> Result<String, Error> {
        Ok(match l {
            Lit::Int(n) | Lit::AnyInt(n, _) => n.to_string(),
            Lit::Float(x) | Lit::AnyFloat(x, _) => (x.to_bits() as i64).to_string(),
            Lit::Float32(x) => (x.to_bits() as i64).to_string(),
            Lit::Word(_, bits) => (*bits as i64).to_string(),
            Lit::Char(c) => (*c as u32).to_string(),
            Lit::Bool(b) => u8::from(*b).to_string(),
            Lit::Unit => "0".into(),
            Lit::Sym(s) => self.sym(*s).to_string(),
            Lit::Str(s) => {
                let (g, len) = self.cstr(s);
                let r = f.t();
                f.i(format!("{r} = call i64 @meadow_text(ptr {g}, i64 {len})"));
                r
            }
            Lit::BigInt(n) => {
                let r = f.t();
                f.i(format!("{r} = call i64 @meadow_bigint(i64 {n})"));
                r
            }
        })
    }

    /// A binary primitive done inline: see [`inline2`] for which.
    fn prim2(
        &mut self,
        p: Prim,
        a: &str,
        b: &str,
        left: Name,
        env: &mut HashMap<Name, V>,
        f: &mut Fun,
    ) -> Result<String, Error> {
        let rep = self.rep(left);
        if !inline2(p, rep) {
            let d = self.desc(left, env);
            return Ok(self.generic(
                p,
                &[],
                &[],
                &[(a.to_string(), d.clone()), (b.to_string(), d)],
                env,
                f,
            ));
        }
        let float = rep == Some(Rep::Float);
        let p = untyped(p);
        let r = f.t();
        let bool_of = |f: &mut Fun, c: String| {
            let z = f.t();
            f.i(format!("{z} = zext i1 {c} to i64"));
            z
        };
        match p {
            Prim::Add => f.i(format!("{r} = add i64 {a}, {b}")),
            Prim::Sub => f.i(format!("{r} = sub i64 {a}, {b}")),
            Prim::Mul => f.i(format!("{r} = mul i64 {a}, {b}")),
            Prim::BitAnd => f.i(format!("{r} = and i64 {a}, {b}")),
            Prim::BitOr => f.i(format!("{r} = or i64 {a}, {b}")),
            Prim::BitXor => f.i(format!("{r} = xor i64 {a}, {b}")),
            Prim::Shl | Prim::Shr => {
                let m = f.t();
                f.i(format!("{m} = and i64 {b}, 63"));
                let ins = if p == Prim::Shl { "shl" } else { "ashr" };
                f.i(format!("{r} = {ins} i64 {a}, {m}"));
            }
            Prim::Div | Prim::Mod => return Ok(self.divide(p, a, b, f)),
            Prim::AddF | Prim::SubF | Prim::MulF | Prim::DivF => {
                let ins = match p {
                    Prim::AddF => "fadd",
                    Prim::SubF => "fsub",
                    Prim::MulF => "fmul",
                    _ => "fdiv",
                };
                let (x, y) = (f.t(), f.t());
                f.i(format!("{x} = bitcast i64 {a} to double"));
                f.i(format!("{y} = bitcast i64 {b} to double"));
                let z = f.t();
                f.i(format!("{z} = {ins} double {x}, {y}"));
                f.i(format!("{r} = bitcast double {z} to i64"));
            }
            Prim::Eq | Prim::Ne | Prim::Lt | Prim::Gt | Prim::Le | Prim::Ge => {
                let c = f.t();
                if float {
                    let (x, y) = (f.t(), f.t());
                    f.i(format!("{x} = bitcast i64 {a} to double"));
                    f.i(format!("{y} = bitcast i64 {b} to double"));
                    let pred = match p {
                        Prim::Eq => "oeq",
                        Prim::Ne => "une",
                        Prim::Lt => "olt",
                        Prim::Gt => "ogt",
                        Prim::Le => "ole",
                        _ => "oge",
                    };
                    f.i(format!("{c} = fcmp {pred} double {x}, {y}"));
                } else {
                    let pred = match p {
                        Prim::Eq => "eq",
                        Prim::Ne => "ne",
                        Prim::Lt => "slt",
                        Prim::Gt => "sgt",
                        Prim::Le => "sle",
                        _ => "sge",
                    };
                    f.i(format!("{c} = icmp {pred} i64 {a}, {b}"));
                }
                return Ok(bool_of(f, c));
            }
            Prim::LtF | Prim::GtF | Prim::LeF | Prim::GeF => {
                let (x, y) = (f.t(), f.t());
                f.i(format!("{x} = bitcast i64 {a} to double"));
                f.i(format!("{y} = bitcast i64 {b} to double"));
                let pred = match p {
                    Prim::LtF => "olt",
                    Prim::GtF => "ogt",
                    Prim::LeF => "ole",
                    _ => "oge",
                };
                let c = f.t();
                f.i(format!("{c} = fcmp {pred} double {x}, {y}"));
                return Ok(bool_of(f, c));
            }
            _ => return err(format!("{p:?} is not a primitive done inline")),
        }
        Ok(r)
    }

    /// The primitives done inline because they are words and loads: lengths,
    /// bytes, characters' codes, elements. A case the inline code does not
    /// cover -- an index out of bounds, a code past the surrogates -- calls
    /// the runtime, which says what is wrong as it always has.
    fn inline_prim(
        &mut self,
        p: Prim,
        args: &[Name],
        vals: &[String],
        result: Option<Name>,
        env: &HashMap<Name, V>,
        f: &mut Fun,
    ) -> Option<String> {
        use Prim::*;
        let word = |f: &mut Fun, v: &str, i: String| {
            let p = f.ptr(v);
            let q = f.t();
            f.i(format!("{q} = getelementptr i64, ptr {p}, i64 {i}"));
            let x = f.t();
            f.i(format!("{x} = load i64, ptr {q}"));
            (x, q)
        };
        let high = |f: &mut Fun, w: &str| {
            let x = f.t();
            f.i(format!("{x} = lshr i64 {w}, 32"));
            x
        };
        match (p, vals) {
            (ArrayLen | StArrayLen, [a]) => {
                let (w0, _) = word(f, a, "0".into());
                Some(high(f, &w0))
            }
            (StringByteLength, [s]) => {
                let (w1, _) = word(f, s, "1".into());
                Some(high(f, &w1))
            }
            (CharCode, [c]) if self.rep(args[0]) == Some(Rep::Bits(desc::CHAR)) => Some(c.clone()),
            (ToInt, [x]) if self.rep(args[0]) == Some(Rep::Int) => Some(x.clone()),
            (ToInt, [x])
                if matches!(self.rep(args[0]), Some(Rep::Bits(d))
                    if (desc::WORD + 3..desc::WORD + 6).contains(&d)) =>
            {
                // `UInt8`, `UInt16`, `UInt32`: the word is the value.
                Some(x.clone())
            }
            (CharFromCode, [n]) if self.rep(args[0]) == Some(Rep::Int) => {
                Some(self.guarded(p, args, vals, env, f, |f| {
                    let c = f.t();
                    f.i(format!("{c} = icmp ult i64 {n}, 55296"));
                    (c, n.clone())
                }))
            }
            (StringByteAt, [s, i]) if self.rep(args[1]) == Some(Rep::Int) => {
                let (s, i) = (s.clone(), i.clone());
                Some(self.guarded(p, args, vals, env, f, move |f| {
                    let (w1, _) = word(f, &s, "1".into());
                    let n = high(f, &w1);
                    let c = f.t();
                    f.i(format!("{c} = icmp ult i64 {i}, {n}"));
                    // Only read past the check: computed on the fast path.
                    (c, format!("byte:{s}:{i}"))
                }))
            }
            (ArrayGet | StGetArray, [a, i]) if self.rep(args[1]) == Some(Rep::Int) => {
                let d = result.map_or(D::Known(desc::REF), |r| self.desc(r, env));
                let (a, i) = (a.clone(), i.clone());
                Some(self.guarded(p, args, vals, env, f, move |f| {
                    let (w0, _) = word(f, &a, "0".into());
                    let n = high(f, &w0);
                    let c = f.t();
                    f.i(format!("{c} = icmp ult i64 {i}, {n}"));
                    (c, format!("elem:{a}:{i}"))
                }))
                .map(|x| {
                    self.share(f, &x, &d, 1);
                    x
                })
            }
            // An element that is not a reference: stored, with nothing to
            // count -- the old one needs no erasing, the new one no sharing.
            (StSetArray, [a, i, v])
                if self.rep(args[1]) == Some(Rep::Int)
                    && matches!(
                        self.rep(args[2]),
                        Some(Rep::Int | Rep::Float | Rep::Bits(_))
                    ) =>
            {
                let (a, i, v) = (a.clone(), i.clone(), v.clone());
                Some(self.guarded(p, args, vals, env, f, move |f| {
                    let (w0, _) = word(f, &a, "0".into());
                    let n = high(f, &w0);
                    let c = f.t();
                    f.i(format!("{c} = icmp ult i64 {i}, {n}"));
                    (c, format!("store:{a}:{i}:{v}"))
                }))
            }
            // A `Ref` is a block of one field: its value is word 3, and the
            // four bits saying how to read it are in word 2. Reading and
            // writing one is what a loop with a counter in it does at every
            // turn, so neither is a call.
            (GetRef, [r]) => {
                let d = result.map_or(D::Known(desc::REF), |n| self.desc(n, env));
                let p = f.ptr(r);
                let q = f.t();
                f.i(format!("{q} = getelementptr i64, ptr {p}, i64 3"));
                let x = f.t();
                f.i(format!("{x} = load i64, ptr {q}"));
                self.share(f, &x, &d, 1);
                Some(x)
            }
            (SetRef, [r, v]) => {
                let d = self.desc(args[1], env);
                let p = f.ptr(r);
                let q = f.t();
                f.i(format!("{q} = getelementptr i64, ptr {p}, i64 3"));
                let old = f.t();
                f.i(format!("{old} = load i64, ptr {q}"));
                // What it held is gone; what it holds now is one more
                // reference. The old one is erased by the same descriptor,
                // since a `Ref` holds one type.
                self.share(f, v, &d, 1);
                f.i(format!("store i64 {v}, ptr {q}"));
                self.erase(f, &old, &d);
                if let D::Dyn(dv) = &d {
                    // A value of a type variable's type: the block says how to
                    // read it, so say it.
                    let w = f.t();
                    f.i(format!("{w} = getelementptr i64, ptr {p}, i64 2"));
                    let had = f.t();
                    f.i(format!("{had} = load i64, ptr {w}"));
                    let (cleared, bits, now) = (f.t(), f.t(), f.t());
                    f.i(format!("{cleared} = and i64 {had}, -16"));
                    f.i(format!("{bits} = and i64 {dv}, 15"));
                    f.i(format!("{now} = or i64 {cleared}, {bits}"));
                    f.i(format!("store i64 {now}, ptr {w}"));
                }
                Some("0".to_string())
            }
            // A hole filled in place, as tail recursion modulo cons does at
            // every step: `meadow_prim` for it was a call, and a count of the
            // arguments, per cell built. The field is found past the
            // descriptor words, which a block's first word says how many of.
            // What the hole held -- the placeholder, of the field's own type
            // -- is erased by the value's descriptor, and the descriptor bits
            // already say what the value is; one not known here is left to
            // the runtime, which writes them.
            (SetField, [o, i, v])
                if self.rep(args[1]) == Some(Rep::Int)
                    && matches!(self.desc(args[2], env), D::Known(_)) =>
            {
                let d = self.desc(args[2], env);
                let p = f.ptr(o);
                let w0 = f.t();
                f.i(format!("{w0} = load i64, ptr {p}"));
                let (n, n15, dw, at, idx) = (f.t(), f.t(), f.t(), f.t(), f.t());
                f.i(format!("{n} = lshr i64 {w0}, 32"));
                f.i(format!("{n15} = add i64 {n}, 15"));
                f.i(format!("{dw} = lshr i64 {n15}, 4"));
                f.i(format!("{at} = add i64 {dw}, 2"));
                f.i(format!("{idx} = add i64 {at}, {i}"));
                let q = f.t();
                f.i(format!("{q} = getelementptr i64, ptr {p}, i64 {idx}"));
                let old = f.t();
                f.i(format!("{old} = load i64, ptr {q}"));
                self.share(f, v, &d, 1);
                f.i(format!("store i64 {v}, ptr {q}"));
                self.erase(f, &old, &d);
                Some("0".to_string())
            }
            (ToFloat, [x]) if self.rep(args[0]) == Some(Rep::Int) => {
                let d = f.t();
                f.i(format!("{d} = sitofp i64 {x} to double"));
                let r = f.t();
                f.i(format!("{r} = bitcast double {d} to i64"));
                Some(r)
            }
            _ => None,
        }
    }

    /// `check` (on the fast path) gives a condition and what to answer when it
    /// holds; otherwise the runtime does `p`. `byte:s:i` and `elem:a:i` in the
    /// answer mean a byte of a string or an element of an array, read only
    /// once the check has passed.
    fn guarded(
        &mut self,
        p: Prim,
        args: &[Name],
        vals: &[String],
        env: &HashMap<Name, V>,
        f: &mut Fun,
        check: impl FnOnce(&mut Fun) -> (String, String),
    ) -> String {
        let (c, answer) = check(f);
        let (fast, slow, done) = (f.b(), f.b(), f.b());
        f.i(format!("br i1 {c}, label %{fast}, label %{slow}"));
        f.label(&fast);
        let quick = if let Some(rest) = answer.strip_prefix("byte:") {
            let (s, i) = rest.split_once(':').expect("byte:s:i");
            let p = f.ptr(s);
            let q = f.t();
            f.i(format!("{q} = getelementptr i8, ptr {p}, i64 16"));
            let r = f.t();
            f.i(format!("{r} = getelementptr i8, ptr {q}, i64 {i}"));
            let b = f.t();
            f.i(format!("{b} = load i8, ptr {r}"));
            let x = f.t();
            f.i(format!("{x} = zext i8 {b} to i64"));
            x
        } else if let Some(rest) = answer.strip_prefix("elem:") {
            let (a, i) = rest.split_once(':').expect("elem:a:i");
            let k = f.t();
            f.i(format!("{k} = add i64 {i}, 2"));
            let p = f.ptr(a);
            let q = f.t();
            f.i(format!("{q} = getelementptr i64, ptr {p}, i64 {k}"));
            let x = f.t();
            f.i(format!("{x} = load i64, ptr {q}"));
            x
        } else if let Some(rest) = answer.strip_prefix("store:") {
            let mut parts = rest.splitn(3, ':');
            let (a, i, v) = (
                parts.next().expect("store:a:i:v"),
                parts.next().expect("store:a:i:v"),
                parts.next().expect("store:a:i:v"),
            );
            let k = f.t();
            f.i(format!("{k} = add i64 {i}, 2"));
            let p = f.ptr(a);
            let q = f.t();
            f.i(format!("{q} = getelementptr i64, ptr {p}, i64 {k}"));
            f.i(format!("store i64 {v}, ptr {q}"));
            "0".to_string()
        } else {
            answer
        };
        let from_fast = f.cur.clone();
        f.i(format!("br label %{done}"));
        f.label(&slow);
        // Only ever reached for what the check refused -- which, for an
        // element or a byte, the runtime reports and does not come back from,
        // so the share an element gets after this is the fast path's alone.
        let r = self.generic(p, args, vals, &[], env, f);
        let from_slow = f.cur.clone();
        f.i(format!("br label %{done}"));
        f.label(&done);
        let out = f.t();
        f.i(format!(
            "{out} = phi i64 [ {quick}, %{from_fast} ], [ {r}, %{from_slow} ]"
        ));
        out
    }

    /// `/` and `%` on `Int`s, as the VM does them: failing on zero, and
    /// wrapping `MIN / -1` -- which LLVM's `sdiv` would leave undefined.
    fn divide(&mut self, p: Prim, a: &str, b: &str, f: &mut Fun) -> String {
        let (zero, minus, normal, done) = (f.b(), f.b(), f.b(), f.b());
        let check = f.b();
        let z = f.t();
        f.i(format!("{z} = icmp eq i64 {b}, 0"));
        f.i(format!("br i1 {z}, label %{zero}, label %{check}"));
        f.label(&zero);
        let msg = if p == Prim::Div {
            "division by zero"
        } else {
            "modulo by zero"
        };
        let (g, len) = self.cstr(msg);
        f.i(format!("call void @meadow_fail(ptr {g}, i64 {len})"));
        f.i("unreachable".into());
        f.label(&check);
        let m = f.t();
        f.i(format!("{m} = icmp eq i64 {b}, -1"));
        f.i(format!("br i1 {m}, label %{minus}, label %{normal}"));
        f.label(&minus);
        let neg = f.t();
        if p == Prim::Div {
            f.i(format!("{neg} = sub i64 0, {a}"));
        } else {
            f.i(format!("{neg} = add i64 0, 0"));
        }
        f.i(format!("br label %{done}"));
        f.label(&normal);
        let q = f.t();
        let ins = if p == Prim::Div { "sdiv" } else { "srem" };
        f.i(format!("{q} = {ins} i64 {a}, {b}"));
        f.i(format!("br label %{done}"));
        f.label(&done);
        let r = f.t();
        f.i(format!(
            "{r} = phi i64 [ {neg}, %{minus} ], [ {q}, %{normal} ]"
        ));
        r
    }

    /// A primitive the runtime does: `@meadow_prim(code, n, args, descs)`,
    /// with the arguments in memory. `args`/`vals` are the named ones,
    /// `extra` any literal operands after them.
    fn generic(
        &mut self,
        p: Prim,
        args: &[Name],
        vals: &[String],
        extra: &[(String, D)],
        env: &HashMap<Name, V>,
        f: &mut Fun,
    ) -> String {
        let mut ops: Vec<(String, D)> = args
            .iter()
            .zip(vals)
            .map(|(a, v)| (v.clone(), self.desc(*a, env)))
            .collect();
        ops.extend(extra.iter().cloned());
        let n = ops.len().max(1);
        let (av, dv) = (f.t(), f.t());
        f.alloca(format!("{av} = alloca [{n} x i64]"));
        f.alloca(format!("{dv} = alloca [{n} x i64]"));
        for (i, (v, d)) in ops.iter().enumerate() {
            let (q, r) = (f.t(), f.t());
            f.i(format!("{q} = getelementptr i64, ptr {av}, i64 {i}"));
            f.i(format!("store i64 {v}, ptr {q}"));
            f.i(format!("{r} = getelementptr i64, ptr {dv}, i64 {i}"));
            let dval = match d {
                D::Known(k) => k.to_string(),
                D::Dyn(x) => x.clone(),
            };
            f.i(format!("store i64 {dval}, ptr {r}"));
        }
        let out = f.t();
        f.i(format!(
            "{out} = call i64 @meadow_prim(i64 {}, i64 {}, ptr {av}, ptr {dv})",
            p.code(),
            ops.len()
        ));
        out
    }

    /// The whole module's text, around its functions: the method table, the
    /// runtime's declarations, the entry point and the program's tables.
    pub fn text(self, entry: meadow_seq::Label, result: i64, fingerprint: &str) -> String {
        self.units(Entries::Main(entry), result, fingerprint, usize::MAX)
            .remove(0)
    }

    /// [`Module::text`], in modules of about `unit` bytes each -- see
    /// [`Module::units`].
    pub fn text_split(
        self,
        entry: meadow_seq::Label,
        result: i64,
        fingerprint: &str,
        unit: usize,
    ) -> Vec<String> {
        self.units(Entries::Main(entry), result, fingerprint, unit)
    }

    /// [`Module::text_split`], for a test executable: a table of `entries`,
    /// and a `main` that runs the one its first argument numbers.
    pub fn text_tests(
        self,
        entries: &[meadow_seq::Label],
        fingerprint: &str,
        unit: usize,
    ) -> Vec<String> {
        self.units(
            Entries::Tests(entries.to_vec()),
            meadow_core::desc::ANY,
            fingerprint,
            unit,
        )
    }

    /// The program as LLVM modules of about `unit` bytes of functions each,
    /// to be compiled apart -- in parallel -- and linked. The first holds the
    /// tables and the entry points; each declares what it uses of the others.
    ///
    /// One module is what LLVM handles worst: its interprocedural passes grow
    /// faster than the program, and a module of the standard library's tests
    /// took tens of gigabytes. Apart, each is quick, and they use every core.
    ///
    /// **A program that never spawns pays for none of it.** Whether it can is
    /// plain here: it spawns somewhere, or it never does. When it never does,
    /// every unit gets a constant zero for the preemption flag, which folds
    /// each safe point away, and a spill area of its own, which is an
    /// ordinary global rather than something the runtime has to look up per
    /// thread. The runtime reads `@meadow_threaded` and keeps its one
    /// thread's state in a static, with no scheduler and no thread-local at
    /// all.
    fn units(
        mut self,
        entries: Entries,
        result: i64,
        fingerprint: &str,
        unit: usize,
    ) -> Vec<String> {
        let header = self.header(entries, result, fingerprint);
        let funs = std::mem::take(&mut self.funs);
        let arity: HashMap<&str, usize> = funs.iter().map(|(n, _, r)| (n.as_str(), *r)).collect();
        // Contiguous runs: a definition and its methods are written together,
        // and call one another most.
        let mut chunks: Vec<String> = vec![header];
        let mut size = 0;
        for (_, text, _) in &funs {
            if size >= unit {
                chunks.push(String::new());
                size = 0;
            }
            size += text.len();
            chunks.last_mut().expect("one").push_str(text);
        }
        let threads = self.threads_part();
        let first_only = format!(
            "@meadow_threaded = constant i8 {}\n@meadow_cycles = constant i8 {}\n\n",
            u8::from(self.threaded),
            u8::from(self.cycles)
        );
        let methods = self.methods.len();
        let strings: String = self
            .string_sizes
            .iter()
            .map(|(g, n)| format!("{g} = external hidden constant [{n} x i8]\n"))
            .collect();
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, body)| {
                let mut out = String::new();
                if i > 0 {
                    let _ = writeln!(
                        out,
                        "; A Meadow program, compiled by meadow-llvm: part {i}.\n"
                    );
                    out.push_str(RUNTIME);
                    out.push_str(&helpers(self.cycles, self.regions));
                    let _ = writeln!(
                        out,
                        "@meadow_methods = external hidden constant [{methods} x ptr]"
                    );
                    out.push_str(&strings);
                    out.push('\n');
                }
                out.push_str(&threads);
                if i == 0 {
                    out.push_str(&first_only);
                }
                out.push_str(&declarations(&body, &arity));
                out.push_str(&body);
                out
            })
            .collect()
    }

    /// What each unit says about threads: see [`Module::units`].
    fn threads_part(&self) -> String {
        let mut out = String::new();
        if self.threaded {
            out.push_str("@meadow_preempt = external hidden global i8\n");
            out.push_str("declare ptr @meadow_spill_area()\n");
        } else {
            // No `spawn` anywhere: nothing ever sets the flag, and the one
            // thread's spill area can be an ordinary global.
            out.push_str("@meadow_preempt = internal constant i8 0\n");
            out.push_str("@mw.spill = internal global [256 x i64] zeroinitializer\n");
            out.push_str(
                "define internal ptr @meadow_spill_area() alwaysinline {\n  \
                 ret ptr @mw.spill\n}\n",
            );
        }
        out.push('\n');
        out
    }

    /// The first module's own part: the runtime's declarations, the tables,
    /// and the entry points.
    fn header(&mut self, entries: Entries, result: i64, fingerprint: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "; A Meadow program, compiled by meadow-llvm.\n");
        out.push_str(RUNTIME);
        out.push_str(&helpers(self.cycles, self.regions));
        out.push_str(INVOKE1);
        let _ = writeln!(out, "@meadow_silo_{fingerprint} = external global i8");
        let _ = writeln!(
            out,
            "@meadow_runtime = constant ptr @meadow_silo_{fingerprint}\n"
        );
        let methods: Vec<String> = self.methods.iter().map(|m| format!("ptr {m}")).collect();
        let _ = writeln!(
            out,
            "@meadow_methods = hidden constant {}\n",
            array(&methods)
        );
        // Constructor names by tag, for printing.
        let mut ctors: Vec<(u32, String)> = self
            .program
            .tags
            .iter()
            .map(|(name, tag)| (*tag, name.to_string()))
            .collect();
        ctors.sort();
        let count = ctors.last().map_or(0, |(t, _)| *t as usize + 1);
        let mut names = vec!["ptr null".to_string(); count];
        for (t, name) in &ctors {
            let (g, _) = self.cstr(name);
            names[*t as usize] = format!("ptr {g}");
        }
        let _ = writeln!(out, "@meadow_ctor_names = constant {}", array(&names));
        // And the named fields of each `record` constructor, for `.field` on one.
        let mut fields = vec!["ptr null".to_string(); count];
        for (t, name) in &ctors {
            if let Some(fs) = self
                .program
                .ctor_fields
                .get(&meadow_intern::InternedString::from(name.as_str()))
            {
                let joined = fs
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let (g, _) = self.cstr(&joined);
                fields[*t as usize] = format!("ptr {g}");
            }
        }
        let _ = writeln!(out, "@meadow_ctor_fields = constant {}", array(&fields));
        let _ = writeln!(out, "@meadow_ctor_count = constant i64 {count}");
        let syms: Vec<String> = self.syms.clone().iter().map(|s| s.to_string()).collect();
        let mut sym_ptrs = Vec::new();
        for s in &syms {
            let (g, _) = self.cstr(s);
            sym_ptrs.push(format!("ptr {g}"));
        }
        let _ = writeln!(out, "@meadow_sym_names = constant {}", array(&sym_ptrs));
        let _ = writeln!(out, "@meadow_sym_count = constant i64 {}", syms.len());
        // Where each goes among a record's labels: the interned names' order.
        let mut by: Vec<usize> = (0..self.syms.len()).collect();
        by.sort_by_key(|i| self.syms[*i]);
        let mut ranks = vec![0usize; self.syms.len()];
        for (r, i) in by.iter().enumerate() {
            ranks[*i] = r;
        }
        let ranks: Vec<String> = ranks.iter().map(|r| format!("i64 {r}")).collect();
        let _ = writeln!(
            out,
            "@meadow_sym_ranks = constant {}",
            if ranks.is_empty() {
                "[0 x i64] zeroinitializer".to_string()
            } else {
                format!("[{} x i64] [{}]", ranks.len(), ranks.join(", "))
            }
        );
        let _ = writeln!(out, "@meadow_result_desc = constant i64 {result}\n");
        out.push_str(&self.string_defs);
        out.push('\n');
        match entries {
            Entries::Main(entry) => {
                let _ = writeln!(
                    out,
                    "define i64 @meadow_entry() {{\n  %r = call ghccc i64 @mw.L{}(i64 0)\n  ret i64 %r\n}}\n",
                    entry.0
                );
                out.push_str(
                    "define i32 @main(i32 %argc, ptr %argv) {\n  \
                     %r = call i32 @meadow_run(ptr @meadow_entry, i32 %argc, ptr %argv)\n  \
                     ret i32 %r\n}\n\n",
                );
            }
            Entries::Tests(labels) => {
                let mut table = Vec::new();
                for (i, l) in labels.iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "define i64 @meadow_test{i}() {{\n  %r = call ghccc i64 @mw.L{}(i64 0)\n  ret i64 %r\n}}\n",
                        l.0
                    );
                    table.push(format!("ptr @meadow_test{i}"));
                }
                let _ = writeln!(out, "@meadow_tests = constant {}\n", array(&table));
                let _ = writeln!(
                    out,
                    "define i32 @main(i32 %argc, ptr %argv) {{\n  \
                     %r = call i32 @meadow_run_test(ptr @meadow_tests, i64 {}, i32 %argc, ptr %argv)\n  \
                     ret i32 %r\n}}\n",
                    labels.len()
                );
            }
        }
        out
    }
}

/// Declarations of the functions `body` calls and does not define, which
/// another unit does.
fn declarations(body: &str, arity: &HashMap<&str, usize>) -> String {
    let mut defined = std::collections::HashSet::new();
    let mut used = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in body.lines() {
        let def = line.starts_with("define ");
        let mut rest = line;
        while let Some(at) = rest.find("@mw.") {
            let tail = &rest[at..];
            let len = 4 + tail[4..]
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(tail.len() - 4);
            let name = &tail[..len];
            rest = &tail[len..];
            if !arity.contains_key(name) {
                continue;
            }
            if def {
                defined.insert(name);
                break;
            }
            if seen.insert(name) {
                used.push(name);
            }
        }
    }
    let mut out = String::new();
    for name in used {
        if !defined.contains(name) {
            let params = vec!["i64"; arity[name]].join(", ");
            let _ = writeln!(out, "declare hidden ghccc i64 {name}({params})");
        }
    }
    out.push('\n');
    out
}

/// What the emitted code calls in the runtime, `silo/src/lib.rs`.
const RUNTIME: &str = "\
declare i64 @meadow_acquire(i64)
declare void @meadow_free(i64)
declare void @meadow_clean(i64)
declare void @meadow_fail(ptr, i64) noreturn
declare i64 @meadow_prim(i64, i64, ptr, ptr)
declare i64 @meadow_field(i64, i64)
declare i64 @meadow_text(ptr, i64)
declare i64 @meadow_bigint(i64)
declare void @meadow_preempted()
declare void @meadow_candidate(i64)
declare void @meadow_region_shared(i64)
declare void @meadow_region_erased(i64)
declare i64 @meadow_hash(i64, i64)
declare i64 @meadow_equal(i64, i64, i64, i64)
declare i64 @meadow_string_index_of(i64, i64, i64)
declare i64 @meadow_string_slice(i64, i64, i64)
declare i64 @meadow_get_ref(i64)
declare i64 @meadow_st_set(i64, i64, i64, i64)
declare i64 @meadow_array_push(i64, i64, i64)
declare i64 @meadow_array_concat(i64, i64)
declare i32 @meadow_run(ptr, i32, ptr)
declare i32 @meadow_run_test(ptr, i64, i32, ptr)
declare i64 @meadow_enter(i64, i64)
declare i64 @meadow_detach(i64, i64)
declare i64 @meadow_reattach(i64, i64)
declare i64 @meadow_native(ptr, ptr, i64, i64)
declare i64 @meadow_record(i64, ptr, ptr, ptr)
declare i64 @meadow_select(i64, i64)
declare i64 @meadow_extend(i64, i64, i64, i64)
declare i64 @meadow_array(i64, ptr, ptr)

";

/// Is `p`, whose left operand is represented as `rep`, done inline rather
/// than by the runtime? Arithmetic on `Int`s and `Float`s, and comparing
/// words the machine can compare directly.
fn inline2(p: Prim, rep: Option<Rep>) -> bool {
    use Prim::*;
    match rep {
        Some(Rep::Int) => matches!(
            p,
            Add | Sub
                | Mul
                | Div
                | Mod
                | BitAnd
                | BitOr
                | BitXor
                | Shl
                | Shr
                | Eq
                | Ne
                | Lt
                | Gt
                | Le
                | Ge
                | IntAdd
                | IntSub
                | IntMul
                | IntDiv
                | IntMod
                | IntEq
                | IntNe
                | IntLt
                | IntLe
                | IntGt
                | IntGe
        ),
        Some(Rep::Float) => matches!(
            p,
            AddF | SubF
                | MulF
                | DivF
                | LtF
                | GtF
                | LeF
                | GeF
                | Eq
                | Ne
                | Lt
                | Gt
                | Le
                | Ge
                | FloatAdd
                | FloatSub
                | FloatMul
                | FloatDiv
                | FloatEq
                | FloatNe
                | FloatLt
                | FloatLe
                | FloatGt
                | FloatGe
        ),
        // Unit, Bool, Char, and the interned names: equal as words, and a
        // `Char` ordered as its code.
        Some(Rep::Bits(d)) if d == desc::UNIT || d == desc::BOOL || d == desc::CHAR => {
            matches!(p, Eq | Ne | Lt | Gt | Le | Ge)
        }
        Some(Rep::Str) => matches!(p, Eq | Ne),
        _ => false,
    }
}

/// The typed forms specialization leaves -- `IntAdd`, `FloatLt` -- as the
/// primitive they are on words already known to be of their type.
fn untyped(p: Prim) -> Prim {
    use Prim::*;
    match p {
        IntAdd => Add,
        IntSub => Sub,
        IntMul => Mul,
        IntDiv => Div,
        IntMod => Mod,
        IntEq | FloatEq => Eq,
        IntNe | FloatNe => Ne,
        IntLt | FloatLt => Lt,
        IntLe | FloatLe => Le,
        IntGt | FloatGt => Gt,
        IntGe | FloatGe => Ge,
        FloatAdd => AddF,
        FloatSub => SubF,
        FloatMul => MulF,
        FloatDiv => DivF,
        p => p,
    }
}

fn lit_desc(l: &Lit) -> i64 {
    match l {
        Lit::Int(_) | Lit::AnyInt(..) => desc::INT,
        Lit::Float(_) | Lit::AnyFloat(..) => desc::FLOAT,
        Lit::Float32(_) => desc::FLOAT32,
        Lit::Word(w, _) => desc::word(*w),
        Lit::Char(_) => desc::CHAR,
        Lit::Bool(_) => desc::BOOL,
        Lit::Unit => desc::UNIT,
        Lit::Sym(_) => desc::STR,
        Lit::Str(_) | Lit::BigInt(_) => desc::REF,
    }
}

/// How many of a function's parameters travel in registers. The functions are
/// in LLVM's GHC convention, which passes ten words in registers on x86-64 and
/// more on aarch64, and whose tail calls LLVM makes as long as nothing goes on
/// the stack: `tailcc` guarantees them too, but on Windows only for arguments
/// that fit its four registers, and an AxCut environment is often bigger.
const REGS: usize = 10;

/// What does not fit [`REGS`] goes through the running thread's spill area
/// (`meadow_spill_area`): stored just before the call, and loaded by the
/// callee first thing, before anything else could use it.
///
/// The area is asked for at each site rather than read from a thread-local
/// the compiler could keep across a call: a thread that waits can carry on
/// on another OS thread, and an address from before the wait would be that
/// OS thread's.
fn spill(f: &mut Fun, ops: &[String]) {
    if ops.len() <= REGS {
        return;
    }
    let area = f.t();
    f.i(format!("{area} = call ptr @meadow_spill_area()"));
    for (i, o) in ops.iter().enumerate().skip(REGS) {
        let q = f.t();
        f.i(format!(
            "{q} = getelementptr i64, ptr {area}, i64 {}",
            i - REGS
        ));
        f.i(format!("store i64 {o}, ptr {q}"));
    }
}

/// The loads of a function's parameters past [`REGS`], at its entry.
fn spilled_params(params: &[String]) -> String {
    let mut out = String::new();
    if params.len() <= REGS {
        return out;
    }
    let _ = writeln!(out, "  %spill.at = call ptr @meadow_spill_area()");
    for (i, p) in params.iter().enumerate().skip(REGS) {
        let _ = writeln!(
            out,
            "  {p}.at = getelementptr i64, ptr %spill.at, i64 {}\n  {p} = load i64, ptr {p}.at",
            i - REGS
        );
    }
    out
}

/// `[n x ptr]` of `elems`, each written `ptr …` -- `zeroinitializer` when
/// there are none, which LLVM wants for an empty array.
fn array(elems: &[String]) -> String {
    if elems.is_empty() {
        "[0 x ptr] zeroinitializer".into()
    } else {
        format!("[{} x ptr] [{}]", elems.len(), elems.join(", "))
    }
}

fn env_with(env: &HashMap<Name, V>, names: &[Name], vals: &[String]) -> HashMap<Name, V> {
    let mut e = env.clone();
    for (n, v) in names.iter().zip(vals) {
        e.insert(*n, V::Val(v.clone()));
    }
    e
}

/// A function being written.
struct Fun {
    name: String,
    params: Vec<String>,
    body: String,
    allocas: String,
    tmp: usize,
    blk: usize,
    cur: String,
    /// Where each reuse token made in this function came from, by its
    /// operand: see [`Module::build_in`].
    origins: HashMap<String, Origin>,
}

/// The block a reuse token holds, as the `switch` that took it apart left
/// it: which constructor, and what each field held and how it is described.
/// Only the reference count and the fields' ownership changed hands; the
/// words themselves are as they were.
#[derive(Clone)]
struct Origin {
    meta: u64,
    fields: Vec<String>,
    descs: Vec<D>,
}

impl Fun {
    fn new(name: String, params: Vec<String>) -> Fun {
        Fun {
            name,
            params,
            body: String::new(),
            allocas: String::new(),
            tmp: 0,
            blk: 0,
            cur: "entry".into(),
            origins: HashMap::new(),
        }
    }

    fn t(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    fn b(&mut self) -> String {
        self.blk += 1;
        format!("b{}", self.blk)
    }

    fn i(&mut self, s: String) {
        self.body.push_str("  ");
        self.body.push_str(&s);
        self.body.push('\n');
    }

    fn alloca(&mut self, s: String) {
        self.allocas.push_str("  ");
        self.allocas.push_str(&s);
        self.allocas.push('\n');
    }

    fn label(&mut self, l: &str) {
        let _ = writeln!(self.body, "{l}:");
        self.cur = l.to_string();
    }

    /// `v` as a `ptr`.
    fn ptr(&mut self, v: &str) -> String {
        let p = self.t();
        self.i(format!("{p} = inttoptr i64 {v} to ptr"));
        p
    }
}

/// What a module's `main` runs: the program's entry point, or one of a table
/// of tests.
enum Entries {
    Main(meadow_seq::Label),
    Tests(Vec<meadow_seq::Label>),
}

/// Sharing and erasing, the paper's `share` and `erase`, as the module's own
/// small functions: skip `0` and odd words (no block), and a value whose
/// descriptor says it is no reference; add to the count, or take from it --
/// handing the block to the runtime when it was the last.
/// The helpers as a program gets them. One that can tie a knot hands every
/// block whose count went down without reaching zero to the collector, which
/// is where a cycle is noticed (`silo/src/cycles.rs`); one that cannot has no
/// such call anywhere in it.
fn helpers(cycles: bool, regions: bool) -> String {
    // The test before the call is inline, and on a program that makes no
    // cycles it is the whole cost, so it is one mask and one compare against
    // a constant. A block is worth keeping only if it is a `Ref` or a mutable
    // array -- the two kinds a cycle must contain, marked as such when they
    // are built -- and only the first time its count goes down, since it is
    // coloured until a collection looks at it. Both questions are in the same
    // word, so both are asked at once: `MUTABLE` set and the colour black.
    // `silo/src/cycles.rs` says why those two kinds, `silo/src/heap.rs` has the
    // bits, and the two must agree.
    let candidate = if cycles {
        "  %w1a = getelementptr i64, ptr %p, i64 1
  %w1 = load i64, ptr %w1a
  %ask = and i64 %w1, 286720
  %want = icmp eq i64 %ask, 262144
  br i1 %want, label %cand, label %done, !prof !0
cand:
  call void @meadow_candidate(i64 %v)
  br label %done
"
    } else {
        "  br label %done\n"
    };
    // A block inside a compact region carries far more references than it
    // has, so the count a share or an erase has already loaded says whether
    // it is touching one -- and the region has to be told, since that is what
    // keeps it alive while anything points into it. `silo/src/region.rs` says
    // why, and `silo/src/heap.rs` has the number, which must be this one.
    let share_tail = if regions {
        "  %inreg = icmp ugt i32 %rc2, 536870912
  br i1 %inreg, label %rshare, label %done, !prof !0
rshare:
  call void @meadow_region_shared(i64 %v)
  br label %done
"
        .to_string()
    } else {
        "  br label %done\n".to_string()
    };
    // The erase side asks before it takes one away, and a block in a region
    // is never a candidate for the cycle collector: a region is closed, and
    // nothing in it can change.
    let erase_tail = if regions {
        format!(
            "  %inreg = icmp ugt i32 %rc, 536870912
  br i1 %inreg, label %rerase, label %noreg, !prof !0
rerase:
  call void @meadow_region_erased(i64 %v)
  br label %done
noreg:
{candidate}"
        )
    } else {
        candidate.to_string()
    };
    // Counting happens everywhere and both of these are rare, so say which
    // way the tests go: the calls and their setup are laid out away from the
    // path every share and every decrement takes.
    let cold = if cycles || regions {
        "\n!0 = !{!\"branch_weights\", i32 1, i32 4096}\n"
    } else {
        ""
    };
    HELPERS
        .replace("; region-share\n", &share_tail)
        .replace("; candidate\n", &erase_tail)
        + cold
}

const HELPERS: &str = "\
define internal void @mw.share(i64 %v, i32 %n) alwaysinline {
entry:
  %low = and i64 %v, 1
  %even = icmp eq i64 %low, 0
  %nz = icmp ne i64 %v, 0
  %block = and i1 %even, %nz
  br i1 %block, label %go, label %done
go:
  %p = inttoptr i64 %v to ptr
  %rc = load i32, ptr %p
  %rc2 = add i32 %rc, %n
  store i32 %rc2, ptr %p
; region-share
done:
  ret void
}

define internal void @mw.erase(i64 %v) alwaysinline {
entry:
  %low = and i64 %v, 1
  %even = icmp eq i64 %low, 0
  %nz = icmp ne i64 %v, 0
  %block = and i1 %even, %nz
  br i1 %block, label %go, label %done
go:
  %p = inttoptr i64 %v to ptr
  %rc = load i32, ptr %p
  %last = icmp eq i32 %rc, 0
  br i1 %last, label %free, label %dec
free:
  call void @meadow_free(i64 %v)
  br label %done
dec:
  %rc2 = sub i32 %rc, 1
  store i32 %rc2, ptr %p
; candidate
done:
  ret void
}

define internal void @mw.share_d(i64 %v, i64 %d, i32 %n) alwaysinline {
entry:
  %isref = icmp eq i64 %d, 0
  br i1 %isref, label %go, label %done
go:
  call void @mw.share(i64 %v, i32 %n)
  br label %done
done:
  ret void
}

define internal void @mw.erase_d(i64 %v, i64 %d) alwaysinline {
entry:
  %isref = icmp eq i64 %d, 0
  br i1 %isref, label %go, label %done
go:
  call void @mw.erase(i64 %v)
  br label %done
done:
  ret void
}

";

/// How the runtime runs a closure of one argument -- the code after a stack
/// segment primitive (`silo/src/segments.rs`) -- when the methods are in a
/// calling convention Rust cannot call: method 0 of `obj`, found as an
/// `invoke` finds it, called from the C convention.
const INVOKE1: &str = "\
define i64 @meadow_invoke1(i64 %obj, i64 %arg) {
entry:
  %low = and i64 %obj, 1
  %odd = icmp ne i64 %low, 0
  br i1 %odd, label %imm, label %blk
imm:
  %m1 = lshr i64 %obj, 1
  br label %go
blk:
  %p = inttoptr i64 %obj to ptr
  %q = getelementptr i64, ptr %p, i64 1
  %w = load i64, ptr %q
  %m2 = lshr i64 %w, 32
  br label %go
go:
  %m = phi i64 [ %m1, %imm ], [ %m2, %blk ]
  %slot = getelementptr ptr, ptr @meadow_methods, i64 %m
  %fp = load ptr, ptr %slot
  %r = call ghccc i64 %fp(i64 %obj, i64 %arg)
  ret i64 %r
}

define i64 @meadow_apply(i64 %obj, i64 %arg, i64 %ev) {
entry:
  %low = and i64 %obj, 1
  %odd = icmp ne i64 %low, 0
  br i1 %odd, label %imm, label %blk
imm:
  %m1 = lshr i64 %obj, 1
  br label %go
blk:
  %p = inttoptr i64 %obj to ptr
  %q = getelementptr i64, ptr %p, i64 1
  %w = load i64, ptr %q
  %m2 = lshr i64 %w, 32
  br label %go
go:
  %m = phi i64 [ %m1, %imm ], [ %m2, %blk ]
  %slot = getelementptr ptr, ptr @meadow_methods, i64 %m
  %fp = load ptr, ptr %slot
  %r = call ghccc i64 %fp(i64 %obj, i64 %arg, i64 0, i64 %ev)
  ret i64 %r
}

";

/// The primitives with an entry of their own in the runtime, and which of
/// their arguments it wants a descriptor beside. These are the ones a program
/// does so often that building an argument array for [`Module::generic`] costs
/// more than the primitive does: `wordfreq` alone makes three and a half
/// million of these calls.
fn direct(p: Prim) -> Option<(&'static str, Descs)> {
    Some(match p {
        Prim::Hash => ("meadow_hash", Descs::Each),
        Prim::Eq => ("meadow_equal", Descs::Each),
        Prim::StringIndexOf => ("meadow_string_index_of", Descs::None),
        Prim::StringSlice => ("meadow_string_slice", Descs::None),
        // The element's descriptor, and no other's: the array and the index
        // are what they are.
        Prim::StSetArray => ("meadow_st_set", Descs::Last),
        // These two consume the array they grow, which is what lets them grow
        // it in place: see [`consumes`].
        Prim::ArrayPush => ("meadow_array_push", Descs::Last),
        Prim::ArrayConcat => ("meadow_array_concat", Descs::None),
        _ => return None,
    })
}

/// The argument a primitive **consumes** rather than borrows, if it does.
///
/// Every other primitive borrows its arguments. One that consumes is handed a
/// reference of its own: the linearization shares the argument first where it
/// is used again, and otherwise gives up the caller's. So a count of zero inside
/// the primitive means nobody else can see the value, and it may be changed in
/// place -- which is how `arrayPush` grows an array without copying it.
pub fn consumes(op: &meadow_seq::Extern) -> Option<usize> {
    match op {
        meadow_seq::Extern::Prim(Prim::ArrayPush | Prim::ArrayConcat) => Some(0),
        _ => None,
    }
}

/// Which arguments of a [`direct`] primitive carry a descriptor.
#[derive(Clone, Copy, PartialEq)]
enum Descs {
    None,
    Each,
    Last,
}
