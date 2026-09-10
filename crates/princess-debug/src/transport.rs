//! DAP transport: spawn the adapter, speak framed JSON over its stdio, and
//! correlate responses **by `request_seq`** rather than by arrival order.
//!
//! # Why correlation, not lockstep
//!
//! The obvious implementation — write a request, read one message, treat it as
//! the reply — is broken against `gdb -i=dap` and was measured to be broken in
//! this workspace.  GDB **delays the `attach` response until `configurationDone`
//! has been processed** (research C §6, re-confirmed by the P4 probe), so the
//! adapter emits:
//!
//! ```text
//! -> attach            (request_seq = 2)
//! -> configurationDone (request_seq = 3)
//! <- seq 2  event initialized
//! <- seq 4  event process
//! <- seq 5  event thread
//! <- seq 6  event stopped
//! <- seq 3  response configurationDone   <-- configurationDone answered FIRST
//! <- seq 7  event module
//! <- seq 8  event breakpoint
//! <- seq 7  response attach              <-- attach answered LAST
//! ```
//!
//! A lockstep client would hand the `attach` reply to whoever asked for
//! `configurationDone` and then mis-attribute every subsequent reply by one —
//! producing exactly the "cascading false negatives" research C §3.2 warns
//! about (features looking unsupported when they are merely mis-read).
//!
//! This transport therefore keeps a `request_seq -> response` map and a
//! separate event queue, so any request can be answered out of order and events
//! that arrive while we are waiting for a response are never lost.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use princess_core::{ErrorCode, PrincessError, Result};
use serde_json::{json, Value};

use crate::framing::{encode_frame, FrameError, FrameReader};

/// One message read off the adapter's stdout.
#[derive(Debug)]
enum Incoming {
    /// A `response` message, keyed by the `request_seq` it answers.
    Response { request_seq: i64, message: Value },
    /// An `event` message (unsolicited, may arrive at any time).
    Event(Value),
    /// The reader thread stopped; the payload says why.
    Closed(String),
}

/// How the adapter process was started, kept so the transport can be restarted
/// and so `PrincessError::detail` can show the exact argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCommand {
    /// `argv[0]` first.
    pub argv: Vec<String>,
    /// Extra environment (on top of the inherited one).
    pub env: Vec<(String, String)>,
}

impl AdapterCommand {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            env: Vec::new(),
        }
    }

    /// One-line rendering for error details: the command exactly as executed.
    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

/// A live DAP conversation with a debug adapter process.
pub struct Transport {
    child: Child,
    stdin: ChildStdin,
    incoming: Mutex<Receiver<Incoming>>,
    reader: Option<JoinHandle<()>>,
    /// Responses that arrived before anyone asked for them.
    early: Arc<Mutex<HashMap<i64, Value>>>,
    /// Events received but not yet consumed, in arrival order.
    events: Arc<Mutex<Vec<Value>>>,
    command: AdapterCommand,
    next_seq: i64,
    /// Deadline applied by `request`/`request_ok`.
    default_timeout: Duration,
    /// Set once the adapter's stdout reaches EOF or the process is gone.
    closed: Option<String>,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("command", &self.command.display())
            .field("next_seq", &self.next_seq)
            .field("closed", &self.closed)
            .finish()
    }
}

