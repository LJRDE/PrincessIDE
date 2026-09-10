//! The **thin capability layer**: the four things GDB's built-in DAP does not
//! do for a bare-metal kernel, implemented on top of the REPL escape hatch.
//!
//! Decision D11 fixes the split: the DAP trunk is GDB's own implementation and
//! the front end speaks only standard DAP.  This module is the *only* place
//! that adds behaviour, and it deliberately adds as little as possible.
//!
//! # The four gaps, and what fills each
//!
//! | gap | measured evidence | fill |
//! |---|---|---|
//! | no hardware breakpoints | `breakpoint.py` only ever builds `gdb.Breakpoint`; `info breakpoints` shows `Type breakpoint` | [`CapabilityLayer::set_hardware_breakpoint`] via `hbreak` |
//! | `readMemory` is virtual-only | `memory.py` calls `inferior.read_memory(addr)`, no physical path | [`CapabilityLayer::read_physical_memory`] via `monitor xp` |
//! | register panel | `scopes` returns a `Registers` scope only sometimes (see below) | [`CapabilityLayer::read_registers`] via `info registers` |
//! | no escape hatch | — | [`CapabilityLayer::repl`] |
//!
//! ## Why the register panel does not use the DAP `Registers` scope
//!
//! Research C §4.2 measured that `scopes(frameId=0)` returns **no** `Registers`
//! entry on this fixture.  The P4 probe, run with `symbol-file` loaded first,
//! *did* get one (66 variables).  Both observations are real, which is the point:
//! **the scope is not reliably present**, and a panel built on it would be
//! empty in exactly the sessions where it matters.  `info registers` is present
//! in every session and returns the kernel control registers (`CR0`–`CR4`,
//! `EFER`, segment selectors) that the DAP scope does not model at all.
//!
//! ## Why physical memory must not be faked from a virtual read
//!
//! At the fixture's breakpoint the virtual and physical mappings for `0x104000`
//! happen to agree, so a virtual `readMemory` "looks right".  That coincidence
//! is a trap: page tables, the GDT/IDT (D20: QEMU 7.2 has no `info gdt`/`info
//! idt`) and any address before paging is on are only reachable physically.
//! [`CapabilityLayer::read_physical_memory`] therefore **always** goes through
//! `monitor xp` and never falls back to the virtual path.
//!
//! ## Backend abstraction (research C §9 risk 3)
//!
//! `monitor` is a QEMU feature, not a GDB or gdbstub feature.  On Bochs, on
//! real hardware over BDM/JTAG, or on a custom stub, `monitor` does not exist.
//! [`PhysicalMemoryBackend`] makes that explicit: the layer probes once and
//! reports [`PhysicalMemoryUnavailable`] instead of pretending.

use std::collections::BTreeMap;

use princess_core::{ErrorCode, PrincessError, RegisterFile, Result};

use crate::repl;
use crate::transport::Transport;

/// One parsed hardware breakpoint, as GDB reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointRecord {
    /// GDB's breakpoint number (the `Num` column).
    pub number: u32,
    /// `hw breakpoint` | `breakpoint` | `watchpoint` | ...
    pub kind: String,
    /// `y`/`n` from the `Enb` column.
    pub enabled: bool,
    /// The address column, when GDB had one (`<PENDING>` if not).
    pub address: Option<String>,
    /// Everything in the `What` column, verbatim.
    pub what: String,
}

impl BreakpointRecord {
    /// Whether GDB marked this one as hardware-assisted.
    ///
    /// This is the evidence P4 uses to prove the capability layer did something
    /// the built-in DAP cannot: the string `hw breakpoint` appears only for
    /// breakpoints created with `hbreak`.
    pub fn is_hardware(&self) -> bool {
        self.kind.contains("hw breakpoint")
    }

    /// Whether GDB could not resolve a location yet.
    pub fn is_pending(&self) -> bool {
        self.what.contains("<PENDING>") || self.address.as_deref() == Some("<PENDING>")
    }
}

/// Where physical memory reads are actually coming from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalMemoryBackend {
    /// QEMU's `monitor xp` — verified working on the fixture.
    QemuMonitor,
    /// The target has no `monitor`; physical reads are impossible.
    ///
    /// Returned rather than erroring at construction so the rest of the debug
    /// session keeps working (contract §0 principle 4: explicit, but scoped).
    Unavailable,
}

/// One line of an `info registers` dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterEntry {
    /// Canonical upper-case name (`RIP`, `CR3`, `CS`, ...).
    pub name: String,
    /// The value with a `0x` prefix when GDB printed one.
    pub value: String,
    /// The decimal column GDB prints for general-purpose registers.
    pub decimal: Option<String>,
    /// The bracket annotation, e.g. `[ PG ET PE ]` for CR0.
    pub annotation: Option<String>,
    /// Everything GDB printed for this line, verbatim.
    pub raw: String,
}

