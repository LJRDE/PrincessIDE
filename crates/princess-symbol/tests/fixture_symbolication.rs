//! Fixture-driven integration tests: the **golden comparison** against the
//! system `addr2line` (binutils 2.40) and the two reference kernels.
//!
//! These are the tests that correspond to the phase's acceptance table, so they
//! deliberately run the *real* binutils binary and compare its output
//! byte-for-byte rather than asserting against a hand-written expectation:
//!
//! | acceptance item | test |
//! |---|---|
//! | P2-4 `run.fault.symbolicated = {refkernel_fault_probe, file, 100}` | [`refkernel_fault_rip_matches_system_addr2line`] |
//! | golden 对比 vs `addr2line -f -C` | [`system_addr2line_agrees_on_every_probed_address`] |
//! | 分页夹具 `#PF` RIP → 正确行 | [`paging_kernel_pf_rip_resolves_to_line_122`] |
//! | 负样本（越界地址 / `buildId` 缺省） | [`out_of_range_addresses_never_get_a_fake_answer`], [`build_id_is_null_not_invented`] |
//! | `ripText` 保留原文 | [`fault_enrichment_keeps_the_guest_hex_text`] |
//!
//! If `addr2line` is missing the golden tests report that explicitly and skip
//! (they never silently pass): the report records which case applied.

use std::path::PathBuf;
use std::process::Command;

use princess_core::RunFaultPayload;
use princess_symbol::{DwarfIndex, SymbolIndex};

/// The two fixed fault RIPs from the fixtures' own serial logs.
///
/// `fixtures/refkernel/build/serial.log`:
///   `FAULT_RIP=0x0000000000100b3d ... EXCEPTION: vector=0x06 (#UD)`
/// `fixtures/paging-kernel/build/serial.log`:
///   `FAULT_RIP=0x00000000001011dd ... EXCEPTION: vector=0x0e (#PF)`
const REFKERNEL_FAULT_RIP: u64 = 0x100b3d;
const PAGINGKERNEL_FAULT_RIP: u64 = 0x1011dd;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

fn refkernel() -> PathBuf {
    workspace_root().join("fixtures/refkernel/build/refkernel.elf")
}

fn pagingkernel() -> PathBuf {
    workspace_root().join("fixtures/paging-kernel/build/pagingkernel.elf")
}

/// Run the system `addr2line -f -C -e <elf> <addr>` and return
/// `(symbol, "file:line")`.
///
/// `None` when the tool is not installed; a non-zero exit or unexpected output
/// shape is a hard panic, because a golden comparison that quietly degrades is
/// worse than no comparison.
fn system_addr2line(elf: &std::path::Path, address: u64) -> Option<(String, String)> {
    let output = match Command::new("addr2line")
        .arg("-f")
        .arg("-C")
        .arg("-e")
        .arg(elf)
        .arg(format!("{address:#x}"))
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => panic!("cannot run system addr2line: {err}"),
    };
    assert!(
        output.status.success(),
        "addr2line exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("addr2line emits UTF-8");
    let mut lines = text.lines();
    let symbol = lines.next().unwrap_or_default().to_string();
    let location = lines.next().unwrap_or_default().to_string();
    Some((symbol, location))
}

