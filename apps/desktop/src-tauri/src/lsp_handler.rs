//! `princess:lsp:start` / `princess:lsp:send` / `princess:lsp:stop` — IPC handlers.
//!
//! Per D17: the engine owns the clangd process lifecycle and JSON-RPC framing.
//! The frontend only does editor interaction over the `lsp.message` /
//! `lsp.stopped` event stream.
//!
//! The LSP server is started as a child process with stdio transport.
//! The engine frames outgoing messages with `Content-Length` headers and
//! strips incoming frames before emitting `lsp.message` events.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};


use crate::contract::{err, ok, ErrorCode as ShellErrorCode};
use crate::events::{EventBus, EVENT_CHANNEL};

/// An active LSP server instance.
struct LspInstance {
    child: Child,
    server_id: String,
    stdin: std::process::ChildStdin,
}

/// Registry of active LSP servers, shared across handlers.
pub struct LspRegistry {
    instances: Mutex<HashMap<String, LspInstance>>,
    counter: std::sync::atomic::AtomicU64,
}

impl LspRegistry {
    pub fn new() -> Self {
        Self {
            instances: Mutex::new(HashMap::new()),
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

/// `princess:lsp:start` — start a language server for a project.
///
/// Args: `{ projectRoot: string }`
///
/// Returns: `{ serverId, command, args }`
///
/// Per D17: The engine owns the language server process lifecycle.
/// Per D31: Language modules are pluggable; Java uses jdtls, C uses clangd-16.
pub fn lsp_start(
    args: &Value,
    app: &AppHandle,
    bus: &std::sync::Arc<EventBus>,
    registry: &LspRegistry,
) -> Value {
    let project_root = args
        .get("projectRoot")
        .and_then(Value::as_str)
        .unwrap_or(".");

    let root = std::path::PathBuf::from(project_root);
    if !root.is_dir() {
        return err(
            ShellErrorCode::InvalidConfig,
            format!("project root {} does not exist", root.display()),
            String::new(),
        );
    }

    // D31: Detect language from project manifest to choose the right language server
    let language = detect_language(&root);
    let (server_bin, server_args) = match language.as_str() {
        "java" => {
            // D31/P-F1: Java uses jdtls
            let jdtls_bin = find_jdtls();
            let args_vec = vec![]; // jdtls doesn't need special args for basic operation
            (jdtls_bin, args_vec)
        }
        _ => {
            // D18: Default to clangd-16 for C/C++ projects
            let clangd_bin = find_clangd();
            let args_vec = vec![
                format!("--compile-commands-dir={}", root.display()),
                format!("--resource-dir={}", root.display()),
                "--pch-storage=memory".to_string(),
                "--log=error".to_string(),
                "-j=2".to_string(),
            ];
            (clangd_bin, args_vec)
        }
    };

    let server_id = {
        let n = registry.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        format!("lsp-{:04x}", n)
    };

    let cmd_display = format!("{} {}", server_bin, server_args.join(" "));

    // Spawn the language server process.
    let mut child = match Command::new(&server_bin)
        .args(&server_args)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return err(
                ShellErrorCode::Internal,
                format!("failed to start {}: {e}", server_bin),
                format!("command: {cmd_display}\nproject root: {}", root.display()),
            );
        }
    };

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let sid = server_id.clone();

    // Register the instance.
    registry.instances.lock().unwrap().insert(
        server_id.clone(),
        LspInstance {
            child,
            server_id: server_id.clone(),
            stdin,
        },
    );

    // Spawn a reader thread for stdout (JSON-RPC responses).
    let app_clone = app.clone();
    let bus_clone = bus.clone();
    let sid_clone = sid.clone();
    std::thread::spawn(move || {
        read_lsp_stream(stdout, &app_clone, &bus_clone, &sid_clone, "stdout");
    });

    // Spawn a reader thread for stderr (diagnostics).
    let app_clone = app.clone();
    let bus_clone = bus.clone();
    let sid_clone = sid.clone();
    std::thread::spawn(move || {
        read_lsp_stream(stderr, &app_clone, &bus_clone, &sid_clone, "stderr");
    });

    // Emit a note about the LSP server starting.
    let note = bus.log_append(
        "ide",
        &format!("princess:lsp:start → {cmd_display} (serverId={server_id})\n"),
        None,
    );
    let _ = app.emit(EVENT_CHANNEL, note);

    ok(json!({
        "serverId": server_id,
        "command": server_bin,
        "args": server_args,
    }))
}

/// `princess:lsp:send` — send a JSON-RPC message to a language server.
///
/// Args: `{ serverId: string, message: string }`
///
/// `message` is the raw JSON-RPC text without frame headers — the engine
/// adds `Content-Length` framing (D17 / contract §2).
pub fn lsp_send(
    args: &Value,
    registry: &LspRegistry,
) -> Value {
    let server_id = args.get("serverId").and_then(Value::as_str);
    let message = args.get("message").and_then(Value::as_str);

    let (Some(sid), Some(msg)) = (server_id, message) else {
        return err(
            ShellErrorCode::InvalidConfig,
            "princess:lsp:send requires { serverId: string, message: string }",
            format!("received args: {args}"),
        );
    };

    let mut instances = registry.instances.lock().unwrap();
    let Some(instance) = instances.get_mut(sid) else {
        return err(
            ShellErrorCode::NotFound,
            format!("no LSP server named {sid}"),
            format!("active servers: {}", instances.keys().cloned().collect::<Vec<_>>().join(", ")),
        );
    };

    // Frame the message: `Content-Length: N\r\n\r\n<body>`
    let header = format!("Content-Length: {}\r\n\r\n", msg.len());
    let write_result = instance.stdin.write_all(header.as_bytes())
        .and_then(|_| instance.stdin.write_all(msg.as_bytes()))
        .and_then(|_| instance.stdin.flush());

    match write_result {
        Ok(()) => ok(json!({})),
        Err(e) => err(
            ShellErrorCode::Internal,
            format!("failed to write to LSP server {sid}"),
            e.to_string(),
        ),
    }
}

/// `princess:lsp:stop` — stop a language server.
///
/// Args: `{ serverId: string }`
pub fn lsp_stop(
    args: &Value,
    app: &AppHandle,
    bus: &std::sync::Arc<EventBus>,
    registry: &LspRegistry,
) -> Value {
    let server_id = args.get("serverId").and_then(Value::as_str);
    let Some(sid) = server_id else {
        return err(
            ShellErrorCode::InvalidConfig,
            "princess:lsp:stop requires { serverId: string }",
            format!("received args: {args}"),
        );
    };

    let mut instances = registry.instances.lock().unwrap();
    let Some(mut instance) = instances.remove(sid) else {
        return err(
            ShellErrorCode::NotFound,
            format!("no LSP server named {sid}"),
            format!("active servers: {}", instances.keys().cloned().collect::<Vec<_>>().join(", ")),
        );
    };

    // Try to send a shutdown request before killing.
    let shutdown_msg = r#"{"jsonrpc":"2.0","id":0,"method":"shutdown"}"#;
    let header = format!("Content-Length: {}\r\n\r\n", shutdown_msg.len());
    let _ = instance.stdin.write_all(header.as_bytes());
    let _ = instance.stdin.write_all(shutdown_msg.as_bytes());
    let _ = instance.stdin.flush();

    // Give it a moment, then kill.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let _ = instance.child.kill();
    let _ = instance.child.wait();

    // Emit lsp.stopped event.
    let envelope = bus.envelope(
        "lsp.stopped",
        None,
        json!({ "serverId": sid, "reason": "shutdown" }),
    );
    let _ = app.emit(EVENT_CHANNEL, envelope);

    let note = bus.log_append("ide", &format!("princess:lsp:stop → {sid}\n"), None);
    let _ = app.emit(EVENT_CHANNEL, note);

    ok(json!({}))
}

/// Find the clangd-16 binary. Per D18: must use the versioned name.
fn find_clangd() -> String {
    // Check env var first.
    if let Ok(bin) = std::env::var("PRINCESSIDE_LANG_SERVICE_CLANGD") {
        return bin;
    }
    // Check the toolchain launcher dir.
    if let Some(bin) = princess_build::launcher_dir() {
        let candidate = bin.parent().unwrap_or(&bin).join("prefix/usr/bin/clangd-16");
        if candidate.is_file() {
            return candidate.display().to_string();
        }
    }
    // Fallback to PATH.
    "clangd-16".to_string()
}

/// Find the jdtls binary for Java language support (D31/P-F1).
fn find_jdtls() -> String {
    // Check env var first.
    if let Ok(bin) = std::env::var("PRINCESSIDE_LANG_SERVICE_JDTLS") {
        return bin;
    }
    // Check the toolchain launcher dir.
    if let Some(bin) = princess_build::launcher_dir() {
        let candidate = bin.parent().unwrap_or(&bin).join("bin/jdtls");
        if candidate.is_file() {
            return candidate.display().to_string();
        }
    }
    // Fallback to PATH.
    "jdtls".to_string()
}

/// Detect the project language from princess.toml (D31).
/// Returns "c" as default if no manifest or language field found.
fn detect_language(project_root: &std::path::Path) -> String {
    let manifest_path = project_root.join("princess.toml");
    if !manifest_path.is_file() {
        return "c".to_string(); // Default to C for projects without manifest
    }

    // Read and parse the manifest
    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(_) => return "c".to_string(),
    };

