//! Turning samples into something a flamegraph tool will read.
//!
//! [`meadow_glade::profile`] collects stacks of program counters. A pc means
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
use meadow_glade::profile::Profile;
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
    let kind = if profile.is_timed() { "time" } else { "work" };
    let mut out = format!("profile: {taken} samples of {kind}");
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
/// Only under `meadow-glade/profile-alloc`. The shape is the folded-stack format
/// again -- name, space, count -- so the same tools read it, with slots in
/// place of samples.
#[cfg(feature = "profile-alloc")]
pub fn allocation(sites: &meadow_glade::profile::Sites, image: &Program) -> String {
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

/// What a JIT could speculate on: how many targets each `invoke` site saw,
/// weighted by how often it ran, and how lopsided the tag tests were.
///
/// Only under `meadow-glade/profile-alloc`. Returns through a frame and calls of
/// a heap object -- a function value, a heap continuation, a handler -- are
/// counted apart, because they are guarded on differently: a frame's return
/// address against a pc, an object's method table against a table.
#[cfg(feature = "profile-alloc")]
pub fn feedback(fb: &meadow_glade::profile::Feedback, image: &Program) -> String {
    let pct = |n: u64, of: u64| {
        if of == 0 {
            0.0
        } else {
            n as f64 * 100.0 / of as f64
        }
    };
    let mut out = String::new();
    // [frame, closure]: invokes, sites, invokes by the targets their site saw
    // (1, 2, 3-4, 5+), and invokes whose site's top target took 90% or more.
    let mut total = [0u64; 2];
    let mut sites = [0u64; 2];
    let mut arity = [[0u64; 4]; 2];
    let mut biased = [0u64; 2];
    let mut poly: Vec<(u64, Pc, usize, u64)> = Vec::new();
    for (site, targets) in &fb.calls {
        for k in 0..2 {
            let counts: Vec<u64> = targets
                .values()
                .map(|t| if k == 0 { t.0 } else { t.1 })
                .filter(|n| *n > 0)
                .collect();
            let n: u64 = counts.iter().sum();
            if n == 0 {
                continue;
            }
            total[k] += n;
            sites[k] += 1;
            let bucket = match counts.len() {
                1 => 0,
                2 => 1,
                3 | 4 => 2,
                _ => 3,
            };
            arity[k][bucket] += n;
            let top = counts.iter().copied().max().unwrap_or(0);
            if top * 10 >= n * 9 {
                biased[k] += n;
            }
            if k == 1 && counts.len() > 1 {
                poly.push((n, *site, counts.len(), top));
            }
        }
    }
    let all = total[0] + total[1];
    out.push_str(&format!(
        "invokes: {all} at {} sites\n",
        sites[0] + sites[1]
    ));
    for (k, what) in ["returns through a frame", "calls of a heap object"]
        .iter()
        .enumerate()
    {
        out.push_str(&format!(
            "  {what}: {} ({:.1}%) at {} sites\n    by targets their site saw: 1 {:.1}%  2 {:.1}%  3-4 {:.1}%  5+ {:.1}%\n    at a site whose top target took >= 90%: {:.1}%\n",
            total[k],
            pct(total[k], all),
            sites[k],
            pct(arity[k][0], total[k]),
            pct(arity[k][1], total[k]),
            pct(arity[k][2], total[k]),
            pct(arity[k][3], total[k]),
            pct(biased[k], total[k]),
        ));
    }
    poly.sort_by(|a, b| b.0.cmp(&a.0));
    if !poly.is_empty() {
        out.push_str("  busiest heap-object sites with more than one target:\n");
        for (n, site, targets, top) in poly.iter().take(8) {
            out.push_str(&format!(
                "    {:>12}  {} targets, top {:.1}%  {} @{site}\n",
                n,
                targets,
                pct(*top, *n),
                frame(image, *site),
            ));
        }
    }
    let moved: u64 = fb.moves.values().sum();
    if moved > 0 {
        let mut by: Vec<_> = fb.moves.iter().collect();
        by.sort_by(|a, b| b.1.cmp(a.1));
        out.push_str(&format!("moves: {moved}, by what ends their run:"));
        for (op, n) in by.iter().take(6) {
            let op = meadow_bytecode::Op::from_byte(**op).unwrap_or(meadow_bytecode::Op::Nop);
            out.push_str(&format!(" {op:?} {:.1}%", pct(**n, moved)));
        }
        out.push('\n');
    }
    let tests: u64 = fb.tags.values().map(|[m, n]| m + n).sum();
    let lopsided: u64 = fb
        .tags
        .values()
        .filter(|[m, n]| (*m).max(*n) * 20 >= (m + n) * 19)
        .map(|[m, n]| m + n)
        .sum();
    out.push_str(&format!(
        "tag tests: {tests} at {} sites, {:.1}% at a site that goes one way >= 95% of the time\n",
        fb.tags.len(),
        pct(lopsided, tests)
    ));
    out
}

/// What a run spent its instructions on, most first.
///
/// Only under `meadow-glade/profile-alloc`, and exact rather than sampled. A
/// stack profile says which function; this says what that function was made
/// of, which is the difference between "the loop is hot" and "the calling
/// convention is".
#[cfg(feature = "profile-alloc")]
pub fn instructions(ops: &meadow_glade::profile::Ops) -> String {
    let total = ops.total();
    if total == 0 {
        return String::new();
    }
    let mut out = format!("instructions: {total} retired\n");
    for (op, n) in ops.each() {
        let share = n as f64 * 100.0 / total as f64;
        if share < 0.5 {
            continue;
        }
        out.push_str(&format!("  {:>5.1}%  {:>12}  {op:?}\n", share, n));
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
