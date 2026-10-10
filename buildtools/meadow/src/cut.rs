//! `meadow cut FILE`: a program of Cut -- the IR a front end hands the back
//! end, `docs/CUT.md` -- lowered to AxCut and run, on Glade's bytecode or as
//! an executable of Silo's. What any front end's programs run through,
//! Meadow's own or not.

use std::path::{Path, PathBuf};

use crate::aot;
use crate::profile::Profile;

/// The program in the file at `path`, lowered to run: read, and refused with
/// why where it does not read or is not lowered yet.
pub fn lowered(path: &Path) -> Result<meadow_seq::Program, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let program = meadow_cut::parse(&text).map_err(|e| format!("{}:{e}", path.display()))?;
    let lowered =
        meadow_cut::lower::executable(&program).map_err(|e| format!("{}: {e}", path.display()))?;
    // `MEADOW_DUMP_AXCUT=1`: every definition's AxCut on stderr, as a build
    // of Meadow source prints it, to set the two beside each other.
    if std::env::var_os("MEADOW_DUMP_AXCUT").is_some() {
        for d in &lowered.defs {
            eprintln!("-- {} (L{})\n{}", d.name, d.label.0, d.block);
        }
    }
    Ok(lowered)
}

/// Run it on Glade: compiled to bytecode, which is interpreted, and what
/// runs often compiled to machine code as it goes -- as `meadow run` does.
/// Where there is no JIT for the machine, it is interpreted throughout.
pub fn run_on_glade(path: &Path) -> Result<(), String> {
    let program = lowered(path)?;
    let image = meadow_codegen::compile(&program).map_err(|e| e.msg)?;
    let entry = image.entry.ok_or("the program has no entry")?;
    let jit = meadow_glade::jit::Native::jit(
        &image,
        meadow_glade::jit::Native::threshold_from_env(),
        meadow_compiler::OptLevel::O2,
    )
    .ok();
    meadow_glade::sched::run_native(
        &image,
        jit.as_ref(),
        entry,
        u64::MAX,
        meadow_glade::sched::workers(),
    )
    .result
    .map(|_| ())
    .map_err(|e| e.msg)
}

/// Build it for Glade ahead of time -- its bytecode compiled to machine
/// code, linked with the `meadow_glade` runtime -- beside the file, under
/// `target`, and answer where the executable is.
pub fn build_for_glade(path: &Path, opt: meadow_compiler::OptLevel) -> Result<PathBuf, String> {
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("a file with a name")?;
    let program = lowered(path)?;
    let image = meadow_codegen::compile(&program).map_err(|e| e.msg)?;
    aot::build(
        root,
        Profile::Release,
        opt,
        name,
        &image,
        aot::Target::host()?,
        &Default::default(),
    )
}

/// Build it for Silo -- LLVM's code, linked with the `meadow_silo` runtime
/// -- beside the file, under `target`, and answer where the executable is.
pub fn build_for_silo(path: &Path, opt: meadow_compiler::OptLevel) -> Result<PathBuf, String> {
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("a file with a name")?;
    let target = aot::Target {
        runtime: aot::Runtime::Silo,
        ..aot::Target::host()?
    };
    aot::build_native_of(root, Profile::Release, opt, name, target, None, || {
        lowered(path)
    })
}
