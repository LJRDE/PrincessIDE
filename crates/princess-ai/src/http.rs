//! A very small blocking HTTP/1.1 client — enough for one JSON POST, and no
//! more.
//!
//! ## Why hand-rolled instead of `ureq` / `reqwest`
//!
//! * **D19**: every new crate is a download on a host whose international
//!   bandwidth is measured in KB/s, and a TLS stack (`rustls` + `ring`) is a
//!   large tree.  This workspace already builds only what it needs.
//! * **P7-2 / "no-network safety"**: the whole request path must be *bounded*.
//!   Here the connect, the request/response headers and every body read share
//!   one explicit deadline, implemented with a non-blocking socket and
//!   `poll(2)`.  There is no code path that can block forever.
//! * The feature is one POST to one endpoint; a general-purpose client would be
//!   mostly unused surface.
//!
//! ## What it supports (deliberately the minimum)
//!
//! `http://` only, `Content-Length` request bodies, `Content-Length` and
//! `Transfer-Encoding: chunked` responses, and no redirects (a redirect would
//! hide a misconfigured `base_url`, and the contract prefers failing loudly).
//!
//! ## What it refuses, loudly
//!
//! An `https://` URL, a `chunked` request, an unsupported
//! `Content-Encoding` — each returns an explicit `E_AI_UNAVAILABLE` /
//! `E_INVALID_CONFIG` error carrying the original text, rather than an
//! approximate fallback.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use princess_core::{ErrorCode, PrincessError, Result};

use crate::error_map;

/// Hard ceiling on a response body, so a misdirected `base_url` cannot make the
/// engine allocate without bound (D22: memory discipline).
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Default connect deadline when the caller does not set one.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 5_000;

/// A fully-described HTTP request.
#[derive(Clone)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Wall-clock ceiling for the *entire* exchange: DNS, connect, request
    /// write, response headers, and every body read.
    pub deadline: Instant,
    /// Connect-only ceiling inside the overall deadline.
    pub connect_timeout: Duration,
    /// The overall budget, kept so a timeout error can quote the number the
    /// caller actually configured rather than one it recomputed.
    pub budget_ms: u64,
    /// Checked on every socket wait.  This is what makes `cancel` immediate
    /// mid-stream instead of "at the next whole frame": the transport is
    /// deliberately ignorant of *why* it should stop, it only asks.
    pub abort: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .field("budget_ms", &self.budget_ms)
            .field("abort", &self.abort.is_some())
            .finish()
    }
}

impl HttpRequest {
    pub fn post_json(url: impl Into<String>, body: Vec<u8>, timeout_ms: u64) -> Self {
        let now = Instant::now();
        Self {
            url: url.into(),
            method: "POST".to_string(),
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Accept".to_string(), "text/event-stream".to_string()),
            ],
            body,
            deadline: now + Duration::from_millis(timeout_ms),
            connect_timeout: Duration::from_millis(timeout_ms.min(DEFAULT_CONNECT_TIMEOUT_MS)),
            budget_ms: timeout_ms,
            abort: None,
        }
    }

    /// Attach a cancellation predicate (see [`HttpRequest::abort`]).
    pub fn with_abort(mut self, abort: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.abort = Some(abort);
        self
    }

    /// The error to return when the predicate fires.
    fn aborted(&self) -> PrincessError {
        error_map::for_cancelled(&self.url)
    }

    fn remaining(&self) -> Option<Duration> {
        self.deadline.checked_duration_since(Instant::now())
    }
}

/// A parsed response, fully read into memory.
///
/// Fully buffering is a deliberate simplification: completions are small, the
/// ceiling above bounds them, and a streamed body is handed to the caller as
/// whole SSE frames rather than as raw socket reads — which is what makes the
/// `cancel` test deterministic ("no chunk is emitted after the cancel point").
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// `host:port` plus the request target, parsed out of an absolute URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub host: String,
    pub port: u16,
    /// Path plus query, always starting with `/`.
    pub target: String,
    /// `true` when the URL asked for TLS, which this client does not speak.
    pub tls: bool,
}

