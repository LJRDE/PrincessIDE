//! `princess:tools:detect` — run `scripts/doctor.sh` and parse it.
//!
//! The shell does not reimplement toolchain detection: P0 owns the doctor, and
//! the engine's job (contract §0.2) is to *run and report*, not to guess.  The
//! parser is therefore written against doctor.sh's actual output contract:
//!
//! ```text
//! report()        printf '%-22s %-10s %s\n' "$name" "ok"        "$version"
//!                 printf '%-22s %-10s %s\n' ""      ""          "$path"
//!                 printf '%-22s %-10s %s\n' "$name" "MISSING"   "(required: $exe)"
//! expect_version()printf '%-22s %-10s %s\n' "$name" "ok"        "version matches /…/"
//!                 printf '%-22s %-10s %s\n' "$name" "WRONG VER" "$got (wanted /…/)"
//! ```
//!
//! Three properties of that output drive the parser, and each one was a real
//! bug source when assumed away:
//!
//!   1. **Tool names may contain spaces** (`clangd-16 (LSP)`, `bear (CDB)`), so a
//!      `split_whitespace()`-and-take-the-first-token parse is wrong.
//!   2. **`WRONG VER` is a status with a space in it**, and a status can be one
//!      of several values — so the status must be *recognised*, not matched
//!      against a two-element set.
//!   3. **A row may appear twice** (once from `report()` with version+path, once
//!      from `expect_version()` with a verification note), so rows are merged by
//!      name and the verification notes are carried alongside.
//!
//! The parser scans for a status marker preceded by whitespace and followed by
//! whitespace/end-of-line, taking the first hit at or after column 20.  That
//! works for both the fixed-width layout and a name that overflows the 22-column
//! field, which a column-offset parse would not.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::Command;

use crate::contract::{err, ErrorCode, IpcFailure};
use crate::ops::OpHandle;

/// Status values doctor.sh can print in the status column.
const STATUS_MARKERS: &[&str] = &["WRONG VER", "MISSING", "ok"];

/// One resolved (or missing) toolchain entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    /// Raw status as printed by doctor.sh: `ok`, `MISSING`, `WRONG VER`, …
    pub status: String,
    /// First line of the tool's version output; `None` when doctor printed no
    /// version for this tool (missing tools, and `expect_version()` checks).
    pub version: Option<String>,
    /// Absolute path doctor.sh resolved, when it printed one.  `None` does *not*
    /// imply "not found": `expect_version()` rows verify a tool without printing
    /// its path.
    pub path: Option<String>,
    /// true only for status `ok`.
    pub available: bool,
    /// true when this tool is required for the project to build.
    pub required: bool,
    /// Extra verification notes doctor.sh printed for this tool, verbatim
    /// (e.g. `version matches /clangd version 16\./`).
    pub checks: Vec<String>,
}

/// Payload of `princess:tools:detect` (§3: 路径 + 版本 + 是否可用).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsDetectData {
    pub tools: Vec<ToolInfo>,
    pub exit_code: i32,
    pub missing: Vec<String>,
    pub root: String,
    pub command: String,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

/// Locate the workspace root: `PRINCESSIDE_ROOT`, else the repo this binary was
/// built from, else walking up from the cwd.  Nothing is guessed silently —
/// every candidate is reported in the failure detail.
pub fn resolve_root() -> Result<PathBuf, IpcFailure> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("PRINCESSIDE_ROOT") {
        candidates.push(PathBuf::from(explicit));
    }
    // CARGO_MANIFEST_DIR = apps/desktop/src-tauri at build time.
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    if let Ok(cwd) = std::env::current_dir() {
        let mut probe = Some(cwd.as_path());
        while let Some(dir) = probe {
            candidates.push(dir.to_path_buf());
            probe = dir.parent();
        }
    }

    pick_root(candidates)
}