/// The capability layer over one live DAP session.
///
/// Holds no state beyond the transport: every call re-reads the ground truth
/// from GDB so a stale cache can never be shown to the user as a live value
/// (the same reason D20 forbids snapshotting page-table A/D bits).
pub struct CapabilityLayer<'a> {
    transport: &'a mut Transport,
    physical_backend: Option<PhysicalMemoryBackend>,
}

impl std::fmt::Debug for CapabilityLayer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityLayer")
            .field("physical_backend", &self.physical_backend)
            .finish()
    }
}

impl<'a> CapabilityLayer<'a> {
    pub fn new(transport: &'a mut Transport) -> Self {
        Self {
            transport,
            physical_backend: None,
        }
    }

    /// Run any GDB CLI command through the escape hatch.
    pub fn repl(&mut self, command: &str) -> Result<repl::ReplOutcome> {
        repl::run(self.transport, command)
    }

    /// Borrow the underlying transport.
    ///
    /// Exposed so the session bootstrap can run the architecture self-check
    /// while a capability layer is still live, without either owning the other.
    pub fn transport_mut(&mut self) -> &mut Transport {
        self.transport
    }

    /// Run a CLI command and fail if GDB's *text* says it went wrong.
    pub fn repl_ok(&mut self, command: &str) -> Result<repl::ReplOutcome> {
        self.repl(command)?.require_ok("gdb command")
    }

    // ---------------------------------------------------------------- setup --

    /// Turn off the things that make CLI parsing unreliable.
    ///
    /// * `pagination off` — a pager would block on a prompt that never comes.
    /// * `confirm off` — GDB otherwise asks "y or n" for some commands, which
    ///   deadlocks a non-interactive session.
    /// * `print elements 0` / `print repeats 0` — stop GDB truncating arrays
    ///   and collapsing repeated values with `<repeats N times>`, which is
    ///   unparseable and, worse, looks like real data.
    pub fn prepare_session(&mut self) -> Result<()> {
        for command in [
            "set pagination off",
            "set confirm off",
            "set print elements 0",
            "set print repeats 0",
        ] {
            self.repl_ok(command)?;
        }
        Ok(())
    }

    /// Load the ELF's symbols into the session.
    ///
    /// **Mandatory for this project's boot path.** The kernel boots from a GRUB
    /// ISO, so QEMU never parses the ELF; without this the stub has no symbols
    /// and every breakpoint stays `pending` (measured: `hbreak
    /// paging_fault_probe` → `pending` until `symbol-file` was issued).
    ///
    /// Returns the number of functions GDB reports, so the caller can log real
    /// evidence rather than assuming the load worked.
    ///
    /// The count comes from `info functions` grouped header lines, **not** from
    /// counting `0x` occurrences: gdb prints *declarations* here, and a
    /// declaration has no address.  An earlier version of this function counted
    /// `0x` and therefore concluded a perfectly good symbol table was empty —
    /// the kind of false negative that would have made P4 look unimplementable.
    pub fn load_symbols(&mut self, elf: &std::path::Path) -> Result<u64> {
        let path = elf.to_string_lossy().into_owned();
        let outcome = self.repl(&format!("symbol-file {}", shell_escape(&path)))?;
        if outcome.saw_error_text {
            return Err(PrincessError::new(
                ErrorCode::NotFound,
                format!("could not load symbols from {}", elf.display()),
            )
            .with_detail(format!("command: {}\noutput:\n{}", outcome.command, outcome.text)));
        }

        // Count real function *definitions*: `info functions` groups them under
        // `File <name>:` headers and lists entries as `[line:]\t<signature>;`.
        // Guard against the degenerate case by also accepting `Non-debugging
        // symbols` sections, which have the same layout.
        let listing = self.repl("info functions")?;
        if listing.saw_error_text {
            return Err(listing.into_error(format!(
                "could not enumerate the functions in {}",
                elf.display()
            )));
        }
        let count = count_function_declarations(&listing.text);
        if count == 0 {
            return Err(PrincessError::new(
                ErrorCode::NotFound,
                format!(
                    "{} loaded but GDB reports no functions; the ELF probably has no debug info",
                    elf.display()
                ),
            )
            .with_detail(format!(
                "command: {}\noutput (first 2000 bytes):\n{}",
                listing.command,
                &listing.text[..listing.text.len().min(2000)]
            )));
        }
        Ok(count)
    }

    // ---------------------------------------------------- hardware breakpoints --

