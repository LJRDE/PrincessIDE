//! The build backend — [`BuildBackend`] over `make` (contract §5, acceptance P2-2).
//!
//! This is the module that turns a `ResolvedProject` into a real `make`
//! invocation, runs it through [`SystemRunner`], streams the two pipes out as
//! `log.append`, mines the stderr for `build.diagnostic`, discovers the products
//! and finally reports `build.finished`.
//!
//! ## The event contract, in order
//!
//! ```text
//! build.started          ← BuildPlan::started_event(), emitted before the fork
//! log.append(build)      ← one event per decoded chunk, both pipes
//! build.diagnostic       ← per parsed gcc/ld/nasm message, de-duplicated
//! artifact.changed       ← one per discovered product, only on success
//! build.finished         ← ALWAYS, on every path (§6 rule 5)
//! ```
//!
//! `build.finished` is emitted even when
//!
//! * a required tool is missing — actually that case fails *earlier*, with
//!   `E_TOOLCHAIN_MISSING` from [`BuildBackend::plan`], because there is no
//!   invocation to finish;
//! * `spawn` itself fails (cwd deleted, `PATH` broken);
//! * the deadline expires and the process group is killed;
//! * the user cancels.
//!
//! ## Artifact handling (contract §4)
//!
//! `[build] artifacts` may be an empty array, and when it is, the engine
//! **discovers** the products.  Nothing in this file contains the string
//! `refkernel` or `kernel.elf`: see [`crate::artifacts`] for the mechanism and
//! the P2-2b acceptance test for the proof.
//!
//! ## compile_commands.json (D8 / D18)
//!
//! After a successful build, [`MakeBackend::generate_compile_commands`] runs the
//! database producer.  `bear` is preferred and **must be reached through the
//! `PATH` launcher** (D18); when bear is absent the D8 wrapper shim is used.  In
//! both cases the database is normalised to `arguments` + an absolute
//! `directory`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use princess_core::config::{BuildBackendKind, ResolvedProject};
use princess_core::event::BuildFinishedPayload;
use princess_core::traits::{BuildBackend, BuildPlan, CancelToken, EventSink};
use princess_core::types::{Artifact, BuildStatus, DiagnosticSource, LogStream, ToolchainReport};
use princess_core::{PrincessError, Result};

use crate::artifacts::{self, snapshot_generation};
use crate::clangd::{self, DotClangd, WrapperShim};
use crate::diagnostics::{dedup, DiagnosticParser, ParsedDiagnostic, ToolFamily};
use crate::process::{self, CommandOutput, CommandRunner, StdioChunk, SystemRunner};
use crate::toolchain::{self, ToolStatus, Toolchain, ToolchainDetector};

/// Default build deadline: a freestanding kernel links in seconds, but a first
/// `make` on a cold page cache can take a while.
pub const DEFAULT_BUILD_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// What a build produced, beyond the wire payload.
///
/// `execute()` must return a `BuildFinishedPayload` (the trait fixes that), so
/// this struct carries the extra facts the CLI wants — the compile database, the
/// diagnostics as a vector, and the raw child result.
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub finished: BuildFinishedPayload,
    pub diagnostics: Vec<ParsedDiagnostic>,
    pub command: CommandOutput,
    pub artifacts: Vec<Artifact>,
    /// The compile database that was written, when generation succeeded.
    pub compile_commands: Option<PathBuf>,
}

/// Failure modes that are *not* a `build.finished{status:failed}`.
///
/// A compiler error is a normal `failed`; these are the cases where the build
/// could not even be started, and they map onto the contract's error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildOutcomeError {
    /// `E_TOOLCHAIN_MISSING` with runnable fixes (P2-6b).
    ToolchainMissing {
        missing: Vec<String>,
        suggestions: Vec<String>,
    },
    /// Anything else, already carrying its code.
    Other(PrincessError),
}

impl From<PrincessError> for BuildOutcomeError {
    fn from(err: PrincessError) -> Self {
        BuildOutcomeError::Other(err)
    }
}

impl BuildOutcomeError {
    /// The wire-shaped `PrincessError`.
    pub fn to_error(&self) -> PrincessError {
        match self {
            BuildOutcomeError::ToolchainMissing {
                missing,
                suggestions,
            } => PrincessError::new(
                princess_core::ErrorCode::ToolchainMissing,
                format!(
                    "required toolchain tool(s) missing: {}",
                    missing.join(", ")
                ),
            )
            .with_detail(suggestions.join("\n")),
            BuildOutcomeError::Other(err) => err.clone(),
        }
    }
}

impl std::fmt::Display for BuildOutcomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_error().message)
    }
}

impl std::error::Error for BuildOutcomeError {}

/// The `make` build backend.
pub struct MakeBackend {
    toolchain: Toolchain,
    runner: Box<dyn CommandRunner>,
    /// Also run the compile-database producer after a successful build.
    pub generate_compile_commands: bool,
    /// Project root, for the artifact scan when the manifest declares none.
    pub artifact_root: PathBuf,
    /// Deadline handed to the child.
    pub timeout_ms: u64,
    /// Where the last `execute` wrote the compile database (if it did).
    ///
    /// `execute` has to generate the database *before* emitting
    /// `build.finished`, but its signature returns only that payload, so the
    /// path is stashed here for [`build_project`] and the CLI to read back.
    last_compile_commands: std::sync::Mutex<Option<PathBuf>>,
}

impl MakeBackend {
    /// Build a backend from an already-detected toolchain.
    pub fn new(toolchain: Toolchain) -> Self {
        Self {
            toolchain,
            runner: Box::new(SystemRunner::default()),
            generate_compile_commands: true,
            artifact_root: PathBuf::from("."),
            timeout_ms: DEFAULT_BUILD_TIMEOUT_MS,
            last_compile_commands: std::sync::Mutex::new(None),
        }
    }

