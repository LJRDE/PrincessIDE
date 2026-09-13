//! The `javac` build backend for Java language module (D31/P-F1).
//!
//! This module implements the [`BuildBackend`] trait for Java projects using
//! `javac` as the compiler. It is the first non-kernel build backend in
//! PrincessIDE, demonstrating the language module layer (M13).
//!
//! ## Design Decisions
//!
//! - **D31**: Language modules are pluggable; javac is the build backend for Java
//! - **D4**: C+asm remains primary path; Java is optional module
//! - **D19**: Network is slow; javac is zero-download (already in JDK)
//! - **P-F1**: Build adapter for Java (javac direct compilation)
//!
//! ## Event Contract
//!
//! The event order for a Java build is identical to the kernel build:
//!
//! ```text
//! build.started → log.append(build) → build.diagnostic* → artifact.changed*
//!               → build.finished
//! ```
//!
//! Diagnostics from javac are parsed and mapped to `build.diagnostic` events,
//! reusing the existing diagnostic panel and contract (no new event types).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use princess_core::config::{BuildBackendKind, ProjectConfig, ResolvedProject};
use princess_core::event::{
    ArtifactChangedPayload, BuildDiagnosticPayload, BuildFinishedPayload, BuildStartedPayload,
    EventBody, LogAppendPayload,
};
use princess_core::traits::{BuildBackend, BuildPlan, CancelToken, EventSink};
use princess_core::types::{
    Artifact, ArtifactKind, BuildStatus, DiagnosticSeverity, DiagnosticSource, LogStream, TextEncoding,
    ToolchainReport, ToolInfo,
};
use princess_core::{PrincessError, Result};

use crate::diagnostics::{dedup, DiagnosticParser, ParsedDiagnostic, ToolFamily};
use crate::process::{self, CommandOutput, CommandRunner, FakeRunner, StdioChunk, StdioStream, SystemRunner};
use crate::toolchain::{self, ToolRole, ToolStatus, Toolchain, ToolchainDetector};

/// Default build deadline for Java compilation.
/// Java compilation is typically fast, but large projects may take longer.
pub const DEFAULT_JAVAC_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// The `javac` build backend for Java projects.
pub struct JavacBackend {
    toolchain: Toolchain,
    runner: Box<dyn CommandRunner>,
    /// Project root, for artifact discovery.
    pub artifact_root: PathBuf,
    /// Deadline handed to javac.
    pub timeout_ms: u64,
    /// Explicit build parallelism from `[build] jobs`. `None` means "use javac default".
    pub jobs: Option<u32>,
}

impl JavacBackend {
    /// Build a backend from an already-detected toolchain.
    pub fn new(toolchain: Toolchain) -> Self {
        Self {
            toolchain,
            runner: Box::new(SystemRunner::default()),
            artifact_root: PathBuf::new(),
            timeout_ms: DEFAULT_JAVAC_TIMEOUT_MS,
            jobs: None,
        }
    }

