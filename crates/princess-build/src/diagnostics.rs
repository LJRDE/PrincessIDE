//! Compiler / assembler / linker diagnostics → `build.diagnostic` (contract §2).
//!
//! The reference fixture and the template both build with **GNU** `gcc`/`ld`
//! while the IDE's language service is `clangd-16`, so this module understands
//! both families instead of pretending they are the same tool:
//!
//! | producer | shape understood | `source` |
//! |---|---|---|
//! | gcc / g++ | `file:line:col: severity: message` (+ `note:`, continuation lines) | `gcc` |
//! | clang | same shape, but clang prints the caret/`|` block and `~~~^` notes | `clang` |
//! | ld | `file:line: message` (no severity) | `ld` |
//! | nasm | `file:line: error: message` | `nasm` |
//!
//! **Why the producer is explicit.**  The contract enumerates
//! `clang|ld|nasm` and adds `gcc` precisely because mislabelling a GNU gcc
//! diagnostic as `clang` would be a lie in the event stream.  We never guess from
//! the message text alone: the parser is told which family the *executable*
//! belongs to (see [`DiagnosticParser::for_program`]) and only falls back to
//! shape-sniffing for lines that arrive from a nested tool.
//!
//! **Why GCC vs clang is decided per line, not per run.**  A GCC-sourced line
//! that carries clang's own markers (`warning: [-Wflag]`, a trailing `~~~^`
//! caret block) is re-labelled `clang`, and vice versa, so a project that mixes
//! toolchains still gets honest `source` values.
//!
//! Paths are made **absolute** against the build working directory, which is what
//! the front end needs to jump to the right file (the contract says `file` is
//! "absolute path when the engine could resolve it, else the raw text").

use std::path::{Path, PathBuf};

use princess_core::event::BuildDiagnosticPayload;
use princess_core::types::{DiagnosticSeverity, DiagnosticSource};

/// Which tool family a stream of lines came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFamily {
    Gcc,
    Clang,
    Ld,
    Nasm,
    /// Unknown: decide per line from its shape.
    Unknown,
}

impl ToolFamily {
    /// Classify from the program name (`gcc-12`, `clang-16`, `ld.lld`, `nasm`).
    pub fn for_program(program: &str) -> ToolFamily {
        let name = Path::new(program)
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        // Tolerate the toolchain prefixes kernels use: x86_64-elf-gcc.
        if name.contains("nasm") || name == "yasm" {
            return ToolFamily::Nasm;
        }
        if name.contains("clang") {
            return ToolFamily::Clang;
        }
        if name.contains("gcc") || name.contains("g++") || name.contains("cc1") {
            return ToolFamily::Gcc;
        }
        if name == "ld" || name.contains("ld.") || name.ends_with("-ld") || name.contains("collect2") {
            return ToolFamily::Ld;
        }
        ToolFamily::Unknown
    }

    fn default_source(self) -> Option<DiagnosticSource> {
        match self {
            ToolFamily::Gcc => Some(DiagnosticSource::Gcc),
            ToolFamily::Clang => Some(DiagnosticSource::Clang),
            ToolFamily::Ld => Some(DiagnosticSource::Ld),
            ToolFamily::Nasm => Some(DiagnosticSource::Nasm),
            ToolFamily::Unknown => None,
        }
    }

    /// Public view of [`ToolFamily::default_source`] for callers (the backend's
    /// `source_for_tool` helper) that need the contract `source` for a program.
    pub fn source_public(self) -> Option<DiagnosticSource> {
        self.default_source()
    }

    /// The `make` recipe echo (`gcc -c kernel.c`) names the compiler that is
    /// about to run, which is how a make backend keeps `source` honest even
    /// though the line itself came from `make`.
    pub fn from_recipe_line(line: &str) -> Option<ToolFamily> {
        let first = line.split_whitespace().next()?;
        if !line.trim_start().starts_with(first) {
            return None;
        }
        let family = ToolFamily::for_program(first);
        match family {
            ToolFamily::Unknown => None,
            other => Some(other),
        }
    }
}

/// A diagnostic before path resolution / de-duplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDiagnostic {
    pub severity: DiagnosticSeverity,
    /// Raw path text as printed by the tool (may be relative).
    pub file: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub message: String,
    pub source: DiagnosticSource,
}