/// Pick the first candidate that really is a PrincessIDE checkout.
///
/// Split out from [`resolve_root`] so the failure path (a workspace with no
/// `scripts/doctor.sh`) is unit-testable without touching the real filesystem.
pub fn pick_root(candidates: Vec<PathBuf>) -> Result<PathBuf, IpcFailure> {
    let mut tried: Vec<String> = Vec::new();

    for candidate in candidates {
        let Ok(canonical) = candidate.canonicalize() else {
            tried.push(format!("{} (does not exist)", candidate.display()));
            continue;
        };
        if canonical.join("scripts/doctor.sh").is_file() && canonical.join("scripts/env.sh").is_file()
        {
            return Ok(canonical);
        }
        tried.push(format!("{} (no scripts/doctor.sh)", canonical.display()));
    }

    Err(IpcFailure::new(
        ErrorCode::NotFound,
        "PrincessIDE workspace root not found",
        format!(
            "looked for a directory containing scripts/doctor.sh + scripts/env.sh.\ntried:\n  {}\n\
             set PRINCESSIDE_ROOT to override.",
            tried.join("\n  ")
        ),
    ))
}

/// Split one tool line into `(name, status, value)`, or `None` when the line is
/// a continuation (path) line, a header, a separator or preamble.
pub fn split_tool_line(line: &str) -> Option<(&str, &str, &str)> {
    for marker in STATUS_MARKERS {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(marker) {
            let at = from + rel;
            let before_ok = at > 0 && line.as_bytes()[at - 1].is_ascii_whitespace();
            let after = at + marker.len();
            let after_ok = after >= line.len()
                || line.as_bytes()[after].is_ascii_whitespace()
                || line.as_bytes()[after] == b'(';
            if before_ok && after_ok {
                let name = line[..at].trim();
                if name.is_empty() {
                    return None; // continuation line (name field blank)
                }
                let value = line[after..].trim();
                return Some((name, marker, value));
            }
            from = at + marker.len();
        }
    }
    None
}

/// Parse doctor.sh stdout into tool rows, merging duplicate names.
pub fn parse_doctor_output(stdout: &str) -> Vec<ToolInfo> {
    let mut tools: Vec<ToolInfo> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Continuation (resolved path) line: doctor indents it with two empty
        // fields, so the raw line starts with whitespace.
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = tools.last_mut() {
                if last.path.is_none() {
                    let path = line.trim();
                    if !path.is_empty() && !path.starts_with('-') && !path.starts_with('=') {
                        last.path = Some(path.to_string());
                    }
                }
            }
            continue;
        }

        let Some((name, status, value)) = split_tool_line(line) else {
            continue; // header row, separator, or preamble line
        };

        let is_check = value.starts_with("version matches /") || status == "WRONG VER";

        if let Some(existing) = tools.iter_mut().find(|t| t.name == name) {
            // doctor.sh prints a verification row for a tool it already
            // reported; keep one row per tool and carry the check verbatim.
            if !value.is_empty() {
                existing.checks.push(value.to_string());
            }
            if status == "WRONG VER" {
                existing.available = false;
            }
            if status == "MISSING" {
                existing.available = false;
            }
            // A report row arriving after its own verification row still carries
            // the version and path; do not lose them.
            if !is_check {
                existing.status = status.to_string();
                if existing.version.is_none() && !value.is_empty() {
                    existing.version = Some(value.to_string());
                }
            }
            continue;
        }

        tools.push(ToolInfo {
            name: name.to_string(),
            status: status.to_string(),
            version: if is_check || value.is_empty() { None } else { Some(value.to_string()) },
            path: None,
            available: status == "ok",
            required: true, // refined by `classify_required`
            checks: if is_check && !value.is_empty() { vec![value.to_string()] } else { Vec::new() },
        });
    }

    tools
}

/// Tools that are *not* required for the engine to work.  Used because
/// doctor.sh prints `(required: <exe>)` on **every** missing row, required or
/// not — so that marker cannot decide requiredness on its own.
const OPTIONAL_TOOLS: &[&str] = &["node", "npm", "pnpm"];