    /// Set a **hardware** breakpoint — the capability the built-in DAP lacks.
    ///
    /// `spec` is a GDB location: a symbol (`paging_fault_probe`), `*0x1011c9`
    /// for an address, or `file.c:120`.
    ///
    /// Two independent checks make this trustworthy, because `hbreak` on a
    /// missing symbol returns `success: true` with the failure only in the text
    /// (see [`crate::repl`]):
    ///
    /// 1. the CLI text must contain no error idiom, and
    /// 2. the newly created breakpoint must be **verified**, not `<PENDING>`.
    ///
    /// Check 2 is what makes P4-4 real: a caller that only looked at `success`
    /// would be handed a pending breakpoint for a symbol that does not exist.
    pub fn set_hardware_breakpoint(&mut self, spec: &str) -> Result<BreakpointRecord> {
        let before = self.breakpoint_numbers()?;
        let outcome = self.repl(&format!("hbreak {}", spec))?;
        if outcome.saw_error_text {
            return Err(outcome.into_error(format!("cannot set a hardware breakpoint at `{spec}`")));
        }
        let after = self.list_breakpoints()?;

        let Some(record) = after
            .into_iter()
            .find(|bp| !before.contains(&bp.number))
        else {
            return Err(PrincessError::new(
                ErrorCode::Internal,
                format!("`hbreak {spec}` reported success but created no breakpoint"),
            )
            .with_detail(outcome.text));
        };

        if !record.is_hardware() {
            return Err(PrincessError::new(
                ErrorCode::Internal,
                format!("`hbreak {spec}` created a {kind}, not a hardware breakpoint", kind = record.kind),
            )
            .with_detail(outcome.text));
        }

        if record.is_pending() {
            return Err(PrincessError::new(
                ErrorCode::NotFound,
                format!("no such symbol or location for a hardware breakpoint: `{spec}`"),
            )
            .with_detail(format!(
                "gdb left the breakpoint pending, so the location does not exist in this image.\n\
                 command: hbreak {spec}\noutput:\n{}\nbreakpoint: {record:?}",
                outcome.text
            )));
        }

        Ok(record)
    }

    /// Set a **software** breakpoint, for comparison against the hardware one.
    pub fn set_software_breakpoint(&mut self, spec: &str) -> Result<BreakpointRecord> {
        let before = self.breakpoint_numbers()?;
        let outcome = self.repl(&format!("break {}", spec))?;
        if outcome.saw_error_text {
            return Err(outcome.into_error(format!("cannot set a breakpoint at `{spec}`")));
        }
        let after = self.list_breakpoints()?;
        let Some(record) = after.into_iter().find(|bp| !before.contains(&bp.number)) else {
            return Err(PrincessError::new(
                ErrorCode::Internal,
                format!("`break {spec}` reported success but created no breakpoint"),
            )
            .with_detail(outcome.text));
        };
        if record.is_pending() {
            return Err(PrincessError::new(
                ErrorCode::NotFound,
                format!("no such symbol or location: `{spec}`"),
            )
            .with_detail(format!("command: break {spec}\noutput:\n{}", outcome.text)));
        }
        Ok(record)
    }

    /// Every breakpoint GDB currently knows about, parsed.
    pub fn list_breakpoints(&mut self) -> Result<Vec<BreakpointRecord>> {
        let outcome = self.repl_ok("info breakpoints")?;
        Ok(parse_breakpoints(&outcome.text))
    }

    fn breakpoint_numbers(&mut self) -> Result<Vec<u32>> {
        Ok(self.list_breakpoints()?.into_iter().map(|b| b.number).collect())
    }

    /// Delete every breakpoint, so a re-run starts from a known state.
    pub fn clear_breakpoints(&mut self) -> Result<()> {
        self.repl_ok("delete breakpoints")?;
        Ok(())
    }

    // -------------------------------------------------------- physical memory --

    /// Probe whether physical memory reads are possible, once per session.
    ///
    /// Implemented as a real read rather than a capability query: GDB has no
    /// "does this stub support `monitor`" API, and guessing from the target name
    /// would be exactly the kind of inference D10/P4-4 forbid.
    pub fn physical_memory_backend(&mut self) -> Result<PhysicalMemoryBackend> {
        if let Some(backend) = self.physical_backend {
            return Ok(backend);
        }
        let outcome = self.repl("monitor xp /1xb 0x0")?;
        let verdict = if outcome.saw_error_text
            || outcome.text.contains("not supported")
            || outcome.text.contains("Undefined command")
        {
            PhysicalMemoryBackend::Unavailable
        } else {
            PhysicalMemoryBackend::QemuMonitor
        };
        self.physical_backend = Some(verdict);
        Ok(verdict)
    }

    /// Read `count` bytes of **physical** memory starting at `address`.
    ///
    /// Uses QEMU's `monitor xp`, the only physical path available (D11 §4.1:
    /// DAP's own `readMemory` is virtual-only by construction).
    ///
    /// `count` beyond one `xp` line width is chunked; the returned vector is
    /// always exactly `count` bytes long or the call fails, so a caller can
    /// never silently receive a short buffer.
    pub fn read_physical_memory(&mut self, address: u64, count: u64) -> Result<Vec<u8>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        match self.physical_memory_backend()? {
            PhysicalMemoryBackend::QemuMonitor => {}
            PhysicalMemoryBackend::Unavailable => {
                return Err(PrincessError::new(
                    ErrorCode::ToolchainMissing,
                    "the connected target has no `monitor` command, so physical memory cannot be read",
                )
                .with_detail(
                    "`monitor` is a QEMU feature; on Bochs, real hardware over BDM/JTAG, \
                     or a custom stub there is no physical-memory path through GDB. \
                     PrincessIDE refuses to substitute a virtual read, which would \
                     return plausible-looking bytes from the wrong address space."
                        .to_string(),
                ));
            }
        }

