//! x86/x86_64 disassembly with **`iced-x86` 1.21.0** (D20, frozen).
//!
//! # What this module is really for
//!
//! P5-2 asks for instruction-level agreement with `objdump -d`.  But D20 adds a
//! requirement that is *not* about agreement: the disassembly view must present
//! **"disassembly + source line" side by side**.  Research D §1.2 measured why —
//! with `-O2`, `addr2line` attributes an address to the inlined callee's own
//! line rather than the call site, so a single number shown as "the line" is
//! wrong in exactly the code that matters most.
//!
//! So this module has two entry points:
//!
//! * [`Disassembler::disassemble`] — a flat instruction list, the unit of
//!   comparison with `objdump -d`.
//! * [`Disassembler::with_source`] — the same instructions **plus** the source
//!   rows that cover the requested address range, emitted as
//!   [`SourceRow`]s carrying an **address interval** and a **line range**, not a
//!   point.  A UI renders the two columns by zipping these rows against the
//!   instruction list; it never has to probe per instruction, and it always has
//!   enough information to draw the grouping.
//!
//! # Decoding from file bytes, not from a process image
//!
//! `iced-x86` wants a byte slice plus a base IP.  We give it the segment's file
//! bytes at the right offset (via the per-segment delta translation in
//! [`crate::elf::SegmentInfo::file_offset_for_vaddr`]) and set the decoder's IP
//! to the *virtual* address, so the emitted operands are link-time addresses and
//! match `objdump` directly.  There is deliberately no global "load base": the
//! fixtures run at their link address, and a relocated image is the caller's
//! translation to apply ([`DisassemblerOptions::base_offset`] exists for that
//! and defaults to zero).

use std::path::{Path, PathBuf};

use iced_x86::{
    Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, MemorySizeOptions,
    NasmFormatter,
};
use princess_core::{DisassembledInstruction, PrincessError, Result};

use crate::elf::SegmentInfo;
use crate::source::{LineRange, LineTable};

/// Output syntax for the instruction text.
///
/// `Intel` and `Nasm` are what a kernel developer expects; `Gas` is offered
/// because it is the closest match to `objdump`'s default (AT&T) output and is
/// how the module cross-checks against `objdump -d` *without* `-M intel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Syntax {
    /// `mov rax, qword ptr [rbx+8]` — the default, and the same syntax
    /// `objdump -d -M intel` emits.
    #[default]
    Intel,
    /// NASM-style, matching the project's assembly fixtures.
    Nasm,
    /// AT&T-style, matching bare `objdump -d`.
    Gas,
    /// Intel syntax configured to agree with `objdump -d -M intel`.
    ///
    /// The default `Intel` formatter writes hex in NASM convention
    /// (`1011CDh`) and omits memory size prefixes; `objdump` writes
    /// `0x1011cd` and, for some operands, `DWORD PTR`.  Both are the same
    /// instruction, but the acceptance criterion for P5-2 is agreement with
    /// `objdump`, and a formatter that matches it directly removes a whole class
    /// of "is this a decoder bug or a formatting difference?" doubt from the
    /// comparison.
    ///
    /// Specifically this mode sets:
    /// * `hex_prefix = "0x"`, `hex_suffix = ""`;
    /// * lower-case hex (`objdump` is lower-case);
    /// * **no** leading zeros on numbers (`objdump` writes `0x4`, not
    ///   `0x00000004`);
    /// * `small_hex_numbers_in_decimal = false`, so `0x0` stays `0x0` instead of
    ///   becoming `0`;
    /// * `rip_relative_addresses = false`, so a RIP-relative operand shows the
    ///   absolute target the way `objdump` resolves it;
    /// * memory size keywords only when they are ambiguous.
    ObjdumpIntel,
}

