//! Loops over arrays of numbers, two elements at a time.
//!
//! A loop the compiler has made a jump (see `meadow_core::inline`'s loops)
//! arrives here as a run of bytecode from a header to the jump back to it.
//! Where its body is index arithmetic, reads and writes of arrays that never
//! change during the loop, and float arithmetic on what was read, it is what a
//! vector unit is for: two doubles per instruction, and none of the checks an
//! access makes, since every one of them can be made once before the loop.
//!
//! What is made here is a **plan**: which register counts, which registers
//! are the arrays, what each instruction of the body becomes, and what has to
//! be true before the plan can run. The architecture's code (`a64`) emits it
//! as a *preheader* that checks the conditions, a loop that does two
//! iterations per trip, and a fall into the ordinary scalar loop for whatever
//! is left -- the last iteration of an odd count, or all of them, if a check
//! fails. The scalar loop is never changed, so the plan is only ever a way of
//! doing what it does faster, and doing none of it is always right.
//!
//! # What qualifies
//!
//! * The header is `bri j >= hi` (or `>= k`), exiting the loop; the body ends
//!   in `addik j <- j + 1` and the jump back. `j` is the **induction
//!   register**; `hi` is never written in the body.
//! * Every register the body reads before writing is either `j` or never
//!   written in the body at all -- an *invariant*. Nothing is carried from one
//!   iteration to the next through a register: what the loop carries, it
//!   carries through the arrays.
//! * Integer arithmetic is on invariants and `j`, and every array index is
//!   `j` plus something invariant: **stride one**, so the element after is
//!   the next word, and two elements are one 128-bit load.
//! * Arrays are read with `stGetArray`/`arrayGet` and written with
//!   `stSetArray`, each on an invariant register; what is read is used as a
//!   `Float` (by `mulf` and the rest), and what is written was made by one.
//! * Nothing else: no calls, no allocation, no branches inside the body, and
//!   no register the body writes is read after the loop, which lowering's
//!   join points guarantee -- the header's parameters are all that leaves.
//!
//! # What is checked at run time, once
//!
//! That each array is the kind expected, with `Float` elements; that every
//! index the whole loop will use is inside its array, which is the body's
//! own index arithmetic run twice in the preheader, at `j`'s first and last
//! values; and that an array written is not also read at a different index
//! through another register that happens to name the same object.

use super::{FloatOp, Operand};
use meadow_bytecode::{Cond, Instr, Op, Program, Reg};
use meadow_core::{Prim, desc};
use std::collections::HashMap;
use std::rc::Rc;

/// An integer the body computes, as an expression over the invariants and
/// `j`: what two index registers are compared by, since lowering computes
/// the same index into two registers as readily as one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Reg(Reg),
    J,
    Const(u64),
    Add(Rc<Expr>, Rc<Expr>),
    Sub(Rc<Expr>, Rc<Expr>),
    Mul(Rc<Expr>, Rc<Expr>),
    Shl(Rc<Expr>, Rc<Expr>),
}

/// One invariant array the loop touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Array {
    pub reg: Reg,
    /// `Kind::MutArray` or `Kind::Array`, as `crate::heap::Kind`'s number.
    pub kind: u8,
    /// Written to somewhere in the body.
    pub stored: bool,
}

/// A float value inside the vector body: two lanes in a vector register, or
/// an invariant scalar, broadcast to both lanes before the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lanes {
    /// Vector register `v0 + n`.
    V(u8),
    /// Invariant register, broadcast: the `n`th of [`Plan::broadcasts`].
    B(u8),
}

/// One instruction of the body, as the vector loop does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VOp {
    /// Integer arithmetic or a move, done as the scalar code does it: on the
    /// fixed registers, for lane zero. The lane after is the next element,
    /// by stride one.
    Scalar(Instr),
    /// `r[reg] = word`, likewise.
    Const(Reg, u64),
    /// `V(lane) = ` two elements of array `array` from index `r[idx]`.
    Load { lane: u8, array: usize, idx: Reg },
    /// Two elements of array `array` at index `r[idx]` `= val`.
    Store { array: usize, idx: Reg, val: Lanes },
    /// `V(lane) = a op b`, lane by lane.
    Float {
        op: FloatOp,
        lane: u8,
        a: Lanes,
        b: Lanes,
    },
}

