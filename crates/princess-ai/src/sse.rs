//! Server-Sent Events parsing for the OpenAI streaming dialect.
//!
//! An OpenAI-compatible stream is a sequence of SSE events, each one
//! `data: <json>` followed by a blank line, terminated by `data: [DONE]`.  This
//! module is a **pure function of bytes**: it owns no socket, so every rule here
//! is unit-testable without a server.
//!
//! Two details matter and are handled deliberately:
//!
//! * **A frame may be split across TCP reads.**  `SseDecoder` buffers until it
//!   sees the blank-line terminator, so a `data:` line cut in half (or a
//!   multi-byte UTF-8 character cut in half, which is the same problem one level
//!   down) never produces a partial chunk.  This is the "engine guarantees it
//!   never splits a UTF-8 code point" rule from `10-contracts.md` §2 applied to
//!   the provider path.
//! * **Only `data:` lines carry payload.**  `event:`, `id:`, `retry:` and
//!   comment lines (`: keep-alive`) are recognised and ignored, as the SSE
//!   specification requires, instead of being reported as malformed JSON.

use serde::Deserialize;

/// One decoded event from the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// A `data:` payload that was not the `[DONE]` sentinel.
    Data(String),
    /// The `data: [DONE]` sentinel: the stream is complete.
    Done,
}

/// Incremental decoder: feed it bytes, get whole events back.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Raw bytes not yet terminated by a blank line.  Kept as bytes, not as a
    /// `String`, so a multi-byte character split across two reads survives.
    buffer: Vec<u8>,
    done: bool,
}

/// What the OpenAI dialect calls the end of a stream.
pub const DONE_SENTINEL: &str = "[DONE]";

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once `data: [DONE]` has been seen: whatever arrives afterwards is
    /// not part of this completion.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Feed one TCP chunk; returns every complete event it completed.
    ///
    /// Once `data: [DONE]` has been seen the decoder is closed: a provider that
    /// keeps talking after it declared the stream finished cannot inject more
    /// events into this completion.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        if self.done {
            return Vec::new();
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            // A frame ends at a blank line.  Accept LF, CRLF and (defensively)
            // CR-only line endings.
            let Some((frame_end, next_start)) = find_frame_end(&self.buffer) else {
                break;
            };
            let frame: Vec<u8> = self.buffer[..frame_end].to_vec();
            self.buffer.drain(..next_start);
            if let Some(event) = decode_frame(&frame) {
                if event == SseEvent::Done {
                    self.done = true;
                }
                out.push(event);
            }
            if self.done {
                self.buffer.clear();
                break;
            }
        }
        out
    }

    /// Flush a stream that ended without a trailing blank line.  Providers that
    /// die mid-frame are the common case here; whatever complete `data:` line
    /// was accumulated is still returned, and the caller decides whether the
    /// missing `[DONE]` matters.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let frame: Vec<u8> = std::mem::take(&mut self.buffer);
        decode_frame(&frame).into_iter().collect()
    }
}

/// `Some((frame_end, next_start))` for the first complete frame in `bytes`.
fn find_frame_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                // "\n\n" or "\n\r\n"
                if bytes.get(i + 1) == Some(&b'\n') {
                    return Some((i, i + 2));
                }
                if bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
                    return Some((i, i + 3));
                }
            }
            b'\r' => {
                // "\r\n\r\n" or "\r\r"
                if bytes.get(i + 1) == Some(&b'\n') && bytes.get(i + 2) == Some(&b'\r') {
                    if bytes.get(i + 3) == Some(&b'\n') {
                        return Some((i, i + 4));
                    }
                } else if bytes.get(i + 1) == Some(&b'\r') {
                    return Some((i, i + 2));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Turn one frame's bytes into an event, or `None` for frames that carry no
/// payload (comments, `event:`/`id:`/`retry:` only).
fn decode_frame(frame: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(frame);
    let mut data: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            // Blank separator or a comment / keep-alive.
            continue;
        }
        let Some(rest) = line.strip_prefix("data:") else {
            // `event:`, `id:`, `retry:` — informative for other clients, not
            // for us; ignoring them is what the SSE spec asks for.
            continue;
        };
        // Exactly one optional leading space is part of the SSE framing.
        data.push(rest.strip_prefix(' ').unwrap_or(rest));
    }
    if data.is_empty() {
        return None;
    }
    let payload = data.join("\n");
    if payload.trim() == DONE_SENTINEL {
        return Some(SseEvent::Done);
    }
    Some(SseEvent::Data(payload))
}

// ------------------------------------------------------------ stream objects ---

/// The `choices[].delta` half of a streaming chunk.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkDelta {
    #[serde(default)]
    pub content: Option<String>,
    /// Reasoning models stream their scratchpad here; surfaced separately so a
    /// future UI can grey it out rather than mixing it into the answer.
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

/// One `choices[]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub delta: Option<ChunkDelta>,
    /// Some servers put the final text here instead of in `delta`.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// One streamed `chat.completion.chunk`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatCompletionChunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    /// Present on the last chunk when the server supports
    /// `stream_options: {"include_usage": true}`.
    #[serde(default)]
    pub usage: Option<StreamUsage>,
}

/// Token accounting as the provider reports it.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct StreamUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default, rename = "total_tokens")]
    pub total_tokens_field: u64,
}