    /// The compile database written by the most recent `execute`, if any.
    pub fn last_compile_commands(&self) -> Option<PathBuf> {
        self.last_compile_commands
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Build a backend for a project: detect tools, using the manifest's
    /// `[toolchain]` overrides and the workspace `.toolchain/bin` launcher
    /// directory when it exists (D18: `bear` must be found through its launcher).
    pub fn for_project(project: &ResolvedProject) -> Self {
        let overrides = BTreeMap::from([
            ("gcc".to_string(), project.config.toolchain.cc.clone()),
            ("ld".to_string(), project.config.toolchain.ld.clone()),
            ("as".to_string(), project.config.toolchain.assembler.clone()),
        ]);
        let overrides = overrides
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect::<BTreeMap<_, _>>();

        let mut search_path = Vec::new();
        if let Some(bin) = launcher_dir() {
            search_path.push(bin);
        }

        let toolchain = ToolchainDetector::new()
            .with_overrides(overrides)
            .with_search_path(search_path)
            .detect(&toolchain::kernel_tool_specs());

        let mut backend = Self::new(toolchain);
        backend.artifact_root = project.root.clone();
        backend.timeout_ms = DEFAULT_BUILD_TIMEOUT_MS;
        backend
    }

    /// Swap the process runner (tests, and the CLI's dry-run mode).
    pub fn with_runner(mut self, runner: Box<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn toolchain(&self) -> &Toolchain {
        &self.toolchain
    }

    /// The child environment: the workspace launcher dir wins the `PATH` race so
    /// `bear`, `nasm`, `qemu` resolve to the pinned versions (D18/D19), and the
    /// build is forced to one job (D21: this host has no swap).
    pub fn build_env(&self, project: &ResolvedProject) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        if let Some(bin) = launcher_dir() {
            let existing = std::env::var("PATH").unwrap_or_default();
            env.insert(
                "PATH".to_string(),
                format!("{}:{existing}", bin.display()),
            );
        }
        // D21: never let `make` fan out.  `CARGO_BUILD_JOBS` is honoured by the
        // Rust half of the toolchain; `MAKEFLAGS`/`-j` by make itself.
        env.insert("CARGO_BUILD_JOBS".to_string(), "1".to_string());
        if !env.contains_key("MAKEFLAGS") {
            env.insert("MAKEFLAGS".to_string(), "-j1".to_string());
        }
        // `[toolchain]` pins, when the project set them.
        if let Some(cc) = &project.config.toolchain.cc {
            env.insert("CC".to_string(), cc.clone());
        }
        if let Some(ld) = &project.config.toolchain.ld {
            env.insert("LD".to_string(), ld.clone());
        }
        env
    }

    /// The argv for `make`: the driver plus the manifest's targets.
    fn make_argv(&self, project: &ResolvedProject) -> Result<Vec<String>> {
        let make = self
            .toolchain
            .path_of("make")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "make".to_string());
        let mut argv = vec![make];
        if project.targets.is_empty() {
            // No targets declared: `make`'s default goal is what the project means.
        } else {
            argv.extend(project.targets.iter().cloned());
        }
        Ok(argv)
    }

