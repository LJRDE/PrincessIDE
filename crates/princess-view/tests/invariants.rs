//! Invariant tests for the view model layer.
//!
//! These tests verify the hard invariants that both frontends rely on.

use princess_view::*;
use princess_core::*;
use chrono::{TimeZone, Utc};

/// Helper: create a minimal event for testing.
fn make_log_event(seq: u64, stream: LogStream, chunk: &str) -> Event {
    Event::new(
        seq,
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        Some("op-test".into()),
        EventBody::LogAppend(LogAppendPayload {
            stream,
            chunk: chunk.into(),
            encoding: TextEncoding::Utf8,
        }),
    )
}

// =============================================================================
// EventLog invariants
// =============================================================================

#[test]
fn eventlog_total_rows_matches_actual_count() {
    let events: Vec<_> = (1..=100)
        .map(|i| make_log_event(i, LogStream::Ide, &format!("line {i}\n")))
        .collect();
    let model = EventLogBuilder::new(&events).build();
    assert_eq!(model.total_rows, 100);
    assert_eq!(model.rows.len(), 100);
}

#[test]
fn eventlog_total_rows_matches_with_pagination() {
    let events: Vec<_> = (1..=50)
        .map(|i| make_log_event(i, LogStream::Ide, &format!("line {i}\n")))
        .collect();
    let model = EventLogBuilder::new(&events).offset(10).limit(5).build();
    assert_eq!(model.total_rows, 50);
    assert_eq!(model.rows.len(), 5);
    assert_eq!(model.offset, 10);
}

#[test]
fn eventlog_empty_input_yields_empty_model() {
    let model = EventLogBuilder::new(&[]).build();
    assert_eq!(model.total_rows, 0);
    assert_eq!(model.rows.len(), 0);
    assert_eq!(model.offset, 0);
}

#[test]
fn eventlog_seq_is_monotonic() {
    let events: Vec<_> = (1..=20)
        .map(|i| make_log_event(i, LogStream::Ide, "x\n"))
        .collect();
    let model = EventLogBuilder::new(&events).build();
    for i in 1..model.rows.len() {
        assert!(model.rows[i].seq > model.rows[i - 1].seq);
    }
}

// =============================================================================
// Source invariants
// =============================================================================

#[test]
fn source_line_numbers_start_at_1_and_increment() {
    let content = "line1\nline2\nline3\nline4\nline5\n";
    let model = SourceBuilder::new("test.c").content(content).build();
    for (i, row) in model.rows.iter().enumerate() {
        assert_eq!(row.line_no, (i + 1) as u32);
    }
}

#[test]
fn source_empty_content_yields_no_rows() {
    let model = SourceBuilder::new("empty.c").content("").build();
    assert_eq!(model.rows.len(), 0);
}

#[test]
fn source_diagnostics_attached_to_correct_lines() {
    let content = "int x;\nreturn 0\n";
    let diags = vec![
        DiagnosticInput {
            line: 2,
            col: Some(1),
            severity: DiagnosticSeverity::Error,
            message: "missing ';'".into(),
        },
    ];
    let model = SourceBuilder::new("test.c")
        .content(content)
        .diagnostics(&diags)
        .build();
    assert_eq!(model.rows[0].diagnostics.len(), 0);
    assert_eq!(model.rows[1].diagnostics.len(), 1);
    assert_eq!(model.rows[1].diagnostics[0].message, "missing ';'");
}

#[test]
fn source_diagnostics_sorted_by_column() {
    let content = "x\n";
    let diags = vec![
        DiagnosticInput { line: 1, col: Some(10), severity: DiagnosticSeverity::Error, message: "second".into() },
        DiagnosticInput { line: 1, col: Some(2), severity: DiagnosticSeverity::Warning, message: "first".into() },
    ];
    let model = SourceBuilder::new("t.c").content(content).diagnostics(&diags).build();
    assert_eq!(model.rows[0].diagnostics[0].col, Some(2));
    assert_eq!(model.rows[0].diagnostics[1].col, Some(10));
}

// =============================================================================
// Hex invariants
// =============================================================================

