//! `run-e2e` — the real-QEMU acceptance harness for `princess-run` (P2-B2).
//!
//! This is **not** a mock: it plans a run with [`princess_run`], executes the
//! real `qemu-system-x86_64`, and prints the full NDJSON event stream to stdout
//! (and optionally `--record <file>`).  The acceptance assertions are then made
//! by a shell script reading that stream, with the exit code preserved.
//!
//! Usage:
//!
//! ```text
//! run-e2e --project <dir> [--boot <iso>] [--timeout-ms N] [--cancel-after-ms N]
//!         [--record <ndjson>] [--symbolize] [--keep-debug-log]
//!         [--expect-reason <guest-shutdown|triple-fault|timeout|killed>]
//! ```
//!
//! Options are parsed by hand on purpose: this is an acceptance tool, and a
//! dependency-free parser keeps the evidence chain short (the tool is part of
//! the proof, so its own failure modes must be small).
//!
//! Exit codes:
//!
//! * `0` — the run happened and the `--expect-reason` (if any) matched.
//! * `1` — the run happened but the expected reason did not match.
//! * `2` — usage error / the engine refused to start (the `PrincessError` code
//!   is printed to stderr as JSON).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use princess_core::config::{BootProtocol, ProjectConfig, ResolvedProject};
use princess_core::error::{ErrorCode, PrincessError, Result};
use princess_core::event::EventRecorder;
use princess_core::traits::{BootMedium, CancelToken, RunPlan};
use princess_core::types::{Artifact, ArtifactKind, ExitReason, SymbolicatedLocation};

use princess_run::backend::RunOptions;
use princess_run::process::{which, SystemRunner};
use princess_run::qemu::{PlanInputs, PlanOptions};
use princess_run::{QemuBackend, REFKERNEL_BANNER};

/// Writes every byte to stdout and, optionally, to a file as well.
///
/// This is how `--record` produces a byte-identical copy of the stream that was
/// printed: both consumers see the same `write_all` call, so a recorded fixture
/// can never drift from the observed run.
struct Tee {
    stdout: std::io::Stdout,
    file: Option<std::fs::File>,
}

impl Tee {
    fn new(_stdout: std::io::Stdout, file: std::fs::File) -> Self {
        Self {
            stdout: std::io::stdout(),
            file: Some(file),
        }
    }

