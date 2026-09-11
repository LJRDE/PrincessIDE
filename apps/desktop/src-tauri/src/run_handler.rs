//! `princess:run:start` / `princess:run:stop` — IPC handlers.
//!
//! These commands bridge the Tauri shell to `princess-run::QemuBackend`.
//! A run spawns QEMU, streams serial output, detects CPU faults, and
//! attributes the exit reason.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use princess_core::traits::{BootMedium, CancelToken};
use princess_core::PrincessError;
use princess_run::{
    PlanInputs, PlanOptions, QemuBackend, RunOptions,
};

use crate::build_handler::TauriEventSink;
use crate::contract::{err, ok, ErrorCode as ShellErrorCode};
use crate::events::{EventBus, EVENT_CHANNEL};

/// Resolve the project and boot medium for a run command.
///
/// If the build produced an ELF artifact and `[run] kernel` is not set,
/// the run uses the build artifact.  For the reference kernel (no manifest),
/// we fall back to `build/kernel.elf`.
fn resolve_run_inputs(
    _project_root: &PathBuf,
    loaded: &princess_core::config::LoadedConfig,
) -> Result<(PathBuf, PathBuf), PrincessError> {
    let project = loaded.config.resolve(&loaded.root);

    // Determine the kernel ELF: manifest `[run] kernel` > build artifact discovery.
    let kernel = if let Some(k) = &project.kernel {
        k.clone()
    } else {
        // Try to find the ELF from the build artifacts.
        let build_dir = project.build_cwd.join("build");
        // Look for any .elf file in build/
        let mut elf = None;
        if build_dir.is_dir() {
            for entry in walkdir(&build_dir, 2) {
                if entry.extension().map(|e| e == "elf").unwrap_or(false) {
                    elf = Some(entry);
                    break;
                }
            }
        }
        elf.unwrap_or_else(|| project.build_cwd.join("build/kernel.elf"))
    };

    // For multiboot2, the run engine creates a GRUB ISO.
    // The ISO is typically at build/<name>.iso
    let iso = project.build_cwd.join("build").join(format!("{}.iso", project.name()));

    Ok((kernel, iso))
}

fn walkdir(root: &PathBuf, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if depth == 0 { return out; }
    let Ok(entries) = std::fs::read_dir(root) else { return out; };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            out.extend(walkdir(&path, depth - 1));
        } else {
            out.push(path);
        }
    }
    out
}

