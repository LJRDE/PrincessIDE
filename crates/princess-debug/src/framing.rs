//! `Content-Length` framing for the Debug Adapter Protocol.
//!
//! DAP rides on a byte stream (here: gdb's stdin/stdout) and delimits messages
//! with an HTTP-like header block:
//!
//! ```text
//! Content-Length: 119\r\n
//! \r\n
//! {"seq":1,"type":"request",...}
//! ```
//!
//! Only `Content-Length` is mandatory; DAP also defines an optional
//! `Content-Type` (`application/vscode-jsonrpc; charset=utf-8`) which gdb does
//! not send but other adapters do, so the parser accepts and ignores it.
//!
//! The encoder is deliberately separated from the reader so both directions can
//! be unit-tested without a live adapter — that is the only way to pin the exact
//! bytes that go on the wire.

use std::io::{self, Read};

/// The header a DAP message must carry.  `Content-Length` counts **bytes** of
/// the UTF-8 encoded JSON body, not characters.
pub const HEADER_LENGTH: &str = "Content-Length";

/// Largest body we are willing to buffer.
///
/// A DAP reply from gdb is normally well under a few hundred KiB, but
/// `variables` on a large register file or `disassemble` over a big range can
/// grow.  16 MiB is far above any legitimate message and still bounds the
/// damage a corrupt or hostile stream can do to the engine's memory (D22).
pub const MAX_CONTENT_LENGTH: usize = 16 * 1024 * 1024;

/// A failure while framing a DAP stream.
///
/// Every variant carries the offending bytes so the engine can put **raw
/// evidence** in `PrincessError::detail` instead of paraphrasing it
/// (contract §0 principle 4: "失败必须显式").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The header block was not terminated by a blank line within the limit.
    HeaderTooLong { limit: usize },
    /// A header line was not `Name: value`.
    MalformedHeader { line: String },
    /// `Content-Length` was absent.
    MissingContentLength { header: String },
    /// `Content-Length` was present but not a non-negative integer.
    BadContentLength { value: String },
    /// `Content-Length` exceeded [`MAX_CONTENT_LENGTH`].
    BodyTooLarge { length: usize, limit: usize },
    /// The stream ended in the middle of a message.
    UnexpectedEof { wanted: usize, got: usize },
    /// The body was not valid UTF-8.
    NotUtf8 { lossy: String },
    /// The underlying reader failed.
    ReadFailed { message: String },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::HeaderTooLong { limit } => {
                write!(f, "DAP header block exceeded {limit} bytes without a blank line")
            }
            FrameError::MalformedHeader { line } => {
                write!(f, "malformed DAP header line: {line:?}")
            }
            FrameError::MissingContentLength { header } => {
                write!(f, "DAP message has no Content-Length header: {header:?}")
            }
            FrameError::BadContentLength { value } => {
                write!(f, "DAP Content-Length is not a number: {value:?}")
            }
            FrameError::BodyTooLarge { length, limit } => {
                write!(f, "DAP Content-Length {length} exceeds the {limit} byte limit")
            }
            FrameError::UnexpectedEof { wanted, got } => {
                write!(f, "DAP body truncated: wanted {wanted} bytes, stream ended after {got}")
            }
            FrameError::NotUtf8 { lossy } => {
                write!(f, "DAP body is not valid UTF-8: {lossy:?}")
            }
            FrameError::ReadFailed { message } => {
                write!(f, "reading the DAP stream failed: {message}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode one JSON body into a complete DAP frame.
///
/// Returns the exact bytes to write to the adapter's stdin.
pub fn encode_frame(body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 32);
    // The length must be the *byte* length; `str::len` already is.
    out.extend_from_slice(format!("{HEADER_LENGTH}: {}\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

/// Decode one frame from `input`, returning the body and how many bytes it
/// consumed.
///
/// `Ok(None)` means "not a whole frame yet" — the caller should read more and
/// retry.  This makes the function usable both on a growing buffer (the async
/// reader) and on a complete in-memory byte string (the unit tests).
pub fn decode_frame(input: &[u8]) -> Result<Option<(&str, usize)>, FrameError> {
    // Locate the blank line that ends the header block.  DAP uses CRLF, but some
    // adapters emit bare LF; accept both rather than hanging on a legal-enough
    // stream.
    let (header_end, body_start) = match find_header_terminator(input) {
        Some(found) => found,
        None => {
            if input.len() > MAX_CONTENT_LENGTH.min(64 * 1024) {
                return Err(FrameError::HeaderTooLong { limit: 64 * 1024 });
            }
            return Ok(None);
        }
    };

    let header_text = String::from_utf8_lossy(&input[..header_end]);
    let mut content_length: Option<usize> = None;
    for raw_line in header_text.split("\r\n").flat_map(|l| l.split('\n')) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(FrameError::MalformedHeader { line: line.to_string() });
        };
        if name.trim().eq_ignore_ascii_case(HEADER_LENGTH) {
            let value = value.trim();
            let parsed = value.parse::<usize>().map_err(|_| FrameError::BadContentLength {
                value: value.to_string(),
            })?;
            content_length = Some(parsed);
        }
        // Any other header (Content-Type, ...) is accepted and ignored.
    }

    let Some(length) = content_length else {
        return Err(FrameError::MissingContentLength {
            header: header_text.into_owned(),
        });
    };
    if length > MAX_CONTENT_LENGTH {
        return Err(FrameError::BodyTooLarge {
            length,
            limit: MAX_CONTENT_LENGTH,
        });
    }

    let total = body_start + length;
    if input.len() < total {
        return Ok(None);
    }
    let bytes = &input[body_start..total];
    let body = std::str::from_utf8(bytes).map_err(|_| FrameError::NotUtf8 {
        lossy: String::from_utf8_lossy(bytes).into_owned(),
    })?;
    Ok(Some((body, total)))
}

/// Find the end of the header block, returning `(index_of_blank_line, index_of_body)`.
fn find_header_terminator(input: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\n' {
            // "\n\n"
            if i + 1 < input.len() && input[i + 1] == b'\n' {
                return Some((i, i + 2));
            }
            // "\n\r\n"
            if i + 2 < input.len() && input[i + 1] == b'\r' && input[i + 2] == b'\n' {
                return Some((i, i + 3));
            }
        }
        i += 1;
    }
    None
}

