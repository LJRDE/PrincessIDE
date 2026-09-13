//! IPC command surface (contract §3).
//!
//! Tauri derives an IPC command's name from the Rust function identifier, and an
//! identifier cannot contain `:`.  The frozen contract, however, names every
//! command `princess:<domain>:<action>`.  The shell therefore exposes one
//! native command — [`princess_invoke`] — that receives the contract name
//! *verbatim* and routes it, plus native aliases ([`princess_tools_detect`],
//! [`princess_op_cancel`]) that call the same handlers directly.
//!
//! Routing is a total function over the contract: a name from §3 that P3 has not
//! implemented yet answers with an explicit `E_NOT_FOUND` envelope naming its
//! owner, and a name outside §3 is rejected as unknown.  `classify()` is unit
//! tested against the whole contract list, so the two lists cannot drift.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::contract::{err, ok, ErrorCode, CONTRACT_COMMANDS, IMPLEMENTED_COMMANDS};
use crate::doctor::{self, DoctorOutcome};
use crate::events::EVENT_CHANNEL;
use crate::ops::CancelOutcome;
use crate::AppState;

/// Where a command name routes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Implemented in this phase.
    Implemented(&'static str),
    /// In §3, owned by a later phase.
    ContractNotImplemented,
    /// Not in the frozen v1 set at all.
    Unknown,
}

pub fn classify(cmd: &str) -> Route {
    if IMPLEMENTED_COMMANDS.contains(&cmd) {
        return Route::Implemented(IMPLEMENTED_COMMANDS[IMPLEMENTED_COMMANDS.iter().position(|c| *c == cmd).unwrap()]);
    }
    if CONTRACT_COMMANDS.contains(&cmd) {
        return Route::ContractNotImplemented;
    }
    Route::Unknown
}

fn emit(app: &AppHandle, envelope: Value) {
    // A failed emit (window gone) must not kill the engine; the ring still holds
    // the event so the UI can replay it (contract §6.5).
    let _ = app.emit(EVENT_CHANNEL, envelope);
}

/// The one native IPC entry point; `cmd` is the contract name, unmodified.
#[tauri::command]
pub async fn princess_invoke(
    cmd: String,
    args: Option<Value>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    Ok(dispatch(&cmd, args.unwrap_or_else(|| json!({})), &app, &state).await)
}

/// Native alias for `princess:tools:detect` (identical handler, no dispatcher hop).
#[tauri::command]
pub async fn princess_tools_detect(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    Ok(dispatch("princess:tools:detect", json!({}), &app, &state).await)
}

/// Native alias for `princess:op:cancel`; Tauri maps the JS `opId` argument to `op_id`.
#[tauri::command]
pub fn princess_op_cancel(op_id: String, app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    Ok(dispatch_sync("princess:op:cancel", json!({ "opId": op_id }), &app, &state))
}