#[test]
fn hex_addresses_are_contiguous() {
    let data: Vec<u8> = (0..80).collect();
    let model = HexBuilder::new(&data).base_addr(0x1000).build();
    for i in 0..model.rows.len() - 1 {
        assert_eq!(
            model.rows[i + 1].addr,
            model.rows[i].addr + 16,
            "row {} addr={:#x}, row {} addr={:#x}",
            i, model.rows[i].addr,
            i + 1, model.rows[i + 1].addr,
        );
    }
}

#[test]
fn hex_last_row_may_be_shorter() {
    let data: Vec<u8> = (0..20).collect();
    let model = HexBuilder::new(&data).build();
    assert_eq!(model.rows.len(), 2);
    assert_eq!(model.rows[0].bytes.len(), 16);
    assert_eq!(model.rows[1].bytes.len(), 4);
}

#[test]
fn hex_ascii_length_matches_bytes() {
    let data: Vec<u8> = (0..48).collect();
    let model = HexBuilder::new(&data).build();
    for row in &model.rows {
        assert_eq!(row.ascii.len(), row.bytes.len());
    }
}

#[test]
fn hex_empty_data_yields_empty_model() {
    let model = HexBuilder::new(&[]).build();
    assert_eq!(model.rows.len(), 0);
    assert_eq!(model.total_size, 0);
}

#[test]
fn hex_printable_ascii_rendered_verbatim() {
    let data = b"Hello, World!";
    let model = HexBuilder::new(data).build();
    assert_eq!(model.rows[0].ascii, "Hello, World!");
}

#[test]
fn hex_non_printable_replaced_with_dot() {
    let data = vec![0x00, 0x01, 0x1f, 0x7f, 0x80, 0xff];
    let model = HexBuilder::new(&data).build();
    assert_eq!(model.rows[0].ascii, "......");
}

// =============================================================================
// Disassembly invariants (D20)
// =============================================================================

#[test]
fn disasm_every_row_has_source_fields() {
    let inputs = vec![
        DisassemblyInput {
            addr: 0x1000,
            bytes: vec![0x90],
            mnemonic: "nop".into(),
            operands: String::new(),
            source: Some(SourceLocation { file: "kernel.c".into(), line: 10, column: None }),
            symbol: None,
        },
        DisassemblyInput {
            addr: 0x1001,
            bytes: vec![0xc3],
            mnemonic: "ret".into(),
            operands: String::new(),
            source: None, // No debug info
            symbol: None,
        },
    ];
    let model = DisassemblyBuilder::new(&inputs).build();
    
    // Row with debug info: sourceLine is Some
    assert_eq!(model.rows[0].source_file, Some("kernel.c".into()));
    assert_eq!(model.rows[0].source_line, Some(10));
    
    // Row without debug info: sourceLine is None (explicit null, not missing)
    assert_eq!(model.rows[1].source_file, None);
    assert_eq!(model.rows[1].source_line, None);
}

#[test]
fn disasm_addr_monotonically_increasing() {
    let inputs: Vec<_> = (0..10)
        .map(|i| DisassemblyInput {
            addr: 0x1000 + i * 4,
            bytes: vec![0x90, 0x90, 0x90, 0x90],
            mnemonic: "nop".into(),
            operands: String::new(),
            source: None,
            symbol: None,
        })
        .collect();
    let model = DisassemblyBuilder::new(&inputs).build();
    for i in 1..model.rows.len() {
        assert!(model.rows[i].addr > model.rows[i - 1].addr);
    }
}

#[test]
fn disasm_empty_inputs_yields_empty_model() {
    let model = DisassemblyBuilder::new(&[]).build();
    assert_eq!(model.rows.len(), 0);
    assert_eq!(model.total_instructions, 0);
}

#[test]
fn disasm_source_file_and_line_both_null_together() {
    let inputs = vec![
        DisassemblyInput {
            addr: 0x1000,
            bytes: vec![0x90],
            mnemonic: "nop".into(),
            operands: String::new(),
            source: None,
            symbol: None,
        },
    ];
    let model = DisassemblyBuilder::new(&inputs).build();
    assert_eq!(model.rows[0].source_file.is_none(), model.rows[0].source_line.is_none());
}