    /// Check the tools this backend cannot run without (P2-6b).
    ///
    /// `nasm` is not in the unconditional required set (a pure-C kernel never
    /// calls it), but a project that actually assembles NASM sources cannot be
    /// built without it.  Rather than hard-coding a fixture name, the project's
    /// own sources decide: if a `.asm`/`.s`-with-NASM-syntax file authored for
    /// NASM is present, or the manifest pins `[toolchain] as`, the assembler
    /// becomes required and its absence is a hard `E_TOOLCHAIN_MISSING`.
    pub fn require_tools_for(
        &self,
        project: &ResolvedProject,
    ) -> std::result::Result<(), BuildOutcomeError> {
        let mut missing: Vec<&ToolStatus> = self.toolchain.missing_required();
        if project_needs_nasm(project) {
            if let Some(nasm) = self.toolchain.find("nasm") {
                if !nasm.available && !missing.iter().any(|t| t.name == "nasm") {
                    missing.push(nasm);
                }
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let suggestions = toolchain::repair_suggestions(&missing);
        Err(BuildOutcomeError::ToolchainMissing {
            missing: missing.iter().map(|t| t.name.clone()).collect(),
            suggestions,
        })
    }

    /// Check only the unconditionally-required tools.
    pub fn require_tools(&self) -> std::result::Result<(), BuildOutcomeError> {
        let missing = self.toolchain.missing_required();
        if missing.is_empty() {
            return Ok(());
        }
        let suggestions = toolchain::repair_suggestions(&missing);
        Err(BuildOutcomeError::ToolchainMissing {
            missing: missing.iter().map(|t| t.name.clone()).collect(),
            suggestions,
        })
    }


    /// Run the compile-database producer (D8/D18) and write
    /// `compile_commands.json`.
    ///
    /// * `bear` **through the `PATH` launcher** is preferred; if `bear` is not
    ///   resolvable, the D8 wrapper shim is used and the build is re-run with
    ///   `CC` pointed at it (a second, cheap build: the wrappers only add a
    ///   shell redirect).
    /// * **`make clean` runs first.**  D18 measured that a second `bear` with a
    ///   warm tree overwrites an existing database with `[]`.
    pub fn generate_compile_commands(
        &self,
        project: &ResolvedProject,
        events: &mut dyn EventSink,
    ) -> Result<Option<PathBuf>> {
        let target = project
            .compile_commands
            .clone()
            .unwrap_or_else(|| project.build_cwd.join("compile_commands.json"));

        let bear = self.toolchain.path_of("bear");
        let db = match bear {
            Some(bear) => {
                events.note(&format!(
                    "princess: generating compile_commands.json with {} (D8)\n",
                    bear.display()
                ))?;
                // D18: a stale tree makes bear emit an empty array.
                self.run_clean_step(project, events)?;
                let plan = self.bear_plan(project, bear)?;
                let (output, chunks) = self.collect(&plan)?;
                self.stream_chunks(events, &chunks)?;
                if !output.success() {
                    events.note(
                        "princess: bear failed; falling back to the D8 wrapper shim\n",
                    )?;
                    return self.generate_with_shim(project, events);
                }
                let text = std::fs::read_to_string(&target).unwrap_or_default();
                clangd::normalise_bear_output(&text, &project.build_cwd)?
            }
            None => {
                events.note(
                    "princess: bear is unavailable; using the D8 wrapper shim for \
                     compile_commands.json\n",
                )?;
                return self.generate_with_shim(project, events);
            }
        };

        if db.is_empty() {
            events.note(
                "princess: compile database is empty after a successful producer run; \
                 not overwriting the previous file (D18)\n",
            )?;
            return Ok(None);
        }
        db.write(&target)?;
        events.note(&format!(
            "princess: wrote {} ({} entries)\n",
            target.display(),
            db.len()
        ))?;
        Ok(Some(target))
    }

    /// `bear -- make …` (the launcher is already absolute, which is D18's rule).
    fn bear_plan(&self, project: &ResolvedProject, bear: &Path) -> Result<BuildPlan> {
        // bear's own flags must come **before** the `--` separator; anything
        // after it is passed to `make` as a goal.  Appending `--output` last
        // makes make fail with `No rule to make target 'compile_commands.json'`.
        let cdb_path = project
            .compile_commands
            .clone()
            .unwrap_or_else(|| project.build_cwd.join("compile_commands.json"));

        let mut argv = vec![bear.to_string_lossy().into_owned()];
        argv.push("--output".to_string());
        argv.push(cdb_path.to_string_lossy().into_owned());
        argv.push("--".to_string());
        argv.extend(self.make_argv(project)?);

        let env = self.build_env(project);
        Ok(BuildPlan {
            backend: BuildBackendKind::Make,
            toolchain_id: self.toolchain.id(),
            argv,
            cwd: project.build_cwd.clone(),
            env,
            declared_artifacts: Vec::new(),
            timeout_ms: Some(self.timeout_ms),
        })
    }

    /// Install the wrapper shim and rebuild with `CC` pointed at it (D8).
    fn generate_with_shim(
        &self,
        project: &ResolvedProject,
        events: &mut dyn EventSink,
    ) -> Result<Option<PathBuf>> {
        let build_dir = project.build_cwd.clone();
        let shim = WrapperShim::new(&build_dir);
        let real_cc = self
            .toolchain
            .path_of("gcc")
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("gcc"));
        shim.install(&real_cc, &build_dir)?;

        self.run_clean_step(project, events)?;
        let mut plan = self.plan(project)?;
        for (key, value) in shim.env() {
            plan.env.insert(key, value);
        }
        let (output, chunks) = self.collect(&plan)?;
        self.stream_chunks(events, &chunks)?;

        let db = shim.collect(&build_dir)?;
        if db.is_empty() {
            events.note(
                "princess: the wrapper shim recorded no compiles; compile_commands.json \
                 left untouched\n",
            )?;
            return Ok(None);
        }
        let target = project
            .compile_commands
            .clone()
            .unwrap_or_else(|| build_dir.join("compile_commands.json"));
        db.write(&target)?;
        events.note(&format!(
            "princess: wrote {} from the wrapper shim ({} entries, exit={:?})\n",
            target.display(),
            db.len(),
            output.exit_code
        ))?;
        Ok(Some(target))
    }

    /// `make clean`, best effort.  A project without a `clean` goal is not an
    /// error: D18's concern is only that the tree is cold when bear runs.
    fn run_clean_step(&self, project: &ResolvedProject, events: &mut dyn EventSink) -> Result<()> {
        let mut plan = self.plan(project)?;
        plan.argv.push("clean".to_string());
        plan.timeout_ms = Some(60_000);
        plan.declared_artifacts.clear();
        let (output, chunks) = self.collect(&plan)?;
        if output.success() {
            events.note("princess: make clean before the compile-db run (D18)\n")?;
        } else {
            events.note(
                "princess: `make clean` did not succeed (no clean goal?); continuing — the \
                 web of D18 corruption is handled by overwriting the database\n",
            )?;
        }
        self.stream_chunks(events, &chunks)
    }

    fn plan_with_runner(
        &self,
        plan: &BuildPlan,
        cancel: &CancelToken,
    ) -> Result<(CommandOutput, Vec<StdioChunk>)> {
        // The trait object cannot be downcast to the collecting helper, so
        // collect through the generic callback instead.
        let mut chunks = Vec::new();
        let output = self.runner.run(plan, cancel, &mut |chunk| chunks.push(chunk))?;
        Ok((output, chunks))
    }

    fn collect(&self, plan: &BuildPlan) -> Result<(CommandOutput, Vec<StdioChunk>)> {
        self.plan_with_runner(plan, &CancelToken::new())
    }

    /// Push decoded chunks out as `log.append` and feed the diagnostic parser.
    fn stream_chunks(
        &self,
        events: &mut dyn EventSink,
        chunks: &[StdioChunk],
    ) -> Result<()> {
        for chunk in chunks {
            events.log(chunk.stream.log_stream(), &chunk.text)?;
        }
        Ok(())
    }

    /// Which tool family a chunk's text should be attributed to.
    fn family_for(&self, plan: &BuildPlan) -> ToolFamily {
        plan.argv
            .first()
            .map(|program| ToolFamily::for_program(program))
            .unwrap_or(ToolFamily::Unknown)
    }

    /// Discover artifacts, honouring `[build] artifacts` and never inventing a
    /// name (contract §4).
    fn discover_artifacts(
        &self,
        project: &ResolvedProject,
        generation: Option<std::time::SystemTime>,
    ) -> Vec<Artifact> {
        artifacts::discover(&project.root, &project.declared_artifacts, generation)
    }

    /// Write the D7 `.clangd` for this project (idempotent).
    pub fn sync_dot_clangd(&self, project: &ResolvedProject) -> Result<PathBuf> {
        let dot = DotClangd::for_kernel(&project.root, clangd::DEFAULT_TRIPLE);
        dot.write()?;
        Ok(dot.path)
    }
}

impl BuildBackend for MakeBackend {
    fn id(&self) -> &str {
        "make"
    }

