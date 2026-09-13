//! PrincessIDE desktop shell — Tauri v2 entry point.
//!
//! Responsibilities (contract §1: `apps/desktop/src-tauri/` owns 窗口、IPC 注册、
//! 子进程编排):
//!   - register the IPC command surface (`commands`);
//!   - own the engine→UI event stream (`events`, contract §2);
//!   - own the unified cancellation registry (`ops`, contract §3 `princess:op:cancel`);
//!   - orchestrate child processes (`doctor` runs scripts/doctor.sh).
//!   - wire engine backends: build, run, debug, LSP (P3-C).

pub mod build_handler;
pub mod commands;
pub mod contract;
pub mod debug_handler;
pub mod doctor;
pub mod events;
pub mod fs_handler;
pub mod lsp_handler;
pub mod ops;
pub mod project_handler;
pub mod run_handler;

use std::sync::Arc;
use tauri::{Emitter, Manager};

use events::{EventBus, EVENT_CHANNEL};
use lsp_handler::LspRegistry;
use ops::OpRegistry;

/// Shared engine state handed to every IPC handler.
pub struct AppState {
    pub bus: Arc<EventBus>,
    pub ops: OpRegistry,
    pub lsp: LspRegistry,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            bus: Arc::new(EventBus::default()),
            ops: OpRegistry::default(),
            lsp: LspRegistry::new(),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
