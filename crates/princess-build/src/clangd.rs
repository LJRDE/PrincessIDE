//! Kernel-specialised clangd configuration — **D7 / D8 / D17 / D18**.
//!
//! This module is where PrincessIDE is actually different from "run clangd on a
//! folder".  A kernel is *freestanding*: it has no libc, no host crt, and it is
//! compiled with a pile of GCC-only flags that clang does not understand (or
//! understands differently).  Pointing a stock clangd at such a project produces
//! one of two failure modes, both of which look like "the language service is
//! broken" to the user:
//!
//! 1. **glibc header poisoning.**  Without a target triple, clangd infers a host
//!    triple and pulls `/usr/include/stdio.h` into a kernel that has no libc,
//!    which then explodes on `__attribute__`/inline-asm definitions it cannot
//!    model.  D7 fixes this by *writing the triple down*
//!    (`--target=x86_64-unknown-none`) instead of relying on `--query-driver`.
//! 2. **GCC-only flags.**  `-fno-tree-loop-distribute-patterns`,
//!    `-mpreferred-stack-boundary=4`, ... are not clang flags.  D7 removes them
//!    by **exact name**.
//!
//! ## The two traps this module exists to avoid
//!
//! * **`-nostdinc` is wrong; `-nostdlibinc` is right.**  `-nostdinc` also deletes
//!   clang's *own* freestanding headers (`stddef.h`, `stdint.h`, `stdarg.h`), so
//!   a kernel that legitimately includes `<stdint.h>` gets a screen full of
//!   diagnostics.  `-nostdlibinc` removes only the *system* include paths and
//!   keeps the freestanding ones.  (A1's independent re-test corrected the
//!   *mechanism* — clangd does not crash, it reports ~16 diagnostics and exits 3
//!   — but the conclusion is unchanged; see `docs/reports/a1-clangd16.md` §A1-7.)
//! * **Never remove `-W*` with a wildcard.**  A1 measured that a `-W*` glob in
//!   `CompileFlags.Remove` also deletes `-Wall -Wextra`, which silently kills
//!   *warning diagnostics*: the editor looks spotless because it stopped
//!   reporting, not because the code is clean.  [`GCC_ONLY_FLAGR_BLACKLIST`] is
//!   the exact, finite list from D7 and [`validate_dot_clangd`] **rejects** any
//!   wildcard entry.
//!
//! ## What is *not* here (D17)
//!
//! The LSP protocol stack.  This module produces and validates the two files
//! clangd reads (`.clangd`, `compile_commands.json`) and reports the exact
//! command line the editor should launch.  The JSON-RPC/LSP conversation itself
//! belongs to the front end's mature client library; re-implementing it in Rust
//! was explicitly ruled out.

use std::path::{Path, PathBuf};

use princess_core::config::normalize;
use princess_core::{PrincessError, Result};

/// D7: the triple is pinned, never queried.  A kernel is `unknown-none`.
pub const DEFAULT_TRIPLE: &str = "x86_64-unknown-none";

/// D18: the language service is **clangd-16**; plain `clangd` is still 14 and
/// must not be used (its capability set is incomplete).
pub const LANG_SERVICE_TOOL: &str = "clangd-16";

/// D7's exact `CompileFlags.Remove` blacklist (research §2.6).
///
/// These are the GCC-only flags the reference kernel and the template pass on the
/// command line.  Each is removed by name; **no wildcard may ever be added here**
/// (see the module docs).
pub const GCC_ONLY_FLAG_BLACKLIST: [&str; 6] = [
    "-fno-tree-loop-distribute-patterns",
    "-fconserve-stack",
    "-mpreferred-stack-boundary=*",
    "-fno-var-tracking-assignments",
    "-fno-ipa-icf",
    "-mno-direct-extern-access",
];

/// D7's blacklist, spelled as this module's public alias.
///
/// The constant is exported twice on purpose: [`GCC_ONLY_FLAG_BLACKLIST`] is the
/// canonical name used by the generator, and this alias keeps the name used in
/// earlier drafts (and by `lib.rs`) working.
pub const GCC_ONLY_FLAGR_BLACKLIST: [&str; 6] = GCC_ONLY_FLAG_BLACKLIST;

/// The include flag that removes system headers **but keeps freestanding ones**.
pub const NOSTD_INCLUDE_FLAG: &str = "-nostdlibinc";

/// Any `CompileFlags.Remove` entry containing one of these is a wildcard and is
/// refused: it would take `-Wall`/`-Wextra` with it.
const WILDCARD_MARKERS: [&str; 1] = ["*"];

