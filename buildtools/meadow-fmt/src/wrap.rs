//! **Breaking a line that is too long** -- the part of `meadow fmt` that keeps
//! source within a width.
//!
//! The indenter never moves a token to another line. This does, and only
//! this: a line longer than the width is cut between two of its tokens, at
//! the place the kind of line it is suggests, and what was cut off goes on
//! lines of its own. Nothing is ever joined, reordered or rewritten, and a
//! comment or a string is never cut. What comes out is handed to the
//! indenter, which places every line that has a structural place -- an arm,
//! a `then`, a block's body -- and leaves the rest where this put them.
//!
//! Cutting a line cannot change what a program means, with one exception the
//! lexer makes: a macro call that starts in column 0 is a declaration. So a
//! piece cut off never starts in column 0.
//!
//! ## Where a line is cut
//!
//! The first of these that applies to the line, looking only at what is not
//! inside brackets; then each piece is looked at again:
//!
//! | the line                                   | is cut                         |
//! |--------------------------------------------|--------------------------------|
//! | a `fun` or `def` with its body             | after the `=`                  |
//! | a `let … in` with what follows             | after each `in`                |
//! | a `match … with` and its arms              | before each `\|`               |
//! | an arm and its body                        | after the `->`                 |
//! | an `if … then … else`                      | before `then` and `else`       |
//! | a pipeline                                 | before each `\|>`              |
//! | a bracketed list with commas               | an item to a line              |
//! | a chain of `++`, `and` or `or`             | before each                    |
//! | a function and its arguments               | an argument to a line          |
//! | one bracketed expression                   | inside the brackets, as above  |
//!
//! A line none of these fits -- a long string, a long comment, one long name
//! -- is left as it is.

use super::{Nest, UNIT, closes_raw, is_word, opens_raw};

/// What a stretch of a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Op,
    Open,
    Close,
    /// A string or a character, holes and all.
    Literal,
    Comma,
    Other,
}

/// A token: where it starts and ends in the line, in characters.
#[derive(Debug, Clone, Copy)]
struct Tok {
    start: usize,
    end: usize,
    kind: Kind,
}

/// Words that are not names: an argument does not start with one.
const KEYWORDS: &[&str] = &[
    "let", "in", "if", "then", "else", "match", "with", "fun", "def", "data", "record", "effect",
    "handle", "type", "trait", "impl", "where", "use", "mod", "and", "or", "as", "macro",
];

/// `src` with every line longer than `width` characters cut into lines that
/// fit, where there is a place to cut it.
pub fn wrap(src: &str, width: usize) -> String {
    let mut nest: Vec<Nest> = Vec::new();
    let mut out = String::with_capacity(src.len() + src.len() / 16);
    for line in src.split_inclusive('\n') {
        let (text, ending) = match line.strip_suffix('\n') {
            Some(t) => match t.strip_suffix('\r') {
                Some(t) => (t, "\r\n"),
                None => (t, "\n"),
            },
            None => (line, ""),
        };
        let chars: Vec<char> = text.chars().collect();
        // A line that starts inside a string is that string's, and is left.
        let inside = !nest.is_empty();
        let (toks, comment) = scan(&chars, &mut nest);
        if !inside
            && chars.len() > width
            && toks.is_empty()
            && let Some(ruled) = shorter_rule(&chars, width)
        {
            out.push_str(&ruled);
            out.push_str(ending);
            continue;
        }
        if inside || chars.len() <= width || toks.is_empty() {
            out.push_str(text);
            out.push_str(ending);
            continue;
        }
        let indent = chars.iter().take_while(|c| **c == ' ').count();
        let mut lines = Vec::new();
        let code_end = toks.last().expect("not empty").end;
        if let Some(at) = comment {
            // A comment after the code goes on the line before it, where it
            // still says what it says about the same thing.
            let note: String = chars[at..].iter().collect();
            lines.push(format!("{}{}", " ".repeat(indent), note.trim_end()));
            if indent + (code_end - toks[0].start) <= width {
                lines.push(piece_text(&chars, &toks, indent));
            } else {
                wrap_piece(&chars, &toks, indent, width, &mut lines);
            }
        } else {
            wrap_piece(&chars, &toks, indent, width, &mut lines);
        }
        let ending = if ending.is_empty() { "\n" } else { ending };
        let last = lines.len() - 1;
        for (i, l) in lines.iter().enumerate() {
            out.push_str(l);
            if i < last || !line.ends_with('\n') {
                out.push_str(if i < last { ending } else { "" });
            } else {
                out.push_str(ending);
            }
        }
    }
    out
}

