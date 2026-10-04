//! **The source formatter** — `meadow fmt`, and the REPL's auto-indent.
//!
//! Two passes. The first is all `meadow fmt` does unless it is given a width:
//! an *indenter*, described here. The second, [`format_within`], cuts lines
//! that are longer than the width and is described in `wrap.rs`; it only ever
//! cuts a line between two tokens, and hands what it made back to the first.
//!
//! This is an *indenter*, not a pretty-printer: it never moves a token to
//! another line and never rewrites an expression. It fixes leading whitespace,
//! trailing whitespace, tabs and blank-line runs, and leaves everything else
//! exactly where the author put it. That is a deliberate limit — it means
//! comments survive untouched (the lexer discards them, so a print-the-AST
//! formatter would delete every doc comment in the standard library) and it
//! keeps the whole thing small enough to trust.
//!
//! ## What decides an indent
//!
//! Meadow's grammar is not layout-sensitive — the lexer skips newlines — so
//! indentation is pure presentation and this pass cannot change what a program
//! means. Structure is tracked with a stack of [`Frame`]s, each opened by a line
//! and each contributing one [`UNIT`] of indent to the lines beneath it:
//!
//! | opened by                              | frame       | lines under it |
//! |----------------------------------------|-------------|----------------|
//! | an unclosed `(` `[` `{`                | `Open`      | one unit in; the closer goes back out |
//! | a line ending in `with`                 | `Arms`      | `|` arms line up *with the `match`* |
//! | a line containing an unresolved `if`    | `Cond`      | a later `then` / `else` lines up with the `if` |
//! | a line ending in `=` `->` `then` `else` | `Block`     | one unit in — two, for a `|` arm's body |
//!
//! A top-level declaration (`fun`, `def`, `data`, `@pub …`) clears the stack, so
//! a mistake can never propagate past the end of a declaration.
//!
//! ## Continuation lines
//!
//! A line that merely continues the expression above it — no closer, no arm, no
//! keyword, and nothing opened on the previous line — has no structural anchor,
//! and this pass keeps whatever indent the author chose (raising it if it sits
//! below the enclosing block). That is what preserves hand-aligned code like
//!
//! ```text
//! bitOr (bitOr (bytesGetOr 0 b i)
//!              (bytesGetOr 0 b (i + 1) << 8))
//!       (bitOr (bytesGetOr 0 b (i + 2) << 16)
//!              (bytesGetOr 0 b (i + 3) << 24))
//! ```
//!
//! which no rule this small could reproduce and which is much worse flattened.

mod wrap;

/// One level of indentation.
pub const UNIT: usize = 2;

/// How many characters a line may be before `meadow fmt` cuts it, unless it
/// is told another width: rustfmt's `max_width`, and the Rust style guide's.
pub const WIDTH: usize = 100;

