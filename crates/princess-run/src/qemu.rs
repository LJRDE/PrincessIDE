//! QEMU argv construction — **D2** (boot path) and **D9** (serial/monitor flags).
//!
//! This module is the single place that decides what `qemu-system-x86_64` is
//! invoked with.  Everything here is a *frozen decision*, not a style choice:
//!
//! | rule | why it is not negotiable |
//! |---|---|
//! | `-display none -serial stdio -monitor none` (**D9**) | `-nographic` multiplexes the monitor onto the serial stream and mixes BIOS/iPXE noise into it; `-serial stdio` + `-monitor stdio` together make QEMU refuse to start |
//! | **never** `-nographic` (**D9**) | it silently changes capture semantics |
//! | GRUB ISO via `-cdrom … -boot d` (**D2**) | QEMU's multiboot option ROM only accepts ELF32 on `-kernel`; the fixtures are real x86_64 ELF64 |
//! | `-d int,cpu_reset,guest_errors` (**D9**) | `Triple fault` appears **only** in the `cpu_reset` output. Measured in this workspace: with `-d int` alone the count is 0; with `int,cpu_reset` it is 1, while the process still exits 0 |
//! | manifest args are *partitioned*, not concatenated | a manifest that repeats an engine-owned flag would otherwise let two `-serial` devices fight over stdout |
//!
//! `-no-reboot` is always added so a triple fault terminates the machine instead
//! of looping, but its exit status is **never** used as fault evidence (D9) —
//! [`crate::exit::QemuEvidence`] does that job from the debug log.

use std::path::{Path, PathBuf};

use princess_core::config::BootProtocol;
use princess_core::error::{ErrorCode, PrincessError, Result};
use princess_core::traits::{BootMedium, RunPlan};
use princess_core::types::{GdbStub, SerialCapture, StubMode};

/// Flags whose value the engine owns.  A manifest may still list them (the
/// reference fixture does); they are dropped and reported rather than
/// duplicated.
const ENGINE_OWNED_FLAGS: &[&str] = &[
    "-serial",
    "-monitor",
    "-display",
    "-nographic",
    "-d",
    "-D",
    "-cdrom",
    "-boot",
    "-kernel",
    "-drive",
    "-no-reboot",
    "-s",
    "-S",
    "-gdb",
];

/// The `-d` item list, in one place.  Both halves are **required** (D9).
pub const QEMU_DEBUG_ITEMS: &str = "int,cpu_reset,guest_errors";

/// Where a run's QEMU debug log goes, relative to the project root.
pub const QEMU_DEBUG_LOG_REL: &str = "build/qemu-debug.log";

/// Options that influence the argv but are not part of `princess.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanOptions {
    /// Override `[run] timeout_ms`.
    pub timeout_ms: Option<u64>,
    /// Attach a GDB stub (`-s`).  `None` = no stub, and `run.started.gdbStub`
    /// is then honestly `null`.
    pub gdb_stub: Option<GdbStub>,
    /// Where the `-d` log is written.  Defaults to
    /// `<root>/build/qemu-debug.log`.
    pub debug_log: Option<PathBuf>,
    /// QEMU's `-L` data directory (BIOS/option ROMs), when it was resolved.
    pub qemu_data_dir: Option<PathBuf>,
    /// Absolute path of the emulator binary.
    pub qemu_bin: PathBuf,
}

/// The manifest args, split into what the engine keeps and what it overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionedArgs {
    pub kept: Vec<String>,
    /// The engine-owned tokens that were dropped, so the caller can *tell the
    /// user* instead of silently rewriting their configuration.
    pub dropped: Vec<String>,
}

impl PartitionedArgs {
    /// A one-line description for an `ide` note, or `None` when nothing was
    /// dropped.
    pub fn dropped_note(&self) -> Option<String> {
        if self.dropped.is_empty() {
            return None;
        }
        Some(format!(
            "[run] args repeats emulator flags the engine owns and enforces (D9); ignoring: {}",
            self.dropped.join(" ")
        ))
    }
}