// =============================================================================
// Negative samples
// =============================================================================

#[test]
fn negative_empty_event_stream() {
    let model = EventLogBuilder::new(&[]).build();
    assert_eq!(model.total_rows, 0);
    assert_eq!(model.rows.len(), 0);
    // Should serialize without panic
    let json = serde_json::to_string(&model).unwrap();
    assert!(json.contains("\"totalRows\":0"));
}

#[test]
fn negative_out_of_bounds_offset() {
    let events: Vec<_> = (1..=3).map(|i| make_log_event(i, LogStream::Ide, "x\n")).collect();
    let model = EventLogBuilder::new(&events).offset(1000).limit(10).build();
    assert_eq!(model.total_rows, 3);
    assert_eq!(model.rows.len(), 0);
    assert_eq!(model.offset, 3); // clamped to end
}

#[test]
fn negative_zero_limit() {
    let events: Vec<_> = (1..=5).map(|i| make_log_event(i, LogStream::Ide, "x\n")).collect();
    let model = EventLogBuilder::new(&events).offset(0).limit(0).build();
    assert_eq!(model.total_rows, 5);
    assert_eq!(model.rows.len(), 0);
}

#[test]
fn negative_single_byte_hex() {
    let data = vec![0xff];
    let model = HexBuilder::new(&data).build();
    assert_eq!(model.rows.len(), 1);
    assert_eq!(model.rows[0].bytes, vec![0xff]);
    assert_eq!(model.rows[0].ascii, ".");
}

#[test]
fn negative_disasm_empty_bytes() {
    let inputs = vec![
        DisassemblyInput {
            addr: 0x1000,
            bytes: vec![],
            mnemonic: "db".into(),
            operands: "0x00".into(),
            source: None,
            symbol: None,
        },
    ];
    let model = DisassemblyBuilder::new(&inputs).build();
    assert_eq!(model.rows[0].bytes.len(), 0);
    // Should not panic
}

#[test]
fn negative_disasm_truncated_bytes() {
    let inputs = vec![
        DisassemblyInput {
            addr: 0x1000,
            bytes: vec![0x48], // Only 1 byte of a multi-byte instruction
            mnemonic: "mov".into(),
            operands: "rbp, rsp".into(),
            source: None,
            symbol: None,
        },
    ];
    let model = DisassemblyBuilder::new(&inputs).build();
    assert_eq!(model.rows[0].bytes, vec![0x48]);
    // Should not panic — the model faithfully represents what it was given
}

// =============================================================================
// Stable serialization tests
// =============================================================================

#[test]
fn eventlog_serialization_is_stable() {
    let events: Vec<_> = (1..=5).map(|i| make_log_event(i, LogStream::Ide, "test\n")).collect();
    let model = EventLogBuilder::new(&events).build();
    let json1 = serde_json::to_string(&model).unwrap();
    let json2 = serde_json::to_string(&model).unwrap();
    assert_eq!(json1, json2);
}

#[test]
fn source_serialization_is_stable() {
    let model = SourceBuilder::new("test.c").content("line1\nline2\n").build();
    let json1 = serde_json::to_string(&model).unwrap();
    let json2 = serde_json::to_string(&model).unwrap();
    assert_eq!(json1, json2);
}

#[test]
fn hex_serialization_is_stable() {
    let data: Vec<u8> = (0..32).collect();
    let model = HexBuilder::new(&data).build();
    let json1 = serde_json::to_string(&model).unwrap();
    let json2 = serde_json::to_string(&model).unwrap();
    assert_eq!(json1, json2);
}

#[test]
fn disasm_serialization_is_stable() {
    let inputs = vec![
        DisassemblyInput {
            addr: 0x1000,
            bytes: vec![0x90],
            mnemonic: "nop".into(),
            operands: String::new(),
            source: None,
            symbol: None,
        },
    ];
    let model = DisassemblyBuilder::new(&inputs).build();
    let json1 = serde_json::to_string(&model).unwrap();
    let json2 = serde_json::to_string(&model).unwrap();
    assert_eq!(json1, json2);
}
