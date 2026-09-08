//! Source positions.
//!
//! [`Span`] is a byte range `start..end` into a source string (`u32` offsets — a
//! source file is never that big). [`Located<T>`] pairs a value with its span; it
//! is the AST's spine wrapper (the HIR uses `meadow_hir::Node`, which adds a
//! node id). The `chumsky` trait impls at the bottom let the parser produce
//! `Located` nodes directly via `map_with`.

use chumsky::span::{Span as ChumskySpan, WrappingSpan};
use std::{
    fmt::{Debug, Display},
    ops::{Index, Range},
};

#[derive(Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub fn extend(&self, other: Span) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Span> for Range<usize> {
    fn from(span: Span) -> Self {
        span.start as usize..span.end as usize
    }
}

impl From<Range<usize>> for Span {
    fn from(range: Range<usize>) -> Self {
        Self {
            start: range.start as u32,
            end: range.end as u32,
        }
    }
}

impl Index<Span> for str {
    type Output = str;

    fn index(&self, index: Span) -> &Self::Output {
        // Convert the Span to a Range<usize>
        let range: Range<usize> = index.into();

        // Ensure the range is within the bounds of the string
        if range.start > self.len() || range.end > self.len() {
            panic!("Index out of bounds");
        }

        // Return the slice of the string for the given range
        &self[range]
    }
}

impl ChumskySpan for Span {
    type Context = ();

    type Offset = u32;

    fn new(_context: Self::Context, range: Range<Self::Offset>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }

    fn context(&self) -> Self::Context {
        ()
    }

    fn start(&self) -> Self::Offset {
        self.start
    }

    fn end(&self) -> Self::Offset {
        self.end
    }
}

impl<T> WrappingSpan<T> for Span {
    type Spanned = Located<T>;

    fn make_wrapped(self, inner: T) -> Self::Spanned {
        Located::new(inner, self)
    }

    fn inner_of(spanned: &Self::Spanned) -> &T {
        spanned.value()
    }

    fn span_of(spanned: &Self::Spanned) -> &Self {
        &spanned.span
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located<T> {
    pub value: Box<T>,
    pub span: Span,
}

impl<T> Located<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self {
            value: Box::new(value),
            span,
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_covers_both_spans() {
        let a = Span::new(2, 5);
        let b = Span::new(8, 12);
        assert_eq!(a.extend(b), Span::new(2, 12));
        assert_eq!(b.extend(a), Span::new(2, 12));
    }

    #[test]
    fn round_trips_through_range() {
        let s = Span::new(3, 7);
        let r: Range<usize> = s.into();
        assert_eq!(r, 3..7);
        assert_eq!(Span::from(3usize..7usize), s);
    }

    #[test]
    fn indexes_a_string_slice() {
        let text = "hello world";
        assert_eq!(&text[Span::new(6, 11)], "world");
    }

    #[test]
    fn display_and_debug_match() {
        let s = Span::new(1, 4);
        assert_eq!(s.to_string(), "1..4");
        assert_eq!(format!("{s:?}"), "1..4");
    }
}
