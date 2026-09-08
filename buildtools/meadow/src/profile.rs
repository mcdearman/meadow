//! Build **profiles** — the driver-level names for a bundle of compiler options.
//!
//! A profile is what a user selects (`meadow build --release`); it expands to a
//! [`meadow_compiler::Options`] before anything reaches the compiler. Keeping the
//! bundle here means the compiler itself only ever sees individual switches, and
//! new ones (optimization levels, debug info) can join a profile without the CLI
//! growing a flag each.

use meadow_compiler::Options;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// The edit-run loop: compile fast, check less.
    #[default]
    Debug,
    /// Shipping: every `match` must be exhaustive.
    Release,
}

impl Profile {
    /// The compiler options this profile stands for.
    pub const fn options(self) -> Options {
        match self {
            Profile::Debug => Options::debug(),
            Profile::Release => Options::release(),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}