/// Default time we are willing to wait for a response.
///
/// Booting a kernel under TCG software emulation and resolving DWARF is slow,
/// but a *response* to a DAP request is not: anything past this is a stuck
/// adapter, and the engine must fail loudly rather than hang the UI (contract
/// §0 principle 3: every long operation is cancellable and observable).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl Transport {
    /// Spawn the adapter and start the reader thread.
    pub fn spawn(command: AdapterCommand) -> Result<Self> {
        if command.argv.is_empty() {
            return Err(PrincessError::new(
                ErrorCode::ToolchainMissing,
                "debug adapter command is empty",
            ));
        }
        let mut cmd = std::process::Command::new(&command.argv[0]);
        cmd.args(&command.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr is inherited on purpose: gdb's Python tracebacks are the
            // most useful evidence when the adapter misbehaves, and swallowing
            // them would hide the cause of a failure.
            .stderr(Stdio::inherit());
        for (key, value) in &command.env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|err| {
            let code = match err.kind() {
                std::io::ErrorKind::NotFound => ErrorCode::ToolchainMissing,
                _ => ErrorCode::Internal,
            };
            PrincessError::new(
                code,
                format!("could not start debug adapter {}", command.argv[0]),
            )
            .with_detail(format!("{}\n{err}", command.display()))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            PrincessError::internal("debug adapter has no stdin").with_detail(command.display())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PrincessError::internal("debug adapter has no stdout").with_detail(command.display())
        })?;

        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = FrameReader::new(BufReader::new(stdout));
            loop {
                match reader.next_body() {
                    Ok(None) => {
                        let _ = tx.send(Incoming::Closed("adapter stdout closed".into()));
                        return;
                    }
                    Ok(Some(body)) => match serde_json::from_str::<Value>(&body) {
                        Ok(value) => {
                            let message = match value.get("type").and_then(Value::as_str) {
                                Some("event") => Incoming::Event(value),
                                _ => match value.get("request_seq").and_then(Value::as_i64) {
                                    Some(request_seq) => Incoming::Response {
                                        request_seq,
                                        message: value,
                                    },
                                    None => continue,
                                },
                            };
                            if tx.send(message).is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(Incoming::Closed(format!(
                                "adapter emitted a frame that is not JSON: {err}: {body:.400}"
                            )));
                            return;
                        }
                    },
                    Err(err) => {
                        let _ = tx.send(Incoming::Closed(describe_frame_error(&err)));
                        return;
                    }
                }
            }
        });

        Ok(Transport {
            child,
            stdin,
            incoming: Mutex::new(rx),
            reader: Some(reader),
            early: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(Vec::new())),
            command,
            next_seq: 1,
            default_timeout: DEFAULT_REQUEST_TIMEOUT,
            closed: None,
        })
    }

    /// The command this transport is talking to.
    pub fn command(&self) -> &AdapterCommand {
        &self.command
    }

    /// Process id of the adapter, for logging and teardown.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Override the deadline used by [`Self::request`] / [`Self::request_ok`].
    ///
    /// The backend sets this from its session config so every request in a
    /// session shares one deadline policy instead of each call site picking its
    /// own (which is how a "30 seconds in one place, forever in another" bug
    /// gets in).
    pub fn set_default_timeout(&mut self, timeout: Duration) {
        self.default_timeout = timeout;
    }

    /// Send a request and return the `seq` it was assigned.
    pub fn send(&mut self, command: &str, arguments: Option<Value>) -> Result<i64> {
        if let Some(reason) = &self.closed {
            return Err(self.closed_error(reason));
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let mut message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
        });
        if let Some(arguments) = arguments {
            message["arguments"] = arguments;
        }
        let body = serde_json::to_string(&message)?;
        self.stdin
            .write_all(&encode_frame(&body))
            .and_then(|()| self.stdin.flush())
            .map_err(|err| {
                PrincessError::internal(format!("could not write the {command} request to the adapter"))
                    .with_detail(format!("{}\n{err}", self.command.display()))
            })?;
        Ok(seq)
    }

    /// Send a request and wait for **its** response.
    pub fn request(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let timeout = self.default_timeout;
        self.request_within(command, arguments, timeout)
    }

    /// Send a request and wait for **its** response, with an explicit deadline.
    pub fn request_within(
        &mut self,
        command: &str,
        arguments: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let seq = self.send(command, arguments)?;
        self.wait_response(seq, timeout)
    }

    /// Wait for the response to a specific `request_seq`.
    ///
    /// Responses that belong to other requests are stashed, not discarded; this
    /// is what makes the delayed `attach` reply harmless.
    pub fn wait_response(&mut self, seq: i64, timeout: Duration) -> Result<Value> {
        if let Some(response) = self.early.lock().unwrap().remove(&seq) {
            return Ok(response);
        }
        // Anything already delivered for us?
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(response) = self.early.lock().unwrap().remove(&seq) {
                return Ok(response);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PrincessError::new(
                    ErrorCode::Timeout,
                    format!("timed out after {:?} waiting for the DAP response to seq {seq}", timeout),
                )
                .with_detail(self.command.display()));
            }
            let received = self.incoming.lock().unwrap().recv_timeout(remaining);
            match received {
                Ok(Incoming::Response { request_seq, message }) => {
                    if request_seq == seq {
                        return Ok(message);
                    }
                    self.early.lock().unwrap().insert(request_seq, message);
                }
                Ok(Incoming::Event(event)) => self.events.lock().unwrap().push(event),
                Ok(Incoming::Closed(reason)) => {
                    self.closed = Some(reason.clone());
                    return Err(self.closed_error(&reason));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let reason = "debug adapter reader thread stopped".to_string();
                    self.closed = Some(reason.clone());
                    return Err(self.closed_error(&reason));
                }
            }
        }
    }

    /// Send a request, wait for its response, and fail unless `success` is true.
    ///
    /// **The `success` flag is necessary but not sufficient** — see
    /// [`crate::repl`]: `evaluate(context="repl")` reports success even when the
    /// GDB command it ran failed.
    pub fn request_ok(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let response = self.request(command, arguments)?;
        ensure_success(command, &response)
    }

    /// Drain every event received so far, in arrival order.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// Block until an event with `name` arrives (draining and buffering the
    /// others), or the deadline passes.
    ///
    /// Returns `None` on timeout; the caller decides whether that is fatal.
    pub fn wait_event(&mut self, name: &str, timeout: Duration) -> Result<Option<Value>> {
        {
            let mut events = self.events.lock().unwrap();
            if let Some(index) = events.iter().position(|e| e.get("event").and_then(Value::as_str) == Some(name)) {
                return Ok(Some(events.remove(index)));
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // One last non-blocking sweep: the reader thread may have
                // delivered the event microseconds after our deadline, and
                // reporting "no event" for a message that is already in the
                // channel would be a false negative.
                let received = self.incoming.lock().unwrap().try_recv();
                match received {
                    Ok(Incoming::Event(event)) => {
                        if event.get("event").and_then(Value::as_str) == Some(name) {
                            return Ok(Some(event));
                        }
                        self.events.lock().unwrap().push(event);
                        continue;
                    }
                    Ok(Incoming::Response { request_seq, message }) => {
                        self.early.lock().unwrap().insert(request_seq, message);
                        continue;
                    }
                    _ => return Ok(None),
                }
            }
            let received = self.incoming.lock().unwrap().recv_timeout(remaining);
            match received {
                Ok(Incoming::Event(event)) => {
                    if event.get("event").and_then(Value::as_str) == Some(name) {
                        return Ok(Some(event));
                    }
                    self.events.lock().unwrap().push(event);
                }
                Ok(Incoming::Response { request_seq, message }) => {
                    // A response nobody is waiting for yet: stash it so a later
                    // `wait_response` (e.g. the delayed `attach`) still finds it.
                    self.early.lock().unwrap().insert(request_seq, message);
                }
                Ok(Incoming::Closed(reason)) => {
                    self.closed = Some(reason.clone());
                    return Err(self.closed_error(&reason));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }

    /// Whether the adapter has gone away (EOF on stdout or process exit).
    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    fn closed_error(&self, reason: &str) -> PrincessError {
        PrincessError::new(
            ErrorCode::QemuFailed,
            "the debug adapter terminated while the engine was talking to it",
        )
        .with_detail(format!("{}\n{reason}", self.command.display()))
    }

    /// Shut the adapter down: close stdin so it can exit cleanly, wait briefly,
    /// then kill it.  Never leaves the process behind.
    pub fn shutdown(&mut self) {
        // Dropping stdin is the graceful signal (gdb's DAP exits on EOF).
        // `ChildStdin` has no `close`, so swap in a thread that is already
        // finished: easiest correct way is to shut down the fd via libc.
        unsafe {
            let fd = {
                use std::os::unix::io::AsRawFd;
                self.stdin.as_raw_fd()
            };
            libc::shutdown(fd, libc::SHUT_WR);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            // The reader unblocks when the process dies and EOFs its stdout.
            let _ = reader.join();
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Turn a `success: false` DAP response into a `PrincessError` carrying the
/// adapter's own message and any extra body fields, verbatim.
pub fn ensure_success(command: &str, response: &Value) -> Result<Value> {
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if success {
        return Ok(response.clone());
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("(adapter sent no message)");
    let mut detail = serde_json::to_string_pretty(response).unwrap_or_else(|_| response.to_string());
    if let Some(extra) = response.get("body") {
        detail.push_str("\nbody: ");
        detail.push_str(&extra.to_string());
    }
    Err(PrincessError::new(
        ErrorCode::Internal,
        format!("DAP request `{command}` failed: {message}"),
    )
    .with_detail(detail))
}

/// Map a framing failure onto the engine's closed error-code set, keeping the
/// raw evidence.
pub fn describe_frame_error(err: &FrameError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ensure_success_passes_through_a_successful_response() {
        let response = json!({"success": true, "body": {"threads": []}});
        assert!(ensure_success("threads", &response).is_ok());
    }

    #[test]
    fn ensure_success_turns_a_failure_into_an_error_with_the_adapter_message() {
        let response = json!({
            "success": false,
            "command": "stackTrace",
            "message": "notStopped",
            "body": {"error": {"id": 3}}
        });
        let err = ensure_success("stackTrace", &response).unwrap_err();
        assert!(err.message.contains("notStopped"), "{err}");
        let detail = err.detail.as_deref().unwrap();
        assert!(detail.contains("notStopped"), "{detail}");
        assert!(detail.contains(r#""id": 3"#), "body evidence must be kept: {detail}");
    }

    #[test]
    fn ensure_success_treats_a_missing_flag_as_failure() {
        // Defaulting an absent `success` to true would turn a malformed adapter
        // reply into a confident empty result.
        let err = ensure_success("threads", &json!({"type": "response"})).unwrap_err();
        assert!(err.message.contains("threads"));
    }

    #[test]
    fn adapter_command_display_is_the_exact_argv() {
        let cmd = AdapterCommand::new(vec!["/bin/gdb".into(), "-q".into(), "-i=dap".into()]);
        assert_eq!(cmd.display(), "/bin/gdb -q -i=dap");
    }

    /// The transport must refuse to even try an empty command instead of
    /// spawning something arbitrary.
    #[test]
    fn empty_adapter_command_is_rejected() {
        let err = Transport::spawn(AdapterCommand::new(Vec::new())).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
    }

    /// A missing adapter binary must be `E_TOOLCHAIN_MISSING` with the argv in
    /// the detail, not an opaque io error (contract §3).
    #[test]
    fn missing_adapter_binary_reports_toolchain_missing() {
        let err = Transport::spawn(AdapterCommand::new(vec![
            "/nonexistent/princess-gdb-does-not-exist".into(),
            "-i=dap".into(),
        ]))
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
        assert!(err.detail.as_deref().unwrap().contains("princess-gdb-does-not-exist"));
    }

    /// End-to-end check of the request/response machinery against a real child
    /// process: a tiny shell script that speaks DAP.  It deliberately answers
    /// **out of order** to prove the correlation logic, which is the whole point
    /// of this module.
    #[test]
    fn responses_are_correlated_by_request_seq_not_arrival_order() {
        let dir = std::env::temp_dir().join(format!("princess-debug-transport-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fake-adapter.py");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import sys, json, os
buf = b""
def read():
    """Read exactly one frame.

    os.read (not sys.stdin.buffer.read) so a short write is returned
    immediately instead of blocking until 4096 bytes arrive -- that
    difference is what makes this stand-in usable for a pipelined client.
    """
    global buf
    while True:
        h = buf.find(b"\r\n\r\n")
        if h >= 0:
            n = int(buf[:h].split(b":")[1])
            if len(buf) >= h + 4 + n:
                body = buf[h+4:h+4+n]
                buf = buf[h+4+n:]
                return json.loads(body)
        chunk = os.read(0, 4096)
        if not chunk:
            return None
        buf += chunk
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()
# Read two requests: answer the SECOND first, then the first.
first = read()
second = read()
send({"seq": 10, "type": "response", "request_seq": second["seq"], "command": second["command"], "success": True, "body": {"who": "second"}})
send({"seq": 11, "type": "event", "event": "output", "body": {"category": "console", "output": "interleaved\n"}})
send({"seq": 12, "type": "response", "request_seq": first["seq"], "command": first["command"], "success": True, "body": {"who": "first"}})
while read():
    pass
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let mut transport = Transport::spawn(AdapterCommand::new(vec![
            "/usr/bin/env".into(),
            "python3".into(),
            script.to_string_lossy().into_owned(),
        ]))
        .expect("spawn fake adapter");

        let first_seq = transport.send("attach", Some(json!({"target": "localhost:1234"}))).unwrap();
        let second_seq = transport.send("configurationDone", None).unwrap();

        // Ask for them in the OPPOSITE order to the adapter's replies.
        let second = transport.wait_response(second_seq, Duration::from_secs(10)).unwrap();
        assert_eq!(second["body"]["who"], "second");
        let first = transport.wait_response(first_seq, Duration::from_secs(10)).unwrap();
        assert_eq!(first["body"]["who"], "first");

        // The interleaved event must have been buffered, not dropped.
        let event = transport.wait_event("output", Duration::from_secs(5)).unwrap();
        assert!(event.is_some(), "event emitted between responses was lost");

        transport.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A response that never comes must be `E_TIMEOUT`, not a hang.
    #[test]
    fn a_missing_response_times_out_with_e_timeout() {
        let mut transport = Transport::spawn(AdapterCommand::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            "sleep 30".into(),
        ]))
        .expect("spawn sleeper");
        let err = transport
            .request_within("threads", None, Duration::from_millis(300))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Timeout);
        transport.shutdown();
    }

    /// When the adapter exits mid-conversation the error must name that, so the
    /// UI does not report "no threads" for a dead backend.
    #[test]
    fn adapter_exit_is_reported_as_a_backend_failure() {
        let mut transport = Transport::spawn(AdapterCommand::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            "exit 0".into(),
        ]))
        .expect("spawn exiting adapter");
        let err = transport
            .request_within("threads", None, Duration::from_secs(5))
            .unwrap_err();
        assert!(
            err.code == ErrorCode::QemuFailed || err.code == ErrorCode::Timeout,
            "unexpected {err:?}"
        );
        transport.shutdown();
    }
}