/// A comment that rules a line across the page -- `-- --- name ----------` --
/// with as many of its dashes as fit in `width`; `None` for any other.
fn shorter_rule(chars: &[char], width: usize) -> Option<String> {
    let dashes = chars.iter().rev().take_while(|c| **c == '-').count();
    let over = chars.len() - width;
    // Still a rule after: a few dashes are left.
    (dashes >= over + 3).then(|| chars[..width].iter().collect())
}

/// The tokens of a line of code, and where its comment starts if it has one.
/// `nest` is what string the line begins inside, and is left as what the next
/// begins inside.
fn scan(chars: &[char], nest: &mut Vec<Nest>) -> (Vec<Tok>, Option<usize>) {
    const OP: &str = "!$%&*+./<=>?@|^~:-";
    let mut toks = Vec::new();
    let mut i = 0;
    let mut literal = if nest.is_empty() { None } else { Some(0) };
    while i < chars.len() {
        let c = chars[i];
        if let Some(&top) = nest.last() {
            match (top, c) {
                (Nest::Raw(hashes), '"') if closes_raw(chars, i, hashes) => {
                    nest.pop();
                    i += 1 + hashes;
                }
                (Nest::Raw(_), _) => i += 1,
                (Nest::Hole(_), 'r') if opens_raw(chars, i).is_some() => {
                    let hashes = opens_raw(chars, i).expect("just checked");
                    nest.push(Nest::Raw(hashes));
                    i += 2 + hashes;
                }
                (Nest::Text, '\\') => i += 2,
                (Nest::Text, '"') => {
                    nest.pop();
                    i += 1;
                }
                (Nest::Text, '$') if chars.get(i + 1) == Some(&'{') => {
                    nest.push(Nest::Hole(0));
                    i += 2;
                }
                (Nest::Hole(_), '"') => {
                    nest.push(Nest::Text);
                    i += 1;
                }
                (Nest::Hole(depth), '{') => {
                    *nest.last_mut().expect("in a hole") = Nest::Hole(depth + 1);
                    i += 1;
                }
                (Nest::Hole(0), '}') => {
                    nest.pop();
                    i += 1;
                }
                (Nest::Hole(depth), '}') => {
                    *nest.last_mut().expect("in a hole") = Nest::Hole(depth - 1);
                    i += 1;
                }
                _ => i += 1,
            }
            if nest.is_empty() {
                let start = literal.take().expect("a literal was open");
                toks.push(Tok {
                    start,
                    end: i.min(chars.len()),
                    kind: Kind::Literal,
                });
            }
            continue;
        }
        let start = i;
        let kind = match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '-' if chars.get(i + 1) == Some(&'-') => return (toks, Some(i)),
            '"' => {
                nest.push(Nest::Text);
                literal = Some(i);
                i += 1;
                continue;
            }
            'r' if opens_raw(chars, i).is_some() => {
                let hashes = opens_raw(chars, i).expect("just checked");
                nest.push(Nest::Raw(hashes));
                literal = Some(i);
                i += 2 + hashes;
                continue;
            }
            '\'' => {
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    i += 1;
                    if c == '\\' {
                        i += 1;
                    } else if c == '\'' {
                        break;
                    }
                }
                i = i.min(chars.len());
                Kind::Literal
            }
            c if is_word(c) => {
                // A path is one word: `V.foldl`, `i.wanted`, `1.5`.
                while i < chars.len()
                    && (is_word(chars[i])
                        || (chars[i] == '.'
                            && chars.get(i + 1).is_some_and(|c| is_word(*c))
                            && chars.get(i + 1) != Some(&'.')))
                {
                    i += 1;
                }
                Kind::Word
            }
            '#' if chars.get(i + 1) == Some(&'[') => {
                i += 2;
                Kind::Open
            }
            '(' | '[' | '{' => {
                i += 1;
                Kind::Open
            }
            ')' | ']' | '}' => {
                i += 1;
                Kind::Close
            }
            ',' | ';' => {
                i += 1;
                Kind::Comma
            }
            c if OP.contains(c) => {
                while i < chars.len()
                    && OP.contains(chars[i])
                    && !(chars[i] == '-' && chars.get(i + 1) == Some(&'-'))
                {
                    i += 1;
                }
                Kind::Op
            }
            _ => {
                i += 1;
                Kind::Other
            }
        };
        toks.push(Tok {
            start,
            end: i,
            kind,
        });
    }
    // A string still open at the end of the line is the rest of the line.
    if let Some(start) = literal {
        toks.push(Tok {
            start,
            end: chars.len(),
            kind: Kind::Literal,
        });
    }
    (toks, None)
}

