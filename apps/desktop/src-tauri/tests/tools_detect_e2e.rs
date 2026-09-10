//! P3 end-to-end evidence for the `princess:tools:detect` vertical slice.
//!
//! These are *integration* tests: they link the same library the Tauri binary
//! links and drive the same `doctor::run_doctor` path the IPC handler drives.
//! No window, no display, no Tauri runtime — which is exactly why this is the
//! strongest evidence available in this container (docs/spec/20-acceptance.md §0:
//! GUI视觉检查不在自动验收范围内).
//!
//! Covered:
//!   1. the real `scripts/doctor.sh` is executed and parsed end to end;
//!   2. the §3 envelope shape (`{ok:true,data}` with camelCase fields);
//!   3. the negative paths — a missing required tool, a missing workspace, and
//!      cancellation through the shared op registry (`E_CANCELLED`).

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use princesside_desktop_lib::contract::{err, ok, ErrorCode};
use princesside_desktop_lib::doctor::{
    cancelled_envelope, resolve_root, run_doctor, to_envelope, DoctorOutcome,
};
use princesside_desktop_lib::ops::OpRegistry;

/// Create a throwaway "workspace" whose scripts/doctor.sh is a stub.
fn stub_workspace(name: &str, doctor_body: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("princesside-p3-{}-{}", name, std::process::id()));
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).expect("create stub scripts dir");
    fs::write(scripts.join("env.sh"), "# stub env (sourced by run_doctor)\n").expect("write env.sh");
    fs::write(scripts.join("doctor.sh"), doctor_body).expect("write doctor.sh");
    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detects_the_real_toolchain_through_scripts_doctor_sh() {
    let root = resolve_root().expect("workspace root must be discoverable");
    assert!(root.join("scripts/doctor.sh").is_file(), "{}", root.display());

    let ops = OpRegistry::new();
    let op = ops.register("tools:detect");
    let outcome = run_doctor(&root, &op).await.expect("doctor run must not error");

    let DoctorOutcome::Done(run) = outcome else {
        panic!("a normal run must complete, not cancel or time out");
    };

    assert_eq!(run.exit_code, 0, "doctor.sh exit code; stderr was:\n{}", run.stderr);
    assert!(
        run.stdout.contains("PrincessIDE toolchain doctor"),
        "doctor banner missing from stdout"
    );
    assert!(run.tools.len() >= 20, "expected the full tool list, got {}", run.tools.len());
    assert!(run.missing.is_empty(), "nothing required should be missing: {:?}", run.missing);

    // The tools this slice exists to check, by substring so a rename in
    // scripts/doctor.sh (which A1 owns) does not silently break the assertion.
    for needle in [
        "cargo",
        "rustc",
        "qemu-system-x86_64",
        "nasm",
        "gcc",
        "readelf",
        "objdump",
        "make",
        "clangd",
        "clang",
        "bear",
    ] {
        let tool = run
            .tools
            .iter()
            .find(|t| t.name.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not reported by doctor.sh: {:?}", run.tools));
        assert!(tool.available, "{needle} reported as unavailable ({})", tool.status);
        let path = tool
            .path
            .as_deref()
            .unwrap_or_else(|| panic!("{needle} has no path; doctor printed no path line"));
        assert!(std::path::Path::new(path).is_absolute(), "{path} is not absolute");
        assert!(std::path::Path::new(path).exists(), "{path} does not exist");
    }

    // Every reported row must carry the three facts the contract asks for.
    for tool in &run.tools {
        assert!(!tool.name.is_empty());
        assert!(!tool.status.is_empty());
        if tool.available {
            assert!(
                tool.version.is_some() || !tool.checks.is_empty(),
                "{} is available but has neither a version nor a check",
                tool.name
            );
        }
    }

    // §3 envelope shape, including camelCase wire keys.
    let envelope = to_envelope(run, &root);
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(envelope["data"]["exitCode"], json!(0));
    assert!(envelope["data"]["tools"].is_array());
    assert!(envelope["data"]["missing"].is_array());
    assert!(envelope["data"]["rawStdout"].is_string());
    assert!(envelope["data"]["rawStderr"].is_string());
    assert!(envelope["data"]["root"].is_string());
    assert!(envelope["data"]["command"].is_string(), "the exact command must be reported");
    let first = &envelope["data"]["tools"][0];
    for key in ["name", "status", "version", "path", "available", "required", "checks"] {
        assert!(first.get(key).is_some(), "tool row is missing the `{key}` field");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_required_tool_is_reported_with_its_exit_code() {
    let root = stub_workspace(
        "missing",
        "#!/usr/bin/env bash\n\
         printf 'TOOL                   STATUS     VERSION / PATH\\n'\n\
         printf -- '----\\n'\n\
         printf '%-22s %-10s %s\\n' nasm MISSING '(required: nasm)'\n\
         printf 'doctor: 0 tool(s) present, MISSING REQUIRED: nasm\\n' >&2\n\
         exit 1\n",
    );

    let ops = OpRegistry::new();
    let op = ops.register("tools:detect");
    let outcome = run_doctor(&root, &op).await.expect("stub run must not error");
    let DoctorOutcome::Done(run) = outcome else {
        panic!("stub run must complete");
    };

    assert_eq!(run.exit_code, 1, "the exit code must be propagated, not swallowed");
    assert_eq!(run.missing, vec!["nasm".to_string()]);
    assert!(run.stderr.contains("MISSING REQUIRED: nasm"), "raw stderr must be kept: {}", run.stderr);

    let nasm = run.tools.iter().find(|t| t.name == "nasm").expect("nasm row");
    assert!(!nasm.available);
    assert!(nasm.required);
    assert_eq!(nasm.path, None);

    // The table is still returned (the UI can show it) — with ok:true, because
    // detection itself succeeded; the *doctor* failed, and that is visible in
    // `exitCode`/`missing`.  Failing the IPC call here would hide the table.
    let envelope = to_envelope(run, &root);
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(envelope["data"]["exitCode"], json!(1));
    assert_eq!(envelope["data"]["missing"], json!(["nasm"]));

    fs::remove_dir_all(&root).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_live_detection_produces_E_CANCELLED() {
    let root = stub_workspace("cancel", "#!/usr/bin/env bash\nsleep 30\n");
    let ops = OpRegistry::new();
    let op = ops.register("tools:detect");

    // Cancel before the run starts: the registry flag is what the run loop polls,
    // so this is deterministic rather than a timing race.
    assert_eq!(ops.cancel(&op.op_id), princesside_desktop_lib::ops::CancelOutcome::Cancelled);

    let outcome = run_doctor(&root, &op).await.expect("cancelled run must not error");
    let DoctorOutcome::Cancelled(run) = outcome else {
        panic!("a pre-cancelled op must not report Done/TimedOut");
    };
    assert_eq!(run.exit_code, -1);
    assert!(run.stderr.contains("cancelled"), "{}", run.stderr);

    let envelope = cancelled_envelope(run);
    assert_eq!(envelope["ok"], json!(false));
    assert_eq!(envelope["error"]["code"], json!("E_CANCELLED"));

    fs::remove_dir_all(&root).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_without_doctor_script_reports_E_NOT_FOUND() {
    // `pick_root` is exercised directly: resolve_root() would otherwise fall back
    // to the real checkout, which is exactly the behaviour under test elsewhere.
    let empty = std::env::temp_dir().join(format!("princesside-p3-empty-{}", std::process::id()));
    fs::create_dir_all(&empty).expect("create empty dir");

    let failure = princesside_desktop_lib::doctor::pick_root(vec![empty.clone()])
        .expect_err("an empty directory is not a PrincessIDE workspace");
    assert_eq!(failure.code, ErrorCode::NotFound);
    assert_eq!(failure.code.as_str(), "E_NOT_FOUND");

    let value = failure.into_value();
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["error"]["code"], json!("E_NOT_FOUND"));
    assert!(value["error"]["detail"].as_str().unwrap().contains("scripts/doctor.sh"));

    // The two envelope constructors are the only way handlers answer, so their
    // shapes are pinned here too.
    assert_eq!(ok(json!({"a": 1}))["data"]["a"], json!(1));
    assert_eq!(err(ErrorCode::Timeout, "m", "d")["error"]["code"], json!("E_TIMEOUT"));

    fs::remove_dir_all(&empty).ok();
}
