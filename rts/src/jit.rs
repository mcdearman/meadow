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
//! Platform business, behind one rule: functions are packed together, as a
//! linker lays them out, and no address is ever both writable and executable.
//! On macOS it is `MAP_JIT` memory, writable only by the thread writing it,
//! for as long as it writes -- other threads go on running what is there --
//! and on Apple silicon the processor's cached copy of the instructions is
//! told they changed. Elsewhere each chunk is mapped twice from the same
//! memory, a view to write and a view to run: a `memfd` on Linux, a section
//! backed by the page file on Windows. See [`Arena`] for why packing matters.

use crate::abi::NativeFn;
use crate::codegen::{self, Arch};
use meadow_bytecode::{Pc, Program};
use meadow_core::OptLevel;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

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
    /// What every block's compilation needs to know of the whole program:
    /// once per program, not per block. See `codegen::Whole`.
    whole: codegen::Whole,
    counts: Box<[AtomicU32]>,
    threshold: u32,
    /// Per `invoke` site: what it has entered while interpreted -- see
    /// [`Native::observe`].
    calls: Box<[AtomicU64]>,
    /// Whether a block's calls are guarded on what they were seen to enter:
    /// unless `MEADOW_JIT_GUESS=0`, which is for telling whether a guess is
    /// what broke something.
    guess: bool,
    code: Mutex<Arena>,
    compiled: AtomicUsize,
}

/// A call site's word, once it has entered something: see [`Native::observe`].
const SEEN: u64 = 1 << 62;
const FRAME_BIT: u32 = 61;
/// It has entered more than one thing.
const POLY: u64 = u64::MAX;

