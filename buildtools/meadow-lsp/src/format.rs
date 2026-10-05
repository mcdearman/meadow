//! `textDocument/formatting`: what `meadow fmt` would do to a document, as the
//! edits an editor applies -- which is how format-on-save reaches the server.
//!
//! The formatter is [`formatted`], what `meadow fmt` runs with no options, so
//! saving in the editor and formatting on the command line cannot disagree. It
//! is given the document as the editor holds it, unsaved changes and all.

use crate::pos::LineIndex;
use lsp_types::{Position, Range, TextEdit};

/// The edits that make `text` formatted: none if it already is, and otherwise
/// one, covering only the lines that change.
///
/// One edit rather than the whole document replaced. An editor applying a
/// replacement of everything can lose the cursor, the scroll position, folds
/// and markers on lines the formatter never touched -- and this runs on every
/// save. The formatter only re-indents, so what changes is usually a few lines
/// together, and everything before and after them is left alone.
///
/// The edit starts at a line start and ends at one. Both ends are therefore
/// ASCII boundaries in both texts -- a newline in the old one, and bytes equal
/// to the old one's in the new -- so neither can land inside a character.
/// `text` as `meadow fmt` leaves it: indented, lines longer than the width
/// cut, and a record or a bracketed `let` set out as the formatter sets them
/// out -- [`meadow_fmt::format_within`] at [`meadow_fmt::WIDTH`].
///
/// Cutting and joining lines move tokens, and must move nothing else: what
/// the compiler's lexer reads out of the result is checked against what it
/// read out of `text`, as the command line checks it, and a document where
/// they differ is only indented, which moves none.
pub fn formatted(text: &str) -> String {
    use meadow_compiler::lexer::tokenize;
    use meadow_compiler::source::{Source, SourceKind};
    let out = meadow_fmt::format_within(text, meadow_fmt::WIDTH);
    let tokens = |text: &str| {
        tokenize(Source::new(SourceKind::Interactive, text.into()))
            .tokens
            .iter()
            .map(|t| t.value().clone())
            .collect::<Vec<_>>()
    };
    if tokens(text) == tokens(&out) {
        out
    } else {
        meadow_fmt::format(text)
    }
}

pub fn edits(text: &str) -> Vec<TextEdit> {
    let formatted = formatted(text);
    if formatted == text {
        return Vec::new();
    }
    let (old, new) = (text.as_bytes(), formatted.as_bytes());

    // The unchanged lines at the top.
    let same = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let start = old[..same]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |i| i + 1);

    // And at the bottom, not reaching back past the top's end in either text.
    let room = (old.len() - start).min(new.len() - start);
    let mut tail = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while tail > 0 && old[old.len() - tail - 1] != b'\n' {
        tail -= 1;
    }

    let index = LineIndex::new(text);
    let at = |offset: usize| {
        let (line, character) = index.position(offset);
        Position::new(line, character)
    };
    vec![TextEdit {
        range: Range {
            start: at(start),
            end: at(old.len() - tail),
        },
        new_text: formatted[start..new.len() - tail].to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an editor does with the edits: every one is checked by applying it
    /// to the document and comparing with what the formatter produces.
    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let index = LineIndex::new(text);
        let mut out = text.to_string();
        let mut sorted = edits.to_vec();
        sorted.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));
        for e in sorted {
            let from = index.offset(e.range.start.line, e.range.start.character);
            let to = index.offset(e.range.end.line, e.range.end.character);
            out.replace_range(from..to, &e.new_text);
        }
        out
    }

    fn check(text: &str) -> Vec<TextEdit> {
        let edits = edits(text);
        assert_eq!(apply(text, &edits), formatted(text), "{text:?}");
        edits
    }

    #[test]
    fn a_save_sets_a_record_out_as_the_command_line_does() {
        // Too long for one line, so set out as a record is: its brace on
        // the constructor's line, a field to a line, and a space before the
        // brace.
        let fields = "id = theIdentifierOfTheBinding,\n       param = theParameterItTakes,\n       value = theValueItIsBoundTo,\n       body = theBodyItIsIn";
        let text = format!("fun p r =\n  LetRec\n    {{\n       {fields}\n     }}\n");
        let want = "fun p r =\n  LetRec {\n    id = theIdentifierOfTheBinding,\n    param = theParameterItTakes,\n    value = theValueItIsBoundTo,\n    body = theBodyItIsIn\n  }\n";
        assert_eq!(formatted(&text), want);
        assert_eq!(formatted(&want.replace("LetRec {", "LetRec{")), want);
        // And one that fits a line is put on it.
        assert_eq!(
            formatted("fun p r =\n  LetRec\n    {\n       id = id,\n       body = b\n     }\n"),
            "fun p r = LetRec { id = id, body = b }\n"
        );
    }

    #[test]
    fn a_formatted_document_needs_no_edits() {
        let text = formatted("fun f x =\n  x + 1\n\ndef main = f 1\n");
        assert!(edits(&text).is_empty());
    }

    #[test]
    fn only_the_changed_lines_are_replaced() {
        let text = "fun f x = x + 1\n\ndef g y =\n        match y with\n   | 0 -> 1\n  | _ -> 2\n\ndef main = f 1\n";
        let e = check(text);
        assert_eq!(e.len(), 1);
        // The first three lines and the last two were already right, and are
        // outside the edit.
        assert!(e[0].range.start.line >= 2, "{e:?}");
        assert!(e[0].range.end.line <= 6, "{e:?}");
    }

    #[test]
    fn changes_at_either_end_of_the_document() {
        check("   fun f x = x\ndef main = f 1\n");
        check("fun f x = x\ndef main = f 1   \n\n\n");
        check("def main = 1");
        check("\n\n\ndef main = 1\n");
    }

    /// Positions are UTF-16, offsets are bytes, and a character that is both
    /// wider than a byte and wider than one UTF-16 unit sits before the change.
    #[test]
    fn characters_wider_than_a_byte_before_and_after_the_change() {
        check("def s = \"héllo 😀\"\nfun f x =\n        x\ndef t = \"ünï 😀\"\n");
    }

    #[test]
    fn crlf_documents() {
        check("fun f x =\r\n      x\r\ndef main = f 1\r\n");
    }
}