impl StreamUsage {
    /// `total_tokens` when the provider sent it, else the sum of the two halves.
    /// Never invents a number: a provider that reports nothing yields zeros.
    pub fn total(&self) -> u64 {
        if self.total_tokens_field != 0 {
            self.total_tokens_field
        } else {
            self.prompt_tokens + self.completion_tokens
        }
    }

    pub fn to_ai_usage(self) -> princess_core::types::AiUsage {
        princess_core::types::AiUsage {
            promptTokens: self.prompt_tokens,
            completionTokens: self.completion_tokens,
            totalTokens: self.total(),
        }
    }
}

/// The text (if any) one chunk contributes to the answer.
///
/// A chunk that only carries a role, a `finish_reason` or usage contributes
/// nothing and must not produce an empty `ai.chunk` event.
pub fn chunk_text(chunk: &ChatCompletionChunk) -> Option<String> {
    let mut out = String::new();
    for choice in &chunk.choices {
        if let Some(delta) = &choice.delta {
            if let Some(content) = &delta.content {
                out.push_str(content);
            }
        }
        if let Some(text) = &choice.text {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Parse one `data:` payload into a chunk.
pub fn parse_chunk(payload: &str) -> std::result::Result<ChatCompletionChunk, serde_json::Error> {
    serde_json::from_str(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_of(events: &[SseEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                SseEvent::Data(d) => Some(d.clone()),
                SseEvent::Done => None,
            })
            .collect()
    }

    #[test]
    fn decodes_a_plain_stream_terminated_by_done() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
              data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
              data: [DONE]\n\n",
        );
        assert_eq!(data_of(&events).len(), 2);
        assert_eq!(events.last(), Some(&SseEvent::Done));
        assert!(decoder.is_done());
    }

    #[test]
    fn a_frame_split_across_reads_is_reassembled_exactly_once() {
        let mut decoder = SseDecoder::new();
        let whole = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
        // Byte-at-a-time is the worst case a TCP stream can produce.
        let mut seen = Vec::new();
        for byte in whole {
            seen.extend(decoder.push(&[*byte]));
        }
        assert_eq!(seen.len(), 1, "one complete frame in, one event out");
        assert_eq!(data_of(&seen)[0], "{\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}");
    }

    #[test]
    fn a_multibyte_character_split_across_reads_is_not_corrupted() {
        let mut decoder = SseDecoder::new();
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"世界\"}}]}\n\n".as_bytes().to_vec();
        // Split in the middle of the first multi-byte character.
        let split = frame.iter().position(|b| *b >= 0x80).unwrap() + 1;
        assert!(decoder.push(&frame[..split]).is_empty());
        let events = decoder.push(&frame[split..]);
        assert_eq!(data_of(&events)[0].contains("世界"), true);
        let chunk = parse_chunk(&data_of(&events)[0]).unwrap();
        assert_eq!(chunk_text(&chunk).as_deref(), Some("世界"));
    }

    #[test]
    fn comments_and_metadata_lines_are_ignored_not_reported_as_json() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(
            b": keep-alive\n\n\
              event: message\nid: 42\nretry: 100\ndata: {\"choices\":[]}\n\n\
              data: [DONE]\n\n",
        );
        assert_eq!(events, vec![SseEvent::Data("{\"choices\":[]}".into()), SseEvent::Done]);
    }

    #[test]
    fn crlf_framing_is_accepted() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: {\"choices\":[]}\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(events, vec![SseEvent::Data("{\"choices\":[]}".into()), SseEvent::Done]);
    }

    #[test]
    fn everything_after_done_is_discarded() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n");
        assert_eq!(events, vec![SseEvent::Done]);
        assert!(decoder.push(b"data: {\"more\":1}\n\n").is_empty());
    }

    #[test]
    fn finish_flushes_a_stream_that_died_mid_frame() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"cut\"}}]}").is_empty());
        let tail = decoder.finish();
        assert_eq!(data_of(&tail).len(), 1);
        assert!(!decoder.is_done(), "no [DONE] was seen; the caller must notice");
    }

    #[test]
    fn chunk_text_collects_deltas_and_skips_empty_chunks() {
        let role_only: ChatCompletionChunk =
            parse_chunk(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).unwrap();
        assert_eq!(chunk_text(&role_only), None);

        let finish: ChatCompletionChunk =
            parse_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#).unwrap();
        assert_eq!(chunk_text(&finish), None);

        let text: ChatCompletionChunk =
            parse_chunk(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).unwrap();
        assert_eq!(chunk_text(&text).as_deref(), Some("hi"));
    }

    #[test]
    fn empty_content_is_not_a_chunk() {
        let empty_delta: ChatCompletionChunk =
            parse_chunk(r#"{"choices":[{"delta":{"content":""}}]}"#).unwrap();
        assert_eq!(chunk_text(&empty_delta), None);
    }

    #[test]
    fn usage_totals_are_taken_or_summed_but_never_invented() {
        let given = StreamUsage {
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens_field: 7,
        };
        assert_eq!(given.total(), 7);
        let summed = StreamUsage {
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens_field: 0,
        };
        assert_eq!(summed.total(), 7);
        assert_eq!(StreamUsage::default().total(), 0);
    }

    #[test]
    fn an_error_object_is_parseable_for_its_message() {
        // Not a chunk shape, but serde must not panic: the caller reports the
        // raw body as `detail` either way.
        let value: serde_json::Value =
            serde_json::from_str(r#"{"error":{"message":"bad key","type":"auth"}}"#).unwrap();
        assert_eq!(value["error"]["message"], "bad key");
    }
}