/// One decoded diagnostic, ready to become a `build.diagnostic` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDiagnostic {
    pub severity: DiagnosticSeverity,
    /// Absolute when resolvable, else the raw text.
    pub file: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub message: String,
    pub source: DiagnosticSource,
}

impl ParsedDiagnostic {
    /// The wire payload for this diagnostic.
    pub fn to_payload(&self) -> BuildDiagnosticPayload {
        BuildDiagnosticPayload {
            severity: self.severity,
            file: self.file.clone(),
            line: self.line,
            col: self.col,
            message: self.message.clone(),
            source: self.source,
        }
    }
}

/// Incremental line → diagnostic parser.
///
/// Feed it the *same* decoded lines that go out as `log.append`, in arrival
/// order; it returns zero or more diagnostics for each line.  State is kept for
/// two things:
///
/// * the current tool family (updated by `make` recipe echoes such as `gcc -c …`),
/// * the "continuation" context so a clang caret block or a gcc `note:` line is
///   attached to the diagnostic it belongs to instead of being emitted as a
///   bogus standalone one.
#[derive(Debug, Clone)]
pub struct DiagnosticParser {
    cwd: PathBuf,
    family: ToolFamily,
    last: Option<RawDiagnostic>,
}

impl DiagnosticParser {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            family: ToolFamily::Unknown,
            last: None,
        }
    }

    /// Pin the family for the whole run (used by tests and by a `custom`
    /// `[build] command` whose backend we know).
    pub fn with_family(mut self, family: ToolFamily) -> Self {
        self.family = family;
        self
    }

    pub fn family(&self) -> ToolFamily {
        self.family
    }

    /// Feed one line (without its trailing newline is fine).
    pub fn feed(&mut self, line: &str) -> Vec<ParsedDiagnostic> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.trim().is_empty() {
            return Vec::new();
        }

        if let Some(family) = ToolFamily::from_recipe_line(trimmed) {
            self.family = family;
        }

        match parse_line_for(trimmed, self.family) {
            Some(raw) => {
                let diagnostic = self.finish(raw.clone());
                self.last = Some(raw);
                diagnostic.into_iter().collect()
            }
            None => {
                // Continuation of the previous diagnostic (gcc's source-echo
                // block, clang's caret lines, ...).  Fold the detail into the
                // *same* diagnostic and emit **nothing**: re-emitting here would
                // publish the same error once per continuation line (gcc prints
                // five lines for one syntax error), which the UI would show as
                // five identical problems with progressively longer text.
                let fragment = trimmed.trim();
                if !is_continuation(fragment) {
                    self.last = None;
                    return Vec::new();
                }
                let Some(last) = self.last.as_mut() else {
                    return Vec::new();
                };
                if is_clang_marker(fragment) {
                    last.source = DiagnosticSource::Clang;
                }
                if !last.message.contains(fragment) {
                    last.message.push('\n');
                    last.message.push_str(fragment);
                }
                Vec::new()
            }
        }
    }

    /// Feed a whole text block.
    pub fn feed_text(&mut self, text: &str) -> Vec<ParsedDiagnostic> {
        let mut out = Vec::new();
        for line in text.split('\n') {
            out.extend(self.feed(line));
        }
        out
    }

    /// Finish the pending diagnostic, if any.
    ///
    /// Continuation lines update the diagnostic already in `self.last` instead of
    /// emitting a new one, so the caller must flush once at the end of a stream
    /// to observe the fully-folded message.
    pub fn flush(&mut self) -> Option<ParsedDiagnostic> {
        let raw = self.last.take()?;
        self.finish(raw)
    }

    /// All diagnostics in a block, de-duplicated (the same diagnostic is often
    /// echoed once by the compiler and once by `make`).
    pub fn feed_text_dedup(&mut self, text: &str) -> Vec<ParsedDiagnostic> {
        let mut out = self.feed_text(text);
        // The *last* diagnostic of a block usually has continuation lines folded
        // into it after it was first emitted, so the flushed copy is the
        // authoritative one.  Drop the stale earlier copy with the same
        // location, otherwise the same error is reported twice (once without the
        // source echo, once with it).
        if let Some(pending) = self.flush() {
            if let Some(index) = out.iter().position(|d| same_problem(d, &pending)) {
                out.remove(index);
            }
            out.push(pending);
        }
        dedup(out)
    }

    fn finish(&self, raw: RawDiagnostic) -> Option<ParsedDiagnostic> {
        let message = raw.message.trim().to_string();
        if message.is_empty() {
            return None;
        }
        // A `source` that is only a column/line pointer with no text is noise.
        Some(ParsedDiagnostic {
            severity: raw.severity,
            file: raw.file.as_deref().map(|f| self.resolve(f)),
            line: raw.line,
            col: raw.col,
            message,
            source: raw.source,
        })
    }

    /// Resolve a tool-reported path against the build cwd, lexically.
    fn resolve(&self, file: &str) -> String {
        let file = file.trim();
        // `<builtin>` / `<command-line>` are clang pseudo-files: keep verbatim.
        if file.starts_with('<') && file.ends_with('>') {
            return file.to_string();
        }
        let path = Path::new(file);
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        princess_core::config::normalize(&joined)
            .to_string_lossy()
            .into_owned()
    }
}