fn text_of(chars: &[char], t: &Tok) -> String {
    chars[t.start..t.end].iter().collect()
}

fn is(chars: &[char], t: &Tok, kind: Kind, text: &str) -> bool {
    t.kind == kind && chars[t.start..t.end].iter().copied().eq(text.chars())
}

/// The tokens `toks` as a line, indented by `indent`.
fn piece_text(chars: &[char], toks: &[Tok], indent: usize) -> String {
    let body: String = chars[toks[0].start..toks[toks.len() - 1].end]
        .iter()
        .collect();
    format!("{}{}", " ".repeat(indent), body)
}

/// How deep in brackets each token is, counting from where the piece starts:
/// a bracket and the one that closes it are as deep as each other.
fn depths(toks: &[Tok]) -> Vec<usize> {
    let mut depth = 0usize;
    toks.iter()
        .map(|t| match t.kind {
            Kind::Open => {
                depth += 1;
                depth - 1
            }
            Kind::Close => {
                depth = depth.saturating_sub(1);
                depth
            }
            _ => depth,
        })
        .collect()
}

/// `toks`, cut at each of `cuts` -- the index a new line starts at -- with
/// the first piece at `indent` and the rest at `rest`; each then wrapped.
fn cut(
    chars: &[char],
    toks: &[Tok],
    cuts: &[usize],
    indent: usize,
    rest: usize,
    width: usize,
    out: &mut Vec<String>,
) {
    let mut from = 0;
    for (n, &at) in cuts.iter().chain(std::iter::once(&toks.len())).enumerate() {
        if at > from {
            // Never column 0 for what was cut off: see the module's note.
            let at_indent = if n == 0 { indent } else { rest.max(UNIT) };
            wrap_piece(chars, &toks[from..at], at_indent, width, out);
        }
        from = at;
    }
}

