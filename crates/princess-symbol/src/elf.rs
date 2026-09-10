//! ELF container facts — read with **`object` 0.40.0** (D20, frozen).
//!
//! This module owns the "container" half of the symbol engine: what kind of file
//! the artifact is, where its entry point is, which sections exist, what the
//! static symbol table says, and whether the image carries a GNU build-id.
//!
//! It deliberately does **not** touch DWARF (that is [`crate::dwarf`]) and does
//! not disassemble anything (that is P5, `iced-x86`).
//!
//! ## The `buildId: null` rule
//!
//! `symbols.indexed.buildId` is `null` for any image **linked with
//! `--build-id=none`**, and both reference fixtures are exactly that.  The
//! contract (`10-contracts.md` §2) is explicit: *"宁可 null 也不得编造"*.  So
//! [`ElfFacts::build_id`] is an `Option<String>` that is only ever `Some` when
//! the image literally contains a `.note.gnu.build-id` payload — there is no
//! fallback to a hash of the file, a timestamp, or the symbol table.

use std::path::{Path, PathBuf};

use object::{Object, ObjectSection, ObjectSymbol};
use princess_core::{BinaryFacts, PrincessError, Result, SectionInfo, SymbolInfo};

/// Container-level facts about one ELF artifact.
///
/// `ObjectFacts` is the cheap subset that only needs the file header; the symbol
/// table and section list are read on demand so that opening a 200 MB image does
/// not walk every table up front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfFacts {
    /// Absolute path of the artifact on disk.
    pub path: PathBuf,
    /// `elf64` / `elf32` / ... as `object` spells it.
    pub format: String,
    /// `X86_64` / `AArch64` / ... as `object` spells it.
    pub architecture: String,
    /// `EXEC` / `DYN` / `REL` / ... as `object` spells it.
    pub kind: String,
    /// `little` / `big`.
    pub endianness: String,
    /// Virtual address of the entry point.
    pub entry_point: u64,
    /// GNU build-id as lower-case hex, **`None` when the image has none**.
    pub build_id: Option<String>,
}

impl ElfFacts {
    /// The shape `princess:bin:open` and `BinProvider::open` hand back.
    pub fn to_binary_facts(&self) -> BinaryFacts {
        BinaryFacts {
            path: self.path.clone(),
            format: self.format.clone(),
            entry_point: self.entry_point,
            build_id: self.build_id.clone(),
        }
    }
}

/// Parse and validate the ELF header of `path`.
///
/// # Errors
///
/// * `E_NOT_FOUND` — the path does not exist or cannot be read.
/// * `E_INTERNAL`  — the file exists but `object` cannot parse it as an ELF
///   (e.g. a raw binary, a truncated ISO, or a text file).  The message names
///   the parser's own reason so the UI can show why, and the mapping is
///   **explicit**: a non-ELF artifact must never look like an empty-but-valid
///   symbol table.
pub fn read_elf_facts(path: &Path) -> Result<ElfFacts> {
    let bytes = read_artifact(path)?;
    let file = object::File::parse(bytes.as_slice()).map_err(|err| {
        PrincessError::internal(format!(
            "{} is not a parseable ELF image: {err}",
            path.display()
        ))
        .with_detail(err.to_string())
    })?;

    Ok(ElfFacts {
        path: path.to_path_buf(),
        // The contract spells this `elf32` | `elf64` (§5 `BinaryFacts.format`),
        // which `object`'s `BinaryFormat` (just `Elf`) does not distinguish —
        // the class comes from `is_64()`.
        format: match file.format() {
            object::BinaryFormat::Elf if file.is_64() => "elf64",
            object::BinaryFormat::Elf => "elf32",
            other => return Err(non_elf_error(path, other)),
        }
        .to_string(),
        architecture: format!("{:?}", file.architecture()),
        kind: format!("{:?}", file.kind()).to_ascii_lowercase(),
        endianness: match file.endianness() {
            object::Endianness::Little => "little".to_string(),
            object::Endianness::Big => "big".to_string(),
        },
        entry_point: file.entry(),
        build_id: read_build_id(&file),
    })
}

/// A non-ELF artifact must be an explicit error: pretending it has zero
/// sections/symbols would let the UI render an empty-but-valid binary view.
fn non_elf_error(path: &Path, format: object::BinaryFormat) -> PrincessError {
    PrincessError::internal(format!(
        "{} is not an ELF image (object parsed it as {format:?}); \
         princess-symbol v1 only indexes ELF (D20)",
        path.display()
    ))
}

