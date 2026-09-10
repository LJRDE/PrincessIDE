//! The symbol engine's public face: artifact indexing, address symbolication,
//! and the `run.fault` enrichment the engine attaches to events.
//!
//! ## Answer strength is explicit
//!
//! Symbolication answers come in two strengths and the type system keeps them
//! apart, because the acceptance criteria for this phase include a **negative
//! sample**: an address outside the image must not produce a plausible-looking
//! fake.
//!
//! | strength | source | type |
//! |---|---|---|
//! | exact | DWARF line program | [`crate::dwarf::SourceLine`] |
//! | coarse | static symbol table only (no `-g`, or an address in a function's padding) | [`CoarseLocation`] |
//! | nothing | address is outside every known mapping | `Ok(None)` |
//!
//! The coarse answer is never returned from the exact API.  A caller that wants
//! it must ask for it by name, and the returned [`CoarseLocation`] carries
//! `exact: false` so the UI can render it differently (D20: a bare line number
//! misleads; a bare symbol is even weaker, and must be labelled).

use std::path::{Path, PathBuf};

use princess_core::{PrincessError, Result, RunFaultPayload, SymbolInfo, SymbolicatedLocation};

use crate::demangle;
use crate::dwarf::{DwarfIndex, SourceLine};
use crate::elf::{self, ElfFacts};

/// A function-level answer derived from `.symtab` when DWARF cannot help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoarseLocation {
    /// Function symbol, demangled where possible.
    pub symbol: String,
    /// Raw name from the symbol table.
    pub raw_symbol: String,
    /// Inclusive start address of the symbol.
    pub address_start: u64,
    /// Exclusive end address (`start + size`; for a zero-size symbol this is
    /// `start + 1` so the range is never degenerate).
    pub address_end: u64,
    /// Always `false`: this is not a DWARF line answer.
    pub exact: bool,
}

impl CoarseLocation {
    /// Coarse answers carry no line, so they cannot satisfy the contract's
    /// `SymbolicatedLocation` (which requires `file` and `line`).
    ///
    /// This always returns `None` **by design** — it exists so a caller cannot
    /// accidentally promote a symbol-table guess into an exact answer.  The
    /// value is still useful on its own for the symbol browser.
    pub fn to_symbolicated_location(&self) -> Option<SymbolicatedLocation> {
        None
    }
}

/// An artifact opened and indexed for symbolication.
#[derive(Debug)]
pub struct SymbolIndex {
    path: PathBuf,
    facts: ElfFacts,
    dwarf: DwarfIndex,
    symbol_count: u64,
}

impl SymbolIndex {
    /// Open `path`, read its ELF facts and parse its DWARF.
    pub fn open(path: &Path) -> Result<Self> {
        let facts = elf::read_elf_facts(path)?;
        let dwarf = DwarfIndex::open(path)?;
        let symbol_count = elf::read_symbols(path)?.len() as u64;
        Ok(SymbolIndex {
            path: path.to_path_buf(),
            facts,
            dwarf,
            symbol_count,
        })
    }

    /// Artifact path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Container facts (entry point, format, and the **possibly `None`** build id).
    pub fn facts(&self) -> &ElfFacts {
        &self.facts
    }

    /// GNU build-id, or `None` — never fabricated (contract §2).
    pub fn build_id(&self) -> Option<&str> {
        self.facts.build_id.as_deref()
    }

    /// Number of symbols this index can browse (matches `symbols.indexed`).
    pub fn symbol_count(&self) -> u64 {
        self.symbol_count
    }

    /// `true` when the artifact carries `.debug_info`.
    pub fn has_debug_info(&self) -> bool {
        self.dwarf.has_debug_info()
    }

    /// The underlying DWARF index, for range queries.
    pub fn dwarf(&self) -> &DwarfIndex {
        &self.dwarf
    }

