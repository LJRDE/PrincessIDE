//! Build diagnostic parsing (`build.diagnostic`, contract §2).
//!
//! Recognised shapes, in the order they are tried:
//!
//! ```text
//! file.c:12:5: error: expected ';' before '}' token      (gcc/clang, with column)
//! file.c:12: warning: unused variable 'x'                (gcc/clang, no column)
//! kernel.asm:5: error: symbol `x' not defined            (nasm, with column)
//! ld: build/kernel.o: undefined reference to `memcpy'    (linker)
//! collect2: error: ld returned 1 exit status             (driver)
//! ```
//!
//! Anything else (progress lines, `make: *** [...] Error 1`, `In file included
//! from ...`) is left to the raw `log.append` stream and not turned into a
//! diagnostic — inventing a line number would be worse than reporting none.
//!
//! `file` is resolved against the build working directory, because a compiler
//! invoked by `make` reports paths relative to where it was run (contract §4:
//! relative paths are the engine's job).

use std::path::{Path, PathBuf};

use princess_core::types::{DiagnosticSeverity, DiagnosticSource};
use princess_core::BuildDiagnosticPayload;

/// Severity tokens that may appear before the message.
fn severity_of(token: &str) -> Option<DiagnosticSeverity> {
    match token {
        "error" | "fatal" => Some(DiagnosticSeverity::Error),
        "warning" => Some(DiagnosticSeverity::Warning),
        "note" => Some(DiagnosticSeverity::Note),
        _ => None,
    }
}

/// Source attribution for a diagnostic line.
fn source_for_file(file: Option<&str>, default_source: DiagnosticSource) -> DiagnosticSource {
    match file {
        Some(path) if path.ends_with(".asm") || path.ends_with(".nasm") => DiagnosticSource::Nasm,
        _ => default_source,
    }
}

/// Parse one output line into a diagnostic, or `None`.
pub fn parse_line(
    line: &str,
    cwd: &Path,
    default_source: DiagnosticSource,
) -> Option<BuildDiagnosticPayload> {
    let line = line.trim_end_matches(['\r', '\n']).trim();
    if line.is_empty() {
        return None;
    }

    // --- `tool: [severity: ]message` (no source location) --------------------
    if let Some((head, tail)) = line.split_once(": ") {
        let head = head.trim();
        let is_linker = head == "ld" || head.ends_with("/ld") || head == "collect2" || head.ends_with("/collect2");
        let is_driver = matches!(head, "cc1" | "clang" | "gcc" | "nasm");
        if is_linker || is_driver {
            let (first, rest) = match tail.split_once(": ") {
                Some((first, rest)) => (first.trim(), Some(rest.trim())),
                None => (tail.trim(), None),
            };
            let severity = severity_of(first);
            // Without an explicit severity token only linker output is treated
            // as an error: `ld:` prints nothing on success, but a bare `gcc:`
            // line can be informational.
            if let Some(severity) = severity.or(if is_linker {
                Some(DiagnosticSeverity::Error)
            } else {
                None
            }) {
                let message = match rest {
                    Some(rest) if !rest.is_empty() => format!("{first}: {rest}"),
                    _ => first.to_string(),
                };
                return Some(BuildDiagnosticPayload {
                    severity,
                    file: None,
                    line: None,
                    col: None,
                    message,
                    source: if is_linker {
                        DiagnosticSource::Ld
                    } else {
                        default_source
                    },
                });
            }
        }
    }

    // --- `file:line[:col]: severity: message` --------------------------------
    let first_colon = line.find(':')?;
    let file = &line[..first_colon];
    if file.is_empty() || file.contains(' ') || file.contains('\t') {
        return None;
    }
    let rest = &line[first_colon + 1..];

    let (line_number, rest) = take_number(rest)?;
    let (col, rest) = match take_number(rest.strip_prefix(':')?) {
        Some((col, rest)) => (Some(col), rest),
        None => (None, rest),
    };
    let rest = rest.strip_prefix(':')?.trim_start();
    let (token, message) = match rest.split_once(':') {
        Some((token, message)) => (token.trim(), message.trim()),
        None => (rest.trim(), ""),
    };
    let severity = severity_of(token)?;

    let file_text = resolve_file(file, cwd);
    Some(BuildDiagnosticPayload {
        severity,
        file: Some(file_text.clone()),
        line: Some(line_number),
        col,
        message: if message.is_empty() {
            token.to_string()
        } else {
            format!("{token}: {message}")
        },
        source: source_for_file(Some(&file_text), default_source),
    })
}