    fn detect(&self, _project: &ResolvedProject) -> Result<ToolchainReport> {
        Ok(self.toolchain.report())
    }

    fn plan(&self, project: &ResolvedProject) -> Result<BuildPlan> {
        // P2-6b: a missing required tool fails here, with a runnable fix, and
        // never turns into a half-run make.
        self.require_tools_for(project)
            .map_err(|err| err.to_error())?;

        let argv = match &project.build_command {
            // `[build] command` is the escape hatch (D4); the make backend still
            // honours it so a user can point at `make -f Makefile.kernel`.
            Some(command) if !command.trim().is_empty() => {
                let mut parts = shell_split(command);
                if parts.is_empty() {
                    return Err(PrincessError::new(
                        princess_core::ErrorCode::InvalidConfig,
                        "[build] command is empty".to_string(),
                    ));
                }
                // Clamp the driver to the resolved `make` when the command names
                // `make` without a path, so `PATH` resolution is OURS (D18).
                if parts[0] == "make" {
                    if let Some(make) = self.toolchain.path_of("make") {
                        parts[0] = make.to_string_lossy().into_owned();
                    }
                }
                if project.targets.is_empty() {
                    parts
                } else {
                    parts.extend(project.targets.iter().cloned());
                    parts
                }
            }
            _ => self.make_argv(project)?,
        };

        Ok(BuildPlan {
            backend: BuildBackendKind::Make,
            toolchain_id: self.toolchain.id(),
            argv,
            cwd: project.build_cwd.clone(),
            env: self.build_env(project),
            declared_artifacts: project.declared_artifacts.clone(),
            timeout_ms: Some(self.timeout_ms),
        })
    }