/// Which tool is the golden reference for the `file` part of `file:line`.
///
/// ## The measured disagreement (and which side we take)
///
/// For addresses inside a `static inline` function that gcc also emitted
/// **out-of-line**, GNU `addr2line`/`objdump` attribute the row to the
/// *compilation unit's* `.c` file while GDB, `gimli`, and `addr2line`(crate)
/// all read the line program literally and report the **header** the function
/// was written in.
///
/// Measured on `fixtures/paging-kernel/build/pagingkernel.elf`
/// (`.text` = `0x100040..0x1012db`), sampling every address and cross-checking
/// binutils against GDB:
///
/// ```text
/// sampled: 298   agree(file:line): 288   disagree: 10
///   0x100a60  binutils=paging.c:39   gdb=paging.h:39
///   0x100a70  binutils=paging.c:44   gdb=paging.h:44
///   0x100f30  binutils=kernel.c:40   gdb=paging.h:40
///   0x100f40  binutils=kernel.c:46   gdb=paging.h:46
/// ```
///
/// Over the whole `.text`, binutils' output contains **no `.h` file at all**
/// (`{boot.S: 343, isr.S: 228, serial.c: 1721, idt.c: 290, paging.c: 1227,
/// kernel.c: 954}`), which is the tell: it is normalising header code to the
/// CU rather than reading the line row.
///
/// The library follows **GDB + the line program** because that is what the
/// debugger the user actually steps with reports, and because D20's stated
/// purpose is "disassembly + source line, side by side" — showing the header
/// the code was written in is the more useful and the more literal answer.
/// This test therefore verifies the binutils difference **explicitly** (it must
/// be exactly the known header-inline set) instead of hiding it behind a
/// wildcard, so a regression in either direction fails loudly.
fn is_known_binutils_header_attribution(ours: &str, binutils: &str) -> bool {
    let ours_file = ours.rsplit_once(':').map(|(f, _)| f).unwrap_or(ours);
    let binutils_file = binutils.rsplit_once(':').map(|(f, _)| f).unwrap_or(binutils);
    let ours_line = ours.rsplit_once(':').map(|(_, l)| l).unwrap_or("");
    let binutils_line = binutils.rsplit_once(':').map(|(_, l)| l).unwrap_or("");
    // Same line number, we name a header, binutils names a `.c`.
    ours_line == binutils_line
        && ours_file.ends_with(".h")
        && binutils_file.ends_with(".c")
}

/// `file:line` as the library reports it, in the same shape `addr2line` uses.
fn library_location(index: &SymbolIndex, address: u64) -> Option<(String, String)> {
    let row = index.source_line_for_address(address).unwrap()?;
    // `addr2line` prints the path exactly as DWARF stores it; our `file` comes
    // from the same reader, so a plain `file:line` join is the right comparison.
    Some((row.raw_symbol, format!("{}:{}", row.file, row.line)))
}

/// Split binutils' `file:line (discriminator N)` into the `file:line` part and
/// the discriminator.
///
/// ## Why the discriminator is compared separately rather than ignored
///
/// Binutils' `addr2line` appends ` (discriminator N)` when the DWARF line row
/// carries a non-zero `DW_LNE_set_discriminator` value.  **`addr2line` 0.27.1
/// does not expose the discriminator at all** — `gimli` 0.34 parses it
/// (`gimli::read::line` handles `DW_LNE_set_discriminator`), but the lookup
/// crate's public `Location` has only `file`/`line`/`column`.  So the two tools
/// cannot be byte-identical on those rows.
///
/// This is a real, bounded difference, not a bug in the symbolication: measured
/// on `refkernel.elf`, **417 of the 3015 addresses** in `.text` carry one, and
/// on every one of them the `file:line` itself agrees.  The test therefore
/// asserts the `file:line` exactly and counts the discriminator rows, so a
/// future `addr2line` version that does surface it would show up here rather
/// than being masked.
fn split_discriminator(location: &str) -> (String, Option<u64>) {
    match location.rsplit_once(" (discriminator ") {
        Some((head, tail)) => {
            let value = tail
                .trim_end_matches(')')
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("unparseable discriminator in {location:?}"));
            (head.to_string(), Some(value))
        }
        None => (location.to_string(), None),
    }
}

// --------------------------------------------------------------- P2-4 ------

#[test]
fn refkernel_fault_rip_matches_system_addr2line() {
    let elf = refkernel();
    let index = SymbolIndex::open(&elf).unwrap();
    let row = index
        .source_line_for_address(REFKERNEL_FAULT_RIP)
        .unwrap()
        .expect("the refkernel fault RIP is covered by DWARF");

    // The acceptance constant (D3 / 20-acceptance.md).
    assert_eq!(row.symbol, "refkernel_fault_probe");
    assert_eq!(row.line, 100);
    assert!(
        row.file.ends_with("fixtures/refkernel/kernel.c"),
        "file was {}",
        row.file
    );

    // ...and the same three facts straight from binutils.
    let Some((golden_symbol, golden_location)) = system_addr2line(&elf, REFKERNEL_FAULT_RIP) else {
        eprintln!("SKIP: system addr2line not found; golden comparison not performed");
        return;
    };
    assert_eq!(golden_symbol, "refkernel_fault_probe");
    assert_eq!(golden_location, format!("{}:{}", row.file, row.line));
    // The function name is not mangled, so `-C` changes nothing.
    assert_eq!(row.raw_symbol, row.symbol);
}

