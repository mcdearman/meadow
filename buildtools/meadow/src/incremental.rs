//! **Incremental compilation**: a package whose inputs have not changed is
//! read back from the last build instead of compiled again.
//!
//! The package is the compilation unit -- its modules are resolved, inferred
//! and lowered together -- so it is also the unit of reuse. After a package
//! compiles cleanly its [`CompiledPackage`] is written under the `target`
//! directory, beside a **fingerprint** of everything that went into it:
//!
//! - the compiler itself (this binary, by version, size and modification time,
//!   which also covers the `Std` embedded in it);
//! - the compiler options -- profile, optimization level, strictness, and the
//!   `@cfg` conditions and flags;
//! - the package's name and every module: its path, its file and its text;
//! - the first variable id it may mint (see below);
//! - the fingerprints of its dependencies, in order.
//!
//! A dependency's fingerprint is part of its dependents', so a change reaches
//! exactly the packages downstream of it and no others: editing `app` in a
//! workspace recompiles `app`, and editing `util` recompiles `util` and what
//! uses it -- but not a member beside it that does not.
//!
//! **Variable ids.** Every package in a build mints its variables above those
//! of every package compiled before it, so that packages linked together never
//! share an id (see `pipeline::compile_graph`). That makes a package's ids
//! depend on the sizes of unrelated packages ahead of it in the build order,
//! which would make any edit anywhere a change to everything after it. So each
//! package starts on a boundary of [`ID_CHUNK`]: a package growing within its
//! chunk moves nobody else.
//!
//! The embedded `Std` is kept the same way, in the same directory: it is most
//! of the compiling a small program's build does, and without it every build
//! would compile it again, however little else changed.
//!
//! What is written is only ever a package that compiled without a diagnostic,
//! against dependencies that did too, so that a build with errors reports them
//! every time. A file that cannot be read, is from another compiler or does
//! not match is simply compiled over. Builds for the debugger, whose programs
//! carry positions tied to this process's source ids, neither read nor write.

use crate::package::Package;
use meadow_compiler::{CompiledPackage, Options};
use std::hash::Hasher;
use std::path::{Path, PathBuf};

/// Variable ids per package slot -- see the module docs.
pub const ID_CHUNK: u32 = 1 << 16;

/// `floor` rounded up to the next package slot.
pub fn align(floor: u32) -> u32 {
    floor.div_ceil(ID_CHUNK) * ID_CHUNK
}

/// What a saved package starts with, so that some other file is not taken for
/// one.
const MAGIC: &[u8; 8] = b"MWPKG\x00\x00\x01";

/// Where the compiled packages of one build configuration live:
/// `target/<profile>/incremental`.
pub struct Cache {
    dir: PathBuf,
    /// The options, as part of a file name: `meadow run` and `meadow test`
    /// differ in `@cfg(test)`, and each keeps its own packages rather than
    /// overwriting the other's. A package's own file is overwritten whenever
    /// it changes, so the directory holds one file per package per set of
    /// options, however many builds there have been.
    tag: u64,
    opts: Options,
}

impl Cache {
    /// The cache for builds with `opts` whose `target` is under `root`; none
    /// for a build the debugger runs, or with `MEADOW_INCREMENTAL=0`.
    pub fn new(root: &Path, opts: Options) -> Option<Cache> {
        if opts.debug_info || !enabled() {
            return None;
        }
        let mut h = hasher();
        text(&mut h, &format!("{opts:?}"));
        Some(Cache {
            dir: crate::artifacts::incremental_dir(root, opts.cfg.profile),
            tag: h.finish(),
            opts,
        })
    }

    /// The package called `name` saved with `fingerprint`, if there is one.
    pub fn load(&self, name: &str, fingerprint: u64) -> Option<CompiledPackage> {
        read(
            &self.dir.join(format!("{name}-{:016x}.mpk", self.tag)),
            fingerprint,
        )
    }

    /// Save `package` under `fingerprint`. Best effort: a build that cannot
    /// write its cache has still built.
    pub fn store(&self, package: &CompiledPackage, fingerprint: u64) {
        let path = self
            .dir
            .join(format!("{}-{:016x}.mpk", package.name, self.tag));
        write(&path, package, fingerprint);
    }

