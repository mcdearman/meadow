//! Converting between the editor's positions and the compiler's byte offsets.
//!
//! LSP counts lines from zero and, by default, measures a character offset in
//! **UTF-16 code units** — not bytes and not characters. The compiler's spans are
//! byte offsets. Getting this wrong is invisible in ASCII and wrong everywhere
//! else, so the conversion is done properly in both directions.

use meadow_compiler::span::Span;

pub struct LineIndex {
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
    text: String,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex {
            starts,
            text: text.to_string(),
        }
    }

    /// `(line, utf16 character)` -> byte offset.
    pub fn offset(&self, line: u32, character: u32) -> usize {
        let start = match self.starts.get(line as usize) {
            Some(&s) => s,
            None => return self.text.len(),
        };
        let rest = &self.text[start..];
        let mut units = 0u32;
        for (i, c) in rest.char_indices() {
            if units >= character || c == '\n' {
                return start + i;
            }
            units += c.len_utf16() as u32;
        }
        start + rest.len()
    }

    /// Byte offset -> `(line, utf16 character)`.
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.starts[line];
        let units: u32 = self.text[start..offset]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum();
        (line as u32, units)
    }

    pub fn range(&self, span: Span) -> ((u32, u32), (u32, u32)) {
        (
            self.position(span.start as usize),
            self.position(span.end as usize),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_and_positions_round_trip() {
        let idx = LineIndex::new("one\ntwo\nthree");
        assert_eq!(idx.offset(0, 0), 0);
        assert_eq!(idx.offset(1, 0), 4);
        assert_eq!(idx.offset(2, 2), 10);
        assert_eq!(idx.position(0), (0, 0));
        assert_eq!(idx.position(4), (1, 0));
        assert_eq!(idx.position(10), (2, 2));
    }

    #[test]
    fn a_character_is_a_utf16_unit_not_a_byte() {
        // `é` is two bytes but one UTF-16 unit; the emoji is four bytes and two.
        let idx = LineIndex::new("é😀x");
        assert_eq!(idx.offset(0, 1), 2, "one unit past `é` is byte 2");
        assert_eq!(idx.offset(0, 3), 6, "past the emoji is byte 6");
        assert_eq!(idx.position(2), (0, 1));
        assert_eq!(idx.position(6), (0, 3));
    }

    #[test]
    fn a_column_past_the_end_of_a_line_clamps_to_it() {
        let idx = LineIndex::new("ab\ncd");
        assert_eq!(idx.offset(0, 99), 2);
        assert_eq!(idx.offset(9, 0), 5);
    }
}