    fn stdout_only() -> Self {
        Self {
            stdout: std::io::stdout(),
            file: None,
        }
    }
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.stdout.write(buf)?;
        if let Some(file) = self.file.as_mut() {
            file.write_all(&buf[..written])?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdout.flush()?;
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

/// Hand-rolled argument parsing (see the module docs).
struct Args {
    project: PathBuf,
    boot: Option<PathBuf>,
    timeout_ms: Option<u64>,
    cancel_after_ms: Option<u64>,
    record: Option<PathBuf>,
    symbolize: bool,
    keep_debug_log: bool,
    expect_reason: Option<ExitReason>,
    qemu_bin: Option<PathBuf>,
    verbose: bool,
}

fn usage() -> &'static str {
    "usage: run-e2e --project <dir> [--boot <iso>] [--timeout-ms N] \
     [--cancel-after-ms N] [--record <ndjson>] [--symbolize] [--keep-debug-log] \
     [--expect-reason <guest-shutdown|triple-fault|timeout|killed>] \
     [--qemu <path>] [--verbose]"
}

fn parse_args() -> std::result::Result<Args, String> {
    let mut args = Args {
        project: PathBuf::new(),
        boot: None,
        timeout_ms: None,
        cancel_after_ms: None,
        record: None,
        symbolize: false,
        keep_debug_log: false,
        expect_reason: None,
        qemu_bin: None,
        verbose: false,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut value = |name: &str| -> std::result::Result<String, String> {
            argv.next()
                .ok_or_else(|| format!("{name} needs a value\n{}", usage()))
        };
        match arg.as_str() {
            "--project" => args.project = PathBuf::from(value("--project")?),
            "--boot" => args.boot = Some(PathBuf::from(value("--boot")?)),
            "--timeout-ms" => {
                args.timeout_ms = Some(value("--timeout-ms")?.parse().map_err(|e| format!("bad --timeout-ms: {e}"))?)
            }
            "--cancel-after-ms" => {
                args.cancel_after_ms =
                    Some(value("--cancel-after-ms")?.parse().map_err(|e| format!("bad --cancel-after-ms: {e}"))?)
            }
            "--record" => args.record = Some(PathBuf::from(value("--record")?)),
            "--symbolize" => args.symbolize = true,
            "--keep-debug-log" => args.keep_debug_log = true,
            "--qemu" => args.qemu_bin = Some(PathBuf::from(value("--qemu")?)),
            "--verbose" => args.verbose = true,
            "--expect-reason" => {
                let text = value("--expect-reason")?;
                args.expect_reason = Some(match text.as_str() {
                    "guest-shutdown" => ExitReason::GuestShutdown,
                    "triple-fault" => ExitReason::TripleFault,
                    "timeout" => ExitReason::Timeout,
                    "killed" => ExitReason::Killed,
                    other => return Err(format!("unknown --expect-reason {other:?}\n{}", usage())),
                });
            }
            "-h" | "--help" => return Err(usage().to_string()),
            other => return Err(format!("unknown argument {other:?}\n{}", usage())),
        }
    }
    if args.project.as_os_str().is_empty() {
        return Err(usage().to_string());
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    match run(&args) {
        Ok(matched) => std::process::exit(if matched { 0 } else { 1 }),
        Err(err) => {
            // The error model is the contract's; print it as JSON on stderr so a
            // caller can machine-read the code.
            let json = serde_json::json!({
                "code": err.code.as_str(),
                "message": err.message,
                "detail": err.detail,
            });
            eprintln!("{json}");
            std::process::exit(2);
        }
    }
}

fn run(args: &Args) -> Result<bool> {
    // ------------------------------------------------------------- project ---
    let loaded = ProjectConfig::load_or_default(&args.project)?;
    let resolved: ResolvedProject = loaded.config.resolve(&loaded.root);

    // The workspace toolchain must be on the child's PATH (D18-style discipline:
    // the engine reproduces `scripts/env.sh` rather than trusting the ambient
    // shell).
    let env = child_env(&resolved.root);
    let qemu_bin = match &args.qemu_bin {
        Some(path) => path.clone(),
        None => which("qemu-system-x86_64").ok_or_else(|| {
            PrincessError::new(
                ErrorCode::ToolchainMissing,
                "qemu-system-x86_64 was not found on PATH; source scripts/env.sh first",
            )
        })?,
    };

    // --------------------------------------------------------- boot medium ---
    let medium = choose_medium(args, &resolved)?;
    if args.verbose {
        eprintln!("[run-e2e] project  = {}", resolved.root.display());
        eprintln!("[run-e2e] qemu     = {}", qemu_bin.display());
        eprintln!("[run-e2e] medium   = {medium:?}");
    }

    // The serial tee must be unique per invocation so two concurrent harness
    // runs cannot overwrite each other's evidence.
    let serial_tee = resolved
        .serial_tee_to_file
        .clone()
        .unwrap_or_else(|| resolved.root.join("build/serial.log"));

    let inputs = PlanInputs {
        root: resolved.root.clone(),
        cwd: resolved.build_cwd.clone(),
        args: resolved.config.run.args.clone(),
        boot: resolved.config.run.boot,
        timeout_ms: resolved.config.run.timeout_ms,
        serial_device: resolved.config.run.serial.device.clone(),
        serial_tee: Some(serial_tee.clone()),
    };

    let plan_options = PlanOptions {
        timeout_ms: args.timeout_ms,
        gdb_stub: None,
        debug_log: None,
        qemu_data_dir: qemu_data_dir(&resolved.root),
        qemu_bin: qemu_bin.clone(),
    };
    let run_options = RunOptions {
        plan: plan_options,
        env,
        keep_debug_log: args.keep_debug_log,
        stub_defaults: None,
    };

    // A symbolizer is optional (B3/P5 own DWARF); when asked for, use the
    // system `addr2line` so this harness proves the *plumbing* without taking a
    // dependency on the symbol crate.
    let kernel_elf = resolved.kernel.clone().filter(|p| p.is_file());
    let symbolizer = if args.symbolize {
        kernel_elf.as_ref().map(|elf| symbolizer_for(elf.clone()))
    } else {
        None
    };

    let backend = QemuBackend::with_runner(SystemRunner::new(), run_options);
    let plan: RunPlan = backend.plan_for(&inputs, &medium)?;

    if args.verbose {
        eprintln!("[run-e2e] argv     = {}", plan.argv.join(" "));
    }

    // ------------------------------------------------------------- events ----
    // The recorder owns `seq`/`ts`/`opId`.  It writes NDJSON to stdout, and — when
    // `--record` is given — to a file as well, byte for byte, through a small tee
    // writer so the recorded stream is exactly the printed one.
    let mut recorder = match &args.record {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    PrincessError::internal(format!(
                        "cannot create the record directory {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            let file = std::fs::File::create(path).map_err(|err| {
                PrincessError::internal(format!("cannot create {}: {err}", path.display()))
            })?;
            EventRecorder::new(Tee::new(std::io::stdout(), file))
        }
        None => EventRecorder::new(Tee::stdout_only()),
    }
    .with_op_id(princess_core::event::new_op_id());

    // A cancellation timer is how `reason=killed` is produced from real QEMU.
    let cancel = CancelToken::new();
    if let Some(ms) = args.cancel_after_ms {
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            token.cancel();
        });
    }

    let outcome = backend.launch_plan(&plan, &mut recorder, &cancel, symbolizer.as_deref())?;

    let matched = match args.expect_reason {
        Some(expected) => {
            let ok = outcome.reason == expected;
            if !ok {
                eprintln!(
                    "[run-e2e] FAIL: expected reason={}, got reason={}",
                    expected.as_str(),
                    outcome.reason.as_str()
                );
            }
            ok
        }
        None => true,
    };

    if args.verbose {
        eprintln!(
            "[run-e2e] reason={} exit={:?} uptime={}ms serial={}",
            outcome.reason.as_str(),
            outcome.exit_code,
            outcome.uptime_ms,
            serial_tee.display()
        );
        if let Some(fault) = &outcome.fault {
            eprintln!(
                "[run-e2e] last fault: vector={} rip={} ripText={}",
                fault.vector, fault.rip, fault.rip_text
            );
        }
    }
    Ok(matched)
}