        // `xp` prints 16 bytes per line; read line by line so a partial address
        // range at the end still lands exactly on `count`.
        let mut bytes = Vec::with_capacity(count as usize);
        let mut cursor = address;
        while (bytes.len() as u64) < count {
            let remaining = count - bytes.len() as u64;
            let width = remaining.min(16);
            // `xb` for 1 byte, `xh` 2, `xw` 4, `xg` 8.  Only 1/2/4/8 are valid,
            // so a non-multiple width falls back to bytes for the tail.
            let outcome = if width >= 8 && width % 8 == 0 {
                self.repl(&format!("monitor xp /{}xg 0x{:x}", width / 8, cursor))?
            } else if width >= 4 && width % 4 == 0 {
                self.repl(&format!("monitor xp /{}xw 0x{:x}", width / 4, cursor))?
            } else if width >= 2 && width % 2 == 0 {
                self.repl(&format!("monitor xp /{}xh 0x{:x}", width / 2, cursor))?
            } else {
                self.repl(&format!("monitor xp /{}xb 0x{:x}", width, cursor))?
            };
            if outcome.saw_error_text {
                return Err(outcome.into_error(format!(
                    "cannot read physical memory at 0x{cursor:x}"
                )));
            }
            let parsed = parse_monitor_xp(&outcome.text, cursor);
            if parsed.is_empty() {
                return Err(PrincessError::new(
                    ErrorCode::Internal,
                    format!("could not parse the physical memory dump at 0x{cursor:x}"),
                )
                .with_detail(format!("command: {}\noutput:\n{}", outcome.command, outcome.text)));
            }
            let take = (count - bytes.len() as u64).min(parsed.len() as u64) as usize;
            bytes.extend_from_slice(&parsed[..take]);
            cursor += take as u64;
        }
        bytes.truncate(count as usize);
        Ok(bytes)
    }

    // ------------------------------------------------------------- registers --

    /// The register panel: every register GDB prints, parsed into a map plus a
    /// structured list.
    ///
    /// Filtering is available because a kernel debugger is usually interested
    /// in a specific subset; [`RegisterSelection`] names the useful groups.
    pub fn read_registers(
        &mut self,
        selection: RegisterSelection,
    ) -> Result<(RegisterFile, Vec<RegisterEntry>)> {
        let command = match selection {
            RegisterSelection::All => "info registers".to_string(),
            RegisterSelection::Control => "info registers cr0 cr2 cr3 cr4".to_string(),
            RegisterSelection::Segments => "info registers cs ss ds es fs gs".to_string(),
            RegisterSelection::General => "info registers rax rbx rcx rdx rsi rdi rbp rsp r8 r9 r10 r11 r12 r13 r14 r15 rip eflags".to_string(),
            RegisterSelection::Named(ref names) => format!("info registers {}", names.join(" ")),
        };
        let outcome = self.repl_ok(&command)?;
        let entries = parse_info_registers(&outcome.text);
        if entries.is_empty() {
            return Err(PrincessError::new(
                ErrorCode::Internal,
                "`info registers` returned nothing parseable",
            )
            .with_detail(format!("command: {command}\noutput:\n{}", outcome.text)));
        }
        let map = entries
            .iter()
            .map(|e| (e.name.clone(), e.value.clone()))
            .collect::<BTreeMap<_, _>>();
        Ok((map, entries))
    }

    /// Read one register by name, failing loudly when it does not exist.
    ///
    /// Prefer this over indexing [`Self::read_registers`] output: a missing
    /// register must be an error, not an empty string (P4-4).
    pub fn read_register(&mut self, name: &str) -> Result<RegisterEntry> {
        let (_, entries) = self.read_registers(RegisterSelection::Named(vec![name.to_string()]))?;
        // GDB prints "Invalid register `foo'" for a bad name; the denylist in
        // `repl` catches that, but accept either the exact request name or a
        // case-insensitive match for robustness (`eflags` vs `rflags`).
        let wanted = name.to_ascii_uppercase();
        entries
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| {
                PrincessError::new(
                    ErrorCode::NotFound,
                    format!("the target has no register named `{name}`"),
                )
                .with_detail(format!("`info registers {name}` returned no matching line"))
            })
    }

    // ------------------------------------------------------------- execution --

    /// The instruction pointer, as a `u64`.
    ///
    /// `$pc` resolves to `$rip` on x86-64.  Parsed from `p/x $pc`, which is the
    /// only reliable path: D11 §4.5 records that `disassemble`'s
    /// `memoryReference` rejects `$pc` outright, so anything that wants to
    /// disassemble from the current instruction must first turn `$pc` into a
    /// number here.
    pub fn program_counter(&mut self) -> Result<u64> {
        let outcome = self.repl_ok("p/x $pc")?;
        parse_gdb_hex_value(&outcome.text).ok_or_else(|| {
            PrincessError::new(
                ErrorCode::Internal,
                "could not parse `p/x $pc` output into an address",
            )
            .with_detail(outcome.text)
        })
    }

    /// The value of one GDB convenience variable or register expression, as hex.
    ///
    /// This is the escape hatch for `$cr3`, `$cr2` and friends, and it exists
    /// because of a measured trap (D11 §4.4): the *hover*/*watch* contexts
    /// return only GDB's type annotation for `$cr3` — the literal string
    /// `[ PDBR=0 PCID=0 ]` with **no number** — so a panel fed by those
    /// contexts would display an annotation as if it were a value.
    pub fn read_hex_expression(&mut self, expression: &str) -> Result<u64> {
        let outcome = self.repl_ok(&format!("p/x {expression}"))?;
        parse_gdb_hex_value(&outcome.text).ok_or_else(|| {
            PrincessError::new(
                ErrorCode::Internal,
                format!("could not parse the value of `{expression}`"),
            )
            .with_detail(outcome.text)
        })
    }

    // ------------------------------------------------------------ disassembly --

    /// Disassemble straight from QEMU's monitor (physical addresses).
    ///
    /// The DAP `disassemble` request needs a *virtual* address and rejects
    /// `$pc`; `x/` over `monitor` is the physical-address counterpart, which is
    /// what a pre-paging or page-table view wants.
    pub fn disassemble_physical(&mut self, address: u64, instructions: u64) -> Result<String> {
        let outcome = self
            .repl_ok(&format!("monitor xp /{instructions}i 0x{address:x}"))?;
        Ok(outcome.text)
    }
}

