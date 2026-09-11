//! P5 acceptance driver: one command per acceptance item, printing raw output.
//!
//! Deliberately an *example* rather than a second binary target: it depends only
//! on the public API of `princess-bin`, so what it exercises is exactly what a
//! consumer (the Tauri IPC layer) can reach.  Every subcommand prints plain lines
//! that the acceptance report quotes verbatim; nothing is summarised.
//!
//! Usage: `cargo run -p princess-bin --example p5check -- <subcommand> [args]`

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use princess_bin::{
    descriptor::{self, Gdt, Idt, Tss64},
    disasm::{Disassembler, DisassemblerOptions, Syntax},
    elf, guestmem::GuestRam, monitor, pagetable, source::LineTable, HexReader, HexWindowOptions,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name)
}

fn refkernel() -> PathBuf {
    fixture("refkernel/build/refkernel.elf")
}

fn pagingkernel() -> PathBuf {
    fixture("paging-kernel/build/pagingkernel.elf")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("elf") => cmd_elf(&args),
        Some("disasm") => cmd_disasm(&args),
        Some("hex") => cmd_hex(&args),
        Some("monitor") => cmd_monitor(&args),
        Some("pagetable") => cmd_pagetable(&args),
        Some("descriptor") => cmd_descriptor(&args),
        Some("lines") => cmd_lines(&args),
        other => {
            eprintln!("unknown subcommand {other:?}");
            eprintln!("known: elf disasm hex monitor pagetable descriptor lines");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ERROR: {err}");
            ExitCode::FAILURE
        }
    }
}

/// P5-1 — sections/symbols/entry, in a format `readelf`-comparable form.
fn cmd_elf(args: &[String]) -> Result<(), String> {
    let path = args.get(1).map(PathBuf::from).unwrap_or_else(refkernel);
    let header = elf::read_header(&path).map_err(|e| e.to_string())?;
    println!("path={}", header.path.display());
    println!("format={}", header.format);
    println!("kind={}", header.kind);
    println!("architecture={}", header.architecture);
    println!("endianness={}", header.endianness);
    println!("class={}", header.class);
    println!("entry={:#x}", header.entry_point);
    println!("section_count={}", header.section_count);
    println!("segment_count={}", header.segment_count);
    println!(
        "build_id={}",
        header.build_id.as_deref().unwrap_or("null")
    );
    println!("--- sections (name|address|size|flags) ---");
    for section in elf::read_sections(&path).map_err(|e| e.to_string())? {
        println!(
            "{}|{:#x}|{:#x}|{}",
            section.name, section.address, section.size, section.flags
        );
    }
    println!("--- segments (kind|off|vaddr|filesz|memsz|flags|align) ---");
    for segment in elf::read_segments(&path).map_err(|e| e.to_string())? {
        println!(
            "{}|{:#x}|{:#x}|{:#x}|{:#x}|{}|{:#x}",
            segment.kind,
            segment.file_offset,
            segment.virtual_address,
            segment.file_size,
            segment.memory_size,
            segment.flags,
            segment.alignment
        );
    }
    println!("--- symbols (name|value|size|kind) ---");
    for symbol in elf::read_symbols(&path).map_err(|e| e.to_string())? {
        println!(
            "{}|{:#x}|{:#x}|{}",
            symbol.name, symbol.address, symbol.size, symbol.kind
        );
    }
    Ok(())
}