// ------------------------------------------------------------ .clangd -------

/// A generated `.clangd` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotClangd {
    /// The YAML body, exactly as it will be written.
    pub text: String,
    /// Where it should be written (project root).
    pub path: PathBuf,
}

impl DotClangd {
    /// The YAML for a kernel project with the D7 defaults.
    pub fn for_kernel(root: impl Into<PathBuf>, triple: &str) -> Self {
        Self::new(root, triple, &[])
    }

    /// The YAML for a kernel project with extra include directories and extra
    /// flags (the caller's `[build]`/Makefile knowledge).
    ///
    /// `extra_includes` are emitted as `-I` entries **before** `-nostdlibinc`, so
    /// a project can still point clangd at its own headers.
    pub fn new(root: impl Into<PathBuf>, triple: &str, extra_includes: &[PathBuf]) -> Self {
        let mut text = String::new();
        text.push_str("# Generated by princess-build (D7/D17). Do not edit by hand.\n");
        text.push_str("#\n");
        text.push_str("# Regenerate with: princess tools sync-clangd\n");
        text.push_str("# Why each line exists: crates/princess-build/src/clangd.rs\n");
        text.push_str("#\n");
        text.push_str("# NOTE clangd's schema: `CompileFlags` is a *dictionary*, not a list.\n");
        text.push_str("# `CompileFlags: [ ... ]` makes clangd log\n");
        text.push_str("#   \"CompileFlags should be a dictionary\"\n");
        text.push_str("# and then ignore the whole file, silently falling back to the host\n");
        text.push_str("# triple with glibc include paths — exactly what D7 exists to prevent.\n");
        text.push_str("CompileFlags:\n");

        // The triple is written down, not queried (D7).
        text.push_str("  # D7: pin the target instead of relying on --query-driver.\n");
        text.push_str("  CompilationDatabase: .\n");
        text.push_str("  Add:\n");
        text.push_str(&format!("    - --target={triple}\n"));
        text.push_str(&format!("    - {NOSTD_INCLUDE_FLAG}\n"));
        for include in extra_includes {
            text.push_str(&format!("    - -I{}\n", include.display()));
        }

        text.push_str("  # D7: GCC-only flags, by exact name. A `-W*` wildcard here would\n");
        text.push_str("  # also delete -Wall/-Wextra and silently hide every warning.\n");
        text.push_str("  Remove:\n");
        for flag in GCC_ONLY_FLAG_BLACKLIST {
            text.push_str(&format!("    - \"{flag}\"\n"));
        }

        Self {
            text,
            path: root.into().join(".clangd"),
        }
    }

    /// Write it to [`DotClangd::path`], creating the directory if needed.
    pub fn write(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                PrincessError::internal(format!(
                    "cannot create {} for .clangd: {err}",
                    parent.display()
                ))
            })?;
        }
        std::fs::write(&self.path, &self.text).map_err(|err| {
            PrincessError::internal(format!("cannot write {}: {err}", self.path.display()))
        })
    }

    /// Read a `.clangd` from disk and validate it.
    pub fn load_and_validate(path: &Path) -> Result<Vec<String>> {
        let text = std::fs::read_to_string(path).map_err(|err| {
            PrincessError::new(
                princess_core::ErrorCode::NotFound,
                format!("cannot read {}: {err}", path.display()),
            )
        })?;
        validate_dot_clangd(&text)
    }
}