    // Simple TOML parsing for language field
    // Look for: language = "java" or language = "c"
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("language") && trimmed.contains('=') {
            if let Some(value) = trimmed.split('=').nth(1) {
                let value = value.trim().trim_matches('"').trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
    }

    "c".to_string() // Default
}

/// Read an LSP stream (stdout or stderr) and emit events.
///
/// For stdout: reads Content-Length framed JSON-RPC messages, strips the
/// frame, and emits `lsp.message` events.
/// For stderr: emits `debug.output` or `log.append` events.
fn read_lsp_stream(
    stream: impl Read + Send + 'static,
    app: &AppHandle,
    bus: &std::sync::Arc<EventBus>,
    server_id: &str,
    pipe: &str,
) {
    let sid = server_id.to_string();
    let app = app.clone();
    let bus = bus.clone();

    if pipe == "stdout" {
        // Read Content-Length framed messages.
        let mut reader = BufReader::new(stream);
        loop {
            // Read headers until we find Content-Length.
            let mut content_length = None;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => return, // EOF
                    Ok(_) => {}
                    Err(_) => return,
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    break; // End of headers
                }
                if let Some(val) = trimmed.strip_prefix("Content-Length:") {
                    if let Ok(n) = val.trim().parse::<usize>() {
                        content_length = Some(n);
                    }
                }
            }

            let Some(len) = content_length else { continue };

            // Read the body.
            let mut body = vec![0u8; len];
            match reader.read_exact(&mut body) {
                Ok(()) => {}
                Err(_) => return,
            }

            let message = String::from_utf8_lossy(&body).to_string();
            let envelope = bus.envelope(
                "lsp.message",
                None,
                json!({ "serverId": sid, "message": message }),
            );
            let _ = app.emit(EVENT_CHANNEL, envelope);
        }
    } else {
        // stderr: just log it.
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let envelope = bus.log_append("ide", &format!("[clangd stderr] {line}\n"), None);
            let _ = app.emit(EVENT_CHANNEL, envelope);
        }
    }
}