/// Split manifest args.  A flag is dropped together with its value, but only
/// when the next token really is a value (does not start with `-`).
pub fn partition_args(args: &[String]) -> PartitionedArgs {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if ENGINE_OWNED_FLAGS.contains(&token.as_str()) {
            dropped.push(token.clone());
            if index + 1 < args.len() && !args[index + 1].starts_with('-') {
                dropped.push(args[index + 1].clone());
                index += 1;
            }
        } else {
            kept.push(token.clone());
        }
        index += 1;
    }
    PartitionedArgs { kept, dropped }
}

/// Inputs [`plan_run`] needs from the resolved project.
///
/// Passing a small struct rather than `ResolvedProject` keeps this function
/// usable from unit tests without building a whole project on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanInputs {
    /// Absolute project root.
    pub root: PathBuf,
    /// Absolute build working directory (QEMU's cwd).
    pub cwd: PathBuf,
    /// `[run] args`, verbatim.
    pub args: Vec<String>,
    /// `[run] boot`, informational for the front end.
    pub boot: BootProtocol,
    /// `[run] timeout_ms`.
    pub timeout_ms: u64,
    /// `[run] serial`.
    pub serial_device: String,
    /// `[run] serial.tee_to_file`, absolute.
    pub serial_tee: Option<PathBuf>,
}

/// Reject a boot medium that cannot work, *before* spawning QEMU.
///
/// `-kernel` only accepts a 32-bit ELF (D2), so a `Kernel` medium pointing at an
/// ELF64 image is a plan-time error with the real reason, not a QEMU error the
/// user has to decode.
fn check_medium(medium: &BootMedium) -> Result<()> {
    match medium {
        BootMedium::Iso(path) | BootMedium::Disk(path) => {
            if path.as_os_str().is_empty() {
                return Err(PrincessError::invalid_config(
                    "boot medium path is empty: refusing to start an emulator with nothing to boot",
                ));
            }
            Ok(())
        }
        BootMedium::Kernel(path) => {
            if path.as_os_str().is_empty() {
                return Err(PrincessError::invalid_config(
                    "kernel path is empty: refusing to start an emulator with nothing to boot",
                ));
            }
            let class = classify_elf(path)?;
            if class == ElfClass::Elf64 {
                return Err(PrincessError::new(
                    ErrorCode::InvalidConfig,
                    format!(
                        "{} is ELF64: QEMU's -kernel multiboot loader only accepts a 32-bit image (D2)",
                        path.display()
                    ),
                )
                .with_detail(
                    "Boot an x86_64 kernel through a GRUB ISO instead: `-cdrom <iso> -boot d`"
                        .to_string(),
                ));
            }
            Ok(())
        }
    }
}

/// ELF class, read from the e_ident byte without a binary-parsing dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfClass {
    Elf32,
    Elf64,
    Other,
}

/// Read `EI_CLASS` from a file.  A missing/unreadable file is **not** an error
/// here (the caller reports it when the medium is really needed); a file that is
/// not an ELF at all is `Other`, and the caller decides.
pub fn classify_elf(path: &Path) -> Result<ElfClass> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|err| {
        PrincessError::not_found(format!("cannot open {}: {err}", path.display()))
    })?;
    let mut ident = [0u8; 5];
    let read = file.read(&mut ident).unwrap_or(0);
    if read < 5 || &ident[0..4] != b"\x7fELF" {
        return Ok(ElfClass::Other);
    }
    Ok(match ident[4] {
        1 => ElfClass::Elf32,
        2 => ElfClass::Elf64,
        _ => ElfClass::Other,
    })
}

