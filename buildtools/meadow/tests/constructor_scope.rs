//! Constructors live under their type -- everywhere, including the module that
//! declares the type.
//!
//! This is Rust's rule for an enum's variants. `Colour.Red` names a constructor
//! wherever `Colour` is in scope; `Red` on its own is in scope only where a
//! `use` put it: `use Colour.*` or `use Colour (Red)` in the declaring module,
//! `use M.Colour.*` or `use M.Colour (Red)` elsewhere. Declaring the type does
//! not, naming the type in a `use` does not, and a module glob does not.
//!
//! Two exceptions, both deliberate:
//!
//! * a constructor with its type's own name -- a `record`'s, or a one-case
//!   `data Wrap = Wrap Int` -- comes with the type, as a Rust struct does;
//! * `Just`, `None`, `Ok`, `Err`, `Less`, `Equal` and `Greater` are re-exported by
//!   the prelude, and `True`, `False`, `Nil` and `Cons` are the language's own.
//!
//! The rule was once enforced only across modules, which let a module's own
//! constructors -- and, through that, `Std`'s -- be written bare without anyone
//! noticing. So every position a constructor can be written in is tested here,
//! in the declaring module, in a sibling, and in a nested module.

mod common;
use common::{errors, eval_main, eval_main_std, eval_unit, unit_errors};

const COLOUR: &str = "data Colour = Red | Green | Blue\n";
const OPT: &str = "data Opt = Nothing | Some Int\n";

/// Every error in `src`, and the check that there is at least one about `name`.
#[track_caller]
fn refused(src: &str, name: &str) -> String {
    let out = errors(src);
    assert!(
        out.contains(&format!("unknown constructor `{name}`")),
        "`{name}` should not be in scope bare:\n{src}\ngot: {out:?}"
    );
    out
}

// --- in the declaring module: bare is refused, wherever it is written -------

#[test]
fn a_bare_constructor_in_an_expression_is_refused() {
    refused(&format!("{COLOUR}def main = Red\n"), "Red");
}

#[test]
fn a_bare_constructor_applied_to_arguments_is_refused() {
    refused(&format!("{OPT}def main = Some 1\n"), "Some");
}

#[test]
fn a_bare_constructor_in_a_match_arm_is_refused() {
    let src = format!(
        "{COLOUR}fun name c = match c with | Red -> 1 | Colour.Green -> 2 | Colour.Blue -> 3\ndef main = name Colour.Red\n"
    );
    refused(&src, "Red");
}

#[test]
fn a_bare_constructor_nested_in_a_pattern_is_refused() {
    let src = format!(
        "{OPT}fun f p = match p with | (Some x, _) -> x | _ -> 0\ndef main = f (Opt.Some 1, 2)\n"
    );
    refused(&src, "Some");
}

#[test]
fn a_bare_constructor_in_a_parameter_pattern_is_refused() {
    let src = "data Box = Box Int | Empty\nfun f (Empty) = 0\ndef main = 1\n";
    refused(src, "Empty");
}

#[test]
fn a_bare_constructor_in_a_let_pattern_is_refused() {
    let src = format!("{OPT}fun f u = let (Some x) = Opt.Some 1 in x\ndef main = f ()\n");
    refused(&src, "Some");
}

#[test]
fn a_bare_constructor_used_as_a_function_is_refused() {
    let src = format!("{OPT}fun apply f x = f x\ndef main = apply Some 1\n");
    refused(&src, "Some");
}

#[test]
fn a_bare_constructor_inside_a_lambda_is_refused() {
    let src = format!("{COLOUR}def main = (\\u -> Red) ()\n");
    refused(&src, "Red");
}

#[test]
fn a_bare_constructor_inside_a_handler_clause_is_refused() {
    let src = format!(
        "{COLOUR}effect Pick {{ pick : () -> Colour }}\n\
         def main = handle pick () with {{ pick u k -> k Blue }}\n"
    );
    refused(&src, "Blue");
}

#[test]
fn a_bare_constructor_in_a_local_function_is_refused() {
    let src = format!("{COLOUR}fun f u = let g v = Green in g ()\ndef main = f ()\n");
    refused(&src, "Green");
}

#[test]
fn the_error_says_where_the_constructor_lives() {
    let out = refused(&format!("{COLOUR}def main = Red\n"), "Red");
    assert_eq!(
        out,
        "unknown constructor `Red`: it belongs to `Colour`, so write `Colour.Red`, or bring it in with `use Colour.*`"
    );
}

#[test]
fn the_error_lists_every_type_it_could_belong_to() {
    let src = "data Tree = Leaf | Node Tree Tree\ndata Rope = Leaf String | Node Rope Rope\ndef main = Leaf\n";
    assert_eq!(
        errors(src),
        "unknown constructor `Leaf`: it could be `Rope.Leaf`, `Tree.Leaf`; say which, or `use` one"
    );
}

#[test]
fn a_constructor_no_type_has_is_just_unknown() {
    assert_eq!(
        errors(&format!("{COLOUR}def main = Purple\n")),
        "unknown constructor `Purple`"
    );
}

