//! The REPL's `Ctrl-F` finder, driven with keys and read off what it drew.

use meadow::finder::{Catalog, Choice, Picker, Reach, Step};
use meadow::{complete, stdlib};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The finder as a fresh session has it: the library, and nothing `use`d.
fn catalog() -> Catalog {
    let (packages, _) = stdlib::std_packages(meadow::Options::debug());
    let names = complete::snapshot(&packages, &[]);
    Catalog {
        index: meadow_find::Index::new(
            packages
                .iter()
                .flat_map(|p| meadow_find::collect::package(p, Default::default()))
                .collect(),
        ),
        scope: names.value_vars,
        aliases: names.qualified_vars,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(p: &mut Picker, text: &str) {
    for c in text.chars() {
        assert_eq!(p.key(key(KeyCode::Char(c))), Step::Continue);
    }
}

/// What the picker draws on a screen of this size, as text.
fn screen(p: &mut Picker, width: u16, height: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(|f| p.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_name_in_scope_goes_on_the_prompt_as_it_is() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "length");
    let d = p.selected().expect("something called length");
    assert_eq!(d.name, "length");
    assert_eq!(c.reach(d), Reach::InScope, "{}", d.qualified());
    assert_eq!(
        p.key(key(KeyCode::Enter)),
        Step::Choose(Choice {
            text: "length".into(),
            needs: None,
        })
    );
}

#[test]
fn a_name_not_in_scope_brings_its_use_with_it() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "Path.components");
    let d = p.selected().expect("Std.Path.components");
    assert_eq!(d.qualified(), "Std.Path.components");
    assert_eq!(
        p.key(key(KeyCode::Enter)),
        Step::Choose(Choice {
            text: "components".into(),
            needs: Some("use Std.Path (components)".into()),
        })
    );
}

#[test]
fn a_name_reached_through_a_qualifier_is_written_with_it() {
    let mut c = catalog();
    let lines = c
        .index
        .decls
        .iter()
        .find(|d| d.qualified() == "Std.String.lines")
        .cloned()
        .unwrap();
    // As `use Std.String as S` would leave it.
    c.aliases.insert(
        "S".into(),
        [("lines".to_string(), lines.var.unwrap())].into(),
    );
    c.scope.remove("lines");
    assert_eq!(c.reach(&lines), Reach::Via("S".into()));
    assert_eq!(c.choose(&lines).text, "S.lines");
}

#[test]
fn a_name_in_scope_that_means_something_else_is_not_in_scope() {
    // `length` means the vector's; the list's is spelled the same and is not it.
    let c = catalog();
    let list = c
        .index
        .decls
        .iter()
        .find(|d| d.qualified() == "Std.Collections.List.length")
        .unwrap();
    assert_eq!(
        c.reach(list),
        Reach::NeedsUse("use Std.Collections.List (length)".into())
    );
}

#[test]
fn a_type_is_searched_as_a_type() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "[a] -> Int");
    let first = p.selected().expect("an answer");
    assert!(
        first.name == "length" || first.name == "len",
        "{}",
        first.qualified()
    );
    let drawn = screen(&mut p, 140, 30);
    assert!(drawn.contains("by type"), "{drawn}");
}

#[test]
fn the_selection_moves_and_wraps() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "map");
    let first = p.selected().unwrap().qualified();
    p.key(key(KeyCode::Down));
    assert_ne!(p.selected().unwrap().qualified(), first);
    p.key(key(KeyCode::Up));
    assert_eq!(p.selected().unwrap().qualified(), first);
    // Up from the top is the bottom.
    p.key(key(KeyCode::Up));
    let last = p.listed().last().unwrap().qualified();
    assert_eq!(p.selected().unwrap().qualified(), last);
}

#[test]
fn editing_the_query() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "lenght");
    p.key(key(KeyCode::Backspace));
    p.key(key(KeyCode::Backspace));
    typed(&mut p, "th");
    assert_eq!(p.query(), "length");
    p.key(key(KeyCode::Home));
    typed(&mut p, "x");
    assert_eq!(p.query(), "xlength");
    // Delete takes what is after the cursor, which is after the `x`.
    p.key(key(KeyCode::Delete));
    assert_eq!(p.query(), "xength");
    // Ctrl-U takes everything before it.
    p.key(ctrl('u'));
    assert_eq!(p.query(), "ength");
    p.key(key(KeyCode::End));
    p.key(ctrl('u'));
    assert_eq!(p.query(), "");
    typed(&mut p, "Maybe a -> ");
    p.key(ctrl('w'));
    assert_eq!(p.query(), "Maybe a ");
}

#[test]
fn escape_and_control_f_close_it_without_choosing() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    assert_eq!(p.key(key(KeyCode::Esc)), Step::Cancel);
    assert_eq!(p.key(ctrl('f')), Step::Cancel);
    assert_eq!(p.key(ctrl('c')), Step::Cancel);
}

#[test]
fn enter_with_nothing_listed_does_nothing() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "zzqqxxnothingmatches");
    assert!(p.selected().is_none());
    assert_eq!(p.key(key(KeyCode::Enter)), Step::Continue);
}

#[test]
fn the_preview_says_what_choosing_will_do() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "Path.components");
    let drawn = screen(&mut p, 140, 30);
    assert!(drawn.contains("Find a declaration"), "{drawn}");
    assert!(drawn.contains("Std.Path.components"), "{drawn}");
    assert!(drawn.contains("use Std.Path (components)"), "{drawn}");
    assert!(drawn.contains("by name"), "{drawn}");
}

#[test]
fn a_documented_declaration_shows_its_doc() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "String.Parse.parse");
    let d = p.selected().unwrap();
    assert_eq!(d.qualified(), "Std.String.Parse.parse");
    let first_line = d
        .doc
        .as_deref()
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let drawn = screen(&mut p, 160, 40);
    // Wrapping may break it, so only the start is certain to be on one line.
    let start: String = first_line.chars().take(20).collect();
    assert!(drawn.contains(&start), "{start:?} in\n{drawn}");
}

#[test]
fn it_draws_on_any_screen_without_panicking() {
    let c = catalog();
    let mut p = Picker::new(&c).plain();
    typed(&mut p, "map");
    for (w, h) in [(200, 60), (100, 30), (70, 20), (40, 10), (10, 5), (1, 1)] {
        screen(&mut p, w, h);
    }
    // Narrow, the list is what is kept.
    let narrow = screen(&mut p, 70, 20);
    assert!(narrow.contains("map"), "{narrow}");
}