/// Build the exact QEMU argv and the [`RunPlan`] the engine will execute.
///
/// Ordering is deliberate and pinned by tests:
/// `qemu [-L data] <kept manifest args> <boot medium> <engine-owned capture
/// flags> -d <items> -D <log> [-s]`.
pub fn plan_run(inputs: &PlanInputs, medium: &BootMedium, options: &PlanOptions) -> Result<RunPlan> {
    if options.qemu_bin.as_os_str().is_empty() {
        return Err(PrincessError::new(
            ErrorCode::ToolchainMissing,
            "no qemu-system-x86_64 binary was resolved; run `bash scripts/bootstrap-toolchain.sh`",
        ));
    }
    check_medium(medium)?;

    let mut argv = vec![options.qemu_bin.to_string_lossy().into_owned()];
    if let Some(data) = &options.qemu_data_dir {
        argv.push("-L".to_string());
        argv.push(data.to_string_lossy().into_owned());
    }

    let PartitionedArgs { kept, .. } = partition_args(&inputs.args);
    argv.extend(kept);

    match medium {
        BootMedium::Iso(path) => {
            argv.push("-cdrom".to_string());
            argv.push(path.to_string_lossy().into_owned());
            argv.push("-boot".to_string());
            argv.push("d".to_string());
        }
        BootMedium::Kernel(path) => {
            argv.push("-kernel".to_string());
            argv.push(path.to_string_lossy().into_owned());
        }
        BootMedium::Disk(path) => {
            argv.push("-drive".to_string());
            argv.push(format!("file={},format=raw,if=floppy", path.display()));
        }
    }

    // ---- D9: the capture triplet, always exactly this shape -----------------
    argv.push("-display".to_string());
    argv.push("none".to_string());
    argv.push("-monitor".to_string());
    argv.push("none".to_string());
    argv.push("-serial".to_string());
    argv.push("stdio".to_string());
    argv.push("-no-reboot".to_string());

    // ---- D9: exception + reset logging (both are mandatory) -----------------
    argv.push("-d".to_string());
    argv.push(QEMU_DEBUG_ITEMS.to_string());
    argv.push("-D".to_string());
    argv.push(debug_log_path(inputs, options).to_string_lossy().into_owned());

    let gdb_stub = options.gdb_stub.clone();
    if gdb_stub.is_some() {
        argv.push("-s".to_string());
    }

    // The invariants the contract freezes, asserted rather than assumed.
    debug_assert!(!argv.iter().any(|a| a == "-nographic"));
    let serial_positions: Vec<usize> = argv
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "-serial")
        .map(|(index, _)| index)
        .collect();
    debug_assert_eq!(serial_positions.len(), 1, "-serial must be emitted exactly once");
    let monitor_count = argv.iter().filter(|token| token.as_str() == "-monitor").count();
    debug_assert_eq!(monitor_count, 1, "-monitor must be emitted exactly once");
    let stdio_serial = serial_positions
        .first()
        .map(|index| argv.get(index + 1).map(String::as_str) == Some("stdio"))
        .unwrap_or(false);
    let stdio_monitor = argv
        .iter()
        .position(|token| token == "-monitor")
        .and_then(|index| argv.get(index + 1).map(String::as_str))
        == Some("stdio");
    debug_assert!(
        !(stdio_serial && stdio_monitor),
        "-serial stdio and -monitor stdio must never be combined (D9)"
    );

    Ok(RunPlan {
        backend: "qemu".to_string(),
        argv,
        cwd: inputs.cwd.clone(),
        boot_medium: medium.clone(),
        boot: inputs.boot,
        timeout_ms: options.timeout_ms.unwrap_or(inputs.timeout_ms),
        serial: SerialCapture {
            device: inputs.serial_device.clone(),
            tee_to_file: inputs
                .serial_tee
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        },
        gdb_stub,
    })
}

/// `<debug_log override>`, else `<root>/build/qemu-debug.log`.
pub fn debug_log_path(inputs: &PlanInputs, options: &PlanOptions) -> PathBuf {
    options
        .debug_log
        .clone()
        .unwrap_or_else(|| inputs.root.join(QEMU_DEBUG_LOG_REL))
}