/// Do two diagnostics describe the same problem at the same place?
///
/// Used to replace a diagnostic that later gained folded continuation detail,
/// rather than reporting it twice.
fn same_problem(a: &ParsedDiagnostic, b: &ParsedDiagnostic) -> bool {
    a.file == b.file && a.line == b.line && a.col == b.col && a.source == b.source
}

/// Remove duplicates while keeping first-seen order.
pub fn dedup(diagnostics: Vec<ParsedDiagnostic>) -> Vec<ParsedDiagnostic> {
    let mut out: Vec<ParsedDiagnostic> = Vec::new();
    for diagnostic in diagnostics {
        // gcc prints `kernel.c:5:3: error: …` and then, for the same problem,
        // `kernel.c:5:3: note: …`; those are *different* severities and must
        // both survive.  Only byte-identical entries are dropped.
        if !out.contains(&diagnostic) {
            out.push(diagnostic);
        }
    }
    out
}

/// Parse a single raw line without any cross-line state.
///
/// The family defaults to `Unknown`: use [`DiagnosticParser::feed`] (or
/// [`parse_line_for`]) when the driving tool is known, so an un-classified
/// message is attributed to the right producer.
pub fn parse_line(line: &str) -> Option<RawDiagnostic> {
    parse_line_for(line, ToolFamily::Unknown)
}

/// Parse one line, attributing un-classified messages to `family`.
pub fn parse_line_for(line: &str, family: ToolFamily) -> Option<RawDiagnostic> {
    if let Some(raw) = parse_colon_form(line, family) {
        return Some(raw);
    }
    if let Some(raw) = parse_make_error(line, family) {
        return Some(raw);
    }
    parse_ld_form(line)
}

/// `ld: build/kernel.o: in function ``kmain':` and friends — a linker message
/// with **no severity word** and no line number.
///
/// This shape is deliberately narrow: the line must *start* with `ld`/`ld.lld`/
/// `<prefix>-ld`, carry a `:`-separated object path, and then carry the rest of
/// the message.  It is what GNU ld prints before the actual `undefined
/// reference` error, so losing it would lose the "which object file" context.
///
/// The shape is self-identifying (`ld:` prefix), so this does not depend on the
/// run's `family` — an `ld` invoked by `gcc` as the linker still has to be
/// labelled `ld`.
fn parse_ld_form(line: &str) -> Option<RawDiagnostic> {
    let trimmed = line.trim_start();
    let (program, rest) = trimmed.split_once(':')?;
    if ToolFamily::for_program(program.trim()) != ToolFamily::Ld {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    // `ld: <file>: <message>` — the file may be absent (`ld: cannot find -lc`).
    let (file, message) = match rest.split_once(':') {
        Some((file, message)) if !file.trim().is_empty() && !message.trim().is_empty() => {
            (file.trim(), message.trim())
        }
        _ => return None,
    };
    Some(RawDiagnostic {
        severity: DiagnosticSeverity::Error,
        file: Some(file.to_string()),
        line: None,
        col: None,
        message: message.to_string(),
        source: DiagnosticSource::Ld,
    })
}

/// `make: *** [Makefile:12: build/kernel.o] Error 1` and friends.
fn parse_make_error(line: &str, family: ToolFamily) -> Option<RawDiagnostic> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("make") {
        return None;
    }
    if !(line.contains("***") || line.contains("Error ")) {
        return None;
    }
    Some(RawDiagnostic {
        severity: DiagnosticSeverity::Error,
        file: None,
        line: None,
        col: None,
        message: line.trim().to_string(),
        // `make` itself is not one of the four contract sources; the *compiler*
        // message that precedes it carries the real source.  We attribute the
        // make-level summary to the family currently in effect so the UI never
        // gets a diagnostic with an invented `clang` label.
        source: family.default_source().unwrap_or(DiagnosticSource::Gcc),
    })
}

