//! PrincessIDE desktop shell — Tauri v2 entry point.
//!
//! Responsibilities (contract §1: `apps/desktop/src-tauri/` owns 窗口、IPC 注册、
//! 子进程编排):
//!   - register the IPC command surface (`commands`);
//!   - own the engine→UI event stream (`events`, contract §2);
//!   - own the unified cancellation registry (`ops`, contract §3 `princess:op:cancel`);
//!   - orchestrate child processes (`doctor` runs scripts/doctor.sh).
//!
//! Deliberately absent: build/run/debug backends (P2/P4), binary views (P5) and
//! **any LSP protocol implementation** — per decision D17 the engine only
//! generates/validates clangd project configuration; the editor's language
//! service lives in the frontend over `@codemirror/lsp-client`.

pub mod commands;
pub mod contract;
pub mod doctor;
pub mod events;
pub mod ops;

use tauri::Emitter;

use events::{EventBus, EVENT_CHANNEL};
use ops::OpRegistry;

/// Shared engine state handed to every IPC handler.
#[derive(Default)]
pub struct AppState {
    pub bus: EventBus,
    pub ops: OpRegistry,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::princess_invoke,
            commands::princess_tools_detect,
            commands::princess_op_cancel,
        ])
        .setup(|app| {
            // First event on the stream: proves the envelope shape to the UI even
            // before any operation runs (contract §2).
            let state = app.state::<AppState>();
            let env = state.bus.log_append("ide", "PrincessIDE shell ready\n", None);
            let _ = app.emit(EVENT_CHANNEL, env);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running PrincessIDE shell");
}