/// Enforce the D9 flag contract on an already-built argv (used as a guard by the
/// backend and by the E2E tests on the *emitted* `run.started.qemuArgv`).
pub fn check_argv_contract(argv: &[String]) -> Result<()> {
    let has = |flag: &str| argv.iter().any(|token| token == flag);
    if has("-nographic") {
        return Err(PrincessError::internal(
            "QEMU argv contains -nographic, which D9 forbids: it multiplexes the monitor onto the serial stream",
        ));
    }
    let value_after = |flag: &str| -> Option<&str> {
        argv.iter()
            .position(|token| token == flag)
            .and_then(|index| argv.get(index + 1))
            .map(String::as_str)
    };
    if value_after("-serial") != Some("stdio") {
        return Err(PrincessError::internal(format!(
            "QEMU argv must carry `-serial stdio` (D9); got {:?}",
            value_after("-serial")
        )));
    }
    if value_after("-monitor") != Some("none") {
        return Err(PrincessError::internal(format!(
            "QEMU argv must carry `-monitor none` (D9); got {:?}",
            value_after("-monitor")
        )));
    }
    if value_after("-display") != Some("none") {
        return Err(PrincessError::internal(format!(
            "QEMU argv must carry `-display none` (D9); got {:?}",
            value_after("-display")
        )));
    }
    if value_after("-d") != Some(QEMU_DEBUG_ITEMS) {
        return Err(PrincessError::internal(format!(
            "QEMU argv must carry `-d {QEMU_DEBUG_ITEMS}` so a triple fault is observable (D9); got {:?}",
            value_after("-d")
        )));
    }
    if !has("-D") {
        return Err(PrincessError::internal(
            "QEMU argv has -d but no -D: the debug output would go to stderr and be lost",
        ));
    }
    Ok(())
}

