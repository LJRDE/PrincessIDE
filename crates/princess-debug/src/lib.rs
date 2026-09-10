//! # princess-debug
//!
//! The engine's **graphical debug backend for bare-metal kernels** — decision
//! **D11**: *GDB's built-in DAP is the trunk; the Rust side adds only a thin
//! capability layer, and never a MI→DAP translation layer.*
//!
//! ## The shape of the thing
//!
//! ```text
//!   princess-core::DebugBackend
//!            ▲
//!            │  backend.rs      trait impl, session state machine
//!   ┌────────┴─────────┐
//!   │   DapBackend     │
//!   └────────┬─────────┘
//!            │
//!   ┌────────┴─────────┐   capability.rs   the four gaps the built-in DAP has
//!   │ CapabilityLayer  │ ─────────────────────────────────────────────────
//!   └────────┬─────────┘   repl.rs         escape hatch + error normalisation
//!            │
//!   ┌────────┴─────────┐   transport.rs    framing + request_seq correlation
//!   │    Transport     │   framing.rs      Content-Length codec
//!   └────────┬─────────┘
//!            │
//!   ┌────────┴─────────┐   qemu.rs         process-group lifecycle, no orphans
//!   │  QemuProcess     │ ─────────────────────────────────────────────────
//!   └──────────────────┘   arch.rs         D10 architecture self-check
//! ```
//!
//! Everything above `capability.rs` is standard DAP and belongs to GDB.
//! Everything in `capability.rs`, `repl.rs` and `arch.rs` is the part
//! PrincessIDE actually owns, and it is deliberately small.
//!
//! ## What the built-in DAP cannot do, and what fills each gap
//!
//! Measured against `gdb 16.3` on this workspace's fixtures; the evidence is in
//! `docs/reports/p4-debug.md`.
//!
//! | gap | why it matters for a kernel | module |
//! |---|---|---|
//! | **no hardware breakpoints** | GRUB loads the kernel before GDB attaches, and software breakpoints work by writing `0xCC` into target memory — impossible until that memory is mapped | [`capability`] `hbreak` |
//! | **`readMemory` is virtual-only** | page tables, the GDT/IDT and anything pre-paging is only reachable physically | [`capability`] `monitor xp` |
//! | **the `Registers` scope is unreliable and incomplete** | no `CR0`–`CR4`, no `EFER`, and in some sessions no scope at all | [`capability`] `info registers` |
//! | **no escape hatch** | some capabilities exist only as GDB CLI commands | [`repl`] |
//!
//! ## Two behaviours that are load-bearing, not incidental
//!
//! 1. **`evaluate(context="repl")` returns `success: true` for a failed
//!    command.** GDB puts the error in the message text. Trusting `success`
//!    would report a *pending* hardware breakpoint for a symbol that does not
//!    exist — plausible-looking fake data, which P4-4 forbids. See [`repl`].
//! 2. **`attach`'s response arrives after `configurationDone`'s.** A lockstep
//!    client mis-attributes every reply by one and then reports real features as
//!    unsupported. See [`transport`].
//!
//! Both were measured here, not read about, and both are covered by tests.
//!
//! ## Public API
//!
//! The trait impl is [`backend::DapBackend`]; everything else is a capability or
//! a helper the acceptance harness drives directly:
//!
//! ```no_run
//! use princess_debug::{DebugSessionConfig, DapBackend, debug_boot_argv};
//! use princess_core::{DebugBackend, GdbStub, StubMode};
//!
//! # fn main() -> princess_core::Result<()> {
//! let argv = debug_boot_argv(
//!     "qemu-system-x86_64", None,
//!     std::path::Path::new("build/pagingkernel.iso"),
//!     "256M", std::path::Path::new("/tmp/serial.log"), 1234, true,
//! );
//! let mut backend = DapBackend::new(DebugSessionConfig::launching(
//!     "/path/to/princess-gdb", "build/pagingkernel.elf", argv, ".",
//! ));
//! let mut sink = princess_core::EventRecorder::new(std::io::sink());
//! backend.attach(&GdbStub { host: "127.0.0.1".into(), port: 1234, mode: StubMode::Attach }, &mut sink)?;
//! backend.prepare_target()?;                        // symbols + D10 self-check
//! let hw = backend.set_hardware_breakpoint("paging_fault_probe", &mut sink)?;
//! assert!(hw.is_hardware());
//! let stop = backend.continue_(None, &mut sink)?;
//! # Ok(()) }
//! ```

pub mod arch;
pub mod backend;
pub mod capability;
pub mod framing;
pub mod qemu;
pub mod repl;
pub mod transport;

pub use arch::{ArchCheck, ElfClass, ElfFacts, TargetArch};
pub use backend::{
    base64_decode, base64_encode, parse_stack_frames, DapBackend, DebugSessionConfig,
    HardwareSupport, SessionState, TargetMode, REGISTERS_SCOPE_REFERENCE,
};
pub use capability::{
    BreakpointRecord, CapabilityLayer, PhysicalMemoryBackend, RegisterEntry, RegisterSelection,
};
pub use framing::{
    decode_frame, encode_frame, read_frame, FrameError, FrameReader, HEADER_LENGTH,
    MAX_CONTENT_LENGTH,
};
pub use qemu::{debug_boot_argv, shell_quote, QemuCommand, QemuProcess};
pub use repl::{ReplOutcome, interpret_response};
pub use transport::{AdapterCommand, Transport, DEFAULT_REQUEST_TIMEOUT};

/// Version of the DAP dialect this crate speaks, for the UI's capability banner.
pub const ADAPTER_ID: &str = "gdb";

/// The minimum GDB major version that has the built-in DAP interpreter.
///
/// Decision D11 records the measurement: the workspace's original gdb 13.1
/// answers `Interpreter 'dap' unrecognized`.  Asserting this in one place means
/// `doctor.sh` and the backend agree on the number.
pub const MIN_GDB_MAJOR: u32 = 14;

/// Parse `GNU gdb (Debian 16.3-1) 16.3` into `Some(16)`.
///
/// Used to fail the session with a clear message when the configured adapter is
/// a gdb too old to have DAP, rather than letting the spawn produce
/// `Interpreter 'dap' unrecognized` deep inside a handshake.
pub fn parse_gdb_major(version_output: &str) -> Option<u32> {
    let line = version_output.lines().next()?;
    // The version is the last whitespace-delimited token on the first line.
    let token = line.split_whitespace().last()?;
    let major: String = token.chars().take_while(char::is_ascii_digit).collect();
    if major.is_empty() {
        return None;
    }
    major.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_princess_gdb_version_line() {
        let output = "GNU gdb (Debian 16.3-1) 16.3\n\
                      Copyright (C) 2024 Free Software Foundation, Inc.\n";
        assert_eq!(parse_gdb_major(output), Some(16));
    }

    #[test]
    fn parses_the_too_old_gdb_13_line_that_has_no_dap() {
        let output = "GNU gdb (Debian 13.1-3) 13.1\n";
        assert_eq!(parse_gdb_major(output), Some(13));
        assert!(parse_gdb_major(output).unwrap() < MIN_GDB_MAJOR);
    }

    #[test]
    fn a_double_digit_major_version_is_not_truncated() {
        assert_eq!(parse_gdb_major("GNU gdb (Debian 21.1) 21.1"), Some(21));
    }

    #[test]
    fn unparseable_version_output_is_none_not_a_guess() {
        assert_eq!(parse_gdb_major(""), None);
        assert_eq!(parse_gdb_major("not gdb at all"), None);
    }
}