impl Syntax {
    fn make_formatter(self) -> Box<dyn Formatter> {
        match self {
            Syntax::Intel => Box::new(IntelFormatter::new()),
            Syntax::Nasm => Box::new(NasmFormatter::new()),
            Syntax::Gas => Box::new(iced_x86::GasFormatter::new()),
            Syntax::ObjdumpIntel => {
                let mut formatter = IntelFormatter::new();
                let options = formatter.options_mut();
                options.set_hex_prefix("0x");
                options.set_hex_suffix("");
                options.set_uppercase_hex(false);
                options.set_leading_zeros(false);
                options.set_small_hex_numbers_in_decimal(false);
                options.set_branch_leading_zeros(false);
                options.set_rip_relative_addresses(false);
                options.set_space_after_operand_separator(false);
                // `objdump` prints the size keyword only where it is needed to
                // disambiguate an immediate from a memory operand.
                options.set_memory_size_options(MemorySizeOptions::Minimal);
                Box::new(formatter)
            }
        }
    }

    /// The `objdump` flag that produces the same syntax, for report writing.
    #[must_use]
    pub fn objdump_flag(self) -> &'static str {
        match self {
            Syntax::Intel => "-M intel",
            Syntax::Nasm => "(nasm syntax; compare mnemonics, not punctuation)",
            Syntax::Gas => "(default AT&T)",
            Syntax::ObjdumpIntel => "-M intel",
        }
    }
}

/// Knobs for one disassembly run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisassemblerOptions {
    /// Output syntax.
    pub syntax: Syntax,
    /// Add `0x` / `h` style number formatting as the syntax's formatter's
    /// defaults dictate (off gives the shortest form).
    pub hex_prefix: bool,
    /// Value subtracted from the *decoded* instruction pointers before they are
    /// reported, so a relocated image can be shown at its link address.
    ///
    /// Zero by default.  Both fixtures are linked at the address they run at, so
    /// the honest default is "no translation" rather than an assumed base.
    pub base_offset: u64,
    /// Emit the raw instruction bytes alongside the text.  Always on for the
    /// `objdump` comparison path; kept switchable because the hex column is the
    /// expensive part to format.
    pub bytes: bool,
    /// Attach the static symbol whose `[address, address+size)` covers each
    /// instruction.  Cost is one sorted symbol table plus a binary search per
    /// instruction, so it is on by default — the UI wants it.
    pub symbols: bool,
}

impl Default for DisassemblerOptions {
    fn default() -> Self {
        Self {
            syntax: Syntax::Intel,
            hex_prefix: true,
            base_offset: 0,
            bytes: true,
            symbols: true,
        }
    }
}

/// One source row covering part of a disassembly range.
///
/// Both ends are ranges: `addresses` is the half-open interval of instructions
/// this line covers, and `line` names the source line.  That pairing is what
/// lets the UI render "instruction | source" side by side without a per-row
/// lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// Half-open address interval of instructions attributed to this row.
    pub addresses: LineRange,
    /// How many decoded instructions fall inside `addresses`.
    pub instruction_count: usize,
    /// True when this row was produced from a function that DWARF says is
    /// inlined here.  The UI is required by D20 to distinguish this case rather
    /// than presenting it as "the line you are on".
    pub inlined: bool,
}

/// A disassembly plus its source map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisassemblyView {
    /// Instructions in address order.
    pub instructions: Vec<DisassembledInstruction>,
    /// Source rows covering the same address span, in address order.
    pub source_rows: Vec<SourceRow>,
    /// The address the caller actually asked for (after range clamping).
    pub requested_address: u64,
    /// The address decoding actually started at.  Equal to the asked-for address
    /// in this implementation: the bytes are decoded from exactly the offset the
    /// caller named, and the first instruction is whatever lives there.
    ///
    /// `objdump --start-address` behaves the same way, which is what makes the
    /// two comparable.  There is deliberately no "snap backwards to an
    /// instruction boundary" pass: silently decoding from an address the caller
    /// did not ask for would make the `objdump` diff disagree for reasons that
    /// are not about instruction decoding.
    pub start_address: u64,
}

/// A loaded artifact ready to disassemble.
pub struct Disassembler {
    path: PathBuf,
    segments: Vec<SegmentInfo>,
    symbols: Vec<(u64, u64, String)>,
    lines: LineTable,
    options: DisassemblerOptions,
}

