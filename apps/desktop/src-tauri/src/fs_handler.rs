//! `princess:fs:read` / `princess:fs:write` — the editor's file I/O.
//!
//! These two commands are what stop the shell from being "a CodeMirror demo
//! buffer": without them there is no way to get a real file into the editor or
//! to write one back, which is exactly the gap the old registry comment
//! admitted to ("Real files are opened by the engine … later phase").
//!
//! They are deliberately small and deliberately paranoid.  Every path is
//! resolved against the project root and anything that escapes it is refused
//! with `E_SANDBOX_DENIED` instead of being read.
//!
//! What the confinement is and is not: the root comes from the user's own file
//! picker, so this does not defend against the user.  It stops the *editor*
//! from being turned into an arbitrary-file read/write primitive by a bad path
//! — a `..`, an absolute path smuggled in from the event stream or the language
//! server, or a symlink that points out of the tree.

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::contract::{err, ok, ErrorCode};

/// Refuse to pull more than this into the editor in one go: a kernel tree holds
/// multi-hundred-megabyte object files and the editor must not try to show one.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

fn denied(path: &str, root: &Path) -> Value {
    err(
        ErrorCode::SandboxDenied,
        format!("path escapes the project root: {path}"),
        format!(
            "resolved against {}.  Pick a file inside the opened project, or open that project first.",
            root.display()
        ),
    )
}

fn canonical_root(root: &str) -> Result<PathBuf, Value> {
    if root.trim().is_empty() {
        return Err(err(
            ErrorCode::InvalidConfig,
            "projectRoot is required",
            "pass the root returned by princess:project:open".to_string(),
        ));
    }
    std::fs::canonicalize(root).map_err(|e| {
        err(
            ErrorCode::NotFound,
            format!("project root does not exist: {root}"),
            e.to_string(),
        )
    })
}

/// Join `path` onto `root`, rejecting `..` before touching the filesystem.
fn join_checked(root: &Path, path: &str) -> Result<PathBuf, Value> {
    if path.trim().is_empty() {
        return Err(err(
            ErrorCode::InvalidConfig,
            "path is required",
            "received an empty path".to_string(),
        ));
    }
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    // `..` is rejected rather than normalised: it is the one component that can
    // climb out of a root which is itself reached through a symlink.
    if candidate.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(denied(path, root));
    }
    Ok(candidate)
}

fn arg_str<'a>(args: &'a Value, key: &str, cmd: &str) -> Result<&'a str, Value> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        err(
            ErrorCode::InvalidConfig,
            format!("{cmd} requires {{ {key}: string }}"),
            format!("received args: {args}"),
        )
    })
}

/// `princess:fs:read` → `{ path, bytes, content, truncated }`.
pub fn fs_read(args: &Value) -> Value {
    let root = match arg_str(args, "projectRoot", "princess:fs:read") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = match arg_str(args, "path", "princess:fs:read") {
        Ok(v) => v,
        Err(e) => return e,
    };

    let root = match canonical_root(root) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let candidate = match join_checked(&root, path) {
        Ok(c) => c,
        Err(e) => return e,
    };

    // Canonicalising the file itself is what actually proves containment: it
    // resolves every symlink on the way, so a link pointing outside the root is
    // caught here rather than being followed.
    let real = match std::fs::canonicalize(&candidate) {
        Ok(p) => p,
        Err(e) => return err(ErrorCode::NotFound, format!("cannot open {path}"), e.to_string()),
    };
    if !real.starts_with(&root) {
        return denied(path, &root);
    }

    let meta = match std::fs::metadata(&real) {
        Ok(m) => m,
        Err(e) => return err(ErrorCode::NotFound, format!("cannot stat {path}"), e.to_string()),
    };
    if !meta.is_file() {
        return err(
            ErrorCode::InvalidConfig,
            format!("not a regular file: {path}"),
            format!("{} is a directory or special file", real.display()),
        );
    }

    let truncated = meta.len() > MAX_READ_BYTES;
    let bytes = match read_prefix(&real, MAX_READ_BYTES) {
        Ok(b) => b,
        Err(e) => return err(ErrorCode::Internal, format!("cannot read {path}"), e.to_string()),
    };

    ok(json!({
        "path": real.display().to_string(),
        "bytes": meta.len(),
        // Lossy on purpose: a stray non-UTF-8 byte must show up as U+FFFD in the
        // buffer, never as a failed open.
        "content": String::from_utf8_lossy(&bytes),
        "truncated": truncated,
    }))
}

fn read_prefix(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf)?;
    Ok(buf)
}