// --- in the declaring module: what is allowed ------------------------------

#[test]
fn a_qualified_constructor_needs_no_use() {
    let src = format!(
        "{COLOUR}fun rank c = match c with | Colour.Red -> 1 | Colour.Green -> 2 | Colour.Blue -> 3\n\
         def main = (rank Colour.Blue, Colour.Red == Colour.Red)\n"
    );
    assert_eq!(errors(&src), "");
    assert_eq!(eval_main(&src), "(3, true)");
}

#[test]
fn use_type_glob_brings_every_constructor_of_a_local_type() {
    let src = format!(
        "{COLOUR}use Colour.*\n\
         fun rank c = match c with | Red -> 1 | Green -> 2 | Blue -> 3\n\
         def main = rank Blue\n"
    );
    assert_eq!(errors(&src), "");
    assert_eq!(eval_main(&src), "3");
}

#[test]
fn use_type_with_a_list_brings_only_those() {
    let src = format!("{COLOUR}use Colour (Red)\ndef main = (Red, Green)\n");
    let out = refused(&src, "Green");
    assert!(!out.contains("`Red`"), "{out}");
    let src = format!("{COLOUR}use Colour (Red, Blue)\ndef main = (Red, Blue, Colour.Green)\n");
    assert_eq!(errors(&src), "");
}

#[test]
fn a_use_may_come_before_the_type_it_names() {
    let src = format!("use Colour.*\n{COLOUR}def main = Red\n");
    assert_eq!(errors(&src), "");
}

#[test]
fn a_use_of_a_constructor_the_type_lacks_is_reported() {
    let src = format!("{COLOUR}use Colour (Red, Purple)\ndef main = Red\n");
    assert_eq!(errors(&src), "`Colour` has no constructor `Purple`");
}

#[test]
fn two_local_types_with_the_same_constructors_stay_apart() {
    let src = "data Tree = Leaf | Node Tree Tree\n\
               data Rope = Leaf String | Node Rope Rope\n\
               def main = (Tree.Node Tree.Leaf Tree.Leaf, Rope.Leaf \"x\")\n";
    assert_eq!(errors(src), "");
    // Both brought in: each use is chosen by its type.
    let src = "data Tree = Leaf | Node Tree Tree\n\
               data Rope = Leaf String | Node Rope Rope\n\
               use Tree.*\nuse Rope.*\n\
               fun depth (t : Tree) = match t with | Leaf -> 0 | Node a b -> 1 + depth a\n\
               def main = (depth (Node Leaf Leaf), Leaf \"x\")\n";
    assert_eq!(errors(src), "");
}

#[test]
fn a_constructor_named_like_its_type_comes_with_the_type() {
    // A one-case wrapper and a record: Rust's structs.
    let src = "data Wrap = Wrap Int\n\
               record Point = { x : Int, y : Int }\n\
               fun unwrap (Wrap n) = n\n\
               def main = (unwrap (Wrap 3), match Point { x = 1, y = 2 } with | Point { x = a, y = b } -> a + b)\n";
    assert_eq!(errors(src), "");
    assert_eq!(eval_main(src), "(3, 3)");
}

#[test]
fn a_types_other_constructors_do_not_come_with_one_named_like_it() {
    let src = "data Wrap = Wrap Int | Unwrapped\ndef main = (Wrap 1, Unwrapped)\n";
    refused(src, "Unwrapped");
}

#[test]
fn the_rule_holds_in_a_package_module_as_in_a_single_file() {
    let out = unit_errors(&[
        ("", "mod Paint\n"),
        (
            "Paint",
            "data Colour = Red | Green\n@pub(pkg) def red = Red\n",
        ),
    ]);
    assert!(out.contains("unknown constructor `Red`"), "{out}");
    assert_eq!(
        unit_errors(&[
            ("", "mod Paint\ndef main = 1\n"),
            (
                "Paint",
                "use Colour.*\ndata Colour = Red | Green\n@pub(pkg) def red = Red\n"
            )
        ]),
        ""
    );
}

#[test]
fn a_local_use_is_this_modules_alone() {
    // `Paint` says `use Colour.*` for itself; the root still has to say so for
    // itself, through the path.
    let modules = [
        ("", "mod Paint\nuse Paint.Colour\ndef main = Red\n"),
        (
            "Paint",
            "use Colour.*\n@pub(pkg) data Colour = Red | Green\n",
        ),
    ];
    assert!(
        unit_errors(&modules).contains("unknown constructor `Red`"),
        "{}",
        unit_errors(&modules)
    );
    let modules = [
        (
            "",
            "mod Paint\nuse Paint.Colour.*\ndef main = match Red with | Red -> 1 | Green -> 2\n",
        ),
        (
            "Paint",
            "use Colour.*\n@pub(pkg) data Colour = Red | Green\n",
        ),
    ];
    assert_eq!(eval_unit(&modules), "1");
}