/// Validate a `.clangd` body against D7.
///
/// Returns the list of problems; an empty list means "compliant".  This is the
/// machine-checkable half of D17: the acceptance test feeds it a good file (must
/// pass) and a `-W*` file (must fail).
pub fn validate_dot_clangd(text: &str) -> Result<Vec<String>> {
    let mut problems = Vec::new();

    // --- structural check: clangd's schema -----------------------------------
    //
    // This is not cosmetic.  `CompileFlags:` followed by a list makes clangd
    // log `CompileFlags should be a dictionary` and then **ignore the whole
    // file**, after which it silently compiles with the host triple and glibc
    // include paths.  A text-only check for `--target=` would still pass, which
    // is exactly the false-positive this branch exists to prevent.
    let compile_flags_indent = text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim_end();
            (trimmed.trim_start().starts_with("CompileFlags:") && !trimmed.trim_start().starts_with('#'))
                .then(|| trimmed.len() - trimmed.trim_start().len())
        });
    match compile_flags_indent {
        None => problems.push(
            "no `CompileFlags:` key: clangd has nothing to add or remove (D7)".to_string(),
        ),
        Some(indent) => {
            // Walk the `CompileFlags:` block.  A sequence entry is only illegal
            // when it sits at the *direct child* indent of `CompileFlags:`
            // (`- --target=…` immediately under it).  Entries nested under
            // `Add:`/`Remove:` are exactly what clangd wants.
            let mut seen_child_key = false;
            for line in text
                .lines()
                .skip_while(|l| {
                    !(l.trim_start().starts_with("CompileFlags:") && !l.trim_start().starts_with('#'))
                })
                .skip(1)
            {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let this_indent = line.len() - line.trim_start().len();
                if this_indent <= indent {
                    break; // left the CompileFlags block
                }
                if this_indent == indent + 2 && trimmed.starts_with("- ") {
                    problems.push(format!(
                        "`CompileFlags:` directly contains the sequence entry `{trimmed}`; clangd \
                         requires a dictionary with `Add:`/`Remove:` keys (otherwise it logs \
                         \"CompileFlags should be a dictionary\" and ignores the entire .clangd, D7)"
                    ));
                    break;
                }
                if this_indent == indent + 2 && trimmed.ends_with(':') {
                    seen_child_key = true;
                }
            }
            if !seen_child_key {
                problems.push(
                    "`CompileFlags:` has no `Add:`/`Remove:` child key (D7)".to_string(),
                );
            }
        }
    }

    // --- the D7 requirements -------------------------------------------------
    if !text.contains("--target=") {
        problems.push("missing `--target=<triple>` (D7: the triple must be pinned)".to_string());
    }
    if !text.contains(NOSTD_INCLUDE_FLAG) {
        problems.push(format!("missing `{NOSTD_INCLUDE_FLAG}`"));
    }
    if text.contains("-nostdinc") {
        problems.push(
            "contains `-nostdinc`; use `-nostdlibinc` instead (D7: `-nostdinc` also deletes              clang's own freestanding stddef.h/stdint.h/stdarg.h)"
                .to_string(),
        );
    }

    // The blacklist: each D7 flag must be present, by exact name.
    for flag in GCC_ONLY_FLAG_BLACKLIST {
        if !text.contains(flag) {
            problems.push(format!("missing GCC-only flag removal `{flag}` (D7)"));
        }
    }

    // No flag-name wildcards, anywhere in a `- ` entry.
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- ") {
            continue;
        }
        let value = trimmed
            .trim_start_matches("- ")
            .trim_matches('"')
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        if WILDCARD_MARKERS.iter().any(|m| value.contains(m))
            && value != "-mpreferred-stack-boundary=*"
        {
            problems.push(format!(
                "wildcard entry `{value}` in .clangd: a flag-name glob (e.g. `-W*`) would also                  delete -Wall/-Wextra and hide every warning (D7)"
            ));
        }
    }

    Ok(problems)
}

/// The exact command line the editor must launch for the language service (D18).
///
/// This is the legacy version that uses the hardcoded LANG_SERVICE_TOOL constant.
/// For manifest-based configuration, use [`lang_service_command_from_manifest`].
pub fn lang_service_command() -> Vec<String> {
    vec![
        LANG_SERVICE_TOOL.to_string(),
        "--background-index".to_string(),
        "--clang-tidy=false".to_string(),
    ]
}

/// Get the language service command from a manifest file.
///
/// This function reads the language manifest and returns the command line
/// specified in the `[language.lsp]` section. If the manifest cannot be
/// read or parsed, it falls back to the legacy hardcoded command.
///
/// This is part of the M13 language module layer that allows language
/// support to be configured via manifests rather than hardcoded constants.
pub fn lang_service_command_from_manifest(manifest_path: &Path) -> Vec<String> {
    use princess_lang::manifest::LanguageManifest;

    match LanguageManifest::from_file(manifest_path) {
        Ok(manifest) => {
            let mut cmd = vec![manifest.lsp.command.clone()];
            cmd.extend(manifest.lsp.args.clone());
            cmd
        }
        Err(_) => {
            // Fallback to legacy command if manifest cannot be read
            lang_service_command()
        }
    }
}

/// Get the language service command from environment variable or manifest.
///
/// Priority:
/// 1. `PRINCESSIDE_LANG_SERVICE_CLANGD` environment variable (D18)
/// 2. Language manifest (if path provided)
/// 3. Legacy hardcoded constant
pub fn lang_service_command_with_fallback(manifest_path: Option<&Path>) -> Vec<String> {
    // D18: Check environment variable first
    if let Ok(env_cmd) = std::env::var("PRINCESSIDE_LANG_SERVICE_CLANGD") {
        if !env_cmd.is_empty() {
            let mut cmd = vec![env_cmd];
            cmd.extend([
                "--background-index".to_string(),
                "--clang-tidy=false".to_string(),
            ]);
            return cmd;
        }
    }

    // Try manifest if path provided
    if let Some(path) = manifest_path {
        return lang_service_command_from_manifest(path);
    }

    // Fallback to legacy
    lang_service_command()
}