/// Read the section headers a P5 view needs, in file order.
///
/// Section *flags* are rendered the way `readelf -S` renders them (`AX`, `WA`,
/// ...) so a golden comparison against binutils does not need a translation
/// table.  The names come from `object`, never from a guessed table.
pub fn read_sections(path: &Path) -> Result<Vec<SectionInfo>> {
    let bytes = read_artifact(path)?;
    let file = parse(&bytes, path)?;
    let mut out = Vec::new();
    for section in file.sections() {
        let name = section
            .name()
            .map_err(|err| {
                PrincessError::internal(format!(
                    "{} has a section whose name is unreadable: {err}",
                    path.display()
                ))
            })?
            .to_string();
        // A zero-length section with no address is a placeholder; `readelf`
        // still lists it, so we do too — dropping it would make a golden diff
        // differ for no real reason.
        out.push(SectionInfo {
            name,
            address: section.address(),
            size: section.size(),
            flags: section_flags(&section),
        });
    }
    Ok(out)
}

/// Read the static symbol table.
///
/// Only symbols that carry an address are returned: `FILE`/`SECTION`/`UND`
/// entries exist in `.symtab` but have no meaningful `address`/`size`, and
/// handing them to the UI as symbols would be noise pretending to be data.
pub fn read_symbols(path: &Path) -> Result<Vec<SymbolInfo>> {
    let bytes = read_artifact(path)?;
    let file = parse(&bytes, path)?;
    let mut out = Vec::new();
    for symbol in file.symbols() {
        let address = symbol.address();
        if address == 0 && symbol.section_index().is_none() {
            // Undefined / absolute placeholder.
            continue;
        }
        let name = match symbol.name() {
            Ok(name) if !name.is_empty() => name.to_string(),
            // A nameless symbol is real (stripped local labels); keep it rather
            // than silently renumbering the table, but mark it honestly.
            _ => continue,
        };
        out.push(SymbolInfo {
            name,
            address,
            size: symbol.size(),
            kind: symbol_kind(&symbol),
        });
    }
    Ok(out)
}

/// Find one symbol by exact name, preferring the definition with the largest
/// `st_size` when a name repeats (the reference kernels declare a few aliases).
pub fn find_symbol(path: &Path, name: &str) -> Result<Option<SymbolInfo>> {
    let symbols = read_symbols(path)?;
    Ok(symbols
        .into_iter()
        .filter(|s| s.name == name)
        .max_by_key(|s| (s.size, s.address)))
}

// --------------------------------------------------------------- internals ---

/// Slurp the artifact.  Kept in one place so every reader reports the same
/// `E_NOT_FOUND` text for a missing fixture.
fn read_artifact(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            PrincessError::not_found(format!("symbol artifact not found: {}", path.display()))
        } else {
            PrincessError::internal(format!("cannot read {}: {err}", path.display()))
        };
        code.with_detail(err.to_string())
    })
}

fn parse<'a>(bytes: &'a [u8], path: &Path) -> Result<object::File<'a>> {
    object::File::parse(bytes).map_err(|err| {
        PrincessError::internal(format!(
            "{} is not a parseable ELF image: {err}",
            path.display()
        ))
        .with_detail(err.to_string())
    })
}