fn wrap_piece(chars: &[char], toks: &[Tok], indent: usize, width: usize, out: &mut Vec<String>) {
    let len = toks[toks.len() - 1].end - toks[0].start;
    if indent + len <= width || toks.len() < 2 {
        out.push(piece_text(chars, toks, indent));
        return;
    }
    let depth = depths(toks);
    let n = toks.len();
    let top = |i: usize| depth[i] == 0;
    let word = |i: usize, w: &str| is(chars, &toks[i], Kind::Word, w);
    let op = |i: usize, o: &str| is(chars, &toks[i], Kind::Op, o);
    let find = |from: usize, f: &dyn Fn(usize) -> bool| (from..n).find(|&i| top(i) && f(i));

    // A bracket opened here and closed on a later line: what follows it is
    // looked at by itself, as far in as the bracket puts it.
    if toks[0].kind == Kind::Open && (1..n).all(|i| depth[i] >= 1) {
        let opener = toks[0].end - toks[0].start;
        let mut inner = Vec::new();
        // Measured one bracket in, and laid out from where the line starts:
        // what is cut off hangs a unit under the line, whatever opened on it.
        wrap_piece(
            chars,
            &toks[1..],
            indent,
            width.saturating_sub(opener),
            &mut inner,
        );
        if inner.len() > 1 {
            let open = text_of(chars, &toks[0]);
            for (i, l) in inner.into_iter().enumerate() {
                out.push(if i == 0 {
                    format!("{}{}{}", " ".repeat(indent), open, l.trim_start())
                } else {
                    l
                });
            }
            return;
        }
    }
    // A variant and its fields: a field to a line, under the constructor.
    if (op(0, "=") || op(0, "|")) && n > 2 && find(1, &|i| op(i, "->") || op(i, "=")).is_none() {
        let fields: Vec<usize> = (2..n)
            .filter(|&i| {
                top(i)
                    && matches!(toks[i].kind, Kind::Word | Kind::Open)
                    && matches!(toks[i - 1].kind, Kind::Word | Kind::Close)
            })
            .collect();
        let plain = (1..n).all(|i| !top(i) || !matches!(toks[i].kind, Kind::Op | Kind::Comma));
        if plain && !fields.is_empty() {
            return cut(chars, toks, &fields, indent, indent + 2 * UNIT, width, out);
        }
    }
    // A definition and its body.
    let declares = word(0, "fun")
        || word(0, "def")
        || (toks[0].kind == Kind::Op && text_of(chars, &toks[0]).starts_with('@'));
    if declares
        && let Some(e) = find(1, &|i| op(i, "="))
        && e + 1 < n
    {
        return cut(chars, toks, &[e + 1], indent, indent + UNIT, width, out);
    }
    // A definition's head alone: as many parameters to a line as fit, the
    // lines after the first further in than its body will be.
    if declares && op(n - 1, "=") {
        let starts: Vec<usize> = (2..n)
            // Before a parameter or the result's `:` -- and never between a
            // `!` and the effects it says.
            .filter(|&i| top(i) && ((toks[i].kind == Kind::Open && !op(i - 1, "!")) || op(i, ":")))
            .collect();
        let mut cuts = Vec::new();
        let mut from = toks[0].start;
        let mut at_indent = indent;
        for (k, &i) in starts.iter().enumerate() {
            let until = starts
                .get(k + 1)
                .map_or(toks[n - 1].end, |&j| toks[j - 1].end);
            if at_indent + (until - from) > width && toks[i].start > from {
                cuts.push(i);
                from = toks[i].start;
                at_indent = indent + 2 * UNIT;
            }
        }
        if !cuts.is_empty() {
            let mut last = 0;
            for (k, &at) in cuts.iter().chain(std::iter::once(&n)).enumerate() {
                let at_indent = if k == 0 { indent } else { indent + 2 * UNIT };
                out.push(piece_text(chars, &toks[last..at], at_indent));
                last = at;
            }
            return;
        }
    }
    // A `let … in` and what follows it.
    if word(0, "let") {
        let cuts: Vec<usize> = (1..n - 1)
            .filter(|&i| top(i) && word(i, "in"))
            .map(|i| i + 1)
            .collect();
        if !cuts.is_empty() {
            return cut(chars, toks, &cuts, indent, indent, width, out);
        }
    }
    // A `let` whose right-hand side is what is long: it goes under the `=`,
    // and an `in` that ended the line ends its last line.
    if word(0, "let")
        && let Some(e) = find(1, &|i| op(i, "="))
        && e + 1 < n
    {
        let ends_in = word(n - 1, "in") && top(n - 1);
        let body = if ends_in {
            &toks[e + 1..n - 1]
        } else {
            &toks[e + 1..]
        };
        if !body.is_empty() {
            out.push(piece_text(chars, &toks[..=e], indent));
            let before = out.len();
            wrap_piece(chars, body, indent + UNIT, width, out);
            // The `in` that ended the line closes the `let` on a line of its
            // own, under it, which is where the indenter looks for it.
            if ends_in && out.len() > before {
                out.push(format!("{}in", " ".repeat(indent)));
            }
            return;
        }
    }
    // A `match … with` and its arms.
    if let Some(w) = find(0, &|i| word(i, "with")) {
        let cuts: Vec<usize> = (w + 1..n).filter(|&i| top(i) && op(i, "|")).collect();
        if !cuts.is_empty() {
            return cut(chars, toks, &cuts, indent, indent, width, out);
        }
    }
    // An arm and its body.
    if op(0, "|")
        && let Some(a) = find(1, &|i| op(i, "->"))
        && a + 1 < n
    {
        return cut(chars, toks, &[a + 1], indent, indent + 2 * UNIT, width, out);
    }
    // An `if` and its branches.
    if let Some(i0) = find(0, &|i| word(i, "if")) {
        let cuts: Vec<usize> = (i0 + 1..n)
            .filter(|&i| top(i) && (word(i, "then") || word(i, "else")))
            .collect();
        if !cuts.is_empty() {
            return cut(chars, toks, &cuts, indent, indent, width, out);
        }
    }
    // A branch that is too long by itself: its body under its keyword.
    if (word(0, "then") || word(0, "else")) && n > 1 {
        return cut(chars, toks, &[1], indent, indent + UNIT, width, out);
    }
    // A function written out: its body under its arrow.
    if is(chars, &toks[0], Kind::Other, "\\")
        && let Some(a) = find(1, &|i| op(i, "->"))
        && a + 1 < n
    {
        return cut(chars, toks, &[a + 1], indent, indent + UNIT, width, out);
    }
    // A pipeline.
    let pipes: Vec<usize> = (1..n).filter(|&i| top(i) && op(i, "|>")).collect();
    if !pipes.is_empty() {
        return cut(chars, toks, &pipes, indent, indent + UNIT, width, out);
    }
    // A chain of `++`, `and` or `or`.
    let joins: Vec<usize> = (1..n)
        .filter(|&i| top(i) && (op(i, "++") || word(i, "and") || word(i, "or")))
        .collect();
    if !joins.is_empty() {
        return cut(chars, toks, &joins, indent, indent + UNIT, width, out);
    }
    // A function and its arguments: nothing at the top but names, literals
    // and bracketed things, one after another.
    let plain = (0..n).all(|i| {
        !top(i)
            || match toks[i].kind {
                Kind::Op | Kind::Comma => false,
                Kind::Word => !KEYWORDS.contains(&text_of(chars, &toks[i]).as_str()),
                _ => true,
            }
    });
    if plain {
        let args: Vec<usize> = (1..n)
            .filter(|&i| {
                top(i)
                    && matches!(toks[i].kind, Kind::Word | Kind::Literal | Kind::Open)
                    && matches!(toks[i - 1].kind, Kind::Word | Kind::Literal | Kind::Close)
            })
            .collect();
        if !args.is_empty() {
            return cut(chars, toks, &args, indent, indent + UNIT, width, out);
        }
    }
    // A bracketed list: the widest one that has commas.
    let mut widest: Option<(usize, usize)> = None;
    for o in 0..n {
        if !(top(o) && toks[o].kind == Kind::Open) {
            continue;
        }
        let Some(c) = (o + 1..n).find(|&i| top(i) && toks[i].kind == Kind::Close) else {
            continue;
        };
        let commas = (o + 1..c).any(|i| depth[i] == 1 && toks[i].kind == Kind::Comma);
        let wider =
            widest.is_none_or(|(a, b)| toks[c].end - toks[o].start > toks[b].end - toks[a].start);
        // One that is most of the line: a pair or a short list in passing is
        // not what is too long about it.
        let long = 2 * (toks[c].end - toks[o].start) > len;
        if commas && c > o + 1 && wider && long {
            widest = Some((o, c));
        }
    }
    if let Some((o, c)) = widest {
        cut(chars, &toks[..=o], &[], indent, indent, width, out);
        let mut from = o + 1;
        for i in o + 1..c {
            if depth[i] == 1 && toks[i].kind == Kind::Comma {
                wrap_piece(chars, &toks[from..=i], indent + UNIT, width, out);
                from = i + 1;
            }
        }
        if from < c {
            wrap_piece(chars, &toks[from..c], indent + UNIT, width, out);
        }
        wrap_piece(chars, &toks[c..], indent, width, out);
        return;
    }
    // One bracketed expression: cut inside it, the brackets staying on its
    // first line and its last.
    if toks[0].kind == Kind::Open
        && toks[n - 1].kind == Kind::Close
        && depth[n - 1] == 0
        && (1..n - 1).all(|i| depth[i] >= 1)
        && n > 2
    {
        let mut inner = Vec::new();
        let opener = toks[0].end - toks[0].start;
        wrap_piece(
            chars,
            &toks[1..n - 1],
            indent,
            width.saturating_sub(opener + 1),
            &mut inner,
        );
        if inner.len() > 1 {
            let open = text_of(chars, &toks[0]);
            let close = text_of(chars, &toks[n - 1]);
            let last = inner.len() - 1;
            for (i, l) in inner.into_iter().enumerate() {
                let l = if i == 0 {
                    format!("{}{}{}", " ".repeat(indent), open, l.trim_start())
                } else {
                    l
                };
                out.push(if i == last { format!("{l}{close}") } else { l });
            }
            return;
        }
    }
    out.push(piece_text(chars, toks, indent));
}

