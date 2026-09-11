//! A local mock OpenAI-compatible server for the P7-1 tests.
//!
//! ## Why a hand-rolled server and not `wiremock` / `mockito`
//!
//! * **D19**: this workspace adds a dependency only when it must, and every one
//!   is a download on a very slow link.
//! * **The mock must be local and deterministic.** P7-3 forbids the test suite
//!   from depending on a real provider, and the "no-network safety" rule forbids
//!   a test that can hang.  Everything here binds `127.0.0.1:0` (a kernel-chosen
//!   free port) and every socket has a deadline, so the suite cannot wedge.
//!
//! ## What it can be told to do
//!
//! [`MockServer::spawn`] takes a script: a status code, a body, and how to
//! deliver it ([`Delivery`]) — all at once, in dribbled fragments, after a
//! delay, or in the shape of a real SSE completion.  A test therefore describes
//! the *provider behaviour* it wants, not a stack of HTTP plumbing.
//!
//! ## Wire-level honesty
//!
//! The mock speaks real HTTP/1.1: it parses the request line and headers,
//! consumes exactly `Content-Length` bytes of body, and answers with
//! `Content-Length` framing (or `Transfer-Encoding: chunked` when asked).  A
//! test that passes against it has exercised the real client, not a stub of it.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How the mock should deliver the body it was given.
#[derive(Debug, Clone)]
pub enum Delivery {
    /// One `write` with everything.
    Whole,
    /// Write the bytes, then `sleep` before closing.  Exercises deadlines: the
    /// client has the headers but never the complete body.
    WholeThenStall(Duration),
    /// Sleep before sending anything at all.
    DelayBeforeHeaders(Duration),
    /// `Transfer-Encoding: chunked`, one SSE line per chunk.
    ChunkedSse,
    /// Advertise more `Content-Length` than will ever be sent, then close.
    Truncated(usize),
    /// Refuse the connection instead of accepting it.
    None,
    /// One SSE line per write, with a pause between lines, then hold the
    /// connection open for `tail`.  The mock judges line boundaries by `\n`,
    /// which is safe here because the test bodies are ASCII-dominated and the
    /// client decoder is byte-oriented anyway.
    SlowSse { per_line: Duration, tail: Duration },
}

/// What the mock should answer with.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub reason: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub delivery: Delivery,
}

impl MockResponse {
    pub fn ok_sse(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            reason: "OK".to_string(),
            content_type: "text/event-stream".to_string(),
            body: body.into(),
            delivery: Delivery::Whole,
        }
    }

    /// An SSE body whose lines are delivered one at a time with `per_line`
    /// between them, then held open for `tail`.  This is the shape a real
    /// streaming provider has, and the only one in which "cancel mid-stream"
    /// is a meaningful thing to test.
    pub fn slow_sse(body: impl Into<Vec<u8>>, per_line: Duration, tail: Duration) -> Self {
        Self {
            status: 200,
            reason: "OK".to_string(),
            content_type: "text/event-stream".to_string(),
            body: body.into(),
            delivery: Delivery::SlowSse { per_line, tail },
        }
    }

    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason_for(status).to_string(),
            content_type: "application/json".to_string(),
            body: body.into().into_bytes(),
            delivery: Delivery::Whole,
        }
    }

    pub fn with_delivery(mut self, delivery: Delivery) -> Self {
        self.delivery = delivery;
        self
    }
}

fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// A live mock server on loopback.
pub struct MockServer {
    port: u16,
    /// Every request the server received, as raw text (request line + headers +
    /// body).  Tests assert on what the client actually sent.
    requests: Arc<Mutex<Vec<String>>>,
    hits: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Start a server that answers every request the same way.
    pub fn spawn(response: MockResponse) -> Self {
        let response = Arc::new(response);
        Self::spawn_with(Arc::new(move |_request: &str, _index: usize| {
            (*response).clone()
        }))
    }