/// Split a leading decimal number off `text`.
fn take_number(text: &str) -> Option<(u32, &str)> {
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let value: u32 = digits.parse().ok()?;
    Some((value, &text[digits.len()..]))
}

/// Make a diagnostic path absolute so the UI can open it directly.
fn resolve_file(file: &str, cwd: &Path) -> String {
    let path = Path::new(file);
    if path.is_absolute() {
        return file.to_string();
    }
    let joined: PathBuf = cwd.join(path);
    princess_core::config::normalize(&joined)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Option<BuildDiagnosticPayload> {
        parse_line(line, Path::new("/proj"), DiagnosticSource::Gcc)
    }

    #[test]
    fn gcc_errors_with_and_without_a_column() {
        let diagnostic = parse("kernel.c:12:5: error: expected expression before ';' token").unwrap();
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostic.file.as_deref(), Some("/proj/kernel.c"));
        assert_eq!(diagnostic.line, Some(12));
        assert_eq!(diagnostic.col, Some(5));
        assert_eq!(diagnostic.message, "error: expected expression before ';' token");
        assert_eq!(diagnostic.source, DiagnosticSource::Gcc);

        let diagnostic = parse("kernel.c:100: warning: unused variable 'x'").unwrap();
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Warning);
        assert_eq!(diagnostic.line, Some(100));
        assert_eq!(diagnostic.col, None);
    }

    #[test]
    fn absolute_and_subdirectory_paths_are_preserved() {
        let diagnostic = parse("/abs/dir/kernel.c:3:1: error: nope").unwrap();
        assert_eq!(diagnostic.file.as_deref(), Some("/abs/dir/kernel.c"));

        let diagnostic = parse("src/./sub/../kernel.c:3:1: error: nope").unwrap();
        assert_eq!(diagnostic.file.as_deref(), Some("/proj/src/kernel.c"));
    }

    #[test]
    fn nasm_and_linker_diagnostics() {
        let diagnostic = parse("boot.asm:42: error: symbol `kmain' not defined").unwrap();
        assert_eq!(diagnostic.source, DiagnosticSource::Nasm);
        assert_eq!(diagnostic.line, Some(42));

        let diagnostic = parse("ld: build/kernel.o: undefined reference to `memcpy'").unwrap();
        assert_eq!(diagnostic.source, DiagnosticSource::Ld);
        assert_eq!(diagnostic.file, None);
        assert_eq!(diagnostic.line, None);

        let diagnostic = parse("collect2: error: ld returned 1 exit status").unwrap();
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostic.source, DiagnosticSource::Ld);

        // A linker line with no severity token is still an error (ld is silent
        // when it succeeds).
        let diagnostic = parse("/usr/bin/ld: cannot find -lnosuch: No such file or directory").unwrap();
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostic.source, DiagnosticSource::Ld);
        assert_eq!(diagnostic.file, None);
        assert!(diagnostic.message.contains("cannot find -lnosuch"), "{diagnostic:?}");
    }

    #[test]
    fn non_diagnostics_are_not_invented() {
        for line in [
            "",
            "make: *** [Makefile:28: build/kernel.o] Error 1",
            "In file included from kernel.c:3:",
            "gcc -m64 -c kernel.c -o build/kernel.o",
            "[refkernel] built build/refkernel.elf",
            "make: Entering directory '/proj'",
            "kernel.c:12:5: something: not a severity",
            "kernel.c: not a number: error: nope",
        ] {
            assert!(parse(line).is_none(), "unexpected diagnostic for {line:?}");
        }
    }

    #[test]
    fn make_style_errors_are_left_to_the_raw_stream() {
        // `make`'s own error line must not become a fake diagnostic at
        // "Makefile:28".
        assert!(parse("make: *** [Makefile:28: build/kernel.o] Error 1").is_none());
    }
}
