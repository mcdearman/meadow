//! The finder against the real standard library: what somebody would search
//! for, and what they would expect near the top.

use meadow_compiler::Options;
use meadow_find::{Index, Kind, collect};

thread_local! {
    static STD: Index = {
        let (packages, diags) = meadow::stdlib::std_packages(Options::debug());
        assert!(diags.is_empty(), "Std compiles: {diags:?}");
        let mut index = Index::default();
        for p in &packages {
            index.extend(collect::package(p, collect::Options::default()));
        }
        index
    };
}

/// The first `n` answers, qualified, for asserting on and for reading a
/// failure.
fn top(query: &str, n: usize) -> Vec<String> {
    STD.with(|index| {
        index
            .search(query, n)
            .iter()
            .map(|h| index.decls[h.decl].qualified())
            .collect()
    })
}

fn decl(qualified: &str) -> meadow_find::Decl {
    STD.with(|index| {
        index
            .decls
            .iter()
            .find(|d| d.qualified() == qualified)
            .unwrap_or_else(|| panic!("{qualified} is in the index"))
            .clone()
    })
}

#[test]
fn a_name_finds_what_is_called_that_first() {
    let found = top("length", 5);
    assert!(
        found.contains(&"Std.Collections.Vector.length".to_string()),
        "{found:?}"
    );
    // The one a program already has, before one it would have to `use`.
    assert_eq!(
        top("map", 1),
        vec!["Std.Collections.Vector.map"],
        "{:?}",
        top("map", 5)
    );
}

#[test]
fn a_name_matches_as_it_is_typed_not_as_it_is_capitalised() {
    // `map` is the function; `Map` is the type.
    assert_eq!(
        top("Map", 1),
        vec!["Std.Collections.Map.Map"],
        "{:?}",
        top("Map", 5)
    );
}

#[test]
fn a_dot_narrows_to_a_module() {
    let found = top("Vector.len", 3);
    assert!(
        found
            .iter()
            .all(|f| f.starts_with("Std.Collections.Vector.")),
        "{found:?}"
    );
}

#[test]
fn a_type_finds_what_has_it() {
    let found = top("[a] -> Int", 2);
    assert!(
        found.contains(&"Std.Collections.Vector.length".to_string()),
        "{found:?}"
    );
    assert_eq!(
        top("(a -> b) -> [a] -> [b]", 1),
        vec!["Std.Collections.Vector.map"]
    );
    let found = top("String -> [String]", 3);
    assert!(found.contains(&"Std.String.lines".to_string()), "{found:?}");
}

#[test]
fn a_type_finds_it_whatever_order_the_arguments_come_in() {
    // `unwrapOr` takes the default first.
    assert_eq!(
        top("Maybe a -> a -> a", 1),
        vec!["Std.Maybe.unwrapOr"],
        "{:?}",
        top("Maybe a -> a -> a", 5)
    );
}

#[test]
fn what_produces_anything_does_not_crowd_out_what_was_asked_for() {
    let found = top("[a] -> Int", 6);
    assert!(!found.contains(&"Std.Exn.raise".to_string()), "{found:?}");
}

#[test]
fn constructors_are_found_as_the_functions_they_are() {
    let just = decl("Std.Maybe.Just");
    assert_eq!(just.kind, Kind::Constructor);
    assert_eq!(just.detail, "a -> Maybe a");
    assert!(just.prelude, "`Just` needs no `use`");
    assert!(top("a -> Maybe a", 3).contains(&"Std.Maybe.Just".to_string()));
}

#[test]
fn a_signature_is_written_as_a_program_writes_it() {
    let length = decl("Std.Collections.Vector.length");
    assert_eq!(length.detail, "[a] -> Int", "no `forall`");
    assert_eq!(length.kind, Kind::Function);
}

#[test]
fn a_documented_declaration_carries_its_doc() {
    // Written in the joined form -- the doc is above the signature, and a
    // clause below it has none of its own.
    let parse = decl("Std.String.Parse.parse");
    let doc = parse.doc.expect("`parse` is documented");
    assert!(!doc.is_empty());
    let location = parse.location.expect("a file to point at");
    assert!(location.file.ends_with("Parse.mw"), "{location:?}");
}

#[test]
fn types_traits_and_effects_are_found_by_name() {
    assert_eq!(decl("Std.Maybe.Maybe").kind, Kind::Type);
    assert!(decl("Std.Maybe.Maybe").detail.starts_with("data Maybe"));
    assert_eq!(decl("Std.Cmp.PartialEq").kind, Kind::Trait);
}

#[test]
fn a_method_is_a_method() {
    assert_eq!(decl("Std.Debug.debug").kind, Kind::Method);
}