/// Split an absolute `http://host[:port]/path` URL.
pub fn parse_url(url: &str) -> Result<ParsedUrl> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| {
        PrincessError::new(
            ErrorCode::InvalidConfig,
            format!("AI base_url is not an absolute URL: {url:?}"),
        )
        .with_detail("expected `http://host[:port]/path` (this build speaks plain HTTP only)")
    })?;
    let tls = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        other => {
            return Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!("unsupported AI endpoint scheme {other:?}"),
            )
            .with_detail(format!("URL: {url}")))
        }
    };

    let (authority, target) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_string()),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err(PrincessError::new(
            ErrorCode::InvalidConfig,
            format!("AI endpoint has no host: {url:?}"),
        ));
    }

    // IPv6 literals are bracketed; a bare colon inside them is not a port.
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|_| authority.find(']')) {
        let host = authority[1..end].to_string();
        let port = authority[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(80);
        (host, port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        match port.parse::<u16>() {
            Ok(port) => (host.to_string(), port),
            Err(_) => (authority.to_string(), 80),
        }
    } else {
        (authority.to_string(), 80)
    };

    Ok(ParsedUrl {
        host,
        port,
        target,
        tls,
    })
}

/// Perform one request to completion, or fail inside the deadline.
pub fn execute(request: &HttpRequest) -> Result<HttpResponse> {
    let mut stream = connect_stream(request)?;
    let response = read_response(&mut stream, request)?;
    // Best effort: a provider that wants to keep the connection open must not
    // delay us, and an error here says nothing about the response we already
    // have.
    let _ = stream.shutdown(Shutdown::Both);
    Ok(response)
}

/// Connect and send the request, leaving the socket positioned at the response.
///
/// Separated from [`execute`] so the streaming path can hand the same socket to
/// [`read_response_streaming`] instead of buffering the whole body first.
pub fn connect_stream(request: &HttpRequest) -> Result<TcpStream> {
    let parsed = parse_url(&request.url)?;
    if parsed.tls {
        return Err(PrincessError::new(
            ErrorCode::AiUnavailable,
            "this build cannot speak HTTPS to an AI provider",
        )
        .with_detail(format!(
            "{} — link a TLS client and re-point this crate's transport, or use a local http endpoint",
            request.url
        )));
    }

    let addr = resolve(&parsed, request)?;
    let mut stream = connect(addr, request)?;
    stream
        .set_nonblocking(true)
        .map_err(|e| error_map::for_io("socket setup", &e))?;

    let raw = build_request_bytes(request, &parsed);
    write_all(&mut stream, &raw, request, "writing the request")?;
    Ok(stream)
}

fn resolve(parsed: &ParsedUrl, request: &HttpRequest) -> Result<SocketAddr> {
    // DNS is the one step `poll` cannot bound; it is done inside the connect
    // budget and any failure is reported as a transport failure.
    let authority = format!("{}:{}", parsed.host, parsed.port);
    let mut last = None;
    for addr in authority
        .to_socket_addrs()
        .map_err(|e| error_map::for_transport(&format!("resolving {}", parsed.host), &e))?
    {
        // Prefer IPv4: local inference servers frequently listen on v4 only,
        // and a v6 attempt that is refused is a slower path to the same answer.
        if addr.is_ipv4() {
            return Ok(addr);
        }
        last = Some(addr);
    }
    last.ok_or_else(|| {
        error_map::for_transport(
            &format!("resolving {}", parsed.host),
            &format!("no address for {}", parsed.host),
        )
            .with_detail(request.url.clone())
    })
}

fn connect(addr: SocketAddr, request: &HttpRequest) -> Result<TcpStream> {
    let budget = request.connect_timeout.min(request.remaining().unwrap_or_default());
    TcpStream::connect_timeout(&addr, budget)
        .map_err(|e| error_map::for_transport(&format!("connecting to {addr}"), &e))
}

fn build_request_bytes(request: &HttpRequest, parsed: &ParsedUrl) -> Vec<u8> {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        request.method,
        parsed.target,
        host_header(parsed),
        request.body.len()
    );
    for (name, value) in &request.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&request.body);
    bytes
}

fn host_header(parsed: &ParsedUrl) -> String {
    if parsed.port == 80 {
        parsed.host.clone()
    } else {
        format!("{}:{}", parsed.host, parsed.port)
    }
}

fn write_all(
    stream: &mut TcpStream,
    bytes: &[u8],
    request: &HttpRequest,
    context: &str,
) -> Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        if aborted(request) {
            return Err(request.aborted());
        }
        ensure_not_expired(request, context)?;
        match stream.write(&bytes[written..]) {
            Ok(0) => {
                return Err(error_map::for_transport(
                    context,
                    &"the provider closed the connection while the request was being sent",
                ))
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_readable(stream, request, context)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(error_map::for_io(context, &e)),
        }
    }
    Ok(())
}

