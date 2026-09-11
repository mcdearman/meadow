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

/// The standard library, in both the shapes the server wants: the bundle a
/// package depends on, and the modules it was bundled from.
fn std() -> Std {
    let opts = meadow::Options::debug();
    let modules = meadow::stdlib::std_modules(opts)
        .0
        .into_iter()
        .map(|(dotted, pkg)| (dotted.to_string(), pkg))
        .collect();
    Std::new(meadow::stdlib::std_packages(opts).0, modules, None)
}

thread_local! {
    static STD: Std = std();
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
    let def = a.definition_at(off, &Default::default(), &Default::default()).expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[def.span.start as usize..def.span.end as usize], "double");
}

#[test]
fn go_to_definition_works_for_a_local_binding() {
    let src = "fun f u =\n  let answer = 42 in\n  ans@wer + 1\ndef main = f ()\n";
    let (a, off) = at(src, "@");
    let def = a.definition_at(off, &Default::default(), &Default::default()).expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[def.span.start as usize..def.span.end as usize], "answer");
    // …and it is the *local* one, not something at the top level.
    assert!(def.span.start as usize > clean.find("let").unwrap());
}

#[test]
fn the_innermost_node_wins() {
    // At `xs` the type is the vector's, not the enclosing `len xs`'s `Int`.
    let (a, off) = at("fun size xs = len @xs\ndef main = size [1, 2]\n", "@");
    assert_eq!(a.type_at(off), Some("[a]"));
}

/// Render the hints back into the source, the way an editor overlays them.
fn hinted(src: &str) -> String {
    use meadow_lsp::analysis::HintPart;
    let a = STD.with(|s| s.analyse(src));
    let mut out = src.to_string();
    let mut hints: Vec<_> = a.binders.iter().collect();
    // Right to left, so an earlier insertion does not move a later offset.
    hints.sort_by_key(|h| std::cmp::Reverse(h.offset));
    for h in hints {
        let text: String = h
            .parts
            .iter()
            .map(|p| match p {
                HintPart::Text(t) => t.clone(),
                HintPart::Name(n) => n.to_string(),
            })
            .collect();
        out.insert_str(h.offset as usize, &text);
    }
    out
}

/// Hints read as the annotation you could have typed in their place.
///
/// The parentheses are the point: `(x : Int)` is real syntax, so the hint is
/// written as a replacement for the name rather than as a notation of its own.
#[test]
fn inlay_hints_read_as_a_writable_annotation() {
    assert_eq!(
        hinted("fun add a b = a + b\n"),
        "fun add (a : Int) (b : Int) : Int = a + b\n"
    );
    assert_eq!(
        hinted("fun f u = let n = 1 in n\n"),
        "fun f (u : a) : Int = let (n : Int) = 1 in n\n"
    );
}

#[test]
fn a_parameter_that_is_already_annotated_is_not_hinted_again() {
    assert_eq!(
        hinted("fun add (a : Int) b = a + b\n"),
        "fun add (a : Int) (b : Int) : Int = a + b\n"
    );
}

/// A parameter that brings its own parentheses closes before its span ends, so
/// the result type cannot be appended to it and gets a hint of its own.
#[test]
fn a_parenthesised_parameter_still_gets_a_result_type() {
    assert_eq!(
        hinted("fun snd (a, b) = b\n"),
        "fun snd ((a : a), (b : b)) : b = b\n"
    );
}

#[test]
fn a_lambda_gets_its_parameters_but_no_result_type() {
    // `\(n : Int) -> …` is writable; a result type there is not.
    assert_eq!(hinted("def f = \\n -> n + 1\n"), "def f = \\(n : Int) -> n + 1\n");
}