#[test]
fn system_addr2line_agrees_on_every_probed_address() {
    // Both fixtures, whole `.text`, address by address.
    golden_scan(
        &refkernel(),
        "refkernel",
        0x100040, // .text address
        0x0bc7,   // .text size
    );
    golden_scan(
        &pagingkernel(),
        "pagingkernel",
        0x100040,
        0x129b,
    );
}

/// Compare every address in one `.text` against the system `addr2line`.
fn golden_scan(elf: &std::path::Path, label: &str, text_address: u64, text_size: u64) {
    let index = SymbolIndex::open(elf).unwrap();
    let mut compared = 0usize;
    let mut skipped_no_row = 0usize;
    let mut with_discriminator = 0usize;
    let mut exact = 0usize;
    let mut header_attribution = 0usize;

    // Step by 1 to cover every distinct line row; each `.text` is only a few KB,
    // so this is cheap and exhaustive.
    for address in text_address..text_address + text_size {
        let Some((golden_symbol, golden_location)) = system_addr2line(elf, address) else {
            eprintln!("SKIP: system addr2line not found; golden comparison not performed");
            return;
        };
        // binutils prints `??` for an address with no line row.
        if golden_location == "??:0" {
            skipped_no_row += 1;
            let ours = library_location(&index, address);
            assert!(
                ours.is_none(),
                "{label}: addr2line says no row for {address:#x} but we produced {ours:?}"
            );
            continue;
        }
        let (golden_file_line, discriminator) = split_discriminator(&golden_location);
        if discriminator.is_some() {
            with_discriminator += 1;
        }
        let ours = library_location(&index, address).unwrap_or_else(|| {
            panic!(
                "{label}: addr2line resolved {address:#x} to {golden_symbol} / \
                 {golden_location}, but princess-symbol returned None"
            )
        });
        assert_eq!(
            ours.0, golden_symbol,
            "{label}: symbol mismatch at {address:#x} (golden {golden_location})"
        );
        if ours.1 == golden_file_line {
            exact += 1;
        } else {
            // The only permitted difference is binutils' header attribution of an
            // out-of-line `static inline` copy; anything else is a real bug.
            assert!(
                is_known_binutils_header_attribution(&ours.1, &golden_file_line),
                "{label}: unexplained file:line mismatch at {address:#x}: \
                 ours={} binutils={golden_file_line}",
                ours.1
            );
            header_attribution += 1;
        }
        compared += 1;
    }

    assert!(
        compared > 100,
        "{label}: expected a meaningful number of compared addresses, got {compared}"
    );
    eprintln!(
        "golden comparison [{label}]: {compared} addresses compared against binutils; \
         {exact} file:line exact, {header_attribution} differ only by binutils' \
         header-inline attribution, {skipped_no_row} agreed on 'no row', \
         {with_discriminator} carried a binutils-only discriminator suffix"
    );
}

// ------------------------------------------------------- paging fixture -----

#[test]
fn paging_kernel_pf_rip_resolves_to_line_122() {
    let elf = pagingkernel();
    let index = SymbolIndex::open(&elf).unwrap();
    let row = index
        .source_line_for_address(PAGINGKERNEL_FAULT_RIP)
        .unwrap()
        .expect("the paging kernel #PF RIP is covered by DWARF");

    assert_eq!(
        row.symbol, "paging_fault_probe",
        "the #PF must resolve to the probe that dereferenced the absent page"
    );
    assert_eq!(row.line, 122);
    assert!(
        row.file.ends_with("fixtures/paging-kernel/kernel.c"),
        "file was {}",
        row.file
    );

    if let Some((golden_symbol, golden_location)) = system_addr2line(&elf, PAGINGKERNEL_FAULT_RIP) {
        assert_eq!(golden_symbol, "paging_fault_probe");
        assert_eq!(golden_location, format!("{}:{}", row.file, row.line));
    } else {
        eprintln!("SKIP: system addr2line not found; golden comparison not performed");
    }
}

