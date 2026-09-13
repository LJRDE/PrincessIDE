//! The `jvm` run backend for Java language module (D31/P-F1).
//!
//! This module implements the [`RunBackend`] trait for Java projects using
//! the Java Virtual Machine (JVM) as the runtime. It is the first non-QEMU
//! run backend in PrincessIDE, demonstrating the language module layer (M13).
//!
//! ## Design Decisions
//!
//! - **D31**: Language modules are pluggable; JVM is the run backend for Java
//! - **D4**: C+asm remains primary path; Java is optional module
//! - **P-F1**: Run adapter for Java (java -jar / classpath)
//!
//! ## Event Contract
//!
//! The event order for a Java run is:
//!
//! ```text
//! run.started → log.append(ide) → run.exited
//! ```
//!
//! JVM stdout/stderr are streamed as `log.append(ide)` events (not `serial.com1`),
//! because the contract's `LogStream` enum is a closed set and `jvm.stdout` would
//! break it.  The `ide` stream is the correct choice for non-kernel language runtimes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use princess_core::config::{RunBackendKind, ResolvedProject};
use princess_core::event::{
    EventBody, LogAppendPayload, RunExitedPayload, RunStartedPayload,
};
use princess_core::traits::{BootMedium, CancelToken, EventSink, RunBackend, RunPlan, RunOutcome};
use princess_core::types::{ExitReason, LogStream, SerialCapture, TextEncoding};
use princess_core::{PrincessError, Result};

use crate::process::{self, FakeRunner, Pipe, ProcessOutcome, ProcessRunner, SpawnSpec, StreamChunk, SystemRunner};

/// Default run deadline for Java programs.
pub const DEFAULT_JVM_TIMEOUT_MS: u64 = 30 * 1000;

/// The `jvm` run backend for Java projects.
pub struct JvmBackend<R: ProcessRunner> {
    runner: R,
    /// Deadline handed to java.
    pub timeout_ms: u64,
}

impl JvmBackend<SystemRunner> {
    /// Build a backend with the system runner.
    pub fn new() -> Self {
        Self {
            runner: SystemRunner::new(),
            timeout_ms: DEFAULT_JVM_TIMEOUT_MS,
        }
    }
}

impl<R: ProcessRunner> JvmBackend<R> {
    /// Build a backend with a custom runner (for testing).
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            timeout_ms: DEFAULT_JVM_TIMEOUT_MS,
        }
    }

    /// Plan the java invocation.
    fn plan_java(&self, project: &ResolvedProject) -> Result<RunPlan> {
        // Find java executable
        let java = which("java").ok_or_else(|| {
            PrincessError::new(
                princess_core::ErrorCode::ToolchainMissing,
                "java not found in PATH",
            )
        })?;

        // Determine the main class or jar file
        // For now, we'll use the classpath approach with build directory
        let build_dir = project.root.join("build");
        let main_class = "Main"; // Default main class name

        let mut argv = vec![java.to_string_lossy().to_string()];

        // Add classpath
        argv.push("-cp".to_string());
        argv.push(build_dir.to_string_lossy().to_string());

        // Add main class
        argv.push(main_class.to_string());

        Ok(RunPlan {
            backend: "jvm".to_string(),
            argv,
            cwd: project.root.clone(),
            boot_medium: BootMedium::Kernel(project.root.join("build")), // Not really a kernel, but we need something
            boot: princess_core::config::BootProtocol::Raw, // JVM doesn't use boot protocols
            timeout_ms: self.timeout_ms,
            serial: SerialCapture::default(),
            gdb_stub: None,
        })
    }
}

impl<R: ProcessRunner> RunBackend for JvmBackend<R> {
    fn id(&self) -> &str {
        "jvm"
    }

    fn plan(&self, project: &ResolvedProject, _medium: &BootMedium) -> Result<RunPlan> {
        self.plan_java(project)
    }

    fn launch(
        &self,
        plan: &RunPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<RunOutcome> {
        let start = Instant::now();

        // Emit run.started
        events.emit(EventBody::RunStarted(RunStartedPayload {
            qemu_argv: plan.argv.clone(),
            gdb_stub: None,
        }))?;

        // Create spawn spec
        let spec = SpawnSpec {
            argv: plan.argv.clone(),
            cwd: plan.cwd.clone(),
            timeout: Some(Duration::from_millis(plan.timeout_ms)),
            serial_tee: None,
            env: BTreeMap::new(),
        };

        // Run java with chunk callback
        let mut chunks = Vec::new();
        let output = self.runner.run(&spec, cancel, &mut |chunk| {
            chunks.push(chunk);
        })?;

        // Stream chunks as log.append(ide)
        // JVM stdout/stderr go to the `ide` stream (not `serial.com1`)
        // because the contract's LogStream enum is closed and `jvm.stdout` would break it
        for chunk in &chunks {
            events.emit(EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::Ide,
                chunk: chunk.text.clone(),
                encoding: TextEncoding::Utf8,
            }))?;
        }

        let uptime_ms = start.elapsed().as_millis() as u64;

        // Determine exit reason
        let exit_facts = princess_core::ExitFacts {
            exit_code: output.exit_code,
            signal: output.signal,
            timed_out: output.timed_out,
            cancelled: output.cancelled,
            guest_powered_off: false, // JVM doesn't have ACPI shutdown
            triple_fault: false, // JVM doesn't have CPU faults
        };
        let reason = princess_core::classify_exit(&exit_facts);