#[test]
fn a_nested_module_does_not_see_its_parents_constructors_bare() {
    // The root declares `Colour` and brings its constructors in for itself; a
    // module inside it names the type through the package, and still has to
    // bring the constructors in for itself.
    let modules = [
        (
            "",
            "mod Inner
use Colour.*
@pub(pkg) data Colour = Red | Green
def main = Red
",
        ),
        (
            "Inner",
            "use test.Colour
@pub(pkg) def shade = Red
",
        ),
    ];
    let out = unit_errors(&modules);
    assert_eq!(
        out,
        "unknown constructor `Red`: it belongs to `Colour`, so write `Colour.Red`, or bring it in with a `use` of `Colour`'s constructors"
    );
    let modules = [
        (
            "",
            "mod Inner
use Colour.*
@pub(pkg) data Colour = Red | Green
def main = Red
",
        ),
        (
            "Inner",
            "use test.Colour.*
@pub(pkg) def shade = match Red with | Red -> Green | Green -> Red
",
        ),
    ];
    assert_eq!(unit_errors(&modules), "");
}

// --- across packages -------------------------------------------------------

#[test]
fn a_standard_library_modules_private_constructors_do_not_leak() {
    // `Std.Stm` declares `data Attempt a = Done a | Retried | Conflicted` for
    // itself. None of those are anyone else's to write.
    for name in ["Done", "Retried", "Conflicted"] {
        let out =
            common::errors_std_with(&format!("def main = {name}\n"), meadow::Options::debug());
        assert!(
            out.contains(&format!("unknown constructor `{name}`")),
            "{name}: {out}"
        );
    }
}

#[test]
fn a_dependencys_constructors_need_a_use_even_when_the_type_is_in_scope() {
    let out = common::errors_std_with(
        "use Std.Either (Either)\ndef main = Left 1\n",
        meadow::Options::debug(),
    );
    assert!(out.contains("unknown constructor `Left`"), "{out}");
    assert_eq!(
        common::eval_main_std("use Std.Either (Either)\ndef main = Either.Left 1\n"),
        "Left(1)"
    );
}

#[test]
fn the_preludes_constructors_are_bare_everywhere() {
    let src = "fun f m = match m with | Just x -> Ok x | None -> Err \"none\"\n\
               def main = (f (Just 1), compare 1 2 == Less, True, [1; 2])\n";
    assert_eq!(eval_main_std(src), "(Ok(1), true, true, [1; 2])");
}

#[test]
fn the_languages_own_constructors_are_bare_everywhere() {
    let src = "fun len xs = match xs with | Nil -> 0 | Cons x rest -> 1 + len rest\n\
               def main = (len (Cons 1 (Cons 2 Nil)), if True then 1 else 0)\n";
    assert_eq!(eval_main(src), "(2, 1)");
}

/// Compile `src` as a unit depending on `lib`, which is compiled first the way
/// an earlier REPL line or an ad-hoc dependency is: every name flat. Answers
/// the second unit's diagnostics.
fn against(lib: &str, src: &str) -> String {
    use meadow_compiler::{
        AstModule,
        intern::InternedString,
        lexer::tokenize,
        parser,
        source::{Source, SourceKind},
    };
    let (dep, diags) = meadow_compiler::compile_str("lib", lib);
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let source = Source::new(SourceKind::Interactive, src.into());
    let lex = tokenize(source);
    let name = InternedString::from("app");
    let (ast, perrs) = parser::parse(name, source, &lex.tokens);
    assert!(perrs.is_empty(), "{perrs:?}");
    let module = AstModule {
        path: Vec::new(),
        name,
        ast: ast.expect("parsed"),
        source,
    };
    let (_, diags) =
        meadow_compiler::compile_unit(name, 1, vec![module], &[&dep], meadow::Options::debug());
    diags
        .iter()
        .map(|d| d.msg.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_earlier_units_constructors_are_under_their_types_too() {
    // What a REPL line sees of the lines before it.
    let lib = "data Colour = Red | Green\nrecord Point = { x : Int }\n";
    assert!(against(lib, "def main = Red\n").contains("unknown constructor `Red`"));
    assert_eq!(against(lib, "def main = Colour.Red\n"), "");
    assert_eq!(against(lib, "use Colour.*\ndef main = (Red, Green)\n"), "");
    // A record's constructor is its type's, and comes with it.
    assert_eq!(against(lib, "def main = Point { x = 1 }\n"), "");
}

#[test]
fn a_package_that_marks_nothing_still_keeps_its_constructors_under_their_types() {
    // No `@pub` anywhere, so every name is exported -- but a constructor is not
    // a name of the package, it is its type's.
    let lib = "data Shape = Circle Int | Square Int\nfun area s = match s with | Shape.Circle r -> r | Shape.Square w -> w\n";
    assert!(against(lib, "def main = area (Circle 1)\n").contains("unknown constructor `Circle`"));
    assert_eq!(against(lib, "def main = area (Shape.Circle 1)\n"), "");
}