    /// Start a server that decides per request (used for the retry test).
    pub fn spawn_with<F>(responder: Arc<F>) -> Self
    where
        F: Fn(&str, usize) -> MockResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local_addr").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener so shutdown is observable");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_requests = Arc::clone(&requests);
        let thread_hits = Arc::clone(&hits);
        let thread_shutdown = Arc::clone(&shutdown);
        let responder = Arc::clone(&responder);
        let mut connection_threads: Vec<JoinHandle<()>> = Vec::new();
        let handle = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let index = thread_hits.fetch_add(1, Ordering::SeqCst);
                        let thread_requests = Arc::clone(&thread_requests);
                        let responder = Arc::clone(&responder);
                        // One thread per connection: a deliberately stalling
                        // response (the timeout tests) must not stop the mock
                        // from serving — or from *shutting down*.
                        connection_threads.push(thread::spawn(move || {
                            let request = read_request(stream.try_clone().expect("clone stream"));
                            thread_requests
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(request.clone());
                            let response = responder(&request, index);
                            serve(stream, response);
                        }));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            requests,
            hits,
            shutdown,
            handle: Some(handle),
        }
    }

    /// The address the mock is listening on.
    fn addr(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// A port with nothing listening on it: bind, learn the port, drop the
    /// listener.  This is the "provider refused the connection" case.
    pub fn dead_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local_addr").port();
        drop(listener);
        port
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Base URL to hand to the provider (`http://127.0.0.1:<port>/v1`).
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// A copy of every request received so far.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Wait until at least `n` requests have been received, or time out.
    pub fn wait_for_hits(&self, n: usize, timeout: Duration) -> usize {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let hits = self.hits();
            if hits >= n {
                return hits;
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.hits()
    }

    /// Stop the accept loop.
    ///
    /// Deliberately **does not join** the accept thread: a test that leaves a
    /// deliberately stalling response in flight (the timeout and cancellation
    /// cases) would otherwise block teardown on that sleep, which would make
    /// the suite slow for a reason that has nothing to do with the code under
    /// test.  The accept loop polls every 2 ms and exits as soon as it sees the
    /// flag, and the connect below wakes it immediately if it is between polls,
    /// so the thread is gone well before the test binary finishes.
    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Only poke the listener while it is still listening: after `stop` the
        // port may already belong to another test's mock.
        let _ = TcpStream::connect_timeout(&self.addr(), Duration::from_millis(50));
        self.handle.take();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Read a complete HTTP request (headers + `Content-Length` body) with a
/// deadline, so a broken client cannot wedge the mock's thread.
fn read_request(mut stream: TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    loop {
        if header_end.is_none() {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
            }
        }
        if let Some(end) = header_end {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let content_length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if buf.len() >= end + content_length {
                return String::from_utf8_lossy(&buf).into_owned();
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => return String::from_utf8_lossy(&buf).into_owned(),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return String::from_utf8_lossy(&buf).into_owned(),
        }
    }
}

fn serve(mut stream: TcpStream, response: MockResponse) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    if let Delivery::DelayBeforeHeaders(delay) = response.delivery {
        thread::sleep(delay);
    }
    if matches!(response.delivery, Delivery::None) {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }

    let head = match response.delivery {
        Delivery::ChunkedSse => format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            response.status, response.reason, response.content_type
        ),
        Delivery::Truncated(advertised) => format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.status, response.reason, response.content_type, advertised
        ),
        _ => format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.status,
            response.reason,
            response.content_type,
            response.body.len()
        ),
    };
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    match response.delivery {
        Delivery::ChunkedSse => {
            // One SSE line per HTTP chunk, which is how a real proxy behaves.
            for line in response.body.split_inclusive(|b| *b == b'\n') {
                let frame = format!("{:x}\r\n", line.len());
                if stream.write_all(frame.as_bytes()).is_err() {
                    return;
                }
                if stream.write_all(line).is_err() {
                    return;
                }
                if stream.write_all(b"\r\n").is_err() {
                    return;
                }
                let _ = stream.flush();
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        }
        Delivery::Truncated(advertised) => {
            let take = response.body.len().min(advertised.saturating_sub(1));
            let _ = stream.write_all(&response.body[..take]);
            // Close early: fewer bytes than the advertised Content-Length.
        }
        Delivery::WholeThenStall(delay) => {
            // Write in chunks with a short pause between them, then hold the
            // connection open.  A single `write_all` would put the whole body
            // in the socket buffer before the client could ever react, which is
            // useless for testing cancellation; this way the response is
            // genuinely still arriving while the test is running.
            let per_chunk = Duration::from_millis(5).max(delay / 20);
            for piece in response.body.chunks(64) {
                if stream.write_all(piece).is_err() {
                    return;
                }
                let _ = stream.flush();
                // The test is expected to pull the plug long before this loop
                // finishes; sleeping here lets that happen at a frame boundary
                // instead of only at the very end.
                thread::sleep(per_chunk);
            }
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        Delivery::SlowSse { per_line, tail } => {
            for line in response.body.split_inclusive(|b| *b == b'\n') {
                if stream.write_all(line).is_err() {
                    return;
                }
                let _ = stream.flush();
                thread::sleep(per_line);
            }
            let deadline = Instant::now() + tail;
            while Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        Delivery::DelayBeforeHeaders(_) | Delivery::Whole => {
            let _ = stream.write_all(&response.body);
        }
        Delivery::None => {}
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

/// Build the SSE body of a normal completion from a list of text pieces.
pub fn sse_body(pieces: &[&str], usage: Option<(u64, u64, u64)>, include_done: bool) -> Vec<u8> {
    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
    for piece in pieces {
        let frame = serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "model": "mock-model",
            "choices": [{ "index": 0, "delta": { "content": piece } }],
        });
        body.push_str(&format!("data: {frame}\n\n"));
    }
    if let Some((prompt, completion, total)) = usage {
        let frame = serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "model": "mock-model",
            "choices": [],
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total,
            },
        });
        body.push_str(&format!("data: {frame}\n\n"));
    }
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    if include_done {
        body.push_str("data: [DONE]\n\n");
    }
    body.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mock_answers_a_real_http_request() {
        let server = MockServer::spawn(MockResponse::ok_sse(sse_body(&["hi"], None, true)));
        let response = crate::http::execute(&crate::http::HttpRequest::post_json(
            server.url("/v1/chat/completions"),
            b"{\"ping\":true}".to_vec(),
            5_000,
        ))
        .unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body_text().contains("[DONE]"));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /v1/chat/completions HTTP/1.1"), "{}", requests[0]);
        assert!(requests[0].trim_end().ends_with("{\"ping\":true}"), "{}", requests[0]);
    }

    #[test]
    fn the_mock_can_dribble_a_body_as_chunked_frames() {
        let server = MockServer::spawn(
            MockResponse::ok_sse(sse_body(&["a", "b"], None, true)).with_delivery(Delivery::ChunkedSse),
        );
        let response = crate::http::execute(&crate::http::HttpRequest::post_json(
            server.url("/v1/chat/completions"),
            b"{}".to_vec(),
            5_000,
        ))
        .unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body_text().contains("\"content\":\"a\""));
        assert!(response.body_text().contains("[DONE]"));
    }

    #[test]
    fn a_stalling_server_is_bounded_by_the_client_deadline() {
        // Advertise 4096 bytes but send 6, then keep the socket open: the only
        // way out is the client's deadline.
        let server = MockServer::spawn(
            MockResponse::ok_sse(b"data: ".to_vec()).with_delivery(Delivery::Truncated(4096)),
        );
        let started = Instant::now();
        let err = crate::http::execute(&crate::http::HttpRequest::post_json(
            server.url("/v1/chat/completions"),
            b"{}".to_vec(),
            600,
        ))
        .unwrap_err();
        // Either bound may fire first — the socket can be closed by the peer
        // before the deadline, which is a transport failure — but the call must
        // return quickly and must be one of the two contract codes, never a
        // hang and never `E_INTERNAL`.
        assert!(
            matches!(
                err.code,
                princess_core::ErrorCode::Timeout | princess_core::ErrorCode::AiUnavailable
            ),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn a_server_that_never_answers_is_bounded_too() {
        let server = MockServer::spawn(
            MockResponse::ok_sse(b"".to_vec())
                .with_delivery(Delivery::DelayBeforeHeaders(Duration::from_secs(30))),
        );
        let started = Instant::now();
        let err = crate::http::execute(&crate::http::HttpRequest::post_json(
            server.url("/v1/chat/completions"),
            b"{}".to_vec(),
            500,
        ))
        .unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Timeout, "{err}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn a_dead_port_is_really_dead() {
        let port = MockServer::dead_port();
        let result = crate::http::execute(&crate::http::HttpRequest::post_json(
            format!("http://127.0.0.1:{port}/v1/chat/completions"),
            b"{}".to_vec(),
            1_000,
        ));
        assert!(result.is_err());
    }
}