/// Read exactly one frame from a blocking reader.
///
/// Used by the reader thread; it blocks until a whole message is available and
/// returns `Ok(None)` on a clean EOF (the adapter exited), which the caller maps
/// to a "debug adapter terminated" condition rather than an I/O error.
/// Read one frame, keeping the leftover bytes of a short read for the caller.
///
/// Prefer [`FrameReader`] when reading more than one message: this helper exists
/// for the single-shot case and makes the buffering requirement explicit by
/// returning the reader.  The test below pins why re-creating a reader per call
/// is wrong.
pub fn read_frame<R: Read>(reader: R) -> Result<(Option<String>, FrameReader<R>), FrameError> {
    let mut stream = FrameReader::new(reader);
    let body = stream.next_body()?;
    Ok((body, stream))
}

/// A buffered DAP frame reader that keeps leftover bytes between calls.
///
/// This is the blocking counterpart of [`decode_frame`]'s "not a whole frame
/// yet" contract.  A reader that re-created its buffer per call would drop the
/// tail of a TCP-like read that carried 1.5 messages — the second message would
/// be silently lost and the conversation would desynchronise, which is exactly
/// the failure mode [`crate::transport`] exists to avoid.  Keeping the overflow
/// here means the caller can call `next_body` in a loop.
#[derive(Debug)]
pub struct FrameReader<R: Read> {
    reader: R,
    buffer: Vec<u8>,
    eof: bool,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::with_capacity(8192),
            eof: false,
        }
    }

    /// Read the next message body.
    ///
    /// `Ok(None)` means the stream ended cleanly between messages — the adapter
    /// exited, which is a state, not an error.
    pub fn next_body(&mut self) -> Result<Option<String>, FrameError> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some((body, used)) = decode_frame(&self.buffer)? {
                let body = body.to_string();
                self.buffer.drain(..used);
                return Ok(Some(body));
            }
            if self.eof {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let got = self.buffer.len();
                return Err(FrameError::UnexpectedEof {
                    wanted: got + 1,
                    got,
                });
            }
            match self.reader.read(&mut chunk) {
                Ok(0) => self.eof = true,
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(FrameError::ReadFailed { message: e.to_string() }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encodes_exact_bytes_on_the_wire() {
        // `{"seq":1}` is exactly 9 bytes: Content-Length must count the JSON
        // body, and getting this wrong desynchronises the whole stream.
        let body = r#"{"seq":1}"#;
        assert_eq!(body.len(), 9);
        let frame = encode_frame(body);
        assert_eq!(frame, b"Content-Length: 9\r\n\r\n{\"seq\":1}");
    }

    #[test]
    fn content_length_counts_utf8_bytes_not_characters() {
        // "é" is 2 bytes; a character count would produce 3 and desynchronise
        // the stream for every following message.
        let body = r#"{"a":"é"}"#;
        assert_eq!(body.chars().count(), 9);
        assert_eq!(body.len(), 10);
        let frame = encode_frame(body);
        assert!(frame.starts_with(b"Content-Length: 10\r\n\r\n"));
        let (decoded, used) = decode_frame(&frame).unwrap().unwrap();
        assert_eq!(decoded, body);
        assert_eq!(used, frame.len());
    }

    #[test]
    fn decodes_a_frame_and_reports_consumed_bytes() {
        let frame = encode_frame(r#"{"type":"event"}"#);
        let mut stream = frame.clone();
        stream.extend_from_slice(&encode_frame(r#"{"type":"response"}"#));

        let (first, used) = decode_frame(&stream).unwrap().unwrap();
        assert_eq!(first, r#"{"type":"event"}"#);
        let (second, used2) = decode_frame(&stream[used..]).unwrap().unwrap();
        assert_eq!(second, r#"{"type":"response"}"#);
        assert_eq!(used + used2, stream.len());
    }

    #[test]
    fn incomplete_input_asks_for_more_instead_of_failing() {
        let frame = encode_frame(r#"{"seq":1,"type":"request"}"#);
        // Every strict prefix must yield Ok(None), never an error and never a
        // short body: a reader that returned a truncated body would hand
        // invalid JSON to serde and look like an adapter bug.
        for cut in 0..frame.len() {
            assert_eq!(
                decode_frame(&frame[..cut]),
                Ok(None),
                "prefix of length {cut} should be incomplete"
            );
        }
        assert!(decode_frame(&frame).unwrap().is_some());
    }

    #[test]
    fn accepts_bare_lf_and_ignores_content_type() {
        let body = r#"{"x":1}"#;
        let frame = format!("Content-Length: {}\nContent-Type: application/vscode-jsonrpc; charset=utf-8\n\n{body}", body.len());
        let (decoded, used) = decode_frame(frame.as_bytes()).unwrap().unwrap();
        assert_eq!(decoded, body);
        assert_eq!(used, frame.len());
    }

    #[test]
    fn missing_content_length_is_an_error_naming_the_header() {
        let err = decode_frame(b"Content-Type: application/json\r\n\r\n{}").unwrap_err();
        match err {
            FrameError::MissingContentLength { header } => assert!(header.contains("Content-Type")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn non_numeric_and_negative_lengths_are_errors() {
        assert!(matches!(
            decode_frame(b"Content-Length: abc\r\n\r\n{}").unwrap_err(),
            FrameError::BadContentLength { .. }
        ));
        // "-1" cannot parse as usize; it must not be read as "huge".
        assert!(matches!(
            decode_frame(b"Content-Length: -1\r\n\r\n{}").unwrap_err(),
            FrameError::BadContentLength { .. }
        ));
    }

    #[test]
    fn oversized_length_is_rejected_before_allocating() {
        let header = format!("Content-Length: {}\r\n\r\n", MAX_CONTENT_LENGTH + 1);
        assert!(matches!(
            decode_frame(header.as_bytes()).unwrap_err(),
            FrameError::BodyTooLarge { .. }
        ));
    }

    #[test]
    fn malformed_header_line_is_reported_verbatim() {
        let err = decode_frame(b"this is not a header\r\n\r\n{}").unwrap_err();
        match err {
            FrameError::MalformedHeader { line } => assert_eq!(line, "this is not a header"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn header_name_matching_is_case_insensitive() {
        let body = r#"{"ok":true}"#;
        let frame = format!("content-length: {}\r\n\r\n{body}", body.len());
        assert_eq!(decode_frame(frame.as_bytes()).unwrap().unwrap().0, body);
    }

    #[test]
    fn read_frame_streams_from_a_reader_and_stops_at_clean_eof() {
        let mut stream = encode_frame(r#"{"a":1}"#);
        stream.extend_from_slice(&encode_frame(r#"{"b":2}"#));
        let mut reader = FrameReader::new(Cursor::new(stream));
        assert_eq!(reader.next_body().unwrap().unwrap(), r#"{"a":1}"#);
        assert_eq!(reader.next_body().unwrap().unwrap(), r#"{"b":2}"#);
        // Clean EOF is None, not an error: the adapter simply went away.
        assert_eq!(reader.next_body().unwrap(), None);
    }

    /// A reader that hands back one byte at a time must still yield whole,
    /// correctly ordered frames: this is the shape a real pipe takes when a
    /// large reply is split across reads.
    #[test]
    fn frames_survive_being_delivered_one_byte_at_a_time() {
        let mut stream = encode_frame(r#"{"a":1}"#);
        stream.extend_from_slice(&encode_frame(r#"{"b":2}"#));
        struct Dribble {
            data: Vec<u8>,
            at: usize,
        }
        impl Read for Dribble {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.at >= self.data.len() || out.is_empty() {
                    return Ok(0);
                }
                out[0] = self.data[self.at];
                self.at += 1;
                Ok(1)
            }
        }
        let mut reader = FrameReader::new(Dribble { data: stream, at: 0 });
        assert_eq!(reader.next_body().unwrap().unwrap(), r#"{"a":1}"#);
        assert_eq!(reader.next_body().unwrap().unwrap(), r#"{"b":2}"#);
        assert_eq!(reader.next_body().unwrap(), None);
    }

    #[test]
    fn read_frame_reports_truncated_body_as_evidence() {
        let cursor = Cursor::new(b"Content-Length: 100\r\n\r\n{\"a\"".to_vec());
        let err = read_frame(cursor).unwrap_err();
        assert!(matches!(err, FrameError::UnexpectedEof { .. }), "{err:?}");
    }

    #[test]
    fn read_frame_rejects_a_body_that_is_not_utf8() {
        let mut bytes = b"Content-Length: 2\r\n\r\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        let err = read_frame(Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, FrameError::NotUtf8 { .. }), "{err:?}");
    }

    /// The frames gdb 16.3 actually sent during the P4 probe, replayed to pin
    /// the parser against real adapter output rather than against our own
    /// encoder.  The `initialize` body below is byte-for-byte what
    /// `gdb -q -i=dap` returned.
    #[test]
    fn parses_real_gdb_16_3_initialize_frame() {
        let body = r#"{"request_seq": 1, "type": "response", "command": "initialize", "success": true, "body": {"supportsTerminateRequest": true, "supportsReadMemoryRequest": true, "supportsWriteMemoryRequest": true, "supportsSteppingGranularity": true}}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let (decoded, used) = decode_frame(frame.as_bytes()).unwrap().unwrap();
        assert_eq!(decoded, body);
        assert_eq!(used, frame.len());
        let value: serde_json::Value = serde_json::from_str(decoded).unwrap();
        assert_eq!(value["command"], "initialize");
        assert_eq!(value["body"]["supportsReadMemoryRequest"], true);
    }
}
