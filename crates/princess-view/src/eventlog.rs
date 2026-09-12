//! Event log view model — paginated rows for virtualised scrolling.
//!
//! The event log view consumes the engine's event stream (from `princess-core`)
//! and produces a flat, paginated list of display rows.  The model is designed
//! for virtualised scrolling: the frontend can request a window of rows without
//! materialising the entire stream.
//!
//! # Invariants
//!
//! * `totalRows` equals the number of rows produced from the input events.
//! * `rows.len()` ≤ `totalRows` (the window may be smaller).
//! * `offset` is the index of the first row in `rows` within the full stream.
//! * `seq` is strictly monotonic within the model.
//! * Every row has a `text` field (may be empty string, never missing).

use princess_core::{Event, EventBody};
use serde::Serialize;

/// Severity level for a log row, derived from event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Normal informational output.
    Info,
    /// Warning from a build diagnostic.
    Warning,
    /// Error from a build diagnostic or fault.
    Error,
}

/// One row in the event log view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogRow {
    /// Monotonic sequence number from the event envelope.
    pub seq: u64,
    /// Timestamp in RFC 3339 millisecond format (UTC).
    pub ts: String,
    /// Correlating operation id, or `null`.
    pub op_id: Option<String>,
    /// Which stream this row came from (e.g. "serial.com1", "build", "ide").
    pub stream: String,
    /// The display text for this row.
    pub text: String,
    /// True when the text encoding was lossy (replacement characters were inserted).
    pub lossy: bool,
    /// Optional severity (present for diagnostic/fault events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

/// Paginated event log view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogModel {
    /// The requested rows (a window into the full log).
    pub rows: Vec<EventLogRow>,
    /// Total number of rows across the entire stream.
    pub total_rows: usize,
    /// Index of the first row in `rows` within the full stream.
    pub offset: usize,
}

/// Builder for [`EventLogModel`].
///
/// # Usage
///
/// ```rust
/// use princess_view::EventLogBuilder;
/// use princess_core::Event;
///
/// let events: Vec<Event> = vec![/* ... */];
/// let model = EventLogBuilder::new(&events)
///     .offset(0)
///     .limit(100)
///     .build();
/// ```
pub struct EventLogBuilder<'a> {
    events: &'a [Event],
    offset: usize,
    limit: usize,
}

impl<'a> EventLogBuilder<'a> {
    /// Create a new builder for the given event stream.
    pub fn new(events: &'a [Event]) -> Self {
        Self {
            events,
            offset: 0,
            limit: usize::MAX,
        }
    }

    /// Set the offset (0-based index into the full row list).
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Set the maximum number of rows to return.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Build the view model.
    ///
    /// Returns an empty model with `total_rows = 0` when `events` is empty.
    pub fn build(self) -> EventLogModel {
        let all_rows = Self::events_to_rows(self.events);
        let total_rows = all_rows.len();

        let clamped_offset = self.offset.min(total_rows);
        let end = clamped_offset.saturating_add(self.limit).min(total_rows);
        let rows = if clamped_offset < end {
            all_rows[clamped_offset..end].to_vec()
        } else {
            Vec::new()
        };

        EventLogModel {
            rows,
            total_rows,
            offset: clamped_offset,
        }
    }

    /// Convert events to display rows.
    fn events_to_rows(events: &[Event]) -> Vec<EventLogRow> {
        events
            .iter()
            .map(|event| Self::event_to_row(event))
            .collect()
    }