/// Read the whole response: status line, headers, then the body per framing.
fn read_response(stream: &mut TcpStream, request: &HttpRequest) -> Result<HttpResponse> {
    read_response_with(stream, request, &mut |_bytes| Ok(()))
}

/// Read the response, handing every body increment to `on_body` **as it
/// arrives**.
///
/// The streaming path uses this instead of [`read_response`] so an SSE frame is
/// decoded and pushed the moment it lands, rather than after the whole body has
/// been buffered.  That is what lets a cancellation take effect mid-stream, and
/// it is also why a slow provider produces a live trickle of `ai.chunk` events
/// instead of one burst at the end.
pub fn read_response_streaming(
    stream: &mut TcpStream,
    request: &HttpRequest,
    on_body: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<HttpResponse> {
    read_response_with(stream, request, on_body)
}

fn read_response_with(
    stream: &mut TcpStream,
    request: &HttpRequest,
    on_body: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<HttpResponse> {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 256 * 1024 {
            return Err(error_map::for_malformed(
                "response headers",
                &"header block exceeds 256 KiB",
            ));
        }
        if !read_more(stream, &mut buf, request, "reading the response headers")? {
            return Err(error_map::for_transport(
                "reading the response headers",
                &"the provider closed the connection before sending a complete response",
            ));
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let body_start = header_end + header_delimiter_len(&buf, header_end);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let (status, reason) = parse_status_line(status_line)?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };

    if let Some(encoding) = header("Content-Encoding") {
        let encoding = encoding.trim().to_ascii_lowercase();
        if encoding != "identity" {
            return Err(PrincessError::new(
                ErrorCode::AiUnavailable,
                format!("AI provider used an unsupported Content-Encoding: {encoding}"),
            )
            .with_detail("this build speaks uncompressed HTTP only"));
        }
    }

    let chunked = header("Transfer-Encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let declared_len = header("Content-Length").and_then(|v| v.trim().parse::<usize>().ok());

    let mut body = buf[body_start..].to_vec();
    if !body.is_empty() {
        // Bytes that arrived with the headers are already a complete increment.
        on_body(&body)?;
    }

    if chunked {
        // `Connection: close` means the terminator is the socket closing; the
        // decoder simply stops when the framing says it is complete.
        loop {
            match decode_chunked(&body) {
                ChunkedState::Complete(decoded) => {
                    body = decoded;
                    break;
                }
                ChunkedState::NeedMore => {
                    let before = body.len();
                    if !read_more(stream, &mut body, request, "reading the response body")? {
                        // Truncated transfer: report what we have rather than
                        // hanging, the caller validates the JSON.
                        body = decode_chunked_partial(&body);
                        break;
                    }
                    if body.len() > before {
                        on_body(&body[before..])?;
                    }
                }
                ChunkedState::Malformed(detail) => {
                    return Err(error_map::for_malformed("chunked response body", &detail))
                }
            }
        }
    } else if let Some(len) = declared_len {
        while body.len() < len {
            let before = body.len();
            if !read_more(stream, &mut body, request, "reading the response body")? {
                return Err(error_map::for_transport(
                    "reading the response body",
                    &format!("stream ended after {} of {len} declared bytes", body.len()),
                ));
            }
            if body.len() > before {
                on_body(&body[before..])?;
            }
        }
        body.truncate(len);
    } else {
        // No framing information: read to EOF (the server is closing anyway).
        loop {
            let before = body.len();
            if !read_more(stream, &mut body, request, "reading the response body")? {
                break;
            }
            if body.len() > before {
                on_body(&body[before..])?;
            }
        }
    }

    Ok(HttpResponse {
        status,
        reason,
        headers,
        body,
    })
}

fn parse_status_line(line: &str) -> Result<(u16, String)> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status = parts.next().unwrap_or_default();
    let reason = parts.next().unwrap_or_default().to_string();
    match status.parse::<u16>() {
        Ok(status) if version.starts_with("HTTP/") => Ok((status, reason)),
        _ => Err(error_map::for_malformed(
            "HTTP status line",
            &line.to_string(),
        )),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn header_delimiter_len(_buf: &[u8], _end: usize) -> usize {
    4
}

/// `false` when the peer closed the connection cleanly.
fn read_more(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    request: &HttpRequest,
    context: &str,
) -> Result<bool> {
    if aborted(request) {
        return Err(request.aborted());
    }
    ensure_not_expired(request, context)?;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                if buf.len() + n > MAX_RESPONSE_BYTES {
                    return Err(error_map::for_malformed(
                        context,
                        &format!("response exceeds the {MAX_RESPONSE_BYTES}-byte ceiling this engine allows"),
                    ));
                }
                buf.extend_from_slice(&chunk[..n]);
                return Ok(true);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_readable(stream, request, context)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(error_map::for_io(context, &e)),
        }
    }
}

fn ensure_not_expired(request: &HttpRequest, context: &str) -> Result<()> {
    match request.remaining() {
        Some(_) => Ok(()),
        None => Err(error_map::for_timeout(request.budget_ms)
            .with_detail(format!("the deadline expired while {context}"))),
    }
}

/// Block until the socket is readable (or writable, which `poll` reports the
/// same way for our purposes) or the deadline fires.
fn wait_readable(stream: &TcpStream, request: &HttpRequest, context: &str) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let mut pfd = libc_pollfd {
        fd: stream.as_raw_fd(),
        events: POLLIN | POLLOUT,
        revents: 0,
    };
    // One `poll` may return early (a wakeup, a signal, a partial write); keep
    // waiting until the socket is ready or the deadline really has passed.
    loop {
        if aborted(request) {
            return Err(request.aborted());
        }
        let Some(remaining) = request.remaining() else {
            return Err(error_map::for_timeout(request.budget_ms)
                .with_detail(format!("expired while {context}")));
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
        // SAFETY: `pfd` points at one initialised `libc_pollfd` that nothing
        // else touches for the duration of the call, `nfds` is 1, and `timeout`
        // is a non-negative i32 — exactly `poll(2)`'s contract.
        let rc = unsafe { poll(&mut pfd, 1, timeout_ms) };
        match rc {
            0 => {
                return Err(error_map::for_timeout(request.budget_ms)
                    .with_detail(format!("no data within {timeout_ms} ms while {context}")))
            }
            n if n > 0 => return Ok(()),
            _ => {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error_map::for_io(context, &err));
            }
        }
    }
}

/// Has the caller asked this request to stop?
fn aborted(request: &HttpRequest) -> bool {
    request.abort.as_ref().map(|f| f()).unwrap_or(false)
}

// ----------------------------------------------------------- chunked decoding ---

/// Outcome of trying to decode a `Transfer-Encoding: chunked` body.
#[derive(Debug)]
enum ChunkedState {
    Complete(Vec<u8>),
    NeedMore,
    Malformed(String),
}

fn decode_chunked(body: &[u8]) -> ChunkedState {
    let mut decoded = Vec::new();
    let mut pos = 0usize;
    loop {
        let Some(line_end) = find_crlf(body, pos) else {
            return ChunkedState::NeedMore;
        };
        let line = String::from_utf8_lossy(&body[pos..line_end]);
        let size_text = line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            return ChunkedState::Malformed(format!("bad chunk size line: {line:?}"));
        };
        let data_start = line_end + 2;
        if size == 0 {
            return ChunkedState::Complete(decoded);
        }
        if body.len() < data_start + size + 2 {
            return ChunkedState::NeedMore;
        }
        decoded.extend_from_slice(&body[data_start..data_start + size]);
        pos = data_start + size + 2;
    }
}

