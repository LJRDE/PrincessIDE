//! The [`princess_core::BinProvider`] implementation (contract §5).
//!
//! The engine holds backends as `Box<dyn BinProvider>` and dispatches statically
//! to a concrete type chosen from the project.  This is the concrete type for
//! v1's x86_64 ELF artifacts.
//!
//! ## What "disassemble" means here
//!
//! `BinProvider::disassemble` is part of the frozen trait, but **disassembly is
//! P5's job** (D20: `iced-x86` 1.21.0).  Rather than ship a second, weaker
//! decoder or silently return an empty vector, this implementation answers
//! `E_INTERNAL` with a message naming the owner.  An explicit refusal is
//! contract rule §0.4 ("失败必须显式"); an empty `Vec` would look like "this
//! function has no instructions", which is a lie the UI would render.

use std::path::{Path, PathBuf};

use princess_core::{
    BinProvider, BinaryFacts, DisassembledInstruction, PrincessError, Result, SectionInfo,
    SymbolInfo, SymbolicatedLocation,
};

use crate::dwarf::DwarfIndex;
use crate::elf;
use crate::symbol::SymbolIndex;

/// ELF/DWARF binary-inspection provider for `BinProvider`.
#[derive(Debug, Default)]
pub struct ElfSymbolProvider {
    artifact: Option<PathBuf>,
    facts: Option<BinaryFacts>,
    index: Option<SymbolIndex>,
}

impl ElfSymbolProvider {
    /// An unopened provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// The artifact currently open, if any.
    pub fn artifact(&self) -> Option<&Path> {
        self.artifact.as_deref()
    }

    /// The full symbol index, once [`BinProvider::open`] has run.
    pub fn index(&self) -> Option<&SymbolIndex> {
        self.index.as_ref()
    }

    /// The DWARF index, once opened.
    pub fn dwarf(&self) -> Option<&DwarfIndex> {
        self.index.as_ref().map(SymbolIndex::dwarf)
    }

    /// Build `symbols.indexed`'s fields from the open artifact.
    ///
    /// `buildId` is `null` for the fixtures and stays `null`.
    pub fn symbols_indexed_fields(&self) -> Result<(String, Option<String>, u64)> {
        let index = self.open_index()?;
        Ok(index.indexed_event_fields())
    }

    fn open_index(&self) -> Result<&SymbolIndex> {
        self.index.as_ref().ok_or_else(|| {
            PrincessError::internal("no artifact is open: call BinProvider::open first")
        })
    }
}

impl BinProvider for ElfSymbolProvider {
    fn open(&mut self, artifact: &Path) -> Result<BinaryFacts> {
        let facts = elf::read_elf_facts(artifact)?.to_binary_facts();
        let index = SymbolIndex::open(artifact)?;
        self.artifact = Some(artifact.to_path_buf());
        self.facts = Some(facts.clone());
        self.index = Some(index);
        Ok(facts)
    }

    fn sections(&self) -> Result<Vec<SectionInfo>> {
        let path = self
            .artifact
            .as_ref()
            .ok_or_else(|| PrincessError::internal("no artifact is open"))?;
        elf::read_sections(path)
    }

    fn symbols(&self) -> Result<Vec<SymbolInfo>> {
        let path = self
            .artifact
            .as_ref()
            .ok_or_else(|| PrincessError::internal("no artifact is open"))?;
        elf::read_symbols(path)
    }

    fn source_line_for_address(&self, address: u64) -> Result<Option<SymbolicatedLocation>> {
        // Deliberately exact-only: a symbol-table guess must not be dressed up as
        // a source line (see crate docs, rule 2).
        self.open_index()?.symbolicate(address)
    }

    fn disassemble(&self, _address: u64, _count: u64) -> Result<Vec<DisassembledInstruction>> {
        Err(PrincessError::internal(
            "disassembly belongs to princess-bin (P5, iced-x86 per D20); \
             princess-symbol supplies source rows via rows_in_range() for P5 to pair with instructions",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    fn open_provider() -> ElfSymbolProvider {
        let mut provider = ElfSymbolProvider::new();
        provider
            .open(&fixture("fixtures/refkernel/build/refkernel.elf"))
            .unwrap();
        provider
    }

    #[test]
    fn open_reports_the_header_facts() {
        let provider = open_provider();
        let facts = provider.facts.as_ref().unwrap();
        assert_eq!(facts.format, "elf64");
        assert_eq!(facts.entry_point, 0x100040);
        assert_eq!(facts.build_id, None, "fixture has no build-id");
    }

    #[test]
    fn source_line_lookup_goes_through_the_trait() {
        let provider = open_provider();
        let location = provider.source_line_for_address(0x100b3d).unwrap().unwrap();
        assert_eq!(location.symbol, "refkernel_fault_probe");
        assert_eq!(location.line, 100);
    }

    #[test]
    fn a_bad_address_is_none_through_the_trait() {
        let provider = open_provider();
        assert!(provider.source_line_for_address(0xdead_beef).unwrap().is_none());
    }

    #[test]
    fn sections_and_symbols_are_readable_through_the_trait() {
        let provider = open_provider();
        let sections = provider.sections().unwrap();
        assert!(sections.iter().any(|s| s.name == ".text"));
        let symbols = provider.symbols().unwrap();
        assert!(symbols.iter().any(|s| s.name == "refkernel_fault_probe"));
    }

    #[test]
    fn disassembly_refuses_explicitly_instead_of_returning_an_empty_vec() {
        let provider = open_provider();
        let err = provider.disassemble(0x100b3d, 4).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        assert!(err.message.contains("P5"), "{err:?}");
    }

    #[test]
    fn using_the_provider_before_open_is_an_explicit_error() {
        let provider = ElfSymbolProvider::new();
        let err = provider.symbols().unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        assert!(err.message.contains("no artifact is open"), "{err:?}");
    }

    #[test]
    fn symbols_indexed_fields_carry_a_null_build_id() {
        let provider = open_provider();
        let (artifact, build_id, count) = provider.symbols_indexed_fields().unwrap();
        assert!(artifact.ends_with("refkernel.elf"));
        assert_eq!(build_id, None);
        assert!(count > 0);
    }
}