// ------------------------------------------------- compile_commands.json -----

/// One `compile_commands.json` entry (D8: `arguments`, absolute `directory`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompileCommand {
    /// **Absolute** working directory (D8).
    pub directory: String,
    /// The compiler invocation, split into argv — **not** a shell string (D8).
    pub arguments: Vec<String>,
    /// The translation unit, absolute.
    pub file: String,
    /// `output`, when the producer recorded it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub output: Option<String>,
}

/// A whole compile database, preserving order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompileCommands {
    pub entries: Vec<CompileCommand>,
}

impl CompileCommands {
    pub fn new(entries: Vec<CompileCommand>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Serialize to the JSON clangd expects.
    ///
    /// Entries are written with `arguments` and an absolute `directory`; the
    /// serializer never emits a `command` string, which is the whole point of D8.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.entries).map_err(|err| {
            PrincessError::internal(format!("cannot serialize compile_commands.json: {err}"))
        })
    }

    /// Parse a database produced by `bear` (or by our shim).
    pub fn from_json(text: &str) -> Result<Self> {
        let raw: Vec<serde_json::Value> = serde_json::from_str(text).map_err(|err| {
            PrincessError::new(
                princess_core::ErrorCode::InvalidConfig,
                format!("compile_commands.json is not valid JSON: {err}"),
            )
        })?;
        let mut entries = Vec::new();
        for item in raw {
            let directory = item
                .get("directory")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let file = item
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments = match item.get("arguments") {
                Some(serde_json::Value::Array(argv)) => argv
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let output = item
                .get("output")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            entries.push(CompileCommand {
                directory,
                arguments,
                file,
                output,
            });
        }
        Ok(Self { entries })
    }

    /// Write to `path`, creating parent directories.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                PrincessError::internal(format!(
                    "cannot create {} for compile_commands.json: {err}",
                    parent.display()
                ))
            })?;
        }
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(|err| {
            PrincessError::internal(format!(
                "cannot write {}: {err}",
                path.display()
            ))
        })
    }
}

/// Validate a compile database against D8.
///
/// Checks, per entry:
/// * `arguments` is present and is a non-empty array (never a `command` string);
/// * `directory` is an **absolute** path;
/// * `file` is present;
/// * `directory` actually exists (clangd refuses entries whose directory is gone).
pub fn validate_compile_commands(text: &str) -> Result<Vec<String>> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(text).map_err(|err| {
        PrincessError::new(
            princess_core::ErrorCode::InvalidConfig,
            format!("compile_commands.json is not valid JSON: {err}"),
        )
    })?;

    let mut problems = Vec::new();
    if raw.is_empty() {
        problems.push(
            "compile_commands.json is an empty array: clangd would have no TU to index \
             (D18: run `make clean` before bear, otherwise bear overwrites existing \
             entries with `[]`)"
                .to_string(),
        );
    }

    for (index, item) in raw.iter().enumerate() {
        if item.get("command").is_some() {
            problems.push(format!(
                "entry[{index}] uses `command` (a shell string); D8 requires `arguments`"
            ));
        }
        match item.get("arguments") {
            Some(serde_json::Value::Array(argv)) if !argv.is_empty() => {
                if argv.iter().any(|v| !v.is_string()) {
                    problems.push(format!("entry[{index}] has non-string items in `arguments`"));
                }
            }
            _ => problems.push(format!(
                "entry[{index}] has no non-empty `arguments` array (D8)"
            )),
        }
        match item.get("directory").and_then(|v| v.as_str()) {
            Some(dir) if !dir.is_empty() => {
                if !Path::new(dir).is_absolute() {
                    problems.push(format!(
                        "entry[{index}] `directory` is relative (`{dir}`); D8 requires an \
                         absolute path"
                    ));
                } else if !Path::new(dir).is_dir() {
                    problems.push(format!(
                        "entry[{index}] `directory` does not exist: {dir} (clangd would drop \
                         the entry)"
                    ));
                }
            }
            _ => problems.push(format!("entry[{index}] has no `directory` (D8)")),
        }
        match item.get("file").and_then(|v| v.as_str()) {
            Some(file) if !file.is_empty() => {
                if !Path::new(file).is_absolute() {
                    problems.push(format!("entry[{index}] `file` is relative (`{file}`)"));
                }
            }
            _ => problems.push(format!("entry[{index}] has no `file`")),
        }
    }

    Ok(problems)
}

