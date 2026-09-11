//! [`BinProvider`] — the frozen `princess-core` contract, implemented over the
//! modules in this crate.
//!
//! # The one thing this wrapper deliberately does not do
//!
//! `BinProvider::source_line_for_address` returns a **single** `SourceLocation`.
//! D20 requires the disassembly view to present "instruction + source line" side
//! by side, and research D §1.2 measured that a lone line number actively
//! misleads on optimised code.  This wrapper therefore answers the point query
//! exactly as the contract says — it does not quietly return something richer
//! through the trait — and the richer **range** API lives on
//! [`crate::source::LineTable`] and [`crate::disasm::Disassembler::with_source`],
//! which is what the UI should call.  A caller that only has the trait object
//! still gets a correct answer; a caller that wants to draw the two-column view
//! reaches for the concrete type.
//!
//! # Laziness
//!
//! [`ElfBinProvider::open`] parses only the ELF header.  Sections, symbols and
//! DWARF are read on demand and cached, because the whole point of the async,
//! cancellable design in the contract is that opening a 200 MB image must not
//! block on a full DWARF load (research D §1.2, §6 item 14).

use std::path::{Path, PathBuf};

use princess_core::{
    BinProvider, BinaryFacts, DisassembledInstruction, PrincessError, Result, SectionInfo,
    SymbolInfo, SymbolicatedLocation,
};

use crate::disasm::{Disassembler, DisassemblerOptions, DisassemblyView};
use crate::elf::ElfHeader;
use crate::source::LineTable;

/// An artifact opened for inspection.
pub struct ElfBinProvider {
    path: PathBuf,
    header: ElfHeader,
    sections: Option<Vec<SectionInfo>>,
    symbols: Option<Vec<SymbolInfo>>,
    lines: LineTable,
    disassembler: Option<Disassembler>,
    disassembler_options: DisassemblerOptions,
}

impl std::fmt::Debug for ElfBinProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElfBinProvider")
            .field("path", &self.path)
            .field("format", &self.header.format)
            .field("entry_point", &format_args!("{:#x}", self.header.entry_point))
            .finish_non_exhaustive()
    }
}

impl ElfBinProvider {
    /// Open `artifact`, reading only its header.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when the path cannot be read, `E_INTERNAL` when it is not a
    /// parseable object file.
    pub fn open_path(artifact: &Path) -> Result<Self> {
        let header = crate::elf::read_header(artifact)?;
        Ok(Self {
            path: artifact.to_path_buf(),
            lines: LineTable::open(artifact)?,
            header,
            sections: None,
            symbols: None,
            disassembler: None,
            disassembler_options: DisassemblerOptions::default(),
        })
    }

    /// Choose the disassembly syntax before the first disassembly call.
    pub fn set_syntax(&mut self, syntax: crate::disasm::Syntax) {
        if self.disassembler_options.syntax != syntax {
            self.disassembler_options.syntax = syntax;
            // Force a rebuild with the new options.
            self.disassembler = None;
        }
    }

    /// The artifact path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The parsed header.
    #[must_use]
    pub fn header(&self) -> &ElfHeader {
        &self.header
    }

    /// The line table, for the range queries the UI needs.
    ///
    /// This is the "ranges, not points" escape hatch described in the module
    /// docs.
    pub fn line_table(&mut self) -> &mut LineTable {
        &mut self.lines
    }

    /// Disassemble `count` instructions at `address` **with the covering source
    /// rows**, which is the shape D20 requires.
    ///
    /// # Errors
    /// As [`Disassembler::with_source`].
    pub fn disassemble_with_source(
        &mut self,
        address: u64,
        count: u64,
    ) -> Result<DisassemblyView> {
        self.disassembler_mut()?.with_source(address, count)
    }

    fn disassembler_mut(&mut self) -> Result<&mut Disassembler> {
        if self.disassembler.is_none() {
            self.disassembler = Some(Disassembler::open(
                &self.path,
                self.disassembler_options.clone(),
            )?);
        }
        Ok(self.disassembler.as_mut().expect("just built"))
    }
}

impl BinProvider for ElfBinProvider {
    fn open(&mut self, artifact: &Path) -> Result<BinaryFacts> {
        let replacement = Self::open_path(artifact)?;
        self.path = replacement.path;
        self.header = replacement.header;
        self.lines = replacement.lines;
        self.sections = None;
        self.symbols = None;
        self.disassembler = None;
        Ok(BinaryFacts {
            path: self.path.clone(),
            format: self.header.format.clone(),
            entry_point: self.header.entry_point,
            build_id: self.header.build_id.clone(),
        })
    }