/// P5-2 — disassembly with the paired source rows.
fn cmd_disasm(args: &[String]) -> Result<(), String> {
    let address = args
        .get(1)
        .map(|text| u64::from_str_radix(text.trim_start_matches("0x"), 16))
        .transpose()
        .map_err(|e| format!("bad address: {e}"))?
        .unwrap_or(0x10_0b39);
    let count: u64 = args.get(2).and_then(|t| t.parse().ok()).unwrap_or(20);
    let syntax = args.get(3).map(String::as_str).unwrap_or("intel");
    let syntax = match syntax {
        "gas" => Syntax::Gas,
        "nasm" => Syntax::Nasm,
        "objdump" => Syntax::ObjdumpIntel,
        _ => Syntax::Intel,
    };
    let mut disassembler = Disassembler::open(
        &refkernel(),
        DisassemblerOptions {
            syntax,
            ..DisassemblerOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let view = disassembler
        .with_source(address, count)
        .map_err(|e| e.to_string())?;
    println!("requested={:#x}", view.requested_address);
    println!("start={:#x}", view.start_address);
    println!("--- instructions (address|bytes|symbol|text) ---");
    for instruction in &view.instructions {
        println!(
            "{}|{}|{}|{}",
            instruction.address,
            instruction.bytes,
            instruction.symbol.as_deref().unwrap_or("-"),
            instruction.text
        );
    }
    println!("--- source rows (addr_start|addr_end|file:line|col|instrs) ---");
    for row in &view.source_rows {
        println!(
            "{:#x}|{:#x}|{}:{}|{}|{}",
            row.addresses.start,
            row.addresses.end,
            row.addresses.file,
            row.addresses.line,
            row.addresses
                .column
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string()),
            row.instruction_count
        );
    }
    Ok(())
}

/// P5-3 — hex reading, including a bounded-memory stress run.
fn cmd_hex(args: &[String]) -> Result<(), String> {
    let sub = args.get(1).map(String::as_str).unwrap_or("lines");
    match sub {
        "lines" => {
            let path = args.get(2).map(PathBuf::from).unwrap_or_else(refkernel);
            let first: u64 = args.get(3).and_then(|t| t.parse().ok()).unwrap_or(0);
            let count: u64 = args.get(4).and_then(|t| t.parse().ok()).unwrap_or(8);
            let mut reader = HexReader::open(&path).map_err(|e| e.to_string())?;
            println!("path={}", path.display());
            println!("len={}", reader.len());
            println!("byte_bound={}", reader.byte_bound());
            for line in reader.read_lines(first, count).map_err(|e| e.to_string())? {
                println!("{:08x}  {:<47}  |{}|", line.offset, line.hex, line.ascii);
            }
            let stats = reader.stats();
            println!(
                "syscalls={} hits={} misses={} resident_bytes={} bytes_read={} bytes_served={}",
                stats.syscalls,
                stats.hits,
                stats.misses,
                stats.resident_bytes,
                stats.bytes_read,
                stats.bytes_served
            );
        }
        "boundary" => {
            // Cross-boundary correctness against the file's own bytes.
            let path = args.get(2).map(PathBuf::from).unwrap_or_else(refkernel);
            let window: usize = args.get(3).and_then(|t| t.parse().ok()).unwrap_or(4096);
            let mut reader = HexReader::with_options(
                &path,
                HexWindowOptions {
                    window_size: window,
                    capacity: 4,
                },
            )
            .map_err(|e| e.to_string())?;
            // Every byte read singly must reconstruct the file exactly.
            let mut collected = Vec::new();
            for offset in 0..reader.len() {
                collected.extend_from_slice(&reader.read_range(offset, 1).map_err(|e| e.to_string())?);
            }
            let whole = std::fs::read(&path).map_err(|e| e.to_string())?;
            println!("file_len={}", reader.len());
            println!("reconstructed_len={}", collected.len());
            println!("byte_exact={}", collected == whole);
            // Straddling reads at every window boundary.
            let mut straddle_ok = true;
            let mut position = window as u64;
            while position + 64 <= reader.len() {
                let expect = &whole[position as usize..position as usize + 64];
                let got = reader.read_range(position - 32, 64).map_err(|e| e.to_string())?;
                let expected = &whole[position as usize - 32..position as usize + 32];
                if got != expected {
                    straddle_ok = false;
                    println!("MISMATCH at {position:#x}: got {got:02x?} want {expected:02x?}");
                }
                let _ = expect;
                position += window as u64;
            }
            println!("straddle_ok={straddle_ok}");
            // Past the end is a short read, not an error.
            let tail = reader.read_range(reader.len(), 16).map_err(|e| e.to_string())?;
            println!("past_eof_len={}", tail.len());
        }
        "stress" => {
            // The research report's own workload: 20 000 random 4 KiB reads over
            // a large image, with the RSS bound asserted from the reader's own
            // accounting.
            let path = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
                std::env::temp_dir().join("princess-bin-hex").join("p5check-sparse.img")
            });
            let size_gib: u64 = args.get(3).and_then(|t| t.parse().ok()).unwrap_or(8);
            let iterations: u64 = args.get(4).and_then(|t| t.parse().ok()).unwrap_or(20_000);
            if !path.exists() {
                std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
                let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                file.set_len(size_gib * 1024 * 1024 * 1024)
                    .map_err(|e| e.to_string())?;
            }
            let options = HexWindowOptions::default();
            let mut reader = HexReader::with_options(&path, options).map_err(|e| e.to_string())?;
            println!("path={}", path.display());
            println!("image_bytes={}", reader.len());
            println!(
                "window_size={} capacity={} byte_bound={}",
                options.window_size,
                options.capacity,
                options.byte_bound()
            );
            let rss_before = resident_kb();
            println!("rss_before_kb={rss_before}");
            let start = std::time::Instant::now();
            let mut state: u64 = 0x2545_f491_4f6c_dd1d;
            let mut total = 0u64;
            for _ in 0..iterations {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let offset = state % (reader.len() - 4096);
                let bytes = reader.read_range(offset, 4096).map_err(|e| e.to_string())?;
                total += bytes.len() as u64;
            }
            let elapsed = start.elapsed();
            let rss_after = resident_kb();
            let stats = reader.stats();
            println!("iterations={iterations}");
            println!("bytes_served={total}");
            println!("elapsed_ms={}", elapsed.as_millis());
            println!(
                "per_access_us={:.1}",
                elapsed.as_micros() as f64 / iterations as f64
            );
            println!("rss_after_kb={rss_after}");
            println!("rss_delta_kb={}", rss_after.saturating_sub(rss_before));
            println!(
                "resident_bytes={} bound={} within_bound={}",
                stats.resident_bytes,
                options.byte_bound(),
                stats.resident_bytes <= options.byte_bound()
            );
            println!("pread_syscalls={}", stats.syscalls);
        }
        other => return Err(format!("unknown hex subcommand {other:?}")),
    }
    Ok(())
}