#[cfg(test)]
mod tests {
    fn w(src: &str) -> String {
        crate::format_within(src, 40)
    }

    #[test]
    fn a_line_that_fits_is_left_alone() {
        let src = "fun f x = x + 1\n";
        assert_eq!(w(src), src);
    }

    #[test]
    fn a_definition_is_cut_after_its_equals() {
        assert_eq!(
            w("fun add (a : Int) (b : Int) : Int = a + b + 100\n"),
            "fun add (a : Int) (b : Int) : Int =\n  a + b + 100\n"
        );
    }

    #[test]
    fn a_match_has_an_arm_to_a_line() {
        assert_eq!(
            w("fun f x =\n  match x with | Just y -> y + 1 | None -> 0 - 1\n"),
            "fun f x =\n  match x with\n  | Just y -> y + 1\n  | None -> 0 - 1\n"
        );
    }

    #[test]
    fn a_pipeline_has_a_step_to_a_line() {
        assert_eq!(
            w("fun f = P.pure pair |> P.keep ident |> P.skip eq |> P.keep value\n"),
            "fun f =\n  P.pure pair\n    |> P.keep ident\n    |> P.skip eq\n    |> P.keep value\n"
        );
    }

    #[test]
    fn a_let_and_an_if_are_cut_at_their_keywords() {
        assert_eq!(
            w("fun f x =\n  let a = compute x in let b = other a in combine a b\n"),
            "fun f x =\n  let a = compute x in\n  let b = other a in\n  combine a b\n"
        );
        assert_eq!(
            w("fun g x =\n  if isReady x then proceed x 1 else waitFor x 2\n"),
            "fun g x =\n  if isReady x\n  then proceed x 1\n  else waitFor x 2\n"
        );
    }