/// An access the preheader has to bound: the register holding its index at
/// the point of the access, and the array it is into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub idx: Reg,
    pub array: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub j: Reg,
    pub hi: Operand,
    pub arrays: Vec<Array>,
    /// Invariant float registers the body reads, in [`Lanes::B`] order.
    pub broadcasts: Vec<Reg>,
    pub ops: Vec<VOp>,
    /// Every read and write, for the bounds checks.
    pub accesses: Vec<Access>,
    /// Pairs of arrays that must not be the same object: one is written and
    /// the other read at an index the body computes differently.
    pub distinct: Vec<(usize, usize)>,
    /// How many vector registers the lanes use.
    pub lanes: u8,
    /// Instructions per iteration of the scalar loop, for the step count.
    pub steps: u32,
}

/// The most lanes a plan may use: `v0` to `v7`, which nothing else in a block
/// function holds across an instruction.
pub const LANES: u8 = 8;

/// What a register holds, symbolically, while the body is read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sym {
    /// An integer: how many times `j` it contains, and an index it is the
    /// index expression of -- `j` plus something invariant -- when `j == 1`.
    /// `alias` is the invariant register it is a plain copy of, if it is
    /// one: lowering moves arguments into a window before a call, and the
    /// array a store names is such a copy.
    Int {
        j: i64,
        invariant: bool,
        expr: Rc<Expr>,
    },
    Float(Lanes),
}

/// The plan for the loop whose header is at `pcs[0]` and whose jump back is
/// at `pcs[last]`, if it has one.
pub fn plan(program: &Program, pcs: &[usize], fixed: usize) -> Option<Plan> {
    match plan_or(program, pcs, fixed) {
        Ok(p) => Some(p),
        Err(line) => {
            if std::env::var_os("MEADOW_VECTOR_DEBUG").is_some() {
                eprintln!("vector: loop at {} refused at vector.rs:{line}", pcs[0]);
            }
            None
        }
    }
}