/// Best-effort decode of a transfer that ended early.
fn decode_chunked_partial(body: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    let mut pos = 0usize;
    while let Some(line_end) = find_crlf(body, pos) {
        let line = String::from_utf8_lossy(&body[pos..line_end]);
        let size_text = line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        let data_start = line_end + 2;
        if size == 0 {
            break;
        }
        // A chunk without its trailing CRLF never arrived in full: stop before
        // it rather than returning bytes the peer may still rewrite.
        if body.len() < data_start + size + 2 {
            break;
        }
        decoded.extend_from_slice(&body[data_start..data_start + size]);
        pos = data_start + size + 2;
    }
    decoded
}

fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    buf[from..]
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|offset| from + offset)
}

// ------------------------------------------------------------------- libc glue ---

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;

#[repr(C)]
struct libc_pollfd {
    fd: i32,
    events: i16,
    revents: i16,
}

/// `nfds_t` on the targets this engine supports (`u64` on x86_64/aarch64
/// Linux).  Using `c_ulong` mirrors the libc crate's definition instead of
/// guessing, because a wrong width here would make `poll(2)` read past the
/// array.
type Nfds = std::os::raw::c_ulong;

// `poll(2)` directly, rather than through `libc`'s safe wrapper: the crate
// deliberately depends on nothing but `princess-core` + `serde` (D19), and this
// is the only syscall the transport needs.  It is what makes the deadline real —
// a blocking `read` on a socket whose peer went silent would ignore it entirely.
extern "C" {
    fn poll(fds: *mut libc_pollfd, nfds: Nfds, timeout: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_split_into_authority_and_target() {
        let parsed = parse_url("http://127.0.0.1:11434/v1/chat/completions").unwrap();
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 11434);
        assert_eq!(parsed.target, "/v1/chat/completions");
        assert!(!parsed.tls);

        let default_port = parse_url("http://example.invalid/v1").unwrap();
        assert_eq!(default_port.port, 80);

        let no_path = parse_url("http://example.invalid").unwrap();
        assert_eq!(no_path.target, "/");

        let query = parse_url("http://h:1/v1?a=b").unwrap();
        assert_eq!(query.target, "/v1?a=b");

        let v6 = parse_url("http://[::1]:8080/v1").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.port, 8080);
    }

    #[test]
    fn https_is_reported_as_unsupported_not_silently_downgraded() {
        let parsed = parse_url("https://api.example.com/v1").unwrap();
        assert!(parsed.tls);
        let err = execute(&HttpRequest::post_json(
            "https://api.example.com/v1/chat/completions",
            b"{}".to_vec(),
            1_000,
        ))
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert!(err.message.contains("HTTPS"), "{err}");
    }

    #[test]
    fn a_schemeless_url_is_refused_as_a_config_problem() {
        let err = parse_url("127.0.0.1:11434/v1").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
    }

    #[test]
    fn chunked_bodies_decode_and_report_incompleteness() {
        let complete = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        match decode_chunked(complete) {
            ChunkedState::Complete(decoded) => assert_eq!(decoded, b"Wikipedia"),
            other => panic!("{other:?}"),
        }

        let partial = b"4\r\nWiki\r\n5\r\nped";
        match decode_chunked(partial) {
            ChunkedState::NeedMore => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(decode_chunked_partial(partial), b"Wiki");

        match decode_chunked(b"zz\r\n") {
            ChunkedState::Malformed(detail) => assert!(detail.contains("bad chunk size"), "{detail}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_connect_to_a_closed_port_is_a_bounded_unavailable_error() {
        // Port 1 on loopback: nothing can be listening.  The call must return
        // quickly with E_AI_UNAVAILABLE, never hang (P7 "no-network safety").
        let started = Instant::now();
        let err = execute(&HttpRequest::post_json(
            "http://127.0.0.1:1/v1/chat/completions",
            b"{}".to_vec(),
            2_000,
        ))
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

    #[test]
    fn an_unresolvable_host_is_bounded_and_never_panics() {
        let started = Instant::now();
        let err = execute(&HttpRequest::post_json(
            "http://a-host-that-cannot-exist.invalid/v1/chat/completions",
            b"{}".to_vec(),
            2_000,
        ))
        .unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::AiUnavailable | ErrorCode::Timeout),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn status_lines_are_parsed_strictly() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK").unwrap(), (200, "OK".into()));
        assert_eq!(parse_status_line("HTTP/1.1 500 Internal Server Error").unwrap().0, 500);
        assert!(parse_status_line("garbage").is_err());
    }

    #[test]
    fn the_request_bytes_carry_the_contract_headers() {
        let request = HttpRequest::post_json("http://127.0.0.1:1/v1/x", b"{}".to_vec(), 1_000);
        let parsed = parse_url(&request.url).unwrap();
        let bytes = String::from_utf8(build_request_bytes(&request, &parsed)).unwrap();
        assert!(bytes.starts_with("POST /v1/x HTTP/1.1\r\n"), "{bytes}");
        assert!(bytes.contains("Host: 127.0.0.1:1\r\n"), "{bytes}");
        assert!(bytes.contains("Content-Length: 2\r\n"), "{bytes}");
        assert!(bytes.contains("Accept: text/event-stream\r\n"), "{bytes}");
        assert!(bytes.ends_with("\r\n\r\n{}"), "{bytes}");
    }
}
