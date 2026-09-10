//! Stream chunking for `log.append` (contract §2, "流式文本约定").
//!
//! Two guarantees live here:
//!
//! 1. **A UTF-8 code point is never split across chunks.**  A partial trailing
//!    sequence stays buffered until its remaining bytes arrive.
//! 2. Bytes that are not valid UTF-8 are replaced with U+FFFD and the chunk is
//!    marked [`TextEncoding::Utf8Lossy`] so the UI can show that the producer,
//!    not the engine, emitted garbage.
//!
//! The chunker is line-oriented on purpose: guest serial output arrives a byte
//! at a time from a polling driver, and a UI (or a test) that wants to grep the
//! boot banner needs the whole line in one event rather than 30 one-character
//! events.

use crate::types::TextEncoding;

/// Incremental UTF-8-safe/top-level chunker for a byte stream.
#[derive(Debug, Default)]
pub struct Utf8Chunker {
    pending: Vec<u8>,
}

impl Utf8Chunker {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Feed bytes as they arrive from the child process.
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// Number of buffered bytes not yet emitted.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Take everything up to and including the last newline.
    ///
    /// Returns `None` while no complete line is buffered.  Splitting only at
    /// `\n` (an ASCII byte) can never cut a code point.
    pub fn take_lines(&mut self) -> Option<(String, TextEncoding)> {
        let cut = self.pending.iter().rposition(|b| *b == b'\n')? + 1;
        Some(self.take_prefix(cut))
    }

    /// Take everything buffered *except* a trailing incomplete UTF-8 sequence
    /// (used on idle timers and at EOF so a partial line is still shown).
    ///
    /// Returns `None` when nothing (or only an incomplete sequence) is
    /// buffered.
    pub fn take_available(&mut self) -> Option<(String, TextEncoding)> {
        if self.pending.is_empty() {
            return None;
        }
        match std::str::from_utf8(&self.pending) {
            Ok(_) => Some(self.take_prefix(self.pending.len())),
            Err(err) => {
                let valid = err.valid_up_to();
                match err.error_len() {
                    // Incomplete trailing sequence: hold it back until the rest
                    // of the code point arrives.
                    None => {
                        if valid == 0 {
                            None
                        } else {
                            Some(self.take_prefix(valid))
                        }
                    }
                    // Genuinely invalid bytes: emit *only* the offending byte
                    // lossily and keep the tail, so a valid sequence that
                    // follows the garbage is not swallowed with it.
                    Some(invalid_len) => Some(self.take_prefix(valid + invalid_len)),
                }
            }
        }
    }

    /// Everything left, including any incomplete trailing sequence, decoded
    /// lossily.  Called once the producer is gone (EOF / process reaped).
    pub fn take_remainder(&mut self) -> Option<(String, TextEncoding)> {
        if self.pending.is_empty() {
            return None;
        }
        Some(self.take_prefix(self.pending.len()))
    }

    fn take_prefix(&mut self, cut: usize) -> (String, TextEncoding) {
        let bytes: Vec<u8> = self.pending.drain(..cut).collect();
        match String::from_utf8(bytes) {
            Ok(text) => (text, TextEncoding::Utf8),
            Err(err) => (
                String::from_utf8_lossy(err.as_bytes()).into_owned(),
                TextEncoding::Utf8Lossy,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_emitted_whole_and_in_order() {
        let mut chunker = Utf8Chunker::new();
        chunker.push(b"PrincessIDE reference kernel booted\n[refkernel] multibo");
        let (first, encoding) = chunker.take_lines().unwrap();
        assert_eq!(first, "PrincessIDE reference kernel booted\n");
        assert_eq!(encoding, TextEncoding::Utf8);
        // The incomplete tail stays buffered: no split line event.
        assert!(chunker.take_lines().is_none());
        assert_eq!(chunker.pending_len(), "[refkernel] multibo".len());
        chunker.push(b"ot: magic=0x36d76289\n");
        let (second, _) = chunker.take_lines().unwrap();
        assert_eq!(second, "[refkernel] multiboot: magic=0x36d76289\n");
        assert!(chunker.is_empty());
        assert!(chunker.take_available().is_none());
    }

    #[test]
    fn a_code_point_split_across_reads_is_never_cut() {
        // U+00E9 ("é") is 0xC3 0xA9; feed the two halves separately.
        let mut chunker = Utf8Chunker::new();
        chunker.push(&[0xC3]);
        assert!(chunker.take_available().is_none(), "half a code point must be held");
        chunker.push(&[0xA9, b'\n']);
        let (text, encoding) = chunker.take_lines().unwrap();
        assert_eq!(text, "é\n");
        assert_eq!(encoding, TextEncoding::Utf8);

        // Four-byte sequence, one byte at a time.
        let mut chunker = Utf8Chunker::new();
        for byte in "🦀".as_bytes() {
            chunker.push(&[*byte]);
        }
        let (text, encoding) = chunker.take_available().unwrap();
        assert_eq!(text, "🦀");
        assert_eq!(encoding, TextEncoding::Utf8);
    }

    #[test]
    fn invalid_bytes_are_replaced_and_marked_lossy() {
        let mut chunker = Utf8Chunker::new();
        chunker.push(b"ok\xFF\xFE\n");
        let (text, encoding) = chunker.take_lines().unwrap();
        assert_eq!(text, "ok\u{FFFD}\u{FFFD}\n");
        assert_eq!(encoding, TextEncoding::Utf8Lossy);

        // Invalid bytes without a newline still come out lossily on demand.
        let mut chunker = Utf8Chunker::new();
        chunker.push(b"\xFF");
        let (text, encoding) = chunker.take_available().unwrap();
        assert_eq!(text, "\u{FFFD}");
        assert_eq!(encoding, TextEncoding::Utf8Lossy);

        // A trailing incomplete sequence after garbage: the garbage goes now,
        // the partial sequence waits for its continuation.
        let mut chunker = Utf8Chunker::new();
        chunker.push(b"\xFF\xC3");
        let (text, encoding) = chunker.take_available().unwrap();
        assert_eq!(text, "\u{FFFD}");
        assert_eq!(encoding, TextEncoding::Utf8Lossy);
        assert_eq!(chunker.pending_len(), 1);
        chunker.push(&[0xA9]);
        let (text, encoding) = chunker.take_remainder().unwrap();
        assert_eq!(text, "é");
        assert_eq!(encoding, TextEncoding::Utf8);
    }

    #[test]
    fn remainder_flushes_an_incomplete_tail_at_eof() {
        let mut chunker = Utf8Chunker::new();
        chunker.push(b"no trailing newline\xC3");
        let (text, encoding) = chunker.take_remainder().unwrap();
        assert_eq!(text, "no trailing newline\u{FFFD}");
        assert_eq!(encoding, TextEncoding::Utf8Lossy);
        assert!(chunker.take_remainder().is_none());
    }
}