    /// Exact DWARF symbolication for `address`.
    ///
    /// `Ok(None)` means "no line-table row covers this address" — which is the
    /// honest answer for an out-of-image address, for the padding between
    /// functions, and for any artifact built without `-g`.
    pub fn source_line_for_address(&self, address: u64) -> Result<Option<SourceLine>> {
        self.dwarf.source_line_for_address(address)
    }

    /// The contract-shaped `run.fault.symbolicated` value.
    pub fn symbolicate(&self, address: u64) -> Result<Option<SymbolicatedLocation>> {
        Ok(self
            .source_line_for_address(address)?
            .map(|row| row.to_symbolicated_location()))
    }

    /// Function-level answer from `.symtab`, for artifacts without debug info.
    ///
    /// Returns `None` when no symbol contains `address`.  A zero-size symbol
    /// (a label) only matches its exact address, which is the truthful rule: a
    /// label does not own the bytes after it.
    pub fn coarse_location(&self, address: u64) -> Result<Option<CoarseLocation>> {
        let symbols = elf::read_symbols(&self.path)?;
        let mut best: Option<SymbolInfo> = None;
        for symbol in symbols {
            let covers = if symbol.size == 0 {
                address == symbol.address
            } else {
                address >= symbol.address && address < symbol.address.saturating_add(symbol.size)
            };
            if !covers {
                continue;
            }
            best = Some(match best {
                // Narrowest containing symbol wins: the most specific fact.
                Some(prev) => {
                    if symbol.size.max(1) <= prev.size.max(1) {
                        symbol
                    } else {
                        prev
                    }
                }
                None => symbol,
            });
        }
        Ok(best.map(|symbol| CoarseLocation {
            symbol: demangle::demangle(&symbol.name).name,
            raw_symbol: symbol.name,
            address_start: symbol.address,
            address_end: symbol.address.saturating_add(symbol.size.max(1)),
            exact: false,
        }))
    }

    /// All symbols, for the symbol browser and `symbols.indexed`.
    pub fn symbols(&self) -> Result<Vec<SymbolInfo>> {
        elf::read_symbols(&self.path)
    }

    /// The `symbols.indexed` payload fields: `(artifact, buildId, symbolCount)`.
    ///
    /// `buildId` is `None` for the fixtures (`--build-id=none`) and stays `None`
    /// — the contract forbids inventing one.
    pub fn indexed_event_fields(&self) -> (String, Option<String>, u64) {
        (
            self.path.to_string_lossy().into_owned(),
            self.build_id().map(str::to_string),
            self.symbol_count,
        )
    }
}

/// Attach symbolication to a `run.fault` payload **without touching `ripText`**.
///
/// The contract (`10-contracts.md` §2) requires `ripText` — the guest's own
/// 16-hex-digit text — to survive verbatim so a human can check the engine's
/// parse.  This function therefore only ever writes `symbolicated`; `rip`,
/// `rip_text`, `vector`, `error_code` and `regs` are passed through unchanged,
/// and a test pins that.
///
/// Returns whether a source line was attached.
pub fn symbolicate_fault_event(index: &SymbolIndex, fault: &mut RunFaultPayload) -> Result<bool> {
    let location = index.symbolicate(fault.rip)?;
    let changed = location.is_some();
    fault.symbolicated = location;
    Ok(changed)
}

/// Convenience for the common engine path: open `artifact` and symbolicate one
/// `run.fault` payload.
pub fn symbolicate_fault(artifact: &Path, fault: &mut RunFaultPayload) -> Result<bool> {
    let index = SymbolIndex::open(artifact)?;
    symbolicate_fault_event(&index, fault)
}

/// One frame of a recovered call stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrameFacts {
    /// Saved frame pointer value (`RBP`) for this frame.
    pub frame_pointer: u64,
    /// Return address pushed by the `call` that entered this frame.
    pub return_address: u64,
    /// DWARF answer for `return_address`, when available.
    pub location: Option<SourceLine>,
}