/// `princess:run:start` — launch QEMU with the built kernel.
///
/// Args: `{ projectRoot?: string, timeoutMs?: number }`
///
/// Events emitted:
///   run.started → log.append(serial.com1) → run.fault* →
///   log.append(qemu.monitor) → run.exited
pub async fn run_start(
    args: &Value,
    app: &AppHandle,
    bus: &Arc<EventBus>,
    ops: &crate::ops::OpRegistry,
) -> Value {
    let project_root_arg = args.get("projectRoot").and_then(Value::as_str);

    let (root, loaded) = match crate::build_handler::resolve_project_from(project_root_arg) {
        Ok(v) => v,
        Err(e) => return err(ShellErrorCode::InvalidConfig, e.message, e.detail.unwrap_or_default()),
    };

    let project = loaded.config.resolve(&root);

    // Override timeout if provided.
    let timeout_ms = args
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(project.config.run.timeout_ms);

    let op = ops.register("run:start");
    let cancel = CancelToken::new();
    let cancel_clone = cancel.clone();

    let op_id = op.op_id.clone();

    // Log start.
    let log_env = bus.log_append(
        "ide",
        &format!("princess:run:start → {}\n", root.display()),
        Some(&op.op_id),
    );
    let _ = app.emit(EVENT_CHANNEL, log_env);

    // Resolve QEMU binary.
    let qemu_bin = std::env::var("PRINCESSIDE_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Try to find it through the toolchain launcher
            if let Some(bin) = princess_build::launcher_dir() {
                let candidate = bin.parent().unwrap_or(&bin).join("prefix/usr/bin/qemu-system-x86_64");
                if candidate.is_file() {
                    return candidate;
                }
            }
            PathBuf::from("qemu-system-x86_64")
        });

    // Determine the boot medium.
    let (kernel_path, iso_path) = match resolve_run_inputs(&root, &loaded) {
        Ok(v) => v,
        Err(e) => {
            ops.finish(&op_id);
            return err(ShellErrorCode::InvalidConfig, e.message, e.detail.unwrap_or_default());
        }
    };

    // Use the ISO if it exists, otherwise the kernel directly.
    let boot_medium = if iso_path.is_file() {
        BootMedium::Iso(iso_path)
    } else if kernel_path.is_file() {
        BootMedium::Iso(kernel_path.parent().unwrap_or(&kernel_path).join(format!("{}.iso", project.name())))
    } else {
        BootMedium::Iso(iso_path) // Let the run engine report the error
    };

    let plan_inputs = PlanInputs {
        root: root.clone(),
        cwd: root.clone(),
        args: project.config.run.args.clone(),
        boot: project.config.run.boot,
        timeout_ms,
        serial_device: project.config.run.serial.device.clone(),
        serial_tee: project.config.run.serial.tee_to_file.as_ref().map(|p| root.join(p)),
    };

    let stub_defaults = Some((
        project.config.debug.stub.host.clone(),
        project.config.debug.stub.port,
    ));

    let mut run_env = std::collections::BTreeMap::new();
    if let Some(bin) = princess_build::launcher_dir() {
        let existing = std::env::var("PATH").unwrap_or_default();
        run_env.insert("PATH".to_string(), format!("{}:{}", bin.display(), existing));
    }

    let options = RunOptions {
        plan: PlanOptions {
            qemu_bin,
            ..PlanOptions::default()
        },
        env: run_env,
        keep_debug_log: false,
        stub_defaults,
    };

    let plan_inputs_clone = plan_inputs.clone();
    let sink_bus = bus.clone();
    let sink_app = app.clone();
    let sink_op_id = op.op_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let backend = QemuBackend::new(options);
        let plan_options = PlanOptions {
            qemu_bin: PathBuf::from("qemu-system-x86_64"),
            ..PlanOptions::default()
        };

        match princess_run::plan_run(&plan_inputs_clone, &boot_medium, &plan_options) {
            Ok(plan) => {
                let mut sink = TauriEventSink::new(sink_app, sink_bus, &sink_op_id);
                backend.launch_plan(&plan, &mut sink, &cancel_clone, None)
            }
            Err(e) => Err(e),
        }
    })
    .await;

    ops.finish(&op_id);

    match result {
        Ok(Ok(outcome)) => {
            ok(json!({
                "exitCode": outcome.exit_code,
                "reason": outcome.reason.as_str(),
                "uptimeMs": outcome.uptime_ms,
                "fault": outcome.fault.as_ref().map(|f| json!({
                    "vector": f.vector,
                    "rip": f.rip,
                    "ripText": f.rip_text,
                    "errorCode": f.error_code,
                    "symbolicated": f.symbolicated.as_ref().map(|s| json!({
                        "symbol": s.symbol,
                        "file": s.file,
                        "line": s.line,
                    })),
                })),
            }))
        }
        Ok(Err(e)) => err(
            ShellErrorCode::QemuFailed,
            e.message,
            e.detail.unwrap_or_default(),
        ),
        Err(join_err) => err(
            ShellErrorCode::Internal,
            "run task panicked",
            join_err.to_string(),
        ),
    }
}

/// `princess:run:stop` — terminate a running QEMU instance.
///
/// Args: `{ opId: string }`
pub fn run_stop(args: &Value, ops: &crate::ops::OpRegistry) -> Value {
    let Some(op_id) = args.get("opId").and_then(Value::as_str) else {
        return err(
            ShellErrorCode::InvalidConfig,
            "princess:run:stop requires { opId: string }",
            format!("received args: {args}"),
        );
    };
    match ops.cancel(op_id) {
        crate::ops::CancelOutcome::Cancelled => ok(json!({ "opId": op_id, "stopped": true })),
        crate::ops::CancelOutcome::AlreadyCancelled => err(
            ShellErrorCode::Cancelled,
            format!("{op_id} was already stopped"),
            String::new(),
        ),
        crate::ops::CancelOutcome::NotFound => err(
            ShellErrorCode::NotFound,
            format!("no live run operation named {op_id}"),
            format!("live operations: {}", ops.live().join(", ")),
        ),
    }
}