    fn sections(&self) -> Result<Vec<SectionInfo>> {
        if let Some(cached) = &self.sections {
            return Ok(cached.clone());
        }
        crate::elf::read_sections(&self.path)
    }

    fn symbols(&self) -> Result<Vec<SymbolInfo>> {
        if let Some(cached) = &self.symbols {
            return Ok(cached.clone());
        }
        crate::elf::read_symbols(&self.path)
    }

    fn source_line_for_address(&self, address: u64) -> Result<Option<SymbolicatedLocation>> {
        // `LineTable` is `Sync` (its DWARF context lives behind a mutex), so the
        // frozen `&self` signature is satisfiable directly.
        let Some(location) = self.lines.location_for_address(address)? else {
            return Ok(None);
        };
        let symbol = self.lines.function_for_address(address)?.unwrap_or_default();
        Ok(Some(SymbolicatedLocation {
            symbol,
            file: location.file,
            line: location.line,
        }))
    }

    fn disassemble(&self, address: u64, count: u64) -> Result<Vec<DisassembledInstruction>> {
        // Same `&self` constraint; the disassembler is stateless apart from its
        // parsed tables, so building it per call is correct, just not free.  The
        // UI path (`disassemble_with_source`) reuses the cached instance.
        let disassembler = Disassembler::open(&self.path, self.disassembler_options.clone())?;
        disassembler.disassemble(address, count)
    }
}

/// A `BinProvider` that has never been opened, for callers that construct one
/// before they have a path (the IPC layer does).
///
/// # Errors
/// Every method returns `E_NOT_FOUND` until [`BinProvider::open`] succeeds —
/// there is no "empty but valid" binary to hand back.
pub fn unopened() -> Result<ElfBinProvider> {
    Err(PrincessError::not_found(
        "no artifact has been opened yet",
    )
    .with_detail("call BinProvider::open(path) first".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refkernel() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/refkernel/build/refkernel.elf")
    }

    #[test]
    fn open_reports_the_contract_facts() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let mut provider = ElfBinProvider::open_path(&path).expect("open");
        let facts = provider
            .open(&path)
            .expect("the trait's open must also work");
        assert_eq!(facts.format, "elf64");
        assert_eq!(facts.entry_point, 0x10_0040);
        assert_eq!(facts.build_id, None, "--build-id=none means null, not a hash");
        assert_eq!(facts.path, path);
    }

    #[test]
    fn sections_and_symbols_match_the_free_functions() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let provider = ElfBinProvider::open_path(&path).expect("open");
        assert_eq!(
            provider.sections().unwrap(),
            crate::elf::read_sections(&path).unwrap()
        );
        assert_eq!(
            provider.symbols().unwrap(),
            crate::elf::read_symbols(&path).unwrap()
        );
    }

    #[test]
    fn the_point_source_query_answers_the_contract_address() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let provider = ElfBinProvider::open_path(&path).expect("open");
        let location = provider
            .source_line_for_address(0x10_0b3d)
            .expect("lookup")
            .expect("0x100b3d must resolve");
        assert_eq!(location.line, 100);
        assert!(location.file.ends_with("kernel.c"));
        assert_eq!(location.symbol, "refkernel_fault_probe");
    }

    #[test]
    fn the_point_query_returns_none_for_an_unmapped_address() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let provider = ElfBinProvider::open_path(&path).expect("open");
        assert!(provider
            .source_line_for_address(0xdead_beef_0000)
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_trait_disassembly_matches_the_direct_one() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let mut provider = ElfBinProvider::open_path(&path).expect("open");
        let via_trait = BinProvider::disassemble(&provider, 0x10_0b39, 4).unwrap();
        let via_inherent = provider.disassemble_with_source(0x10_0b39, 4).unwrap();
        assert_eq!(via_trait, via_inherent.instructions);
        assert_eq!(via_trait[2].bytes, "0f0b");
    }

    #[test]
    fn a_non_elf_reports_internal_not_an_empty_success() {
        let dir = std::env::temp_dir().join("princess-bin-provider-neg");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nope.elf");
        std::fs::write(&path, b"not an elf").unwrap();
        let err = ElfBinProvider::open_path(&path).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
    }

    #[test]
    fn unopened_is_an_error_not_an_empty_provider() {
        assert!(unopened().is_err());
    }
}
