//! What to offer when someone asks for completions.
//!
//! The interesting case is the pipe. After `expr |>` the document says what
//! the value *is*, so the list can be what that value can be piped into rather
//! than every name in scope -- the menu an object-oriented language puts
//! behind a full stop, except that here there are two ways to fit:
//!
//! * `x |> f` is `f x`, so a function whose **first** parameter takes `x`;
//! * a library written to chain takes its subject **last** --
//!   `table |> setWidth 40` is `setWidth 40 table` -- so a function whose last
//!   parameter takes `x`, offered with the earlier arguments left to fill in.
//!
//! A half-written pipe does not parse, and a document that does not parse has
//! no types at all, so the text is repaired before it is analysed: a
//! placeholder goes where the function will be, and the value to the left of
//! the `|>` is then a typed node like any other.
//!
//! The other half of this is the dot. `Shape.` and `use Std.Collections.` are
//! both a path with something under it, and so is a type with its constructors
//! -- so one index answers all of them, and what differs is only which kinds of
//! name belong where the cursor is: modules and types after a `use`, values and
//! constructors in an expression.

use crate::analysis::{Analysis, Candidate, PathKind};
use meadow_compiler::infer::{PipeFit, Piped, Type, pipes_into};
use std::collections::HashMap;

/// What the cursor is asking for.
#[derive(Debug, PartialEq)]
pub enum Ask {
    /// After `expr |>`: `pipe_at` is where the piped value ends, and `word` is
    /// what has been typed of the function's name.
    Piped { pipe_at: usize, word: String },
    /// After `Q.` in an expression: `Q` is a module brought in with `as`, or a
    /// type, and what may follow is what sits under it.
    Member { qualifier: String, word: String },
    /// The path of a `use`, with the segments already written.
    UsePath { segments: Vec<String>, word: String },
    /// Inside the `( … )` of a `use`: what that module has to offer.
    UseNames { segments: Vec<String>, word: String },
    /// Somewhere no existing name fits: an alias being invented after `as`, or
    /// a field of a record, which is not a path and is not offered here.
    Nothing { word: String },
    /// An ordinary name, with the word typed so far.
    Name { word: String },
}

impl Ask {
    pub fn word(&self) -> &str {
        match self {
            Ask::Piped { word, .. }
            | Ask::Name { word }
            | Ask::Member { word, .. }
            | Ask::UsePath { word, .. }
            | Ask::UseNames { word, .. }
            | Ask::Nothing { word } => word,
        }
    }
}