impl std::fmt::Debug for Disassembler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disassembler")
            .field("path", &self.path)
            .field("segments", &self.segments.len())
            .field("symbols", &self.symbols.len())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Disassembler {
    /// Open `path` and prepare to disassemble it.
    ///
    /// # Errors
    /// * `E_NOT_FOUND` — the artifact does not exist.
    /// * `E_INTERNAL` — it is not a parseable object file, or its segment table
    ///   cannot be read.
    pub fn open(path: &Path, options: DisassemblerOptions) -> Result<Self> {
        let segments = crate::elf::read_segments(path)?;
        let symbols = if options.symbols {
            crate::elf::read_symbols(path)?
                .into_iter()
                .filter(|symbol| symbol.kind == "FUNC" && symbol.size > 0)
                .map(|symbol| (symbol.address, symbol.address + symbol.size, symbol.name))
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            path: path.to_path_buf(),
            segments,
            symbols,
            lines: LineTable::open(path)?,
            options,
        })
    }

    /// The artifact under inspection.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decode `count` instructions starting at `address`.
    ///
    /// # Errors
    /// * `E_NOT_FOUND` — `address` is not inside any file-backed segment.  This
    ///   is the honest answer for "disassemble unmapped memory from a static
    ///   file": there are no bytes to decode.  Once the debugger is attached,
    ///   live memory is read through `DebugBackend` instead.
    /// * `E_INTERNAL` — the artifact cannot be re-read.
    pub fn disassemble(&self, address: u64, count: u64) -> Result<Vec<DisassembledInstruction>> {
        let (bytes, start, _) = self.slice_for(address)?;
        Ok(self.decode(&bytes, start, count))
    }

    /// Decode from an already-read buffer.
    fn decode(&self, bytes: &[u8], base: u64, count: u64) -> Vec<DisassembledInstruction> {
        let mut decoder = Decoder::with_ip(64, bytes, base, DecoderOptions::NONE);
        let mut formatter = self.options.syntax.make_formatter();
        let mut instruction = Instruction::default();
        let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        while out.len() < usize::try_from(count).unwrap_or(usize::MAX) && decoder.can_decode() {
            decoder.decode_out(&mut instruction);
            if instruction.is_invalid() {
                // `iced-x86` emits an invalid instruction rather than panicking.
                // Reporting it honestly (with its bytes) is far better than
                // silently stopping, because "the decoder gave up here" is
                // exactly the signal a kernel developer needs.
                out.push(self.render_invalid(bytes, base, &instruction));
                continue;
            }
            out.push(self.render(bytes, base, &instruction, &mut *formatter));
        }
        out
    }

    /// Decode `count` instructions and attach the covering source rows.
    ///
    /// This is the shape D20 requires of the disassembly view: the instruction
    /// column and the source column come from the same call, and the source side
    /// carries **ranges** ([`SourceRow::addresses`]) rather than a lone line
    /// number, so the UI can group.
    ///
    /// # Errors
    /// Same as [`Self::disassemble`], plus the DWARF errors from the line lookup.
    /// Missing debug info is **not** an error: `source_rows` is simply empty and
    /// the UI shows the assembly alone.
    pub fn with_source(&mut self, address: u64, count: u64) -> Result<DisassemblyView> {
        let instructions = self.disassemble(address, count)?;
        let requested_address = address;
        let start_address = instructions
            .first()
            .and_then(|instruction| parse_hex_address(&instruction.address))
            .unwrap_or(address);

        let span_end = instructions
            .last()
            .map(|instruction| {
                parse_hex_address(&instruction.address).unwrap_or(start_address)
                    + (instruction.bytes.len() / 2) as u64
            })
            .unwrap_or(start_address);

        // Collect the address of every instruction first so the source-row pass
        // does not hold a borrow of `self`.
        let instruction_addresses: Vec<u64> = instructions
            .iter()
            .filter_map(|instruction| parse_hex_address(&instruction.address))
            .collect();

        let mut source_rows = Vec::new();
        if span_end > start_address {
            let segments = self.lines.segments()?;
            for segment in &segments {
                if segment.end <= start_address || segment.start >= span_end {
                    continue;
                }
                let clipped_start = segment.start.max(start_address);
                let clipped_end = segment.end.min(span_end);
                let instruction_count = instruction_addresses
                    .iter()
                    .filter(|at| **at >= clipped_start && **at < clipped_end)
                    .count();
                source_rows.push(SourceRow {
                    addresses: LineRange {
                        start: clipped_start,
                        end: clipped_end,
                        ..segment.clone()
                    },
                    instruction_count,
                    inlined: segment.inlined_into.is_some(),
                });
            }
        }

        Ok(DisassemblyView {
            instructions,
            source_rows,
            requested_address,
            start_address,
        })
    }

    /// The source line covering `address`, in the point form the `BinProvider`
    /// contract asks for.
    ///
    /// # Errors
    /// DWARF load failures.
    pub fn source_line(&mut self, address: u64) -> Result<Option<princess_core::SourceLocation>> {
        self.lines.location_for_address(address)
    }

    /// The address interval sharing `address`'s source line.
    ///
    /// # Errors
    /// DWARF load failures.
    pub fn source_range(&mut self, address: u64) -> Result<Option<LineRange>> {
        self.lines.line_range_for_address(address)
    }

    /// Where in the file `address` lives, and the byte range from that offset.
    ///
    /// Returns `(bytes, decoded_base_address, file_offset)`.
    fn slice_for(&self, address: u64) -> Result<(Vec<u8>, u64, u64)> {
        // Resolve the requested address to (segment, file offset).
        let target = self
            .segments
            .iter()
            .find_map(|segment| {
                let offset = segment.file_offset_for_vaddr(address)?;
                Some((segment.clone(), offset))
            })
            .ok_or_else(|| {
                PrincessError::not_found(format!(
                    "address {address:#x} is not inside any file-backed segment of {}",
                    self.path.display()
                ))
                .with_detail(
                    "a static disassembly can only decode bytes that exist in the file; \
                     live/unmapped memory is the debugger's job"
                        .to_string(),
                )
            })?;
        let (segment, file_offset) = target;

        // How many file-backed bytes remain *from the requested address to the
        // end of this segment*.  This is `p_filesz - (vaddr - p_vaddr)`, **not**
        // `p_filesz - p_offset`: `file_offset` is already the absolute offset
        // into the file, which is normally far larger than the segment's own
        // file size, and subtracting the two underflows to zero — silently
        // decoding nothing.  Cap it so a 100 MB `.text` is not copied whole for
        // a 10-instruction request.
        let consumed_in_segment = address.saturating_sub(segment.virtual_address);
        let remaining = segment
            .file_size
            .saturating_sub(consumed_in_segment)
            .min(MAX_DECODE_BYTES);
        let bytes = read_at(&self.path, file_offset, remaining)?;
        Ok((bytes, address, file_offset))
    }

    fn render(
        &self,
        buffer: &[u8],
        buffer_base: u64,
        instruction: &Instruction,
        formatter: &mut dyn Formatter,
    ) -> DisassembledInstruction {
        let address = instruction_address(instruction).wrapping_sub(self.options.base_offset);
        let mut text = String::new();
        formatter.format(instruction, &mut text);
        let bytes = if self.options.bytes {
            instruction_bytes_at(buffer, buffer_base, instruction)
        } else {
            String::new()
        };
        DisassembledInstruction {
            address: format!("{address:016x}"),
            bytes,
            text: text.trim().to_string(),
            symbol: self.symbol_at(address),
            source: None,
        }
    }

    fn render_invalid(
        &self,
        buffer: &[u8],
        buffer_base: u64,
        instruction: &Instruction,
    ) -> DisassembledInstruction {
        let address = instruction_address(instruction).wrapping_sub(self.options.base_offset);
        DisassembledInstruction {
            address: format!("{address:016x}"),
            bytes: instruction_bytes_at(buffer, buffer_base, instruction),
            // `(bad)` is `objdump`'s own spelling for an undecodable byte.
            text: "(bad)".to_string(),
            symbol: self.symbol_at(address),
            source: None,
        }
    }

    fn symbol_at(&self, address: u64) -> Option<String> {
        self.symbols
            .iter()
            .find(|(start, end, _)| address >= *start && address < *end)
            .map(|(_, _, name)| crate::demangle_name(name))
    }
}

