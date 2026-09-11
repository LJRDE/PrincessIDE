//! ELF container facts, read with **`object` 0.40.0** (D20, frozen).
//!
//! P5-1 asserts that what this module reports matches `readelf` / `objdump`
//! byte for byte once rendered.  That constraint drives three choices:
//!
//! 1. **Flags are rendered the way `readelf` renders them.**  Section flags come
//!    out as `WAX`-style letters in `readelf`'s own order and program-header
//!    flags as `R E` / `RW ` with `readelf -l`'s three-letter layout, so the
//!    golden comparison needs no translation table that could hide a bug.
//! 2. **Nothing is filtered out.**  `readelf -S` prints zero-length sections and
//!    `readelf -l` prints zero-size segments; so do we.  Silently dropping them
//!    would make a golden diff differ for reasons that are not the file's fault.
//! 3. **The symbol table is enumerated in link order and not de-duplicated.**
//!    `.symtab` genuinely contains `FILE`/`SECTION`/`UND` entries and `ABS`
//!    constants like `GDT_CODE64`; they are part of what `readelf -s` shows.
//!    The one thing we *do* add on top is [`SymbolInfo::kind`], which is what
//!    the UI filters on.
//!
//! ## Why the whole file is read for `symbols()` / `sections()`
//!
//! `object::File` borrows the image.  Keeping a `'static`-ish view would mean an
//! arena or a self-referential struct; instead the *symbol table* path reads the
//! file into a `Vec<u8>` for the duration of one call.  That is bounded by
//! [`MAX_ARTIFACT_BYTES`] and the caller is told when an artifact is too big
//! rather than being handed a half-parsed table.  The long-lived, randomly
//! accessed images (hex view, disk images) deliberately go through
//! [`crate::hex::HexReader`] instead, which never reads more than 1 MiB.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol};
use princess_core::{PrincessError, Result, SectionInfo, SymbolInfo};

/// Largest artifact this module will read into memory for a table walk.
///
/// 512 MiB is far above any kernel image the IDE is expected to open, and far
/// below anything that would threaten the 3.8 GiB host.  Above it the caller
/// gets an explicit error naming the limit — never a silent partial parse.
pub const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// ELF header facts (`readelf -h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfHeader {
    /// Absolute path of the artifact.
    pub path: PathBuf,
    /// `elf64` / `elf32`, as `object` spells it.
    pub format: String,
    /// `EXEC` / `DYN` / `REL` / `CORE`, as `object` spells it.
    pub kind: String,
    /// `X86_64` / `AArch64` / ... — `object`'s spelling, not `readelf`'s.
    pub architecture: String,
    /// `little` / `big`.
    pub endianness: String,
    /// `ET_EXEC`-family ELF class: `ELFCLASS32` / `ELFCLASS64`.
    pub class: String,
    /// Virtual address of the entry point (`e_entry`).
    pub entry_point: u64,
    /// Number of section headers.
    pub section_count: usize,
    /// Number of program headers.
    pub segment_count: usize,
    /// GNU build-id as lower-case hex, `None` when the image has none.
    ///
    /// Both reference fixtures are linked `--build-id=none`, and the contract
    /// (`10-contracts.md` §2) is explicit that `null` is the right answer there:
    /// *"宁可 null 也不得编造"*.  There is no fallback to a file hash.
    pub build_id: Option<String>,
}

/// `readelf -h`'s `Type:` value.
fn elf_kind(kind: object::ObjectKind) -> String {
    match kind {
        object::ObjectKind::Relocatable => "REL",
        object::ObjectKind::Executable => "EXEC",
        object::ObjectKind::Dynamic => "DYN",
        object::ObjectKind::Core => "CORE",
        _ => "OTHER",
    }
    .to_string()
}

