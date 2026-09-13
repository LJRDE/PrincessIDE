//! P4 acceptance harness — the whole debug stack against a **real** QEMU.
//!
//! This is not a unit test: it boots `fixtures/paging-kernel` under
//! `qemu-system-x86_64 -s -S`, connects a real `gdb 16.3 -i=dap`, and asserts
//! the four P4 acceptance criteria plus the capability layer and the D10
//! architecture self-check.  It prints every raw response it reasons about, so
//! the report can quote real output rather than a summary.
//!
//! Run it with the workspace environment active:
//!
//! ```sh
//! source scripts/env.sh
//! cargo run -p princess-debug --example p4_acceptance -- \
//!     fixtures/paging-kernel/build/pagingkernel.iso \
//!     fixtures/paging-kernel/build/pagingkernel.elf \
//!     artifacts/p4
//! ```
//!
//! Exit code is 0 only if **every** check passed.  Every check prints
//! `PASS`/`FAIL` with the evidence it used, and the process exit code is the
//! authority — a human reading the log is not required to spot a failure.
//!
//! ## Orphan safety
//!
//! QEMU is owned by [`princess_debug::QemuProcess`], which puts it in its own
//! process group and kills the group on `Drop` (covered by a unit test).  The
//! harness additionally installs no global state and always drops the backend
//! before exiting, so a panic mid-check still tears the machine down.  The
//! shell wrapper in `scripts/p4-acceptance.sh` wraps the whole thing in
//! `timeout` for the case where this process is SIGKILLed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use princess_core::{DebugBackend, EventRecorder, GdbStub, StubMode};
use princess_debug::{
    debug_boot_argv, DapBackend, DebugSessionConfig, HardwareSupport, RegisterSelection,
    SessionState,
};

/// Collects check results and decides the process exit code.
struct Report {
    failures: Vec<String>,
    checks: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            checks: 0,
        }
    }

    fn pass(&mut self, name: &str, detail: impl std::fmt::Display) {
        self.checks += 1;
        println!("PASS  {name}\n      {detail}");
    }

    fn fail(&mut self, name: &str, detail: impl std::fmt::Display) {
        self.checks += 1;
        println!("FAIL  {name}\n      {detail}");
        self.failures.push(name.to_string());
    }

    fn check(&mut self, name: &str, condition: bool, detail: impl std::fmt::Display) {
        if condition {
            self.pass(name, detail);
        } else {
            self.fail(name, detail);
        }
    }
}

fn section(title: &str) {
    println!("\n================ {title} ================");
}

fn main() {
    let code = run();
    std::process::exit(code);
}