fn resident_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

/// P5-4 — parse every recorded sample and print the decoded structure.
fn cmd_monitor(_args: &[String]) -> Result<(), String> {
    let dir = fixture("qemu-monitor");
    let read = |name: &str| -> Result<String, String> {
        std::fs::read_to_string(dir.join(name))
            .map_err(|e| format!("{}: {e}", dir.join(name).display()))
    };
    let registers = read("info-registers.txt")?;
    let mem = read("info-mem.txt")?;
    let tlb = read("info-tlb.txt")?;
    let cpus = read("info-cpus.txt")?;

    let report = monitor::parse_info_registers(&registers).map_err(|e| e.to_string())?;
    println!("--- info registers ---");
    println!("cpu={:?}", report.cpu);
    println!("CR0={:#x}", report.register_u64("CR0").unwrap());
    println!("CR2={:#x}", report.register_u64("CR2").unwrap());
    println!("CR3={:#x}", report.register_u64("CR3").unwrap());
    println!("CR4={:#x}", report.register_u64("CR4").unwrap());
    println!("RIP={:#x}", report.register_u64("RIP").unwrap());
    println!("EFER={:#x}", report.register_u64("EFER").unwrap());
    println!("GDT_base={:#x} GDT_limit={:#x}", report.gdt_base.unwrap(), report.gdt_limit.unwrap());
    println!("GDT_slots={}", report.gdt_entry_count().unwrap());
    println!("IDT_base={:#x} IDT_limit={:#x}", report.idt_base.unwrap(), report.idt_limit.unwrap());
    println!("IDT_gates={}", report.idt_entry_count().unwrap());
    println!("halted={:?} cpl={:?}", report.halted, report.cpl);
    for segment in &report.segments {
        println!(
            "seg {} sel={:#06x} base={:#x} limit={:#x} access={:#010x} DPL={} type={} perms={}",
            segment.name,
            segment.selector,
            segment.base,
            segment.limit,
            segment.access,
            segment.dpl,
            segment.type_name,
            segment.permissions.as_deref().unwrap_or("-")
        );
    }

    println!("--- info mem ---");
    let ranges = monitor::parse_info_mem(&mem).map_err(|e| e.to_string())?;
    for range in &ranges {
        println!(
            "{:016x}-{:016x} {:016x} u={} r={} w={}",
            range.start, range.end, range.size, range.user as u8, range.readable as u8, range.writable as u8
        );
    }
    println!("range_count={}", ranges.len());

    println!("--- info tlb ---");
    let entries = monitor::parse_info_tlb(&tlb).map_err(|e| e.to_string())?;
    let granularities = monitor::resolve_granularities(&entries);
    println!("entry_count={}", entries.len());
    let ps_count = entries.iter().filter(|e| e.flags.ps).count();
    println!("ps_set_count={ps_count}");
    println!("ps_clear_count={}", entries.len() - ps_count);
    for index in [0usize, 511, 512, 1020] {
        let entry = entries[index];
        let granularity = granularities[index];
        println!(
            "tlb[{index}] va={:016x} pa={:016x} flags(X={} G={} PS={} D={} A={} C={} T={} U={} W={}) granularity={:?}",
            entry.virtual_address,
            entry.physical_address,
            entry.flags.nx as u8,
            entry.flags.global as u8,
            entry.flags.ps as u8,
            entry.flags.dirty as u8,
            entry.flags.accessed as u8,
            entry.flags.pcd as u8,
            entry.flags.pwt as u8,
            entry.flags.user as u8,
            entry.flags.writable as u8,
            granularity
        );
    }
    // The 4 KiB leaves print `--------W`; if `P` meant present they would all
    // look absent.  State the two populations explicitly.
    let four_k_ps_set = entries
        .iter()
        .filter(|e| e.physical_address % 0x200000 != 0 && e.flags.ps)
        .count();
    println!("4k_entries_with_ps_set={four_k_ps_set} (must be 0)");

    println!("--- info cpus ---");
    for cpu in monitor::parse_info_cpus(&cpus).map_err(|e| e.to_string())? {
        println!("cpu index={} thread_id={} current={}", cpu.index, cpu.thread_id, cpu.current);
    }

    println!("--- QMP ---");
    let version = monitor::parse_qmp_version(&read("qmp-query-version.json")?).map_err(|e| e.to_string())?;
    println!("qemu_version={}", version.version_string());
    let status = monitor::parse_qmp_status(&read("qmp-query-status.json")?).map_err(|e| e.to_string())?;
    println!("status={} running={:?}", status.status, status.running);
    for cpu in monitor::parse_qmp_cpus_fast(&read("qmp-query-cpus-fast.json")?).map_err(|e| e.to_string())? {
        println!("qmp_cpu index={} thread_id={} target={:?}", cpu.cpu_index, cpu.thread_id, cpu.target);
    }
    let memory = monitor::parse_qmp_memory_summary(&read("qmp-query-memory-size-summary.json")?)
        .map_err(|e| e.to_string())?;
    println!(
        "base_memory={} total={} contains_0x104000={}",
        memory.base_memory,
        memory.total(),
        memory.contains_physical(0x10_4000)
    );
    Ok(())
}