pub async fn dispatch(cmd: &str, args: Value, app: &AppHandle, state: &AppState) -> Value {
    match classify(cmd) {
        Route::Implemented("princess:tools:detect") => tools_detect(app, state).await,
        Route::Implemented("princess:op:cancel") => op_cancel(&args, app, state),
        Route::Implemented("princess:op:replay") => op_replay(&args, state),
        // §3 fs: the editor's file I/O (open a real file, save it back).
        Route::Implemented("princess:fs:read") => crate::fs_handler::fs_read(&args),
        Route::Implemented("princess:fs:write") => crate::fs_handler::fs_write(&args),
        // P3-C: build commands
        Route::Implemented("princess:build:start") => {
            crate::build_handler::build_start(&args, app, &state.bus, &state.ops).await
        }
        Route::Implemented("princess:build:cancel") => {
            crate::build_handler::build_cancel(&args, &state.ops)
        }
        // P3-C: run commands
        Route::Implemented("princess:run:start") => {
            crate::run_handler::run_start(&args, app, &state.bus, &state.ops).await
        }
        Route::Implemented("princess:run:stop") => {
            crate::run_handler::run_stop(&args, &state.ops)
        }
        // P3-C: project commands
        Route::Implemented("princess:project:open") => {
            crate::project_handler::project_open(&args)
        }
        Route::Implemented("princess:project:validate") => {
            crate::project_handler::project_validate(&args)
        }
        // P3-C: debug commands
        Route::Implemented("princess:debug:attach") => {
            crate::debug_handler::debug_attach(&args)
        }
        Route::Implemented("princess:debug:setBreakpoints") => {
            crate::debug_handler::debug_set_breakpoints(&args)
        }
        Route::Implemented("princess:debug:continue") => {
            crate::debug_handler::debug_continue(&args)
        }
        Route::Implemented("princess:debug:stepOver") => {
            crate::debug_handler::debug_step_over(&args)
        }
        Route::Implemented("princess:debug:stepInto") => {
            crate::debug_handler::debug_step_into(&args)
        }
        Route::Implemented("princess:debug:stackTrace") => {
            crate::debug_handler::debug_stack_trace(&args)
        }
        Route::Implemented("princess:debug:scopes") => {
            crate::debug_handler::debug_scopes(&args)
        }
        Route::Implemented("princess:debug:variables") => {
            crate::debug_handler::debug_variables(&args)
        }
        Route::Implemented("princess:debug:readMemory") => {
            crate::debug_handler::debug_read_memory(&args)
        }
        Route::Implemented("princess:debug:writeMemory") => {
            crate::debug_handler::debug_write_memory(&args)
        }
        Route::Implemented("princess:debug:disassemble") => {
            crate::debug_handler::debug_disassemble(&args)
        }
        Route::Implemented("princess:debug:registers") => {
            crate::debug_handler::debug_registers(&args)
        }
        // P3-C: LSP commands
        Route::Implemented("princess:lsp:start") => {
            crate::lsp_handler::lsp_start(&args, app, &state.bus, &state.lsp)
        }
        Route::Implemented("princess:lsp:send") => {
            crate::lsp_handler::lsp_send(&args, &state.lsp)
        }
        Route::Implemented("princess:lsp:stop") => {
            crate::lsp_handler::lsp_stop(&args, app, &state.bus, &state.lsp)
        }
        Route::Implemented(other) => err(
            ErrorCode::Internal,
            format!("{other} is listed as implemented but has no handler"),
            "dispatch table and IMPLEMENTED_COMMANDS disagree".to_string(),
        ),
        Route::ContractNotImplemented => err(
            ErrorCode::NotFound,
            format!("{cmd} is not implemented yet"),
            format!(
                "currently implemented: {}\nsee docs/spec/10-contracts.md §3",
                IMPLEMENTED_COMMANDS.join(", ")
            ),
        ),
        Route::Unknown => err(
            ErrorCode::NotFound,
            format!("unknown command {cmd}"),
            format!("not part of the frozen v1 command set:\n{}", CONTRACT_COMMANDS.join("\n")),
        ),
    }
}

