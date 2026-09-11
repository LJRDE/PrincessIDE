//! `princess:build:start` / `princess:build:cancel` — IPC handlers.
//!
//! These commands bridge the Tauri shell to `princess-build::MakeBackend`.
//! The build emits events through a `TauriEventSink` that forwards
//! `EventBody` as Tauri event envelopes on the `princess:event` channel.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use princess_build::build_project;
use princess_core::config::{LoadedConfig, ProjectConfig};
use princess_core::event::EventBody;
use princess_core::traits::{CancelToken, EventSink};
use princess_core::PrincessError;

use crate::contract::{err, ok, ErrorCode as ShellErrorCode};
use crate::events::{EventBus, EVENT_CHANNEL};

/// Bridge between `princess_core::EventSink` and the Tauri event bus.
///
/// Every `EventBody` emitted by the build backend is wrapped in a contract
/// envelope by the `EventBus` and pushed to the frontend through
/// `app.emit("princess:event", envelope)`.
pub struct TauriEventSink {
    app: AppHandle,
    bus: Arc<EventBus>,
    op_id: String,
}

impl TauriEventSink {
    pub fn new(app: AppHandle, bus: Arc<EventBus>, op_id: &str) -> Self {
        Self {
            app,
            bus,
            op_id: op_id.to_string(),
        }
    }

    fn emit_envelope(&self, envelope: Value) {
        let _ = self.app.emit(EVENT_CHANNEL, envelope);
    }
}

impl EventSink for TauriEventSink {
    fn emit(&mut self, body: EventBody) -> princess_core::Result<()> {
        let envelope = self.bus.envelope_from_body(body, Some(&self.op_id));
        self.emit_envelope(envelope);
        Ok(())
    }

    fn op_id(&self) -> Option<&str> {
        Some(&self.op_id)
    }
}

/// Resolve the project root and load its config.
///
/// Accepts an optional `projectRoot` from the frontend; when absent, falls
/// back to the workspace root detection logic from `doctor::resolve_root`.
pub fn resolve_project_from(root_arg: Option<&str>) -> Result<(PathBuf, LoadedConfig), PrincessError> {
    let root = match root_arg {
        Some(r) => PathBuf::from(r),
        None => match crate::doctor::resolve_root() {
            Ok(root) => root,
            Err(failure) => {
                return Err(PrincessError::new(
                    princess_core::ErrorCode::NotFound,
                    failure.message,
                ).with_detail(failure.detail));
            }
        },
    };
    let loaded = ProjectConfig::load_or_default(&root)?;
    Ok((root, loaded))
}

/// `princess:build:start` — build a kernel project end-to-end.
///
/// Args: `{ projectRoot?: string, targets?: string[] }`
///
/// Events emitted (contract §2):
///   build.started → log.append(build) → build.diagnostic* →
///   artifact.changed* → build.finished
///
/// Returns: `{ build.finished.status, exitCode, durationMs, artifacts }`
pub async fn build_start(
    args: &Value,
    app: &AppHandle,
    bus: &Arc<EventBus>,
    ops: &crate::ops::OpRegistry,
) -> Value {
    let project_root = args.get("projectRoot").and_then(Value::as_str);

    let (_root, loaded) = match resolve_project_from(project_root) {
        Ok(v) => v,
        Err(e) => return err(ShellErrorCode::InvalidConfig, e.message, e.detail.unwrap_or_default()),
    };

    let mut project = loaded.config.resolve(&loaded.root);

    // Override targets from args if provided.
    if let Some(targets) = args.get("targets").and_then(Value::as_array) {
        project.targets = targets
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    let op = ops.register("build:start");
    let cancel = CancelToken::new();
    let cancel_clone = cancel.clone();

    // Register a cancellation hook in the ops registry so `princess:op:cancel`
    // can cancel this build.
    let op_id = op.op_id.clone();

    // Log start.
    emit_log(app, bus, "ide", &format!("princess:build:start → {}\n", project.root.display()), Some(&op.op_id));

    let sink_bus = bus.clone();
    let sink_app = app.clone();
    let sink_op_id = op.op_id.clone();

    // Run the build in a blocking task (build_project is synchronous).
    let result = tokio::task::spawn_blocking(move || {
        let mut sink = TauriEventSink::new(sink_app, sink_bus, &sink_op_id);
        build_project(&project, &mut sink, &cancel_clone)
    })
    .await;

    ops.finish(&op_id);

    match result {
        Ok(Ok(outcome)) => {
            let status_str = match outcome.finished.status {
                princess_core::types::BuildStatus::Ok => "ok",
                princess_core::types::BuildStatus::Failed => "failed",
                princess_core::types::BuildStatus::Cancelled => "cancelled",
            };
            ok(json!({
                "status": status_str,
                "exitCode": outcome.finished.exit_code,
                "durationMs": outcome.finished.duration_ms,
                "artifacts": outcome.finished.artifacts.iter().map(|a| {
                    json!({
                        "path": a.path,
                        "kind": format!("{:?}", a.kind).to_lowercase(),
                        "size": a.size,
                        "sha256": a.sha256,
                    })
                }).collect::<Vec<_>>(),
                "compileCommands": outcome.compile_commands.map(|p| p.to_string_lossy().to_string()),
            }))
        }
        Ok(Err(build_err)) => {
            let princess_err = build_err.to_error();
            err(
                ShellErrorCode::BuildFailed,
                princess_err.message,
                princess_err.detail.unwrap_or_default(),
            )
        }
        Err(join_err) => err(
            ShellErrorCode::Internal,
            "build task panicked",
            join_err.to_string(),
        ),
    }
}

/// `princess:build:cancel` — cancel a running build.
///
/// Args: `{ opId: string }`
pub fn build_cancel(args: &Value, ops: &crate::ops::OpRegistry) -> Value {
    let Some(op_id) = args.get("opId").and_then(Value::as_str) else {
        return err(
            ShellErrorCode::InvalidConfig,
            "princess:build:cancel requires { opId: string }",
            format!("received args: {args}"),
        );
    };
    match ops.cancel(op_id) {
        crate::ops::CancelOutcome::Cancelled => ok(json!({ "opId": op_id, "cancelled": true })),
        crate::ops::CancelOutcome::AlreadyCancelled => err(
            ShellErrorCode::Cancelled,
            format!("{op_id} was already cancelled"),
            String::new(),
        ),
        crate::ops::CancelOutcome::NotFound => err(
            ShellErrorCode::NotFound,
            format!("no live build operation named {op_id}"),
            format!("live operations: {}", ops.live().join(", ")),
        ),
    }
}

fn emit_log(app: &AppHandle, bus: &EventBus, stream: &str, chunk: &str, op_id: Option<&str>) {
    let envelope = bus.log_append(stream, chunk, op_id);
    let _ = app.emit(EVENT_CHANNEL, envelope);
}