/// Upper bound on bytes copied for one decode run.
const MAX_DECODE_BYTES: u64 = 1 << 20;

fn read_at(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|err| {
        PrincessError::not_found(format!("cannot open {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    file.seek(SeekFrom::Start(offset)).map_err(|err| {
        PrincessError::internal(format!("cannot seek {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    let mut out = vec![0u8; usize::try_from(len).unwrap_or(0)];
    let read = file.read(&mut out).map_err(|err| {
        PrincessError::internal(format!("cannot read {}: {err}", path.display()))
            .with_detail(err.to_string())
    })?;
    out.truncate(read);
    Ok(out)
}

/// The virtual address an instruction *starts* at.
///
/// `iced_x86::Instruction::ip()` is already this.  The crate stores `next_rip`
/// internally and `ip()` returns `next_rip - len`, with `next_ip()` as the
/// separate accessor for the address *after* the instruction.  This helper exists
/// only so the intent is stated once: the address to show is `ip()`, and a
/// relative branch's target is computed by iced from `next_ip()` internally.
fn instruction_address(instruction: &Instruction) -> u64 {
    instruction.ip()
}

/// Render the raw instruction bytes.
///
/// `iced-x86` intentionally does not retain the encoding, so the bytes are taken
/// from the buffer the decoder was fed: the instruction's start address minus the
/// buffer's base is exactly the byte offset.
fn instruction_bytes_at(buffer: &[u8], buffer_base: u64, instruction: &Instruction) -> String {
    let start = instruction_address(instruction).saturating_sub(buffer_base) as usize;
    let length = instruction.len();
    let mut bytes = String::with_capacity(length * 2);
    for index in 0..length {
        use std::fmt::Write as _;
        match buffer.get(start + index) {
            Some(byte) => {
                let _ = write!(bytes, "{byte:02x}");
            }
            // Past the end of the read window: this is a decoder/read bug, and
            // printing `??` makes it visible instead of silently short.
            None => bytes.push_str("??"),
        }
    }
    bytes
}

fn parse_hex_address(text: &str) -> Option<u64> {
    u64::from_str_radix(text, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refkernel() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/refkernel/build/refkernel.elf")
    }

    fn have() -> bool {
        refkernel().is_file()
    }

    #[test]
    fn fault_probe_disassembles_to_the_contract_ud2() {
        if !have() {
            return;
        }
        let disassembler = Disassembler::open(&refkernel(), DisassemblerOptions::default())
            .expect("open");
        // D3: refkernel_fault_probe is at 0x100b39; the fault RIP is 0x100b3d.
        let instructions = disassembler.disassemble(0x10_0b39, 6).expect("disassemble");
        assert!(!instructions.is_empty());
        assert_eq!(instructions[0].address, "0000000000100b39");
        // Golden `objdump -d -M intel` bytes for the probe.
        assert_eq!(instructions[0].bytes, "55", "push rbp");
        assert_eq!(instructions[1].bytes, "4889e5", "mov rbp, rsp");
        assert_eq!(instructions[2].bytes, "0f0b", "ud2");
        assert_eq!(instructions[2].address, "0000000000100b3d");
        assert!(
            instructions[2].text.contains("ud2"),
            "got {}",
            instructions[2].text
        );
    }

    #[test]
    fn instruction_bytes_match_the_file_at_the_segment_offset() {
        if !have() {
            return;
        }
        let disassembler = Disassembler::open(&refkernel(), DisassemblerOptions::default())
            .expect("open");
        let instructions = disassembler.disassemble(0x10_0040, 4).expect("disassemble");
        // Read the same bytes straight out of the file using the segment delta.
        let segments = crate::elf::read_segments(&refkernel()).expect("segments");
        for instruction in &instructions {
            let address = parse_hex_address(&instruction.address).unwrap();
            let offset = segments
                .iter()
                .find_map(|segment| segment.file_offset_for_vaddr(address))
                .expect("instruction must be inside a file-backed segment");
            let mut expected = String::new();
            for index in 0..instruction.bytes.len() / 2 {
                use std::fmt::Write as _;
                let _ = write!(expected, "{:02x}", read_byte(&refkernel(), offset + index as u64));
            }
            assert_eq!(instruction.bytes, expected, "at {address:#x}");
        }
    }

    fn read_byte(path: &Path, offset: u64) -> u8 {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0]
    }

    #[test]
    fn gas_syntax_is_available_for_the_objdump_default_comparison() {
        if !have() {
            return;
        }
        let options = DisassemblerOptions {
            syntax: Syntax::Gas,
            ..DisassemblerOptions::default()
        };
        let disassembler = Disassembler::open(&refkernel(), options).expect("open");
        let instructions = disassembler.disassemble(0x10_0b39, 3).expect("disassemble");
        // AT&T writes the source last and uses `%` registers.
        assert!(
            instructions[0].text.contains("%rbp"),
            "got {}",
            instructions[0].text
        );
    }

    #[test]
    fn with_source_returns_address_ranges_not_a_lone_line() {
        if !have() {
            return;
        }
        let mut disassembler =
            Disassembler::open(&refkernel(), DisassemblerOptions::default()).expect("open");
        let view = disassembler.with_source(0x10_0b39, 24).expect("view");
        assert!(!view.instructions.is_empty());
        assert!(
            !view.source_rows.is_empty(),
            "refkernel has DWARF; the source column must be populated"
        );
        for row in &view.source_rows {
            assert!(row.addresses.start < row.addresses.end, "empty range");
            assert!(!row.addresses.file.is_empty());
            assert!(row.addresses.line > 0, "line numbers are 1-based");
        }
        // The rows must tile the requested span without overlapping.
        for pair in view.source_rows.windows(2) {
            assert!(pair[0].addresses.end <= pair[1].addresses.start);
        }
        // And at least one row must be the D3 contract line.
        assert!(
            view.source_rows
                .iter()
                .any(|row| row.addresses.line == 100 && row.addresses.file.ends_with("kernel.c")),
            "rows: {:?}",
            view.source_rows
        );
    }

    #[test]
    fn objdump_intel_uses_the_objdump_radix_and_branch_conventions() {
        if !have() {
            return;
        }
        let options = DisassemblerOptions {
            syntax: Syntax::ObjdumpIntel,
            ..DisassemblerOptions::default()
        };
        let disassembler = Disassembler::open(&refkernel(), options).expect("open");
        let instructions = disassembler.disassemble(0x10_0b39, 12).expect("disassemble");
        let texts: Vec<&str> = instructions.iter().map(|i| i.text.as_str()).collect();
        // `0x`-prefixed, lower-case, no `h` suffix, no leading zeros.
        assert_eq!(texts[0], "push rbp");
        assert_eq!(texts[1], "mov rbp,rsp");
        assert_eq!(texts[2], "ud2");
        assert_eq!(texts[3], "mov edi,0x101180");
        assert_eq!(texts[4], "call 0x1002f5");
        for text in &texts {
            // A NASM-suffixed number ends in `h` after a hex digit; the check is
            // on that shape, not on the letter appearing anywhere (mnemonics
            // like `push` and `shl` legitimately contain an `h`).
            assert!(
                !text.split_whitespace().any(|token| {
                    token.len() > 1
                        && token.ends_with('h')
                        && token[..token.len() - 1]
                            .chars()
                            .all(|c| c.is_ascii_hexdigit())
                        && token[..token.len() - 1].chars().any(|c| c.is_ascii_digit())
                }),
                "no NASM `h` suffix: {text}"
            );
            assert!(
                !text.contains("0x0000000000"),
                "no leading zeros: {text}"
            );
        }
    }

    #[test]
    fn disassembling_a_non_mapped_address_is_not_found() {
        if !have() {
            return;
        }
        let disassembler = Disassembler::open(&refkernel(), DisassemblerOptions::default())
            .expect("open");
        let err = disassembler.disassemble(0xdead_beef_0000, 4).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
        assert!(err.message.contains("not inside any file-backed segment"));
    }

    #[test]
    fn symbol_attribution_uses_the_demangled_name() {
        if !have() {
            return;
        }
        let disassembler = Disassembler::open(&refkernel(), DisassemblerOptions::default())
            .expect("open");
        let instructions = disassembler.disassemble(0x10_0b39, 1).expect("disassemble");
        assert_eq!(
            instructions[0].symbol.as_deref(),
            Some("refkernel_fault_probe")
        );
    }
}