        // Emit run.exited
        let outcome = RunOutcome {
            exit_code: output.exit_code,
            reason,
            uptime_ms,
            fault: None, // JVM doesn't have CPU faults
            serial_log: None, // JVM doesn't tee to file
        };

        events.emit(EventBody::RunExited(outcome.exited_event()))?;

        Ok(outcome)
    }

    fn serial(&self, _plan: &RunPlan) -> SerialCapture {
        // JVM doesn't use serial capture
        SerialCapture::default()
    }

    fn stop(&self, _op_id: &str) -> Result<()> {
        // Stop is handled by the process runner
        Ok(())
    }

    fn classify_exit(&self, facts: &princess_core::ExitFacts) -> ExitReason {
        princess_core::classify_exit(facts)
    }
}

/// Find an executable on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let full = Path::new(dir).join(name);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::FakeRunner;
    use princess_core::event::EventRecorder;

    #[test]
    fn jvm_backend_id_is_jvm() {
        let backend = JvmBackend::new();
        assert_eq!(backend.id(), "jvm");
    }

    #[test]
    fn jvm_backend_plan_requires_java() {
        // Temporarily remove java from PATH for this test
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", "/tmp/empty");

        let backend = JvmBackend::new();
        let project = ResolvedProject {
            root: PathBuf::from("/tmp/test"),
            build_cwd: PathBuf::from("/tmp/test"),
            build_command: None,
            targets: vec![],
            declared_artifacts: vec![],
            compile_commands: None,
            kernel: None,
            serial_tee_to_file: None,
            symbols: None,
            config: Default::default(),
        };

        let err = backend.plan(&project, &BootMedium::Kernel(PathBuf::new())).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::ToolchainMissing);

        // Restore PATH
        std::env::set_var("PATH", old_path);
    }

    #[test]
    fn jvm_backend_launch_success() {
        let runner = FakeRunner {
            chunks: vec![
                StreamChunk {
                    pipe: Pipe::Serial, // JVM stdout goes to serial (like QEMU)
                    text: "PrincessIDE java fixture ok\n".to_string(),
                    encoding: TextEncoding::Utf8,
                    at_eof: false,
                },
            ],
            outcome: Some(ProcessOutcome {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                cancelled: false,
                wall_ms: 100,
                group_dead: true,
            }),
            seen: std::sync::Mutex::new(Vec::new()),
            delay: None,
        };

        let backend = JvmBackend::with_runner(runner);
        let project = ResolvedProject {
            root: PathBuf::from("/tmp/test"),
            build_cwd: PathBuf::from("/tmp/test"),
            build_command: None,
            targets: vec![],
            declared_artifacts: vec![],
            compile_commands: None,
            kernel: None,
            serial_tee_to_file: None,
            symbols: None,
            config: Default::default(),
        };

        let plan = RunPlan {
            backend: "jvm".to_string(),
            argv: vec!["java".to_string(), "-cp".to_string(), "/tmp/test/build".to_string(), "Main".to_string()],
            cwd: PathBuf::from("/tmp/test"),
            boot_medium: BootMedium::Kernel(PathBuf::new()),
            boot: princess_core::config::BootProtocol::Raw,
            timeout_ms: 10000,
            serial: SerialCapture::default(),
            gdb_stub: None,
        };

        let mut events = EventRecorder::new(Vec::new());
        let cancel = CancelToken::new();

        let outcome = backend.launch(&plan, &mut events, &cancel).unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.reason, ExitReason::GuestShutdown);
    }

    #[test]
    fn jvm_backend_launch_failure() {
        let runner = FakeRunner {
            chunks: vec![
                StreamChunk {
                    pipe: Pipe::Emulator, // JVM stderr goes to emulator (like QEMU)
                    text: "Error: Could not find or load main class Main\n".to_string(),
                    encoding: TextEncoding::Utf8,
                    at_eof: false,
                },
            ],
            outcome: Some(ProcessOutcome {
                exit_code: Some(1),
                signal: None,
                timed_out: false,
                cancelled: false,
                wall_ms: 100,
                group_dead: true,
            }),
            seen: std::sync::Mutex::new(Vec::new()),
            delay: None,
        };

        let backend = JvmBackend::with_runner(runner);
        let project = ResolvedProject {
            root: PathBuf::from("/tmp/test"),
            build_cwd: PathBuf::from("/tmp/test"),
            build_command: None,
            targets: vec![],
            declared_artifacts: vec![],
            compile_commands: None,
            kernel: None,
            serial_tee_to_file: None,
            symbols: None,
            config: Default::default(),
        };

        let plan = RunPlan {
            backend: "jvm".to_string(),
            argv: vec!["java".to_string(), "-cp".to_string(), "/tmp/test/build".to_string(), "Main".to_string()],
            cwd: PathBuf::from("/tmp/test"),
            boot_medium: BootMedium::Kernel(PathBuf::new()),
            boot: princess_core::config::BootProtocol::Raw,
            timeout_ms: 10000,
            serial: SerialCapture::default(),
            gdb_stub: None,
        };

        let mut events = EventRecorder::new(Vec::new());
        let cancel = CancelToken::new();

        let outcome = backend.launch(&plan, &mut events, &cancel).unwrap();
        assert_eq!(outcome.exit_code, Some(1));
        assert_eq!(outcome.reason, ExitReason::Killed); // Non-zero exit is "killed" in QEMU classification
    }
}