/// P5-5 — walk the page tables from CR3 using the recorded register state.
fn cmd_pagetable(args: &[String]) -> Result<(), String> {
    let address: u64 = args
        .get(1)
        .map(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16))
        .transpose()
        .map_err(|e| format!("bad address: {e}"))?
        .unwrap_or(0x1234);

    let registers_text = std::fs::read_to_string(fixture("qemu-monitor/info-registers.txt"))
        .map_err(|e| e.to_string())?;
    let report = monitor::parse_info_registers(&registers_text).map_err(|e| e.to_string())?;

    // Build the guest's page tables out of the fixture's *own* ground truth:
    // the paging kernel prints every entry it programmed on the serial line.
    let ram = paging_fixture_ram()?;
    let cr0 = pagetable::Cr0(report.register_u64("CR0").unwrap());
    let cr3 = pagetable::Cr3(report.register_u64("CR3").unwrap());
    let cr4 = pagetable::Cr4(report.register_u64("CR4").unwrap());
    let efer = pagetable::Efer(report.register_u64("EFER").unwrap());

    println!("CR0={:#018x} PG={}", cr0.0, cr0.paging_enabled());
    println!("CR3={:#018x} table={:#x} pcid={:#x}", cr3.0, cr3.table_address(), cr3.pcid());
    println!("CR4={:#018x} PAE={} LA57={}", cr4.0, cr4.pae(), cr4.la57());
    println!("EFER={:#018x} LME={} LMA={} NXE={}", efer.0, efer.lme(), efer.lma(), efer.nxe());

    // Two hard-coded outputs from the guest's own serial log, to make the
    // ground-truth pairing explicit rather than implied.
    println!("--- guest serial ground truth (capture-serial.log) ---");
    println!("PML4[0]=0x0000000000105023 PDPT[0]=0x0000000000106023 PD[0]=0x0000000000107023");
    println!("PD[1]=0x0000000000200083 PD[2]=0x0000000000000000 PT[0]=0x0000000000000003");
    println!("PT[510]=0x00000000001fe003 PT[511]=0x0000000000000000");

    let walker = pagetable::Walker::new(&ram, cr0, cr4, efer).map_err(|e| e.to_string())?;
    println!("--- walk {address:#x} ---");
    match walker.walk(cr3, address) {
        Ok(walk) => {
            println!("result=present");
            for level in &walk.levels {
                println!(
                    "{}[{}] table={:#x} entry_addr={:#x} raw={:#018x} P={} RW={} US={} PWT={} PCD={} A={} D={} PS={} G={} AVL={} addr={:#x} PK={} NX={}",
                    level.level.name(),
                    level.index,
                    level.table_address,
                    level.entry_address,
                    level.entry.raw,
                    level.entry.present as u8,
                    level.entry.writable as u8,
                    level.entry.user as u8,
                    level.entry.pwt as u8,
                    level.entry.pcd as u8,
                    level.entry.accessed as u8,
                    level.entry.dirty as u8,
                    level.entry.page_size as u8,
                    level.entry.global as u8,
                    level.entry.available,
                    level.entry.address_bits,
                    level.entry.protection_key,
                    level.entry.no_execute as u8
                );
            }
            println!("path={}", walk.path_summary());
            println!("leaf_size={:?} bytes={}", walk.leaf_size, walk.leaf_size.bytes());
            println!("physical_base={:#x} offset={:#x} physical={:#x}", walk.physical_base, walk.page_offset, walk.physical_address());
            println!("virtual_range={:#x}..{:#x}", walk.virtual_range().0, walk.virtual_range().1);
            println!("physical_range={:#x}..{:#x}", walk.physical_range().0, walk.physical_range().1);
            println!("effective user={} writable={} executable={}", walk.effective_user(), walk.effective_writable(), walk.executable());
        }
        Err(err) => {
            println!("result=error");
            println!("error={err}");
            // A not-present stop is a *finding*, so print the resolved path too.
            if let Ok(explained) = walker.walk_or_explain(cr3, address) {
                if let Err(not_present) = explained {
                    for level in &not_present.levels {
                        println!(
                            "resolved {}[{}] raw={:#018x} P={}",
                            level.level.name(), level.index, level.entry.raw, level.entry.present as u8
                        );
                    }
                    println!(
                        "absent {}[{}] entry_addr={:#x}",
                        not_present.level.name(), not_present.index, not_present.entry_address
                    );
                }
            }
        }
    }
    Ok(())
}

