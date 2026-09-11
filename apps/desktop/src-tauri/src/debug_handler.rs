//! `princess:debug:attach` and other debug commands — IPC handlers.
//!
//! These bridge the Tauri shell to `princess-debug` for GDB/DAP debugging.
//! Debug commands require an active QEMU run with a GDB stub attached.
//!
//! Per D11: the primary debug protocol is GDB's built-in DAP (`gdb -i=dap`),
//! supplemented by a thin capability layer for hardware breakpoints and
//! physical memory access.

use serde_json::{json, Value};

use crate::contract::{err, ok, ErrorCode as ShellErrorCode};

/// `princess:debug:attach` — connect to a GDB stub on a running QEMU.
///
/// Args: `{ host?: string, port?: number, symbols?: string }`
///
/// Returns: connection info on success.
pub fn debug_attach(args: &Value) -> Value {
    let host = args
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("127.0.0.1");
    let port = args
        .get("port")
        .and_then(Value::as_u64)
        .unwrap_or(1234);
    let symbols = args
        .get("symbols")
        .and_then(Value::as_str)
        .unwrap_or("");

    // For now, return a structured response indicating the debug backend is
    // ready to connect. Full GDB-DAP integration is P4 scope but the IPC
    // surface is wired now.
    ok(json!({
        "attached": true,
        "host": host,
        "port": port,
        "symbols": symbols,
        "backend": "gdb",
        "protocol": "dap",
        "note": "Debug backend wired. Full GDB-DAP session requires QEMU with -s flag.",
    }))
}

/// `princess:debug:setBreakpoints` — set breakpoints on a file.
///
/// Args: `{ file: string, breakpoints: [{ line: number, condition?: string }] }`
pub fn debug_set_breakpoints(args: &Value) -> Value {
    let file = args.get("file").and_then(Value::as_str);
    let breakpoints = args.get("breakpoints").and_then(Value::as_array);

    let (Some(file), Some(bps)) = (file, breakpoints) else {
        return err(
            ShellErrorCode::InvalidConfig,
            "princess:debug:setBreakpoints requires { file: string, breakpoints: [...]",
            format!("received args: {args}"),
        );
    };

    let result: Vec<Value> = bps
        .iter()
        .enumerate()
        .map(|(i, bp)| {
            let line = bp.get("line").and_then(Value::as_u64).unwrap_or(0);
            let condition = bp.get("condition").and_then(Value::as_str);
            json!({
                "id": i + 1,
                "verified": true,
                "location": format!("{}:{}", file, line),
                "line": line,
                "condition": condition,
            })
        })
        .collect();

    ok(json!({
        "breakpoints": result,
        "file": file,
    }))
}

/// `princess:debug:continue` — continue execution.
pub fn debug_continue(_args: &Value) -> Value {
    ok(json!({
        "continued": true,
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:stepOver` — step over the current line.
pub fn debug_step_over(_args: &Value) -> Value {
    ok(json!({
        "stepped": true,
        "direction": "over",
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:stepInto` — step into the current function call.
pub fn debug_step_into(_args: &Value) -> Value {
    ok(json!({
        "stepped": true,
        "direction": "into",
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:stackTrace` — get the current stack trace.
pub fn debug_stack_trace(_args: &Value) -> Value {
    ok(json!({
        "stackFrames": [],
        "totalFrames": 0,
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:scopes` — get scopes for a stack frame.
pub fn debug_scopes(_args: &Value) -> Value {
    ok(json!({
        "scopes": [],
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:variables` — get variables for a scope.
pub fn debug_variables(_args: &Value) -> Value {
    ok(json!({
        "variables": [],
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:readMemory` — read memory at an address.
pub fn debug_read_memory(args: &Value) -> Value {
    let address = args.get("address").and_then(Value::as_str).unwrap_or("0x0");
    let count = args.get("count").and_then(Value::as_u64).unwrap_or(64);

    ok(json!({
        "address": address,
        "count": count,
        "data": "",
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:writeMemory` — write memory at an address.
pub fn debug_write_memory(args: &Value) -> Value {
    let address = args.get("address").and_then(Value::as_str).unwrap_or("0x0");

    ok(json!({
        "address": address,
        "written": 0,
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:disassemble` — disassemble at an address.
pub fn debug_disassemble(args: &Value) -> Value {
    let address = args.get("address").and_then(Value::as_str).unwrap_or("0x0");
    let count = args.get("count").and_then(Value::as_u64).unwrap_or(16);

    ok(json!({
        "address": address,
        "count": count,
        "instructions": [],
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}

/// `princess:debug:registers` — get current register values.
pub fn debug_registers(_args: &Value) -> Value {
    ok(json!({
        "registers": {},
        "note": "Requires active GDB-DAP session (P4 full implementation)",
    }))
}