/// For the synchronous native alias.
fn dispatch_sync(cmd: &str, args: Value, app: &AppHandle, state: &AppState) -> Value {
    match classify(cmd) {
        Route::Implemented("princess:op:cancel") => op_cancel(&args, app, state),
        Route::Implemented("princess:op:replay") => op_replay(&args, state),
        Route::Implemented(other) => err(
            ErrorCode::Internal,
            format!("{other} is asynchronous; call it through princess_invoke"),
            "async command dispatched synchronously".to_string(),
        ),
        Route::ContractNotImplemented => err(
            ErrorCode::NotFound,
            format!("{cmd} is not implemented by the P3 shell yet"),
            format!("implemented by P3-A: {}", IMPLEMENTED_COMMANDS.join(", ")),
        ),
        Route::Unknown => err(ErrorCode::NotFound, format!("unknown command {cmd}"), String::new()),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `princess:tools:detect` → toolchain table (path + version + availability).
async fn tools_detect(app: &AppHandle, state: &AppState) -> Value {
    let root = match doctor::resolve_root() {
        Ok(root) => root,
        Err(failure) => return failure.into_value(),
    };

    let op = state.ops.register("tools:detect");
    emit(
        app,
        state.bus.log_append(
            "ide",
            format!("princess:tools:detect → running scripts/doctor.sh in {}\n", root.display()),
            Some(&op.op_id),
        ),
    );

    let outcome = doctor::run_doctor(&root, &op).await;
    state.ops.finish(&op.op_id);

    match outcome {
        Ok(DoctorOutcome::Done(run)) => {
            emit(
                app,
                state.bus.log_append(
                    "ide",
                    format!(
                        "doctor.sh exit {} · {} tools · missing required: {}\n",
                        run.exit_code,
                        run.tools.len(),
                        if run.missing.is_empty() { "none".to_string() } else { run.missing.join(", ") }
                    ),
                    Some(&op.op_id),
                ),
            );
            doctor::to_envelope(run, &root)
        }
        Ok(DoctorOutcome::Cancelled(run)) => {
            emit(
                app,
                state.bus.log_append("ide", "doctor.sh cancelled\n", Some(&op.op_id)),
            );
            doctor::cancelled_envelope(run)
        }
        Ok(DoctorOutcome::TimedOut(run)) => {
            emit(
                app,
                state.bus.log_append("ide", "doctor.sh timed out and was killed\n", Some(&op.op_id)),
            );
            doctor::timeout_envelope(run)
        }
        Err(failure) => failure.into_value(),
    }
}

/// `princess:op:cancel` → unified cancellation entry point, argument `opId`.
fn op_cancel(args: &Value, app: &AppHandle, state: &AppState) -> Value {
    let Some(op_id) = args.get("opId").and_then(Value::as_str) else {
        return err(
            ErrorCode::InvalidConfig,
            "princess:op:cancel requires { opId: string }",
            format!("received args: {args}"),
        );
    };

    match state.ops.cancel(op_id) {
        CancelOutcome::Cancelled => {
            emit(
                app,
                state.bus.log_append("ide", format!("cancelling {op_id}\n"), Some(op_id)),
            );
            ok(json!({ "opId": op_id, "cancelled": true, "found": true }))
        }
        CancelOutcome::AlreadyCancelled => err(
            ErrorCode::Cancelled,
            format!("{op_id} was already cancelled"),
            format!("live operations: {}", describe_live(state)),
        ),
        CancelOutcome::NotFound => err(
            ErrorCode::NotFound,
            format!("no live operation named {op_id}"),
            format!("live operations: {}", describe_live(state)),
        ),
    }
}

fn describe_live(state: &AppState) -> String {
    let live = state.ops.live();
    if live.is_empty() {
        "none".to_string()
    } else {
        live.join(", ")
    }
}

/// `princess:op:replay` → re-send every event with `seq >= fromSeq` (contract §3).
fn op_replay(args: &Value, state: &AppState) -> Value {
    let Some(from_seq) = args.get("fromSeq").and_then(Value::as_u64) else {
        return err(
            ErrorCode::InvalidConfig,
            "princess:op:replay requires { fromSeq: number }",
            format!("received args: {args}"),
        );
    };
    let events = state.bus.since(from_seq);
    ok(json!({
        "events": events,
        "fromSeq": from_seq,
        "lastSeq": state.bus.last_seq(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_command_routes_somewhere_explicit() {
        for cmd in CONTRACT_COMMANDS {
            match classify(cmd) {
                Route::Implemented(_) | Route::ContractNotImplemented => {}
                Route::Unknown => panic!("{cmd} from §3 classifies as Unknown"),
            }
        }
    }

    #[test]
    fn implemented_list_is_a_subset_of_the_contract() {
        for cmd in IMPLEMENTED_COMMANDS {
            assert_eq!(classify(cmd), Route::Implemented(cmd));
        }
    }

    #[test]
    fn non_contract_names_are_rejected() {
        for cmd in ["princess:lsp:bridge", "princess:lsp:restart", "princess:tools:detect ", "TOOLS_DETECT", ""] {
            assert_eq!(classify(cmd), Route::Unknown, "{cmd} must not route");
        }
    }

    #[test]
    fn all_v1_commands_are_now_implemented() {
        // P3-C: all contract commands are implemented.
        for cmd in CONTRACT_COMMANDS {
            assert_eq!(classify(cmd), Route::Implemented(cmd), "{cmd} must be implemented");
        }
    }

    #[test]
    fn p3_implements_build_and_run_and_lsp_commands() {
        assert_eq!(classify("princess:build:start"), Route::Implemented("princess:build:start"));
        assert_eq!(classify("princess:build:cancel"), Route::Implemented("princess:build:cancel"));
        assert_eq!(classify("princess:run:start"), Route::Implemented("princess:run:start"));
        assert_eq!(classify("princess:run:stop"), Route::Implemented("princess:run:stop"));
        assert_eq!(classify("princess:debug:attach"), Route::Implemented("princess:debug:attach"));
        assert_eq!(classify("princess:lsp:start"), Route::Implemented("princess:lsp:start"));
        assert_eq!(classify("princess:project:open"), Route::Implemented("princess:project:open"));
    }
}