/// What a site's word says it entered, if one thing.
fn known(w: u64) -> Option<codegen::Known> {
    (w != POLY && w & SEEN != 0).then(|| codegen::Known {
        frame: w >> FRAME_BIT & 1 != 0,
        meta: w as u32,
    })
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
                whole: codegen::Whole::of(program),
                counts: (0..len).map(|_| AtomicU32::new(0)).collect(),
                calls: (0..len).map(|_| AtomicU64::new(0)).collect(),
                guess: std::env::var_os("MEADOW_JIT_GUESS").is_none_or(|v| v != "0"),
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

    /// Note that the `invoke` at `site` entered `known`, while its block was
    /// interpreted. What a block's compilation guesses its calls enter -- see
    /// [`codegen::Known`] -- and what [`Native::calls`] hands on to a build
    /// ahead of time.
    ///
    /// One word per site, and nothing to take a lock for: empty, then the one
    /// thing seen, then [`POLY`] for good once a second one is.
    #[inline]
    pub fn observe(&self, site: usize, known: codegen::Known) {
        let Some(slot) = self.tier.as_ref().and_then(|t| t.calls.get(site)) else {
            return;
        };
        let want = SEEN | (known.frame as u64) << FRAME_BIT | known.meta as u64;
        let now = slot.load(Ordering::Relaxed);
        if now == want || now == POLY {
            return;
        }
        if now == 0
            && slot
                .compare_exchange(0, want, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            return;
        }
        if slot.load(Ordering::Relaxed) != want {
            slot.store(POLY, Ordering::Relaxed);
        }
    }

    /// Every `invoke` site that entered one thing, and only one, while it was
    /// interpreted.
    pub fn calls(&self) -> codegen::Calls {
        let Some(tier) = &self.tier else {
            return codegen::Calls::new();
        };
        tier.calls
            .iter()
            .enumerate()
            .filter_map(|(pc, w)| Some((pc as Pc, known(w.load(Ordering::Relaxed))?)))
            .collect()
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
        let (code, stub) = codegen::compile_block(
            tier.program,
            tier.arch,
            pc as Pc,
            tier.opt,
            &tier.whole,
            &|site| {
                if !tier.guess {
                    return None;
                }
                known(tier.calls.get(site as usize)?.load(Ordering::Relaxed))
            },
        );
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
///
/// Functions are packed one after another into chunks, as a linker would lay
/// them out. Giving each its own mapping instead starts every one on a page --
/// on Windows a 64 KiB boundary -- so every function's entry has the same low
/// address bits and they all compete for the same few ways of the instruction
/// cache and the branch predictor, besides taking a page of the instruction TLB
/// each. On closure-heavy code that made the JIT half again slower than the
/// same translation ahead of time.
///
/// Memory is never writable and executable at one address. On macOS a
/// `MAP_JIT` mapping is writable only by the thread writing it, for as long as
/// it writes. Elsewhere each chunk is mapped twice, from the same memory: a
/// view to write through and a view to run, which never changes. Where the
/// system will not make the pair, each function gets pages of its own after
/// all, written and then made executable.
#[derive(Default)]
struct Arena {
    /// Every single mapping made: `(start, length)`.
    maps: Vec<(*mut u8, usize)>,
    /// How far into the last chunk it is filled.
    free: usize,
    /// Chunks mapped twice, the last one being filled.
    #[cfg(not(target_os = "macos"))]
    views: Vec<Views>,
    /// The system would not map a chunk twice: stop asking.
    #[cfg(not(target_os = "macos"))]
    alone: bool,
}

/// One chunk's two views of the same memory.
#[cfg(not(target_os = "macos"))]
struct Views {
    write: *mut u8,
    exec: *mut u8,
    len: usize,
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
        #[cfg(not(target_os = "macos"))]
        for v in &self.views {
            // Safety: as above.
            unsafe {
                unmap_views(v);
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

/// How much a chunk holds, unless one function needs more.
const CHUNK: usize = 1 << 20;

/// Where a function starts: where the architecture's fetch likes it.
fn align(arch: Arch) -> usize {
    match arch {
        Arch::Aarch64 => 4,
        Arch::X86_64 => 16,
    }
}

impl Arena {
    /// `code`, somewhere executable: where it starts.
    #[cfg(target_os = "macos")]
    fn put(&mut self, code: &[u8], arch: Arch) -> Option<*mut u8> {
        let align = align(arch);
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

    /// `code`, somewhere executable: where it starts. Packed into a chunk
    /// mapped twice if it can be, on pages of its own if not.
    #[cfg(not(target_os = "macos"))]
    fn put(&mut self, code: &[u8], arch: Arch) -> Option<*mut u8> {
        if !self.alone {
            match self.packed(code, arch) {
                Some(at) => return Some(at),
                None => self.alone = true,
            }
        }
        self.put_alone(code)
    }

    #[cfg(not(target_os = "macos"))]
    fn packed(&mut self, code: &[u8], arch: Arch) -> Option<*mut u8> {
        let align = align(arch);
        let offset = self.free.div_ceil(align) * align;
        let fits = self
            .views
            .last()
            .is_some_and(|v| offset + code.len() <= v.len);
        let offset = if fits {
            offset
        } else {
            // Whole 64 KiB, the granularity Windows maps views at.
            let len = code.len().max(CHUNK).div_ceil(1 << 16) << 16;
            // Safety: a fresh pair of views.
            let v = unsafe { views(len) }?;
            self.views.push(v);
            0
        };
        let v = self.views.last().expect("a chunk");
        // Safety: `offset + code.len()` is within the chunk, and nothing runs
        // that part of it yet: it is published only once this returns.
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), v.write.add(offset), code.len());
            let at = v.exec.add(offset);
            flush(at, code.len());
            self.free = offset + code.len();
            Some(at)
        }
    }

    /// `code` on pages of its own, written and then made executable.
    #[cfg(windows)]
    fn put_alone(&mut self, code: &[u8]) -> Option<*mut u8> {
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

    /// `code` on pages of its own, written and then made executable.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn put_alone(&mut self, code: &[u8]) -> Option<*mut u8> {
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

/// `len` bytes of fresh memory, mapped twice: once writable, once executable.
#[cfg(windows)]
unsafe fn views(len: usize) -> Option<Views> {
    // Safety: a pagefile-backed section of our own, and views of all of it.
    unsafe {
        let h = CreateFileMappingW(
            -1isize as *mut c_void,
            std::ptr::null_mut(),
            PAGE_EXECUTE_READWRITE,
            (len >> 32) as u32,
            len as u32,
            std::ptr::null(),
        );
        if h.is_null() {
            return None;
        }
        let write = MapViewOfFile(h, FILE_MAP_WRITE, 0, 0, len) as *mut u8;
        let exec = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_EXECUTE, 0, 0, len) as *mut u8;
        // The views keep the section alive.
        CloseHandle(h);
        let v = Views { write, exec, len };
        if write.is_null() || exec.is_null() {
            unmap_views(&v);
            return None;
        }
        Some(v)
    }
}

#[cfg(windows)]
unsafe fn unmap_views(v: &Views) {
    // Safety: the caller's.
    unsafe {
        for p in [v.write, v.exec] {
            if !p.is_null() {
                UnmapViewOfFile(p as *const c_void);
            }
        }
    }
}

/// Code just written at `at`, through the other view, is what runs there.
#[cfg(windows)]
unsafe fn flush(at: *mut u8, len: usize) {
    // Safety: a range of a view this process mapped.
    unsafe {
        FlushInstructionCache(GetCurrentProcess(), at as *const c_void, len);
    }
}

#[cfg(windows)]
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
#[cfg(windows)]
const FILE_MAP_WRITE: u32 = 0x0002;
#[cfg(windows)]
const FILE_MAP_READ: u32 = 0x0004;
#[cfg(windows)]
const FILE_MAP_EXECUTE: u32 = 0x0020;

/// `len` bytes of fresh memory, mapped twice: once writable, once executable.
#[cfg(all(unix, not(target_os = "macos")))]
unsafe fn views(len: usize) -> Option<Views> {
    // Safety: an anonymous file of our own, and shared mappings of all of it.
    unsafe {
        let fd = memfd_create(c"meadow-jit".as_ptr(), MFD_CLOEXEC);
        if fd < 0 {
            return None;
        }
        if ftruncate(fd, len as i64) != 0 {
            close(fd);
            return None;
        }
        let at = |prot| {
            let p = mmap(std::ptr::null_mut(), len, prot, MAP_SHARED, fd, 0);
            if p as isize == -1 {
                std::ptr::null_mut()
            } else {
                p as *mut u8
            }
        };
        let write = at(PROT_READ | PROT_WRITE);
        let exec = at(PROT_READ | PROT_EXEC);
        // The mappings keep the file alive.
        close(fd);
        let v = Views { write, exec, len };
        if write.is_null() || exec.is_null() {
            unmap_views(&v);
            return None;
        }
        Some(v)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
unsafe fn unmap_views(v: &Views) {
    // Safety: the caller's.
    unsafe {
        for p in [v.write, v.exec] {
            if !p.is_null() {
                munmap(p as *mut c_void, v.len);
            }
        }
    }
}

/// Code just written at `at`, through the other view, is what runs there.
#[cfg(all(unix, not(target_os = "macos")))]
unsafe fn flush(at: *mut u8, len: usize) {
    // Safety: a range of a mapping this process made.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        __clear_cache(at as *mut c_void, at.add(len) as *mut c_void);
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = (at, len);
}

#[cfg(all(unix, not(target_os = "macos")))]
const MAP_SHARED: i32 = 0x0001;
#[cfg(all(unix, not(target_os = "macos")))]
const MFD_CLOEXEC: u32 = 0x0001;

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
    fn CreateFileMappingW(
        file: *mut c_void,
        attributes: *mut c_void,
        protect: u32,
        size_high: u32,
        size_low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        access: u32,
        offset_high: u32,
        offset_low: u32,
        size: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(base: *const c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
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
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    fn memfd_create(name: *const std::ffi::c_char, flags: u32) -> i32;
    #[cfg(target_os = "android")]
    fn syscall(number: std::ffi::c_long, ...) -> std::ffi::c_long;
    #[cfg(all(unix, not(target_os = "macos")))]
    fn ftruncate(fd: i32, len: i64) -> i32;
    #[cfg(all(unix, not(target_os = "macos")))]
    fn close(fd: i32) -> i32;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn pthread_jit_write_protect_np(enabled: i32);
    #[cfg(target_os = "macos")]
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "android"),
        target_arch = "aarch64"
    ))]
    fn __clear_cache(start: *mut c_void, end: *mut c_void);
}

/// `memfd_create`, which Android's C library has only from Android 11 (API
/// 30). An Android build links against an older one -- Termux runs on Android
/// 7 -- so this asks the kernel directly, which has had the call since Linux
/// 3.17. A kernel older still answers -1, and so does anything that refuses
/// it; either way `views` fails, and the JIT puts code on pages of its own.
#[cfg(target_os = "android")]
unsafe fn memfd_create(name: *const std::ffi::c_char, flags: u32) -> i32 {
    let number: std::ffi::c_long = if cfg!(target_arch = "aarch64") {
        279
    } else if cfg!(target_arch = "x86_64") {
        319
    } else {
        return -1;
    };
    // Safety: the call's own arguments, as the kernel takes them.
    unsafe { syscall(number, name, flags) as i32 }
}

/// What `__clear_cache` does on ARM64, written out. On Linux it is libgcc's,
/// and Android links no library that has it -- it is a few instructions.
///
/// Code written through one view and run through another is only seen as
/// written once the data cache has been cleaned to where the instruction side
/// reads from, and the instruction cache invalidated over it: each a line at a
/// time, by the line sizes `CTR_EL0` gives. A core that says it keeps the two
/// coherent itself -- `IDC`, `DIC` -- skips the step it does not need.
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
unsafe fn __clear_cache(start: *mut c_void, end: *mut c_void) {
    use std::arch::asm;
    let (start, end) = (start as usize, end as usize);
    let ctr: u64;
    // Safety: a register the kernel lets user code read, on every ARM64 Linux.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    if (ctr >> 28) & 1 == 0 {
        let line = 4usize << ((ctr >> 16) & 15);
        let mut at = start & !(line - 1);
        while at < end {
            // Safety: a line of a mapping this process made.
            unsafe { asm!("dc cvau, {}", in(reg) at, options(nostack, preserves_flags)) };
            at += line;
        }
    }
    // Safety: barriers, no operands.
    unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
    if (ctr >> 29) & 1 == 0 {
        let line = 4usize << (ctr & 15);
        let mut at = start & !(line - 1);
        while at < end {
            // Safety: as above.
            unsafe { asm!("ic ivau, {}", in(reg) at, options(nostack, preserves_flags)) };
            at += line;
        }
        // Safety: as above.
        unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
    }
    // Safety: as above.
    unsafe { asm!("isb sy", options(nostack, preserves_flags)) };
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