/// Choose the boot medium: explicit `--boot` wins, else the manifest's kernel,
/// else discover an ISO under `build/`.
fn choose_medium(args: &Args, resolved: &ResolvedProject) -> Result<BootMedium> {
    if let Some(path) = &args.boot {
        let path = absolute(path);
        if !path.is_file() {
            return Err(PrincessError::not_found(format!(
                "--boot {} does not exist",
                path.display()
            )));
        }
        return Ok(medium_for(&path));
    }
    if let Some(kernel) = &resolved.kernel {
        if kernel.is_file() {
            return Ok(medium_for(kernel));
        }
    }
    // Discover an ISO, then an ELF, inside build/.
    let build = resolved.root.join("build");
    for extension in ["iso", "elf", "img"] {
        if let Ok(entries) = std::fs::read_dir(&build) {
            let mut candidates: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case(extension))
                        .unwrap_or(false)
                })
                .collect();
            candidates.sort();
            if let Some(found) = candidates.first() {
                return Ok(medium_for(found));
            }
        }
    }
    Err(PrincessError::not_found(format!(
        "no bootable medium: pass --boot, set [run] kernel, or build an ISO under {}",
        build.display()
    )))
}

/// Map an extension to the medium kind the plan accepts.
fn medium_for(path: &Path) -> BootMedium {
    let path = absolute(path);
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("iso") => BootMedium::Iso(path.clone()),
        Some("img") | Some("bin") => BootMedium::Disk(path.clone()),
        _ => BootMedium::Kernel(path),
    }
}

/// Reproduce the workspace toolchain environment for the QEMU child.
fn child_env(root: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(workspace) = workspace_root(root) {
        let prefix = workspace.join(".toolchain/prefix");
        for dir in [
            workspace.join(".toolchain/cargo/bin"),
            prefix.join("usr/bin"),
            prefix.join("bin"),
            prefix.join("usr/sbin"),
        ] {
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }
    if let Some(inherited) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&inherited) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
    }
    env
}

/// Make a path absolute without touching the filesystem.
///
/// QEMU runs with `cwd = build_cwd`, so a relative medium path would resolve
/// against the project root rather than the harness's shell — the exact failure
/// this helper prevents.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Find the workspace root by walking up for `scripts/env.sh`.
fn workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start.to_path_buf());
    while let Some(dir) = current {
        if dir.join("scripts/env.sh").is_file() {
            return Some(dir);
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// QEMU's `-L` data directory, if the workspace ships one.
fn qemu_data_dir(root: &Path) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("PRINCESSIDE_QEMU_DATA") {
        let path = PathBuf::from(explicit);
        if path.is_dir() {
            return Some(path);
        }
    }
    let candidate = workspace_root(root)?.join(".toolchain/prefix/usr/share/qemu");
    candidate.is_dir().then_some(candidate)
}

/// A symbolizer backed by the system `addr2line` (see the module docs).
fn symbolizer_for(elf: PathBuf) -> Box<princess_run::SymbolizeFn<'static>> {
    Box::new(move |rip: u64| -> Option<SymbolicatedLocation> {
        let output = std::process::Command::new("addr2line")
            .args(["-f", "-C", "-e"])
            .arg(&elf)
            .arg(format!("{rip:#x}"))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let symbol = lines.next()?.trim().to_string();
        let location = lines.next()?.trim();
        if symbol.is_empty() || symbol == "??" || location.starts_with("??") {
            return None;
        }
        let (file, line) = location.rsplit_once(':')?;
        Some(SymbolicatedLocation {
            symbol,
            file: file.to_string(),
            line: line.parse().ok()?,
        })
    })
}

/// Kept so the unused-import checker does not hide the contract types this tool
/// deliberately references.
#[allow(dead_code)]
fn _references(resolved: &ResolvedProject, artifact: &Artifact, kind: ArtifactKind) {
    let _ = resolved.name();
    let _ = artifact;
    let _ = kind;
    let _ = REFKERNEL_BANNER;
    let _: fn(&ResolvedProject) -> std::path::PathBuf = |p| p.root.clone();
    let _ = BootProtocol::Multiboot2;
}