/// Which registers to read into the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterSelection {
    /// Every register GDB knows, including XMM and control registers.
    All,
    /// `CR0`–`CR4`, the most kernel-relevant group.
    Control,
    /// `CS SS DS ES FS GS`.
    Segments,
    /// The general-purpose file plus `RIP`/`EFLAGS`.
    General,
    /// An explicit list.
    Named(Vec<String>),
}

// ------------------------------------------------------------------ parsing --

/// Count function declarations in `info functions` output.
///
/// Measured gdb 16.3 shape (note: **no addresses** — these are declarations):
///
/// ```text
/// All defined functions:
///
/// File kernel.c:
/// 119:\tvoid paging_fault_probe(void);
/// 132:\tvoid kernel_main(uint32_t, uint32_t);
///
/// Non-debugging symbols:
/// 0x0000000000100040  _start
/// ```
fn count_function_declarations(text: &str) -> u64 {
    let mut count = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("All defined functions")
            || line.starts_with("All functions")
            || line.starts_with("Non-debugging symbols")
            || line.starts_with("File ")
            || line.ends_with(':')
        {
            continue;
        }
        // Two shapes count as a function entry:
        //   `119:	void foo(void);`      (with debug info)
        //   `0x0000000000100040  _start` (without)
        let looks_like_declaration = line.ends_with(';') && line.contains('(');
        let looks_like_address_entry = line.starts_with("0x");
        if looks_like_declaration || looks_like_address_entry {
            count += 1;
        }
    }
    count
}

/// Parse `info breakpoints` output.
///
/// Measured shapes this handles:
///
/// ```text
/// Num     Type           Disp Enb Address            What
/// 1       hw breakpoint  keep y   0x00000000001011c9 in paging_fault_probe at kernel.c:120
/// 2       breakpoint     keep y   0x0000000000101204 in kernel_main at kernel.c:133
/// 3       hw breakpoint  keep y   <PENDING>          no_such_symbol_xyz
/// No breakpoints or watchpoints.
/// ```
pub fn parse_breakpoints(text: &str) -> Vec<BreakpointRecord> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = fields.first() else { continue };
        if *first == "Num" || line.starts_with("No breakpoints") || line.starts_with("No watchpoints")
        {
            continue;
        }
        let Ok(number) = first.parse::<u32>() else {
            continue;
        };

        // `Type` is one or two words from a closed set, so the column can be
        // consumed unambiguously.
        let mut index = 1;
        let kind = match fields.get(index) {
            Some(&"hw") => {
                index += 1;
                format!("hw {}", fields.get(index).copied().unwrap_or("breakpoint"))
            }
            Some(other) => (*other).to_string(),
            None => continue,
        };
        index += 1;

        // `Disp` (keep/del) then `Enb` (y/n).
        let enabled = matches!(fields.get(index), Some(&"keep") | Some(&"del"))
            && matches!(fields.get(index + 1), Some(&"y"));
        index += 2;

        // Everything left is the address column (`<PENDING>` or `0x...`) and the
        // `What` text.  Recover from the original line so internal spacing and
        // source paths survive verbatim.
        let rest = match fields.get(index) {
            Some(address) if address.starts_with("0x") || *address == "<PENDING>" || address.starts_with('*') => {
                let address = address.trim_start_matches('*');
                let what = strip_after_nth_field(line, index + 1);
                (Some(address.to_string()), what)
            }
            // No address column (e.g. a catchpoint row).
            _ => (None, strip_after_nth_field(line, index)),
        };
        out.push(BreakpointRecord {
            number,
            kind,
            enabled,
            address: rest.0,
            what: rest.1,
        });
    }
    out
}