/// One program header, in the shape `readelf -l` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    /// `LOAD` / `PHDR` / `GNU_STACK` / ...
    pub kind: String,
    /// `p_offset`.
    pub file_offset: u64,
    /// `p_vaddr`.
    ///
    /// `object` 0.40 exposes `p_vaddr` (`ObjectSegment::address`) but **not**
    /// `p_paddr`.  On every fixture here the two are equal, and inventing a
    /// separate `p_paddr` field that silently mirrored `p_vaddr` would be a
    /// fabricated fact; the report records this as the one `readelf -l` column
    /// this crate does not reproduce.
    pub virtual_address: u64,
    /// `p_filesz`.
    pub file_size: u64,
    /// `p_memsz`.  When this exceeds `file_size` the tail is **BSS: there are no
    /// bytes in the file for it** (research D §2.4) and the hex overlay must
    /// render that part differently.
    pub memory_size: u64,
    /// `readelf -l`'s three-character flag field, e.g. `R E`, `RW `.
    pub flags: String,
    /// `p_align`.
    pub alignment: u64,
}

impl SegmentInfo {
    /// Does this segment carry file bytes at `vaddr`?
    ///
    /// False for the BSS tail.  The hex overlay needs this to decide between
    /// "read the byte" and "this byte does not exist in the file".
    #[must_use]
    pub fn has_file_bytes_for_vaddr(&self, vaddr: u64) -> bool {
        vaddr >= self.virtual_address
            && vaddr < self.virtual_address.saturating_add(self.file_size)
    }

    /// Translate a virtual address inside this segment to a file offset.
    ///
    /// `None` outside the segment's file-backed range.  Each segment has its own
    /// `vaddr - offset` delta — research D §2.4 measured two `LOAD`s on
    /// `refkernel.elf` whose deltas differ — so this is deliberately per-segment
    /// and there is no global "base" anywhere in this crate.
    #[must_use]
    pub fn file_offset_for_vaddr(&self, vaddr: u64) -> Option<u64> {
        if self.has_file_bytes_for_vaddr(vaddr) {
            Some(self.file_offset + (vaddr - self.virtual_address))
        } else {
            None
        }
    }
}

// ------------------------------------------------------------------ reading ---