    #[test]
    fn a_list_has_an_item_to_a_line_and_arguments_one_each() {
        assert_eq!(
            w("def xs = [alphaAlpha, betaBeta, gammaGamma, deltaDelta]\n"),
            "def xs =\n  [\n    alphaAlpha,\n    betaBeta,\n    gammaGamma,\n    deltaDelta\n  ]\n"
        );
        assert_eq!(
            w("fun t () =\n  assertEq (parse \"null\") (Ok Null) \"it is null\"\n"),
            "fun t () =\n  assertEq\n    (parse \"null\")\n    (Ok Null)\n    \"it is null\"\n"
        );
    }

    #[test]
    fn a_long_right_hand_side_goes_under_its_let_and_in_closes_it() {
        assert_eq!(
            w("fun f x =\n  let total = computeSomething x 1 2 3 4 5 in\n  total\n"),
            "fun f x =\n  let total =\n    computeSomething x 1 2 3 4 5\n  in\n  total\n"
        );
    }

    #[test]
    fn a_long_head_takes_as_many_parameters_to_a_line_as_fit() {
        assert_eq!(
            w("fun compute (first : Int) (second : Int) (third : Int) : Int =\n  first\n"),
            "fun compute (first : Int) (second : Int)\n    (third : Int) : Int =\n  first\n"
        );
    }