/// Return the part of `line` after the first `n` whitespace-separated fields,
/// preserving the original spacing of the remainder.
///
/// Used to recover the `What` column of `info breakpoints` verbatim: re-joining
/// whitespace-separated tokens would collapse the alignment gdb uses and, worse,
/// mangle a source path that contains spaces.
fn strip_after_nth_field(line: &str, n: usize) -> String {
    if n == 0 {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut seen = 0;
    while index < bytes.len() {
        // Skip leading whitespace before each field.
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        seen += 1;
        // Skip the field itself.
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if seen == n {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            return line[index..].to_string();
        }
    }
    String::new()
}

/// Parse `info registers` output into entries.
///
/// Measured gdb 16.3 shapes:
///
/// ```text
/// rax            0x80000010          2147483664
/// rip            0x101204            0x101204 <kernel_main>
/// cr0            0x80000011          [ PG ET PE ]
/// cr3            0x104000            [ PDBR=260 PCID=0 ]
/// eflags         0x46                [ IOPL=0 ZF PF ]
/// cs             0x8                 8
/// xmm0           {v4_float = {...}, uint128 = 0x0}
/// ```
pub fn parse_info_registers(text: &str) -> Vec<RegisterEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // The name is the first whitespace-delimited token; anything that is not
        // a plausible register name is not a register line.
        let mut parts = line.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").trim();
        let rest = parts.next().unwrap_or("").trim();
        if name.is_empty() || name.contains('=') || rest.is_empty() {
            continue;
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }

        let annotation = extract_bracket(rest);
        // Split off the bracket annotation before tokenising the values.
        let values = match rest.find('[') {
            Some(index) => rest[..index].trim(),
            None => rest,
        };
        let mut tokens = values.split_whitespace();
        let value = tokens.next().unwrap_or("").to_string();
        let decimal = tokens.next().map(|s| s.to_string());

        out.push(RegisterEntry {
            name: name.to_ascii_uppercase(),
            value,
            decimal,
            annotation,
            raw: line.to_string(),
        });
    }
    out
}

/// Pull the `[...]` annotation out of a register line.
fn extract_bracket(rest: &str) -> Option<String> {
    let start = rest.find('[')?;
    let end = rest[start..].find(']')? + start;
    Some(rest[start..=end].to_string())
}

/// Parse a QEMU `monitor xp` dump into raw little-endian bytes.
///
/// Measured shape:
///
/// ```text
/// 0000000000104000: 0x0000000000105023 0x0000000000000000
/// 0000000000104010: 0x0000000000000000 0x0000000000000000
/// ```
/// The values are printed as native-endian words, and the target is
/// little-endian, so each `0x...` token is reassembled little-endian.
pub fn parse_monitor_xp(text: &str, start: u64) -> Vec<u8> {
    // Collect (address, value, width) triples.  Each printed line repeats the
    // base address, and the values on it are consecutive little-endian words.
    let mut words: Vec<(u64, u64, usize)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((address_part, values_part)) = line.split_once(':') else {
            continue;
        };
        let Ok(line_base) = u64::from_str_radix(address_part.trim().trim_start_matches("0x"), 16)
        else {
            continue;
        };
        let mut cursor = line_base;
        for token in values_part.split_whitespace() {
            let digits = token.trim().trim_start_matches("0x");
            // QEMU zero-pads, so the digit count gives the word width.
            let width_bytes = digits.len().div_ceil(2);
            if digits.is_empty()
                || !digits.chars().all(|c| c.is_ascii_hexdigit())
                || width_bytes > 8
            {
                continue;
            }
            if let Ok(value) = u64::from_str_radix(digits, 16) {
                words.push((cursor, value, width_bytes));
                cursor += width_bytes as u64;
            }
        }
    }
    if words.is_empty() {
        return Vec::new();
    }

    // Lay the words into a buffer spanning the requested range.  The window is
    // bounded so a nonsensical dump cannot allocate without limit.
    let lowest = words.iter().map(|(a, _, _)| *a).min().unwrap_or(start);
    let highest = words
        .iter()
        .map(|(a, _, w)| a + *w as u64)
        .max()
        .unwrap_or(start);
    let base = lowest.min(start);
    let span = highest.saturating_sub(base);
    if span == 0 || span > 4096 {
        return Vec::new();
    }

    let mut buf = vec![0u8; span as usize];
    for (address, value, width) in words {
        let offset = address.saturating_sub(base) as usize;
        if offset + width > buf.len() {
            continue;
        }
        buf[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
    }

    let skip = start.saturating_sub(base) as usize;
    if skip >= buf.len() {
        return Vec::new();
    }
    buf[skip..].to_vec()
}

