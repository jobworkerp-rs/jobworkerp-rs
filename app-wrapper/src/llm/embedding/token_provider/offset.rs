//! Byte-offset ↔ char-offset conversion for tokenizer spans.
//!
//! HuggingFace `tokenizers` (and any byte-oriented tokenizer) report token
//! spans as **byte** offsets, but the embedding chunker's contract is in
//! half-open Unicode scalar (char) offsets. [`CharByteMap`] converts between the
//! two and rejects offsets that fall inside a multi-byte code point rather than
//! silently rounding, so a corrupted span surfaces loudly.

use anyhow::{Result, anyhow};

/// Precomputed map from byte offset to char offset for one text.
///
/// `starts[i]` is the byte offset at which the `i`-th char begins; the final
/// element is the total byte length (`text.len()`) so that a byte offset equal
/// to the length maps to the char count (a valid half-open end).
#[derive(Debug, Clone)]
pub struct CharByteMap {
    starts: Vec<usize>,
}

impl CharByteMap {
    /// Build the map in a single pass over `text`.
    pub fn new(text: &str) -> Self {
        // One entry per char start, plus a trailing sentinel at text.len().
        let mut starts = Vec::with_capacity(text.len() + 1);
        for (byte_idx, _) in text.char_indices() {
            starts.push(byte_idx);
        }
        starts.push(text.len());
        Self { starts }
    }

    /// Number of Unicode scalars (chars) in the source text.
    pub fn char_len(&self) -> usize {
        // Excludes the trailing sentinel.
        self.starts.len() - 1
    }

    /// Convert a byte offset to a char offset.
    ///
    /// # Errors
    /// Returns an error if `byte` is out of range or falls inside a multi-byte
    /// code point (i.e. is not a char boundary).
    pub fn byte_to_char(&self, byte: usize) -> Result<usize> {
        match self.starts.binary_search(&byte) {
            Ok(char_idx) => Ok(char_idx),
            Err(_) => Err(anyhow!(
                "byte offset {byte} is not a char boundary (text bytes={})",
                self.starts.last().copied().unwrap_or(0)
            )),
        }
    }

    /// Convert a half-open byte range to a half-open char range.
    ///
    /// # Errors
    /// Errors if either endpoint is not a char boundary, or if `end < start`.
    pub fn byte_range_to_char_range(&self, start: usize, end: usize) -> Result<(usize, usize)> {
        let cs = self.byte_to_char(start)?;
        let ce = self.byte_to_char(end)?;
        if ce < cs {
            return Err(anyhow!(
                "inverted byte range: start={start} (char {cs}) > end={end} (char {ce})"
            ));
        }
        Ok((cs, ce))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_byte_equals_char() {
        let m = CharByteMap::new("hello");
        assert_eq!(m.char_len(), 5);
        assert_eq!(m.byte_to_char(0).unwrap(), 0);
        assert_eq!(m.byte_to_char(5).unwrap(), 5); // end sentinel
        assert_eq!(m.byte_range_to_char_range(0, 5).unwrap(), (0, 5));
    }

    #[test]
    fn test_japanese_three_byte() {
        // "あい": each char is 3 bytes → byte 0,3,6.
        let m = CharByteMap::new("あい");
        assert_eq!(m.char_len(), 2);
        assert_eq!(m.byte_range_to_char_range(0, 6).unwrap(), (0, 2));
        assert_eq!(m.byte_range_to_char_range(3, 6).unwrap(), (1, 2));
    }

    #[test]
    fn test_emoji_four_byte() {
        // "a🦀b": 'a'=1B @0, '🦀'=4B @1, 'b'=1B @5.
        let m = CharByteMap::new("a🦀b");
        assert_eq!(m.char_len(), 3);
        assert_eq!(m.byte_range_to_char_range(1, 5).unwrap(), (1, 2));
        assert_eq!(m.byte_range_to_char_range(0, 6).unwrap(), (0, 3));
    }

    #[test]
    fn test_offset_inside_multibyte_errors() {
        // "あ" occupies bytes 0..3; byte 1 and 2 are inside the code point.
        let m = CharByteMap::new("あ");
        assert!(m.byte_to_char(1).is_err());
        assert!(m.byte_to_char(2).is_err());
        // Boundaries are fine.
        assert_eq!(m.byte_to_char(0).unwrap(), 0);
        assert_eq!(m.byte_to_char(3).unwrap(), 1);
    }

    #[test]
    fn test_out_of_range_errors() {
        let m = CharByteMap::new("abc");
        assert!(m.byte_to_char(4).is_err());
    }

    #[test]
    fn test_inverted_range_errors() {
        let m = CharByteMap::new("hello");
        assert!(m.byte_range_to_char_range(3, 1).is_err());
    }

    #[test]
    fn test_empty_string() {
        let m = CharByteMap::new("");
        assert_eq!(m.char_len(), 0);
        assert_eq!(m.byte_to_char(0).unwrap(), 0);
        assert_eq!(m.byte_range_to_char_range(0, 0).unwrap(), (0, 0));
    }

    #[test]
    fn test_nfc_nfd_char_counts_differ() {
        // Precomposed "é" (NFC, 1 char / 2 bytes) vs decomposed "e\u{301}"
        // (NFD, 2 chars / 3 bytes). char_len must reflect the actual scalars.
        let nfc = CharByteMap::new("Caf\u{e9}");
        assert_eq!(nfc.char_len(), 4);
        let nfd = CharByteMap::new("Cafe\u{301}");
        assert_eq!(nfd.char_len(), 5);
    }
}
