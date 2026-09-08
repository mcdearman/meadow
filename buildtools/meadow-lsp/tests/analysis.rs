//! What the server answers, without the protocol in the way.

use meadow_lsp::analysis::Std;
use meadow_lsp::pos::LineIndex;

/// Analyse `src` and resolve `⟨marker⟩` to a byte offset. The marker is removed
/// before compiling, so it never affects the result.
fn at(src: &str, marker: &str) -> (meadow_lsp::analysis::Analysis, usize) {
    let offset = src.find(marker).unwrap_or_else(|| panic!("no {marker:?} in source"));
    let clean = src.replacen(marker, "", 1);
    // The marker sits just before the token of interest.
    (STD.with(|s| s.analyse(&clean)), offset)
}

thread_local! {
    static STD: Std = Std::new(meadow::stdlib::std_packages(meadow::Options::debug()).0);
}

#[test]
fn hover_reports_the_type_at_a_position() {
    let (a, off) = at("fun double n = n * 2\ndef main = do@uble 21\n", "@");
    let hover = a.hover_at(off).expect("hover");
    assert!(hover.contains("double : Int -> Int"), "got: {hover}");
}

#[test]
fn hover_shows_the_generalised_scheme_not_the_use_site() {
    let (a, off) = at("fun ident x = x\ndef main = ide@nt 1\n", "@");
    let hover = a.hover_at(off).expect("hover");
    assert!(
        hover.contains("forall"),
        "a top-level binding should hover as its scheme: {hover}"
    );
}

#[test]
fn hover_includes_the_doc_comment_above_the_definition() {
    let src = "\
-- Doubles its argument.
-- Twice as much, in other words.
fun double n = n * 2

def main = do@uble 21
";
    let (a, off) = at(src, "@");
    let hover = a.hover_at(off).expect("hover");
    assert!(hover.contains("Doubles its argument."), "got: {hover}");
    assert!(hover.contains("Twice as much"), "got: {hover}");
}

#[test]
fn a_doc_comment_stops_at_a_blank_line() {
    let src = "\
-- Not part of the docs.

-- The real docs.
fun double n = n * 2

def main = do@uble 21
";
    let (a, off) = at(src, "@");
    let hover = a.hover_at(off).unwrap();
    assert!(hover.contains("The real docs."));
    assert!(!hover.contains("Not part of the docs."), "got: {hover}");
}

#[test]
fn go_to_definition_finds_the_binder() {
    let src = "fun double n = n * 2\ndef main = do@uble 21\n";
    let (a, off) = at(src, "@");
    let def = a.definition_at(off).expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[def.start as usize..def.end as usize], "double");
}

#[test]
fn go_to_definition_works_for_a_local_binding() {
    let src = "fun f u =\n  let answer = 42 in\n  ans@wer + 1\ndef main = f ()\n";
    let (a, off) = at(src, "@");
    let def = a.definition_at(off).expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[def.start as usize..def.end as usize], "answer");
    // …and it is the *local* one, not something at the top level.
    assert!(def.start as usize > clean.find("let").unwrap());
}

#[test]
fn the_innermost_node_wins() {
    // At `xs` the type is the vector's, not the enclosing `len xs`'s `Int`.
    let (a, off) = at("fun size xs = len @xs\ndef main = size [1, 2]\n", "@");
    assert_eq!(a.type_at(off), Some("[a]"));
}

#[test]
fn inlay_hints_cover_parameters_and_lets() {
    let a = STD.with(|s| {
        s.analyse("fun add a b = a + b\nfun f u = let n = 1 in n\ndef main = add 1 2\n")
    });
    let hinted: Vec<&str> = a.binders.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        hinted.iter().filter(|t| **t == "Int").count() >= 3,
        "expected hints for `a`, `b` and `n`: {hinted:?}"
    );
}

#[test]
fn a_top_level_name_gets_no_inlay_hint() {
    // Its signature is already on the line; repeating it is noise.
    let a = STD.with(|s| s.analyse("def answer = 42\n"));
    assert!(a.binders.is_empty(), "got: {:?}", a.binders);
}

#[test]
fn diagnostics_come_through() {
    let a = STD.with(|s| s.analyse("def main = 1 + \"nope\"\n"));
    assert!(
        a.diagnostics.iter().any(|d| d.msg.contains("type mismatch")),
        "got: {:?}",
        a.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

#[test]
fn a_clean_file_has_no_diagnostics() {
    let a = STD.with(|s| s.analyse("def main = 1 + 2\n"));
    assert!(a.diagnostics.is_empty(), "got: {:?}", a.diagnostics);
}

#[test]
fn a_parse_error_does_not_panic() {
    let a = STD.with(|s| s.analyse("def main = let in\n"));
    assert!(!a.diagnostics.is_empty());
    assert!(a.hover_at(0).is_none() || a.hover_at(0).is_some());
}

#[test]
fn positions_map_through_to_spans() {
    let src = "fun double n = n * 2\ndef main = double 21\n";
    let a = STD.with(|s| s.analyse(src));
    let idx = LineIndex::new(src);
    // Line 1, character 11 is inside `double` in `def main = double 21`.
    let off = idx.offset(1, 11);
    let def = a.definition_at(off).expect("definition");
    let (line, _) = idx.position(def.start as usize);
    assert_eq!(line, 0, "the definition is on the first line");
}