/// Decide `required` for every row.
///
/// Two sources of truth are combined, in this order:
///   1. doctor.sh's **exit code**: 0 means "all required tools present", so any
///      unavailable row in a zero-exit run is by definition optional;
///   2. the known-optional list for the non-zero-exit case.
///
/// An unknown tool name defaults to required — failing loud beats silently
/// dropping a missing dependency.
pub fn classify_required(tools: &mut [ToolInfo], doctor_exit_code: i32) {
    for tool in tools.iter_mut() {
        let known_optional = OPTIONAL_TOOLS.contains(&tool.name.as_str());
        tool.required = if tool.available {
            // Present tools are required unless they are in the optional set.
            !known_optional
        } else {
            // A missing row is only fatal when doctor.sh itself failed: its exit
            // code is the authority on "something required is missing".
            doctor_exit_code != 0 && !known_optional
        };
    }
}

pub fn missing_names(tools: &[ToolInfo]) -> Vec<String> {
    tools.iter().filter(|t| !t.available && t.required).map(|t| t.name.clone()).collect()
}

/// Result of one doctor.sh invocation, before it is wrapped in the IPC envelope.
#[derive(Debug, Clone)]
pub struct DoctorRun {
    pub tools: Vec<ToolInfo>,
    pub exit_code: i32,
    pub missing: Vec<String>,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
}

/// Hard ceiling for the doctor run; a hung shell script must not hang the IDE.
pub const DOCTOR_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of a run that can be cancelled or time out.
pub enum DoctorOutcome {
    Done(DoctorRun),
    Cancelled(DoctorRun),
    TimedOut(DoctorRun),
}

/// Run `scripts/doctor.sh` with `scripts/env.sh` sourced first (contract: the
/// engine owns subprocess orchestration; `tokio::process` per §6.1).
pub async fn run_doctor(root: &Path, op: &OpHandle) -> Result<DoctorOutcome, IpcFailure> {
    run_doctor_with_env(root, op, &[]).await
}

/// Same, with extra environment variables (used by tests to drive failure paths).
pub async fn run_doctor_with_env(
    root: &Path,
    op: &OpHandle,
    extra_env: &[(String, String)],
) -> Result<DoctorOutcome, IpcFailure> {
    let env_sh = root.join("scripts/env.sh");
    let doctor_sh = root.join("scripts/doctor.sh");
    let script = format!(
        "set -o pipefail; source {env} && exec bash {doctor}",
        env = shell_quote(&env_sh),
        doctor = shell_quote(&doctor_sh)
    );
    let command_repr = format!(
        "bash -c 'source {} && exec bash {}'   (cwd {})",
        env_sh.display(),
        doctor_sh.display(),
        root.display()
    );

    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&script)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A cancelled/timed-out operation must not leave the child behind (§6.3).
        .kill_on_drop(true);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let child = command.spawn().map_err(|e| {
        IpcFailure::new(
            ErrorCode::Internal,
            "failed to spawn scripts/doctor.sh",
            format!("{command_repr}\n{e}"),
        )
    })?;

    let started = Instant::now();
    let handle = Arc::new(op.clone());

    // Race the child against cancellation and the deadline.  `interval` fires
    // immediately, so a cancel request that arrives before the spawn still wins.
    let outcome = {
        let wait = child.wait_with_output();
        tokio::pin!(wait);
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                biased;
                res = &mut wait => break Some(res),
                _ = ticker.tick() => {
                    if handle.is_cancelled() || started.elapsed() > DOCTOR_TIMEOUT {
                        break None;
                    }
                }
            }
        }
    };

    let Some(result) = outcome else {
        // Either cancelled or over the deadline; the child is killed on drop.
        let timed_out = started.elapsed() > DOCTOR_TIMEOUT;
        return Ok(if timed_out {
            DoctorOutcome::TimedOut(aborted_run(
                command_repr,
                format!("doctor.sh exceeded {}s and was killed", DOCTOR_TIMEOUT.as_secs()),
            ))
        } else {
            DoctorOutcome::Cancelled(aborted_run(
                command_repr,
                "doctor.sh cancelled by princess:op:cancel".to_string(),
            ))
        });
    };

    let out = result.map_err(|e| {
        IpcFailure::new(
            ErrorCode::Internal,
            "failed while waiting for scripts/doctor.sh",
            format!("{command_repr}\n{e}"),
        )
    })?;

    let exit_code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let mut tools = parse_doctor_output(&stdout);
    classify_required(&mut tools, exit_code);
    let missing = missing_names(&tools);

    Ok(DoctorOutcome::Done(DoctorRun {
        tools,
        exit_code,
        missing,
        command: command_repr,
        stdout,
        stderr,
    }))
}