/// Where the word being completed starts.
///
/// What the editor replaces when an offer is taken, and what it filters the
/// list against as more is typed. Left to the client's own guess, a list whose
/// entries do not start where it thinks the word does simply empties as
/// someone types.
pub fn word_start(text: &str, offset: usize) -> usize {
    let head = &text[..offset.min(text.len())];
    head.rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// One thing to offer.
pub struct Offer {
    /// The name, as it goes in the document.
    pub name: String,
    /// What to insert: the name, or a call with holes for the arguments that
    /// come before the piped value.
    pub insert: String,
    /// Whether `insert` is a snippet, with `${1:…}` holes for the editor.
    pub snippet: bool,
    /// The type, for the line beside the name.
    pub detail: String,
    /// Where it came from.
    pub from: String,
    /// What the editor should sort on: the rank, so its own alphabetical
    /// ordering does not undo this one.
    pub sort: String,
    /// What this is, for the editor's icon.
    pub kind: PathKind,
}

/// What `text[..offset]` is asking for.
///
/// A pipe counts only when nothing but a name is between it and the cursor --
/// `x |> ma` is still choosing a function, `x |> map f` is no longer.
pub fn ask(text: &str, offset: usize) -> Ask {
    let head = &text[..offset.min(text.len())];
    let word_start = word_start(text, offset);
    let word = head[word_start..].to_string();
    let before = &head[..word_start];
    // A `use` is a line of its own, and every part of it wants names of its
    // own kind, so it is decided before anything else.
    if let Some(rest) = used(line_of(before)) {
        return use_ask(rest, word);
    }
    // `Q.` -- a path. Case says which kind: a capital is a module or a type, and
    // anything else is a record field, which is a question about a value's type
    // rather than about a path.
    if let Some(upto) = before.strip_suffix('.') {
        let qualifier = &upto[word_start_of(upto)..];
        return match qualifier.chars().next() {
            Some(c) if c.is_uppercase() => Ask::Member {
                qualifier: qualifier.to_string(),
                word,
            },
            _ => Ask::Nothing { word },
        };
    }
    let before = before.trim_end();
    if let Some(rest) = before.strip_suffix("|>") {
        return Ask::Piped {
            // Where the value ends, so a typed node can be found by its end.
            pipe_at: rest.trim_end().len(),
            word,
        };
    }
    Ask::Name { word }
}

/// The line `text` ends on.
fn line_of(text: &str) -> &str {
    &text[text.rfind('\n').map_or(0, |i| i + 1)..]
}

fn word_start_of(text: &str) -> usize {
    word_start(text, text.len())
}

/// What follows `use` on this line, if it is a `use` at all.
fn used(line: &str) -> Option<&str> {
    // `@pub use …` is a `use` like any other.
    let line = line.trim_start();
    let line = match line.strip_prefix('@') {
        Some(rest) => rest.split_once(char::is_whitespace).map_or("", |(_, r)| r),
        None => line,
    };
    line.trim_start()
        .strip_prefix("use")
        .filter(|rest| rest.starts_with(char::is_whitespace))
}

/// Which part of a `use` the cursor is in.
fn use_ask(rest: &str, word: String) -> Ask {
    // `use M (a, b` -- naming what to take, rather than where from.
    if let Some(open) = rest.rfind('(')
        && !rest[open..].contains(')')
    {
        return Ask::UseNames {
            segments: path_of(&rest[..open]),
            word,
        };
    }
    // `use M as ` -- a name is being invented, and nothing existing would do.
    if rest.split_whitespace().any(|w| w == "as") {
        return Ask::Nothing { word };
    }
    Ask::UsePath {
        segments: finished(rest),
        word,
    }
}

/// The segments of a path that is finished: `Std.Collections` is two.
fn path_of(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    text.split('.').map(|s| s.to_string()).collect()
}

/// The segments of a path still being written, which are the ones a dot has
/// already closed: `Std.Coll` is one, because `Coll` is the word being
/// completed rather than a segment.
fn finished(text: &str) -> Vec<String> {
    match text.trim().strip_suffix('.') {
        Some(done) => path_of(done),
        None => Vec::new(),
    }
}

/// The name a repair puts where the function will go.
///
/// Long and unlikely on purpose: it is resolved like anything else, and its
/// only job is to be a syntactically complete expression that nothing else in
/// the document is called.
pub const HOLE: &str = "meadowCompletionHole";

/// `text` with the half-written pipe finished, so that it parses.
///
/// What has been typed of the function's name goes too: `xs |> fol` asks the
/// same question as `xs |> `, and a half-written name is one more thing that
/// does not resolve.
pub fn repaired(text: &str, start: usize, end: usize) -> String {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    let mut out = String::with_capacity(text.len() + HOLE.len() + 1);
    out.push_str(&text[..start]);
    out.push_str(HOLE);
    out.push_str(&text[end..]);
    out
}

/// `text` with the line the cursor is on emptied.
///
/// A half-written `use` does not parse -- `use Std.` is a path with nothing
/// after the dot -- and one line that does not parse takes the whole document's
/// types with it, including the very declarations the path is asking about.
/// Emptying the line leaves the rest of the document to say what is there; the
/// line itself stays, so nothing below it moves to another line.
pub fn without_line(text: &str, at: usize) -> String {
    let at = at.min(text.len());
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    format!("{}{}", &text[..start], &text[end..])
}

/// The type of the value being piped: the outermost typed node that ends where
/// the `|>` begins.
///
/// Outermost twice over. The longest span wins, because at `f x |> ` the value
/// is the whole application where a position lookup would want the innermost
/// node. And where several nodes share one span -- `[11..20]` is written once
/// but becomes a call, the function called, and its arguments, all spanning
/// the literal -- the first recorded wins, which is the one the walk reached
/// first and so the outermost of them. Taking the last instead offers what can
/// be done with an `Int` to someone who wrote a vector.
pub fn subject(a: &Analysis, pipe_at: usize) -> Option<&Type> {
    let mut best: Option<&(meadow_compiler::span::Span, Type)> = None;
    for node in a
        .typed_nodes
        .iter()
        .filter(|(s, _)| s.end as usize == pipe_at)
    {
        let longer = match best {
            Some((s, _)) => node.0.end - node.0.start > s.end - s.start,
            None => true,
        };
        if longer {
            best = Some(node);
        }
    }
    best.map(|(_, t)| t)
}

/// Everything `subject` can be piped into, best first.
pub fn piped(a: &Analysis, subject: &Type, word: &str) -> Vec<Offer> {
    let used = usage(&a.source);
    let mut out: Vec<(Rank, Offer)> = Vec::new();
    for c in &a.candidates {
        if !c.name.starts_with(word) {
            continue;
        }
        let Some(fit) = pipes_into(&c.scheme, subject) else {
            continue;
        };
        out.push((rank(c, &fit, &used), offer(c, &fit)));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    out.into_iter()
        .enumerate()
        .map(|(i, (_, mut o))| {
            // The editor sorts on this, so it has to be ordered as text.
            o.sort = format!("{i:04}");
            o
        })
        .collect()
}

/// Names in scope, for a cursor that is not after a pipe.
pub fn names(a: &Analysis, word: &str) -> Vec<Offer> {
    let used = usage(&a.source);
    let mut out: Vec<(Rank, Offer)> = a
        .candidates
        .iter()
        .filter(|c| c.name.starts_with(word))
        .map(|c| {
            (
                Rank {
                    generic: false,
                    effectful: false,
                    unused: -used.get(&c.name).copied().unwrap_or(0),
                    uncommon: -(c.common as i32),
                    distance: c.distance,
                    holes: 0,
                    tier: 0,
                },
                Offer {
                    name: c.name.clone(),
                    insert: c.name.clone(),
                    snippet: false,
                    detail: c.scheme.to_string(),
                    from: c.from.clone(),
                    sort: String::new(),
                    kind: PathKind::Value,
                },
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    out.into_iter()
        .enumerate()
        .map(|(i, (_, mut o))| {
            o.sort = format!("{i:04}");
            o
        })
        .collect()
}

/// The names under a `use`'s path: the modules that continue it, and the
/// types it can reach into for their constructors.
pub fn use_path(a: &Analysis, segments: &[String], word: &str) -> Vec<Offer> {
    under(a, segments, &[PathKind::Module, PathKind::Type], word)
}

/// What a `use M (…)` can name: the module's values, and its types.
pub fn use_names(a: &Analysis, segments: &[String], word: &str) -> Vec<Offer> {
    under(a, segments, &[PathKind::Value, PathKind::Type], word)
}

/// What can follow `Q.` in an expression.
///
/// Two things can be written there and they are found in different ways. A
/// module is only ever reached through an alias -- `use … as Q` -- so the
/// document says which module `Q` is. A type is written bare wherever it was
/// declared, so `Maybe.` has to be matched against the tail of every path.
pub fn member(a: &Analysis, qualifier: &str, word: &str) -> Vec<Offer> {
    if let Some(written) = aliases(&a.source).get(qualifier) {
        let at: Vec<String> = written.split('.').map(|s| s.to_string()).collect();
        let Some(path) = locate(a, &at) else {
            return Vec::new();
        };
        let mut out = entries(a, &path, &[PathKind::Value], word);
        // `Q.Ctor` reaches the constructors of the module's own types too, and
        // those sit under the type rather than under the module.
        for e in a.paths.under(&path) {
            if e.kind == PathKind::Type {
                let ty = format!("{path}.{}", e.name);
                out.extend(entries(a, &ty, &[PathKind::Ctor], word));
            }
        }
        return numbered(sorted(out));
    }
    // A type, then: the first path ending in this name with constructors under
    // it. `Maybe` is a module *and* the type inside it, and only the type is
    // something a constructor can follow.
    let owner = a
        .paths
        .ending_in(qualifier)
        .into_iter()
        .find(|p| a.paths.under(p).iter().any(|e| e.kind == PathKind::Ctor));
    match owner {
        Some(path) => numbered(sorted(entries(a, path, &[PathKind::Ctor], word))),
        None => Vec::new(),
    }
}

/// The names of the given kinds directly under a written path.
fn under(a: &Analysis, segments: &[String], kinds: &[PathKind], word: &str) -> Vec<Offer> {
    match locate(a, segments) {
        Some(path) => numbered(sorted(entries(a, &path, kinds, word))),
        None => Vec::new(),
    }
}

/// Where a written path sits in the index.
///
/// What is written is often not the whole of it: a `use` inside a package may
/// leave the package's own name off, and a type is always named bare. So an
/// exact path wins, and otherwise the shortest one ending the same way does.
fn locate(a: &Analysis, segments: &[String]) -> Option<String> {
    let written = segments.join(".");
    if written.is_empty() || !a.paths.under(&written).is_empty() {
        return Some(written);
    }
    a.paths.ending_in(&written).first().map(|p| p.to_string())
}

fn entries(a: &Analysis, path: &str, kinds: &[PathKind], word: &str) -> Vec<Offer> {
    let mut out: Vec<Offer> = Vec::new();
    for e in a.paths.under(path) {
        if !kinds.contains(&e.kind) || !e.name.starts_with(word) {
            continue;
        }
        // One name is one entry, whatever it also happens to be.
        if out.iter().any(|o| o.name == e.name) {
            continue;
        }
        out.push(Offer {
            name: e.name.clone(),
            insert: e.name.clone(),
            snippet: false,
            detail: if e.detail.is_empty() {
                describe(e.kind).to_string()
            } else {
                e.detail.clone()
            },
            from: if path.is_empty() {
                "this document".to_string()
            } else {
                path.to_string()
            },
            sort: String::new(),
            kind: e.kind,
        });
    }
    out
}

/// The module aliases a document sets up: `use Std.Maybe as M` maps `M` to
/// `Std.Maybe`.
///
/// Read from the text rather than from the tree, because completion runs on a
/// document in the middle of being edited, and the tree is the first thing an
/// edit takes away.
pub fn aliases(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut words = line.split_whitespace().peekable();
        if words.peek().is_some_and(|w| w.starts_with('@')) {
            words.next();
        }
        if words.next() != Some("use") {
            continue;
        }
        if let (Some(path), Some("as"), Some(alias)) = (words.next(), words.next(), words.next()) {
            out.insert(alias.to_string(), path.to_string());
        }
    }
    out
}

/// What to say about a name that has no type to show.
fn describe(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Module => "module",
        PathKind::Type => "type",
        PathKind::Ctor => "constructor",
        PathKind::Value => "value",
    }
}

/// Alphabetical: nothing about a path says which of its names matters more,
/// and an order that is not obvious is worse than one that is.
fn sorted(mut offers: Vec<Offer>) -> Vec<Offer> {
    offers.sort_by(|x, y| x.name.cmp(&y.name));
    offers
}

/// Fix the order the editor will show these in.
fn numbered(offers: Vec<Offer>) -> Vec<Offer> {
    offers
        .into_iter()
        .enumerate()
        .map(|(i, mut o)| {
            o.sort = format!("{i:04}");
            o
        })
        .collect()
}

/// How good an offer is, smallest first.
///
/// A parameter of this very type comes before one that is a type variable and
/// so fits everything; something that performs no effect before something that
/// does, since a pipeline is usually a chain of plain transformations; then
/// what this file already uses, what is near, and how much is left to write.
///
/// Whether the value goes in first or last is the *last* thing considered.
/// Both are ordinary here -- `len v` takes the collection first and `map f v`
/// takes it last -- so ranking by that would bury `map`, `filter` and `foldl`
/// under every function that happens to take its argument the other way round.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    generic: bool,
    effectful: bool,
    /// Negated, so that *more* uses sorts first.
    unused: i32,
    /// The same, for the standard library's own source: what idiomatic Meadow
    /// reaches for, when this file has said nothing either way.
    uncommon: i32,
    distance: u8,
    holes: usize,
    tier: u8,
}

fn rank(c: &Candidate, fit: &PipeFit, used: &HashMap<String, i32>) -> Rank {
    let (tier, holes) = match fit.how {
        Piped::First => (0, 0),
        Piped::Last(before) => (1, before),
    };
    Rank {
        generic: fit.generic,
        effectful: fit.effectful,
        unused: -used.get(&c.name).copied().unwrap_or(0),
        uncommon: -(c.common as i32),
        distance: c.distance,
        holes,
        tier,
    }
}

/// How often each name is already written in `text`.
///
/// What a file reaches for, it reaches for again -- the cheapest measure of
/// "commonly used" that knows anything about *this* project, and one that
/// needs no list to be kept up to date.
fn usage(text: &str) -> HashMap<String, i32> {
    let mut out: HashMap<String, i32> = HashMap::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else if !word.is_empty() {
            *out.entry(std::mem::take(&mut word)).or_default() += 1;
        }
    }
    if !word.is_empty() {
        *out.entry(word).or_default() += 1;
    }
    out
}

fn offer(c: &Candidate, fit: &PipeFit) -> Offer {
    let (insert, snippet) = match fit.how {
        Piped::First => (c.name.clone(), false),
        // `setWidth ${1:…}`: the arguments that come before the piped value
        // are what is left to write, so the cursor lands in the first.
        Piped::Last(before) => {
            let holes: Vec<String> = (1..=before).map(|i| format!("${{{i}}}")).collect();
            (format!("{} {}", c.name, holes.join(" ")), true)
        }
    };
    Offer {
        name: c.name.clone(),
        insert,
        snippet,
        detail: c.scheme.to_string(),
        from: c.from.clone(),
        sort: String::new(),
        kind: PathKind::Value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_is_what_the_cursor_is_after() {
        assert_eq!(
            ask("def main = x |> ", 16),
            Ask::Piped {
                pipe_at: 12,
                word: String::new()
            }
        );
        assert_eq!(
            ask("def main = x |> ma", 18),
            Ask::Piped {
                pipe_at: 12,
                word: "ma".to_string()
            }
        );
    }

    #[test]
    fn a_pipe_with_a_call_after_it_is_no_longer_choosing_one() {
        // The function is already named, so this is an ordinary name position.
        assert_eq!(
            ask("def main = x |> map f", 21),
            Ask::Name {
                word: "f".to_string()
            }
        );
    }

    #[test]
    fn a_name_on_its_own_is_a_name() {
        assert_eq!(
            ask("def main = leng", 15),
            Ask::Name {
                word: "leng".to_string()
            }
        );
    }

    #[test]
    fn a_repair_finishes_the_pipe() {
        let text = "def main = x |> ";
        assert_eq!(repaired(text, 16, 16), format!("def main = x |> {HOLE}"));
    }

    #[test]
    fn a_repair_takes_the_half_written_name_with_it() {
        let text = "def main = x |> fol";
        assert_eq!(repaired(text, 16, 19), format!("def main = x |> {HOLE}"));
    }

    #[test]
    fn a_dot_after_a_capital_asks_what_is_under_it() {
        assert_eq!(
            ask("def main = Shape.", 17),
            Ask::Member {
                qualifier: "Shape".to_string(),
                word: String::new()
            }
        );
        assert_eq!(
            ask("def main = Shape.Ci", 19),
            Ask::Member {
                qualifier: "Shape".to_string(),
                word: "Ci".to_string()
            }
        );
    }

    #[test]
    fn a_dot_after_a_value_is_a_field_which_is_not_a_path() {
        // `p.x` selects a field: a question about the record's type, not about
        // what is under a name. Offering every name in scope there would only
        // offer things that do not compile.
        assert_eq!(
            ask("def main = p.", 13),
            Ask::Nothing {
                word: String::new()
            }
        );
        // A number being written is not a path either.
        assert_eq!(
            ask("def main = 1.", 13),
            Ask::Nothing {
                word: String::new()
            }
        );
    }

    #[test]
    fn a_use_asks_for_a_module_path() {
        assert_eq!(
            ask("use ", 4),
            Ask::UsePath {
                segments: Vec::new(),
                word: String::new()
            }
        );
        assert_eq!(
            ask("use Std.", 8),
            Ask::UsePath {
                segments: vec!["Std".to_string()],
                word: String::new()
            }
        );
        // The last segment is the word being completed, not a segment yet.
        assert_eq!(
            ask("use Std.Coll", 12),
            Ask::UsePath {
                segments: vec!["Std".to_string()],
                word: "Coll".to_string()
            }
        );
        assert_eq!(
            ask("@pub use Std.", 13),
            Ask::UsePath {
                segments: vec!["Std".to_string()],
                word: String::new()
            }
        );
    }

    #[test]
    fn the_list_of_a_use_asks_for_what_the_module_has() {
        assert_eq!(
            ask("use Std.Maybe (ma", 17),
            Ask::UseNames {
                segments: vec!["Std".to_string(), "Maybe".to_string()],
                word: "ma".to_string()
            }
        );
        // Closed again: the `use` is finished, and this is its path once more.
        assert!(matches!(
            ask("use Std.Maybe (map) ", 20),
            Ask::UsePath { .. }
        ));
    }

    #[test]
    fn an_alias_is_a_name_being_invented() {
        assert_eq!(
            ask("use Std.Maybe as M", 18),
            Ask::Nothing {
                word: "M".to_string()
            }
        );
    }

    #[test]
    fn a_path_does_not_swallow_the_pipe() {
        assert_eq!(
            ask("def main = xs |> ma", 19),
            Ask::Piped {
                pipe_at: 13,
                word: "ma".to_string()
            }
        );
    }

    #[test]
    fn a_line_is_emptied_where_it_stands() {
        let text = "use Std.\n\ndef main = 1";
        // The text goes, the line stays: what is below it is still where it was.
        assert_eq!(without_line(text, 8), "\n\ndef main = 1");
    }
}