    /// Convert a single event to a display row.
    fn event_to_row(event: &Event) -> EventLogRow {
        let ts = event
            .ts
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();

        let (text, stream, severity) = match &event.body {
            EventBody::LogAppend(p) => (
                p.chunk.clone(),
                p.stream.as_str().to_string(),
                None,
            ),
            EventBody::BuildStarted(p) => (
                format!("build started: {} {}", p.backend, p.argv.join(" ")),
                "ide".to_string(),
                None,
            ),
            EventBody::BuildDiagnostic(p) => {
                let sev = match p.severity {
                    princess_core::DiagnosticSeverity::Error => Severity::Error,
                    princess_core::DiagnosticSeverity::Warning => Severity::Warning,
                    princess_core::DiagnosticSeverity::Note => Severity::Info,
                };
                (
                    p.message.clone(),
                    "build".to_string(),
                    Some(sev),
                )
            }
            EventBody::BuildFinished(p) => (
                format!("build finished: status={}, exit={:?}, {}ms", 
                    p.status.as_str(), p.exit_code, p.duration_ms),
                "ide".to_string(),
                if p.status == princess_core::BuildStatus::Ok { None } else { Some(Severity::Error) },
            ),
            EventBody::RunStarted(p) => (
                format!("run started: {}", p.qemu_argv.join(" ")),
                "ide".to_string(),
                None,
            ),
            EventBody::RunFault(p) => (
                format!("fault: {} at RIP {}", p.vector, p.rip_text),
                "ide".to_string(),
                Some(Severity::Error),
            ),
            EventBody::RunExited(p) => (
                format!("run exited: reason={}, exit={:?}, uptime={}ms", 
                    p.reason.as_str(), p.exit_code, p.uptime_ms),
                "ide".to_string(),
                None,
            ),
            EventBody::DebugStopped(p) => (
                format!("debug stopped: {:?} thread={}", p.reason, p.thread_id),
                "ide".to_string(),
                None,
            ),
            EventBody::DebugBreakpointChanged(p) => (
                format!("breakpoint {}: verified={}", p.id, p.verified),
                "ide".to_string(),
                None,
            ),
            EventBody::DebugOutput(p) => (
                p.text.clone(),
                "gdb.console".to_string(),
                None,
            ),
            EventBody::SymbolsIndexed(p) => (
                format!("symbols indexed: {} ({} symbols)", p.artifact, p.symbol_count),
                "ide".to_string(),
                None,
            ),
            EventBody::ArtifactChanged(p) => {
                let kind_str = serde_json::to_string(&p.kind).unwrap_or_default();
                let kind_clean = kind_str.trim_matches('"');
                (
                    format!("artifact changed: {} ({})", p.path, kind_clean),
                    "ide".to_string(),
                    None,
                )
            }
            EventBody::AiChunk(p) => (
                p.text.clone(),
                "ai".to_string(),
                None,
            ),
            EventBody::AiFinished(p) => (
                format!("ai finished: {} tokens", p.usage.totalTokens),
                "ide".to_string(),
                None,
            ),
        };

        let lossy = match &event.body {
            EventBody::LogAppend(p) => p.encoding == princess_core::TextEncoding::Utf8Lossy,
            _ => false,
        };

        EventLogRow {
            seq: event.seq,
            ts,
            op_id: event.op_id.clone(),
            stream,
            text,
            lossy,
            severity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use princess_core::{EventBody, EventKind, LogStream};

    fn make_event(seq: u64, kind: EventKind) -> Event {
        let body = match kind {
            EventKind::LogAppend => EventBody::LogAppend(princess_core::LogAppendPayload {
                stream: LogStream::Ide,
                chunk: "test line\n".into(),
                encoding: princess_core::TextEncoding::Utf8,
            }),
            _ => EventBody::LogAppend(princess_core::LogAppendPayload {
                stream: LogStream::Ide,
                chunk: "test\n".into(),
                encoding: princess_core::TextEncoding::Utf8,
            }),
        };
        Event::new(seq, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(), None, body)
    }

    #[test]
    fn empty_events_produces_empty_model() {
        let model = EventLogBuilder::new(&[]).build();
        assert_eq!(model.total_rows, 0);
        assert_eq!(model.rows.len(), 0);
        assert_eq!(model.offset, 0);
    }

    #[test]
    fn total_rows_matches_actual_count() {
        let events: Vec<_> = (1..=5).map(|i| make_event(i, EventKind::LogAppend)).collect();
        let model = EventLogBuilder::new(&events).build();
        assert_eq!(model.total_rows, 5);
        assert_eq!(model.rows.len(), 5);
    }

    #[test]
    fn pagination_respects_offset_and_limit() {
        let events: Vec<_> = (1..=10).map(|i| make_event(i, EventKind::LogAppend)).collect();
        let model = EventLogBuilder::new(&events).offset(3).limit(3).build();
        assert_eq!(model.total_rows, 10);
        assert_eq!(model.offset, 3);
        assert_eq!(model.rows.len(), 3);
        assert_eq!(model.rows[0].seq, 4);
        assert_eq!(model.rows[2].seq, 6);
    }

    #[test]
    fn offset_beyond_end_returns_empty() {
        let events: Vec<_> = (1..=3).map(|i| make_event(i, EventKind::LogAppend)).collect();
        let model = EventLogBuilder::new(&events).offset(100).build();
        assert_eq!(model.total_rows, 3);
        assert_eq!(model.rows.len(), 0);
        assert_eq!(model.offset, 3);
    }
}