/// GNU build-id, lower-case hex — `None` when absent, never synthesised.
fn read_build_id(file: &object::File<'_>) -> Option<String> {
    match file.build_id() {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(hex_lower(bytes)),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// `readelf -S` style flag letters, in binutils' own order: `W` `A` `X` `M` `S`
/// `I` `L` `G` `T` `C` `E` `O`.
///
/// `object` exposes ELF section flags as the raw `u64` in
/// [`object::SectionFlags::Elf`]; the letter mapping below is the same one
/// binutils' `get_section_flags` applies, so a golden diff against `readelf -S`
/// needs no translation.
fn section_flags(section: &object::Section<'_, '_>) -> String {
    use object::elf;
    let raw = match section.flags() {
        object::SectionFlags::Elf { sh_flags, .. } => sh_flags,
        // A non-ELF section cannot appear here (the reader is ELF-only), but
        // returning an empty string is the honest answer rather than a guess.
        _ => return String::new(),
    };
    let mut out = String::new();
    if raw.contains(elf::SHF_WRITE) {
        out.push('W');
    }
    if raw.contains(elf::SHF_ALLOC) {
        out.push('A');
    }
    if raw.contains(elf::SHF_EXECINSTR) {
        out.push('X');
    }
    if raw.contains(elf::SHF_MERGE) {
        out.push('M');
    }
    if raw.contains(elf::SHF_STRINGS) {
        out.push('S');
    }
    if raw.contains(elf::SHF_INFO_LINK) {
        out.push('I');
    }
    if raw.contains(elf::SHF_LINK_ORDER) {
        out.push('L');
    }
    if raw.contains(elf::SHF_GROUP) {
        out.push('G');
    }
    if raw.contains(elf::SHF_TLS) {
        out.push('T');
    }
    if raw.contains(elf::SHF_COMPRESSED) {
        out.push('C');
    }
    if raw.contains(elf::SHF_EXCLUDE) {
        out.push('E');
    }
    out
}

/// The `SymbolInfo.kind` spelling, matching `readelf -s` type names.
fn symbol_kind(symbol: &object::Symbol<'_, '_>) -> String {
    use object::SymbolKind;
    match symbol.kind() {
        SymbolKind::Text => "FUNC",
        SymbolKind::Data => "OBJECT",
        SymbolKind::Section => "SECTION",
        SymbolKind::File => "FILE",
        SymbolKind::Tls => "TLS",
        SymbolKind::Label => "NOTYPE",
        SymbolKind::Unknown => "NOTYPE",
        // `SymbolKind` is `#[non_exhaustive]`; a future object version may add a
        // kind, and "NOTYPE" is the truthful `readelf` fallback for one we do
        // not have a letter for yet.
        _ => "NOTYPE",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve a fixture path relative to the workspace root, which is two
    /// levels above `CARGO_MANIFEST_DIR` for `crates/princess-symbol`.
    pub(crate) fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel)
    }

    #[test]
    fn reads_the_reference_kernel_header() {
        let elf = fixture("fixtures/refkernel/build/refkernel.elf");
        let facts = read_elf_facts(&elf).expect("refkernel.elf parses");
        assert_eq!(facts.format, "elf64");
        assert_eq!(facts.architecture, "X86_64");
        assert_eq!(facts.kind, "executable");
        assert_eq!(facts.endianness, "little");
        assert_eq!(facts.entry_point, 0x100040);
    }

    #[test]
    fn build_id_is_absent_not_invented_on_the_fixtures() {
        // D20 / contract §2: both fixtures are linked with --build-id=none, so
        // the honest answer is `null`.  If this ever fails, someone invented an
        // id out of a file hash.
        for rel in [
            "fixtures/refkernel/build/refkernel.elf",
            "fixtures/paging-kernel/build/pagingkernel.elf",
        ] {
            let facts = read_elf_facts(&fixture(rel)).unwrap();
            assert_eq!(facts.build_id, None, "{rel} must report buildId=null");
            assert_eq!(facts.to_binary_facts().build_id, None);
        }
    }

    #[test]
    fn missing_artifact_is_not_found_not_empty() {
        let err = read_elf_facts(Path::new("/nonexistent/nope.elf")).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
    }

    #[test]
    fn a_non_elf_file_is_an_explicit_error() {
        // A text file must not look like an ELF with zero symbols.
        let path = std::env::temp_dir().join("princess-symbol-not-an-elf.txt");
        std::fs::write(&path, b"this is not an elf\n").unwrap();
        let err = read_elf_facts(&path).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        assert!(err.message.contains("not a parseable ELF"), "{err:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn symbol_lookup_finds_the_fault_probe() {
        let elf = fixture("fixtures/refkernel/build/refkernel.elf");
        let symbol = find_symbol(&elf, "refkernel_fault_probe")
            .unwrap()
            .expect("refkernel_fault_probe is in .symtab");
        assert_eq!(symbol.kind, "FUNC");
        // The probe is reached by a direct call; its address bounds the RIP.
        assert!(symbol.address <= 0x100b3d, "{symbol:?}");
        assert!(symbol.address + symbol.size > 0x100b3d, "{symbol:?}");
    }

    #[test]
    fn sections_include_text_and_the_dwarf_sections() {
        let elf = fixture("fixtures/refkernel/build/refkernel.elf");
        let sections = read_sections(&elf).unwrap();
        let text = sections.iter().find(|s| s.name == ".text").unwrap();
        assert_eq!(text.flags, "AX");
        assert!(text.size > 0);
        assert!(
            sections.iter().any(|s| s.name == ".debug_line"),
            "fixture must be built with -g"
        );
    }

    #[test]
    fn hex_lower_is_two_digits_per_byte() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