/// [`format`], with every line longer than `width` characters cut into lines
/// that fit, wherever there is a place to cut it: see [`wrap`].
pub fn format_within(src: &str, width: usize) -> String {
    // Indented first, so that what is measured is what will be written; cut;
    // and indented again, which places the lines the cuts made.
    //
    // Again until nothing moves: a cut can leave a line the indenter then
    // puts further in than it was measured at.
    let mut text = format(src);
    for _ in 0..6 {
        let next = format(&join_braces(
            &join_ins(&wrap::wrap(&text, width), width),
            width,
        ));
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// Format a whole source file.
///
/// Re-indents every line, strips trailing whitespace, collapses runs of blank
/// lines to one, drops leading and trailing blank lines, and ends with exactly
/// one newline. The result uses `\r\n` if that is what `src` mostly used.
/// `in` and what follows it on one line, for a `let` written in brackets:
///
/// ```text
/// (let u =
///     mention env name
///   in Decl.Record { name = name })
/// ```
///
/// A cut leaves that `in` on a line of its own, as it does one that closes a
/// `let` heading its line -- where the body under it is the next of a
/// sequence, and reads so. In brackets there is one `let` and one body, and
/// the body reads as what the `in` introduces. Joined where the two fit in
/// `width`; left apart where they do not.
fn join_ins(text: &str, width: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let joined = (|| {
            if line.trim() != "in" {
                return None;
            }
            let at = indent(line);
            // The line that opened it: the nearest above that is further out,
            // which for a bracketed `let` is `(let …`, a unit out.
            let opener = lines[..i]
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty() && indent(l) < at)?;
            let rest = opener.trim_start().trim_start_matches(['(', '[', '{']);
            let bracketed = rest.len() < opener.trim_start().len()
                && (rest == "let" || rest.starts_with("let "))
                && indent(opener) + UNIT == at;
            let next = lines.get(i + 1)?.trim();
            let fits = at + "in ".len() + next.chars().count() <= width;
            (bracketed && !next.is_empty() && !next.starts_with("--") && fits)
                .then(|| format!("{}in {next}", " ".repeat(at)))
        })();
        match joined {
            Some(both) => {
                out.push(both);
                i += 2;
            }
            None => {
                out.push(line.to_string());
                i += 1;
            }
        }
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// A record's opening brace on the line of what it belongs to, as rustfmt
/// has a struct's:
///
/// ```text
/// Pat.Cons {
///   ref = refAlts out env n,
///   args = args
/// }
/// ```
///
/// A `{` alone on its line -- which is where an argument to a line once put
/// a record -- joins the line above it when that leaves the line within
/// `width`, and what it holds, with the brace that closes it, comes out by as
/// much as the brace was in from that line.
fn join_braces(text: &str, width: usize) -> String {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut out: Vec<String> = Vec::new();
    // How far the lines of a record being joined come out, and the column
    // its opening brace was in: lines deeper than that are its.
    let mut pulling: Option<(usize, usize)> = None;
    for line in text.lines() {
        if let Some((by, brace)) = pulling {
            let at = indent(line);
            let closes = at == brace && line.trim_start().starts_with('}');
            if !line.trim().is_empty() && (at > brace || closes) {
                out.push(format!("{}{}", " ".repeat(at - by), line.trim_start()));
                if closes {
                    pulling = None;
                }
                continue;
            }
            if !line.trim().is_empty() {
                pulling = None;
            }
        }
        let above = out.last().map(String::as_str).unwrap_or("");
        let code = above.split("--").next().unwrap_or("").trim_end();
        let joins = line.trim() == "{"
            && !above.trim().is_empty()
            && code.len() == above.trim_end().len()
            && above.matches('"').count() % 2 == 0
            && indent(line) > indent(above)
            && above.trim_end().chars().count() + 2 <= width
            // A brace the line above already ends with opens something else.
            && !code.ends_with(['{', '(', '[', ',']);
        if joins {
            let brace = indent(line);
            let by = brace - indent(above);
            let joined = format!("{} {{", above.trim_end());
            if let Some(last) = out.last_mut() {
                *last = joined;
            }
            pulling = Some((by, brace));
        } else {
            out.push(line.to_string());
        }
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

pub fn format(src: &str) -> String {
    // A byte-order mark is kept as it was, and never read as code.
    if let Some(rest) = src.strip_prefix('\u{feff}') {
        return format!("\u{feff}{}", format(rest));
    }
    let crlf = src.matches("\r\n").count() * 2 > src.matches('\n').count();
    let mut out: Vec<String> = Vec::new();
    let mut blanks = 0usize;

    for line in Indenter::new().run(src) {
        match line {
            Some(text) => {
                // Blank lines only count once they are followed by something.
                if !out.is_empty() {
                    for _ in 0..blanks.min(1) {
                        out.push(String::new());
                    }
                }
                blanks = 0;
                out.push(text);
            }
            None => blanks += 1,
        }
    }

    let sep = if crlf { "\r\n" } else { "\n" };
    let mut text = out.join(sep);
    if !text.is_empty() {
        text.push_str(sep);
    }
    text
}

/// Whether `format` would leave `src` alone.
pub fn is_formatted(src: &str) -> bool {
    format(src) == src
}

/// The indent a REPL continuation line should open with, given everything typed
/// so far. Empty when the next line belongs at the left margin.
pub fn continuation_indent(src: &str) -> String {
    " ".repeat(Indenter::new().next_indent(src))
}

// ===========================================================================
// The indenter
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// An unclosed bracket, and the closer that will match it.
    Open { at: usize, close: char },
    /// A `match … with`, whose `|` arms sit in the `match` keyword's own column
    /// — which is not the line's indent when the `match` sits behind a `(`.
    Arms { at: usize },
    /// An `if` still waiting for a `then` or `else` to start a line.
    Cond { at: usize },
    /// A `let` whose `in` is still to come, on some later line.
    Let { at: usize },
    /// Anything else that opened a deeper block.
    Block { at: usize },
}

impl Frame {
    /// Where the lines *inside* this frame belong.
    fn body(self) -> usize {
        match self {
            Frame::Open { at, .. } | Frame::Block { at } => at + UNIT,
            // These three do not indent by themselves: a `|` arm lines up with its
            // `match`, a `then` with its `if`, an `in` with its `let`.
            Frame::Arms { at } | Frame::Cond { at } | Frame::Let { at } => at,
        }
    }

    fn at(self) -> usize {
        match self {
            Frame::Open { at, .. }
            | Frame::Block { at }
            | Frame::Arms { at }
            | Frame::Cond { at }
            | Frame::Let { at } => at,
        }
    }
}

/// What one input line turned into.
enum Line {
    Blank,
    /// A comment-only line; its indent is settled in a second pass.
    Comment(String),
    Code {
        text: String,
        indent: usize,
    },
}

/// Where a line starts or ends up inside a string literal: in its text, or in
/// a `${…}` hole of it, how many braces deep.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nest {
    Text,
    Hole(usize),
    /// A raw string, `r#"…"#`, closed by a quote and this many `#`s.
    Raw(usize),
}

struct Indenter {
    stack: Vec<Frame>,
    /// Carried across lines: a string literal may span them, and so may the
    /// holes in it -- which hold strings of their own. Empty in code.
    nest: Vec<Nest>,
    /// Whether the line before this one opened a frame — which makes the next
    /// line the first of a block, and so structurally anchored.
    opened: bool,
    /// Whether a declaration's head is still being written: it began on a
    /// line above and has not reached its `=`. The lines that carry it on
    /// hang two units in, clear of the body that will hang one.
    head: bool,
    /// Where on the stack the brace of each `mod Name {` still open is,
    /// outermost first. What is inside one is declarations, as at the top
    /// level, one unit further in.
    modules: Vec<usize>,
    /// How far the last line with a place of its own was moved from where
    /// its author had it. A line that only carries it on keeps its author's
    /// column, and that column was measured from where the line above stood:
    /// it moves as far, so what hung under a line still hangs under it.
    shift: isize,
    /// Whether the line just placed was one that carries on the line above.
    continued: bool,
}

impl Indenter {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            nest: Vec::new(),
            opened: true,
            head: false,
            modules: Vec::new(),
            shift: 0,
            continued: false,
        }
    }

    /// Where declarations start here, as a place on the stack and a column:
    /// the bottom and the margin, or just inside the innermost `mod Name {`.
    fn floor(&self) -> (usize, usize) {
        // One whose brace has been closed is no longer one.
        match self.modules.iter().rev().find(|&&i| i < self.stack.len()) {
            Some(&i) => (i + 1, self.stack[i].body()),
            None => (0, 0),
        }
    }

    /// Where the lines inside the innermost frame belong.
    fn body(&self) -> usize {
        self.stack.last().map_or(0, |f| f.body())
    }

    /// Format every line of `src`. `None` is a blank line.
    ///
    /// Comment-only lines are resolved in a second pass: a comment introduces
    /// whatever comes after it, so it takes the indent of the next line of code.
    /// Nothing in the state machine can know that when the comment goes by.
    fn run(mut self, src: &str) -> Vec<Option<String>> {
        let lines: Vec<Line> = src.lines().map(|line| self.line(line)).collect();
        let mut out = Vec::with_capacity(lines.len());
        let mut next_code = 0;
        for line in lines.iter().rev() {
            out.push(match line {
                Line::Blank => None,
                Line::Code { text, indent } => {
                    next_code = *indent;
                    Some(text.clone())
                }
                Line::Comment(text) => Some(format!("{}{}", " ".repeat(next_code), text)),
            });
        }
        out.reverse();
        out
    }

    /// The indent a line appended to `src` should get.
    fn next_indent(mut self, src: &str) -> usize {
        for line in src.lines() {
            self.line(line);
        }
        self.body()
    }

    fn line(&mut self, raw: &str) -> Line {
        // Inside a string literal every byte is content, including the leading
        // whitespace and the blank lines. Reproduce the line exactly.
        if !self.nest.is_empty() {
            self.code(raw);
            return Line::Code {
                text: raw.to_string(),
                indent: 0,
            };
        }

        let text = raw.trim_end();
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            return Line::Blank;
        }

        let was = text.len() - trimmed.len();
        // A `mod Name {` whose brace has been closed is no longer one.
        while self.modules.last().is_some_and(|&i| i >= self.stack.len()) {
            self.modules.pop();
        }
        let (start, margin) = self.floor();
        // `Ctor{` is `Ctor {`: a brace is set off from the name it follows.
        let spaced = space_before_braces(trimmed);
        let trimmed = spaced.as_str();
        // Scan the trimmed line, so a token's column is relative to the line's
        // own indent and stays right after the line is moved.
        let code = self.code(trimmed);
        let toks = tokens(&code);

        // A comment-only line changes nothing — in particular it must not come
        // between a line that opened a block and the first line inside it.
        if toks.is_empty() {
            return Line::Comment(trimmed.to_string());
        }

        // As `indent_for` reads one: a macro call at the margin is a
        // declaration too.
        let declares = starts_declaration(&toks) || (was == margin && starts_macro_call(&toks));
        // A clause (`| f x = …`) or a variant list is not the head going
        // on: it has a place of its own.
        let carries_on = self.head
            && !declares
            && self.stack.len() == start
            && !matches!(toks.first_text(), "|" | "=");
        self.continued = false;
        let indent = if carries_on {
            margin + 2 * UNIT
        } else {
            self.indent_for(&toks, was)
        };
        if !self.continued {
            self.shift = indent as isize - was as isize;
        }
        // The body belongs to the declaration, not to the line of its head
        // that happens to end it.
        self.update(&toks, if carries_on { margin } else { indent });
        while self.modules.last().is_some_and(|&i| i >= self.stack.len()) {
            self.modules.pop();
        }
        if opens_module(&toks) && matches!(self.stack.last(), Some(Frame::Open { close: '}', .. }))
        {
            self.modules.push(self.stack.len() - 1);
        }
        let start = self.floor().0;
        self.head = (declares || carries_on) && !toks.has("=") && self.stack.len() == start;
        Line::Code {
            text: format!("{}{}", " ".repeat(indent), trimmed),
            indent,
        }
    }

    /// Decide this line's indent, popping any frame it closes.
    fn indent_for(&mut self, toks: &[Tok<'_>], author: usize) -> usize {
        let first = toks.first_text();

        // A closer goes back out to the line that opened it. Read the stack
        // without touching it: `update` walks this line's tokens in a moment and
        // pops the bracket there, and popping it twice would take the enclosing
        // frames with it.
        if let Some(close) = first
            .chars()
            .next()
            .filter(|c| matches!(c, ')' | ']' | '}'))
        {
            return self
                .stack
                .iter()
                .rev()
                .find(|f| matches!(f, Frame::Open { close: c, .. } if *c == close))
                .map_or(0, |f| f.at());
        }

        // `in` closes its `let`, and lines up with it — discarding whatever the
        // right-hand side opened along the way.
        if first == "in" {
            while let Some(frame) = self.stack.pop() {
                if let Frame::Let { at } = frame {
                    return at;
                }
            }
            return 0;
        }

        // A `|` is a match arm or a `data` variant, and a leading `=` is the head
        // of a variant list. An arm lines up with its `match`; a variant hangs one
        // unit under the `data` line that introduced it.
        if first == "|" || first == "=" {
            if first == "|" {
                while let Some(&frame) = self.stack.last() {
                    match frame {
                        Frame::Arms { at } => return at,
                        Frame::Open { .. } => break,
                        _ => {
                            self.stack.pop();
                        }
                    }
                }
            }
            return self.body() + UNIT;
        }

        // A `where` that heads its line -- a pattern synonym's builder, or an
        // `impl`'s bounds -- hangs one unit under what it belongs to.
        if first == "where" {
            return self.body() + UNIT;
        }

        // `then` / `else` line up with their `if`.
        if first == "then" || first == "else" {
            while let Some(&frame) = self.stack.last() {
                match frame {
                    Frame::Cond { at } => {
                        // `then` leaves the `if` open — its `else` still has to
                        // find it. `else` is the one that answers it.
                        if first == "else" {
                            self.stack.pop();
                        }
                        return at;
                    }
                    Frame::Open { .. } => break,
                    _ => {
                        self.stack.pop();
                    }
                }
            }
            return self.body();
        }

        // A top-level declaration starts over at the left margin. Guarded on
        // there being no open bracket, so a record field never resets anything.
        // A macro call written at the left margin is a declaration too, as the
        // parser reads it; indented, it is part of what is above it.
        let (start, margin) = self.floor();
        let declares = starts_declaration(toks) || (author == margin && starts_macro_call(toks));
        let inside = self.stack.get(start..).unwrap_or(&[]);
        if declares && !inside.iter().any(|f| matches!(f, Frame::Open { .. })) {
            self.stack.truncate(start);
            return margin;
        }

        // An item of a `trait` or an `impl` starts over too, one step inside
        // the braces that hold it: `fun`, `def` and `type` begin nothing else,
        // so inside a single top-level `{` they can only be the next item.
        if matches!(first, "fun" | "def" | "type")
            && matches!(inside.first(), Some(Frame::Open { at, close: '}' }) if *at == margin)
            && inside
                .iter()
                .filter(|f| matches!(f, Frame::Open { .. }))
                .count()
                == 1
        {
            self.stack.truncate(start + 1);
            return margin + UNIT;
        }

        let body = self.body();
        if self.stack.len() == start && author > margin {
            // At the top level and not a declaration: what a declaration
            // above carries on with -- the rest of a variant's fields, say --
            // which stays in from the margin, where its author put it.
            author
        } else if self.opened || self.stack.len() == start {
            // The first line of a block, or the top level: structural.
            body
        } else {
            // An unanchored continuation — the author's own alignment stands, as
            // long as it clears the block it sits in, moved as far as the line
            // it carries on was.
            self.continued = true;
            let moved = (author as isize + self.shift).max(0) as usize;
            moved.max(body)
        }
    }

    /// Push the frames this line opens.
    fn update(&mut self, toks: &[Tok<'_>], indent: usize) {
        let before = self.stack.len();

        // One left-to-right pass. Brackets are tracked twice: on the real stack,
        // which outlives the line, and line-locally, because two of the rules
        // below only care about nesting *within* this line.
        let mut open_here: Vec<usize> = Vec::new();
        // Where a `match`/`handle`'s arms belong. A `match` that *heads* its line —
        // including one continuing a `then` or `else` — keeps its arms at the
        // line's own indent. A `match` that something introduced (a bracket, an
        // `=`, an arm's `->`) puts them under the keyword instead, which is where
        // the expression it belongs to actually starts.
        let mut arms_at = indent;
        let mut introduced = false;
        // The column of an `if` still waiting for its `else`, at bracket depth 0 —
        // an `if … else …` nested inside brackets is already answered and must not
        // claim the `else` on the next line.
        let mut pending_if: Option<usize> = None;

        // The column of an `else` immediately before an `if`, so an `else if`
        // chain stays in one column instead of stepping right each rung.
        let mut chain: Option<usize> = None;
        // Between an arm's `|` and its `->`: where an `if` is the arm's guard,
        // which has no `else` to wait for.
        let mut in_arm_head = false;
        // Where on the stack each `let` of this line is, until its `in` comes:
        // one still here when the line ends wants its `in` on a later line.
        let mut lets_here: Vec<usize> = Vec::new();
        // Whether one of them is written in brackets, `(let x =`.
        let mut bracketed_let = false;
        for tok in toks {
            // A `let` wherever it is on the line, and not only at its head:
            // `(let x =` opens one too, and its `in` belongs under it and not
            // at the margin, which is where a `let` nobody recorded sent it --
            // taking the bracket around it along.
            match tok.text {
                "let" => {
                    // Its own place when only brackets come before it --
                    // `(let x =` -- and under its line when it comes after
                    // something else, far to the right: an `in` out there
                    // would be under nothing a reader looks at.
                    let after_brackets = toks
                        .iter()
                        .take_while(|t| !std::ptr::eq(*t, tok))
                        .all(|t| matches!(t.text, "(" | "[" | "{"));
                    // In brackets, its `in` is a unit in from the bracket
                    // and what it binds a unit further: `(let x =`, the
                    // value under it at four, `in` at two.
                    let at = if after_brackets && !std::ptr::eq(tok, &toks[0]) {
                        bracketed_let = true;
                        indent + UNIT
                    } else {
                        indent
                    };
                    lets_here.push(self.stack.len());
                    self.stack.push(Frame::Let { at });
                }
                "in" => {
                    if let Some(at) = lets_here.pop() {
                        self.stack.truncate(at);
                    } else if !std::ptr::eq(tok, &toks[0]) {
                        // The `in` of a `let` from a line above, ending this
                        // one: it closes that `let` too, if no bracket opened
                        // since stands between them. (One that heads its line
                        // was answered when the line was placed.)
                        let open = self
                            .stack
                            .iter()
                            .rposition(|f| matches!(f, Frame::Open { .. }));
                        let found = self
                            .stack
                            .iter()
                            .rposition(|f| matches!(f, Frame::Let { .. }));
                        if let Some(at) = found
                            && open.is_none_or(|o| o < at)
                        {
                            self.stack.truncate(at);
                        }
                    }
                }
                _ => {}
            }
            if open_here.is_empty() {
                match tok.text {
                    "|" => in_arm_head = true,
                    "->" => in_arm_head = false,
                    _ => {}
                }
                match tok.text {
                    "if" if in_arm_head => chain = None,
                    "if" => {
                        pending_if = Some(chain.unwrap_or(tok.col));
                        chain = None;
                    }
                    "else" => {
                        pending_if = None;
                        chain = Some(tok.col);
                    }
                    _ => chain = None,
                }
            }
            if matches!(tok.text, "match" | "handle") {
                arms_at = if introduced { indent + tok.col } else { indent };
            }
            if matches!(tok.text, "(" | "[" | "{" | "=" | "->" | "<-" | "\\") {
                introduced = true;
            }
            for (i, c) in tok.text.char_indices() {
                match c {
                    '(' | '[' | ']' | '{' | '}' | ')' => {
                        let close = match c {
                            '(' => ')',
                            '[' => ']',
                            '{' => '}',
                            other => other,
                        };
                        if matches!(c, '(' | '[' | '{') {
                            self.stack.push(Frame::Open { at: indent, close });
                            open_here.push(tok.col + i);
                        } else {
                            open_here.pop();
                            while let Some(frame) = self.stack.pop() {
                                if matches!(frame, Frame::Open { close: k, .. } if k == close) {
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // An `else` on a later line lines up with the `if` itself, which is not
        // the start of the line when the `if` is a right-hand side.
        if let Some(col) = pending_if {
            self.stack.push(Frame::Cond { at: indent + col });
        }

        match toks.last_text() {
            "with" => self.stack.push(Frame::Arms { at: arms_at }),
            // A `|` arm whose body starts on the next line hangs two units in, so
            // it clears the `->` and reads as subordinate to the arm.
            "->" if toks.has("|") => self.stack.push(Frame::Block { at: indent + UNIT }),
            // What a bracketed `let` binds hangs a unit past its `in`.
            "=" if bracketed_let && !lets_here.is_empty() => {
                self.stack.push(Frame::Block { at: indent + UNIT })
            }
            "=" | "->" | "then" | "else" | "\\" | "<-" => {
                self.stack.push(Frame::Block { at: indent })
            }
            _ => {}
        }

        // A trailing `,` ends an item of a bracketed list — a handler's clauses, a
        // record's fields, an array's elements. Whatever this line opened belongs
        // to the item and is finished with it, so drop back to the bracket the
        // list lives in and let the next item start level with this one.
        if toks.last_text() == "," && self.stack.iter().any(|f| matches!(f, Frame::Open { .. })) {
            while let Some(&frame) = self.stack.last() {
                if matches!(frame, Frame::Open { .. }) {
                    break;
                }
                self.stack.pop();
            }
        }

        // Only a keyword-opened block anchors the line below it. A bracket does
        // not: code inside brackets is routinely aligned under the opener or under
        // an argument, and no rule this small reproduces that — so the author's
        // own column is the better answer there.
        // And an `in` alone on its line: what is under it is what the `let`
        // is for, and stands where the block it is in puts it.
        let lone_in = toks.len() == 1 && toks.first_text() == "in";
        // And a bracket that ends its line: there is nothing after it for
        // what it holds to be lined up under, so that goes a unit in -- a
        // record's fields under `Ctor {`, as rustfmt has a struct's.
        let ends_open = matches!(toks.last_text(), "{" | "(" | "[")
            && self.stack.len() > before
            && matches!(self.stack.last(), Some(Frame::Open { .. }));
        self.opened = lone_in
            || ends_open
            || (self.stack.len() > before
                && !matches!(self.stack.last(), Some(Frame::Open { .. })));
    }

    /// Reduce a line to just its code: comments dropped, and each string or
    /// character literal replaced by a single `"` so it reads as one opaque token
    /// rather than vanishing (a vanished literal would leave `def x = "…"` ending
    /// in `=`, which opens a block that is not there).
    fn code(&mut self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            // Inside a literal everything is its content, holes included: a
            // hole's braces and strings are never structure here, but they do
            // decide where the literal ends.
            if let Some(&top) = self.nest.last() {
                out.push(' ');
                match (top, c) {
                    (Nest::Raw(hashes), '"') if closes_raw(&bytes, i, hashes) => {
                        self.nest.pop();
                        out.push_str(&" ".repeat(hashes));
                        i += 1 + hashes;
                        continue;
                    }
                    (Nest::Raw(_), _) => {}
                    (Nest::Hole(_), 'r') if opens_raw(&bytes, i).is_some() => {
                        let hashes = opens_raw(&bytes, i).expect("just checked");
                        self.nest.push(Nest::Raw(hashes));
                        out.push_str(&" ".repeat(1 + hashes));
                        i += 2 + hashes;
                        continue;
                    }
                    (Nest::Text, '\\') => {
                        i += 2;
                        out.push(' ');
                        continue;
                    }
                    (Nest::Text, '"') => {
                        self.nest.pop();
                    }
                    (Nest::Text, '$') if bytes.get(i + 1) == Some(&'{') => {
                        self.nest.push(Nest::Hole(0));
                        out.push(' ');
                        i += 2;
                        continue;
                    }
                    (Nest::Hole(_), '"') => self.nest.push(Nest::Text),
                    (Nest::Hole(depth), '{') => {
                        *self.nest.last_mut().expect("in a hole") = Nest::Hole(depth + 1)
                    }
                    (Nest::Hole(0), '}') => {
                        self.nest.pop();
                    }
                    (Nest::Hole(depth), '}') => {
                        *self.nest.last_mut().expect("in a hole") = Nest::Hole(depth - 1)
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            match c {
                '"' => {
                    self.nest.push(Nest::Text);
                    out.push('"');
                    i += 1;
                }
                'r' if opens_raw(&bytes, i).is_some() => {
                    let hashes = opens_raw(&bytes, i).expect("just checked");
                    self.nest.push(Nest::Raw(hashes));
                    out.push('"');
                    out.push_str(&" ".repeat(1 + hashes));
                    i += 2 + hashes;
                }
                // `--` always starts a comment: the lexer prefers the comment rule
                // over the operator one, so there is no `--` operator to confuse.
                '-' if bytes.get(i + 1) == Some(&'-') => break,
                // An apostrophe is part of an identifier (`xs'`) unless it opens a
                // character literal.
                '\'' if i > 0 && is_word(bytes[i - 1]) => {
                    out.push(c);
                    i += 1;
                }
                '\'' => {
                    out.push('"');
                    i += 1;
                    while i < bytes.len() {
                        let c = bytes[i];
                        out.push(' ');
                        i += 1;
                        if c == '\\' {
                            if i < bytes.len() {
                                out.push(' ');
                                i += 1;
                            }
                        } else if c == '\'' {
                            break;
                        }
                    }
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }
}

/// Whether these tokens begin a top-level declaration.
fn starts_declaration(toks: &[Tok<'_>]) -> bool {
    match toks.first_text() {
        "mod" | "use" | "def" | "fun" | "data" | "record" | "effect" | "type" | "trait"
        | "impl" | "infix" | "infixl" | "infixr" => true,
        // `pattern P …` — a pattern synonym; `pattern` is a name anywhere else.
        "pattern" => toks
            .get(1)
            .is_some_and(|t| t.text.starts_with(|c: char| c.is_uppercase())),
        // `@pub`, `@attr(…)` — an attribute, not a user-defined `@` operator.
        "@" => toks
            .get(1)
            .is_some_and(|t| t.text.starts_with(|c: char| c.is_alphabetic())),
        _ => false,
    }
}

/// `line` with a space between a name and a `{` written against it: `Ctor{`
/// is `Ctor {`, as rustfmt has a struct's. Only in code -- what is in a
/// string, a character or a comment is left -- and not at all on a line with
/// a raw string in it, which this does not read.
fn space_before_braces(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if !chars.contains(&'{') || (0..chars.len()).any(|i| opens_raw(&chars, i).is_some()) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 2);
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_string = true,
            // A comment: the rest is as it was written.
            '-' if chars.get(i + 1) == Some(&'-') => {
                out.extend(&chars[i..]);
                return out;
            }
            // A character: `'{'`, `'\\n'`.
            '\'' if chars.get(i + 2) == Some(&'\'') => {
                out.extend(&chars[i..i + 3]);
                i += 3;
                continue;
            }
            '\'' if chars.get(i + 1) == Some(&'\\') && chars.get(i + 3) == Some(&'\'') => {
                out.extend(&chars[i..i + 4]);
                i += 4;
                continue;
            }
            '{' if i > 0 && is_word(chars[i - 1]) => out.push(' '),
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Whether these tokens are `mod Name {`, attributes before it or not: the
/// head of a module written where it is declared.
fn opens_module(toks: &[Tok<'_>]) -> bool {
    let Some(i) = toks.iter().position(|t| t.text == "mod") else {
        return false;
    };
    starts_declaration(toks)
        && toks.len() == i + 3
        && toks[i + 2].text == "{"
        && (i == 0 || toks.first_text() == "@")
}

/// Whether these tokens begin with a macro call: `name!` or `A.b.name!`.
fn starts_macro_call(toks: &[Tok<'_>]) -> bool {
    let mut i = 0;
    loop {
        match toks.get(i) {
            Some(t) if t.text.starts_with(|c: char| c.is_alphabetic() || c == '_') => {}
            _ => return false,
        }
        match toks.get(i + 1).map(|t| t.text) {
            Some("!") => return true,
            Some(".") => i += 2,
            _ => return false,
        }
    }
}

/// How many `#`s the raw string starting at `i` opens with, if one does: an `r`
/// that is not the end of a longer name, `#`s, and a quote.
fn opens_raw(line: &[char], i: usize) -> Option<usize> {
    if line.get(i) != Some(&'r') || (i > 0 && is_word(line[i - 1])) {
        return None;
    }
    let hashes = line[i + 1..].iter().take_while(|&&c| c == '#').count();
    (line.get(i + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// Whether the quote at `i` closes a raw string opened with `hashes` `#`s.
fn closes_raw(line: &[char], i: usize, hashes: usize) -> bool {
    line.get(i + 1..i + 1 + hashes)
        .is_some_and(|h| h.iter().all(|&c| c == '#'))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

/// A token plus the column it starts at, so a `match` buried behind a `(` can
/// still say where its arms belong.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Tok<'a> {
    col: usize,
    text: &'a str,
}

/// Split a line of code into words, single brackets, and maximal operator runs —
/// enough to see the first token, the last token, and every bracket. Mirrors the
/// lexer's operator character class so `->` and `|>` stay whole.
///
/// By character, not byte: `é` is one letter of a name, not two bytes that
/// each look like something else. A token's column counts characters, which
/// is what an indent lines up with.
fn tokens(code: &str) -> Vec<Tok<'_>> {
    const OP: &str = "!$%&*+./<=>?@|^~:-";
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = code.char_indices().collect();
    // The byte offset of the `k`th character, or the end.
    let at = |k: usize| chars.get(k).map_or(code.len(), |&(b, _)| b);
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k].1;
        let start = k;
        if c.is_whitespace() {
            k += 1;
            continue;
        } else if is_word(c) {
            while k < chars.len() && is_word(chars[k].1) {
                k += 1;
            }
        } else if OP.contains(c) {
            while k < chars.len() && OP.contains(chars[k].1) {
                k += 1;
            }
        } else {
            k += 1;
        }
        out.push(Tok {
            col: start,
            text: &code[at(start)..at(k)],
        });
    }
    out
}

/// Helpers that read a line's tokens as plain text.
trait Toks {
    fn first_text(&self) -> &str;
    fn last_text(&self) -> &str;
    fn has(&self, text: &str) -> bool;
}

impl Toks for [Tok<'_>] {
    fn first_text(&self) -> &str {
        self.first().map_or("", |t| t.text)
    }
    fn last_text(&self) -> &str {
        self.last().map_or("", |t| t.text)
    }
    fn has(&self, text: &str) -> bool {
        self.iter().any(|t| t.text == text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(src: &str) -> String {
        format(src)
    }

    // --- text that is not ASCII ----------------------------------------------
    //
    // The tokenizer once walked bytes, and read the first byte of `é` as a
    // letter of its own: slicing the line there panicked, and an editor that
    // formats on save took the language server down with it.

    #[test]
    fn an_in_closes_a_let_that_does_not_head_its_line() {
        // `(let x =`: what it binds hangs at four, the `in` is at two inside
        // the bracket, and the bracket is still open for what follows.
        let src = "\
fun f xs =
  if V.isEmpty xs then 0
  else
    (let fits =
      V.filter
        (\\c -> c > 0)
        xs
in
      if V.len fits == 1
      then 1
      else 2)
";
        let want = "\
fun f xs =
  if V.isEmpty xs then 0
  else
    (let fits =
        V.filter
          (\\c -> c > 0)
          xs
      in
      if V.len fits == 1
      then 1
      else 2)
";
        assert_eq!(f(src), want);
        assert_eq!(f(want), want, "formatting is idempotent");
        // One that heads its line is where it was.
        let headed = "fun g x =\n  let total =\n    compute x\n  in\n  total\n";
        assert_eq!(f(headed), headed);
        // A `let` far along its line has its `in` under the line, not out
        // under the `let`; and what a bracket opened after a `let` holds is
        // a unit in from the `let`.
        let far = "fun k n =\n  if n == 0 then () else let u = step n\n  in\n  k (n - 1)\n";
        assert_eq!(f(far), far);
        let listed = "fun g () =\n  let xs = [\n    1,\n    2\n  ] in\n  xs\n";
        assert_eq!(f(listed), listed);
        // And a `let … in` on one line leaves nothing open.
        let one = "fun h x =\n  (let y = x in\n    y + 1)\n";
        assert_eq!(f(one), one);
    }

    #[test]
    fn a_bracketed_let_has_its_body_on_the_line_of_its_in() {
        let src = "\
fun d name =
  (let u =
    mention env name
  in
    Decl.Record { name = name })
";
        let want = "\
fun d name =
  (let u =
      mention env name
    in Decl.Record { name = name })
";
        assert_eq!(format_within(src, 80), want);
        assert_eq!(format_within(want, 80), want, "and again changes nothing");
        // Too long to share a line: the body stays under the `in`.
        let long = "\
fun d name =
  (let u =
      mention env name
    in
    Decl.Record { name = name, params = params, fields = fields, more = more })
";
        assert_eq!(format_within(long, 80), long);
        // A `let` heading its line keeps its body on the next: a sequence.
        let headed = "fun g x =\n  let total =\n    compute x\n  in\n  total\n";
        assert_eq!(format_within(headed, 80), headed);
    }

    #[test]
    fn a_brace_is_set_off_from_the_name_before_it() {
        assert_eq!(
            f("def p = Point{ x = 1, y = 2 }\n"),
            "def p = Point { x = 1, y = 2 }\n"
        );
        // Not in a string, a hole of one, a character or a comment.
        for same in [
            "def s = \"Point{ x }\"\n",
            "def s = \"a ${show x} b\"\n",
            "def c = '{'\n",
            "def n = 1 -- Point{ x }\n",
            "def q = Point { x = 1 }\n",
        ] {
            assert_eq!(f(same), same);
        }
    }

    #[test]
    fn a_brace_alone_joins_the_line_its_record_belongs_to() {
        let src = "\
fun p r =
  wrap
    (Pat.Cons
      {
        ref = refAlts out env n,
        args = args
      })
";
        let want = "\
fun p r =
  wrap
    (Pat.Cons {
      ref = refAlts out env n,
      args = args
    })
";
        assert_eq!(format_within(src, 100), want);
        assert_eq!(format_within(want, 100), want, "and again changes nothing");
    }

    #[test]
    fn what_a_module_holds_is_one_unit_in() {
        let src = "\
@pub mod Core {
fun secret n = n + 1

@pub fun eval e =
match e with
| 0 -> 1
| _ -> 2

@pub mod Expr {
use Node.*

@pub data Node
= Int Int
| Add Node Node

trait Shown a {
fun shown : a -> String
}
}
}

def after = 1
";
        let want = "\
@pub mod Core {
  fun secret n = n + 1

  @pub fun eval e =
    match e with
    | 0 -> 1
    | _ -> 2

  @pub mod Expr {
    use Node.*

    @pub data Node
      = Int Int
      | Add Node Node

    trait Shown a {
      fun shown : a -> String
    }
  }
}

def after = 1
";
        assert_eq!(f(src), want);
        assert_eq!(f(want), want, "formatting is idempotent");
    }

    #[test]
    fn a_name_that_is_not_ascii_is_one_word() {
        assert_eq!(f("def café = 1\n"), "def café = 1\n");
        assert_eq!(f("fun naïve x =\n    x + 1\n"), "fun naïve x =\n  x + 1\n");
    }

    #[test]
    fn symbols_and_wide_characters_in_code_are_kept() {
        for src in [
            "def π = 3\n",
            "def x = '→'\n",
            "def 名前 = 1\n",
            "def e = \"😀 emoji\"\n",
            "def f x = x -- a comment with ümlauts\n",
        ] {
            assert_eq!(f(src), src, "{src}");
        }
    }

    #[test]
    fn indentation_after_a_wide_character_is_still_decided() {
        // A `match` after a non-ASCII name: its arms still line up under it.
        let src = "def größe x = match x with\n| 0 -> 1\n| _ -> 2\n";
        let once = f(src);
        assert_eq!(f(&once), once, "formatting is idempotent");
        assert!(
            once.contains("| 0 -> 1") && once.contains("| _ -> 2"),
            "{once}"
        );
    }

    #[test]
    fn a_byte_order_mark_is_kept_and_not_read_as_code() {
        assert_eq!(f("\u{feff}def x = 1   \n"), "\u{feff}def x = 1\n");
        assert_eq!(
            f("\u{feff}def f y =\n\tf y\n"),
            "\u{feff}def f y =\n  f y\n"
        );
    }

    #[test]
    fn trailing_whitespace_and_tabs_go() {
        assert_eq!(f("def x = 1   \n"), "def x = 1\n");
        assert_eq!(f("def f y =\n\tf y\n"), "def f y =\n  f y\n");
    }

    #[test]
    fn blank_line_runs_collapse_and_the_file_ends_once() {
        assert_eq!(
            f("\n\ndef a = 1\n\n\n\ndef b = 2\n\n\n"),
            "def a = 1\n\ndef b = 2\n"
        );
    }

    #[test]
    fn an_empty_file_stays_empty() {
        assert_eq!(f(""), "");
        assert_eq!(f("\n\n"), "");
    }

    #[test]
    fn a_declaration_body_indents_one_unit() {
        assert_eq!(f("fun f x =\nx + 1\n"), "fun f x =\n  x + 1\n");
    }

    /// The items of a `trait` or an `impl` each start over inside its braces,
    /// however deep the one before ended.
    #[test]
    fn the_items_of_an_impl_line_up() {
        let src = "impl Show [a;] where Show a {
fun show xs =
match xs with
| [;] -> \".\"
| x :: rest ->
show x

fun other x = 1
}
trait T a {
type E a
fun m : a -> E a
}
";
        let want = "impl Show [a;] where Show a {
  fun show xs =
    match xs with
    | [;] -> \".\"
    | x :: rest ->
        show x

  fun other x = 1
}
trait T a {
  type E a
  fun m : a -> E a
}
";
        assert_eq!(f(src), want);
        assert_eq!(f(want), want);
    }

    /// A `type` alias and a signature start declarations of their own, rather
    /// than continuing the body before them — and the clauses under a signature
    /// hang off it, one unit in, with their bodies one unit further.
    #[test]
    fn an_alias_and_a_signature_are_declarations() {
        assert_eq!(
            f("fun f x =\nx + 1\ntype P = (Int, Int)\nfun g : P -> Int\n| g p =\nf 1\n"),
            "fun f x =\n  x + 1\ntype P = (Int, Int)\nfun g : P -> Int\n  | g p =\n    f 1\n"
        );
    }

    /// A guard is an `if` with no `else` to wait for: an arm having one is
    /// laid out exactly as the same arm without.
    #[test]
    fn a_guard_is_not_an_if_waiting_for_its_else() {
        for plain in [
            "fun f n =\n  match n with\n  | x -> 1\n  | _ ->\n    let y = 2 in\n    y\n",
            "fun f n =\nmatch n with\n| x ->\nif x > 5\nthen 1\nelse 2\n| _ ->\nif n < 0\nthen 3\nelse 4\n",
            "fun f n =\n  match n with\n  | x -> 1\n  | _ -> 0\n\ndef g =\n  if True\n  then 1\n  else 2\n",
        ] {
            let guard = |s: &str| s.replacen("| x ->", "| x if x > 0 ->", 1);
            assert_eq!(f(&guard(plain)), guard(&f(plain)), "{plain}");
        }
    }

    #[test]
    fn match_arms_line_up_with_the_match() {
        let src = "fun f xs =\nmatch xs with\n| Nil -> 0\n| Cons x r -> 1\n";
        assert_eq!(
            f(src),
            "fun f xs =\n  match xs with\n  | Nil -> 0\n  | Cons x r -> 1\n"
        );
    }

    #[test]
    fn an_arm_body_on_its_own_line_hangs_two_units() {
        let src =
            "fun f xs =\nmatch xs with\n| Nil -> 0\n| Cons x r ->\nmatch r with\n| Nil -> 1\n";
        assert_eq!(
            f(src),
            "fun f xs =\n  match xs with\n  | Nil -> 0\n  | Cons x r ->\n      match r with\n      | Nil -> 1\n"
        );
    }

    #[test]
    fn then_and_else_line_up_with_their_if() {
        let src = "fun f n =\nif n == 0\nthen 1\nelse\nn * 2\n";
        assert_eq!(
            f(src),
            "fun f n =\n  if n == 0\n  then 1\n  else\n    n * 2\n"
        );
    }

    #[test]
    fn a_trailing_then_opens_a_block_and_else_closes_it() {
        let src = "fun f n =\nif n == 0 then\n1\nelse\n2\n";
        assert_eq!(
            f(src),
            "fun f n =\n  if n == 0 then\n    1\n  else\n    2\n"
        );
    }

    #[test]
    fn brackets_indent_their_contents_and_the_closer_goes_back_out() {
        let src = "record R = {\na : Int,\nb : Int,\n}\n";
        assert_eq!(f(src), "record R = {\n  a : Int,\n  b : Int,\n}\n");
    }

    #[test]
    fn a_macro_call_at_the_margin_is_a_declaration() {
        // At the left margin it begins a declaration, as the parser reads it;
        // indented, it is part of what is above.
        let src = "def x =\n  f 1\nderive! { a }\n\ndef y =\n  g\n  stringify!(b)\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn a_declaration_resets_the_indent() {
        // Even after something deeply nested, the next declaration is at column 0.
        let src = "fun f x =\nmatch x with\n| A ->\nB\n\ndef g = 1\n";
        assert_eq!(
            f(src),
            "fun f x =\n  match x with\n  | A ->\n      B\n\ndef g = 1\n"
        );
    }

    #[test]
    fn a_closing_bracket_does_not_pop_the_frames_around_it() {
        // The closer is read twice — once to pick this line's indent, once when
        // the line's tokens are scanned — so it must only *pop* once, or the
        // `let` it sits inside goes with it and `in` lands at the margin.
        let src = "fun f x =\n  let body =\n    g [\n      1,\n    ]\n  in\n  body\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn a_trailing_comma_ends_the_item_it_belongs_to() {
        // Each handler clause starts level with the last, however deep the one
        // before it went.
        let src = "fun f a =\n  handle a () with {\n    one x k ->\n      match x with\n      | A -> k 1,\n    two y k -> k 2,\n    return r -> r\n  }\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn hand_aligned_continuations_are_left_alone() {
        // No frame opened on the first body line, so the second is a continuation
        // and keeps the column the author picked.
        let src = "fun f b i =\n  bitOr (g 0 b i)\n        (g 0 b (i + 1))\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn a_continuation_still_clears_its_block() {
        let src = "fun f b i =\n  g b\nh b\n";
        assert_eq!(f(src), "fun f b i =\n  g b\n  h b\n");
    }

    #[test]
    fn comments_and_strings_are_never_parsed_as_code() {
        // The `{` and `match` here are text, not structure.
        let src = "def a = \"{ match with\"\n-- a comment with ( and | in it\ndef b = 2\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn an_interpolated_string_ends_where_its_holes_let_it() {
        // The quotes and braces inside the holes are the holes', so the literal
        // ends at the last quote and `def b` is back at the top level.
        let src = "def a = \"x ${f \"}\" { y = 1 }} (\"\ndef b = 2\n";
        assert_eq!(f(src), src);
        // A hole that spans lines keeps its lines as they are.
        let src = "def a = \"sum: ${\n    1 +\n  2\n}\"\ndef b = 2\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn a_raw_string_ends_only_at_its_own_closing() {
        // The `"` and `{` inside are text; the literal ends at `"#`.
        let src = "def a = r#\"x \" { (\"#\ndef b = 2\n";
        assert_eq!(f(src), src);
        // Across lines, its lines are kept as they are.
        let src = "def a = r\"one\n   two {\"\ndef b = 2\n";
        assert_eq!(f(src), src);
    }

    #[test]
    fn an_apostrophe_in_a_name_is_not_a_char_literal() {
        assert_eq!(f("fun f xs' =\nxs'\n"), "fun f xs' =\n  xs'\n");
    }

    #[test]
    fn formatting_is_idempotent() {
        let src = "fun f xs =\n  match xs with\n  | Nil -> 0\n  | Cons x r ->\n      if x then\n        1\n      else\n        2\n";
        assert_eq!(f(&f(src)), f(src));
        assert_eq!(f(src), src);
    }

    // --- the REPL's auto-indent ---------------------------------------------

    #[test]
    fn continuation_indent_follows_the_structure() {
        assert_eq!(continuation_indent("fun f x ="), "  ");
        assert_eq!(continuation_indent("fun f x =\n  match x with"), "  ");
        assert_eq!(continuation_indent("def x = ("), "  ");
        assert_eq!(continuation_indent("def x = 1"), "");
    }

    #[test]
    fn continuation_indent_hangs_an_arm_body() {
        assert_eq!(
            continuation_indent("fun f x =\n  match x with\n  | A ->"),
            "      "
        );
    }
}
