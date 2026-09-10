//! A tiny demonstration of the engine's symbolication path, used to capture the
//! raw acceptance evidence in `docs/reports/p2-symbol.md`.
//!
//! It is deliberately *not* a test: it prints exactly what the engine will emit
//! so the report can quote real command output rather than a test assertion.
//!
//! ```text
//! cargo run -p princess-symbol --example symbolicate -- \
//!     fixtures/refkernel/build/refkernel.elf 0x100b3d
//! ```

use std::path::PathBuf;

use princess_core::RunFaultPayload;
use princess_symbol::{SymbolIndex, SYMBOL_STACK};

fn main() {
    let mut args = std::env::args().skip(1);
    let artifact = match args.next() {
        Some(path) => PathBuf::from(path),
        None => {
            eprintln!("usage: symbolicate <artifact.elf> <address-hex> [address-hex ...]");
            std::process::exit(2);
        }
    };

    let index = match SymbolIndex::open(&artifact) {
        Ok(index) => index,
        Err(err) => {
            // Fail loud, with the error code the IPC layer would return.
            eprintln!("error [{}]: {}", err.code.as_str(), err.message);
            if let Some(detail) = &err.detail {
                eprintln!("detail: {detail}");
            }
            std::process::exit(1);
        }
    };

    println!("artifact      : {}", index.path().display());
    println!("format        : {}", index.facts().format);
    println!("architecture  : {}", index.facts().architecture);
    println!("kind          : {}", index.facts().kind);
    println!("entryPoint    : {:#x}", index.facts().entry_point);
    match index.build_id() {
        // The point of this branch: `--build-id=none` must print literally null.
        Some(id) => println!("buildId       : {id}"),
        None => println!("buildId       : null"),
    }
    println!("hasDebugInfo  : {}", index.has_debug_info());
    println!("symbolCount   : {}", index.symbol_count());
    println!(
        "stack         : object={} gimli={} addr2line={} rustc-demangle={} cpp_demangle={}",
        SYMBOL_STACK.elf_parser,
        SYMBOL_STACK.dwarf_decoder,
        SYMBOL_STACK.line_resolver,
        SYMBOL_STACK.rust_demangler,
        SYMBOL_STACK.cpp_demangler,
    );
    println!();

    let addresses: Vec<u64> = args
        .filter_map(|raw| {
            let raw = raw.trim();
            let parsed = u64::from_str_radix(raw.trim_start_matches("0x"), 16);
            match parsed {
                Ok(value) => Some(value),
                Err(_) => {
                    eprintln!("not a hex address: {raw}");
                    None
                }
            }
        })
        .collect();

    if addresses.is_empty() {
        eprintln!("no addresses given");
        std::process::exit(2);
    }

    for address in addresses {
        println!("address       : {address:#x}  (ripText \"0x{address:016x}\")");
        match index.source_line_for_address(address) {
            // The exact path: DWARF line row.
            Ok(Some(row)) => {
                println!("  symbolicated: {{");
                println!("    symbol        : \"{}\"", row.symbol);
                println!("    rawSymbol     : \"{}\"", row.raw_symbol);
                println!("    file          : \"{}\"", row.file);
                println!("    line          : {}", row.line);
                match row.column {
                    Some(column) => println!("    column        : {column}"),
                    None => println!("    column        : null"),
                }
                println!(
                    "    addressRange  : [{:#x}, {:#x})   // {} bytes, for the disassembly gutter",
                    row.address_start,
                    row.address_end,
                    row.address_end - row.address_start
                );
                println!(
                    "    lineRange     : [{}, {})",
                    row.line_range_start, row.line_range_end
                );
                println!("    inlined       : {}", row.inlined);
                println!("  }}");
                // Show the contract payload the engine attaches to `run.fault`.
                let mut fault = RunFaultPayload {
                    vector: "#UD".into(),
                    rip: address,
                    rip_text: format!("0x{address:016x}"),
                    error_code: 0,
                    regs: Default::default(),
                    symbolicated: None,
                };
                let _ = princess_symbol::symbolicate_fault_event(&index, &mut fault);
                println!(
                    "  run.fault.symbolicated = {}",
                    serde_json::to_string(&fault.symbolicated).unwrap()
                );
                println!("  ripText preserved      = {:?}", fault.rip_text);
            }
            // The honest negative: no fake, and a reason.
            Ok(None) => {
                let why = princess_symbol::explain_missing_symbolication(&index, address);
                println!("  symbolicated: null");
                println!("  reason        : {} — {}", why.code.as_str(), why.message);
                if let Some(coarse) = index.coarse_location(address).unwrap() {
                    println!(
                        "  coarse (untrusted, exact={}) : {} [{:#x},{:#x})",
                        coarse.exact, coarse.symbol, coarse.address_start, coarse.address_end
                    );
                } else {
                    println!("  coarse        : null (no symbol contains this address)");
                }
            }
            Err(err) => {
                println!("  error         : [{}] {}", err.code.as_str(), err.message);
                std::process::exit(1);
            }
        }
        println!();
    }
}
