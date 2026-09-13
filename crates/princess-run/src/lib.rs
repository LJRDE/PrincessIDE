//! # princess-run
//!
//! The PrincessIDE **run engine** (P2-B2): everything between "the user pressed
//! Run" and "the front end knows why the machine stopped".
//!
//! | module | responsibility | spec |
//! |---|---|---|
//! | [`qemu`] | build the exact QEMU argv: GRUB ISO (`-cdrom -boot d`, **D2**), the serial/monitor/display triplet and `-d int,cpu_reset` (**D9**) | §4, D2, D9 |
//! | [`serial`] | turn guest COM1 text into `log.append(serial.com1)` and `run.fault` (with `ripText` preserved verbatim) | §2, D3 |
//! | [`exit`] | assemble exit evidence and attribute `run.exited.reason` through [`princess_core::classify_exit`] | §6 rule 4, D9 |
//! | [`process`] | spawn QEMU in its **own process group**, stream both pipes, enforce the deadline, kill the *group*, prove no orphan survived | §6 |
//! | [`backend`] | the [`RunBackend`](princess_core::RunBackend) implementation that sequences all of the above | §5 |
//!
//! ## The three decisions this crate exists to enforce
//!
//! 1. **D9 — capture.** `-display none -serial stdio -monitor none`, never
//!    `-nographic`, never `-serial stdio` with `-monitor stdio`.  `-d int`
//!    **and** `-d cpu_reset` are both mandatory, because `Triple fault` exists
//!    only in the latter.  Measured in this workspace: with `-d int` alone the
//!    count is 0; with both it is 1, and the process still exits **0** — so
//!    `-no-reboot`'s exit code is never panic evidence.
//! 2. **D2 — boot path.** x86_64 goes through a GRUB ISO.  `-kernel` only
//!    accepts a 32-bit image, and [`qemu::plan_run`] refuses an ELF64 medium at
//!    plan time with that explanation instead of letting QEMU fail cryptically.
//! 3. **Process groups.** Every QEMU is signalled as a *group* and the group is
//!    verified empty before the run is reported.  Orphans would directly
//!    falsify the P2-7 assertion.
//!
//! ## The one thing it deliberately does not do
//!
//! It does not parse DWARF.  `run.fault.symbolicated` is filled through a
//! caller-supplied lookup ([`backend::SymbolizeFn`]), because symbolication is
//! `princess-symbol`'s job (P2-B3) and duplicating it here would create a second
//! source of truth.

pub mod backend;
pub mod exit;
pub mod jvm;
pub mod process;
pub mod qemu;
pub mod serial;

pub use backend::{
    debug_log_from_argv, last_fault_rip, plan_with_stub, serial_reached_kernel, summarize,
    QemuBackend, ResolvedQemuBackend, RunOptions, RunSummary, SymbolizeFn, MAX_FAULT_EVENTS,
};
pub use exit::{detect_triple_fault, triple_fault_in_file, QemuEvidence};
pub use jvm::JvmBackend;
pub use process::{
    group_alive, kill_group, kill_group_blocking, which, FakeRunner, Pipe, ProcessOutcome,
    ProcessRunner, RunnerConfig, SpawnSpec, StreamChunk, SystemRunner,
};
pub use qemu::{
    check_argv_contract, classify_elf, debug_log_path, default_stub, partition_args, plan_run,
    ElfClass, PlanOptions, PlanInputs, QEMU_DEBUG_ITEMS, QEMU_DEBUG_LOG_REL,
};
pub use serial::{
    banner_in_log, fault_rip_in_log, parse_hex_u64, vector_name, PageFaultGate, RawFault,
    SerialObservation, SerialObserver, BANNER_PREFIX, PAGING_BANNER, PAGING_ENABLED_MARKER,
    REFKERNEL_BANNER,
};

/// The event ordering one run produces, pinned in one place so the CLI, the
/// Tauri shell and the tests cannot disagree about it.
pub const RUN_EVENT_ORDER: [&str; 5] = [
    "run.started",
    "log.append",
    "run.fault",
    "log.append(qemu.monitor)",
    "run.exited",
];

/// The banner the reference fixture prints (`20-acceptance.md`, frozen).
pub const ACCEPTANCE_REFKERNEL_BANNER: &str = REFKERNEL_BANNER;
/// The banner the paging fixture prints (`20-acceptance.md`, frozen).
pub const ACCEPTANCE_PAGING_BANNER: &str = PAGING_BANNER;