/// A type name in a hint is a part of its own, so the server can link it.
#[test]
fn a_hint_names_the_types_inside_it_separately() {
    use meadow_lsp::analysis::HintPart;
    let a = STD.with(|s| s.analyse("fun pick m = match m with | Just v -> v | None -> 0\n"));
    let names: Vec<String> = a
        .binders
        .iter()
        .flat_map(|h| &h.parts)
        .filter_map(|p| match p {
            HintPart::Name(n) => Some(n.to_string()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"Maybe".to_string()), "got {names:?}");
    assert!(names.contains(&"Int".to_string()), "got {names:?}");
    // Whether a name *links* is decided when the request is answered, by
    // whether the index has it: `Maybe` is declared somewhere, `Int` is not.
    STD.with(|s| {
        let ty = |n: &str| {
            meadow_compiler::intern::InternedString::from(n)
        };
        assert!(s.declared_names().types.contains_key(&ty("Maybe")));
        assert!(!s.declared_names().types.contains_key(&ty("Int")));
    });
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
    let def = a.definition_at(off, &Default::default(), &Default::default()).expect("definition");
    let (line, _) = idx.position(def.span.start as usize);
    assert_eq!(line, 0, "the definition is on the first line");
}


// --- editing the standard library itself -----------------------------------

#[test]
fn a_std_module_recognises_itself_by_path() {
    let s = std();
    for (uri, want) in [
        ("file:///c%3A/repos/meadow/lib/Std/src/Collections/Vector.mw", Some("Collections.Vector")),
        ("file:///home/u/meadow/lib/Std/src/Maybe.mw", Some("Maybe")),
        ("file:///home/u/meadow/lib/Std/src/prelude.mw", Some("prelude")),
        // Backslashes, as a Windows path reaches us.
        ("c:\\repos\\meadow\\lib\\Std\\src\\Json.mw", Some("Json")),
        // Not one of ours, however much it looks like one.
        ("file:///home/u/other/lib/Std/src/NotAModule.mw", None),
        ("file:///home/u/meadow/examples/euler/src/main.mw", None),
    ] {
        let got = s.module_at(uri).map(|i| s.module_name(i));
        assert_eq!(got, want, "{uri}");
    }
}

#[test]
fn every_std_module_analyses_clean_as_itself() {
    // The bug this exists for: a `Std` source opened in the editor was analysed
    // as a package *depending* on `Std`, so every type and constructor it
    // declares was declared twice — once here, once in its own dependency — and
    // the file filled with `already defined`.
    //
    // It also keeps `module_path` here honest against the one in
    // `meadow::stdlib`, which this crate cannot call: if the two ever disagree
    // about where a module sits, its exports land in the wrong place and
    // something below stops resolving.
    let s = std();
    let opts = meadow::Options::debug();
    for (dotted, _) in meadow::stdlib::std_modules(opts).0 {
        let source = meadow::stdlib::MODULES
            .iter()
            .find(|(name, _)| *name == dotted)
            .expect("every compiled module comes from MODULES")
            .1;
        let i = s
            .module_at(&format!("file:///w/lib/Std/src/{}.mw", dotted.replace('.', "/")))
            .unwrap_or_else(|| panic!("{dotted} should be recognised by its path"));
        let a = s.analyse_module(i, source);
        assert!(
            a.diagnostics.is_empty(),
            "{dotted} does not analyse clean:\n{}",
            a.diagnostics
                .iter()
                .map(|d| format!("  {}", d.msg))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn a_std_module_analysed_the_ordinary_way_collides_with_itself() {
    // Which is why the special case exists. `Maybe` declares `Option`, and the
    // `Std` a normal document depends on already has one.
    let s = std();
    let source = meadow::stdlib::MODULES
        .iter()
        .find(|(name, _)| *name == "Maybe")
        .unwrap()
        .1;
    let a = s.analyse(source);
    assert!(
        a.diagnostics.iter().any(|d| d.msg.contains("already defined")),
        "expected the collision this feature avoids, got {:?}",
        a.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
}

// --- across modules -----------------------------------------------------------

/// A name from another module resolves to *that module's* definition.
///
/// The point is not that a span comes back — it is that the span is an offset
/// into a different file, and that the file is the one that defines the name.
/// Checking the text at the span against the source the `Loc` names is what
/// makes this test able to fail: a span from the wrong file would land on
/// whatever happens to sit at that offset.
#[test]
fn go_to_definition_reaches_into_the_standard_library() {
    let src = "use Std.String (concatAll)\ndef main = concat@All\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for an imported name");

    assert_ne!(
        loc.source.id, a.source_id,
        "the definition should be in another source, not the open document"
    );
    let text = &loc.source.content[loc.span.start as usize..loc.span.end as usize];
    assert_eq!(text, "concatAll");
    assert!(
        loc.source.name().to_string().ends_with("String.mw"),
        "expected Std/String.mw, got {}",
        loc.source.name()
    );
}

/// A name the prelude re-exports, reached with no `use` at all.
///
/// `map` is deliberate: `Std.Collections.List` defines one too, and the prelude
/// says bare `map` is the *`Vector`* one. Landing in `Vector.mw` is the test --
/// an index that merely found *a* binding called `map` would be wrong here.
#[test]
fn a_prelude_name_is_followed_to_the_module_that_defines_it() {
    let src = "fun incr x = x + 1\ndef main = ma@p incr [1; 2; 3]\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for a prelude name");
    let text = &loc.source.content[loc.span.start as usize..loc.span.end as usize];
    assert_eq!(text, "map");
    assert!(
        loc.source.name().to_string().ends_with("Collections/Vector.mw"),
        "bare `map` is Vector's, got {}",
        loc.source.name()
    );
}

/// A local binding still wins over an imported one of the same name, and still
/// reports the open document.
#[test]
fn a_local_definition_is_preferred_to_an_imported_one() {
    let src = "fun map f xs = xs\nfun id x = x\ndef main = ma@p id [1; 2]\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("definition");
    assert_eq!(
        loc.source.id, a.source_id,
        "the shadowing definition is the one in this file"
    );
}

/// Editing one `Std` module and following a name into an earlier one — the case
/// that only works because a module analysed in its own place is compiled
/// against the *same* units the index was built from.
#[test]
fn one_std_module_can_reach_another() {
    STD.with(|s| {
        let i = s
            .module_at("/anywhere/lib/Std/src/Collections/Vector.mw")
            .expect("Vector is a Std module");
        let text = meadow::stdlib::MODULES
            .iter()
            .find(|(d, _)| *d == "Collections.Vector")
            .expect("its source")
            .1;
        let a = s.analyse_module(i, text);

        // Some name it uses that is defined elsewhere in the library.
        let (off, _) = a
            .refs
            .iter()
            .filter_map(|(span, v)| s.definitions().get(v).map(|l| (span.start as usize, *l)))
            .find(|(_, l)| l.source.id != a.source_id)
            .expect("Vector refers to something from another module");

        let loc = a
            .definition_at(off, s.definitions(), s.declared_names())
            .expect("and it can be followed");
        assert_ne!(loc.source.id, a.source_id);
        assert!(loc.source.name().to_string().ends_with(".mw"));
    });
}

// --- types and data constructors ----------------------------------------------

/// Follow a type name, in the same file.
#[test]
fn go_to_definition_finds_a_type() {
    let src = "data Colour = Red | Green\ndata Box = Box Col@our\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for a type");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[loc.span.start as usize..loc.span.end as usize], "Colour");
    assert_eq!(loc.span.start as usize, clean.find("Colour").unwrap());
}

/// Follow a data constructor from an expression, and from a pattern.
#[test]
fn go_to_definition_finds_a_constructor() {
    for src in [
        "data Colour = Red | Green\ndef main = Gre@en\n",
        "data Colour = Red | Green\nfun f c = match c with | Gre@en -> 1 | Red -> 0\ndef main = f Red\n",
    ] {
        let (a, off) = at(src, "@");
        let loc = STD
            .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
            .expect("a definition for a constructor");
        let clean = src.replacen("@", "", 1);
        assert_eq!(&clean[loc.span.start as usize..loc.span.end as usize], "Green");
        // The *declaration*, not the other use site.
        assert_eq!(loc.span.start as usize, clean.find("Green").unwrap(), "{src}");
    }
}

/// A name that is both a type and a constructor resolves by where it is written.
///
/// `data Pair = Pair Int Int` puts `Pair` in both namespaces at different
/// positions. An index that kept one map would answer one of them for both, and
/// be right half the time.
#[test]
fn a_type_and_a_constructor_may_share_a_name() {
    let src = "data Pair = Pair Int Int\nfun fst p = match p with | Pai@r a b -> a\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("definition");
    let clean = src.replacen("@", "", 1);
    // The *constructor* `Pair`, which is the second occurrence on line 1.
    let ctor = clean.find("= Pair").unwrap() + 2;
    assert_eq!(loc.span.start as usize, ctor, "should be the constructor, not the type");

    // And in a type position, the type.
    let src = "data Pair = Pair Int Int\ndata Box = Box Pai@r\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(loc.span.start as usize, clean.find("Pair").unwrap());
}

/// The innermost name wins: in `Outer Inner` the cursor decides which.
#[test]
fn the_innermost_type_wins() {
    let src = "\
data Inner = I
data Outer a = O a
data Holder = Holder (Outer Inn@er)
def main = 0
";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("definition");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[loc.span.start as usize..loc.span.end as usize], "Inner");
}

/// A builtin type has no declaration, and the answer is *nothing* rather than
/// the enclosing type.
///
/// `Int` in `Outer Int` is nested inside a name that does have a definition, so
/// a lookup that widened on a miss would jump to `Outer` — a confident wrong
/// answer, which is worse than none.
#[test]
fn a_builtin_type_has_nowhere_to_go() {
    let src = "data Outer a = O a\ndata Holder = Holder (Outer In@t)\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD.with(|s| a.definition_at(off, s.definitions(), s.declared_names()));
    assert!(
        loc.is_none(),
        "expected no definition for a builtin, got {:?}",
        loc.map(|l| l.source.name().to_string())
    );
}

/// A type from another module, across the package boundary.
#[test]
fn go_to_definition_reaches_a_type_in_the_standard_library() {
    let src = "data Holder = Holder (May@be Int)\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for an imported type");
    assert_ne!(loc.source.id, a.source_id, "it is in another file");
    let text = &loc.source.content[loc.span.start as usize..loc.span.end as usize];
    assert_eq!(text, "Maybe");
    assert!(
        loc.source.name().to_string().ends_with("Maybe.mw"),
        "got {}",
        loc.source.name()
    );
}