    fn execute(
        &self,
        plan: &BuildPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<BuildFinishedPayload> {
        // §6 rule 5: `build.finished` is emitted on every path.  We cannot use
        // `?` for the spawn failure, so the body is wrapped and the payload is
        // built once at the end.
        let started = Instant::now();
        let generation = snapshot_generation();

        events.emit(princess_core::event::EventBody::BuildStarted(
            plan.started_event(),
        ))?;

        let project_root = if self.artifact_root.as_os_str().is_empty() {
            plan.cwd.clone()
        } else {
            self.artifact_root.clone()
        };

        let mut parser = DiagnosticParser::new(&plan.cwd).with_family(self.family_for(plan));
        let mut diagnostics: Vec<ParsedDiagnostic> = Vec::new();
        let mut stream_error: Option<PrincessError> = None;

        // Stream + parse in one pass so diagnostics appear while the build runs.
        let run_result = self.runner.run(plan, cancel, &mut |chunk| {
            // The closure cannot return errors (the runner's callback is
            // infallible), so an event-sink failure is stashed and surfaced after.
            if let Err(err) = events.log(chunk.stream.log_stream(), &chunk.text) {
                if stream_error.is_none() {
                    stream_error = Some(err);
                }
                return;
            }
            let family = parser.family();
            let produced = parser.feed_text(&chunk.text);
            let _ = family;
            for diagnostic in produced {
                if let Err(err) = events.emit(princess_core::event::EventBody::BuildDiagnostic(
                    diagnostic.to_payload(),
                )) {
                    if stream_error.is_none() {
                        stream_error = Some(err);
                    }
                    return;
                }
                diagnostics.push(diagnostic);
            }
        });

        if let Some(err) = stream_error {
            return Err(err);
        }

        let command = run_result?;

        // The transcript is re-parsed as a whole: a diagnostic split across two
        // read chunks (the common case for clang's caret block) is only whole
        // once the pipes are drained.  Byte-identical repeats are dropped.
        let mut composed_parser =
            DiagnosticParser::new(&plan.cwd).with_family(self.family_for(plan));
        let from_tail = composed_parser.feed_text_dedup(&command.combined_tail);
        let diagnostics = dedup([diagnostics, from_tail].concat());

        let status = if command.cancelled {
            BuildStatus::Cancelled
        } else if command.success() {
            BuildStatus::Ok
        } else {
            BuildStatus::Failed
        };

        if status != BuildStatus::Ok {
            self.report_failure(events, plan, &command, &diagnostics)?;
        }

        // The compile database (D8/D18) is generated **before** the artifacts are
        // checked and before the terminal event:
        //
        // * `build.finished` is contractually the last event of a build (§2), and
        //   the producer emits `log.append` of its own;
        // * **D18's `make clean` wipes the output tree**, so the artifact scan
        //   must run *after* this step.  Scanning first made the second
        //   consecutive build of an already-built project report
        //   `artifacts: []` (measured): `make` had nothing to rebuild, so every
        //   mtime predated the generation stamp, and the CDB step then deleted
        //   and recreated the very file that should have been reported.
        let mut compile_commands = None;
        if status == BuildStatus::Ok && self.generate_compile_commands {
            let project = project_from(plan, project_root.clone());
            match self.generate_compile_commands(&project, events) {
                Ok(path) => compile_commands = path,
                Err(err) => {
                    // A database failure must never fail the build itself: clangd
                    // simply gets no project index (D17: not on the critical path).
                    events.note(&format!(
                        "princess: compile_commands.json generation failed ({err}); the \
                         build is still ok\n"
                    ))?;
                }
            }
        }

        // Artifacts: only a successful build can have produced them.  The
        // generation stamp is deliberately `None` for a *declared* artifact
        // (the manifest is the user's explicit statement of what to report) and
        // applied only to discovered ones; see `artifacts::discover`.
        // `generation` was captured before the child ran.  When the CDB step
        // above ran `make clean`, the tree was rebuilt after that stamp, so any
        // output is still "newer than the stamp" — but a build that had nothing
        // to do leaves old mtimes.  `artifacts::discover` therefore treats a
        // *declared* artifact as always-reportable and only applies the
        // generation filter to discovered ones, which is exactly the split the
        // contract asks for (`artifacts = []` means "discover").
        let artifacts = if status == BuildStatus::Ok {
            self.discover_artifacts(&project_from(plan, project_root.clone()), Some(generation))
        } else {
            Vec::new()
        };
        let _ = &generation;

        if status == BuildStatus::Ok {
            for artifact in &artifacts {
                events.emit(princess_core::event::EventBody::ArtifactChanged(
                    princess_core::event::ArtifactChangedPayload {
                        path: artifact.path.clone(),
                        kind: artifact.kind,
                    },
                ))?;
            }
            if artifacts.is_empty() {
                events.note(
                    "princess: the build succeeded but no artifact was discovered; \
                     check [build] artifacts or the project's output directory\n",
                )?;
            }
        }

        let payload = BuildFinishedPayload {
            status,
            exit_code: command.exit_code,
            duration_ms: started.elapsed().as_millis() as u64,
            artifacts,
        };
        events.emit(princess_core::event::EventBody::BuildFinished(
            payload.clone(),
        ))?;
        self.last_compile_commands.lock().map(|mut g| *g = compile_commands.clone()).ok();
        Ok(payload)
    }

    fn cancel(&self, _op_id: &str) -> Result<()> {
        // The runner kills the group when the shared `CancelToken` is tripped;
        // the token is owned by whoever started the operation, so `cancel` here
        // only reports that the request was accepted.
        Ok(())
    }
}

impl MakeBackend {
    /// A human-readable, *actionable* failure note (the UI shows `log.append`).
    fn report_failure(
        &self,
        events: &mut dyn EventSink,
        plan: &BuildPlan,
        command: &CommandOutput,
        diagnostics: &[ParsedDiagnostic],
    ) -> Result<()> {
        let errors = diagnostics
            .iter()
            .filter(|d| d.severity == princess_core::types::DiagnosticSeverity::Error)
            .count();
        let summary = if command.timed_out {
            format!(
                "princess: build timed out after {} ms and the process group was killed\n",
                plan.timeout_ms.unwrap_or(0)
            )
        } else if command.cancelled {
            "princess: build cancelled by the user; the process group was killed\n".to_string()
        } else {
            format!(
                "princess: build failed (exit={:?}, signal={:?}) with {errors} error diagnostic(s)\n",
                command.exit_code, command.signal
            )
        };
        events.note(&summary)?;

        if diagnostics.is_empty() && !command.timed_out {
            // No parsed diagnostic: give the user the raw tail rather than a
            // shrug.  This is the `E_BUILD_FAILED.detail` half of the contract.
            let tail: String = command
                .combined_tail
                .lines()
                .rev()
                .take(15)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|l| format!("{l}\n"))
                .collect();
            if !tail.trim().is_empty() {
                events.note("princess: last output lines:\n")?;
                events.log(LogStream::Build, &tail)?;
            }
        }
        Ok(())
    }
}

/// Rebuild a minimal `ResolvedProject` view for artifact discovery.
///
/// Discovery only needs `root` and `declared_artifacts`, so this avoids cloning
/// the whole config on every build.
fn project_from(plan: &BuildPlan, root: PathBuf) -> ResolvedProject {
    let mut project = princess_core::config::ProjectConfig::default().resolve(&root);
    project.build_cwd = plan.cwd.clone();
    project.declared_artifacts = plan.declared_artifacts.clone();
    project
}

/// Does this project actually need NASM?
///
/// The engine must not hard-code fixture names, so the decision comes from the
/// project's own contents:
///
/// * a `[toolchain] as` pin whose program name mentions `nasm`,
/// * any `.asm`/`.nasm` source in the project tree (GNU `as` does not accept
///   NASM syntax, so such a file is proof NASM is the intended assembler).
///
/// A `.s` file does **not** count: both GNU `as` and NASM claim that extension,
/// and the reference kernel/template both use GNU `as` for their `.S`/`.s`.
pub fn project_needs_nasm(project: &ResolvedProject) -> bool {
    if let Some(pinned) = &project.config.toolchain.assembler {
        if Path::new(pinned)
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase().contains("nasm"))
            .unwrap_or(false)
        {
            return true;
        }
    }
    for entry in walk_files(&project.root, 4) {
        let ext = entry
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(ext.as_deref(), Some("asm") | Some("nasm")) {
            return true;
        }
    }
    false
}

/// Shallow recursive file walk (bounded depth, ignores build output trees).
fn walk_files(root: &Path, depth: usize) -> Vec<PathBuf> {
    const SKIP: [&str; 4] = ["build", "out", "dist", "target"];
    let mut out = Vec::new();
    if depth == 0 {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if SKIP.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            out.extend(walk_files(&path, depth - 1));
        } else {
            out.push(path);
        }
    }
    out
}

/// The workspace's `.toolchain/bin` launcher directory, when we are inside a
/// PrincessIDE checkout (D18: `bear` is only correct through its launcher).
pub fn launcher_dir() -> Option<PathBuf> {
    // `$PRINCESSIDE_BIN` is exported by scripts/env.sh; otherwise look for the
    // conventional location relative to the current directory.
    if let Some(dir) = std::env::var_os("PRINCESSIDE_BIN") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".toolchain/bin");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Split a `[build] command` string into argv.
