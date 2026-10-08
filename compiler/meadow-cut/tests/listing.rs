//! A program's text with what its parts are: Cut's own, and the AxCut it is
//! lowered to with what each part of that came from -- so that a tool can
//! show where one name is in each.

use meadow_cut::print::{Listing, Part, listing};
use meadow_cut::{Symbol, parse};

const COUNT: &str = "cut 0
entry t:Main/main
answer str

data t:Main/Nat { Z; S(ptr) }

val t:Main/two : ptr =
  <t:Main/Nat.S(t:Main/Nat.S(t:Main/Nat.Z)) | halt>

def t:Main/count (n: ptr, acc: i64; k: ptr) =
  <n | case {
    t:Main/Nat.Z => <acc | k>;
    t:Main/Nat.S(m: ptr) => prim add(acc, 1; μ̃ more: i64. t:Main/count(m, more; k))
  }>

def t:Main/main (; k: ptr) =
  t:Main/count(t:Main/two, 0; μ̃ c: i64.
    prim eq(c, 2; μ̃ u: unit. <\"no\" | k>, μ̃ u: unit. <\"two\" | k>))
";

fn symbol(of: &meadow_cut::Program, name: &str) -> Symbol {
    of.defs
        .iter()
        .map(|d| &d.symbol)
        .chain(of.vals.iter().map(|v| &v.symbol))
        .find(|s| s.to_string().ends_with(name))
        .unwrap_or_else(|| panic!("no `{name}`"))
        .clone()
}

/// The text of each segment that is the variable `name` of `of`.
fn written<'a>(l: &'a Listing, of: &Symbol, name: &str) -> Vec<&'a str> {
    l.var(of, name).map(|s| &l.text[s.start..s.end]).collect()
}

#[test]
fn a_listing_of_cut_says_which_declaration_and_variable_each_part_is() {
    let p = parse(COUNT).expect("a program");
    let l = listing(&p);
    assert_eq!(l.text, meadow_cut::print::program(&p), "the same text");
    let count = symbol(&p, "count");
    let main = symbol(&p, "main");
    // A declaration is the whole of what it was written as.
    let decl = l.decl(&count).expect("count's declaration");
    let text = &l.text[decl.start..decl.end];
    assert!(text.starts_with("def t:Main/count (n: ptr"), "{text}");
    assert!(text.ends_with("}>"), "{text}");
    // A variable is where it is bound and each place it is mentioned, and
    // is its declaration's alone: `k` is one of `count`'s and one of `main`'s.
    assert_eq!(written(&l, &count, "acc"), ["acc", "acc", "acc"]);
    assert_eq!(written(&l, &count, "k"), ["k", "k", "k"]);
    assert_eq!(written(&l, &main, "k"), ["k", "k", "k"]);
    assert!(written(&l, &main, "acc").is_empty());
    // A symbol mentioned says where: `main` calls `count` and reads `two`.
    let mentioned: Vec<String> = l
        .segments
        .iter()
        .filter_map(|s| match &s.part {
            Part::Symbol { of, symbol } if *of == main => Some(symbol.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(mentioned, ["t:Main/count", "t:Main/two"]);
}

#[test]
fn the_axcut_a_program_is_lowered_to_says_what_each_part_came_from() {
    let p = parse(COUNT).expect("a program");
    let lowered = meadow_cut::lower::lower_mapped(&p).expect("lowered");
    let l = lowered.listing();
    assert_eq!(l.text, lowered.program.pretty(), "the same text");
    let count = symbol(&p, "count");
    let two = symbol(&p, "two");
    // A definition is a block, by its label.
    let decl = l.decl(&count).expect("count's block");
    assert!(
        l.text[decl.start..decl.end].starts_with("def #0 ("),
        "{}",
        &l.text[decl.start..decl.end]
    );
    // A variable of Cut's is one name of the AxCut's wherever it is in
    // scope by that name -- `acc` in the definition's block and in each arm
    // of the switch -- and every mention of it is inside its definition.
    let acc = written(&l, &count, "acc");
    assert!(acc.len() > 3, "{acc:?}");
    assert!(acc.iter().all(|n| *n == acc[0]), "{acc:?}");
    assert!(
        l.var(&count, "acc")
            .all(|s| decl.start <= s.start && s.end <= decl.end)
    );
    // `more` names the sum, which the lowering made a name for.
    assert_eq!(
        lowered
            .map
            .names
            .values()
            .filter(|o| o.vars == ["more"])
            .count(),
        1
    );
    // A top-level value's block is the one that makes it the first time a
    // thread wants it, and what was made lowering it says it is the value's.
    assert!(l.decl(&two).is_some());
    assert!(
        lowered
            .map
            .names
            .values()
            .any(|o| o.of.as_ref() == Some(&two))
    );
}
