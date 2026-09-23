//! **AxCut to LLVM IR**: the backend of `meadow build --runtime aot`.
//!
//! A program is compiled all the way to machine code by LLVM and manages its
//! memory by reference counting, the discipline of the AxCut paper -- Schuster,
//! Müller, Ostermann and Brachthäuser, _Compiling Classical Sequent Calculus to
//! Stock Hardware: The Duality of Compilation_, OOPSLA 2025,
//! <https://doi.org/10.1145/3720507>. `docs/AOT.md` is the design; this crate
//! is its compiler half, and `aot/` in a checkout is the runtime it links
//! against.
//!
//! Two passes over `meadow_seq`'s AxCut:
//!
//! - [`linear`] makes it linear: every share and every erase written down.
//! - [`emit`] writes LLVM IR as text, for clang to compile and link.

pub mod emit;
pub mod linear;

use meadow_seq::Program;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

impl From<linear::Error> for Error {
    fn from(e: linear::Error) -> Error {
        Error { msg: e.msg }
    }
}

impl From<emit::Error> for Error {
    fn from(e: emit::Error) -> Error {
        Error { msg: e.msg }
    }
}

/// The symbol a runtime library built from the sources this compiler was
/// defines, and that the emitted module refers to: so that a program links
/// with that runtime and no other. See `build.rs`.
pub fn runtime_symbol() -> String {
    format!("meadow_aot_{}", fingerprint())
}

/// The fingerprint of the sources that decide the ABI between the emitted
/// code and the runtime: this crate's and the runtime's. See `build.rs`.
pub fn fingerprint() -> &'static str {
    env!("MEADOW_AOT_FINGERPRINT")
}

/// `program` as an LLVM module, as text.
pub fn compile(program: &Program) -> Result<String, Error> {
    Ok(compile_split(program, usize::MAX)?.remove(0))
}

/// About how much of a program each of the modules [`compile_split`] makes
/// holds: small enough that LLVM optimizes it quickly.
pub const UNIT: usize = 2 << 20;

/// `program` as LLVM modules of about `unit` bytes each, to compile apart and
/// link: see `emit::Module::units`.
pub fn compile_split(program: &Program, unit: usize) -> Result<Vec<String>, Error> {
    let entry = program.entry.ok_or_else(|| Error {
        msg: "the program has no entry point".into(),
    })?;
    let mut module = emit::Module::new(program);
    for d in &program.defs {
        let lb = linear::block(program, &d.block).map_err(|e| Error {
            msg: format!("in {}: {}", d.name, e.msg),
        })?;
        module.def(d.label, &lb).map_err(|e| Error {
            msg: format!("in {}: {}", d.name, e.msg),
        })?;
    }
    let result = program
        .results
        .get(&entry)
        .and_then(|r| r.desc())
        .unwrap_or(meadow_core::desc::ANY);
    Ok(module.text_split(entry, result, fingerprint(), unit))
}

/// `program` as an LLVM module for a test executable: `tests` are the labels
/// of the definitions to run, and the executable's first argument says which,
/// by its place among them. In modules of about `unit` bytes, as
/// [`compile_split`] makes them.
pub fn compile_tests(
    program: &Program,
    tests: &[meadow_seq::Label],
    unit: usize,
) -> Result<Vec<String>, Error> {
    let mut module = emit::Module::new(program);
    for d in &program.defs {
        let lb = linear::block(program, &d.block).map_err(|e| Error {
            msg: format!("in {}: {}", d.name, e.msg),
        })?;
        module.def(d.label, &lb).map_err(|e| Error {
            msg: format!("in {}: {}", d.name, e.msg),
        })?;
    }
    Ok(module.text_tests(tests, fingerprint(), unit))
}