    /// Build a backend with a custom runner (for testing).
    #[cfg(test)]
    pub fn with_runner(toolchain: Toolchain, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            toolchain,
            runner,
            artifact_root: PathBuf::new(),
            timeout_ms: DEFAULT_JAVAC_TIMEOUT_MS,
            jobs: None,
        }
    }

    /// Detect javac in the toolchain.
    fn detect_javac(&self) -> ToolchainReport {
        let mut tools = Vec::new();

        // Check for javac
        let javac_status = self.toolchain.find("javac");
        match javac_status {
            Some(status) => {
                tools.push(status.to_tool_info());
            }
            None => {
                tools.push(ToolInfo {
                    name: "javac".to_string(),
                    path: None,
                    version: None,
                    available: false,
                    required: true,
                });
            }
        }

        ToolchainReport {
            tools,
            ok: javac_status.map_or(false, |s| s.available),
        }
    }

    /// Plan the javac invocation.
    fn plan_javac(&self, project: &ResolvedProject) -> Result<BuildPlan> {
        let javac = self
            .toolchain
            .path_of("javac")
            .ok_or_else(|| {
                PrincessError::new(
                    princess_core::ErrorCode::ToolchainMissing,
                    "javac not found in toolchain",
                )
            })?;

        let mut argv = vec![javac.to_string_lossy().to_string()];

        // Add output directory
        let build_dir = project.root.join("build");
        argv.push("-d".to_string());
        argv.push(build_dir.to_string_lossy().to_string());

        // Add source files (find all .java files in project)
        // For now, we'll just compile Main.java as specified in the manifest
        // In a real implementation, we'd scan for .java files
        let main_java = project.root.join("Main.java");
        if main_java.exists() {
            argv.push(main_java.to_string_lossy().to_string());
        }

        // Add jobs if specified (javac doesn't have -j, but we can use -J flags)
        // Note: javac parallelism is controlled by the JVM, not by a flag
        // We'll leave this for future optimization

        Ok(BuildPlan {
            backend: BuildBackendKind::Javac,
            toolchain_id: "javac".to_string(),
            argv,
            cwd: project.root.clone(),
            env: BTreeMap::new(),
            declared_artifacts: vec![],
            timeout_ms: Some(self.timeout_ms),
        })
    }

    /// Parse javac diagnostics from stderr.
    fn parse_diagnostics(&self, stderr: &str, cwd: &Path) -> Vec<ParsedDiagnostic> {
        let mut parser = DiagnosticParser::new(cwd).with_family(ToolFamily::Gcc); // javac uses similar format to gcc
        let mut diagnostics = Vec::new();

        for line in stderr.lines() {
            let produced = parser.feed(line);
            diagnostics.extend(produced);
        }

        // Flush any remaining diagnostic
        if let Some(diag) = parser.flush() {
            diagnostics.push(diag);
        }

        dedup(diagnostics)
    }

    /// Discover build artifacts (class files).
    fn discover_artifacts(&self, project: &ResolvedProject) -> Vec<Artifact> {
        let build_dir = project.root.join("build");
        let mut artifacts = Vec::new();

        if build_dir.exists() {
            // Find all .class files in build directory
            if let Ok(entries) = std::fs::read_dir(&build_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map_or(false, |ext| ext == "class") {
                        let metadata = std::fs::metadata(&path).ok();
                        artifacts.push(Artifact {
                            path: path.to_string_lossy().to_string(),
                            kind: ArtifactKind::Other, // class files are "other" in our classification
                            size: metadata.map(|m| m.len()).unwrap_or(0),
                            sha256: String::new(), // We could compute this if needed
                        });
                    }
                }
            }
        }

        artifacts
    }
}

impl BuildBackend for JavacBackend {
    fn id(&self) -> &str {
        "javac"
    }

    fn detect(&self, _project: &ResolvedProject) -> Result<ToolchainReport> {
        Ok(self.detect_javac())
    }

    fn plan(&self, project: &ResolvedProject) -> Result<BuildPlan> {
        self.plan_javac(project)
    }

    fn execute(
        &self,
        plan: &BuildPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<BuildFinishedPayload> {
        let start = Instant::now();

        // Emit build.started
        events.emit(EventBody::BuildStarted(plan.started_event()))?;

        // Create build directory if it doesn't exist
        let build_dir = plan.cwd.join("build");
        std::fs::create_dir_all(&build_dir)?;

        // Run javac with chunk callback
        let mut chunks = Vec::new();
        let output = self.runner.run(plan, cancel, &mut |chunk| {
            chunks.push(chunk);
        })?;

        // Stream chunks as log.append
        for chunk in &chunks {
            events.log(chunk.stream.log_stream(), &chunk.text)?;
        }

        // Parse diagnostics from stderr chunks
        let stderr_text = chunks
            .iter()
            .filter(|c| c.stream == StdioStream::Stderr)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        let diagnostics = self.parse_diagnostics(&stderr_text, &plan.cwd);

        // Emit build.diagnostic events
        for diag in &diagnostics {
            events.emit(EventBody::BuildDiagnostic(diag.to_payload()))?;
        }

        // Discover artifacts
        let artifacts = self.discover_artifacts(&ResolvedProject {
            root: plan.cwd.clone(),
            build_cwd: plan.cwd.clone(),
            build_command: None,
            targets: vec![],
            declared_artifacts: vec![],
            compile_commands: None,
            kernel: None,
            serial_tee_to_file: None,
            symbols: None,
            config: ProjectConfig::default(),
        });

        // Emit artifact.changed events
        for artifact in &artifacts {
            events.emit(EventBody::ArtifactChanged(ArtifactChangedPayload {
                path: artifact.path.clone(),
                kind: artifact.kind,
            }))?;
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Determine build status
        let status = if output.success() {
            BuildStatus::Ok
        } else {
            BuildStatus::Failed
        };

        // Emit build.finished
        let finished = BuildFinishedPayload {
            status,
            exit_code: output.exit_code,
            duration_ms,
            artifacts: artifacts.clone(),
        };

        events.emit(EventBody::BuildFinished(finished.clone()))?;

        Ok(finished)
    }

    fn cancel(&self, _op_id: &str) -> Result<()> {
        // Cancel is handled by the process runner
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::FakeRunner;
    use princess_core::event::EventRecorder;

    #[test]
    fn javac_backend_id_is_javac() {
        let toolchain = Toolchain::default();
        let backend = JavacBackend::new(toolchain);
        assert_eq!(backend.id(), "javac");
    }

    #[test]
    fn javac_backend_detects_missing_javac() {
        let toolchain = Toolchain::default();
        let backend = JavacBackend::new(toolchain);
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
            config: ProjectConfig::default(),
        };

        let report = backend.detect(&project).unwrap();
        let javac_info = report.find("javac").unwrap();
        assert!(javac_info.path.is_none());
        assert!(javac_info.required);
        assert!(!javac_info.available);
    }

    #[test]
    fn javac_backend_plan_requires_javac() {
        let toolchain = Toolchain::default();
        let backend = JavacBackend::new(toolchain);
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
            config: ProjectConfig::default(),
        };

        let err = backend.plan(&project).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::ToolchainMissing);
    }