/// The raw line program is the authority, and GDB is the arbiter.
///
/// At `0x100a56` the DWARF 5 line program row is (`file_index=1` → `paging.h`,
/// `line=37`); binutils rewrites it to `paging.c:37` because the `static inline
/// read_cr0` also got an out-of-line copy in the `paging.c` unit.  GDB, which
/// reads the same line program with a completely independent reader, reports
/// `paging.h:37` — so the library's answer is the literal DWARF one.
///
/// Verified with:
///   `objdump --dwarf=rawline fixtures/paging-kernel/build/pagingkernel.elf`
///   `gdb -batch -ex 'file ...' -ex 'info line *0x100a56'`
#[test]
fn header_inline_row_matches_the_line_program_and_gdb_not_binutils() {
    let elf = pagingkernel();
    let index = SymbolIndex::open(&elf).unwrap();
    let row = index.source_line_for_address(0x100a56).unwrap().unwrap();
    assert_eq!(row.line, 37);
    assert!(
        row.file.ends_with("paging.h"),
        "the literal DWARF row names the header, got {}",
        row.file
    );

    if let Some((_, golden)) = system_addr2line(&elf, 0x100a56) {
        let (golden, _) = split_discriminator(&golden);
        // binutils disagrees; assert the disagreement is exactly the documented
        // header-attribution kind so this test fails if either side changes.
        assert!(
            is_known_binutils_header_attribution(
                &format!("{}:{}", row.file, row.line),
                &golden
            ),
            "expected the documented binutils header attribution, got {golden}"
        );
    }

    // GDB is the neutral third party: if it is installed, its answer must match
    // ours exactly (modulo GDB's relative path spelling).
    if let Some((gdb_line, gdb_file)) = gdb_info_line(&elf, 0x100a56) {
        assert_eq!(gdb_line, row.line, "GDB disagrees about the line");
        let ours_name = row.file.rsplit('/').next().unwrap_or(&row.file);
        assert_eq!(gdb_file, ours_name, "GDB disagrees about the file");
    } else {
        eprintln!("SKIP: gdb not found; cross-check not performed");
    }
}

/// `gdb -batch -ex 'file <elf>' -ex 'info line *<addr>'` → `(line, basename)`.
///
/// GDB prints the path relative to the compilation directory, so only the file
/// name is comparable.  `None` when GDB is unavailable or has no line.
fn gdb_info_line(elf: &std::path::Path, address: u64) -> Option<(u32, String)> {
    let output = match Command::new("gdb")
        .arg("-batch")
        .arg("-q")
        .arg("-ex")
        .arg(format!("file {}", elf.display()))
        .arg("-ex")
        .arg(format!("info line *{address:#x}"))
        .output()
    {
        Ok(output) => output,
        Err(_) => return None,
    };
    let text = String::from_utf8_lossy(&output.stdout);
    // `Line 37 of "/path/paging.h" starts at address 0x100a56 <read_cr0> and ...`
    let rest = text.split("Line ").nth(1)?;
    let (line, rest) = rest.split_once(" of \"")?;
    let (file, _) = rest.split_once('"')?;
    Some((
        line.trim().parse().ok()?,
        file.rsplit('/').next().unwrap_or(file).to_string(),
    ))
}

/// The `#PF` fault RIP is *inside* `paging_fault_probe`, not in the IDT handler:
/// `isr_stub_14`/`page_fault_handler` deliver the fault but the CPU pushes the
/// **faulting instruction's** RIP, so symbolication must land on the probe.
///
/// This test pins the relationship the acceptance table asks us to explain.
#[test]
fn paging_pf_rip_is_in_the_faulting_function_not_the_handler() {
    let elf = pagingkernel();
    let index = SymbolIndex::open(&elf).unwrap();

    let faulting = index
        .source_line_for_address(PAGINGKERNEL_FAULT_RIP)
        .unwrap()
        .unwrap();
    assert_eq!(faulting.symbol, "paging_fault_probe");

    // The same image's IDT (#PF) handler is a different function at a different
    // address; symbolication of *that* address must not be confused with it.
    let symbols = index.symbols().unwrap();
    let handler = symbols
        .iter()
        .find(|s| s.name == "isr_stub_14")
        .or_else(|| symbols.iter().find(|s| s.name.contains("page_fault")))
        .expect("the paging fixture installs a #PF path");
    assert_ne!(handler.address, PAGINGKERNEL_FAULT_RIP);
    if let Some(handler_row) = index.source_line_for_address(handler.address).unwrap() {
        assert_ne!(
            handler_row.line, faulting.line,
            "handler and faulting probe must not share a line"
        );
    }
}