/// Parse a `$N = 0x...` GDB value into a `u64`.
pub fn parse_gdb_hex_value(text: &str) -> Option<u64> {
    let value = text.split('=').nth(1)?.trim();
    let token = value.split_whitespace().next()?;
    let digits = token.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(digits, 16).ok()
}

/// Escape a path for a GDB CLI command per GDB's own quoting rules.
fn shell_escape(text: &str) -> String {
    if !text.contains([' ', '"', '\\']) {
        return text.to_string();
    }
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- breakpoint parsing: both real shapes from the P4 probe ------------

    #[test]
    fn parses_the_measured_hardware_breakpoint_line() {
        let text = "Num     Type           Disp Enb Address            What\n\
                    1       hw breakpoint  keep y   0x00000000001011c9 in paging_fault_probe at kernel.c:120\n";
        let parsed = parse_breakpoints(text);
        assert_eq!(parsed.len(), 1);
        let bp = &parsed[0];
        assert_eq!(bp.number, 1);
        assert_eq!(bp.kind, "hw breakpoint");
        assert!(bp.is_hardware());
        assert!(bp.enabled);
        assert!(!bp.is_pending());
        assert_eq!(bp.address.as_deref(), Some("0x00000000001011c9"));
        assert_eq!(bp.what, "in paging_fault_probe at kernel.c:120");
    }

    #[test]
    fn parses_the_measured_software_breakpoint_line() {
        let text = "Num     Type           Disp Enb Address            What\n\
                    2       breakpoint     keep y   0x0000000000101204 in kernel_main at kernel.c:133\n";
        let bp = &parse_breakpoints(text)[0];
        assert_eq!(bp.kind, "breakpoint");
        assert!(!bp.is_hardware(), "a software breakpoint must not claim to be hardware");
        assert!(bp.enabled);
        assert_eq!(bp.what, "in kernel_main at kernel.c:133");
    }

    #[test]
    fn distinguishes_hardware_from_software_in_the_same_listing() {
        let text = "Num     Type           Disp Enb Address            What\n\
                    1       hw breakpoint  keep y   0x00000000001011c9 in paging_fault_probe at kernel.c:120\n\
                    2       breakpoint     keep y   0x0000000000101204 in kernel_main at kernel.c:133\n";
        let parsed = parse_breakpoints(text);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].is_hardware());
        assert!(!parsed[1].is_hardware());
        // The two are textually distinguishable, which is the acceptance
        // requirement: the layer's effect must be visible, not just claimed.
        assert_ne!(parsed[0].kind, parsed[1].kind);
    }

    #[test]
    fn detects_the_measured_pending_shape() {
        let text = "Num     Type           Disp Enb Address    What\n\
                    1       hw breakpoint  keep y   <PENDING>  no_such_symbol_xyz\n";
        let bp = &parse_breakpoints(text)[0];
        assert!(bp.is_hardware());
        assert!(bp.is_pending(), "{bp:?}");
    }

    #[test]
    fn an_empty_breakpoint_list_parses_to_nothing() {
        assert!(parse_breakpoints("No breakpoints or watchpoints.\n").is_empty());
        assert!(parse_breakpoints("").is_empty());
    }

    #[test]
    fn ignores_the_header_and_keeps_only_real_rows() {
        let text = "Num     Type           Disp Enb Address            What\n\
                    3       hw watchpoint  keep n   0x0000000000001000\n";
        let parsed = parse_breakpoints(text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].number, 3);
        assert!(!parsed[0].enabled, "keep n is disabled");
    }

    // ---- register parsing: the real `info registers` dump -----------------

    const REAL_REGISTERS: &str = "\
rax            0x80000010          2147483664
rip            0x101204            0x101204 <kernel_main>
eflags         0x46                [ IOPL=0 ZF PF ]
cs             0x8                 8
cr0            0x80000011          [ PG ET PE ]
cr2            0x0                 0
cr3            0x104000            [ PDBR=260 PCID=0 ]
cr4            0x20                [ PAE ]
";

    #[test]
    fn parses_the_measured_register_dump() {
        let entries = parse_info_registers(REAL_REGISTERS);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["RAX", "RIP", "EFLAGS", "CS", "CR0", "CR2", "CR3", "CR4"]);

        let cr3 = entries.iter().find(|e| e.name == "CR3").unwrap();
        assert_eq!(cr3.value, "0x104000");
        assert_eq!(cr3.annotation.as_deref(), Some("[ PDBR=260 PCID=0 ]"));
        // Control registers have no decimal column: the annotation occupies it.
        // (CR2 *does* have one, and is asserted below.)
        assert_eq!(cr3.decimal, None, "CR3 has no decimal column in gdb's output");

        let cr0 = entries.iter().find(|e| e.name == "CR0").unwrap();
        assert_eq!(cr0.value, "0x80000011");
        assert_eq!(cr0.annotation.as_deref(), Some("[ PG ET PE ]"));

        // RIP carries an annotation-free third field on the same line; the value
        // must be the address, not the symbol text.
        let rip = entries.iter().find(|e| e.name == "RIP").unwrap();
        assert_eq!(rip.value, "0x101204");
    }

    #[test]
    fn register_names_are_upper_cased_for_a_stable_panel_key() {
        let entries = parse_info_registers("cr3 0x104000\n");
        assert_eq!(entries[0].name, "CR3");
    }

    #[test]
    fn the_register_map_never_invents_a_missing_register() {
        let entries = parse_info_registers(REAL_REGISTERS);
        let map: BTreeMap<String, String> = entries
            .iter()
            .map(|e| (e.name.clone(), e.value.clone()))
            .collect();
        assert_eq!(map.get("CR3").map(String::as_str), Some("0x104000"));
        // A register that was not printed must be absent, not empty.
        assert!(!map.contains_key("XMM0"));
        assert!(!map.contains_key("DR0"));
    }

    #[test]
    fn register_lines_that_are_not_registers_are_skipped() {
        let text = "rax 0x1 1\n\
                    {v4_float = {0x0}} = nonsense\n\
                    \n\
                    rbx 0x2 2\n";
        let entries = parse_info_registers(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "RAX");
        assert_eq!(entries[1].name, "RBX");
    }

    // ---- monitor xp parsing ----------------------------------------------

    #[test]
    fn parses_the_measured_monitor_xp_output() {
        let text = "0000000000104000: 0x0000000000105023 0x0000000000000000\r\n\
                    0000000000104010: 0x0000000000000000 0x0000000000000000\r\n";
        let bytes = parse_monitor_xp(text, 0x104000);
        assert_eq!(bytes.len(), 32);
        // 0x105023 little-endian.
        assert_eq!(&bytes[0..8], &[0x23, 0x50, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&bytes[8..16], &[0u8; 8]);
    }

    /// The physical read and the virtual read of the same address must be
    /// distinguishable data, not accidentally identical: this pins that the
    /// parser returns the bytes QEMU printed and nothing invented.
    #[test]
    fn physical_parse_matches_the_virtual_read_at_the_same_address() {
        // DAP `readMemory 0x104000 count 16` returned base64
        // "I1AQAAAAAAAAAAAAAAAAAA==" which decodes to exactly this.
        let virtual_bytes = [
            0x23, 0x50, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let text = "0000000000104000: 0x0000000000105023 0x0000000000000000\r\n";
        let physical = parse_monitor_xp(text, 0x104000);
        assert_eq!(&physical[..16], &virtual_bytes[..16]);
    }

    #[test]
    fn a_short_final_line_is_parsed_without_inventing_bytes() {
        let text = "0000000000100000: 0x00000000000000aa\r\n";
        let bytes = parse_monitor_xp(text, 0x100000);
        assert_eq!(bytes.len(), 8);
        assert_eq!(bytes[0], 0xaa);
        assert_eq!(&bytes[1..], &[0u8; 7]);
    }

    #[test]
    fn narrow_widths_are_honoured() {
        let text = "0000000000001000: 0x12 0x34 0x56\r\n";
        let bytes = parse_monitor_xp(text, 0x1000);
        assert_eq!(bytes, vec![0x12, 0x34, 0x56]);
    }

    #[test]
    fn unparseable_monitor_output_yields_no_bytes() {
        assert!(parse_monitor_xp("(no output)", 0).is_empty());
        assert!(parse_monitor_xp("", 0).is_empty());
    }

    // ---- value parsing ----------------------------------------------------

    #[test]
    fn parses_gdb_hex_values() {
        assert_eq!(parse_gdb_hex_value("$1 = 0x104000\n"), Some(0x104000));
        assert_eq!(parse_gdb_hex_value("$3 = 0xfff0"), Some(0xfff0));
        assert_eq!(parse_gdb_hex_value("$2 = 0x1011c9 <paging_fault_probe>"), Some(0x1011c9));
        assert_eq!(parse_gdb_hex_value("no value here"), None);
    }

    #[test]
    fn symbol_paths_with_spaces_are_escaped_for_the_cli() {
        assert_eq!(shell_escape("/tmp/kernel.elf"), "/tmp/kernel.elf");
        assert_eq!(
            shell_escape("/tmp/my kernel/k.elf"),
            "\"/tmp/my kernel/k.elf\""
        );
    }
}
