//! **What the string primitives mean**, on bytes: one definition for every
//! engine to agree with, whatever it keeps a string in.
//!
//! Offsets count bytes, as `Std.String` does. A string is UTF-8 whatever is
//! done to it, so a slice through the middle of a character comes out with
//! U+FFFD in place of the bytes it cut -- the same rule `bytesToString`
//! follows.

/// Bytes `from` up to `to` of `bytes`, both clamped to it.
pub fn slice(bytes: &[u8], from: i64, to: i64) -> String {
    let (lo, hi) = clamp(bytes.len(), from, to);
    String::from_utf8_lossy(&bytes[lo..hi]).into_owned()
}

/// `from..to` clamped to a string of `len` bytes, never backwards.
pub fn clamp(len: usize, from: i64, to: i64) -> (usize, usize) {
    let n = len as i64;
    let lo = from.clamp(0, n);
    let hi = to.clamp(lo, n);
    (lo as usize, hi as usize)
}

/// Where `needle` first occurs in `hay` at or after byte `from`, or -1. An
/// empty needle is found wherever the search starts, up to the end.
pub fn index_of(hay: &[u8], needle: &[u8], from: i64) -> i64 {
    let start = from.max(0) as usize;
    if start > hay.len() {
        return -1;
    }
    if needle.is_empty() {
        return start as i64;
    }
    hay[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map_or(-1, |i| (start + i) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicing_clamps_and_keeps_utf8() {
        assert_eq!(slice(b"hello", 1, 3), "el");
        assert_eq!(slice(b"hi", -4, 99), "hi");
        assert_eq!(slice(b"hi", 5, 9), "");
        assert_eq!(slice(b"hi", 2, 1), "");
        assert_eq!(slice("é".as_bytes(), 0, 1), "\u{FFFD}");
    }

    #[test]
    fn searching_counts_bytes_from_where_it_starts() {
        assert_eq!(index_of(b"hello", b"l", 0), 2);
        assert_eq!(index_of(b"hello", b"l", 3), 3);
        assert_eq!(index_of(b"hello", b"l", 4), -1);
        assert_eq!(index_of(b"hello", b"", 2), 2);
        assert_eq!(index_of(b"hello", b"", 9), -1);
        assert_eq!(index_of(b"hi", b"hello", 0), -1);
    }
}