/// And a constructor from another module.
#[test]
fn go_to_definition_reaches_a_constructor_in_the_standard_library() {
    let src = "def main = Jus@t 5\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for an imported constructor");
    assert_ne!(loc.source.id, a.source_id);
    let text = &loc.source.content[loc.span.start as usize..loc.span.end as usize];
    assert_eq!(text, "Just");
    assert!(
        loc.source.name().to_string().ends_with("Maybe.mw"),
        "got {}",
        loc.source.name()
    );
}

/// An effect is a type too, and its operations are values.
#[test]
fn go_to_definition_covers_effects_and_their_operations() {
    let src = "effect Counter { bump : Int -> Int }\ndef main = bum@p 1\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("an effect operation is a value");
    let clean = src.replacen("@", "", 1);
    assert_eq!(&clean[loc.span.start as usize..loc.span.end as usize], "bump");
}

/// A result type that is written is not also hinted.
///
/// The four spellings below differ only in how much the author typed; the line
/// an editor shows is the same for all of them, which is the property that
/// makes a hint and an annotation interchangeable.
#[test]
fn a_written_result_type_is_not_hinted_again() {
    let want = "fun f (x : Int) (y : Int) : Int = x + y\n";
    assert_eq!(hinted("fun f x y = x + y\n"), want);
    assert_eq!(hinted("fun f (x : Int) y = x + y\n"), want);
    assert_eq!(hinted("fun f x y : Int = x + y\n"), want);
    assert_eq!(hinted("fun f (x : Int) (y : Int) : Int = x + y\n"), want);
}

/// A type named in a result annotation can be followed, like any other.
#[test]
fn go_to_definition_reaches_a_type_from_a_result_annotation() {
    let src = "fun f (n : Int) : May@be Int = None\ndef main = 0\n";
    let (a, off) = at(src, "@");
    let loc = STD
        .with(|s| a.definition_at(off, s.definitions(), s.declared_names()))
        .expect("a definition for the result type");
    assert_ne!(loc.source.id, a.source_id);
    assert!(
        loc.source.name().to_string().ends_with("Maybe.mw"),
        "got {}",
        loc.source.name()
    );
}
