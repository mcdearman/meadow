//! **Native code for a running program**: ahead of time, or as it runs.
//!
//! [`Native`] is what a machine consults at each block it enters (see
//! [`crate::Vm::advance`]): the function compiled for the block, if there is
//! one. Every thread of a run shares one, so a block compiled on one thread
//! is native on all of them.
//!
//! # Ahead of time
//!
//! [`Native::ahead_of_time`]: every block's function, from an executable the
//! linker made (see [`crate::aot`]). Nothing is compiled while the program runs.
//!
//! # Just in time
//!
//! [`Native::jit`]: nothing, to begin with. The machine interprets, and counts
//! the times it enters each block; a block entered `threshold` times is
//! compiled then (see [`crate::codegen::compile_block`]) -- under a lock, so
//! it is compiled once however many threads get there -- and run natively from
//! then on, by every thread. Code that runs once -- most of what a program's
//! start does -- is never compiled at all, and what runs in a loop is compiled
//! within a few iterations.
//!
//! The translation is the same either way, and needs no relocation: a block's
//! function can go anywhere, which is what lets it be compiled on its own.
//!
//! # Executable memory
//!
//! Platform business. On macOS it is `MAP_JIT` memory, writable only by the
//! thread writing it, for as long as it writes -- other threads go on running
//! what is there -- and on Apple silicon the processor's cached copy of the
//! instructions is told they changed. Elsewhere each compiled function gets
//! pages of its own: written, then made read-and-execute -- `mmap` and
//! `mprotect` on Linux, `VirtualAlloc` and `VirtualProtect` on Windows.

use crate::abi::NativeFn;
use crate::codegen::{self, Arch};
use meadow_bytecode::{Pc, Program};
use meadow_core::OptLevel;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

/// Native code for a program's blocks, by entry pc.
pub struct Native<'p> {
    pub(crate) table: Box<[AtomicPtr<c_void>]>,
    /// The method entry of each pc that begins a method, or null: see
    /// [`crate::Vm::use_native`].
    pub(crate) methods: Box<[AtomicPtr<c_void>]>,
    tier: Option<Tier<'p>>,
    /// The program's method tables, flattened for native code: see
    /// [`crate::Vm::use_native`].
    pub(crate) method_pcs: Box<[u32]>,
    pub(crate) method_starts: Box<[u32]>,
}

/// Every method table's pcs in a row, and where each table starts (and the
/// last one ends).
fn methods(program: &Program) -> (Box<[u32]>, Box<[u32]>) {
    let mut pcs = Vec::new();
    let mut starts = Vec::with_capacity(program.methods.len() + 1);
    for table in &program.methods {
        starts.push(pcs.len() as u32);
        pcs.extend_from_slice(table);
    }
    starts.push(pcs.len() as u32);
    (pcs.into_boxed_slice(), starts.into_boxed_slice())
}

/// What compiling as the program runs needs.
struct Tier<'p> {
    program: &'p Program,
    arch: Arch,
    opt: OptLevel,
    /// Which pcs start a block.
    entry: Box<[bool]>,
    /// See `codegen::loop_live`: once per program, not per block.
    loops: std::collections::HashMap<usize, u32>,
    counts: Box<[AtomicU32]>,
    threshold: u32,
    code: Mutex<Arena>,
    compiled: AtomicUsize,
}

/// How many entries make a block hot, unless `MEADOW_JIT_THRESHOLD` says.
pub const DEFAULT_THRESHOLD: u32 = 16;