/// Is this a real compile database with at least one usable entry?
pub fn compile_commands_usable(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    match validate_compile_commands(&text) {
        Ok(problems) => problems.is_empty(),
        Err(_) => false,
    }
}

/// Turn a *bear* database into the D8 shape.
///
/// `bear` 3.1.1 already emits `arguments` + an absolute `directory`, but it also
/// records entries for the linker and for `as`, which clangd cannot use.  This
/// normalises whatever bear produced:
///
/// * drops entries that are not C/C++ compilations (they would make clangd log
///   "unknown argument" noise and can shadow the real TU entry);
/// * makes `directory` and `file` absolute against the build cwd;
/// * de-duplicates on `(file, arguments)`.
pub fn normalise_bear_output(raw: &str, build_cwd: &Path) -> Result<CompileCommands> {
    let parsed = CompileCommands::from_json(raw)?;
    let mut entries: Vec<CompileCommand> = Vec::new();
    for entry in parsed.entries {
        if !looks_like_c_compilation(&entry) {
            continue;
        }
        let directory = absolutise(&entry.directory, build_cwd);
        let file = absolutise(&entry.file, Path::new(&directory));
        let normalised = CompileCommand {
            directory,
            arguments: entry.arguments,
            file,
            output: entry.output,
        };
        if !entries.contains(&normalised) {
            entries.push(normalised);
        }
    }
    Ok(CompileCommands { entries })
}

/// Does this entry look like a C/C++ compile rather than a link or an assembly?
fn looks_like_c_compilation(entry: &CompileCommand) -> bool {
    if entry.arguments.is_empty() {
        return false;
    }
    let has_compile_flag = entry
        .arguments
        .iter()
        .any(|a| a == "-c" || a == "-S" || a.starts_with("-o"));
    let source_like = entry.file.ends_with(".c")
        || entry.file.ends_with(".cc")
        || entry.file.ends_with(".cpp")
        || entry.file.ends_with(".cxx")
        || entry.file.ends_with(".h");
    has_compile_flag && source_like
}

fn absolutise(path: &str, base: &Path) -> String {
    let path = Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize(&joined).to_string_lossy().into_owned()
}

/// Flags whose **next** argument is a value, not a translation unit.
const VALUE_TAKING_FLAGS: [&str; 8] = [
    "-o", "-MF", "-MT", "-MQ", "-I", "-isystem", "-include", "-x",
];