/// `file:line[:col]: severity: message` — the gcc/clang/nasm colon form.
fn parse_colon_form(line: &str, family: ToolFamily) -> Option<RawDiagnostic> {
    // Scan for the `: severity:` marker so a Windows-style drive letter or a
    // colon inside a path cannot confuse us.  Build systems never emit those
    // here, but the scan is cheap and makes the parser robust.
    //
    // Markers are listed longest-first and the **earliest** match wins:
    // `: fatal error:` also contains the substring `: error:`, and a later
    // marker appearing further right must not overwrite the real one.
    let markers = [
        (": fatal error:", DiagnosticSeverity::Error),
        (": error:", DiagnosticSeverity::Error),
        (": warning:", DiagnosticSeverity::Warning),
        (": note:", DiagnosticSeverity::Note),
    ];
    let (at, needle_len, severity) = markers
        .iter()
        .filter_map(|(needle, severity)| line.find(needle).map(|at| (at, needle.len(), *severity)))
        .min_by_key(|(at, _, _)| *at)?;

    let location = &line[..at];
    let message = line[at + needle_len..].trim().to_string();
    if message.is_empty() {
        return None;
    }

    let (file, line_no, col) = split_location(location)?;
    let source = classify_source(&message, line, family);
    Some(RawDiagnostic {
        severity,
        file: Some(file),
        line: line_no,
        col,
        message,
        source,
    })
}

/// Split `path:line:col` / `path:line` / `path` (ld's `a.o:12: undefined …`).
///
/// Returns `None` when the prefix cannot be a source location at all, which is
/// how ordinary stdout noise (`mkdir -p build`, `[refkernel] built …`) is
/// rejected instead of being turned into a fake diagnostic.
fn split_location(location: &str) -> Option<(String, Option<u32>, Option<u32>)> {
    let bytes = location.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // Walk back over `:digits` groups.
    let mut end = bytes.len();
    let mut numbers: Vec<u32> = Vec::new();
    while numbers.len() < 2 {
        // A missing further colon is the normal end of the `file:line` shape
        // (`boot.s:12`), not an error: stop walking, do not bail out.  Using
        // `?` here would make every `file:line` (no column) unparseable.
        let Some(colon) = location[..end].rfind(':') else {
            break;
        };
        let tail = &location[colon + 1..end];
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        numbers.push(tail.parse().ok()?);
        end = colon;
    }
    if numbers.is_empty() {
        // No line number at all: that is ld's `linker.ld: undefined symbol`
        // shape, still worth reporting.
        let file = location.trim();
        if file.is_empty() || file.contains(' ') {
            return None;
        }
        return Some((file.to_string(), None, None));
    }
    let file = location[..end].trim();
    if file.is_empty() {
        return None;
    }
    numbers.reverse();
    let line = numbers.first().copied();
    let col = numbers.get(1).copied();
    Some((file.to_string(), line, col))
}