/// `princess:fs:write` → `{ path, bytes, created }`.
pub fn fs_write(args: &Value) -> Value {
    let root = match arg_str(args, "projectRoot", "princess:fs:write") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = match arg_str(args, "path", "princess:fs:write") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let content = match arg_str(args, "content", "princess:fs:write") {
        Ok(v) => v,
        Err(e) => return e,
    };

    let root = match canonical_root(root) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let candidate = match join_checked(&root, path) {
        Ok(c) => c,
        Err(e) => return e,
    };

    // The file may not exist yet, so canonicalise its *parent*: that is where
    // an escape would have to happen, and it is the part that must exist.
    let Some(parent) = candidate.parent() else {
        return denied(path, &root);
    };
    let parent = match std::fs::canonicalize(parent) {
        Ok(p) => p,
        Err(e) => {
            return err(
                ErrorCode::NotFound,
                format!("cannot open the directory of {path}"),
                e.to_string(),
            )
        }
    };
    if !parent.starts_with(&root) {
        return denied(path, &root);
    }
    let Some(name) = candidate.file_name() else {
        return denied(path, &root);
    };
    let target = parent.join(name);

    let existed = target.exists();
    if existed && !target.is_file() {
        return err(
            ErrorCode::InvalidConfig,
            format!("not a regular file: {path}"),
            format!("{} is a directory or special file", target.display()),
        );
    }

    // Write to a sibling temp file, then rename.  A crash or a full disk then
    // leaves the original source file intact instead of half-written.
    let tmp = parent.join(format!(
        ".{}.princesside-{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&tmp, content.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return err(ErrorCode::Internal, format!("cannot write {path}"), e.to_string());
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return err(ErrorCode::Internal, format!("cannot replace {path}"), e.to_string());
    }

    ok(json!({
        "path": target.display().to_string(),
        "bytes": content.len(),
        "created": !existed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory; avoids a dev-dependency (the project forbids
    /// adding one just for tests).
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "princesside-fs-{}-{}-{:?}",
            name,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn root_str(dir: &Path) -> String {
        dir.display().to_string()
    }

    #[test]
    fn reads_a_file_inside_the_root() {
        let dir = scratch("read");
        std::fs::write(dir.join("a.c"), "int main(void) {}\n").unwrap();
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "a.c" }));
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["data"]["content"], "int main(void) {}\n");
        assert_eq!(v["data"]["truncated"], false);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_a_file_by_absolute_path_inside_the_root() {
        let dir = scratch("abs");
        let file = dir.join("b.c");
        std::fs::write(&file, "x\n").unwrap();
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": file.display().to_string() }));
        assert_eq!(v["ok"], true, "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_dir_traversal_is_denied() {
        let dir = scratch("traverse");
        std::fs::write(dir.join("inside.c"), "x\n").unwrap();
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "../etc/passwd" }));
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["error"]["code"], "E_SANDBOX_DENIED", "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_path_outside_the_root_is_denied() {
        let dir = scratch("outside");
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "/etc/hostname" }));
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["error"]["code"], "E_SANDBOX_DENIED", "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_not_found() {
        let dir = scratch("missing");
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "nope.c" }));
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["error"]["code"], "E_NOT_FOUND", "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_creates_then_reads_back() {
        let dir = scratch("write");
        let w = fs_write(&json!({
            "projectRoot": root_str(&dir), "path": "new.c", "content": "hello\n"
        }));
        assert_eq!(w["ok"], true, "{w}");
        assert_eq!(w["data"]["created"], true, "{w}");
        assert_eq!(w["data"]["bytes"], 6, "{w}");

        let r = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "new.c" }));
        assert_eq!(r["data"]["content"], "hello\n", "{r}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_reports_created_false_when_overwriting() {
        let dir = scratch("overwrite");
        std::fs::write(dir.join("a.c"), "old\n").unwrap();
        let w = fs_write(&json!({
            "projectRoot": root_str(&dir), "path": "a.c", "content": "new\n"
        }));
        assert_eq!(w["ok"], true, "{w}");
        assert_eq!(w["data"]["created"], false, "{w}");
        assert_eq!(std::fs::read_to_string(dir.join("a.c")).unwrap(), "new\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_traversal_is_denied_and_leaves_no_temp_file() {
        let dir = scratch("wtraverse");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let v = fs_write(&json!({
            "projectRoot": sub.display().to_string(), "path": "../escaped.c", "content": "x"
        }));
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["error"]["code"], "E_SANDBOX_DENIED", "{v}");
        assert!(!dir.join("escaped.c").exists(), "traversal must not write");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_arguments_are_invalid_config() {
        let dir = scratch("args");
        // fs_read needs projectRoot and path...
        for args in [json!({}), json!({ "projectRoot": root_str(&dir) })] {
            let v = fs_read(&args);
            assert_eq!(v["ok"], false, "{v}");
            assert_eq!(v["error"]["code"], "E_INVALID_CONFIG", "{v}");
        }
        // ...and fs_write needs content on top of those, so a read-shaped call
        // must not be silently accepted as an empty write.
        for args in [
            json!({}),
            json!({ "projectRoot": root_str(&dir), "path": "a.c" }),
        ] {
            let v = fs_write(&args);
            assert_eq!(v["ok"], false, "{v}");
            assert_eq!(v["error"]["code"], "E_INVALID_CONFIG", "{v}");
        }
        // An empty path is rejected too, rather than being treated as the root.
        let v = fs_read(&json!({ "projectRoot": root_str(&dir), "path": "   " }));
        assert_eq!(v["error"]["code"], "E_INVALID_CONFIG", "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
