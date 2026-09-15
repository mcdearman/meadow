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
//! 2. the package's `[profile.<name>]` section, if it has a manifest -- or its
//!    workspace root's, if it is in a workspace;
//! 3. flags on the command line.
//!
//! [`Resolved`] is what that produces. The built-in layer is what makes a
//! manifest optional, and the manifest layer is what lets a package say
//! `opt-level = 2` in debug builds without every `meadow run` needing a flag.

use crate::package::{Manifest, ProfileConfig};
use meadow_compiler::{OptLevel, Options, Strictness};
use std::path::Path;

/// How a built program is run.
///
/// Not a compiler option: the program is the same bytecode whichever runs it.
/// Chosen by a profile, a package's `backend = ...` in `[profile.<name>]`, or
/// `--backend` -- in that order of authority, as everything in a profile is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The bytecode interpreter.
    Vm,
    /// The interpreter, compiling the blocks that run often to machine code as
    /// it goes -- see `meadow_rts::jit`.
    #[default]
    Jit,
    /// Machine code compiled ahead of time, linked into an executable -- see
    /// [`crate::aot`].
    Aot,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Backend> {
        match s {
            "vm" => Some(Backend::Vm),
            "jit" => Some(Backend::Jit),
            "aot" | "native" => Some(Backend::Aot),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Backend::Vm => "vm",
            Backend::Jit => "jit",
            Backend::Aot => "aot",
        }
    }
}

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

    /// How this profile runs a program before a manifest or a flag says
    /// otherwise: debug on the interpreter and the JIT, which start at once;
    /// release as an executable, compiled ahead of time.
    pub const fn backend(self) -> Backend {
        match self {
            Profile::Debug => Backend::Jit,
            Profile::Release => Backend::Aot,
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
    pub backend: Backend,
    /// Whether a flag or the manifest named `backend`, rather than the profile
    /// supplying it. A backend nobody asked for gives way when it cannot be
    /// had -- see [`Resolved::fallback`].
    pub backend_named: bool,
    /// Compile only the definitions the entry point reaches, rather than every
    /// definition of the package and its dependencies -- `Std` included, which
    /// a small program otherwise carries whole (see `meadow_core::prune`). On
    /// in every profile; `prune = false` in a `[profile.<name>]`, or
    /// `--no-prune`, turns it off. `meadow test` and the debugger never prune:
    /// they start from more places than one.
    pub prune: bool,
}

impl Resolved {
    /// Just the built-in meaning of `profile`.
    pub const fn new(profile: Profile) -> Resolved {
        let mut options = profile.options();
        options.cfg.profile = profile.name();
        options.cfg.backend = profile.backend().name();
        Resolved {
            profile,
            options,
            backend: profile.backend(),
            backend_named: false,
            prune: true,
        }
    }

    /// Layer the package manifest at `path` over the profile, then `flags` over
    /// that.
    ///
    /// `path` is whatever the user named on the command line — a package
    /// directory or a single `.mw` file. A package with no manifest, or one
    /// that says nothing about this profile, simply keeps the built-in meaning.
    ///
    /// In a workspace the manifest is the workspace root's, whichever member
    /// `path` names: every member builds with the same profiles.
    pub fn resolve(profile: Profile, path: &Path, flags: ProfileConfig) -> Resolved {
        let from_manifest = crate::workspace::Workspace::find(path)
            .ok()
            .flatten()
            .map(|ws| ws.manifest)
            .or_else(|| Manifest::find(path))
            .map(|m| m.profile(profile.name()))
            .unwrap_or_default();
        let named = flags.backend.or(from_manifest.backend);
        let backend = named.unwrap_or(profile.backend());
        let mut options = flags.apply(from_manifest.apply(profile.options()));
        // What `@cfg(profile = …)` and `@cfg(backend = …)` see: this build's.
        options.cfg.profile = profile.name();
        options.cfg.backend = backend.name();
        Resolved {
            profile,
            options,
            backend,
            backend_named: named.is_some(),
            prune: flags.prune.or(from_manifest.prune).unwrap_or(true),
        }
    }

    /// What runs the program when `backend` cannot -- `aot` without a package
    /// to put an executable in, or without a linker or runtime library to make
    /// one: the JIT, if the backend was only the profile's default, and
    /// nothing, if someone asked for it.
    pub fn fallback(self) -> Option<Resolved> {
        (self.backend == Backend::Aot && !self.backend_named).then_some(Resolved {
            backend: Backend::Jit,
            ..self
        })
    }

    pub const fn opt(self) -> OptLevel {
        self.options.opt
    }

    pub const fn strictness(self) -> Strictness {
        self.options.strictness
    }

    /// `program`, pruned to what its entry point reaches if this profile
    /// prunes, and as it is if not.
    pub fn program<'a>(
        self,
        program: &'a meadow_compiler::core::Program,
    ) -> std::borrow::Cow<'a, meadow_compiler::core::Program> {
        if self.prune {
            std::borrow::Cow::Owned(meadow_compiler::core::prune::prune(program))
        } else {
            std::borrow::Cow::Borrowed(program)
        }
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
            backend: None,
            prune: None,
            cfg: None,
        };
        assert_eq!(manifest.apply(base).opt, OptLevel::O2);
        // Untouched by a section that says nothing about it.
        assert_eq!(manifest.apply(base).strictness, base.strictness);

        let flag = ProfileConfig {
            opt: Some(OptLevel::O0),
            strictness: None,
            backend: None,
            prune: None,
            cfg: None,
        };
        assert_eq!(flag.apply(manifest.apply(base)).opt, OptLevel::O0);
    }
}