/// Decide `gcc` vs `clang` (or `ld`/`nasm`) for a parsed line.
///
/// The **content fingerprints win** (a `[-W…]` suffix means clang even if the
/// run was started by `gcc`), but the fallback is the family of the tool that is
/// actually running — not a hard-coded `gcc`.  Without this, `clang`-driven
/// builds would have every un-classified message mislabelled as `gcc`.
fn classify_source(message: &str, whole_line: &str, family: ToolFamily) -> DiagnosticSource {
    let lower = whole_line.to_ascii_lowercase();
    // NASM's own fingerprints.  These are checked against the *whole line*, not
    // just the message: the message has already been cut at its first
    // `: severity:` marker, which in NASM's
    // `boot.s:12: error: parser: instruction expected` leaves only `parser`.
    if lower.contains("nasm:")
        || lower.contains("nasm ")
        || family == ToolFamily::Nasm
        || whole_line.contains("parser: instruction expected")
    {
        return DiagnosticSource::Nasm;
    }
    // `[-Wflag]` is **not** a clang fingerprint: GNU gcc prints the same
    // bracketed flag for `-Wall`/`-Wextra` warnings, so treating it as clang
    // mislabels every gcc warning (measured: `-Wunused-variable` from gcc came
    // out as `source: clang`).  The running family decides instead.
    if family == ToolFamily::Clang {
        return DiagnosticSource::Clang;
    }
    if family == ToolFamily::Ld {
        return DiagnosticSource::Ld;
    }
    // Phrasings that only clang produces, used when the family is unknown.
    if family == ToolFamily::Unknown
        && (message.contains("use of undeclared identifier")
            || message.contains("expected ';' after")
            || message.contains("incompatible integer to pointer"))
    {
        return DiagnosticSource::Clang;
    }
    // GNU gcc's default phrasing for the classic mistakes.
    if message.contains("each undeclared identifier is reported only once")
        || message.contains("expected '=' , ',' , ';' , 'asm' or '__attribute__'")
        || message.contains("note: expected")
    {
        return DiagnosticSource::Gcc;
    }
    // ld's linker errors.
    if message.contains("undefined reference to")
        || message.contains("cannot find entry symbol")
        || message.contains("relocation truncated")
        || message.contains("final link failed")
    {
        return DiagnosticSource::Ld;
    }
    // Nothing distinctive: attribute it to the tool that is running.
    family
        .default_source()
        .unwrap_or(DiagnosticSource::Gcc)
}