    #[test]
    fn a_pair_in_passing_is_not_what_is_cut() {
        assert_eq!(
            w("fun f xs =\n  V.foldl (\\acc x -> step acc x) (0, 1) xs\n"),
            "fun f xs =\n  V.foldl\n    (\\acc x -> step acc x)\n    (0, 1)\n    xs\n"
        );
    }

    #[test]
    fn a_variant_has_a_field_to_a_line_and_stays_in_from_the_margin() {
        assert_eq!(
            w("data Live k\n  = Live (TVar Int) (TVar (HashMap k Int)) (k -> Maybe Int)\n"),
            "data Live k\n  = Live\n      (TVar Int)\n      (TVar (HashMap k Int))\n      (k -> Maybe Int)\n"
        );
    }

    #[test]
    fn an_effect_row_stays_with_its_bang() {
        assert_eq!(
            w("fun newDb (compute : k -> Maybe v) : Db k v ! { Thread | e } =\n  go\n"),
            "fun newDb (compute : k -> Maybe v)\n    : Db k v ! { Thread | e } =\n  go\n"
        );
    }

    #[test]
    fn arguments_hang_a_unit_under_their_line_whatever_opened_on_it() {
        assert_eq!(
            w("fun f o =\n  g (Once (newRef firstThing) (newRef secondThing) (newRef third))\n"),
            "fun f o =\n  g\n    (Once\n      (newRef firstThing)\n      (newRef secondThing)\n      (newRef third))\n"
        );
    }

    #[test]
    fn strings_and_comments_are_never_cut() {
        let long = "def s = \"a string that is a good deal longer than forty characters\"\n";
        assert_eq!(
            w(long),
            "def s =\n  \"a string that is a good deal longer than forty characters\"\n"
        );
        let note = "-- a comment that is a good deal longer than forty characters\n";
        assert_eq!(w(note), note);
        assert_eq!(
            w("fun t () =\n  f x -- a note that makes the line too long to fit\n"),
            "fun t () =\n  -- a note that makes the line too long to fit\n  f x\n"
        );
    }

    #[test]
    fn wrapping_again_changes_nothing() {
        let src = "fun f x = match x with | Just y -> P.pure pair |> P.keep ident |> P.keep value | None -> assertEq (parse \"null\") (Ok Null) \"it is null\"\n";
        let once = w(src);
        assert_eq!(w(&once), once);
        for line in once.lines() {
            assert!(
                line.chars().count() <= 40 || !line.contains(' ') || line.contains('"'),
                "{line}"
            );
        }
    }
}