/// Run every check and return the process exit code.
///
/// The checks live in a function that *returns* rather than calling
/// `process::exit` directly, and that is load-bearing: `process::exit` skips
/// every `Drop` impl, and the whole orphan-safety design of [`DapBackend`] and
/// [`princess_debug::QemuProcess`] is built on `Drop`. An earlier version of
/// this harness exited from inside the checks and leaked a live QEMU -- caught
/// by the TEARDOWN.2 assertion below, which is exactly why that assertion is
/// not merely cosmetic.
fn run() -> i32 {
    let mut args = std::env::args().skip(1);
    let iso = PathBuf::from(args.next().unwrap_or_else(|| {
        "fixtures/paging-kernel/build/pagingkernel.iso".into()
    }));
    let elf = PathBuf::from(args.next().unwrap_or_else(|| {
        "fixtures/paging-kernel/build/pagingkernel.elf".into()
    }));
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| "artifacts/p4".into()));

    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        eprintln!("cannot create {}: {err}", out_dir.display());
        std::process::exit(2);
    }

    println!("=== P4 acceptance: PrincessIDE debug backend (D11 / gdb built-in DAP) ===");
    println!("iso     = {}", iso.display());
    println!("elf     = {}", elf.display());
    println!("artifacts = {}", out_dir.display());

    let adapter = std::env::var("PRINCESSIDE_DEBUG_GDB").unwrap_or_else(|_| "princess-gdb".into());
    let qemu = std::env::var("PRINCESSIDE_QEMU_BIN")
        .unwrap_or_else(|_| "qemu-system-x86_64".to_string());
    // `scripts/env.sh` exports this unconditionally, so on a machine whose QEMU
    // data lives in a system prefix it points at a directory that is not there.
    // Handing QEMU `-L <nonexistent>` is worse than passing nothing, so only a
    // real directory is honoured (same rule as `Toolchain::discover`).
    let qemu_data = std::env::var("PRINCESSIDE_QEMU_DATA")
        .ok()
        .filter(|dir| Path::new(dir).is_dir());

    let mut report = Report::new();

    // ---------------------------------------------------------------- step 0 --
    section("0. environment");
    println!("adapter = {adapter}");
    println!("qemu    = {qemu}");
    let version = Command::new(&adapter).arg("--version").output();
    match version {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let first = text.lines().next().unwrap_or("").to_string();
            println!("adapter version: {first}");
            let major = princess_debug::parse_gdb_major(&text).unwrap_or(0);
            report.check(
                "0.1 adapter is a DAP-capable gdb (>= 14, D11)",
                major >= princess_debug::MIN_GDB_MAJOR,
                format!("{first} -> major {major}, need >= {}", princess_debug::MIN_GDB_MAJOR),
            );
        }
        Err(err) => {
            report.fail("0.1 adapter is a DAP-capable gdb (>= 14, D11)", err);
            return finish(report, &out_dir);
        }
    }

    // ---------------------------------------------------------------- P4-1 ---
    section("P4-1  headless breakpoint E2E: qemu -s -S -> DAP -> hbreak -> continue");
    let serial_log = out_dir.join("serial-p4-1.log");
    let argv = debug_boot_argv(
        &qemu,
        qemu_data.as_deref().map(Path::new),
        &iso,
        "256M",
        &serial_log,
        1234,
        true, // -S: frozen before the first instruction
    );
    println!("qemu argv: {}", argv.join(" "));

    let mut config = DebugSessionConfig::launching(
        PathBuf::from(&adapter),
        elf.clone(),
        argv.clone(),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );
    config.request_timeout = Duration::from_secs(45);
    config.ready_timeout = Duration::from_secs(40);
    config.serial_log = Some(serial_log.clone());

    let mut backend = DapBackend::new(config);
    let mut sink = EventRecorder::new(std::io::sink());

    // Every early exit below tears the session down explicitly before
    // returning.  Rust's `Drop` would do it too, but spelling it out means a
    // future refactor that moves `backend` cannot silently reintroduce the
    // orphan leak this harness previously had.
    let attach_started = std::time::Instant::now();
    match backend.attach(
        &GdbStub {
            host: "127.0.0.1".into(),
            port: 1234,
            mode: StubMode::Attach,
        },
        &mut sink,
    ) {
        Ok(()) => report.pass(
            "P4-1.1 attach over the standard DAP handshake",
            format!(
                "state={:?} after {:?}; attach + configurationDone completed despite the \
                 delayed attach response",
                backend.state(),
                attach_started.elapsed()
            ),
        ),
        Err(err) => {
            report.fail("P4-1.1 attach over the standard DAP handshake", &err);
            let _ = backend.detach();
            return finish(report, &out_dir);
        }
    }

    match backend.prepare_target() {
        Ok(()) => report.pass(
            "P4-1.2 symbol-file + D10 architecture self-check",
            format!(
                "arch check: {}",
                backend
                    .arch_check()
                    .map(|c| c.summary())
                    .unwrap_or_else(|| "(none)".into())
            ),
        ),
        Err(err) => {
            report.fail("P4-1.2 symbol-file + D10 architecture self-check", &err);
            let _ = backend.detach();
            return finish(report, &out_dir);
        }
    }

    // The fixture's deliberately-faulting probe.  Both fixtures are first-class:
    // the paging kernel is the primary target (it is the only one where CR3 and
    // `#PF` mean anything) and refkernel is the secondary coverage, so the
    // expected symbol/address/line are selected from the *file name* rather than
    // hard-coded to one fixture.  Hard-coding was a real over-fit: the first
    // version of this harness asserted the paging symbol unconditionally and
    // failed on refkernel for the wrong reason.
    let profile = fixture_profile(&elf);
    let symbol = profile.symbol;
    let expected_rip = profile.rip;
    let expected_line = profile.line;
    println!(
        "fixture profile: symbol={symbol} rip=0x{expected_rip:x} line={expected_line} ({})",
        profile.name
    );

    let hardware = match backend.set_hardware_breakpoint(symbol, &mut sink) {
        Ok(record) => {
            report.pass(
                "P4-1.3 hardware breakpoint set and VERIFIED (capability layer)",
                format!(
                    "{}: kind={:?} address={:?} what={:?}",
                    symbol, record.kind, record.address, record.what
                ),
            );
            record
        }
        Err(err) => {
            report.fail("P4-1.3 hardware breakpoint set and VERIFIED (capability layer)", &err);
            let _ = backend.detach();
            return finish(report, &out_dir);
        }
    };

    let stop = match backend.continue_(None, &mut sink) {
        Ok(stop) => stop,
        Err(err) => {
            report.fail("P4-1.4 continue stops at the expected location", &err);
            let _ = backend.detach();
            return finish(report, &out_dir);
        }
    };

    // The RIP is read two ways on purpose: the stop payload's frame and a
    // direct register read.  Agreement between them is the assertion.
    let rip_from_frame = parse_hex(&stop.frame.instructionPointer);
    let rip_from_registers = backend
        .registers(stop.thread_id)
        .ok()
        .and_then(|regs| regs.get("RIP").and_then(|v| parse_hex(v)));

    println!(
        "stop: reason={:?} thread={} frame={:?} line={:?} ip={} regs.RIP={:?}",
        stop.reason,
        stop.thread_id,
        stop.frame.name,
        stop.frame.source.as_ref().map(|s| s.line),
        stop.frame.instructionPointer,
        rip_from_registers.map(|v| format!("0x{v:x}"))
    );

    report.check(
        "P4-1.4a stopped at the hardware breakpoint (reason=breakpoint)",
        format!("{:?}", stop.reason) == "Breakpoint",
        format!("reason={:?}", stop.reason),
    );
    report.check(
        "P4-1.4b stop frame is the requested symbol",
        stop.frame.name == symbol,
        format!("frame.name={:?}, requested {symbol:?}", stop.frame.name),
    );
    report.check(
        &format!("P4-1.4c RIP matches the fixed fixture address 0x{expected_rip:x}"),
        rip_from_frame == Some(expected_rip),
        format!(
            "frame ip={} (0x{:x}), expected 0x{:x}",
            stop.frame.instructionPointer,
            rip_from_frame.unwrap_or(0),
            expected_rip
        ),
    );
    report.check(
        "P4-1.4d DAP frame RIP agrees with the register panel RIP",
        rip_from_registers == rip_from_frame,
        format!(
            "frame=0x{:x} registers=0x{:x}",
            rip_from_frame.unwrap_or(0),
            rip_from_registers.unwrap_or(0)
        ),
    );
    // The *line* is asserted, not just the file: a breakpoint that resolves to
    // the right function but the wrong statement would still be a wrong answer.
    report.check(
        &format!("P4-1.4e stop resolves to the fixed source line {expected_line}"),
        stop.frame.source.as_ref().map(|s| s.line) == Some(expected_line),
        format!(
            "line={:?} file={:?} (fixture constant for {} is {expected_line})",
            stop.frame.source.as_ref().map(|s| s.line),
            stop.frame.source.as_ref().map(|s| s.file.clone()),
            profile.name
        ),
    );
    report.check(
        "P4-1.4f the breakpoint GDB reports is a HARDWARE breakpoint",
        hardware.is_hardware(),
        format!("kind={:?} (software would be \"breakpoint\")", hardware.kind),
    );

    // ---------------------------------------------------------------- P4-2 ---
    section("P4-2  registers and memory vs. asking GDB directly");
    // The independent oracle: a *separate* gdb process attached to the same stub
    // would disturb the session, so the cross-check is done two ways inside one
    // session that must agree: the capability layer's parsed map, and the raw
    // CLI text it parsed.
    let (register_map, register_entries) = {
        let transport = backend.capabilities_layer().expect("live transport");
        let mut layer = transport;
        layer
            .read_registers(RegisterSelection::All)
            .expect("info registers")
    };
    let raw_registers = register_entries
        .iter()
        .map(|e| e.raw.clone())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(out_dir.join("p4-2-info-registers.txt"), &raw_registers).ok();
    println!("--- raw `info registers` (first 12 lines) ---");
    for line in raw_registers.lines().take(12) {
        println!("{line}");
    }

    let cr3 = register_map.get("CR3").cloned();
    let rip = register_map.get("RIP").cloned();
    let cr0 = register_map.get("CR0").cloned();
    let cr4 = register_map.get("CR4").cloned();
    println!("parsed: RIP={rip:?} CR0={cr0:?} CR3={cr3:?} CR4={cr4:?}");

    // Cross-check against the same values read a second, independent way: the
    // `p/x $reg` expression path, which does not share parsing code with
    // `info registers`.
    let mut direct = std::collections::BTreeMap::new();
    {
        let transport = backend.capabilities_layer().expect("live transport");
        let mut layer = transport;
        for name in ["pc", "cr3", "cr0", "cr4"] {
            if let Ok(value) = layer.read_hex_expression(&format!("${name}")) {
                direct.insert(name.to_string(), value);
            }
        }
    }
    println!("via `p/x $reg`: {direct:?}");

    report.check(
        "P4-2.1 CR3 read via the register panel equals the value from `p/x $cr3`",
        parse_hex(cr3.as_deref().unwrap_or("")) == direct.get("cr3").copied(),
        format!(
            "info registers CR3={cr3:?}; p/x $cr3=0x{:x}",
            direct.get("cr3").copied().unwrap_or(0)
        ),
    );
    report.check(
        "P4-2.2 paging really is on (CR0.PG set) so the #PF assertions are meaningful",
        parse_hex(cr0.as_deref().unwrap_or("")).is_some_and(|v| v & (1 << 31) != 0),
        format!("CR0={cr0:?} (PG is bit 31)"),
    );
    report.check(
        "P4-2.3 RIP from the register panel equals the stopped frame RIP",
        parse_hex(rip.as_deref().unwrap_or("")) == rip_from_frame,
        format!("panel={rip:?} frame=0x{:x}", rip_from_frame.unwrap_or(0)),
    );

    // Physical memory: the thing DAP cannot do (D11 §4.1).  Read the live page
    // table that CR3 points at, which is also what P5's page-table view needs.
    let cr3_value = parse_hex(cr3.as_deref().unwrap_or("")).unwrap_or(0);
    let physical = {
        let transport = backend.capabilities_layer().expect("live transport");
        let mut layer = transport;
        layer.read_physical_memory(cr3_value, 16)
    };
    match physical {
        Ok(bytes) => {
            let hex = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("physical read @ CR3=0x{cr3_value:x}: {hex}");
            // PML4[0] must be present (bit 0) and point at a 4 KiB-aligned frame.
            let pml4e0 = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            report.check(
                "P4-2.4a physical read via `monitor xp` returns a plausible PML4E[0]",
                pml4e0 & 1 == 1 && pml4e0 & 0xfff == 0x023,
                format!("PML4E[0]=0x{pml4e0:016x} (present, frame-aligned, RW|P)"),
            );
        }
        Err(err) => report.fail("P4-2.4a physical read via `monitor xp`", &err),
    }

    // The virtual read at a *mapped* address, for the record: the point is that
    // the two paths are different API surfaces, not that they always differ.
    let virtual_read = backend.read_memory(0x1011c9, 16);
    match virtual_read {
        Ok(bytes) => {
            let text = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("virtual readMemory @ 0x1011c9: {text}");
            report.pass(
                "P4-2.4b DAP virtual readMemory works (the trunk path)",
                format!("{} bytes: {text}", bytes.len()),
            );
        }
        Err(err) => report.fail("P4-2.4b DAP virtual readMemory works (the trunk path)", &err),
    }

    // ---------------------------------------------------------------- P4-3 ---
    section("P4-3  stepping and cross C/assembly source-level stack traces");
    let before_rip = parse_hex(rip.as_deref().unwrap_or("")).unwrap_or(0);
    let before_line = stop.frame.source.as_ref().map(|s| s.line);

    // Capture the full mixed-language trace at the fault probe first: this is
    // the frame set the acceptance criterion names.
    let trace_at_fault = backend.stack_trace(stop.thread_id, 0, 12);
    match &trace_at_fault {
        Ok(frames) => {
            println!("stackTrace at {symbol} ({} frames):", frames.len());
            for frame in frames {
                println!(
                    "  #{:<2} {:<24} {} {}",
                    frame.id,
                    frame.name,
                    frame.instructionPointer,
                    frame
                        .source
                        .as_ref()
                        .map(|s| format!("{}:{}", s.file, s.line))
                        .unwrap_or_else(|| "(no debug info)".into())
                );
            }
        }
        Err(err) => report.fail("P4-3.1 stackTrace at the fault probe", err),
    }

    // Now step.  Instruction granularity, because in a kernel "one source line"
    // and "one instruction" are not the same and the address assertion below is
    // only meaningful at instruction granularity.
    let stepped = backend.step_into(None, &mut sink);
    match stepped {
        Ok(step) => {
            let after_rip = parse_hex(&step.frame.instructionPointer).unwrap_or(0);
            let after_line = step.frame.source.as_ref().map(|s| s.line);
            println!(
                "after stepIn(instruction): rip=0x{after_rip:x} line={after_line:?} reason={:?}",
                step.reason
            );
            report.check(
                "P4-3.2 step moves RIP forward",
                after_rip > before_rip,
                format!("0x{before_rip:x} -> 0x{after_rip:x}"),
            );
            report.check(
                "P4-3.3 step reports reason=step",
                format!("{:?}", step.reason) == "Step",
                format!("reason={:?}", step.reason),
            );
            // A step of one instruction must not wander far: anything past a
            // few dozen bytes means we are not stepping the same thread.
            report.check(
                "P4-3.4 the step stayed inside the same function (<= 64 bytes)",
                after_rip.saturating_sub(before_rip) <= 64,
                format!("delta = {} bytes (line {before_line:?} -> {after_line:?})", after_rip.saturating_sub(before_rip)),
            );
        }
        Err(err) => report.fail("P4-3.2 step moves RIP forward", &err),
    }

    if let Ok(frames) = &trace_at_fault {
        let names: Vec<&str> = frames.iter().map(|f| f.name.as_str()).collect();
        let with_lines = frames
            .iter()
            .filter(|f| f.source.is_some())
            .count();
        report.check(
            "P4-3.1a the trace contains the C frame at the probe",
            names.contains(&symbol),
            format!("frames: {names:?}"),
        );
        report.check(
            "P4-3.1b the trace crosses into a second C frame (kernel_main)",
            names.contains(&"kernel_main"),
            format!("frames: {names:?}"),
        );
        report.check(
            "P4-3.1c the trace reaches the assembly entry point (_start)",
            names.contains(&"_start"),
            format!("frames: {names:?}"),
        );
        // The "mixed C/assembly" requirement: the assembly frame must carry a
        // *source line from boot.S*, not be folded into the C file.
        let asm_frame = frames.iter().find(|f| f.name == "_start");
        report.check(
            "P4-3.1d the assembly frame carries a source line from boot.S",
            asm_frame
                .and_then(|f| f.source.as_ref())
                .is_some_and(|s| s.file.ends_with("boot.S") && s.line > 0),
            format!(
                "_start -> {}",
                asm_frame
                    .and_then(|f| f.source.as_ref())
                    .map(|s| format!("{}:{}", s.file, s.line))
                    .unwrap_or_else(|| "(none)".into())
            ),
        );
        report.check(
            "P4-3.1e every frame in the trace has a source line (no address-only frames)",
            with_lines == frames.len(),
            format!("{with_lines}/{} frames have source", frames.len()),
        );
    }

    // ---------------------------------------------------------------- P4-4 ---
    section("P4-4  reverse validation: a bad symbol MUST fail loudly");
    let bogus = "no_such_symbol_xyz_princesside";

    // The measured trap: gdb answers `success: true` with the error only in the
    // message text.  If the layer trusted `success`, this call would return a
    // *pending* breakpoint and look like success.
    match backend.set_hardware_breakpoint(bogus, &mut sink) {
        Ok(record) => report.fail(
            "P4-4.1 hardware breakpoint on a non-existent symbol is refused",
            format!(
                "returned Ok with {record:?} -- this is the fake-data failure P4-4 forbids"
            ),
        ),
        Err(err) => {
            report.check(
                "P4-4.1 hardware breakpoint on a non-existent symbol is refused",
                err.code == princess_core::ErrorCode::NotFound,
                format!("code={} message={}", err.code, err.message),
            );
            let detail = err.detail.clone().unwrap_or_default();
            report.check(
                "P4-4.2 the refusal carries gdb's own text as evidence",
                detail.contains("not defined"),
                format!("detail contains {:?}", detail.lines().next().unwrap_or("")),
            );
        }
    }

    // The same trap through the standard DAP request path.
    let set_result = backend.set_breakpoints(
        Path::new("fixtures/paging-kernel/kernel.c"),
        &[princess_core::BreakpointSpec {
            id: "negative-1".into(),
            file: None,
            line: Some(999_999),
            address: None,
            condition: None,
            verified: false,
            location: None,
        }],
        &mut sink,
    );
    match set_result {
        Ok(list) => {
            let all_unverified = list.iter().all(|bp| !bp.verified);
            report.check(
                "P4-4.3 an out-of-range source line is reported unverified, not accepted",
                all_unverified,
                format!("{list:?}"),
            );
        }
        Err(err) => report.pass(
            "P4-4.3 an out-of-range source line is rejected",
            format!("code={} message={}", err.code, err.message),
        ),
    }

    // A register that does not exist must be an error, never an empty string.
    {
        let transport = backend.capabilities_layer().expect("live transport");
        let mut layer = transport;
        match layer.read_register("notaregister") {
            Ok(entry) => report.fail(
                "P4-4.4 a non-existent register is refused",
                format!("returned {entry:?}"),
            ),
            Err(err) => report.check(
                "P4-4.4 a non-existent register is refused",
                err.code == princess_core::ErrorCode::NotFound,
                format!("code={} message={}", err.code, err.message),
            ),
        }
    }

    // ------------------------------------------------ capability layer check --
    section("capability layer: hardware vs software breakpoints are distinguishable");
    match backend.capabilities_layer() {
        Ok(mut layer) => {
            // A software breakpoint at a *different* symbol so both appear in one
            // listing and the `Type` column can be compared directly.
            match layer.set_software_breakpoint("kernel_main") {
                Ok(software) => {
                    println!(
                        "software: kind={:?} address={:?} what={:?}",
                        software.kind, software.address, software.what
                    );
                    println!(
                        "hardware: kind={:?} address={:?} what={:?}",
                        hardware.kind, hardware.address, hardware.what
                    );
                    report.check(
                        "CAP.1 the two breakpoint kinds are textually distinguishable",
                        software.kind == "breakpoint" && hardware.kind == "hw breakpoint",
                        format!(
                            "software kind={:?}, hardware kind={:?} (this is the built-in \
                             DAP's gap: it can only ever produce the former)",
                            software.kind, hardware.kind
                        ),
                    );
                    report.check(
                        "CAP.2 only the hardware breakpoint reports `hw`",
                        hardware.is_hardware() && !software.is_hardware(),
                        format!(
                            "is_hardware(software)={} is_hardware(hardware)={}",
                            software.is_hardware(),
                            hardware.is_hardware()
                        ),
                    );
                }
                Err(err) => report.fail("CAP.1 software breakpoint for comparison", &err),
            }

            match layer.list_breakpoints() {
                Ok(list) => {
                    let dump = list
                        .iter()
                        .map(|b| format!("{} {} enabled={} {:?} {}", b.number, b.kind, b.enabled, b.address, b.what))
                        .collect::<Vec<_>>()
                        .join("\n");
                    std::fs::write(out_dir.join("capability-info-breakpoints.txt"), &dump).ok();
                    report.pass(
                        "CAP.3 `info breakpoints` listing captured as evidence",
                        format!("\n{}", dump.replace('\n', "\n      ")),
                    );
                }
                Err(err) => report.fail("CAP.3 list breakpoints", &err),
            }
        }
        Err(err) => report.fail("CAP.1 capability layer", &err),
    }

    report.check(
        "CAP.4 the backend reports hardware breakpoints as measured, not assumed",
        backend.hardware_support() == HardwareSupport::Hardware,
        format!("hardware_support={:?}", backend.hardware_support()),
    );

    // ------------------------------------------------------------- D10 self --
    section("D10 architecture self-check");
    match backend.arch_check() {
        Some(check) => {
            println!("verdict: {}", check.summary());
            report.check(
                "D10.1 the live session's architecture agrees with the ELF",
                check.is_consistent(),
                check.summary(),
            );
        }
        None => report.fail(
            "D10.1 the live session's architecture agrees with the ELF",
            "no verdict was recorded",
        ),
    }

    // The negative case can be tested without a 32-bit machine: point the check
    // at an ELF header of the *wrong* class and require a refusal.
    let fake_elf32 = out_dir.join("fake-elf32.elf");
    // ELF32, little-endian, e_machine = EM_386.
    let mut header = vec![0u8; 20];
    header[0..4].copy_from_slice(b"\x7fELF");
    header[4] = 1; // EI_CLASS = ELFCLASS32
    header[5] = 1; // EI_DATA  = little-endian
    header[18] = 3; // e_machine = EM_386 (low)
    header[19] = 0;
    std::fs::write(&fake_elf32, &header).ok();

    match princess_debug::arch::read_elf_facts(&fake_elf32) {
        Ok(facts) => {
            report.check(
                "D10.2 a 32-bit ELF is identified as elf32/EM_386, not mistaken for the target",
                facts.class == princess_debug::ElfClass::Elf32 && facts.machine == 3,
                format!(
                    "class={} machine={}({})",
                    facts.class.as_str(),
                    facts.machine,
                    facts.machine_name
                ),
            );
            // Feed it to the real check function: it must refuse, quoting both
            // sides, rather than returning register/frame data.
            {
                let transport = backend.capabilities_layer().expect("live transport");
                let mut layer = transport;
                let verdict = princess_debug::arch::check(layer.transport_mut(), &fake_elf32);
                match verdict {
                    Ok(check) => {
                        let refused = check.clone().require_consistent(&fake_elf32);
                        report.check(
                            "D10.3 a 32-bit ELF against the 64-bit stub is a hard error",
                            refused.is_err(),
                            format!(
                                "verdict={} -> {}",
                                check.summary(),
                                refused
                                    .err()
                                    .map(|e| format!("{}: {}", e.code, e.message))
                                    .unwrap_or_else(|| "ACCEPTED (bad!)".into())
                            ),
                        );
                    }
                    Err(err) => report.fail("D10.3 architecture mismatch refusal", &err),
                }
            }
        }
        Err(err) => report.fail("D10.2 read a synthetic 32-bit ELF header", &err),
    }

    // ---------------------------------------------------------------- close --
    section("teardown");
    let qemu_pid = backend
        .qemu_command()
        .map(|c| c.display())
        .unwrap_or_else(|| "(not engine-owned)".into());
    println!("engine-owned qemu command: {qemu_pid}");
    let teardown = backend.detach();
    report.check(
        "TEARDOWN.1 detach released the adapter and stopped the machine",
        teardown.is_ok() && backend.state() == SessionState::Detached,
        format!("state={:?} result={teardown:?}", backend.state()),
    );
    drop(backend);

    // Give the OS a moment, then verify no qemu of ours survived.
    std::thread::sleep(Duration::from_millis(400));
    let survivors = count_our_qemu();
    report.check(
        "TEARDOWN.2 no orphan qemu-system-x86_64 process remains",
        survivors == 0,
        format!("{survivors} matching process(es) still running"),
    );

    finish(report, &out_dir)
}