/// Is this line part of the previous diagnostic's block?
fn is_continuation(trimmed: &str) -> bool {
    if trimmed.len() < 2 {
        return false;
    }
    // clang caret lines: `     ^`, `     ~~~^`, `|`
    if trimmed.starts_with('^') || trimmed.starts_with('~') || trimmed.starts_with('|') {
        return true;
    }
    if trimmed.starts_with("In file included from")
        || trimmed.starts_with("In function")
        || trimmed.starts_with("from ")
    {
        return true;
    }
    // gcc's " 5 |  if (x {  " source echo block.
    let mut parts = trimmed.splitn(2, '|');
    if let (Some(left), Some(_)) = (parts.next(), parts.next()) {
        if !left.is_empty() && left.trim().bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    false
}

/// Does this fragment carry clang's own fingerprint?
fn is_clang_marker(trimmed: &str) -> bool {
    trimmed.contains("~~~^") || trimmed.contains("[-W") || trimmed.starts_with("~~~")
}

/// Convenience for a whole captured stderr blob from a known tool.
pub fn parse_chunk(stderr: &str, cwd: &Path, family: ToolFamily) -> Vec<ParsedDiagnostic> {
    DiagnosticParser::new(cwd)
        .with_family(family)
        .feed_text_dedup(stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcc_error_gets_file_line_col_and_the_gcc_source() {
        let line = "kernel.c:7:15: error: expected ';' before '}' token";
        let raw = parse_line(line).expect("recognised");
        assert_eq!(raw.severity, DiagnosticSeverity::Error);
        assert_eq!(raw.file.as_deref(), Some("kernel.c"));
        assert_eq!(raw.line, Some(7));
        assert_eq!(raw.col, Some(15));
        assert_eq!(raw.source, DiagnosticSource::Gcc);
        assert_eq!(raw.message, "expected ';' before '}' token");
    }

    #[test]
    fn the_running_tool_decides_gcc_vs_clang_not_the_message_text() {
        // `[-Wflag]` is printed by GNU gcc as well, so it cannot be used as a
        // clang fingerprint.  The same line must come out as `gcc` from a gcc
        // run and `clang` from a clang run.
        let line = "kernel.c:9:5: warning: unused variable 'x' [-Wunused-variable]";

        let from_gcc = parse_line_for(line, ToolFamily::Gcc).unwrap();
        assert_eq!(from_gcc.severity, DiagnosticSeverity::Warning);
        assert_eq!(
            from_gcc.source,
            DiagnosticSource::Gcc,
            "a gcc warning must never be labelled clang"
        );

        let from_clang = parse_line_for(line, ToolFamily::Clang).unwrap();
        assert_eq!(from_clang.source, DiagnosticSource::Clang);
        assert_eq!(from_clang.line, Some(9));
    }

    #[test]
    fn nasm_and_ld_shapes_are_understood() {
        let nasm = parse_line("boot.s:12: error: parser: instruction expected").unwrap();
        assert_eq!(nasm.source, DiagnosticSource::Nasm);
        assert_eq!(nasm.line, Some(12));

        let ld = parse_line("ld: build/kernel.o: in function `kmain':").unwrap();
        assert_eq!(ld.file.as_deref(), Some("build/kernel.o"));
        assert_eq!(ld.line, None);
    }

    #[test]
    fn the_parser_resolves_relative_paths_against_the_build_cwd() {
        let mut parser = DiagnosticParser::new("/work/kernel");
        let diagnostics = parser.feed("src/kernel.c:3:1: error: nope");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file.as_deref(), Some("/work/kernel/src/kernel.c"));
    }

    #[test]
    fn ordinary_make_noise_is_not_a_diagnostic() {
        let noisy = "mkdir -p build\n[refkernel] built build/refkernel.elf\n\
                     gcc -m64 -c kernel.c -o build/kernel.o\n";
        let diagnostics = DiagnosticParser::new("/p").feed_text_dedup(noisy);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_recipe_line_re_attributes_the_next_diagnostic_to_the_real_compiler() {
        let mut parser = DiagnosticParser::new("/p");
        parser.feed("gcc -Wall -c kernel.c -o build/kernel.o");
        assert_eq!(parser.family(), ToolFamily::Gcc);
        let diagnostics = parser.feed("kernel.c:1:1: error: something");
        assert_eq!(diagnostics[0].source, DiagnosticSource::Gcc);

        let mut parser = DiagnosticParser::new("/p");
        parser.feed("clang -Wall -c kernel.c -o build/kernel.o");
        assert_eq!(parser.family(), ToolFamily::Clang);
        let diagnostics = parser.feed("kernel.c:1:1: error: something");
        assert_eq!(diagnostics[0].source, DiagnosticSource::Clang);
    }

    #[test]
    fn gccs_multiline_block_is_one_diagnostic_not_five() {
        // Real gcc output for one missing paren: the error line plus the source
        // echo, the caret line and the following source line.  Re-emitting on
        // each continuation line would publish five events for one problem.
        let text = "kernel.c:101:40: error: expected \u{2018})\u{2019} before \u{2018}__asm__\u{2019}\n\
                    \x20 101 |     int x = (1 + 2\n\
                    \x20     |                 ~     ^\n\
                    \x20     |                       )\n\
                    \x20 102 |     __asm__ volatile(\"ud2\": : : \"memory\");\n";
        let diagnostics = DiagnosticParser::new("/p").feed_text_dedup(text);
        assert_eq!(
            diagnostics.len(),
            1,
            "one error must yield one diagnostic: {diagnostics:#?}"
        );
        assert_eq!(diagnostics[0].line, Some(101));
        // The folded detail is still there for the UI to show.
        assert!(diagnostics[0].message.contains("int x = (1 + 2"));
    }

    #[test]
    fn duplicate_lines_are_emitted_once() {
        let text = "kernel.c:1:1: error: boom\nkernel.c:1:1: error: boom\n";
        assert_eq!(DiagnosticParser::new("/p").feed_text_dedup(text).len(), 1);
    }

    #[test]
    fn a_program_name_maps_to_its_family() {
        assert_eq!(ToolFamily::for_program("gcc-12"), ToolFamily::Gcc);
        assert_eq!(ToolFamily::for_program("/usr/bin/x86_64-elf-gcc"), ToolFamily::Gcc);
        assert_eq!(ToolFamily::for_program("clang-16"), ToolFamily::Clang);
        assert_eq!(ToolFamily::for_program("ld"), ToolFamily::Ld);
        assert_eq!(ToolFamily::for_program("ld.lld-14"), ToolFamily::Ld);
        assert_eq!(ToolFamily::for_program("nasm"), ToolFamily::Nasm);
        assert_eq!(ToolFamily::for_program("make"), ToolFamily::Unknown);
    }

    #[test]
    fn payloads_carry_the_contract_field_names() {
        let payload = DiagnosticParser::new("/p")
            .feed("a.c:2:3: error: x")
            .remove(0)
            .to_payload();
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["severity"], "error");
        assert_eq!(json["file"], "/p/a.c");
        assert_eq!(json["line"], 2);
        assert_eq!(json["col"], 3);
        assert_eq!(json["source"], "gcc");
        assert_eq!(json["message"], "x");
    }
}
