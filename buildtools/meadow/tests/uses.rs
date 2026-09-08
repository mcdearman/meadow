//! `use` declarations: module resolution, the `as` alias, and reporting a path
//! that names no module.

mod common;
use common::{errors_std_with, eval_main_std};
use meadow::Options;

fn errors(src: &str) -> String {
    errors_std_with(src, Options::debug())
}

// --- resolution errors -------------------------------------------------------

#[test]
fn unknown_module_is_an_error() {
    // Previously this was silently accepted, and only a *use site* of the
    // qualifier complained.
    assert_eq!(errors("use Nowhere\ndef main = 1\n"), "no module `Nowhere`");
}

#[test]
fn unknown_submodule_of_a_real_package_is_an_error() {
    assert_eq!(
        errors("use Std.Nope\ndef main = 1\n"),
        "no module `Std.Nope`"
    );
}

#[test]
fn a_misplaced_module_suggests_its_real_path() {
    assert_eq!(
        errors("use List\ndef main = 1\n"),
        "no module `List` — did you mean `Std.Collections.List`?"
    );
    assert_eq!(
        errors("use Std.Vector\ndef main = 1\n"),
        "no module `Std.Vector` — did you mean `Std.Collections.Vector`?"
    );
}

#[test]
fn a_real_module_is_accepted() {
    assert_eq!(errors("use Std.Collections.List\ndef main = 1\n"), "");
}

#[test]
fn a_module_that_only_declares_submodules_is_accepted() {
    // `Std.Collections` is nothing but `mod` lines, so it exports no values —
    // which must not be mistaken for "no such module".
    assert_eq!(errors("use Std.Collections\ndef main = 1\n"), "");
}

// --- qualifiers and aliases --------------------------------------------------

#[test]
fn last_path_segment_is_the_default_qualifier() {
    assert_eq!(
        eval_main_std("use Std.Collections.List\ndef main = List.length [1; 2; 3]\n"),
        "3"
    );
}

#[test]
fn as_renames_the_qualifier() {
    assert_eq!(
        eval_main_std("use Std.Collections.List as L\ndef main = L.length [1; 2; 3]\n"),
        "3"
    );
}

#[test]
fn the_alias_replaces_the_default_qualifier() {
    assert_eq!(
        errors("use Std.Collections.List as L\ndef main = List.length [1; 2]\n"),
        "module `List` is not in scope here (add `use List`)"
    );
}

#[test]
fn two_aliases_for_one_module_coexist() {
    assert_eq!(
        eval_main_std(
            "use Std.Collections.List\n\
             use Std.Collections.List as L\n\
             def main = (List.length [1; 2; 3], L.length [1; 2])\n"
        ),
        "(3, 2)"
    );
}

#[test]
fn alias_combines_with_selected_names() {
    assert_eq!(
        eval_main_std(
            "use Std.Collections.List as L (length)\n\
             def main = (length [1; 2], L.reverse [1; 2])\n"
        ),
        "(2, [2; 1])"
    );
}

#[test]
fn distinct_modules_keep_distinct_aliases() {
    assert_eq!(
        eval_main_std(
            "use Std.Collections.List as L\n\
             use Std.Collections.Tree as T\n\
             def main = (L.length [1; 2], T.toList (T.fromList [2; 1]))\n"
        ),
        "(2, [1; 2])"
    );
}