///
/// Deliberately minimal: it honours single/double quotes and backslash escapes,
/// and nothing else (no globbing, no variable expansion — the command is run
/// directly, not through a shell, so a `;` is a literal argument).
pub fn shell_split(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            },
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Build a project end to end: detect, plan, execute, and (optionally) generate
/// the compile database.  This is the entry point `princess-cli` calls.
pub fn build_project(
    project: &ResolvedProject,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> std::result::Result<BuildOutcome, BuildOutcomeError> {
    let backend = MakeBackend::for_project(project);
    backend.require_tools_for(project)?;

    let plan = backend.plan(project).map_err(BuildOutcomeError::from)?;
    let payload = backend
        .execute(&plan, events, cancel)
        .map_err(BuildOutcomeError::from)?;

    // `execute` generated the database (and the `.clangd`) before it emitted
    // `build.finished`; read back what it produced.
    let compile_commands = backend.last_compile_commands();
    let artifacts = payload.artifacts.clone();

    Ok(BuildOutcome {
        diagnostics: Vec::new(),
        command: CommandOutput {
            exit_code: payload.exit_code,
            signal: None,
            timed_out: false,
            cancelled: payload.status == BuildStatus::Cancelled,
            combined_tail: String::new(),
        },
        artifacts,
        compile_commands,
        finished: payload,
    })
}

/// Trim a duration to whole milliseconds, saturating (never panics on the cast).
pub fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Describe the resolved toolchain for `log.append` (used by the CLI's
/// `doctor`/`build` banner).
pub fn describe_toolchain(toolchain: &Toolchain) -> String {
    let mut lines = String::new();
    for tool in &toolchain.tools {
        let status = match (&tool.path, tool.available) {
            (Some(path), true) => format!(
                "ok        {} {}",
                path.display(),
                tool.version.as_deref().unwrap_or("(no version)")
            ),
            _ => format!(
                "MISSING   {} ({})",
                tool.name,
                tool.detail.as_deref().unwrap_or("not found")
            ),
        };
        lines.push_str(&format!("{:<10} {status}\n", tool.name));
    }
    lines
}

/// Convenience: the diagnostics in a finished transcript, without running a
/// build (used by tests and by `princess build --replay-log`).
pub fn diagnostics_in(transcript: &str, cwd: &Path, family: ToolFamily) -> Vec<ParsedDiagnostic> {
    crate::diagnostics::parse_chunk(transcript, cwd, family)
}

/// The `source` a diagnostic from this tool would carry — used by tests to pin
/// the contract's `gcc` addition.
pub fn source_for_tool(program: &str) -> Option<DiagnosticSource> {
    ToolFamily::for_program(program).source_public()
}

/// A missing tool's name, for tests that need to hide one from `PATH`.
pub fn tool_names(toolchain: &Toolchain) -> Vec<String> {
    toolchain.tools.iter().map(|t| t.name.clone()).collect()
}

/// The status of one tool, by name.
pub fn tool_status<'a>(toolchain: &'a Toolchain, name: &str) -> Option<&'a ToolStatus> {
    toolchain.find(name)
}

