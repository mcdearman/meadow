//! The VS Code extension's declarative parts.
//!
//! The grammar and the manifest are JSON that nothing else validates: a stray
//! comma ships an extension that fails to load, and the failure only shows up in
//! an editor. These are cheap to check here.

use serde_json::Value;
use std::path::PathBuf;

fn editor_file(rel: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode")
        .join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_manifest_is_valid_and_points_at_files_that_exist() {
    let pkg = editor_file("package.json");
    assert_eq!(pkg["contributes"]["languages"][0]["id"], "meadow");
    assert_eq!(pkg["contributes"]["languages"][0]["extensions"][0], ".mw");
    assert_eq!(pkg["activationEvents"][0], "onLanguage:meadow");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editors/vscode");
    for rel in [
        pkg["main"].as_str().unwrap(),
        pkg["contributes"]["languages"][0]["configuration"]
            .as_str()
            .unwrap(),
        pkg["contributes"]["grammars"][0]["path"].as_str().unwrap(),
    ] {
        let p = root.join(rel.trim_start_matches("./"));
        assert!(p.exists(), "the manifest points at {rel}, which is missing");
    }
}

#[test]
fn the_grammar_is_valid_and_matches_the_declared_scope() {
    let grammar = editor_file("syntaxes/meadow.tmLanguage.json");
    let pkg = editor_file("package.json");
    assert_eq!(
        grammar["scopeName"],
        pkg["contributes"]["grammars"][0]["scopeName"],
        "the grammar's scope must be the one the manifest declares"
    );
    // Every `include` has to name a rule that exists, or highlighting silently
    // stops at that point.
    let repo = grammar["repository"].as_object().unwrap();
    for pat in grammar["patterns"].as_array().unwrap() {
        let name = pat["include"].as_str().unwrap();
        let key = name.trim_start_matches('#');
        assert!(repo.contains_key(key), "no rule named {name}");
    }
}

#[test]
fn the_grammar_covers_the_languages_keywords() {
    let grammar = editor_file("syntaxes/meadow.tmLanguage.json");
    let text = grammar.to_string();
    for kw in [
        "fun", "def", "let", "in", "match", "with", "if", "then", "else", "data", "record",
        "effect", "handle", "use", "mod",
    ] {
        assert!(text.contains(kw), "the grammar never mentions `{kw}`");
    }
}

#[test]
fn the_language_configuration_uses_meadows_comment_syntax() {
    let cfg = editor_file("language-configuration.json");
    assert_eq!(cfg["comments"]["lineComment"], "--");
}