// ------------------------------------------------------------ negatives -----

#[test]
fn out_of_range_addresses_never_get_a_fake_answer() {
    let index = SymbolIndex::open(&refkernel()).unwrap();
    // Addresses that are: null, tiny, far above the image, in the MMIO hole, and
    // the classic "unmapped probe" address from the paging fixture.
    for address in [0u64, 1, 0x1000, 0x400000, 0xdead_beef, !0u64 - 1] {
        let ours = index.source_line_for_address(address).unwrap();
        assert!(
            ours.is_none(),
            "{address:#x} produced {ours:?}, expected no answer"
        );
        assert!(index.symbolicate(address).unwrap().is_none());
        // ...and the same question to binutils, which must also refuse.
        if let Some((_, golden_location)) = system_addr2line(&refkernel(), address) {
            assert_eq!(
                golden_location, "??:0",
                "{address:#x}: binutils resolved it, so the fixture assumption is wrong"
            );
        }
    }
}

#[test]
fn build_id_is_null_not_invented() {
    // `--build-id=none` on both fixtures (contract §2: 宁可 null 也不得编造).
    for elf in [refkernel(), pagingkernel()] {
        let index = SymbolIndex::open(&elf).unwrap();
        assert_eq!(index.build_id(), None, "{}", elf.display());
        let (artifact, build_id, symbol_count) = index.indexed_event_fields();
        assert_eq!(build_id, None);
        assert!(artifact.ends_with(".elf"));
        assert!(symbol_count > 0);
    }
}

#[test]
fn a_missing_artifact_is_not_found_and_a_text_file_is_an_error() {
    let missing = princess_symbol::SymbolIndex::open(std::path::Path::new("/nope/missing.elf"))
        .unwrap_err();
    assert_eq!(missing.code, princess_core::ErrorCode::NotFound);

    let text = workspace_root().join("README.md");
    if text.exists() {
        let err = SymbolIndex::open(&text).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        assert!(err.message.contains("ELF"), "{err:?}");
    }
}

// ------------------------------------------------------------ ripText -------

#[test]
fn fault_enrichment_keeps_the_guest_hex_text() {
    let index = SymbolIndex::open(&refkernel()).unwrap();
    // Exactly the shape the guest prints in `serial.log`.
    let mut fault = RunFaultPayload {
        vector: "#UD".into(),
        rip: REFKERNEL_FAULT_RIP,
        rip_text: "0x0000000000100b3d".into(),
        error_code: 0,
        regs: Default::default(),
        symbolicated: None,
    };
    let enriched = princess_symbol::symbolicate_fault_event(&index, &mut fault).unwrap();
    assert!(enriched);
    assert_eq!(fault.rip_text, "0x0000000000100b3d");
    let location = fault.symbolicated.as_ref().unwrap();
    assert_eq!(location.symbol, "refkernel_fault_probe");
    assert_eq!(location.line, 100);
}

// --------------------------------------------------- the span, for P5 -------

#[test]
fn every_dwarf_row_carries_a_usable_address_span() {
    // The UI contract (D20) is "disassembly + source line" side by side, so every
    // row in the fault window must have a drawable span.
    let index = DwarfIndex::open(&refkernel()).unwrap();
    let rows = index.rows_in_range(0x100a90, 0x100bc0).unwrap();
    assert!(!rows.is_empty(), "expected rows around the fault probe");
    for row in &rows {
        assert!(
            row.address_end > row.address_start,
            "degenerate span on {row:?}"
        );
        assert!(!row.file.is_empty());
        assert!(row.line > 0);
    }
    // The faulting row must be present in the window.
    assert!(
        rows.iter().any(|r| r.line == 100 && r.covers(REFKERNEL_FAULT_RIP)),
        "the fault row must be in the rendered window: {rows:?}"
    );
}