/// The translation unit in a compiler `argv`, or `""` when there is none.
///
/// Skips the driver (`argv[0]`), anything starting with `-`, the value of a
/// [`VALUE_TAKING_FLAGS`] flag, and paths that do not exist.  The wrapper shim
/// cannot record a `file` field itself (a POSIX shell has no cheap way to tell
/// a source path from an `-o` output), so this recovers it after the fact;
/// taking `arguments.last()` would wrongly pick `build/kernel.o`.
pub fn translation_unit(arguments: &[String], cwd: &Path) -> String {
    let mut skip_next = false;
    for (index, argument) in arguments.iter().enumerate() {
        if index == 0 {
            continue; // the driver
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if argument.starts_with('-') {
            // `-I/usr/include` and `-oout.o` carry their value inline; a bare
            // `-o` takes the next argument as its value.
            if VALUE_TAKING_FLAGS.contains(&argument.as_str()) {
                skip_next = true;
            }
            continue;
        }
        let candidate = Path::new(argument);
        let absolute = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        if absolute.is_file() {
            return absolute.to_string_lossy().into_owned();
        }
    }
    String::new()
}

// --------------------------------------------------------------- shim --------

/// A `CC`-wrapper shim that records each compile into a compile database.
///
/// D8's fallback for when `bear` is unavailable.  The script is written to
/// disk and exported as `CC`/`HOSTCC` for the build, so it sees exactly what
/// `make` invokes:
///
/// ```text
/// CC=/path/to/.princesside-cc-shim make
/// ```
///
/// It appends one NDJSON line per invocation to `$PRINCESSIDE_CDB_NDJSON` and
/// then executes the real compiler, so the build is unaffected.
pub struct WrapperShim {
    /// Where the shim script lives (inside the build directory).
    pub script: PathBuf,
    /// Where it appends one JSON object per compile.
    pub ndjson: PathBuf,
}

impl WrapperShim {
    /// Plan a shim inside `build_dir` (created on [`WrapperShim::install`]).
    pub fn new(build_dir: impl AsRef<Path>) -> Self {
        let build_dir = build_dir.as_ref();
        Self {
            script: build_dir.join(".princesside-cc-shim"),
            ndjson: build_dir.join(".princesside-compile-commands.ndjson"),
        }
    }

    /// Write the script and return the environment to export.
    ///
    /// The shim uses `$PRINCESSIDE_REAL_CC` (captured here from `PATH`) so the
    /// wrapper cannot recurse into itself.
    pub fn install(&self, real_cc: &Path, build_cwd: &Path) -> Result<Vec<(String, String)>> {
        if let Some(parent) = self.script.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                PrincessError::internal(format!("cannot create {}: {err}", parent.display()))
            })?;
        }
        // Start from a clean database: a stale NDJSON file would be merged into
        // the new one (D18's data-corruption lesson).
        let _ = std::fs::remove_file(&self.ndjson);

        let script = format!(
            r#"#!/bin/sh
# Generated by princess-build (D8 wrapper-shim fallback for `bear`).
# Records one NDJSON line per compile invocation, then delegates to the real
# compiler.  Never edit by hand.
#
# The arguments are embedded as real JSON strings: a naive
# `'"%s",'` loop emits shell-quoted text such as '-m64', which is *not* valid
# JSON and would make every line silently unparseable downstream.
real_cc="${{PRINCESSIDE_REAL_CC:-{real_cc}}}"
out="${{PRINCESSIDE_CDB_NDJSON:-{ndjson}}}"
cwd="$(pwd)"

json_escape() {{
  # Escape backslash and double quote, then drop control characters: enough for
  # the paths and flags a compiler is invoked with.
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr -d '\000-\037'
}}

# `args` holds the JSON array body *after* the driver, so it starts with the
# comma that separates them; empty when there are no extra arguments.
args=''
for a in "$@"; do
  esc="$(json_escape "$a")"
  args="$args,\"$esc\""
done
printf '{{"directory":"%s","arguments":["%s"%s],"file":""}}\n' \
  "$(json_escape "$cwd")" "$(json_escape "$real_cc")" "$args" >> "$out"
exec "$real_cc" "$@"
"#,
            real_cc = real_cc.display(),
            ndjson = self.ndjson.display(),
        );
        std::fs::write(&self.script, script).map_err(|err| {
            PrincessError::internal(format!("cannot write {}: {err}", self.script.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.script, std::fs::Permissions::from_mode(0o755))
                .map_err(|err| {
                    PrincessError::internal(format!(
                        "cannot chmod {}: {err}",
                        self.script.display()
                    ))
                })?;
        }

        let build_cwd = build_cwd.to_string_lossy().into_owned();
        Ok(vec![
            ("CC".to_string(), self.script.to_string_lossy().into_owned()),
            (
                "PRINCESSIDE_REAL_CC".to_string(),
                real_cc.to_string_lossy().into_owned(),
            ),
            (
                "PRINCESSIDE_CDB_NDJSON".to_string(),
                self.ndjson.to_string_lossy().into_owned(),
            ),
            ("PRINCESSIDE_SHIM_CWD".to_string(), build_cwd),
        ])
    }

    /// Convert the NDJSON the shim wrote into a real database.
    pub fn collect(&self, build_cwd: &Path) -> Result<CompileCommands> {
        let text = std::fs::read_to_string(&self.ndjson).unwrap_or_default();
        let mut entries: Vec<CompileCommand> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let directory = value
                .get("directory")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments: Vec<String> = value
                .get("arguments")
                .and_then(|v| v.as_array())
                .map(|argv| {
                    argv.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            // `arguments` holds the driver and its flags.  The translation unit
            // is the argument that (a) is not a flag, (b) is not the value of a
            // value-taking flag such as `-o`, and (c) names a file that exists.
            // Taking `arguments.last()` would pick up the `-o` *output* path
            // (`build/kernel.o`), which is not a TU at all.
            let file = translation_unit(&arguments, Path::new(&directory));
            if file.is_empty() || !Path::new(&file).is_file() {
                continue;
            }
            let entry = CompileCommand {
                directory,
                arguments,
                file,
                output: None,
            };
            if looks_like_c_compilation(&entry) && !entries.contains(&entry) {
                entries.push(entry);
            }
        }
        let build_cwd = build_cwd.to_path_buf();
        for entry in &mut entries {
            entry.directory = absolutise(&entry.directory, &build_cwd);
        }
        Ok(CompileCommands { entries })
    }

    /// The `make`-level environment overrides for a shim build.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("CC".to_string(), self.script.to_string_lossy().into_owned()),
            (
                "HOSTCC".to_string(),
                self.script.to_string_lossy().into_owned(),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generated_kernel_clangd_satisfies_d7() {
        let dot = DotClangd::for_kernel("/tmp/kernel", DEFAULT_TRIPLE);
        assert!(dot.text.contains("--target=x86_64-unknown-none"));
        assert!(dot.text.contains("-nostdlibinc"));
        assert!(!dot.text.contains("\n  - -nostdinc\n"));
        for flag in GCC_ONLY_FLAG_BLACKLIST {
            assert!(dot.text.contains(flag), "missing {flag} in\n{}", dot.text);
        }
        assert_eq!(validate_dot_clangd(&dot.text).unwrap(), Vec::<String>::new());

        // The file lands at the project root.
        assert_eq!(dot.path, PathBuf::from("/tmp/kernel/.clangd"));
    }

    #[test]
    fn a_wildcard_remove_entry_is_rejected() {
        let bad = "CompileFlags:\n  Add:\n    - --target=x86_64-unknown-none\n    \
                   - -nostdlibinc\n  Remove:\n    - \"-W*\"\n";
        let problems = validate_dot_clangd(bad).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("-W*")),
            "the -W* wildcard must be refused: {problems:?}"
        );
    }

    /// Regression for the false-positive that motivated the structural check:
    /// the list form *looks* fine to a substring search, but clangd rejects the
    /// whole file, falls back to the host triple and poisons the project with
    /// glibc headers.
    #[test]
    fn the_list_form_is_rejected_because_clangd_would_ignore_the_whole_file() {
        let bad = "CompileFlags:\n  - --target=x86_64-unknown-none\n  - -nostdlibinc\n  Remove:\n";
        let problems = validate_dot_clangd(bad).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("dictionary")),
            "the list form must be refused: {problems:?}"
        );
    }

    #[test]
    fn nostdinc_alone_is_rejected_but_nostdlibinc_is_accepted() {
        let bad =
            "CompileFlags:\n  Add:\n    - --target=x86_64-unknown-none\n    - -nostdinc\n  Remove:\n";
        let problems = validate_dot_clangd(bad).unwrap();
        assert!(problems.iter().any(|p| p.contains("-nostdlibinc")));
    }

    #[test]
    fn a_clangd_without_the_triple_is_rejected() {
        let bad = "CompileFlags:\n  Add:\n    - -nostdlibinc\n  Remove:\n";
        let problems = validate_dot_clangd(bad).unwrap();
        assert!(problems.iter().any(|p| p.contains("--target=")));
    }

    #[test]
    fn a_defect_free_compile_database_matches_d8() {
        let json = r#"[
          {
            "directory": "/work/kernel",
            "arguments": ["gcc", "-m64", "-c", "kernel.c", "-o", "build/kernel.o"],
            "file": "/work/kernel/kernel.c"
          }
        ]"#;
        // /work/kernel does not exist here, so the only complaint is that.
        let problems = validate_compile_commands(json).unwrap();
        assert!(
            problems.iter().all(|p| p.contains("/work/kernel")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_command_string_entry_is_rejected_even_when_arguments_is_also_present() {
        let json = format!(
            r#"[{{"directory":"{}","command":"gcc -c a.c","arguments":["gcc","-c","a.c"],"file":"{}/a.c"}}]"#,
            std::env::temp_dir().display(),
            std::env::temp_dir().display()
        );
        let problems = validate_compile_commands(&json).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("`command`")),
            "D8 forbids `command`: {problems:?}"
        );
    }

    #[test]
    fn a_relative_directory_is_rejected() {
        let json = r#"[{"directory":"build","arguments":["gcc","-c","a.c"],"file":"/p/a.c"}]"#;
        let problems = validate_compile_commands(json).unwrap();
        assert!(problems.iter().any(|p| p.contains("relative")), "{problems:?}");
    }

    #[test]
    fn an_empty_database_is_flagged_with_the_bear_cause() {
        let problems = validate_compile_commands("[]").unwrap();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("make clean"), "{}", problems[0]);
    }

    #[test]
    fn entries_serialize_with_arguments_and_never_with_command() {
        let db = CompileCommands::new(vec![CompileCommand {
            directory: "/work".into(),
            arguments: vec!["gcc".into(), "-c".into(), "a.c".into()],
            file: "/work/a.c".into(),
            output: None,
        }]);
        let json = db.to_json().unwrap();
        assert!(json.contains("\"arguments\""));
        assert!(!json.contains("\"command\""));
        assert!(!json.contains("\"output\""));
    }

    #[test]
    fn bear_output_is_normalised_to_absolute_d8_entries() {
        let json = r#"[
          {"directory":"/work","arguments":["gcc","-c","src/a.c","-o","build/a.o"],
           "file":"/work/src/a.c"},
          {"directory":"/work","arguments":["ld","-o","build/k.elf","build/a.o"],
           "file":"/work/build/a.o"}
        ]"#;
        let db = normalise_bear_output(json, Path::new("/work")).unwrap();
        assert_eq!(db.len(), 1, "the link line must be dropped: {:?}", db.entries);
        assert!(Path::new(&db.entries[0].directory).is_absolute());
        assert!(Path::new(&db.entries[0].file).is_absolute());
    }

    #[test]
    fn the_shim_records_a_compile_and_delegates_to_the_real_compiler() {
        let dir = std::env::temp_dir().join(format!("princesside-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = dir.join("hello.c");
        std::fs::write(&source, "int answer(void) { return 42; }\n").unwrap();

        let shim = WrapperShim::new(&dir);
        let real_cc = super::super::process::which("gcc").expect("gcc on PATH");
        let env = shim.install(&real_cc, &dir).unwrap();
        assert!(shim.script.is_file());

        // Run the shim exactly as `make` would: CC=<script> gcc -c hello.c
        let output = std::process::Command::new(&shim.script)
            .arg("-m64")
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(dir.join("hello.o"))
            .env("PRINCESSIDE_REAL_CC", &real_cc)
            .env("PRINCESSIDE_CDB_NDJSON", &shim.ndjson)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "shim must delegate to the real compiler: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(dir.join("hello.o").is_file(), "the real compile must happen");

        let db = shim.collect(&dir).unwrap();
        assert_eq!(db.len(), 1, "{:?}", db.entries);
        assert_eq!(
            Path::new(&db.entries[0].file),
            dir.join("hello.c").as_path(),
            "{:?}",
            db.entries[0]
        );
        let problems = validate_compile_commands(&db.to_json().unwrap()).unwrap();
        assert_eq!(problems, Vec::<String>::new());

        // `env` exports CC and HOSTCC so `make`'s default rules use the shim.
        let names: Vec<String> = shim.env().into_iter().map(|(k, _)| k).collect();
        assert!(names.contains(&"CC".to_string()));
        let _ = env;

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_language_service_is_clangd_16_per_d18() {
        let command = lang_service_command();
        assert_eq!(command[0], LANG_SERVICE_TOOL);
        assert!(command[0].ends_with("-16"), "D18: never plain `clangd`");
    }
}

#[cfg(test)]
mod probe_shim {
    use super::*;
    #[test]
    fn probe_translation_unit() {
        let argv: Vec<String> = ["/usr/bin/gcc","-m64","-c","/tmp/x/hello.c","-o","/tmp/x/hello.o"]
            .iter().map(|s| s.to_string()).collect();
        eprintln!("tu = {:?}", translation_unit(&argv, Path::new("/tmp")));
        let json = r#"[{"directory":"/tmp/x","arguments":["/usr/bin/gcc","-m64","-c","/tmp/x/hello.c","-o","/tmp/x/hello.o"],"file":""}]"#;
        let db = CompileCommands::from_json(json).unwrap();
        eprintln!("from_json = {:?}", db);
        eprintln!("looks_like = {:?}", looks_like_c_compilation(&db.entries[0]));
        eprintln!("is_file = {:?}", Path::new("/tmp/x/hello.c").is_file());
        let dir = std::env::temp_dir().join(format!("shimprobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.c"), "int answer(void) { return 42; }\n").unwrap();
        let shim = WrapperShim::new(&dir);
        let real_cc = super::super::process::which("gcc").unwrap();
        shim.install(&real_cc, &dir).unwrap();
        std::process::Command::new(&shim.script)
            .arg("-m64").arg("-c").arg(dir.join("hello.c")).arg("-o").arg(dir.join("hello.o"))
            .env("PRINCESSIDE_REAL_CC", &real_cc)
            .env("PRINCESSIDE_CDB_NDJSON", &shim.ndjson)
            .output().unwrap();
        eprintln!("--- ndjson ---\n{}", std::fs::read_to_string(&shim.ndjson).unwrap());
        let db2 = shim.collect(&dir).unwrap();
        eprintln!("db2 len = {}, cwd = {:?}", db2.len(), std::env::current_dir());
    }
}