/// Read an artifact, refusing anything larger than [`MAX_ARTIFACT_BYTES`].
///
/// # Errors
/// * `E_NOT_FOUND` — missing or unreadable path.
/// * `E_INTERNAL` — the file exists but exceeds the in-memory parse limit.  The
///   message names both the size and the limit so the caller can decide.
pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|err| {
        PrincessError::not_found(format!("cannot stat {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    if meta.len() > MAX_ARTIFACT_BYTES {
        return Err(PrincessError::internal(format!(
            "{} is {} bytes, above the {MAX_ARTIFACT_BYTES}-byte in-memory parse limit",
            path.display(),
            meta.len()
        ))
        .with_detail(format!(
            "use the hex reader (crate::hex::HexReader) for images this large; it is \
             windowed and never reads the whole file"
        )));
    }
    let mut file = File::open(path).map_err(|err| {
        PrincessError::not_found(format!("cannot open {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    // Pre-size exactly; the file may grow between stat and read (a QEMU image
    // being written), in which case `read_to_end` appends and we simply keep the
    // extra bytes rather than truncating a live image mid-parse.
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.read_to_end(&mut bytes).map_err(|err| {
        PrincessError::internal(format!("cannot read {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    Ok(bytes)
}

fn parse<'a>(bytes: &'a [u8], path: &Path) -> Result<object::File<'a>> {
    object::File::parse(bytes).map_err(|err| {
        PrincessError::internal(format!(
            "{} is not a parseable object file: {err}",
            path.display()
        ))
        .with_detail(err.to_string())
    })
}

// ------------------------------------------------------------------- facts ----

/// Read the ELF header facts of `path`.
///
/// # Errors
/// `E_NOT_FOUND` when the path cannot be read, `E_INTERNAL` when it is not a
/// parseable object file.  A text file or a raw binary must **never** look like
/// an ELF with an empty symbol table.
pub fn read_header(path: &Path) -> Result<ElfHeader> {
    let bytes = read_bounded(path)?;
    let file = parse(&bytes, path)?;
    Ok(ElfHeader {
        path: path.to_path_buf(),
        format: match file.format() {
            object::BinaryFormat::Elf => {
                if file.is_64() {
                    "elf64".to_string()
                } else {
                    "elf32".to_string()
                }
            }
            // A non-ELF container keeps `object`'s own spelling, so the caller
            // is never told "elf64" about something that is not an ELF.
            other => format!("{other:?}").to_ascii_lowercase(),
        },
        // `readelf -h` spells `e_type` as the bare `EXEC`/`DYN`/`REL` token
        // (the `(Executable file)` gloss is a separate column), so map to that
        // spelling instead of `object`'s prose.
        kind: elf_kind(file.kind()),
        architecture: format!("{:?}", file.architecture()),
        endianness: match file.endianness() {
            object::Endianness::Little => "little".to_string(),
            object::Endianness::Big => "big".to_string(),
        },
        class: match file.format() {
            object::BinaryFormat::Elf => {
                if file.is_64() {
                    "ELFCLASS64".to_string()
                } else {
                    "ELFCLASS32".to_string()
                }
            }
            other => format!("{other:?}"),
        },
        entry_point: file.entry(),
        section_count: file.sections().count(),
        segment_count: file.segments().count(),
        build_id: read_build_id(&file),
    })
}

/// Just the entry point, for callers that only need `e_entry`.
///
/// # Errors
/// Same as [`read_header`].
pub fn read_entry_point(path: &Path) -> Result<u64> {
    Ok(read_header(path)?.entry_point)
}

/// Read the section headers, in file order, rendered the way `readelf -S` does.
///
/// # Errors
/// `E_NOT_FOUND` / `E_INTERNAL` as above; also `E_INTERNAL` if `object` cannot
/// decode a section name (a corrupt `.shstrtab`), because guessing a name would
/// put a fabricated string in front of the user.
pub fn read_sections(path: &Path) -> Result<Vec<SectionInfo>> {
    let bytes = read_bounded(path)?;
    let file = parse(&bytes, path)?;
    let mut out = Vec::new();
    for section in file.sections() {
        let name = section.name().map_err(|err| {
            PrincessError::internal(format!(
                "{} has a section with an unreadable name: {err}",
                path.display()
            ))
            .with_detail(err.to_string())
        })?;
        out.push(SectionInfo {
            name: name.to_string(),
            address: section.address(),
            size: section.size(),
            flags: section_flags(&section),
        });
    }
    Ok(out)
}

/// Read the program headers, in file order, rendered the way `readelf -l` does.
///
/// # Errors
/// Same as [`read_sections`].
pub fn read_segments(path: &Path) -> Result<Vec<SegmentInfo>> {
    let bytes = read_bounded(path)?;
    let file = parse(&bytes, path)?;
    Ok(file
        .segments()
        .map(|segment| SegmentInfo {
            kind: segment_kind(&segment),
            file_offset: segment.file_range().0,
            virtual_address: segment.address(),
            file_size: segment.file_range().1,
            memory_size: segment.size(),
            flags: segment_flags(segment.flags()),
            alignment: segment.align(),
        })
        .collect())
}

/// Read the static symbol table, in link order.
///
/// # Errors
/// Same as [`read_sections`].
pub fn read_symbols(path: &Path) -> Result<Vec<SymbolInfo>> {
    let bytes = read_bounded(path)?;
    let file = parse(&bytes, path)?;
    let mut out = Vec::new();
    for symbol in file.symbols() {
        let name = match symbol.name() {
            Ok(name) => name,
            // An unreadable name is corruption, not "the empty symbol".  Show it
            // as such instead of silently emitting a nameless entry.
            Err(_) => "<unreadable name>",
        };
        out.push(SymbolInfo {
            name: name.to_string(),
            address: symbol.address(),
            size: symbol.size(),
            kind: symbol_kind(&symbol),
        });
    }
    Ok(out)
}

/// `readelf`'s section flag letters, in `readelf`'s order.
///
/// `readelf -S` prints these as a fixed 3-character field from the set
/// `W`(write) `A`(alloc) `X`(execute) `M`(merge) `S`(strings) `I`(info)
/// `L`(link order) `O`(OS specific) `G`(group) `T`(TLS) `C`(compressed) and
/// `E`(exclude).  `object` exposes `SHF_OS_NONCONFORMING` but nothing that
/// distinguishes it from other OS bits in a way `readelf` records as `O`, so `O`
/// is not emitted; that gap is documented rather than papered over with a guess.
fn section_flags(section: &object::Section<'_, '_>) -> String {
    use object::elf as e;

    let object::SectionFlags::Elf { sh_flags, .. } = section.flags() else {
        return String::new();
    };
    let mut out = String::new();
    // `object`'s flag macro emits the individual flags as **free** constants in
    // the `elf` module (`elf::SHF_WRITE`), typed as `SectionFlags`, and the
    // newtype has `contains`/`intersects` for testing them.
    for (flag, letter) in [
        (e::SHF_WRITE, 'W'),
        (e::SHF_ALLOC, 'A'),
        (e::SHF_EXECINSTR, 'X'),
        (e::SHF_MERGE, 'M'),
        (e::SHF_STRINGS, 'S'),
        (e::SHF_INFO_LINK, 'I'),
        (e::SHF_LINK_ORDER, 'L'),
        (e::SHF_GROUP, 'G'),
        (e::SHF_TLS, 'T'),
        (e::SHF_COMPRESSED, 'C'),
        (e::SHF_EXCLUDE, 'E'),
    ] {
        if sh_flags.contains(flag) {
            out.push(letter);
        }
    }
    out
}

/// `readelf -l`'s three-character flag field: read / write / execute.
fn segment_flags(flags: object::SegmentFlags) -> String {
    let object::SegmentFlags::Elf { p_flags, .. } = flags else {
        return "   ".to_string();
    };
    let mut out = [' '; 3];
    if p_flags.contains(object::elf::PF_R) {
        out[0] = 'R';
    }
    if p_flags.contains(object::elf::PF_W) {
        out[1] = 'W';
    }
    if p_flags.contains(object::elf::PF_X) {
        out[2] = 'E';
    }
    out.iter().collect()
}

/// `readelf -l`'s `Type` column, taken from `p_type` rather than inferred.
fn segment_kind(segment: &object::Segment<'_, '_>) -> String {
    let object::SegmentFlags::Elf { p_type, .. } = segment.flags() else {
        return "OTHER".to_string();
    };
    match p_type {
        object::elf::PT_LOAD => "LOAD",
        object::elf::PT_DYNAMIC => "DYNAMIC",
        object::elf::PT_INTERP => "INTERP",
        object::elf::PT_NOTE => "NOTE",
        object::elf::PT_PHDR => "PHDR",
        object::elf::PT_TLS => "TLS",
        object::elf::PT_GNU_EH_FRAME => "GNU_EH_FRAME",
        object::elf::PT_GNU_STACK => "GNU_STACK",
        object::elf::PT_GNU_RELRO => "GNU_RELRO",
        object::elf::PT_GNU_PROPERTY => "GNU_PROPERTY",
        _ => "OTHER",
    }
    .to_string()
}

fn symbol_kind(symbol: &object::Symbol<'_, '_>) -> String {
    match symbol.kind() {
        object::SymbolKind::Text => "FUNC",
        object::SymbolKind::Data => "OBJECT",
        object::SymbolKind::Section => "SECTION",
        object::SymbolKind::File => "FILE",
        object::SymbolKind::Label => "NOTYPE",
        object::SymbolKind::Tls => "TLS",
        // `SymbolKind` is `#[non_exhaustive]`-shaped in practice: object may add
        // variants, and a new one must degrade to NOTYPE rather than fail to
        // compile a future upgrade for a cosmetic reason.
        _ => "NOTYPE",
    }
    .to_string()
}

/// The GNU build-id note payload as lower-case hex, if the image has one.
///
/// Returns `None` for `--build-id=none` images.  There is deliberately **no**
/// fallback to a content hash: the contract forbids inventing the fact.
fn read_build_id(file: &object::File<'_>) -> Option<String> {
    let data = file.build_id().ok().flatten()?;
    if data.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(data.len() * 2);
    for byte in data {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name)
    }

    fn refkernel() -> PathBuf {
        fixture("refkernel/build/refkernel.elf")
    }

    fn pagingkernel() -> PathBuf {
        fixture("paging-kernel/build/pagingkernel.elf")
    }

    fn have(path: &Path) -> bool {
        path.is_file()
    }

    #[test]
    fn refkernel_header_matches_readelf() {
        let path = refkernel();
        if !have(&path) {
            return; // fixture not built in this checkout
        }
        let header = read_header(&path).expect("refkernel header");
        assert_eq!(header.format, "elf64");
        assert_eq!(header.kind, "EXEC", "readelf prints EXEC, not `Executable`");
        assert_eq!(header.architecture, "X86_64");
        assert_eq!(header.endianness, "little");
        assert_eq!(header.class, "ELFCLASS64");
        // Golden from `readelf -hW fixtures/refkernel/build/refkernel.elf`:
        //   Type: EXEC (Executable file)   Entry point address: 0x100040
        assert_eq!(header.entry_point, 0x10_0040);
        // --build-id=none => null, never a fabricated hash.
        assert_eq!(header.build_id, None);
    }

    #[test]
    fn refkernel_symbols_contain_the_contract_constant() {
        let path = refkernel();
        if !have(&path) {
            return;
        }
        let symbols = read_symbols(&path).expect("refkernel symbols");
        let probe = symbols
            .iter()
            .find(|symbol| symbol.name == "refkernel_fault_probe")
            .expect("D3 fixed assertion constant refkernel_fault_probe must exist");
        assert_eq!(probe.kind, "FUNC");
        // `readelf -sW` golden values.  Note `refkernel_fault_probe` is *not*
        // the function start: it is the pre-probe label at `ud2`.  D3's
        // `FAULT_RIP=0x100b3d` lands 4 bytes into it.
        assert_eq!(probe.address, 0x10_0b39);
        assert_eq!(probe.size, 21);
    }

    #[test]
    fn pagingkernel_page_table_symbols_match_the_walk_ground_truth() {
        let path = pagingkernel();
        if !have(&path) {
            return;
        }
        let symbols = read_symbols(&path).expect("pagingkernel symbols");
        let by_name = |needle: &str| {
            symbols
                .iter()
                .find(|symbol| symbol.name == needle)
                .unwrap_or_else(|| panic!("symbol {needle} missing"))
                .address
        };
        // These are the addresses P5-5's CR3 walk depends on; the guest itself
        // prints them on the serial line at boot.
        assert_eq!(by_name("pml4"), 0x10_4000);
        assert_eq!(by_name("pdpt"), 0x10_5000);
        assert_eq!(by_name("pd"), 0x10_6000);
        assert_eq!(by_name("pt"), 0x10_7000);
        assert_eq!(by_name("pagingkernel_gdt"), 0x10_12e0);
        assert_eq!(by_name("pagingkernel_idt"), 0x10_8000);
    }

    #[test]
    fn pagingkernel_segments_translate_their_own_way() {
        let path = pagingkernel();
        if !have(&path) {
            return;
        }
        let segments = read_segments(&path).expect("segments");
        for segment in &segments {
            if segment.file_size == 0 {
                continue;
            }
            // Round-trip: vaddr -> offset -> vaddr.
            let vaddr = segment.virtual_address;
            let offset = segment
                .file_offset_for_vaddr(vaddr)
                .expect("a file-backed segment must map its own start");
            assert_eq!(offset, segment.file_offset);
            assert!(segment.has_file_bytes_for_vaddr(vaddr));
            // One byte past filesz is BSS, not file content.
            assert!(!segment.has_file_bytes_for_vaddr(vaddr + segment.file_size));
        }
    }

    #[test]
    fn missing_file_is_not_found_not_internal() {
        let err = read_header(Path::new("/nonexistent/definitely-not-here.elf")).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
    }

    #[test]
    fn a_text_file_is_loudly_rejected() {
        let dir = std::env::temp_dir().join("princess-bin-elf-neg");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-an-elf.bin");
        std::fs::write(&path, b"this is not an object file at all\n").unwrap();
        let err = read_header(&path).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
        assert!(
            err.message.contains("not a parseable object file"),
            "message must name the reason, got: {}",
            err.message
        );
    }

    #[test]
    fn section_flags_render_like_readelf_letters() {
        let path = refkernel();
        if !have(&path) {
            return;
        }
        let sections = read_sections(&path).expect("sections");
        let text = sections
            .iter()
            .find(|section| section.name == ".text")
            .expect(".text");
        assert_eq!(text.flags, "AX");
        let bss = sections
            .iter()
            .find(|section| section.name == ".bss")
            .expect(".bss");
        assert_eq!(bss.flags, "WA");
    }
}