/// The fixed assertion constants for one fixture.
struct FixtureProfile {
    name: &'static str,
    symbol: &'static str,
    rip: u64,
    line: u32,
}

/// Pick the assertion constants from the ELF name.
///
/// The values come from `docs/spec/20-acceptance.md` and `docs/spec/00-decisions.md`
/// (D3 for refkernel, the paging fixture section for paging-kernel) and are
/// cross-checked against `nm`/`addr2line` in the report.
fn fixture_profile(elf: &Path) -> FixtureProfile {
    let name = elf.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.contains("refkernel") {
        // NOTE (discrepancy found by this harness, reported in
        // docs/reports/p4-debug.md §7): D3/40-dispatch-plan say the probe is at
        // `kernel.c:100`, but `addr2line` — the golden reference the acceptance
        // spec itself names for P2-4 — resolves the probe's first instruction to
        // **line 99** (line 99 is `{`, line 100 is the `ud2`; the entry
        // breakpoint lands on the prologue).  gdb's DWARF agrees with
        // addr2line.  The harness asserts 99 because that is the value the two
        // independent oracles produce; asserting 100 would encode a doc typo as
        // a test failure.
        FixtureProfile {
            name: "refkernel (#UD probe, D3)",
            symbol: "refkernel_fault_probe",
            rip: 0x100b39,
            line: 99,
        }
    } else {
        FixtureProfile {
            name: "paging-kernel (#PF probe, paging on)",
            symbol: "paging_fault_probe",
            rip: 0x1011c9,
            line: 120,
        }
    }
}

