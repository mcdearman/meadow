//! **The source formatter** — `meadow fmt`, and the REPL's auto-indent.
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

/// One level of indentation.
pub const UNIT: usize = 2;

/// Format a whole source file.
///
/// Re-indents every line, strips trailing whitespace, collapses runs of blank
/// lines to one, drops leading and trailing blank lines, and ends with exactly
/// one newline. The result uses `\r\n` if that is what `src` mostly used.
pub fn format(src: &str) -> String {
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
    Code { text: String, indent: usize },
}

struct Indenter {
    stack: Vec<Frame>,
    /// Carried across lines: a string literal may span them.
    in_string: bool,
    /// Whether the line before this one opened a frame — which makes the next
    /// line the first of a block, and so structurally anchored.
    opened: bool,
}

impl Indenter {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            in_string: false,
            opened: true,
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
        if self.in_string {
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
        // Scan the trimmed line, so a token's column is relative to the line's
        // own indent and stays right after the line is moved.
        let code = self.code(trimmed);
        let toks = tokens(&code);

        // A comment-only line changes nothing — in particular it must not come
        // between a line that opened a block and the first line inside it.
        if toks.is_empty() {
            return Line::Comment(trimmed.to_string());
        }

        let indent = self.indent_for(&toks, was);
        self.update(&toks, indent);
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
        if let Some(close) = first.chars().next().filter(|c| matches!(c, ')' | ']' | '}')) {
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
        if starts_declaration(toks) && !self.stack.iter().any(|f| matches!(f, Frame::Open { .. })) {
            self.stack.clear();
            return 0;
        }

        let body = self.body();
        if self.opened || self.stack.is_empty() {
            // The first line of a block, or the top level: structural.
            body
        } else {
            // An unanchored continuation — the author's own alignment stands, as
            // long as it clears the block it sits in.
            author.max(body)
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
        for tok in toks {
            if open_here.is_empty() {
                match tok.text {
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

        // A `let` whose `in` is not on this line will want one later.
        if toks.first_text() == "let" && !toks.has("in") {
            self.stack.push(Frame::Let { at: indent });
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
        self.opened = self.stack.len() > before
            && !matches!(self.stack.last(), Some(Frame::Open { .. }));
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
            if self.in_string {
                out.push(' ');
                if c == '\\' {
                    i += 2;
                    out.push(' ');
                    continue;
                }
                if c == '"' {
                    self.in_string = false;
                }
                i += 1;
                continue;
            }
            match c {
                '"' => {
                    self.in_string = true;
                    out.push('"');
                    i += 1;
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
        "mod" | "use" | "def" | "fun" | "data" | "record" | "effect" => true,
        // `@pub`, `@attr(…)` — an attribute, not a user-defined `@` operator.
        "@" => toks
            .get(1)
            .is_some_and(|t| t.text.starts_with(|c: char| c.is_alphabetic())),
        _ => false,
    }
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
fn tokens(code: &str) -> Vec<Tok<'_>> {
    const OP: &str = "!$%&*+./<=>?@|^~:-";
    let mut out = Vec::new();
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if is_word(c) {
            let start = i;
            while i < bytes.len() && is_word(bytes[i] as char) {
                i += 1;
            }
            out.push(Tok { col: start, text: &code[start..i] });
        } else if OP.contains(c) {
            let start = i;
            while i < bytes.len() && OP.contains(bytes[i] as char) {
                i += 1;
            }
            out.push(Tok { col: start, text: &code[start..i] });
        } else {
            out.push(Tok { col: i, text: &code[i..i + 1] });
            i += 1;
        }
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

    #[test]
    fn trailing_whitespace_and_tabs_go() {
        assert_eq!(f("def x = 1   \n"), "def x = 1\n");
        assert_eq!(f("def f y =\n\tf y\n"), "def f y =\n  f y\n");
    }

    #[test]
    fn blank_line_runs_collapse_and_the_file_ends_once() {
        assert_eq!(f("\n\ndef a = 1\n\n\n\ndef b = 2\n\n\n"), "def a = 1\n\ndef b = 2\n");
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
        let src = "fun f xs =\nmatch xs with\n| Nil -> 0\n| Cons x r ->\nmatch r with\n| Nil -> 1\n";
        assert_eq!(
            f(src),
            "fun f xs =\n  match xs with\n  | Nil -> 0\n  | Cons x r ->\n      match r with\n      | Nil -> 1\n"
        );
    }

    #[test]
    fn then_and_else_line_up_with_their_if() {
        let src = "fun f n =\nif n == 0\nthen 1\nelse\nn * 2\n";
        assert_eq!(f(src), "fun f n =\n  if n == 0\n  then 1\n  else\n    n * 2\n");
    }

    #[test]
    fn a_trailing_then_opens_a_block_and_else_closes_it() {
        let src = "fun f n =\nif n == 0 then\n1\nelse\n2\n";
        assert_eq!(f(src), "fun f n =\n  if n == 0 then\n    1\n  else\n    2\n");
    }

    #[test]
    fn brackets_indent_their_contents_and_the_closer_goes_back_out() {
        let src = "record R = {\na : Int,\nb : Int,\n}\n";
        assert_eq!(f(src), "record R = {\n  a : Int,\n  b : Int,\n}\n");
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