/// Walk a frame-pointer chain starting at `rbp`.
///
/// The kernel fixtures are compiled `-O0` with frame pointers, so the saved
/// `RBP` chain is a reliable locator; DWARF CFI unwinding is P4's job (it owns
/// the live target) and is deliberately **not** duplicated here.
///
/// `read_word` supplies 8 bytes at a guest address — the engine passes a closure
/// over the debug backend's memory reader, so this function never invents memory
/// it could not read.
///
/// The walk stops on: an unreadable frame, a return address of `0` (the
/// outermost frame), a next-pointer that does not advance (a corrupt or
/// self-referential chain), or `max_frames`.  A frame whose saved `RBP` is `0`
/// is still a real frame and is recorded — `0` ends the chain *after* it, it
/// does not invalidate it.
pub fn walk_frame_pointer_chain<F>(
    index: &DwarfIndex,
    rbp: u64,
    max_frames: usize,
    mut read_word: F,
) -> Result<Vec<StackFrameFacts>>
where
    F: FnMut(u64) -> Option<u64>,
{
    let mut out = Vec::new();
    let mut frame_pointer = rbp;

    while out.len() < max_frames {
        if frame_pointer == 0 {
            // Not a frame: RBP==0 means "no caller", so there is nothing here.
            break;
        }
        // The chain walks towards higher addresses.  A pointer that fails to
        // advance means the chain is corrupt or self-referential; following it
        // would loop forever, so stop before reading anything more.
        if let Some(last) = out.last() {
            let last: &StackFrameFacts = last;
            if frame_pointer <= last.frame_pointer {
                break;
            }
        }
        let Some(next) = read_word(frame_pointer) else {
            break;
        };
        let Some(return_address) = read_word(frame_pointer.wrapping_add(8)) else {
            break;
        };
        // A return address of 0 is the sentinel for "outermost frame": this
        // frame is still real, so record it and then stop.
        let outermost = return_address == 0;
        out.push(StackFrameFacts {
            frame_pointer,
            return_address,
            location: if outermost {
                None
            } else {
                index.source_line_for_address(return_address)?
            },
        });
        if outermost || next <= frame_pointer {
            break;
        }
        frame_pointer = next;
    }
    Ok(out)
}

