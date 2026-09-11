//! Engine → UI event stream (contract §2).
//!
//! The shell publishes real envelopes: `{v, seq, ts, opId, kind, payload}` with a
//! session-monotonic `seq`.  A bounded ring keeps the recent tail so the UI can
//! ask for `princess:op:replay` after a reconnect and detect gaps itself.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Event-model version carried in every envelope's `v` (contract §2/§7).
pub const EVENT_MODEL_VERSION: u64 = 1;

/// Tauri event channel the UI subscribes to.
pub const EVENT_CHANNEL: &str = "princess:event";

/// How many recent envelopes stay replayable.
pub const RING_CAPACITY: usize = 5000;

pub struct EventBus {
    seq: AtomicU64,
    ring: Mutex<VecDeque<Value>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self { seq: AtomicU64::new(0), ring: Mutex::new(VecDeque::with_capacity(256)) }
    }

    pub fn last_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    /// Build one envelope, record it in the ring and return it (the caller emits it).
    pub fn envelope(&self, kind: &str, op_id: Option<&str>, payload: Value) -> Value {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let env = json!({
            "v": EVENT_MODEL_VERSION,
            "seq": seq,
            "ts": now_iso8601(),
            "opId": op_id,
            "kind": kind,
            "payload": payload,
        });
        let mut ring = self.ring.lock().expect("event ring poisoned");
        if ring.len() == RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(env.clone());
        env
    }

    /// Convenience for `log.append` (contract §2).
    pub fn log_append(&self, stream: &str, chunk: impl Into<String>, op_id: Option<&str>) -> Value {
        self.envelope(
            "log.append",
            op_id,
            json!({ "stream": stream, "chunk": chunk.into(), "encoding": "utf8" }),
        )
    }

    /// Envelopes with `seq >= from_seq`, ascending (backs `princess:op:replay`).
    pub fn since(&self, from_seq: u64) -> Vec<Value> {
        let ring = self.ring.lock().expect("event ring poisoned");
        ring.iter().filter(|e| e["seq"].as_u64().unwrap_or(0) >= from_seq).cloned().collect()
    }

    /// Build an envelope from an engine `EventBody`.
    ///
    /// The engine crate defines `EventBody` (an enum of all v1 event kinds) and
    /// provides `to_ndjson()` on `Event` which produces the contract envelope.
    /// This method uses the engine's own serialization so the shape is always
    /// consistent with the contract.
    pub fn envelope_from_body(&self, body: princess_core::event::EventBody, op_id: Option<&str>) -> Value {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let ts = now_iso8601();
        let event = princess_core::event::Event::new(
            seq,
            parse_timestamp(&ts),
            op_id.map(String::from),
            body,
        );
        let json_str = event.to_ndjson().unwrap_or_default();
        // Parse back to Value so we can record in the ring and emit.
        let env: Value = serde_json::from_str(json_str.trim()).unwrap_or_else(|_| json!({}));
        let mut ring = self.ring.lock().expect("event ring poisoned");
        if ring.len() == RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(env.clone());
        env
    }
}

/// UTC timestamp in the contract's example format: `2026-05-05T12:00:00.123Z`.
///
/// Implemented locally (no chrono dependency) so the shell's dependency tree
/// stays small; this is the same shape `Date.prototype.toISOString()` produces.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60,
        millis
    )
}

/// Howard Hinnant's `civil_from_days` (days since 1970-01-01 → y/m/d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse an ISO 8601 UTC timestamp string into a `chrono::DateTime`.
///
/// The engine's `Event::new` takes a `DateTime<Utc>`, so we need to convert
/// our hand-rolled ISO string to what chrono expects.
fn parse_timestamp(ts: &str) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    // Parse the format "2026-05-05T12:00:00.123Z"
    let date_part = &ts[0..10];
    let time_part = &ts[11..23]; // "12:00:00.123"
    let parts: Vec<&str> = date_part.split('-').collect();
    let year: i32 = parts[0].parse().unwrap_or(2026);
    let month: u32 = parts[1].parse().unwrap_or(1);
    let day: u32 = parts[2].parse().unwrap_or(1);
    let time_parts: Vec<&str> = time_part.split(':').collect();
    let hour: u32 = time_parts[0].parse().unwrap_or(0);
    let min: u32 = time_parts[1].parse().unwrap_or(0);
    let sec_ms: Vec<&str> = time_parts[2].split('.').collect();
    let sec: u32 = sec_ms[0].parse().unwrap_or(0);
    let ms: u32 = sec_ms.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    chrono::Utc.with_ymd_and_hms(year, month, day, hour, min, sec).unwrap()
        + chrono::Duration::milliseconds(ms as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_iso8601_utc() {
        let ts = now_iso8601();
        assert_eq!(ts.len(), 24, "{ts}");
        assert!(ts.ends_with('Z'), "{ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        let year: i32 = ts[0..4].parse().expect("year parses");
        assert!((2024..2100).contains(&year), "implausible year in {ts}");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn seq_is_monotonic_and_replayable() {
        let bus = EventBus::new();
        let a = bus.envelope("log.append", None, json!({"stream":"ide","chunk":"a","encoding":"utf8"}));
        let b = bus.envelope("log.append", Some("op-1"), json!({"stream":"ide","chunk":"b","encoding":"utf8"}));
        assert_eq!(a["seq"], json!(1));
        assert_eq!(b["seq"], json!(2));
        assert_eq!(b["opId"], json!("op-1"));
        assert_eq!(a["v"], json!(EVENT_MODEL_VERSION));

        let replayed = bus.since(2);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0]["seq"], json!(2));
        assert_eq!(bus.since(1).len(), 2);
    }

    #[test]
    fn ring_is_bounded() {
        let bus = EventBus::new();
        // Not RING_CAPACITY iterations (too slow for a unit test); verify the
        // eviction path directly on a small capacity by shrinking the ring.
        for i in 1..=RING_CAPACITY + 3 {
            bus.envelope("log.append", None, json!({ "stream": "ide", "chunk": i.to_string(), "encoding": "utf8" }));
        }
        let all = bus.since(1);
        assert_eq!(all.len(), RING_CAPACITY);
        assert_eq!(all.first().unwrap()["seq"], json!(4));
        assert_eq!(bus.last_seq(), (RING_CAPACITY + 3) as u64);
    }
}