    /// The embedded `Std`, as an earlier build with this cache compiled it.
    pub fn load_std(&self) -> Option<CompiledPackage> {
        let (path, fingerprint) = self.std();
        read(&path, fingerprint)
    }

    pub fn store_std(&self, package: &CompiledPackage) {
        let (path, fingerprint) = self.std();
        write(&path, package, fingerprint);
    }

    /// Where `Std` is kept, and its fingerprint. `Std` sees only the platform
    /// and whether `match` must be exhaustive (see `stdlib::std_modules`), so
    /// one file serves every other option; its sources are part of the
    /// compiler.
    fn std(&self) -> (PathBuf, u64) {
        let mut h = hasher();
        text(&mut h, &format!("{:?}", self.opts.cfg.platform()));
        h.write_u8(u8::from(self.opts.check_exhaustive()));
        let tag = h.finish();
        h.write_u64(compiler());
        (self.dir.join(format!("Std-{tag:016x}.mpk")), h.finish())
    }
}

/// The package saved at `path`, if it was saved with `fingerprint`.
fn read(path: &Path, fingerprint: u64) -> Option<CompiledPackage> {
    let bytes = std::fs::read(path).ok()?;
    let rest = bytes.strip_prefix(MAGIC)?;
    let (saved, payload) = rest.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*saved) != fingerprint {
        return None;
    }
    postcard::from_bytes(payload).ok()
}

fn write(path: &Path, package: &CompiledPackage, fingerprint: u64) {
    let Ok(payload) = postcard::to_stdvec(package) else {
        return;
    };
    let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&fingerprint.to_le_bytes());
    bytes.extend_from_slice(&payload);
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return;
    }
    // Written aside and renamed into place, so that two builds at once never
    // read half a file.
    let partial = path.with_extension(format!("mpk.{}", std::process::id()));
    if std::fs::write(&partial, &bytes).is_err() || std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// Everything compiling `package` depends on -- see the module docs.
pub fn fingerprint(package: &Package, deps: &[u64], opts: Options, floor: u32) -> u64 {
    let mut h = hasher();
    h.write_u64(compiler());
    text(&mut h, &format!("{opts:?}"));
    h.write_u32(floor);
    text(&mut h, &package.name);
    h.write_usize(package.modules.len());
    for m in &package.modules {
        h.write_usize(m.path.len());
        for seg in &m.path {
            text(&mut h, seg);
        }
        text(&mut h, &m.name);
        text(&mut h, &m.source.name());
        text(&mut h, &m.source.content);
    }
    h.write_usize(deps.len());
    for d in deps {
        h.write_u64(*d);
    }
    h.finish()
}

/// Whether builds reuse what earlier ones compiled: unless
/// `MEADOW_INCREMENTAL=0`, which is for ruling the cache out when something
/// looks wrong.
pub fn enabled() -> bool {
    std::env::var("MEADOW_INCREMENTAL").map_or(true, |v| v != "0")
}

/// A hasher that answers the same in every process of one binary. Nothing
/// promises that across Rust releases, but the binary is in every
/// fingerprint anyway.
fn hasher() -> std::hash::DefaultHasher {
    std::hash::DefaultHasher::new()
}

/// `s`, with its length, so that two strings cannot run together into one.
fn text(h: &mut impl Hasher, s: &str) {
    h.write_usize(s.len());
    h.write(s.as_bytes());
}

/// This compiler: a rebuilt `meadow` never reads what an older one wrote.
fn compiler() -> u64 {
    static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ID.get_or_init(|| {
        let mut h = hasher();
        text(&mut h, env!("CARGO_PKG_VERSION"));
        let exe = std::env::current_exe().and_then(std::fs::metadata);
        match exe {
            Ok(meta) => {
                h.write_u64(meta.len());
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                h.write_u128(modified.map_or(0, |d| d.as_nanos()));
            }
            // Nothing to tell one build of the compiler from another: a
            // fingerprint no file will ever match.
            Err(_) => h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            ),
        }
        h.finish()
    })
}
