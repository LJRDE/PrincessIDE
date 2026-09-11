//! `princess:project:open` / `princess:project:validate` — IPC handlers.
//!
//! These handle project manifest loading and validation.

use serde_json::{json, Value};

use princess_core::config::{ConfigSource, ProjectConfig};

use crate::contract::{err, ok, ErrorCode as ShellErrorCode};
use crate::doctor::resolve_root;

/// `princess:project:open` — load a project manifest from disk.
///
/// Args: `{ path?: string }`
///
/// Returns: the resolved project config with provenance info.
pub fn project_open(args: &Value) -> Value {
    let path_arg = args.get("path").and_then(Value::as_str);
    let root = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => match resolve_root() {
            Ok(r) => r,
            Err(f) => return f.into_value(),
        },
    };

    match ProjectConfig::load_or_default(&root) {
        Ok(loaded) => {
            let source_str = loaded.source.to_string();
            let is_default = matches!(loaded.source, ConfigSource::Defaults { .. });
            ok(json!({
                "root": root.display().to_string(),
                "source": source_str,
                "isDefault": is_default,
                "schema": loaded.config.schema,
                "project": {
                    "name": loaded.config.project.name,
                    "language": format!("{:?}", loaded.config.project.language).to_lowercase(),
                    "arch": format!("{:?}", loaded.config.project.arch).to_lowercase(),
                },
                "build": {
                    "backend": loaded.config.build.backend.as_str(),
                    "command": loaded.config.build.command,
                    "targets": loaded.config.build.targets,
                    "artifacts": loaded.config.build.artifacts.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                },
                "run": {
                    "backend": loaded.config.run.backend.as_str(),
                    "boot": loaded.config.run.boot.as_str(),
                    "timeoutMs": loaded.config.run.timeout_ms,
                    "args": loaded.config.run.args,
                    "serial": {
                        "device": loaded.config.run.serial.device,
                        "teeToFile": loaded.config.run.serial.tee_to_file.as_ref().map(|p| p.display().to_string()),
                    },
                },
                "debug": {
                    "backend": loaded.config.debug.backend.as_str(),
                    "symbols": loaded.config.debug.symbols.as_ref().map(|p| p.display().to_string()),
                    "stub": {
                        "host": loaded.config.debug.stub.host,
                        "port": loaded.config.debug.stub.port,
                        "mode": format!("{:?}", loaded.config.debug.stub.mode).to_lowercase(),
                    },
                },
            }))
        }
        Err(e) => err(
            ShellErrorCode::InvalidConfig,
            e.message,
            e.detail.unwrap_or_default(),
        ),
    }
}

/// `princess:project:validate` — validate a project's config without loading backends.
///
/// Args: `{ path?: string }`
///
/// Returns: `{ valid: true, config: ... }` or an error.
pub fn project_validate(args: &Value) -> Value {
    let path_arg = args.get("path").and_then(Value::as_str);
    let root = match path_arg {
        Some(p) => std::path::PathBuf::from(p),
        None => match resolve_root() {
            Ok(r) => r,
            Err(f) => return f.into_value(),
        },
    };

    match ProjectConfig::load_or_default(&root) {
        Ok(loaded) => {
            let resolved = loaded.config.resolve(&root);
            // Check that the project has a Makefile or a build command.
            let has_makefile = root.join("Makefile").is_file();
            let has_build_cmd = loaded.config.build.command.is_some();
            let buildable = has_makefile || has_build_cmd;

            ok(json!({
                "valid": true,
                "root": root.display().to_string(),
                "name": resolved.name(),
                "buildable": buildable,
                "source": loaded.source.to_string(),
                "hasMakefile": has_makefile,
                "hasBuildCommand": has_build_cmd,
                "declaredArtifacts": resolved.declared_artifacts.len(),
                "targets": resolved.targets,
            }))
        }
        Err(e) => err(
            ShellErrorCode::InvalidConfig,
            e.message,
            e.detail.unwrap_or_default(),
        ),
    }
}
