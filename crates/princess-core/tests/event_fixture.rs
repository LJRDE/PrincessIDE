//! P2-8 — the recorded event fixture must be consumable by `princess-core`.
//!
//! `fixtures/events/refkernel-run.ndjson` is a **real** recording: it was
//! produced by `princess-cli run --project fixtures/refkernel --record …` against
//! the reference kernel, with no hand editing (see `docs/reports/p2-core.md`).
//! P3 replays it to drive the UI state machine, so this test is the contract
//! between the engine's serializer and every future consumer:
//!
//! 1. every line deserialises into a typed [`Event`];
//! 2. `v` is the supported model version and `seq` is strictly increasing;
//! 3. the fixed acceptance strings are present (banner, fault symbol, line);
//! 4. re-serialising each event reproduces the recorded line **byte for byte** —
//!    the strongest available check that the JSON shape has not drifted.

use std::collections::BTreeSet;
use std::path::PathBuf;

use princess_core::types::{ExitReason, LogStream};
use princess_core::{
    parse_ndjson, validate_stream, Event, EventBody, EventKind, EVENT_MODEL_VERSION,
};

/// Fixed acceptance constants (`docs/spec/20-acceptance.md` §0).
const BANNER: &str = "PrincessIDE reference kernel booted";
const FAULT_SYMBOL: &str = "refkernel_fault_probe";
const FAULT_LINE: u32 = 100;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/events/refkernel-run.ndjson")
}

fn load_fixture() -> (String, Vec<Event>) {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "the recorded event fixture {} is missing ({err}); regenerate it with \
             `cargo run -p princess-cli -- run --project fixtures/refkernel --record {}`",
            path.display(),
            path.display()
        )
    });
    let events = parse_ndjson(&text).expect("fixture must deserialise into typed events");
    (text, events)
}

#[test]
fn recorded_stream_deserialises_and_is_monotonic() {
    let (text, events) = load_fixture();
    assert!(!events.is_empty(), "fixture is empty");
    assert_eq!(
        text.lines().filter(|line| !line.trim().is_empty()).count(),
        events.len(),
        "every non-blank line must be one event"
    );

    // seq strictly increasing, model version supported (also checked by
    // `validate_stream`, asserted explicitly here for a readable failure).
    validate_stream(&events).unwrap();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.v, EVENT_MODEL_VERSION, "event {index} has a bad version");
        assert_eq!(event.seq, index as u64 + 1, "seq must start at 1 and be dense");
    }

    // One operation per recording, so every event carries the same opId.
    let op_ids: BTreeSet<&str> = events
        .iter()
        .filter_map(|event| event.op_id.as_deref())
        .collect();
    assert_eq!(op_ids.len(), 1, "expected exactly one opId, got {op_ids:?}");

    // The terminal events are present and last-ish (contract §6 rule 5).
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind()).collect();
    assert!(kinds.contains(&EventKind::BuildStarted));
    assert!(kinds.contains(&EventKind::BuildFinished));
    assert!(kinds.contains(&EventKind::RunStarted));
    assert!(kinds.contains(&EventKind::RunFault));
    assert!(kinds.contains(&EventKind::RunExited));
    let build_finished = kinds.iter().position(|k| *k == EventKind::BuildFinished).unwrap();
    let run_started = kinds.iter().position(|k| *k == EventKind::RunStarted).unwrap();
    assert!(build_finished < run_started, "the build must finish before the run starts");
    assert_eq!(*kinds.last().unwrap(), EventKind::RunExited);
}

#[test]
fn recorded_stream_carries_the_fixed_acceptance_evidence() {
    let (_, events) = load_fixture();

    // P2-2: the build produced the kernel ELF.
    let build_finished = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::BuildFinished(payload) => Some(payload),
            _ => None,
        })
        .expect("fixture must contain build.finished");
    assert_eq!(build_finished.status.as_str(), "ok");
    assert_eq!(build_finished.exit_code, Some(0));
    assert!(
        build_finished
            .artifacts
            .iter()
            .any(|artifact| artifact.path.ends_with("refkernel.elf")),
        "artifacts[] must contain refkernel.elf: {:?}",
        build_finished.artifacts
    );
    for artifact in &build_finished.artifacts {
        assert_eq!(artifact.sha256.len(), 64, "sha256 must be full hex");
        assert!(artifact.size > 0);
    }

    // P2-3: the banner arrives on serial.com1, whole, in one chunk.
    let banner_chunks: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::LogAppend(payload) if payload.stream == LogStream::SerialCom1 => {
                Some(payload.chunk.as_str())
            }
            _ => None,
        })
        .filter(|chunk| chunk.contains(BANNER))
        .collect();
    assert_eq!(banner_chunks.len(), 1, "the banner must appear exactly once, unsplit");
    assert_eq!(banner_chunks[0].trim_end(), BANNER);

    // P2-4: run.fault carries a RIP and a symbolicated location.
    let fault = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::RunFault(payload) => Some(payload),
            _ => None,
        })
        .expect("fixture must contain run.fault");
    assert_eq!(fault.vector, "#UD");
    assert_eq!(fault.rip_text, format!("0x{:016x}", fault.rip));
    assert_ne!(fault.rip, 0);
    assert_eq!(fault.regs.get("RIP").map(String::as_str), Some(fault.rip_text.as_str()));
    let symbolicated = fault.symbolicated.as_ref().expect("fault must be symbolicated");
    assert_eq!(symbolicated.symbol, FAULT_SYMBOL);
    assert_eq!(symbolicated.line, FAULT_LINE);
    assert!(symbolicated.file.ends_with("kernel.c"), "{symbolicated:?}");

    // P2-5: the exit reason is one of the four contract values.
    let exited = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::RunExited(payload) => Some(payload),
            _ => None,
        })
        .expect("fixture must contain run.exited");
    assert!(
        [
            ExitReason::GuestShutdown,
            ExitReason::TripleFault,
            ExitReason::Timeout,
            ExitReason::Killed,
        ]
        .contains(&exited.reason),
        "unknown reason {}",
        exited.reason.as_str()
    );
    assert!(exited.uptime_ms > 0);
}

#[test]
fn recorded_lines_round_trip_byte_for_byte() {
    let (text, events) = load_fixture();
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    for (event, line) in events.iter().zip(lines.iter()) {
        let rendered = serde_json::to_string(event).unwrap();
        assert_eq!(
            &rendered, line,
            "the serialised shape of {} drifted from the recording",
            event.kind()
        );
        // And the NDJSON helper used by the recorder agrees.
        assert_eq!(event.to_ndjson().unwrap(), format!("{line}\n"));
    }
}

#[test]
fn a_tampered_stream_is_rejected() {
    let (text, events) = load_fixture();

    // A dropped sequence number must be caught (that is how the UI detects loss).
    let mut tampered = events.clone();
    tampered[3].seq = tampered[2].seq;
    assert!(validate_stream(&tampered).is_err());

    // An unknown kind must not silently deserialise into something else.
    let broken = text.replacen("\"kind\":\"build.started\"", "\"kind\":\"build.exploded\"", 1);
    let err = parse_ndjson(&broken).unwrap_err();
    assert!(err.message.contains("not a valid PrincessIDE event"), "{err}");
    assert!(err.message.contains("line "), "{err}");

    // A payload of the wrong shape for its kind must fail loudly.
    let broken = text.replacen("\"vector\":\"#UD\"", "\"vector\":5", 1);
    let err = parse_ndjson(&broken).unwrap_err();
    assert!(err.message.contains("run.fault"), "{err}");
}
