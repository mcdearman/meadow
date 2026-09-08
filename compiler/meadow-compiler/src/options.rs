//! Compiler options — the individual switches a build turns on or off.
//!
//! The driver never passes these one at a time: it picks a *profile* (`debug` or
//! `release`, see `meadow::Profile`) and the profile expands to a bundle of
//! options. Today the only switch is [`Options::check_exhaustive`]; optimization
//! levels will join it here.

/// Flags for one compilation unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Report a non-exhaustive `match` as an error.
    ///
    /// Off while iterating (a half-written `match` should still run, and fail at
    /// runtime only if it is actually reached) and on for release builds, where a
    /// missing case is a bug you want to hear about before shipping.
    ///
    /// Irrefutability of *binding* positions — function and lambda parameters,
    /// `def` / `let` destructuring — is checked regardless of this flag: those
    /// have no fallback arm, so a refutable pattern there is always an error.
    pub check_exhaustive: bool,
}

impl Options {
    /// Fast edit-run loop: no exhaustiveness check.
    pub const fn debug() -> Self {
        Options {
            check_exhaustive: false,
        }
    }

    /// Shipping build: every `match` must cover its scrutinee.
    pub const fn release() -> Self {
        Options {
            check_exhaustive: true,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Options::debug()
    }
}