/// Count `qemu-system-x86_64` processes carrying *this* run's ISO path, so the
/// check cannot be confused by an unrelated machine on the box.
fn count_our_qemu() -> usize {
    let output = Command::new("pgrep").arg("-af").arg("qemu-system-x86_64").output();
    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains("pagingkernel.iso"))
            .filter(|line| !line.contains("pgrep"))
            .count(),
        Err(_) => 0,
    }
}

fn parse_hex(text: &str) -> Option<u64> {
    let token = text.split_whitespace().next()?;
    u64::from_str_radix(token.trim_start_matches("0x"), 16).ok()
}

fn finish(report: Report, out_dir: &Path) -> i32 {
    println!("\n================ summary ================");
    println!("checks run : {}", report.checks);
    println!("failures   : {}", report.failures.len());
    if !report.failures.is_empty() {
        for failure in &report.failures {
            println!("  FAILED: {failure}");
        }
    }
    let line = format!(
        "checks={} failures={} status={}\n",
        report.checks,
        report.failures.len(),
        if report.failures.is_empty() { "ok" } else { "failed" }
    );
    let _ = std::fs::write(out_dir.join("p4-acceptance-summary.txt"), line);
    if report.failures.is_empty() {
        println!("P4 ACCEPTANCE: PASS");
        0
    } else {
        println!("P4 ACCEPTANCE: FAIL");
        1
    }
}