/// [`plan`], saying which rule refused: the line of it.
fn plan_or(program: &Program, pcs: &[usize], fixed: usize) -> Result<Plan, u32> {
    let code = &program.code;
    if pcs.len() < 3 {
        return Err(line!());
    }
    // The header: exit when `j >= hi`.
    let head = code[pcs[0]];
    let (j, hi) = match head.op {
        Op::BrI if Cond::from_byte(head.c as u32) == Some(Cond::Ge) => {
            (head.a, Operand::Reg(head.b))
        }
        Op::BrIK if Cond::from_byte(head.c as u32) == Some(Cond::Ge) => {
            (head.a, Operand::Imm(head.b as i8 as i64))
        }
        _ => return Err(line!()),
    };
    // The end: `addik j <- j + 1`, then the jump back.
    let &last = pcs.last().ok_or(line!())?;
    let bump = code[pcs[pcs.len() - 2]];
    if code[last].op != Op::Jump
        || code[last].imm as usize != pcs[0]
        || bump.op != Op::AddIK
        || bump.a != j
        || bump.b != j
        || bump.imm != 1
    {
        return Err(line!());
    }
    // The header branches over the loop's exit code to the body proper.
    let start = (head.imm as usize).checked_sub(pcs[0]).ok_or(line!())?;
    if start < 1 || start > pcs.len() - 2 {
        return Err(line!());
    }
    let body = &pcs[start..pcs.len() - 2];
    // Registers the body writes: none may be read as a live-in but `j`.
    let written: std::collections::HashSet<Reg> = body
        .iter()
        .filter_map(|&pc| super::writes(&code[pc]))
        .collect();
    if written.contains(&j) {
        return Err(line!());
    }
    if let Operand::Reg(h) = hi
        && written.contains(&h)
    {
        return Err(line!());
    }
    let mut syms: HashMap<Reg, Sym> = HashMap::new();
    let mut arrays: Vec<Array> = Vec::new();
    let mut broadcasts: Vec<Reg> = Vec::new();
    let mut ops = Vec::new();
    let mut accesses = Vec::new();
    let mut lanes: u8 = 0;
    // Every load and store, for the aliasing rule: `(array, index expression)`.
    let mut loads: Vec<(usize, Rc<Expr>)> = Vec::new();
    let mut stores: Vec<(usize, Rc<Expr>)> = Vec::new();

    let read_int = |syms: &HashMap<Reg, Sym>, r: Reg| -> Option<Sym> {
        match syms.get(&r) {
            Some(s) => Some(s.clone()),
            None if r == j => Some(Sym::Int {
                j: 1,
                invariant: true,
                expr: Rc::new(Expr::J),
            }),
            None if !written.contains(&r) => Some(Sym::Int {
                j: 0,
                invariant: true,
                expr: Rc::new(Expr::Reg(r)),
            }),
            None => None,
        }
    };
    let mut read_float = |syms: &HashMap<Reg, Sym>, r: Reg| -> Option<Lanes> {
        match syms.get(&r) {
            Some(Sym::Float(l)) => Some(*l),
            Some(Sym::Int { .. }) => None,
            None if r != j && !written.contains(&r) => {
                let n = match broadcasts.iter().position(|b| *b == r) {
                    Some(n) => n,
                    None => {
                        broadcasts.push(r);
                        broadcasts.len() - 1
                    }
                };
                Some(Lanes::B(n as u8))
            }
            None => None,
        }
    };
    let array_of = |arrays: &mut Vec<Array>,
                    syms: &HashMap<Reg, Sym>,
                    r: Reg,
                    kind: u8,
                    stored: bool|
     -> Option<usize> {
        // The register itself, if invariant, or the invariant it copies.
        let r = match syms.get(&r) {
            Some(Sym::Int { expr, .. }) => match &**expr {
                Expr::Reg(src) => *src,
                _ => return None,
            },
            Some(Sym::Float(_)) => return None,
            None if !written.contains(&r) && r != j => r,
            None => return None,
        };
        match arrays.iter().position(|a| a.reg == r) {
            Some(n) => {
                if arrays[n].kind != kind {
                    return None;
                }
                arrays[n].stored |= stored;
                Some(n)
            }
            None => {
                arrays.push(Array {
                    reg: r,
                    kind,
                    stored,
                });
                Some(arrays.len() - 1)
            }
        }
    };
    let new_lane = |lanes: &mut u8| -> Option<u8> {
        if *lanes >= LANES {
            return None;
        }
        *lanes += 1;
        Some(*lanes - 1)
    };
    // An index: `j` once, plus invariants -- as the register holding it, and
    // the expression it holds.
    let index_of = |syms: &HashMap<Reg, Sym>, r: Reg| -> Option<(Reg, Rc<Expr>)> {
        match read_int(syms, r)? {
            Sym::Int {
                j: 1,
                invariant: true,
                expr,
            } => Some((r, expr)),
            _ => None,
        }
    };

    for &pc in body {
        let i = code[pc];
        match i.op {
            Op::Move => {
                let s = match syms.get(&i.b) {
                    Some(s) => s.clone(),
                    None => read_int(&syms, i.b).ok_or(line!())?,
                };
                // A move of an integer is done; a move of lanes is a name.
                if let Sym::Int { .. } = s {
                    ops.push(VOp::Scalar(i));
                }
                syms.insert(i.a, s);
            }
            Op::Const => {
                let w = super::immediate(program, i.imm).ok_or(line!())?;
                syms.insert(
                    i.a,
                    Sym::Int {
                        j: 0,
                        invariant: true,
                        expr: Rc::new(Expr::Const(w)),
                    },
                );
                ops.push(VOp::Const(i.a, w));
            }
            Op::AddIK | Op::SubIK | Op::ShlIK | Op::MulIK => {
                let Sym::Int {
                    j: jb,
                    invariant,
                    expr,
                } = read_int(&syms, i.b).ok_or(line!())?
                else {
                    return Err(line!());
                };
                let jj = match i.op {
                    Op::AddIK | Op::SubIK => jb,
                    // Scaled: only an invariant may be.
                    _ if jb == 0 => 0,
                    _ => return Err(line!()),
                };
                let k = Rc::new(Expr::Const(i.imm as i32 as i64 as u64));
                let e = match i.op {
                    Op::AddIK => Expr::Add(expr, k),
                    Op::SubIK => Expr::Sub(expr, k),
                    Op::ShlIK => Expr::Shl(expr, k),
                    _ => Expr::Mul(expr, k),
                };
                syms.insert(
                    i.a,
                    Sym::Int {
                        j: jj,
                        invariant,
                        expr: Rc::new(e),
                    },
                );
                ops.push(VOp::Scalar(i));
            }
            Op::AddI | Op::SubI | Op::MulI => {
                let Sym::Int {
                    j: jb,
                    invariant: ib,
                    expr: eb,
                } = read_int(&syms, i.b).ok_or(line!())?
                else {
                    return Err(line!());
                };
                let Sym::Int {
                    j: jc,
                    invariant: ic,
                    expr: ec,
                } = read_int(&syms, i.c).ok_or(line!())?
                else {
                    return Err(line!());
                };
                let jj = match i.op {
                    Op::AddI => jb + jc,
                    Op::SubI => jb - jc,
                    _ if jb == 0 && jc == 0 => 0,
                    _ => return Err(line!()),
                };
                let e = match i.op {
                    Op::AddI => Expr::Add(eb, ec),
                    Op::SubI => Expr::Sub(eb, ec),
                    _ => Expr::Mul(eb, ec),
                };
                syms.insert(
                    i.a,
                    Sym::Int {
                        j: jj,
                        invariant: ib && ic,
                        expr: Rc::new(e),
                    },
                );
                ops.push(VOp::Scalar(i));
            }
            Op::MulF | Op::AddF | Op::SubF | Op::DivF => {
                let a = read_float(&syms, i.b).ok_or(line!())?;
                let b = read_float(&syms, i.c).ok_or(line!())?;
                if matches!((a, b), (Lanes::B(_), Lanes::B(_))) {
                    return Err(line!());
                }
                let op = match i.op {
                    Op::MulF => FloatOp::Mul,
                    Op::AddF => FloatOp::Add,
                    Op::SubF => FloatOp::Sub,
                    _ => FloatOp::Div,
                };
                let lane = new_lane(&mut lanes).ok_or(line!())?;
                ops.push(VOp::Float { op, lane, a, b });
                syms.insert(i.a, Sym::Float(Lanes::V(lane)));
            }
            Op::Prim2 => {
                let kind = match program.prims.get(i.imm as usize).ok_or(line!())? {
                    Prim::StGetArray => crate::heap::Kind::MutArray as u8,
                    Prim::ArrayGet => crate::heap::Kind::Array as u8,
                    _ => return Err(line!()),
                };
                let array = array_of(&mut arrays, &syms, i.b, kind, false).ok_or(line!())?;
                let (idx, expr) = index_of(&syms, i.c).ok_or(line!())?;
                let lane = new_lane(&mut lanes).ok_or(line!())?;
                ops.push(VOp::Load { lane, array, idx });
                accesses.push(Access { idx, array });
                loads.push((array, expr));
                syms.insert(i.a, Sym::Float(Lanes::V(lane)));
            }
            Op::Prim if i.c == 3 => {
                if !matches!(
                    program.prims.get(i.imm as usize).ok_or(line!())?,
                    Prim::StSetArray
                ) {
                    return Err(line!());
                }
                let (arr, idx, val) = (i.b, i.b + 1, i.b + 2);
                let array = array_of(
                    &mut arrays,
                    &syms,
                    arr,
                    crate::heap::Kind::MutArray as u8,
                    true,
                )
                .ok_or(line!())?;
                let (idx, expr) = index_of(&syms, idx).ok_or(line!())?;
                let val = read_float(&syms, val).ok_or(line!())?;
                ops.push(VOp::Store { array, idx, val });
                accesses.push(Access { idx, array });
                stores.push((array, expr));
                // `stSetArray` answers unit, which nothing here reads.
                syms.insert(
                    i.a,
                    Sym::Int {
                        j: 0,
                        invariant: true,
                        expr: Rc::new(Expr::Const(0)),
                    },
                );
            }
            _ => return Err(line!()),
        }
    }
    if arrays.is_empty() || stores.is_empty() {
        return Err(line!());
    }
    // An index register is read at the access, so it must hold the index
    // then: it is, since every register written is written by a scalar op
    // that runs in order in the vector loop too.
    //
    // Aliasing: two registers can name one array. A store through one and a
    // load through the other at an index computed by another register may
    // then be the same element in a lane the other way round. Where both
    // accesses go through the *same* index register, the lanes line up and
    // nothing is lost. Otherwise the objects have to be different, which
    // the preheader checks.
    let mut distinct = Vec::new();
    for (sa, se) in &stores {
        for (la, le) in &loads {
            let same = se == le;
            if la != sa
                && !same
                && !distinct.contains(&(*sa, *la))
                && !distinct.contains(&(*la, *sa))
            {
                distinct.push((*sa, *la));
            }
            if la == sa && !same {
                // One array, two index expressions: a carried dependence
                // the vector cannot honour.
                return Err(line!());
            }
        }
    }
    // The fixed registers are what the vector body works on.
    let fixed = |r: Reg| (r as usize) < fixed;
    if !fixed(j)
        || !arrays.iter().all(|a| fixed(a.reg))
        || !accesses.iter().all(|a| fixed(a.idx))
        || !broadcasts.iter().all(|r| fixed(*r))
    {
        return Err(line!());
    }
    if let Operand::Reg(h) = hi
        && !fixed(h)
    {
        return Err(line!());
    }
    Ok(Plan {
        j,
        hi,
        arrays,
        broadcasts,
        ops,
        accesses,
        distinct,
        lanes,
        steps: pcs.len() as u32,
    })
}

/// The element descriptor every array of the plan must have: what the loop
/// reads as floats and writes from floats.
pub const ELEMENT: desc::Desc = desc::FLOAT;