impl<'p> Native<'p> {
    /// Functions compiled ahead of time: `(entry pc, function)` for the
    /// blocks of a program of `len` instructions.
    pub fn ahead_of_time(
        program: &Program,
        blocks: impl IntoIterator<Item = (Pc, NativeFn)>,
        stubs: impl IntoIterator<Item = (Pc, *const u8)>,
    ) -> Native<'p> {
        let len = program.code.len();
        let table: Box<[AtomicPtr<c_void>]> = (0..len)
            .map(|_| AtomicPtr::new(std::ptr::null_mut()))
            .collect();
        for (pc, f) in blocks {
            if let Some(slot) = table.get(pc as usize) {
                slot.store(f as *mut c_void, Ordering::Release);
            }
        }
        let entries: Box<[AtomicPtr<c_void>]> = (0..len)
            .map(|_| AtomicPtr::new(std::ptr::null_mut()))
            .collect();
        for (pc, at) in stubs {
            if let Some(slot) = entries.get(pc as usize) {
                slot.store(at as *mut c_void, Ordering::Release);
            }
        }
        let (method_pcs, method_starts) = methods(program);
        Native {
            table,
            methods: entries,
            tier: None,
            method_pcs,
            method_starts,
        }
    }

    /// Compile `program`'s blocks as they get hot, at `opt`: each the
    /// `threshold`th time the machine enters it. A threshold of 1 compiles every
    /// block that runs, when it first does.
    pub fn jit(program: &'p Program, threshold: u32, opt: OptLevel) -> Result<Native<'p>, String> {
        let arch = Arch::host().ok_or("the JIT is for aarch64 and x86-64 only")?;
        let len = program.code.len();
        let mut entry = vec![false; len].into_boxed_slice();
        for pc in crate::abi::block_entries(program) {
            entry[pc as usize] = true;
        }
        let (method_pcs, method_starts) = methods(program);
        Ok(Native {
            method_pcs,
            method_starts,
            table: (0..len)
                .map(|_| AtomicPtr::new(std::ptr::null_mut()))
                .collect(),
            methods: (0..len)
                .map(|_| AtomicPtr::new(std::ptr::null_mut()))
                .collect(),
            tier: Some(Tier {
                program,
                arch,
                opt,
                entry,
                loops: codegen::loop_live(program),
                counts: (0..len).map(|_| AtomicU32::new(0)).collect(),
                threshold: threshold.max(1),
                code: Mutex::new(Arena::default()),
                compiled: AtomicUsize::new(0),
            }),
        })
    }

    /// Every block of `program`, compiled now, at `opt`.
    pub fn eager(program: &'p Program, opt: OptLevel) -> Result<Native<'p>, String> {
        let native = Native::jit(program, 1, opt)?;
        let tier = native.tier.as_ref().expect("a JIT has a tier");
        for pc in crate::abi::block_entries(program) {
            native.compile(tier, pc as usize);
        }
        Ok(native)
    }

    /// The threshold `MEADOW_JIT_THRESHOLD` names, or [`DEFAULT_THRESHOLD`].
    pub fn threshold_from_env() -> u32 {
        std::env::var("MEADOW_JIT_THRESHOLD")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(DEFAULT_THRESHOLD)
    }

    /// The function for the block at `pc`, if it has one -- compiling it, if
    /// this entry makes it hot.
    #[inline]
    pub fn at(&self, pc: usize) -> Option<NativeFn> {
        let p = self.table.get(pc)?.load(Ordering::Acquire);
        if !p.is_null() {
            // Safety: only ever a compiled function's address goes in.
            return Some(unsafe { std::mem::transmute::<*mut c_void, NativeFn>(p) });
        }
        let tier = self.tier.as_ref()?;
        if !tier.entry[pc] {
            return None;
        }
        let n = tier.counts[pc].fetch_add(1, Ordering::Relaxed) + 1;
        if n < tier.threshold {
            return None;
        }
        self.compile(tier, pc)
    }

    /// Blocks compiled so far, as the program runs.
    pub fn compiled(&self) -> usize {
        self.tier
            .as_ref()
            .map_or(0, |t| t.compiled.load(Ordering::Relaxed))
    }

    #[cold]
    fn compile(&self, tier: &Tier<'p>, pc: usize) -> Option<NativeFn> {
        let mut arena = tier.code.lock().unwrap_or_else(|e| e.into_inner());
        let slot = &self.table[pc];
        let p = slot.load(Ordering::Acquire);
        if !p.is_null() {
            // Another thread got here first.
            return Some(unsafe { std::mem::transmute::<*mut c_void, NativeFn>(p) });
        }
        let (code, stub) =
            codegen::compile_block(tier.program, tier.arch, pc as Pc, tier.opt, &tier.loops);
        let Some(at) = arena.put(&code, tier.arch) else {
            // No memory to put it in: interpret it, and stop asking.
            tier.counts[pc].store(0, Ordering::Relaxed);
            return None;
        };
        if let Some(offset) = stub {
            // Safety: an offset into the function just written.
            let entry = unsafe { at.add(offset as usize) };
            self.methods[pc].store(entry as *mut c_void, Ordering::Release);
        }
        slot.store(at as *mut c_void, Ordering::Release);
        tier.compiled.fetch_add(1, Ordering::Relaxed);
        // Safety: the function just written there.
        Some(unsafe { std::mem::transmute::<*mut u8, NativeFn>(at) })
    }
}

/// Executable memory, handed out a function at a time.
#[derive(Default)]
struct Arena {
    /// Every mapping made: `(start, length)`.
    maps: Vec<(*mut u8, usize)>,
    /// How far into the last mapping it is filled.
    #[cfg(target_os = "macos")]
    free: usize,
}

// The mappings are only written under the arena's lock, and only read and run
// elsewhere.
unsafe impl Send for Arena {}

impl Drop for Arena {
    fn drop(&mut self) {
        for &(p, len) in &self.maps {
            // Safety: mappings this arena made, which nothing runs once the
            // `Native` holding it is gone.
            unsafe {
                unmap(p, len);
            }
        }
    }
}

/// Give back a mapping [`Arena`] made.
#[cfg(unix)]
unsafe fn unmap(p: *mut u8, len: usize) {
    // Safety: the caller's.
    unsafe {
        munmap(p as *mut c_void, len);
    }
}

#[cfg(windows)]
unsafe fn unmap(p: *mut u8, _len: usize) {
    // Safety: the caller's. `MEM_RELEASE` takes the whole reservation, and
    // wants a size of zero to do it.
    unsafe {
        VirtualFree(p as *mut c_void, 0, MEM_RELEASE);
    }
}

#[cfg(target_os = "macos")]
const CHUNK: usize = 1 << 20;

impl Arena {
    /// `code`, somewhere executable: where it starts.
    #[cfg(target_os = "macos")]
    fn put(&mut self, code: &[u8], arch: Arch) -> Option<*mut u8> {
        let align = match arch {
            Arch::Aarch64 => 4,
            Arch::X86_64 => 16,
        };
        let offset = self.free.div_ceil(align) * align;
        let fits = self
            .maps
            .last()
            .is_some_and(|&(_, len)| offset + code.len() <= len);
        let offset = if fits {
            offset
        } else {
            let len = code.len().max(CHUNK);
            // Safety: a fresh anonymous mapping.
            let p = unsafe { map(len, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_JIT) }?;
            self.maps.push((p, len));
            0
        };
        let &(base, _) = self.maps.last().expect("a mapping");
        // Safety: `offset + code.len()` is within the mapping, and nothing runs
        // that part of it yet.
        let at = unsafe { base.add(offset) };
        unsafe {
            write_protect(false);
            std::ptr::copy_nonoverlapping(code.as_ptr(), at, code.len());
            write_protect(true);
            sys_icache_invalidate(at as *mut c_void, code.len());
        }
        self.free = offset + code.len();
        Some(at)
    }

    /// `code`, somewhere executable: where it starts.
    #[cfg(windows)]
    fn put(&mut self, code: &[u8], _arch: Arch) -> Option<*mut u8> {
        let page = 4096;
        let len = code.len().max(1).div_ceil(page) * page;
        // Safety: fresh pages, written and then made executable before anyone
        // is told where they are.
        unsafe {
            let p = VirtualAlloc(
                std::ptr::null_mut(),
                len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            ) as *mut u8;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
            let mut old = 0u32;
            if VirtualProtect(p as *mut c_void, len, PAGE_EXECUTE_READ, &mut old) == 0 {
                VirtualFree(p as *mut c_void, 0, MEM_RELEASE);
                return None;
            }
            FlushInstructionCache(GetCurrentProcess(), p as *const c_void, code.len());
            self.maps.push((p, len));
            Some(p)
        }
    }

    /// `code`, somewhere executable: where it starts.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn put(&mut self, code: &[u8], _arch: Arch) -> Option<*mut u8> {
        let page = 4096;
        let len = code.len().max(1).div_ceil(page) * page;
        // Safety: a fresh anonymous mapping, written and then made executable
        // before anyone is told where it is.
        unsafe {
            let p = map(len, PROT_READ | PROT_WRITE, 0)?;
            std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
            if mprotect(p as *mut c_void, len, PROT_READ | PROT_EXEC) != 0 {
                munmap(p as *mut c_void, len);
                return None;
            }
            #[cfg(target_arch = "aarch64")]
            __clear_cache(p as *mut c_void, p.add(code.len()) as *mut c_void);
            self.maps.push((p, len));
            Some(p)
        }
    }
}

