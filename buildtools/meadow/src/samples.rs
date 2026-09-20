//! Turning samples into something a flamegraph tool will read.
//!
//! [`meadow_rts::profile`] collects stacks of program counters. A pc means
//! nothing to anyone, so this is where they become names -- which is here and
//! not in the runtime because the runtime has no source table and no business
//! growing one.
//!
//! The output is **folded stacks**: one line per distinct stack, outermost
//! frame first, frames separated by `;`, then a space and the number of
//! samples.
//!
//! ```text
//! main;fib;fib;fib 412
//! main;fib;fib 180
//! ```
//!
//! That is what `flamegraph.pl` reads, and what speedscope opens directly. It
//! is also legible without either: sorted by count it is a profile.

use meadow_bytecode::{Pc, Program};
use meadow_rts::profile::Profile;
use std::collections::HashMap;

/// What to call the code at `pc`.
///
/// The block's defining name, which [`meadow_bytecode::Region`] records --
/// so a frame is the function a person wrote, not the block a compiler made of
/// it. Several blocks share a name, which is what makes a flamegraph of them
/// readable: the continuations of one function fold together under it.
fn frame(image: &Program, pc: Pc) -> String {
    // Instruction 0 is the `halt`, which no definition made and which every
    // stack ends at -- see `meadow_codegen`. Named before anything is looked
    // up, because it belongs to no region and would otherwise put a number at
    // the bottom of every flame.
    if pc == 0 {
        return "(halt)".to_string();
    }
    let Some(debug) = image.debug.as_deref() else {
        return format!("pc {pc}");
    };
    match debug.region(pc) {
        Some(r) if !r.name.is_empty() => r.name.to_string(),
        _ => format!("pc {pc}"),
    }
}

/// Samples as folded stacks, one per line, most sampled first.
///
/// Ordering is for a person reading it; a flamegraph tool sorts for itself.
pub fn folded(profile: &Profile, image: &Program) -> String {
    let mut names: HashMap<Pc, String> = HashMap::new();
    // Counted by the *names*, not the pcs. A function is many blocks and many
    // instructions, and two samples in different parts of one function are two
    // samples of that function -- which is the whole reason a flamegraph is
    // readable. Folding after naming is what makes that happen.
    let mut counts: HashMap<String, u64> = HashMap::new();
    for (stack, count) in profile.stacks() {
        // Outermost first: a flamegraph is read from the bottom up, and the
        // walk collected them innermost first.
        let mut folded = String::new();
        for pc in stack.iter().rev() {
            if !folded.is_empty() {
                folded.push(';');
            }
            let name = names
                .entry(*pc)
                .or_insert_with(|| frame(image, *pc))
                .clone();
            folded.push_str(&name);
        }
        *counts.entry(folded).or_insert(0) += count;
    }
    let mut lines: Vec<(u64, String)> = counts.into_iter().map(|(s, n)| (n, s)).collect();
    lines.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = String::new();
    for (count, stack) in lines {
        out.push_str(&stack);
        out.push(' ');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out
}

/// A line or two about what was sampled, for the terminal.
pub fn summary(profile: &Profile) -> String {
    let taken = profile.taken();
    if taken == 0 {
        return "profile: no samples -- the program did not run long enough".into();
    }
    let mut out = format!("profile: {taken} samples");
    if profile.shallow() * 2 > taken {
        out.push_str(&format!(
            "\nprofile: {} of them saw one frame -- was this built with debug info?",
            profile.shallow()
        ));
    }
    out
}

/// Allocation by the instruction that asked for it, folded to one line per
/// function, most first.
///
/// Only under `meadow-rts/profile-alloc`. The shape is the folded-stack format
/// again -- name, space, count -- so the same tools read it, with slots in
/// place of samples.
#[cfg(feature = "profile-alloc")]
pub fn allocation(sites: &meadow_rts::profile::Sites, image: &Program) -> String {
    let mut by_name: HashMap<String, (u64, u64)> = HashMap::new();
    for (pc, a) in sites.each() {
        let e = by_name.entry(frame(image, pc)).or_insert((0, 0));
        e.0 += a.objects;
        e.1 += a.slots;
    }
    let mut lines: Vec<(u64, u64, String)> =
        by_name.into_iter().map(|(n, (o, s))| (s, o, n)).collect();
    lines.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    let mut out = String::new();
    for (slots, objects, name) in lines {
        out.push_str(&format!("{name} {slots} # {objects} objects\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(stacks: &[(&[Pc], u64)]) -> Profile {
        let mut p = Profile::new(1, 16);
        for (stack, n) in stacks {
            for _ in 0..*n {
                p.record(stack.to_vec());
            }
        }
        p
    }

    /// Frames come out outermost first, which is the way round a flamegraph
    /// wants them -- the walk collects them the other way.
    #[test]
    fn a_stack_is_folded_outermost_first() {
        let image = Program::default();
        let p = samples(&[(&[7, 8, 9], 1)]);
        let got = folded(&p, &image);
        assert_eq!(got, "pc 9;pc 8;pc 7 1\n");
    }

    /// The most sampled stack is first, and equal counts are ordered by name
    /// so the output of two runs of the same program can be compared.
    #[test]
    fn the_most_sampled_stack_comes_first() {
        let image = Program::default();
        let p = samples(&[(&[1], 3), (&[2], 9), (&[3], 3)]);
        let got = folded(&p, &image);
        assert_eq!(got, "pc 2 9\npc 1 3\npc 3 3\n");
    }

    /// Two samples in different parts of one function are two samples of that
    /// function. Folding by name rather than by pc is what makes a flamegraph
    /// of a function readable instead of a list of its instructions.
    #[test]
    fn samples_in_the_same_function_fold_together() {
        let image = Program::default();
        // Without debug info every pc names itself, so use the same one twice
        // through different stacks to show the counts adding up.
        let p = samples(&[(&[5], 2), (&[5], 3)]);
        assert_eq!(folded(&p, &image), "pc 5 5\n");
    }

    /// Instruction 0 is the halt every stack ends at, and it belongs to no
    /// definition. Calling it `pc 0` would put a number at the bottom of every
    /// flame.
    #[test]
    fn the_halt_at_the_bottom_is_named() {
        let image = Program::default();
        let p = samples(&[(&[9, 0], 1)]);
        assert_eq!(folded(&p, &image), "(halt);pc 9 1\n");
    }

    #[test]
    fn a_run_with_no_samples_says_so() {
        let p = Profile::new(1, 8);
        assert!(summary(&p).contains("no samples"));
    }

    /// A program built without debug info profiles to a list of places rather
    /// than a tree, and the summary says which it is looking at.
    #[test]
    fn a_profile_that_saw_no_stacks_says_why() {
        let p = samples(&[(&[1], 5), (&[2], 5)]);
        let said = summary(&p);
        assert!(said.contains("one frame"), "{said}");
        assert!(said.contains("debug info"), "{said}");
    }
}