/// Explain why symbolication produced nothing, for an explicit error message.
///
/// The acceptance criteria require "明确报错或返回 None" for a bad address; the
/// engine uses this to say *which* honest reason applies instead of a bare
/// `null`.
pub fn explain_missing_symbolication(index: &SymbolIndex, address: u64) -> PrincessError {
    if !index.has_debug_info() {
        return PrincessError::not_found(format!(
            "{} carries no DWARF (.debug_info): rebuild with -g to symbolicate {address:#x}",
            index.path().display()
        ));
    }
    PrincessError::not_found(format!(
        "no DWARF line row covers {address:#x} in {}",
        index.path().display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use princess_core::RunFaultPayload;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    fn fault(rip: u64, rip_text: &str) -> RunFaultPayload {
        RunFaultPayload {
            vector: "#UD".into(),
            rip,
            rip_text: rip_text.into(),
            error_code: 0,
            regs: Default::default(),
            symbolicated: None,
        }
    }

    #[test]
    fn refkernel_fault_symbolicates_to_line_100() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        let location = index.symbolicate(0x100b3d).unwrap().unwrap();
        assert_eq!(location.symbol, "refkernel_fault_probe");
        assert_eq!(location.line, 100);
        assert!(location.file.ends_with("fixtures/refkernel/kernel.c"));
    }

    #[test]
    fn fault_enrichment_preserves_rip_text_verbatim() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        // The guest prints 16 zero-padded hex digits; the engine must keep them.
        let original_text = "0x0000000000100b3d";
        let mut payload = fault(0x100b3d, original_text);
        let changed = symbolicate_fault_event(&index, &mut payload).unwrap();
        assert!(changed);
        assert_eq!(payload.rip_text, original_text, "ripText must be untouched");
        assert_eq!(payload.rip, 0x100b3d);
        assert_eq!(payload.vector, "#UD");
        assert_eq!(
            payload.symbolicated.as_ref().unwrap().symbol,
            "refkernel_fault_probe"
        );
    }

    #[test]
    fn an_out_of_image_address_yields_none_and_leaves_the_payload_alone() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        let mut payload = fault(0xdead_beef, "0x00000000deadbeef");
        let changed = symbolicate_fault_event(&index, &mut payload).unwrap();
        assert!(!changed);
        assert!(payload.symbolicated.is_none(), "no fabricated answer");
        assert_eq!(payload.rip_text, "0x00000000deadbeef");
    }

    #[test]
    fn coarse_location_finds_the_function_without_dwarf() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        let coarse = index.coarse_location(0x100b3d).unwrap().unwrap();
        assert_eq!(coarse.raw_symbol, "refkernel_fault_probe");
        assert!(!coarse.exact);
        // A coarse answer must not be dressed up as an exact one.
        assert!(coarse.to_symbolicated_location().is_none());
        assert!(coarse.address_end > coarse.address_start);
    }

    #[test]
    fn coarse_location_is_none_for_an_address_outside_every_symbol() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        assert!(index.coarse_location(0xdead_beef).unwrap().is_none());
    }

    #[test]
    fn build_id_is_none_and_indexed_fields_say_so() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        assert_eq!(index.build_id(), None);
        let (artifact, build_id, count) = index.indexed_event_fields();
        assert!(artifact.ends_with("refkernel.elf"));
        assert_eq!(build_id, None, "must be null, never fabricated");
        assert!(count > 0);
    }

    #[test]
    fn explain_missing_symbolication_names_the_real_reason() {
        let index = SymbolIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        let err = explain_missing_symbolication(&index, 0xdead_beef);
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
        assert!(err.message.contains("no DWARF line row"), "{err:?}");
    }

    #[test]
    fn frame_pointer_walk_reads_one_frame_and_symbolicates_it() {
        let index = DwarfIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        // A reader that only knows one frame: rbp -> next, rbp+8 -> return addr.
        let rbp = 0x108000u64;
        let frames = walk_frame_pointer_chain(&index, rbp, 8, |address| match address {
            a if a == rbp => Some(0),            // end of chain
            a if a == rbp + 8 => Some(0x100b3d), // the fault return address
            _ => None,
        })
        .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].return_address, 0x100b3d);
        assert_eq!(
            frames[0].location.as_ref().unwrap().symbol,
            "refkernel_fault_probe"
        );
    }

    #[test]
    fn frame_pointer_walk_rejects_a_non_monotonic_chain() {
        let index = DwarfIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        // Every read returns the same pointer: a loop must not run forever.  It
        // is allowed to record the (garbage) first frame, but then must stop —
        // the assertion that matters is that the walk terminated at all.
        let frames = walk_frame_pointer_chain(&index, 0x108000, 16, |_| Some(0x108000)).unwrap();
        assert!(
            frames.len() <= 1,
            "self-referential chain must terminate after at most one frame: {frames:?}"
        );
    }

    #[test]
    fn frame_pointer_walk_terminates_on_a_zero_return_address() {
        let index = DwarfIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        // `[rbp+8] == 0` is the outermost-frame sentinel: the frame is real, but
        // there is no more chain to follow and the walk must stop.
        let frames = walk_frame_pointer_chain(&index, 0x108000, 16, |address| {
            if address == 0x108000 {
                Some(0x108800) // a plausible caller frame pointer
            } else {
                Some(0) // the return address slot of every frame
            }
        })
        .unwrap();
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0].return_address, 0);
        assert!(frames[0].location.is_none(), "address 0 is not symbolicated");
    }

    #[test]
    fn frame_pointer_walk_respects_max_frames() {
        let index = DwarfIndex::open(&fixture("fixtures/refkernel/build/refkernel.elf")).unwrap();
        // A perfectly valid chain that would go on forever must be capped.
        let frames = walk_frame_pointer_chain(&index, 0x108000, 3, |address| {
            Some(address.wrapping_add(0x100))
        })
        .unwrap();
        assert_eq!(frames.len(), 3, "{frames:?}");
    }
}
