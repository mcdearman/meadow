//! **What a run saw, for a build ahead of time**: profile-guided `aot` builds.
//!
//! The JIT watches every `invoke` it interprets while a block is warming up,
//! and guards the calls that only ever entered one thing on that one thing
//! when it compiles the block (see `meadow_rts::codegen::Known`). An `aot`
//! build compiles before anything has run, so it has nothing to guess from --
//! unless a run leaves what it saw behind.
//!
//! `meadow run --train` runs on the JIT and writes those calls to
//! `target/<profile>/calls.pgo`; `meadow build`, and `run`, for an `aot`
//! backend, read them back when the file is there and was made from the same
//! image. A profile from another image is ignored, and said to be: its pcs
//! would name other instructions.
//!
//! The file is text, one site to a line, after a line naming the image:
//!
//! ```text
//! meadow-calls 1 9f3c0e21a4b7d655
//! 358 closure 33
//! 374 frame 373
//! ```

use meadow_rts::codegen::{Calls, Known};
use std::path::{Path, PathBuf};

const HEADER: &str = "meadow-calls 1";

/// Where a package's profile for `profile` goes.
pub fn path(root: &Path, profile: &str) -> PathBuf {
    root.join("target").join(profile).join("calls.pgo")
}

/// Which image a profile is of: its pcs mean nothing in any other.
pub fn image_hash(image: &meadow_bytecode::Program) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in meadow_bytecode::image::encode(image) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The text of a profile of `calls`, made from `image`.
pub fn render(image: &meadow_bytecode::Program, calls: &Calls) -> String {
    let mut sites: Vec<_> = calls.iter().collect();
    sites.sort_by_key(|(pc, _)| **pc);
    let mut out = format!("{HEADER} {:016x}\n", image_hash(image));
    for (pc, k) in sites {
        let kind = if k.frame { "frame" } else { "closure" };
        out.push_str(&format!("{pc} {kind} {}\n", k.meta));
    }
    out
}

/// Write the profile of `calls` to `to`.
pub fn write(to: &Path, image: &meadow_bytecode::Program, calls: &Calls) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(to, render(image, calls))
        .map_err(|e| format!("could not write {}: {e}", to.display()))
}

/// What reading a profile for an image found.
#[derive(Debug, PartialEq)]
pub enum Found {
    /// No profile there.
    None,
    /// A profile of this image.
    Calls(Calls),
    /// A profile of another one -- the source changed since it was made.
    Stale,
}

/// The calls in `text`, if it is a profile of `image`.
pub fn parse(text: &str, image: &meadow_bytecode::Program) -> Result<Found, String> {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or_default();
    let Some(hash) = head.strip_prefix(HEADER).map(str::trim) else {
        return Err("not a profile of calls".into());
    };
    if u64::from_str_radix(hash, 16).ok() != Some(image_hash(image)) {
        return Ok(Found::Stale);
    }
    let mut calls = Calls::new();
    for (n, line) in lines.enumerate() {
        let bad = || format!("line {}: {line:?} is not `pc frame|closure meta`", n + 2);
        let mut words = line.split_whitespace();
        let pc = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
        let frame = match words.next() {
            Some("frame") => true,
            Some("closure") => false,
            _ => return Err(bad()),
        };
        let meta = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
        calls.insert(pc, Known { frame, meta });
    }
    Ok(Found::Calls(calls))
}

/// The profile at `from` for `image`, if there is one.
pub fn read(from: &Path, image: &meadow_bytecode::Program) -> Result<Found, String> {
    match std::fs::read_to_string(from) {
        Ok(text) => parse(&text, image).map_err(|e| format!("{}: {e}", from.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Found::None),
        Err(e) => Err(format!("could not read {}: {e}", from.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_reads_back_as_it_was_written() {
        let image = meadow_bytecode::Program::default();
        let mut calls = Calls::new();
        calls.insert(
            374,
            Known {
                frame: true,
                meta: 373,
            },
        );
        calls.insert(
            358,
            Known {
                frame: false,
                meta: 33,
            },
        );
        let text = render(&image, &calls);
        assert!(text.contains("358 closure 33\n374 frame 373\n"), "{text}");
        assert_eq!(parse(&text, &image), Ok(Found::Calls(calls)));
    }

    /// A profile of another image names other instructions, so it is not
    /// used -- and is told apart from no profile at all, to be reported.
    #[test]
    fn a_profile_of_another_image_is_stale() {
        let image = meadow_bytecode::Program::default();
        let mut other = meadow_bytecode::Program::default();
        other.regs = 3;
        let text = render(&other, &Calls::new());
        assert_eq!(parse(&text, &image), Ok(Found::Stale));
    }
}