/// The paging fixture's page tables, transcribed from the guest's own serial
/// output (`fixtures/qemu-monitor/capture-serial.log`) and cross-checked against
/// the ELF's symbol table.
fn paging_fixture_ram() -> Result<GuestRam, String> {
    let mut ram = GuestRam::new(256 * 1024 * 1024);
    let mut set = |address: u64, value: u64| -> Result<(), String> {
        ram.write_u64(address, value).map_err(|e| e.to_string())
    };
    set(0x10_4000, 0x10_5023)?; // PML4[0] -> PDPT
    set(0x10_5000, 0x10_6023)?; // PDPT[0] -> PD
    set(0x10_6000, 0x10_7023)?; // PD[0] -> PT
    set(0x10_6008, 0x0020_0083)?; // PD[1] 2 MiB huge
    set(0x10_6010, 0x0)?; // PD[2] absent
    for index in 3..512u64 {
        set(0x10_6000 + index * 8, (index << 21) | 0x83)?;
    }
    for index in 0..511u64 {
        set(0x10_7000 + index * 8, (index << 12) | 0x3)?;
    }
    set(0x10_7000 + 511 * 8, 0)?; // PT[511] absent
    Ok(ram)
}

/// The GDT/IDT half of P5-5.
fn cmd_descriptor(_args: &[String]) -> Result<(), String> {
    let text = std::fs::read_to_string(fixture("qemu-monitor/info-registers.txt"))
        .map_err(|e| e.to_string())?;
    let report = monitor::parse_info_registers(&text).map_err(|e| e.to_string())?;
    let gdt_base = report.gdt_base.ok_or("no GDT= line")?;
    let gdt_limit = report.gdt_limit.ok_or("no GDT limit")?;
    let idt_base = report.idt_base.ok_or("no IDT= line")?;
    let idt_limit = report.idt_limit.ok_or("no IDT limit")?;
    println!("GDT base={gdt_base:#x} limit={gdt_limit:#x}");
    println!("IDT base={idt_base:#x} limit={idt_limit:#x}");

    // The paging fixture's GDT words, from its serial log.
    let mut ram = GuestRam::new(256 * 1024 * 1024);
    let words: [u64; 5] = [
        0x0000_0000_0000_0000,
        0x00af_9a00_0000_ffff,
        0x00cf_9300_0000_ffff,
        0x00cf_9a00_0000_ffff,
        0x00cf_9300_0000_ffff,
    ];
    for (index, word) in words.iter().enumerate() {
        ram.write_u64(0x10_12e0 + index as u64 * 8, *word)
            .map_err(|e| e.to_string())?;
    }
    // IDT: the fixture installs only #PF (vector 14), handler 0x1001f6, sel 8,
    // ist 0, type 0x8e.
    let handler = 0x10_01f6u64;
    let mut gate = [0u8; 16];
    gate[0..2].copy_from_slice(&(handler as u16).to_le_bytes());
    gate[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
    gate[5] = 0x8e;
    gate[6..8].copy_from_slice(&((handler >> 16) as u16).to_le_bytes());
    gate[8..12].copy_from_slice(&((handler >> 32) as u32).to_le_bytes());
    ram.write_bytes(0x10_8000 + 14 * 16, &gate)
        .map_err(|e| e.to_string())?;

    let (gdt, idt, tss) = descriptor::read_tables(&ram, gdt_base, gdt_limit, idt_base, idt_limit)
        .map_err(|e| e.to_string())?;
    println!("gdt_slots={}", gdt.slot_count());
    for entry in &gdt.entries {
        match entry {
            descriptor::GdtEntry::Null { offset } => println!("GDT[{offset:#06x}] null"),
            descriptor::GdtEntry::Segment(d) => println!(
                "GDT[{:#06x}] {} raw={:#018x} base={:#x} limit={:#x} DPL={} P={} A={} L={} D/B={} G={}",
                d.offset, d.label(), d.raw, d.base, d.limit, d.dpl, d.present as u8,
                d.accessed as u8, d.long_mode as u8, d.default_size_32 as u8, d.granularity_4k as u8
            ),
            descriptor::GdtEntry::System(d) => println!(
                "GDT[{:#06x}] {} raw_low={:#018x} base={:#x} limit={:#x} DPL={} slots={} busy={}",
                d.offset, d.kind.label(), d.raw_low, d.base, d.limit, d.dpl, d.slot_count(), d.is_busy()
            ),
            descriptor::GdtEntry::HighHalf { offset, raw, low_half_offset } => println!(
                "GDT[{offset:#06x}] high-half of GDT[{low_half_offset:#06x}] raw={raw:#018x}"
            ),
        }
    }
    println!("tss={}", if tss.is_some() { "present" } else { "none" });
    println!("idt_gates={}", idt.gate_count());
    for gate in idt.gates.iter().filter(|g| g.present) {
        println!(
            "IDT[{}] handler={:#018x} sel={:#06x} ist={} type={:?} DPL={} selector_resolves_to={}",
            gate.vector, gate.handler, gate.selector, gate.ist, gate.kind, gate.dpl,
            descriptor::describe_selector(&gdt, gate.selector).unwrap_or_else(|| "?".to_string())
        );
    }
    println!("installed_vectors={:?}", idt.installed_vectors());
    println!("user_accessible_vectors={:?}", idt.user_accessible_vectors());
    let _ = (Gdt::read, Idt::read, Tss64::read);
    Ok(())
}

/// Address-range -> line-range evidence for the side-by-side view.
fn cmd_lines(args: &[String]) -> Result<(), String> {
    let address = args
        .get(1)
        .map(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16))
        .transpose()
        .map_err(|e| format!("bad address: {e}"))?
        .unwrap_or(0x10_0b39);
    let path = args.get(2).map(PathBuf::from).unwrap_or_else(refkernel);
    let table = LineTable::open(&path).map_err(|e| e.to_string())?;
    println!("artifact={}", path.display());
    println!("loaded_before_first_query={}", table.is_loaded());
    let range = table
        .line_range_for_address(address)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no line info for {address:#x}"))?;
    println!("loaded_after_first_query={}", table.is_loaded());
    println!("range_start={:#x}", range.start);
    println!("range_end={:#x}", range.end);
    println!("range_len={}", range.len());
    println!("file={}", range.file);
    println!("line={}", range.line);
    println!("column={:?}", range.column);
    let segments = table.segments().map_err(|e| e.to_string())?;
    println!("segment_count={}", segments.len());
    for segment in segments.iter().take(8) {
        println!(
            "{:#x}..{:#x} {}:{}",
            segment.start, segment.end, segment.file, segment.line
        );
    }
    Ok(())
}
