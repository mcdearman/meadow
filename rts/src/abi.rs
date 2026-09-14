//! The boundary with native code: what code compiled from bytecode calls, and
//! how the machine runs it.
//!
//! # The model
//!
//! Native code replaces the interpreter a *block* at a time. A block is the
//! code starting at a pc that control enters from elsewhere -- a method's
//! entry, a definition, the target of a jump -- together with every instruction
//! reachable from there without leaving: falling through, and taking a
//! conditional branch. A block's native function runs from its pc until
//! control leaves -- an invoke, a jump, a halt, a failure, or a thread
//! operation the scheduler has to carry out -- sets the pc, and returns.
//! [`crate::Vm::advance`] then enters whatever is at the new pc: another native
//! block, or the interpreter, a single instruction at a time, where there is
//! none. So native and interpreted code mix freely, and every thread, the
//! collector and the scheduler go on working exactly as they do for bytecode.
//!
//! # What native code may do
//!
//! Everything the machine's state is, it reaches through the `*mut Vm` it is
//! handed: the registers, and the instructions it calls back to run. It holds
//! no [`crate::Value`] of its own across a call back into the machine, since a
//! call may collect and move what an address named; the registers are the
//! roots, as they are for the interpreter.
//!
//! A block function's status says why it returned:
//!
//! | status | meaning |
//! |---|---|
//! | [`JUMPED`] | control left the block; the pc says where |
//! | [`HALTED`] | the program finished; the value is in the machine |
//! | [`FAILED`] | it failed; the error is in the machine |
//! | [`REQUESTED`] | a thread operation is waiting for the scheduler |

use std::ffi::c_void;

use meadow_bytecode::Pc;

use crate::value::Value;
use crate::vm::{Error, Vm};

/// A block's native code. Its argument is the machine, as `*mut Vm`.
pub type NativeFn = unsafe extern "C" fn(vm: *mut c_void) -> u32;

/// Native code per pc: the function for a block starting there, if any.
pub type NativeTable = [Option<NativeFn>];

/// An instruction finished and control falls through to the next.
pub const CONTINUE: u32 = 0;
/// Control went elsewhere: a branch was taken, or a jump or an invoke set the pc.
pub const JUMPED: u32 = 1;
pub const HALTED: u32 = 2;
pub const FAILED: u32 = 3;
pub const REQUESTED: u32 = 4;

/// Run the instruction at `pc`, the way the interpreter would, and say what
/// became of control: [`CONTINUE`] if it falls through to `pc + 1`, or a status
/// native code returns with.
///
/// # Safety
///
/// `vm` must be the machine the calling native code was entered with.
pub unsafe extern "C" fn meadow_exec(vm: *mut c_void, pc: u32) -> u32 {
    // Safety: native code only ever passes back the machine it was entered
    // with, and holds no other reference to it.
    let vm = unsafe { &mut *(vm as *mut Vm<'static>) };
    let Some(&i) = vm.program.code.get(pc as usize) else {
        vm.failure = Some(Error {
            msg: format!("pc {pc} is outside the program"),
        });
        return FAILED;
    };
    let next = pc as usize + 1;
    vm.pc = next;
    vm.steps += 1;
    match vm.exec(i) {
        Err(e) => {
            vm.failure = Some(e);
            FAILED
        }
        Ok(Some(v)) => {
            vm.halted = Some(v);
            HALTED
        }
        Ok(None) if vm.request.is_some() => REQUESTED,
        Ok(None) if vm.pc != next => JUMPED,
        Ok(None) => CONTINUE,
    }
}

/// Enter the native block `f` at the machine's pc, and run it until it leaves.
pub(crate) fn enter(vm: &mut Vm, f: NativeFn) -> Result<Option<Value>, Error> {
    // Safety: `f` was compiled from this machine's program, and gets the
    // machine exclusively for the call.
    let status = unsafe { f(vm as *mut Vm as *mut c_void) };
    match status {
        HALTED => Ok(Some(vm.halted.take().unwrap_or(Value::Unit))),
        FAILED => Err(vm.failure.take().unwrap_or_else(|| Error {
            msg: "native code failed without saying why".into(),
        })),
        _ => Ok(None),
    }
}

/// The entry pcs a program's native code is compiled for: every block control
/// can enter from elsewhere.
pub fn block_entries(program: &meadow_bytecode::Program) -> Vec<Pc> {
    use meadow_bytecode::Op;
    let mut entries: Vec<Pc> = program.entries.clone();
    entries.extend(program.entry);
    for m in &program.methods {
        entries.extend(m.iter().copied());
    }
    for i in &program.code {
        if i.op == Op::Jump {
            entries.push(i.imm);
        }
    }
    entries.retain(|pc| (*pc as usize) < program.code.len());
    entries.sort_unstable();
    entries.dedup();
    entries
}
