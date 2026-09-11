//! Build **profiles** — the driver-level names for a bundle of compiler options.
//!
//! A profile is what a user selects (`meadow build --release`); it expands to a
//! [`meadow_compiler::Options`] before anything reaches the compiler. Keeping the
//! bundle here means the compiler itself only ever sees individual switches, and
//! new ones (optimization levels, debug info) can join a profile without the CLI
//! growing a flag each.
//!
//! Three layers, each overriding the one before it:
//!
//! 1. the profile's built-in meaning — [`Profile::options`];
//! 2. the package's `[profile.<name>]` section, if it has a manifest;
//! 3. flags on the command line.
//!
//! [`Resolved`] is what that produces. The built-in layer is what makes a
//! manifest optional, and the manifest layer is what lets a package say
//! `opt-level = 2` in debug builds without every `meadow run` needing a flag.

use crate::package::{Manifest, ProfileConfig};
use meadow_compiler::{OptLevel, Options, Strictness};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// The edit-run loop: compile fast, check less.
    #[default]
    Debug,
    /// Shipping: optimize, and every `match` must be exhaustive.
    Release,
}

impl Profile {
    /// What this profile means before a manifest or a flag has had its say.
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

/// A profile plus the overrides that apply to it.
///
/// Carrying the profile alongside the options keeps the *name* available for
/// messages and for build directories, which the expanded options no longer
/// say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resolved {
    pub profile: Profile,
    pub options: Options,
}

impl Resolved {
    /// Just the built-in meaning of `profile`.
    pub const fn new(profile: Profile) -> Resolved {
        Resolved {
            profile,
            options: profile.options(),
        }
    }

    /// Layer the package manifest at `path` over the profile, then `flags` over
    /// that.
    ///
    /// `path` is whatever the user named on the command line — a package
    /// directory or a single `.mw` file. A package with no manifest, or one
    /// that says nothing about this profile, simply keeps the built-in meaning.
    pub fn resolve(profile: Profile, path: &Path, flags: ProfileConfig) -> Resolved {
        let from_manifest = Manifest::find(path)
            .map(|m| m.profile(profile.name()))
            .unwrap_or_default();
        Resolved {
            profile,
            options: flags.apply(from_manifest.apply(profile.options())),
        }
    }

    pub const fn opt(self) -> OptLevel {
        self.options.opt
    }

    pub const fn strictness(self) -> Strictness {
        self.options.strictness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_beats_the_manifest_beats_the_profile() {
        let base = Profile::Debug.options();
        assert_eq!(base.opt, OptLevel::O1);

        let manifest = ProfileConfig {
            opt: Some(OptLevel::O2),
            strictness: None,
        };
        assert_eq!(manifest.apply(base).opt, OptLevel::O2);
        // Untouched by a section that says nothing about it.
        assert_eq!(manifest.apply(base).strictness, base.strictness);

        let flag = ProfileConfig {
            opt: Some(OptLevel::O0),
            strictness: None,
        };
        assert_eq!(flag.apply(manifest.apply(base)).opt, OptLevel::O0);
    }
}