fn aborted_run(command: String, stderr: String) -> DoctorRun {
    DoctorRun {
        tools: Vec::new(),
        exit_code: -1,
        missing: Vec::new(),
        command,
        stdout: String::new(),
        stderr,
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Convert a finished run into the §3 envelope value.
pub fn to_envelope(run: DoctorRun, root: &Path) -> serde_json::Value {
    let data = ToolsDetectData {
        tools: run.tools,
        exit_code: run.exit_code,
        missing: run.missing,
        root: root.display().to_string(),
        command: run.command,
        raw_stdout: run.stdout,
        raw_stderr: run.stderr,
    };
    crate::contract::ok(data)
}

pub fn cancelled_envelope(run: DoctorRun) -> serde_json::Value {
    err(ErrorCode::Cancelled, "toolchain detection was cancelled", run.stderr)
}

pub fn timeout_envelope(run: DoctorRun) -> serde_json::Value {
    err(ErrorCode::Timeout, "toolchain detection timed out", run.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from `bash scripts/doctor.sh` in this workspace
    /// (abridged, but every row shape is real, including the ones A1 added:
    /// names with spaces, `WRONG VER`, and the duplicated verification rows).
    const REAL_OUTPUT: &str = "\
PrincessIDE toolchain doctor
workspace : /root/PrincessIDE
prefix    : /root/PrincessIDE/.toolchain/prefix
RUSTUP_HOME=/root/PrincessIDE/.toolchain/rustup
CARGO_HOME =/root/PrincessIDE/.toolchain/cargo
-------------------------------------------------------------------------------
TOOL                   STATUS     VERSION / PATH
-------------------------------------------------------------------------------
cargo                  ok         cargo 1.98.1 (797e8a9bc 2026-08-05)
                                  /root/PrincessIDE/.toolchain/cargo/bin/cargo
qemu-system-x86_64     ok         QEMU emulator version 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/qemu-system-x86_64
clangd-16 (LSP)        ok         Debian clangd version 16.0.6 (15~deb12u1)
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16
bear (CDB)             ok         bear 3.1.1
                                  /root/PrincessIDE/.toolchain/bin/bear
clangd-16 (LSP)        ok         version matches /clangd version 16\\./
clangd-14 (legacy)     ok         version matches /clangd version 14\\./
clangd (P0 default)    ok         version matches /clangd version 14\\./
node                   ok         v24.20.0
                                  /root/node-v24.20.0-linux-x64/bin/node
pnpm                   ok         12.3.4
                                  /root/node-v24.20.0-linux-x64/bin/pnpm
-------------------------------------------------------------------------------
doctor: all required tools present (28 resolved).
";

    const OUTPUT_WITH_MISSING: &str = "\
TOOL                   STATUS     VERSION / PATH
-------------------------------------------------------------------------------
cargo                  ok         cargo 1.98.1 (797e8a9bc 2026-08-05)
                                  /root/PrincessIDE/.toolchain/cargo/bin/cargo
nasm                   MISSING    (required: nasm)
node                   MISSING    (required: node)
-------------------------------------------------------------------------------
doctor: 1 tool(s) present, MISSING REQUIRED: nasm
";

    const OUTPUT_WITH_WRONG_VERSION: &str = "\
clangd-16 (LSP)        ok         Debian clangd version 16.0.6
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16
clangd-16 (LSP)        WRONG VER  Debian clangd version 14.0.6 (wanted /clangd version 16\\./)
-------------------------------------------------------------------------------
doctor: 0 tool(s) present, MISSING REQUIRED: clangd-16 (LSP) (wrong version)
";

    #[test]
    fn parses_two_line_records() {
        let tools = parse_doctor_output(REAL_OUTPUT);
        let cargo = tools.iter().find(|t| t.name == "cargo").expect("cargo row");
        assert_eq!(cargo.status, "ok");
        assert_eq!(cargo.version.as_deref(), Some("cargo 1.98.1 (797e8a9bc 2026-08-05)"));
        assert_eq!(cargo.path.as_deref(), Some("/root/PrincessIDE/.toolchain/cargo/bin/cargo"));
        assert!(cargo.available);
        assert!(cargo.checks.is_empty());

        let qemu = tools.iter().find(|t| t.name == "qemu-system-x86_64").expect("qemu row");
        assert_eq!(
            qemu.path.as_deref(),
            Some("/root/PrincessIDE/.toolchain/prefix/usr/bin/qemu-system-x86_64")
        );
        assert!(qemu.version.as_deref().unwrap().starts_with("QEMU emulator version 7.2.22"));
    }

    #[test]
    fn handles_tool_names_containing_spaces() {
        let tools = parse_doctor_output(REAL_OUTPUT);
        let clangd16 = tools.iter().find(|t| t.name == "clangd-16 (LSP)").expect("clangd-16 row");
        assert_eq!(clangd16.status, "ok");
        assert_eq!(clangd16.version.as_deref(), Some("Debian clangd version 16.0.6 (15~deb12u1)"));
        assert_eq!(
            clangd16.path.as_deref(),
            Some("/root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16")
        );

        let bear = tools.iter().find(|t| t.name == "bear (CDB)").expect("bear row");
        assert_eq!(bear.path.as_deref(), Some("/root/PrincessIDE/.toolchain/bin/bear"));
        // A whitespace-token parse would have produced a tool literally named "bear".
        assert!(!tools.iter().any(|t| t.name == "bear"));
    }

    #[test]
    fn merges_verification_rows_into_the_tool_they_check() {
        let tools = parse_doctor_output(REAL_OUTPUT);
        let clangd16 = tools.iter().find(|t| t.name == "clangd-16 (LSP)").unwrap();
        assert_eq!(clangd16.checks, vec!["version matches /clangd version 16\\./".to_string()]);
        // The verification row must not become a second, version-less tool row.
        assert_eq!(tools.iter().filter(|t| t.name == "clangd-16 (LSP)").count(), 1);

        let legacy = tools.iter().find(|t| t.name == "clangd-14 (legacy)").unwrap();
        assert_eq!(legacy.checks.len(), 1);
        assert!(legacy.version.is_none(), "a check is not a version string");
        assert!(legacy.path.is_none(), "doctor did not print a path for a check row");
        assert!(legacy.available);
    }

    #[test]
    fn headers_and_separators_are_not_tools() {
        let tools = parse_doctor_output(REAL_OUTPUT);
        for bad in ["TOOL", "STATUS", "workspace", "doctor:", "PrincessIDE"] {
            assert!(!tools.iter().any(|t| t.name == bad), "{bad} parsed as a tool");
        }
    }

    #[test]
    fn missing_tools_have_no_path_or_version() {
        let mut tools = parse_doctor_output(OUTPUT_WITH_MISSING);
        classify_required(&mut tools, 1);
        let nasm = tools.iter().find(|t| t.name == "nasm").expect("nasm row");
        assert_eq!(nasm.status, "MISSING");
        assert!(!nasm.available);
        assert_eq!(nasm.version, None);
        assert_eq!(nasm.path, None);
        assert!(nasm.required);
        assert_eq!(missing_names(&tools), vec!["nasm".to_string()]);
    }

    #[test]
    fn doctor_exit_code_decides_requiredness_for_missing_rows() {
        // A missing *optional* tool: doctor prints "(required: node)" anyway, so
        // only the exit code tells us it is not fatal.
        let mut tools = parse_doctor_output(OUTPUT_WITH_MISSING);
        classify_required(&mut tools, 0);
        let node = tools.iter().find(|t| t.name == "node").unwrap();
        assert!(!node.available);
        assert!(!node.required, "with exit code 0 nothing required can be missing");
        assert!(missing_names(&tools).is_empty());

        // Non-zero exit: a missing optional tool is still not fatal…
        let mut tools = parse_doctor_output(OUTPUT_WITH_MISSING);
        classify_required(&mut tools, 1);
        assert!(!tools.iter().find(|t| t.name == "node").unwrap().required);
        // …but an unknown missing tool is treated as required (fail loud).
        assert!(tools.iter().find(|t| t.name == "nasm").unwrap().required);
    }

    #[test]
    fn a_wrong_version_is_visible_and_marks_the_tool_unavailable() {
        let mut tools = parse_doctor_output(OUTPUT_WITH_WRONG_VERSION);
        classify_required(&mut tools, 1);
        let row = tools.iter().find(|t| t.name == "clangd-16 (LSP)").expect("row");
        assert!(!row.available, "a WRONG VER row must not read as available");
        assert_eq!(row.checks.len(), 1);
        assert!(row.checks[0].contains("wanted /clangd version 16"));
        assert_eq!(missing_names(&tools), vec!["clangd-16 (LSP)".to_string()]);
    }

    #[test]
    fn long_tool_names_do_not_shift_the_parse() {
        let line = "a-really-long-tool-name-exceeding-22 ok        1.2.3\n                                                 /usr/bin/x\n";
        let tools = parse_doctor_output(line);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "a-really-long-tool-name-exceeding-22");
        assert_eq!(tools[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(tools[0].path.as_deref(), Some("/usr/bin/x"));
    }

    #[test]
    fn a_value_containing_the_word_ok_is_not_a_status() {
        // "broken" contains "ok" but is not a status marker.
        let line = "weird                  ok        this tool is broken ok?\n";
        let tools = parse_doctor_output(line);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "weird");
        assert_eq!(tools[0].version.as_deref(), Some("this tool is broken ok?"));
    }

    #[test]
    fn pick_root_reports_every_candidate_it_tried() {
        let bogus = std::env::temp_dir().join("princesside-p3-does-not-exist");
        let failure = pick_root(vec![bogus.clone(), PathBuf::from("/definitely/not/here")])
            .expect_err("no candidate is a workspace");
        assert_eq!(failure.code, ErrorCode::NotFound);
        assert_eq!(failure.code.as_str(), "E_NOT_FOUND");
        assert!(failure.detail.contains(&bogus.display().to_string()), "{}", failure.detail);
        assert!(failure.detail.contains("/definitely/not/here"), "{}", failure.detail);
        assert!(failure.detail.contains("PRINCESSIDE_ROOT"), "{}", failure.detail);
    }

    #[test]
    fn split_tool_line_ignores_preamble_and_path_lines() {
        assert_eq!(split_tool_line("workspace : /root/PrincessIDE"), None);
        assert_eq!(split_tool_line("TOOL                   STATUS     VERSION / PATH"), None);
        assert_eq!(split_tool_line("                                  /usr/bin/gcc"), None);
        assert_eq!(
            split_tool_line("gcc                    ok         gcc (Debian 12.2.0-14+deb12u1) 12.2.0"),
            Some(("gcc", "ok", "gcc (Debian 12.2.0-14+deb12u1) 12.2.0"))
        );
    }
}