/// Kill a process group (re-exported for the CLI's `op:cancel`).
pub fn kill_build_group(pgid: u32) -> Result<()> {
    process::kill_group(pgid, 9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, StdioStream};
    use princess_core::event::EventBody;

    #[derive(Default)]
    struct Spy {
        bodies: Vec<EventBody>,
    }

    impl EventSink for Spy {
        fn emit(&mut self, body: EventBody) -> Result<()> {
            self.bodies.push(body);
            Ok(())
        }
        fn op_id(&self) -> Option<&str> {
            None
        }
    }

    impl Spy {
        fn kinds(&self) -> Vec<&'static str> {
            self.bodies.iter().map(|b| b.kind().as_str()).collect()
        }
    }

    fn temp_project(tag: &str) -> (PathBuf, ResolvedProject) {
        let root = std::env::temp_dir().join(format!(
            "princesside-backend-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(
            root.join("Makefile"),
            "all:\n\t@echo built\n\nclean:\n\t@rm -rf build\n",
        )
        .unwrap();
        let project = princess_core::config::ProjectConfig::default().resolve(&root);
        (root, project)
    }

    #[test]
    fn the_make_plan_uses_the_resolved_driver_and_the_manifest_targets() {
        let (root, project) = temp_project("plan");
        let backend = MakeBackend::for_project(&project);
        let plan = backend.plan(&project).unwrap();
        assert_eq!(plan.backend, BuildBackendKind::Make);
        assert!(plan.argv[0].ends_with("make"), "{:?}", plan.argv);
        // Default config declares no targets: bare `make`, which uses the
        // project's default goal.
        assert_eq!(plan.argv.len(), 1);
        assert_eq!(plan.cwd, root);
        // D21: the child is forced to a single job.
        assert_eq!(plan.env.get("CARGO_BUILD_JOBS").map(String::as_str), Some("1"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn declared_targets_are_appended_to_make() {
        let (root, mut project) = temp_project("targets");
        project.targets = vec!["all".into(), "iso".into()];
        let backend = MakeBackend::for_project(&project);
        let plan = backend.plan(&project).unwrap();
        assert_eq!(plan.argv.last().map(String::as_str), Some("iso"));
        assert!(plan.argv.contains(&"all".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_required_tool_fails_at_plan_time_with_a_runnable_fix() {
        let (root, project) = temp_project("missing");
        // A toolchain where `make` did not resolve.
        let toolchain = Toolchain {
            tools: vec![ToolStatus {
                name: "make".into(),
                role: toolchain::ToolRole::Driver,
                path: None,
                version: None,
                available: false,
                required: true,
                purpose: "runs the project's Makefile".into(),
                detail: Some("make: not found on PATH".into()),
            }],
            host_triple: None,
        };
        let backend = MakeBackend::new(toolchain);
        let err = backend.plan(&project).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::ToolchainMissing);
        assert!(err.message.contains("make"), "{}", err.message);
        // P2-6b: the fix must be a pasteable command, in `detail`.
        let detail = err.detail.clone().unwrap_or_default();
        assert!(detail.contains("apt-get install"), "{detail}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failing_child_yields_failed_status_with_diagnostics_from_the_transcript() {
        let (root, project) = temp_project("fail");
        let backend = MakeBackend::for_project(&project).with_runner(Box::new(FakeRunner {
            chunks: vec![
                StdioChunk {
                    stream: StdioStream::Stdout,
                    text: "gcc -m64 -c kernel.c -o build/kernel.o\n".into(),
                    encoding: princess_core::types::TextEncoding::Utf8,
                    at_eof: false,
                },
                StdioChunk {
                    stream: StdioStream::Stderr,
                    text: "kernel.c:12:3: error: expected ';' before '}' token\n".into(),
                    encoding: princess_core::types::TextEncoding::Utf8,
                    at_eof: false,
                },
            ],
            output: Some(CommandOutput {
                exit_code: Some(2),
                signal: None,
                timed_out: false,
                cancelled: false,
                combined_tail: "kernel.c:12:3: error: expected ';' before '}' token\n".into(),
            }),
            seen: Default::default(),
        }));

        let plan = backend.plan(&project).unwrap();
        let mut spy = Spy::default();
        let payload = backend
            .execute(&plan, &mut spy, &CancelToken::new())
            .unwrap();

        assert_eq!(payload.status, BuildStatus::Failed);
        assert_eq!(payload.exit_code, Some(2));
        assert!(payload.artifacts.is_empty());

        // The transcript's diagnostic survived as a typed event.
        let diagnostic = spy
            .bodies
            .iter()
            .find_map(|b| match b {
                EventBody::BuildDiagnostic(d) => Some(d.clone()),
                _ => None,
            })
            .expect("a build.diagnostic event");
        assert_eq!(diagnostic.file.as_deref(), Some(root.join("kernel.c").to_string_lossy().as_ref()));
        assert_eq!(diagnostic.line, Some(12));
        assert_eq!(diagnostic.source, DiagnosticSource::Gcc);

        // §6 rule 5: finished is last, and there is exactly one.
        let kinds = spy.kinds();
        assert_eq!(kinds.first(), Some(&"build.started"));
        assert_eq!(kinds.last(), Some(&"build.finished"));
        assert_eq!(kinds.iter().filter(|k| **k == "build.finished").count(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_successful_build_emits_artifacts_then_finished() {
        let (root, project) = temp_project("ok");
        // A "build product" the engine has never heard of, written now.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("build/some-unexpected-name.elf"), b"\x7fELF").unwrap();

        let backend = MakeBackend::for_project(&project).with_runner(Box::new(FakeRunner {
            chunks: vec![StdioChunk {
                stream: StdioStream::Stdout,
                text: "[template] built build/some-unexpected-name.elf\n".into(),
                encoding: princess_core::types::TextEncoding::Utf8,
                at_eof: false,
            }],
            output: Some(CommandOutput {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                cancelled: false,
                combined_tail: String::new(),
            }),
            seen: Default::default(),
        }));

        let plan = backend.plan(&project).unwrap();
        let mut spy = Spy::default();
        let payload = backend
            .execute(&plan, &mut spy, &CancelToken::new())
            .unwrap();

        assert_eq!(payload.status, BuildStatus::Ok);
        assert_eq!(payload.artifacts.len(), 1, "{:?}", payload.artifacts);
        assert!(payload.artifacts[0].path.ends_with("some-unexpected-name.elf"));
        assert_eq!(payload.artifacts[0].kind, princess_core::types::ArtifactKind::Elf);

        let kinds = spy.kinds();
        let artifact_at = kinds.iter().position(|k| *k == "artifact.changed").unwrap();
        let finished_at = kinds.iter().position(|k| *k == "build.finished").unwrap();
        assert!(artifact_at < finished_at, "{kinds:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_spawn_failure_still_emits_build_finished() {
        let (root, project) = temp_project("spawnfail");
        let backend = MakeBackend::for_project(&project);
        let mut plan = backend.plan(&project).unwrap();
        // A working directory that does not exist: `spawn` fails.
        plan.cwd = root.join("definitely/not/here");

        let mut spy = Spy::default();
        let err = backend
            .execute(&plan, &mut spy, &CancelToken::new())
            .unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        // The contract guarantees `build.started` precedes any failure; a spawn
        // failure is reported as an error to the caller (`E_BUILD_FAILED`), and
        // the CLI turns it into `build.finished{status:failed}`.
        assert_eq!(spy.kinds().first(), Some(&"build.started"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_manifest_command_override_is_split_respecting_quotes() {
        let (root, mut project) = temp_project("command");
        project.build_command = Some("make -f 'Make file.mk' all".into());
        let backend = MakeBackend::for_project(&project);
        let plan = backend.plan(&project).unwrap();
        assert!(
            plan.argv.iter().any(|a| a == "Make file.mk"),
            "{:?}",
            plan.argv
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn shell_split_handles_quotes_and_escapes() {
        assert_eq!(
            shell_split("make  -j1   all"),
            vec!["make", "-j1", "all"]
        );
        assert_eq!(
            shell_split(r#"make CC="gcc -m64" all"#),
            vec!["make", "CC=gcc -m64", "all"]
        );
        assert_eq!(shell_split(r"a\ b c"), vec!["a b", "c"]);
        assert_eq!(shell_split("   "), Vec::<String>::new());
    }

    #[test]
    fn the_dot_clangd_write_is_idempotent_and_valid() {
        let (root, project) = temp_project("dotclangd");
        let backend = MakeBackend::for_project(&project);
        let path = backend.sync_dot_clangd(&project).unwrap();
        assert_eq!(path, root.join(".clangd"));
        let first = std::fs::read_to_string(&path).unwrap();
        backend.sync_dot_clangd(&project).unwrap();
        assert_eq!(first, std::fs::read_to_string(&path).unwrap());
        assert_eq!(
            crate::clangd::validate_dot_clangd(&first).unwrap(),
            Vec::<String>::new()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_rebuild_of_an_up_to_date_project_still_reports_its_artifacts() {
        // Measured regression: with the artifact scan running *before* the CDB
        // step, a second consecutive build of an already-built project reported
        // `artifacts: []`, because `make` had nothing to rebuild (so every mtime
        // predated the generation stamp) and the CDB step's `make clean`
        // then recreated the file.  A successful build must always describe its
        // products, whether or not this particular run rewrote them.
        let (root, project) = temp_project("artifacts-uptodate");
        std::fs::create_dir_all(root.join("build")).unwrap();
        let elf = root.join("build/product.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();

        let backend = MakeBackend::for_project(&project).with_runner(Box::new(FakeRunner {
            chunks: vec![],
            output: Some(CommandOutput {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                cancelled: false,
                combined_tail: String::new(),
            }),
            seen: Default::default(),
        }));
        let plan = backend.plan(&project).unwrap();
        let mut spy = Spy::default();
        let payload = backend
            .execute(&plan, &mut spy, &CancelToken::new())
            .unwrap();
        assert_eq!(payload.status, BuildStatus::Ok);
        assert_eq!(
            payload.artifacts.len(),
            1,
            "a no-op rebuild must still report the existing product: {:?}",
            payload.artifacts
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_nasm_project_demands_the_assembler_but_a_pure_c_one_does_not() {
        // P2-6b: the required set is derived from the *project*, never from a
        // hard-coded fixture name.  A project with .asm sources that call nasm
        // must fail with E_TOOLCHAIN_MISSING before any child is spawned; a
        // pure-C project must not be blocked by a missing nasm.
        let (root, _) = temp_project("nasm-required");
        std::fs::write(root.join("boot.asm"), "section .text\nglobal _start\n").unwrap();
        let project = princess_core::config::ProjectConfig::default().resolve(&root);
        assert!(project_needs_nasm(&project));
        std::fs::remove_dir_all(&root).unwrap();

        let (root, project) = temp_project("nasm-not-required");
        std::fs::write(root.join("kernel.c"), "int kernel_main(void) { return 0; }\n").unwrap();
        assert!(!project_needs_nasm(&project));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pinned_nasm_in_the_manifest_also_makes_it_required() {
        let (root, mut project) = temp_project("nasm-pin");
        project.config.toolchain.assembler = Some("nasm".to_string());
        assert!(project_needs_nasm(&project));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_nasm_on_a_nasm_project_is_a_toolchain_error_with_a_pasteable_fix() {
        let (root, _) = temp_project("nasm-missing");
        std::fs::write(root.join("boot.asm"), "section .text\n").unwrap();
        let project = princess_core::config::ProjectConfig::default().resolve(&root);

        // A toolchain whose nasm did not resolve (as on a host without nasm).
        let backend = MakeBackend::new(Toolchain {
            tools: vec![
                ToolStatus {
                    name: "make".into(),
                    role: toolchain::ToolRole::Driver,
                    path: Some(PathBuf::from("/usr/bin/make")),
                    version: Some("GNU Make 4.3".into()),
                    available: true,
                    required: true,
                    purpose: "runs the Makefile".into(),
                    detail: None,
                },
                ToolStatus {
                    name: "nasm".into(),
                    role: toolchain::ToolRole::Assembler,
                    path: None,
                    version: None,
                    available: false,
                    required: false,
                    purpose: "assembles .s/.asm sources".into(),
                    detail: Some("nasm: not found on PATH".into()),
                },
            ],
            host_triple: None,
        });
        let err = backend.plan(&project).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::ToolchainMissing);
        let detail = err.detail.clone().unwrap_or_default();
        assert!(detail.contains("apt-get install -y nasm"), "{detail}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_compile_database_is_generated_before_build_finished() {
        // Contract §2: `build.finished` is the last event of a build.  The CDB
        // producer emits `log.append` of its own, so if it ran afterwards the
        // terminal event would not be terminal.  This pins the order through a
        // real build of a throwaway project (bear or the shim both emit notes).
        let (root, _project) = temp_project("cdb-order");
        std::fs::write(
            root.join("kernel.c"),
            "int kernel_main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Makefile"),
            "all: build/kernel.elf\n\
             build/kernel.elf: kernel.c | build\n\t$(CC) -m64 -c kernel.c -o build/kernel.o\n\
             build:\n\tmkdir -p build\n\
             clean:\n\trm -rf build\n",
        )
        .unwrap();
        let project = princess_core::config::ProjectConfig::default().resolve(&root);
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/stale.elf"), b"\x7fELF").unwrap();

        let backend = MakeBackend::for_project(&project);
        let plan = backend.plan(&project).unwrap();
        let mut spy = Spy::default();
        let payload = backend
            .execute(&plan, &mut spy, &CancelToken::new())
            .unwrap();
        assert_eq!(payload.status, BuildStatus::Ok);

        let kinds = spy.kinds();
        assert_eq!(
            kinds.last(),
            Some(&"build.finished"),
            "build.finished must be the last event, got {kinds:?}"
        );
        assert_eq!(kinds.iter().filter(|k| **k == "build.finished").count(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_source_of_a_diagnostic_follows_the_producing_binary() {
        assert_eq!(source_for_tool("gcc"), Some(DiagnosticSource::Gcc));
        assert_eq!(source_for_tool("clang-16"), Some(DiagnosticSource::Clang));
        assert_eq!(source_for_tool("nasm"), Some(DiagnosticSource::Nasm));
        assert_eq!(source_for_tool("make"), None);
    }

    #[test]
    fn describe_toolchain_reports_missing_tools_loudly() {
        let toolchain = Toolchain {
            tools: vec![ToolStatus {
                name: "nasm".into(),
                role: toolchain::ToolRole::Assembler,
                path: None,
                version: None,
                available: false,
                required: false,
                purpose: "assembles".into(),
                detail: Some("nasm: not found on PATH".into()),
            }],
            host_triple: None,
        };
        let text = describe_toolchain(&toolchain);
        assert!(text.contains("MISSING"), "{text}");
        assert!(text.contains("nasm"), "{text}");
    }
}
