//! Compiler options — the individual switches a build turns on or off.
//!
//! Two axes, and they are independent. How hard the compiler works to make the
//! program fast ([`OptLevel`]), and how much it insists on before it will build
//! at all ([`Strictness`]). A driver bundles them into a named profile —
//! `meadow build --release` — but the compiler only ever sees the switches, and
//! either can be set on its own.
//!
//! They used to be one thing: `--release` meant "check `match` exhaustiveness",
//! which is a diagnostic and not an optimisation at all.

pub use meadow_core::OptLevel;

/// How much the compiler insists on before it will build.
///
/// Nothing here changes what a program *means* — only whether the compiler is
/// willing to hand it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Strictness {
    /// The edit-run loop. A half-written `match` should still run, and fail at
    /// run time only if it is actually reached.
    #[default]
    Lenient,
    /// Shipping. A missing case is a bug you want to hear about first.
    Strict,
}

impl Strictness {
    pub fn parse(s: &str) -> Option<Strictness> {
        match s.trim() {
            "lenient" => Some(Strictness::Lenient),
            "strict" => Some(Strictness::Strict),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Strictness::Lenient => "lenient",
            Strictness::Strict => "strict",
        }
    }
}

/// Flags for one compilation unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub opt: OptLevel,
    pub strictness: Strictness,
    /// Keep where each call, branch and body was written, for a debugger.
    ///
    /// Not part of either profile: it is what `meadow dap` asks for, and it
    /// changes nothing a program does -- only what the compiler remembers.
    pub debug_info: bool,
}

impl Options {
    /// Report a non-exhaustive `match` as an error.
    ///
    /// Irrefutability of *binding* positions — function and lambda parameters,
    /// `def` / `let` destructuring — is checked regardless: those have no
    /// fallback arm, so a refutable pattern there is always an error.
    pub const fn check_exhaustive(self) -> bool {
        matches!(self.strictness, Strictness::Strict)
    }

    /// Fast edit-run loop: no exhaustiveness check, cheap passes only.
    pub const fn debug() -> Self {
        Options {
            opt: OptLevel::O1,
            strictness: Strictness::Lenient,
            debug_info: false,
        }
    }

    /// Shipping build: every `match` must cover its scrutinee, and the compiler
    /// does everything it knows how to.
    pub const fn release() -> Self {
        Options {
            opt: OptLevel::O2,
            strictness: Strictness::Strict,
            debug_info: false,
        }
    }
}