    #[test]
    fn javac_backend_parses_diagnostics() {
        let toolchain = Toolchain::default();
        let backend = JavacBackend::new(toolchain);

        // javac error format: file.java:line: error: message
        let stderr = "Main.java:5: error: cannot find symbol\n  symbol: variable x\n  location: class Main\n";
        let diagnostics = backend.parse_diagnostics(stderr, Path::new("/tmp/test"));

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file, Some("/tmp/test/Main.java".to_string()));
        assert_eq!(diagnostics[0].line, Some(5));
        assert!(diagnostics[0].message.contains("cannot find symbol"));
    }

    #[test]
    fn javac_backend_execute_success() {
        let mut toolchain = Toolchain::default();
        toolchain.tools.push(ToolStatus {
            name: "javac".to_string(),
            role: ToolRole::Compiler,
            path: Some(PathBuf::from("/usr/bin/javac")),
            version: Some("17.0.20.1".to_string()),
            available: true,
            required: true,
            purpose: "Java compiler".to_string(),
            detail: None,
        });

        let runner = FakeRunner {
            chunks: vec![StdioChunk {
                stream: StdioStream::Stdout,
                text: "Compiled successfully\n".to_string(),
                encoding: TextEncoding::Utf8,
                at_eof: false,
            }],
            output: Some(CommandOutput {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                cancelled: false,
                combined_tail: String::new(),
            }),
            seen: std::sync::Mutex::new(Vec::new()),
        };

        let backend = JavacBackend::with_runner(toolchain, Box::new(runner));
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
            config: ProjectConfig::default(),
        };

        let plan = backend.plan(&project).unwrap();
        let mut events = EventRecorder::new(Vec::new());
        let cancel = CancelToken::new();

        let result = backend.execute(&plan, &mut events, &cancel).unwrap();
        assert_eq!(result.status, BuildStatus::Ok);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn javac_backend_execute_failure() {
        let mut toolchain = Toolchain::default();
        toolchain.tools.push(ToolStatus {
            name: "javac".to_string(),
            role: ToolRole::Compiler,
            path: Some(PathBuf::from("/usr/bin/javac")),
            version: Some("17.0.20.1".to_string()),
            available: true,
            required: true,
            purpose: "Java compiler".to_string(),
            detail: None,
        });

        let runner = FakeRunner {
            chunks: vec![StdioChunk {
                stream: StdioStream::Stderr,
                text: "Main.java:5: error: cannot find symbol\n".to_string(),
                encoding: TextEncoding::Utf8,
                at_eof: false,
            }],
            output: Some(CommandOutput {
                exit_code: Some(1),
                signal: None,
                timed_out: false,
                cancelled: false,
                combined_tail: String::new(),
            }),
            seen: std::sync::Mutex::new(Vec::new()),
        };

        let backend = JavacBackend::with_runner(toolchain, Box::new(runner));
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
            config: ProjectConfig::default(),
        };

        let plan = backend.plan(&project).unwrap();
        let mut events = EventRecorder::new(Vec::new());
        let cancel = CancelToken::new();

        let result = backend.execute(&plan, &mut events, &cancel).unwrap();
        assert_eq!(result.status, BuildStatus::Failed);
        assert_eq!(result.exit_code, Some(1));
    }
}