/// The default stub the engine offers when `[debug] stub.mode = "launch"`.
pub fn default_stub(host: &str, port: u16, mode: StubMode) -> GdbStub {
    GdbStub {
        host: host.to_string(),
        port,
        mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> PlanInputs {
        PlanInputs {
            root: PathBuf::from("/proj"),
            cwd: PathBuf::from("/proj"),
            args: Vec::new(),
            boot: BootProtocol::Multiboot2,
            timeout_ms: 15_000,
            serial_device: "com1".to_string(),
            serial_tee: Some(PathBuf::from("/proj/build/serial.log")),
        }
    }

    fn options() -> PlanOptions {
        PlanOptions {
            qemu_bin: PathBuf::from("/usr/bin/qemu-system-x86_64"),
            ..PlanOptions::default()
        }
    }

    #[test]
    fn iso_boot_uses_cdrom_and_boot_d_never_kernel() {
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &options(),
        )
        .unwrap();
        let argv = plan.argv.join(" ");
        assert!(argv.contains("-cdrom /proj/build/k.iso"), "{argv}");
        assert!(argv.contains("-boot d"), "{argv}");
        assert!(!argv.contains("-kernel"), "ISO boot must not also pass -kernel: {argv}");
    }

    #[test]
    fn the_d9_capture_triplet_is_exact_and_nographic_is_absent() {
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &options(),
        )
        .unwrap();
        let argv = plan.argv.join(" ");
        assert!(argv.contains("-display none"), "{argv}");
        assert!(argv.contains("-serial stdio"), "{argv}");
        assert!(argv.contains("-monitor none"), "{argv}");
        assert!(!argv.contains("-nographic"), "D9 forbids -nographic: {argv}");
        // D9 also forbids mixing stdio serial with stdio monitor.
        assert!(!argv.contains("-monitor stdio"), "{argv}");
        check_argv_contract(&plan.argv).unwrap();
    }

    #[test]
    fn both_debug_items_are_always_requested() {
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &options(),
        )
        .unwrap();
        let argv = plan.argv.join(" ");
        assert!(argv.contains("-d int,cpu_reset,guest_errors"), "{argv}");
        assert!(argv.contains("-D /proj/build/qemu-debug.log"), "{argv}");
    }

    /// The whole point of D9's `-d` rule: a plan without `cpu_reset` would make
    /// a triple fault invisible.  Guard the contract *function*, too.
    #[test]
    fn check_argv_contract_rejects_a_plan_that_could_not_see_a_triple_fault() {
        let mut argv = vec![
            "qemu-system-x86_64".to_string(),
            "-display".to_string(),
            "none".to_string(),
            "-serial".to_string(),
            "stdio".to_string(),
            "-monitor".to_string(),
            "none".to_string(),
            "-d".to_string(),
            "int".to_string(),
            "-D".to_string(),
            "/tmp/q.log".to_string(),
        ];
        let err = check_argv_contract(&argv).unwrap_err();
        assert!(err.message.contains("cpu_reset"), "{err}");

        argv[8] = "int,cpu_reset,guest_errors".to_string();
        check_argv_contract(&argv).unwrap();

        // -nographic is refused outright.
        argv.push("-nographic".to_string());
        let err = check_argv_contract(&argv).unwrap_err();
        assert!(err.message.contains("-nographic"), "{err}");

        // stdio monitor is refused.
        let mut bad = vec![
            "qemu-system-x86_64".to_string(),
            "-display".to_string(),
            "none".to_string(),
            "-serial".to_string(),
            "stdio".to_string(),
            "-monitor".to_string(),
            "stdio".to_string(),
            "-d".to_string(),
            "int,cpu_reset,guest_errors".to_string(),
            "-D".to_string(),
            "/tmp/q.log".to_string(),
        ];
        let err = check_argv_contract(&bad).unwrap_err();
        assert!(err.message.contains("-monitor none"), "{err}");
        // sanity: our canonical argv passes
        bad[6] = "none".to_string();
        check_argv_contract(&bad).unwrap();
    }

    #[test]
    fn engine_owned_manifest_args_are_dropped_with_their_values() {
        let mut i = inputs();
        i.args = ["-m", "512M", "-serial", "stdio", "-display", "none", "-no-reboot", "-smp", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let PartitionedArgs { kept, dropped } = partition_args(&i.args);
        assert_eq!(kept, vec!["-m", "512M", "-smp", "2"]);
        assert_eq!(
            dropped,
            vec!["-serial", "stdio", "-display", "none", "-no-reboot"]
        );

        let plan = plan_run(&i, &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")), &options())
            .unwrap();
        let serial_count = plan.argv.iter().filter(|t| t.as_str() == "-serial").count();
        assert_eq!(serial_count, 1, "exactly one -serial after partitioning: {:?}", plan.argv);
        assert_eq!(partition_args(&["-nographic".to_string()]).dropped, vec!["-nographic"]);
    }

    #[test]
    fn the_reference_fixture_manifest_args_are_partitioned_the_same_way() {
        // fixtures/paging-kernel/princess.toml [run].args, verbatim.
        let args: Vec<String> = [
            "-m", "256M", "-cdrom", "build/pagingkernel.iso", "-boot", "d", "-display", "none",
            "-serial", "stdio", "-monitor", "none", "-no-reboot",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let PartitionedArgs { kept, dropped } = partition_args(&args);
        assert_eq!(kept, vec!["-m", "256M"]);
        assert_eq!(
            dropped,
            vec![
                "-cdrom",
                "build/pagingkernel.iso",
                "-boot",
                "d",
                "-display",
                "none",
                "-serial",
                "stdio",
                "-monitor",
                "none",
                "-no-reboot"
            ]
        );
    }

    #[test]
    fn a_gdb_stub_adds_s_and_is_reported() {
        let mut o = options();
        o.gdb_stub = Some(default_stub("127.0.0.1", 1234, StubMode::Launch));
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &o,
        )
        .unwrap();
        assert!(plan.argv.iter().any(|token| token == "-s"));
        assert_eq!(plan.gdb_stub.unwrap().endpoint(), "127.0.0.1:1234");

        // No stub requested -> honestly null, never a fabricated endpoint.
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &options(),
        )
        .unwrap();
        assert!(plan.gdb_stub.is_none());
    }

    #[test]
    fn timeout_override_and_serial_capture_flow_into_the_plan() {
        let mut o = options();
        o.timeout_ms = Some(1234);
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &o,
        )
        .unwrap();
        assert_eq!(plan.timeout_ms, 1234);
        assert_eq!(plan.serial.device, "com1");
        assert_eq!(plan.serial.tee_to_file.as_deref(), Some("/proj/build/serial.log"));
        assert_eq!(plan.cwd, PathBuf::from("/proj"));

        // Falling back to the manifest value when there is no override.
        let plan = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &options(),
        )
        .unwrap();
        assert_eq!(plan.timeout_ms, 15_000);
    }

    #[test]
    fn an_empty_qemu_binary_is_a_loud_toolchain_error() {
        let mut o = options();
        o.qemu_bin = PathBuf::new();
        let err = plan_run(
            &inputs(),
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &o,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
        assert!(err.message.contains("bootstrap-toolchain"), "{err}");
    }

    #[test]
    fn an_empty_boot_medium_is_refused_before_spawning() {
        let err = plan_run(&inputs(), &BootMedium::Iso(PathBuf::new()), &options()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        let err = plan_run(&inputs(), &BootMedium::Disk(PathBuf::new()), &options()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
    }

    #[test]
    fn elf_class_is_read_from_the_identification_bytes() {
        let dir = std::env::temp_dir().join(format!("princesside-elfclass-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let elf64 = dir.join("a.elf");
        let mut bytes = b"\x7fELF".to_vec();
        bytes.push(2);
        std::fs::write(&elf64, &bytes).unwrap();
        assert_eq!(classify_elf(&elf64).unwrap(), ElfClass::Elf64);

        let elf32 = dir.join("b.elf");
        let mut bytes = b"\x7fELF".to_vec();
        bytes.push(1);
        std::fs::write(&elf32, &bytes).unwrap();
        assert_eq!(classify_elf(&elf32).unwrap(), ElfClass::Elf32);

        let text = dir.join("c.txt");
        std::fs::write(&text, b"not an elf at all").unwrap();
        assert_eq!(classify_elf(&text).unwrap(), ElfClass::Other);

        let missing = dir.join("nope.elf");
        assert_eq!(classify_elf(&missing).unwrap_err().code, ErrorCode::NotFound);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// D2's measured trap: QEMU refuses ELF64 on `-kernel`.  The engine must
    /// catch that at plan time with the real explanation.
    #[test]
    fn an_elf64_kernel_medium_is_refused_with_the_d2_explanation() {
        let dir = std::env::temp_dir().join(format!("princesside-d2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let elf64 = dir.join("kernel.elf");
        let mut bytes = b"\x7fELF".to_vec();
        bytes.push(2);
        std::fs::write(&elf64, &bytes).unwrap();

        let err = plan_run(&inputs(), &BootMedium::Kernel(elf64.clone()), &options()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("ELF64"), "{err}");
        assert!(
            err.detail.as_deref().unwrap_or("").contains("-cdrom"),
            "the error must point at the GRUB ISO path: {err:?}"
        );

        // A 32-bit image on the same path is accepted (that is what -kernel eats).
        let elf32 = dir.join("kernel32.elf");
        let mut bytes = b"\x7fELF".to_vec();
        bytes.push(1);
        std::fs::write(&elf32, &bytes).unwrap();
        let plan = plan_run(&inputs(), &BootMedium::Kernel(elf32), &options()).unwrap();
        assert!(plan.argv.join(" ").contains("-kernel"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn debug_log_defaults_to_the_project_build_directory() {
        let inputs = inputs();
        assert_eq!(
            debug_log_path(&inputs, &options()),
            PathBuf::from("/proj/build/qemu-debug.log")
        );
        let mut o = options();
        o.debug_log = Some(PathBuf::from("/tmp/elsewhere.log"));
        assert_eq!(debug_log_path(&inputs, &o), PathBuf::from("/tmp/elsewhere.log"));
    }
}