#[cfg(windows)]
const MEM_COMMIT: u32 = 0x1000;
#[cfg(windows)]
const MEM_RESERVE: u32 = 0x2000;
#[cfg(windows)]
const MEM_RELEASE: u32 = 0x8000;
#[cfg(windows)]
const PAGE_READWRITE: u32 = 0x04;
#[cfg(windows)]
const PAGE_EXECUTE_READ: u32 = 0x20;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, protect: u32, old: *mut u32) -> i32;
    fn VirtualFree(addr: *mut c_void, size: usize, kind: u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, base: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}

#[cfg(unix)]
const PROT_READ: i32 = 1;
#[cfg(unix)]
const PROT_WRITE: i32 = 2;
#[cfg(unix)]
const PROT_EXEC: i32 = 4;
#[cfg(unix)]
const MAP_PRIVATE: i32 = 0x0002;
#[cfg(target_os = "macos")]
const MAP_ANON: i32 = 0x1000;
#[cfg(all(unix, not(target_os = "macos")))]
const MAP_ANON: i32 = 0x20;
#[cfg(target_os = "macos")]
const MAP_JIT: i32 = 0x0800;

#[cfg(unix)]
unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
    -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    #[cfg(all(unix, not(target_os = "macos")))]
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn pthread_jit_write_protect_np(enabled: i32);
    #[cfg(target_os = "macos")]
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
    #[cfg(all(unix, not(target_os = "macos"), target_arch = "aarch64"))]
    fn __clear_cache(start: *mut c_void, end: *mut c_void);
}

/// An anonymous private mapping of `len` bytes.
#[cfg(unix)]
unsafe fn map(len: usize, prot: i32, flags: i32) -> Option<*mut u8> {
    // Safety: asked for in full, and checked.
    let p = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            prot,
            MAP_PRIVATE | MAP_ANON | flags,
            -1,
            0,
        )
    };
    (p as isize != -1).then_some(p as *mut u8)
}

/// Make this thread's view of `MAP_JIT` memory writable, or executable again.
#[cfg(target_os = "macos")]
unsafe fn write_protect(on: bool) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        pthread_jit_write_protect_np(on as i32);
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = on;
}
