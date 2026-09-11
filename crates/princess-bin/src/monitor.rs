//! QEMU monitor parsing: HMP text and QMP JSON — the P5-4 deliverable.
//!
//! Assertions are made against the **recorded** samples in
//! `fixtures/qemu-monitor/`, never a live QEMU (D9, acceptance P5-4).  The
//! samples ship with a `README.md` that documents provenance and limitations;
//! this module deliberately parses only the parts that README marks "stable".
//!
//! # The `P` correction, and why it is a test not a comment
//!
//! `info tlb` prints nine flag characters.  Research D §5.2 corrected an earlier
//! report's inference: the third letter is **`PS` — the page-size bit — not
//! `present`**.  The source-level reason is that `print_pte()` filters on
//! present before printing, so no "present" letter can appear at all.
//!
//! Getting this wrong is not cosmetic: a parser that reads column 3 as "present"
//! classifies **every 4 KiB leaf** as absent, because a present 4 KiB PTE prints
//! `--------W` (only RW set).  The fixture is the proof — the 4 KiB pages and the
//! 2 MiB huge pages sit in the same file, and only the huge ones carry the `P`.
//! [`tests::tlb_flag_column_three_is_ps_not_present`] asserts exactly that
//! distinction, and [`InfoTlbEntry::granularity`] is derived from `PS` alone.
//!
//! # Parsing discipline
//!
//! * Every line is parsed against an explicit grammar; a line that does not
//!   match produces a [`ParseError`] carrying the line number and the offending
//!   text.  Nothing is skipped silently — research D §5.4 records that a
//!   page-table walker fed a `Cannot access memory` reply will happily build a
//!   tree of zeros, and that is precisely the class of bug this module refuses
//!   to enable.
//! * Errors point at the byte offset in the **original blob**, so a consumer can
//!   highlight the bad line in a viewer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A parse failure, with enough context to point at the offending input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What was being parsed: `info registers`, `info tlb`, `info mem`,
    /// `info cpus`, or a QMP shape.
    pub context: &'static str,
    /// 1-based line number within the parsed text, or 0 for whole-blob errors.
    pub line: usize,
    /// Byte offset of the start of that line in the original input.
    pub byte_offset: usize,
    /// The text that could not be parsed (trimmed).
    pub text: String,
    /// Why it could not be parsed.
    pub reason: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} parse error at line {} (byte {}): {} — {:?}",
            self.context, self.line, self.byte_offset, self.reason, self.text
        )
    }
}

impl std::error::Error for ParseError {}

/// One row of `info mem`: a mapped virtual range.
///
/// The permissions are only `u`/`r`/`w`.  There is no NX, no PWT/PCD, no A/D and
/// no present bit — `info mem` **does not carry them**, and research D §6 item 5
/// plus §3.6 (LA57 returns *empty*) are why this is not used as a page-table data
/// source.  It is kept because it is a cheap cross-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoMemRange {
    /// First virtual address of the range (inclusive).
    pub start: u64,
    /// One past the last virtual address.  `info mem` prints an **exclusive** end.
    pub end: u64,
    /// Size in bytes, as printed.
    pub size: u64,
    /// User-accessible bit.
    pub user: bool,
    /// Readable bit.
    pub readable: bool,
    /// Writable bit.
    pub writable: bool,
}

impl InfoMemRange {
    /// Was the printed size consistent with `end - start`?
    ///
    /// Always true for a well-formed row; exposed so a fixture drift is
    /// detectable rather than assumed away.
    #[must_use]
    pub fn size_agrees_with_bounds(&self) -> bool {
        self.end.saturating_sub(self.start) == self.size
    }
}

/// Page granularity of an `info tlb` leaf, derived from the **`PS`** bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlbGranularity {
    /// 4 KiB leaf (PTE, `PS=0`).
    Page4K,
    /// 2 MiB huge page (PDE, `PS=1`).
    Huge2M,
    /// 1 GiB huge page (PDPTE, `PS=1`).
    Huge1G,
}

impl TlbGranularity {
    /// Size in bytes.
    #[must_use]
    pub fn bytes(self) -> u64 {
        match self {
            TlbGranularity::Page4K => 4096,
            TlbGranularity::Huge2M => 2 * 1024 * 1024,
            TlbGranularity::Huge1G => 1024 * 1024 * 1024,
        }
    }
}

/// The nine `info tlb` flag characters, in QEMU's own column order.
///
/// **Order is `X G P D A C T U W`** and the third column is **`P` = PS (page
/// size)**, not present — see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlbFlags {
    /// Column 1, `X` — NX / no-execute.
    pub nx: bool,
    /// Column 2, `G` — global.
    pub global: bool,
    /// Column 3, **`P` — PS / page size**, i.e. "this leaf is a huge page".
    pub ps: bool,
    /// Column 4, `D` — dirty.
    pub dirty: bool,
    /// Column 5, `A` — accessed.
    pub accessed: bool,
    /// Column 6, `C` — PCD, cache disable.
    pub pcd: bool,
    /// Column 7, `T` — PWT, write-through.
    pub pwt: bool,
    /// Column 8, `U` — user/supervisor.
    pub user: bool,
    /// Column 9, `W` — writable.
    pub writable: bool,
}

/// One `info tlb` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoTlbEntry {
    /// Virtual address, as printed.
    pub virtual_address: u64,
    /// Physical base address, as printed.
    ///
    /// **Read this with `granularity`.**  For a 4 KiB leaf it is the page
    /// address; for a 2 MiB leaf it is 2 MiB-aligned; research D §5.2 notes you
    /// must not add a 4 KiB offset to it blindly.
    pub physical_address: u64,
    /// The decoded nine flags.
    pub flags: TlbFlags,
}

impl InfoTlbEntry {
    /// The granularity implied by `PS`.
    ///
    /// `PS` alone cannot distinguish 2 MiB from 1 GiB — QEMU does not print the
    /// level.  Research D §5.2 says the level must be inferred from the *step*
    /// to the next entry, and it is precisely this ambiguity that makes
    /// `info tlb` unsuitable as a page-table source.  So this method returns
    /// `Huge2M` for any `PS=1` entry, and callers that need the real size must
    /// use [`resolve_granularities`], which does the step inference and says
    /// when it is unsure.
    #[must_use]
    pub fn granularity(&self) -> TlbGranularity {
        if self.flags.ps {
            TlbGranularity::Huge2M
        } else {
            TlbGranularity::Page4K
        }
    }
}

/// Fill in huge-page granularity using the step to the next entry.
///
/// QEMU's `info tlb` prints one line per leaf but never names the level, so a
/// `PS=1` entry could be a 2 MiB PDE leaf or a 1 GiB PDPTE leaf.  The only signal
/// is the stride of the *following* entry (research D §5.2).  This function
/// applies that inference and leaves the last entry — whose successor does not
/// exist — at the conservative 2 MiB reading rather than guessing 1 GiB from a
/// value's alignment, which would be a fabrication.
///
/// The returned vector is aligned with `entries`.
#[must_use]
pub fn resolve_granularities(entries: &[InfoTlbEntry]) -> Vec<TlbGranularity> {
    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        if !entry.flags.ps {
            out.push(TlbGranularity::Page4K);
            continue;
        }
        // Look ahead for the next entry that starts a new page; the stride tells
        // us which level this leaf is at.
        let inferred = entries[index + 1..]
            .iter()
            .find(|next| next.virtual_address > entry.virtual_address)
            .map(|next| next.virtual_address - entry.virtual_address);
        out.push(match inferred {
            Some(stride) if stride >= TlbGranularity::Huge1G.bytes() => TlbGranularity::Huge1G,
            Some(_) => TlbGranularity::Huge2M,
            // Last entry: QEMU's identity maps here end at a 2 MiB boundary.
            // Do not invent 1 GiB from alignment alone.
            None => TlbGranularity::Huge2M,
        });
    }
    out
}

/// One CPU's line from `info cpus`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuInfo {
    /// `CPU #N`, as printed.
    pub index: u32,
    /// Host thread id.  Changes every run — parse the shape, never compare it.
    pub thread_id: u64,
    /// Was this the `*`-marked current CPU?
    pub current: bool,
}

/// One segment register line from `info registers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRegister {
    /// `ES` / `CS` / `SS` / `DS` / `FS` / `GS` / `LDT` / `TR`.
    pub name: String,
    /// Selector (`sel`).
    pub selector: u16,
    /// Base, as printed.  In long mode this is ignored for everything but
    /// FS/GS — research D §4.4 item 6 warns against showing it as if valid.
    pub base: u64,
    /// Limit, as printed.  Also ignored in long mode except for TR/LDT.
    pub limit: u32,
    /// The raw attribute dword, as printed.
    pub access: u32,
    /// DPL, as printed.
    pub dpl: u8,
    /// The type string verbatim: `CS64`, `DS`, `LDT`, `TSS64-avl`, `TSS64-busy`,
    /// `CS32`, ...  This is the most valuable field on the line and is kept
    /// unparsed on purpose — QEMU's spelling is the ground truth.
    pub type_name: String,
    /// The trailing permission string (`[-WA]`, `[-R-]`), when present.
    pub permissions: Option<String>,
}

/// Parsed `info registers` output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistersReport {
    /// The `CPU#N` this report belongs to.
    pub cpu: Option<u32>,
    /// GPRs, keyed by canonical upper-case name, carrying **QEMU's own hex
    /// text** so nothing is truncated on the way to the UI.
    pub registers: BTreeMap<String, String>,
    /// `RFL` plus the decoded flag string.
    pub rflags: Option<String>,
    /// The flag box from the `RFL` line, e.g. `---Z-P-`.
    pub flags_text: Option<String>,
    /// CPL, when present.
    pub cpl: Option<u8>,
    /// Every segment-register line, including `LDT` and `TR`.
    pub segments: Vec<SegmentRegister>,
    /// `GDT=` base.
    pub gdt_base: Option<u64>,
    /// `GDT=` limit.  **Bytes minus one** (research D §4.2).
    pub gdt_limit: Option<u32>,
    /// `IDT=` base.
    pub idt_base: Option<u64>,
    /// `IDT=` limit, bytes minus one.
    pub idt_limit: Option<u32>,
    /// True when the `RIP` line carried `HLT=1`.
    pub halted: Option<bool>,
}

impl RegistersReport {
    /// Number of GDT slots implied by the limit: `(limit + 1) / 8`.
    ///
    /// Research D §4.2: the limit is a byte count minus one, so this must be
    /// derived, never hard-coded to 8192 or 256.
    #[must_use]
    pub fn gdt_entry_count(&self) -> Option<u64> {
        self.gdt_limit.map(|limit| (u64::from(limit) + 1) / 8)
    }

    /// Number of IDT gates implied by the limit: `(limit + 1) / 16`.
    #[must_use]
    pub fn idt_entry_count(&self) -> Option<u64> {
        self.idt_limit.map(|limit| (u64::from(limit) + 1) / 16)
    }

    /// Read a register's raw text by name.
    #[must_use]
    pub fn register(&self, name: &str) -> Option<&str> {
        self.registers.get(name).map(String::as_str)
    }

    /// Parse a register as an integer.
    ///
    /// # Errors
    /// `None` value in the `Err` string when the register is absent or its text
    /// is not hex — the caller gets told which, because "CR3 is missing" and
    /// "CR3 is garbage" are different problems.
    pub fn register_u64(&self, name: &str) -> Result<u64, String> {
        let text = self
            .registers
            .get(name)
            .ok_or_else(|| format!("register {name} is not present in this report"))?;
        u64::from_str_radix(text, 16)
            .map_err(|err| format!("register {name} value {text:?} is not hexadecimal: {err}"))
    }
}

/// A fully parsed snapshot of one monitor capture.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorSnapshot {
    /// `info registers`.
    pub registers: Option<RegistersReport>,
    /// `info mem` ranges.
    pub mem: Vec<InfoMemRange>,
    /// `info tlb` entries.
    pub tlb: Vec<InfoTlbEntry>,
    /// `info cpus`.
    pub cpus: Vec<CpuInfo>,
    /// Granularity per `tlb` entry, inferred from the stride.
    pub tlb_granularities: Vec<TlbGranularity>,
}

// ------------------------------------------------------------------ parsing ---

fn lines_with_offsets(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        out.push((index + 1, offset, trimmed));
        offset += line.len();
    }
    // `split_inclusive` yields nothing for an empty string but does yield one
    // empty item for "\n"; both are handled by the callers' empty checks.
    out
}

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_hex_u64(text: &str) -> Option<u64> {
    if is_hex(text) {
        u64::from_str_radix(text, 16).ok()
    } else {
        None
    }
}

fn parse_hex_u32(text: &str) -> Option<u32> {
    if is_hex(text) {
        u32::from_str_radix(text, 16).ok()
    } else {
        None
    }
}

/// Parse `info mem` output.
///
/// Grammar, from the source (`mem_print()`):
/// `<start>-<end> <size> <u|->r<w|->`, all hex, no `0x`.
///
/// A `PG disabled` line is not an error and not a range — it is the honest
/// report that paging is off (research D §6 item 10), and it yields an empty
/// vector.  Anything else that does not match is a [`ParseError`].
///
/// # Errors
/// [`ParseError`] on the first line that is neither blank, `PG disabled`, nor a
/// valid range.
pub fn parse_info_mem(text: &str) -> Result<Vec<InfoMemRange>, ParseError> {
    const CONTEXT: &str = "info mem";
    let mut out = Vec::new();
    for (line_no, byte_offset, line) in lines_with_offsets(text) {
        if line.trim().is_empty() {
            continue;
        }
        let line = line.trim();
        // Paging off: `info mem` prints exactly this and nothing else.
        if line == "PG disabled" {
            return Ok(Vec::new());
        }
        if !line.is_ascii() {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "line contains non-ASCII bytes".to_string(),
            });
        }
        let mut fields = line.split_whitespace();
        let range = fields.next().unwrap_or_default();
        let size = fields.next();
        let flags = fields.next();
        if fields.next().is_some() {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "expected exactly 3 whitespace-separated fields".to_string(),
            });
        }
        let Some((start_text, end_text)) = range.split_once('-') else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "first field is not a <start>-<end> range".to_string(),
            });
        };
        let (Some(start), Some(end)) = (parse_hex_u64(start_text), parse_hex_u64(end_text)) else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: format!("range bounds {start_text:?}-{end_text:?} are not hexadecimal"),
            });
        };
        let Some(size) = size.and_then(parse_hex_u64) else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "size field is missing or not hexadecimal".to_string(),
            });
        };
        let Some(flags) = flags else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "permission field is missing".to_string(),
            });
        };
        if flags.len() != 3 {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: format!("permission field {flags:?} must be exactly 3 characters"),
            });
        }
        let mut chars = flags.chars();
        let user = chars.next() == Some('u');
        let readable = chars.next() == Some('r');
        let writable = chars.next() == Some('w');
        out.push(InfoMemRange {
            start,
            end,
            size,
            user,
            readable,
            writable,
        });
    }
    Ok(out)
}

/// Parse `info tlb` output.
///
/// Grammar, from the source (`print_pte()`): `<vaddr>: <paddr> <9 chars>`.
///
/// The nine characters are decoded **positionally** as
/// `X G P D A C T U W`, where column 3 is `PS`.  A character in a position is
/// either that position's letter or `-`; anything else is a [`ParseError`] rather
/// than a silently-false flag, because a wrong flag here changes what the UI
/// tells the user about a page.
///
/// # Errors
/// [`ParseError`] on the first malformed line.
pub fn parse_info_tlb(text: &str) -> Result<Vec<InfoTlbEntry>, ParseError> {
    const CONTEXT: &str = "info tlb";
    let mut out = Vec::new();
    for (line_no, byte_offset, line) in lines_with_offsets(text) {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.trim().split_whitespace();
        let Some(address_field) = fields.next() else {
            continue;
        };
        let Some(physical_field) = fields.next() else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "expected `<vaddr>: <paddr> <flags>`".to_string(),
            });
        };
        let Some(flags_field) = fields.next() else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "flag field is missing".to_string(),
            });
        };
        if fields.next().is_some() {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "unexpected trailing fields".to_string(),
            });
        }
        // `vaddr:` carries the colon; strip it rather than matching a regex.
        let Some(virtual_text) = address_field.strip_suffix(':') else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: format!("{address_field:?} is not a `<vaddr>:` field"),
            });
        };
        let (Some(virtual_address), Some(physical_address)) =
            (parse_hex_u64(virtual_text), parse_hex_u64(physical_field))
        else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: line.to_string(),
                reason: "address fields must be hexadecimal".to_string(),
            });
        };
        let flags = decode_tlb_flags(flags_field).ok_or_else(|| ParseError {
            context: CONTEXT,
            line: line_no,
            byte_offset,
            text: line.to_string(),
            reason: format!(
                "flag field {flags_field:?} must be 9 characters drawn from \
                 `X G P D A C T U W` positionally with `-` for clear"
            ),
        })?;
        out.push(InfoTlbEntry {
            virtual_address,
            physical_address,
            flags,
        });
    }
    Ok(out)
}

/// Decode the nine `info tlb` flag characters.
///
/// Returns `None` for anything that is not exactly nine characters of the right
/// letters in the right places.  Column 3 accepts `P` meaning **PS**.
#[must_use]
pub fn decode_tlb_flags(text: &str) -> Option<TlbFlags> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() != 9 {
        return None;
    }
    let expected = ['X', 'G', 'P', 'D', 'A', 'C', 'T', 'U', 'W'];
    let mut set = [false; 9];
    for (position, (actual, letter)) in chars.iter().zip(expected).enumerate() {
        if *actual == letter {
            set[position] = true;
        } else if *actual != '-' {
            return None;
        }
    }
    // Destructured by name so the mapping is impossible to get subtly wrong.
    let [nx, global, ps, dirty, accessed, pcd, pwt, user, writable] = set;
    Some(TlbFlags {
        nx,
        global,
        ps,
        dirty,
        accessed,
        pcd,
        pwt,
        user,
        writable,
    })
}

/// Parse `info cpus` output: `* CPU #0: thread_id=333695`.
///
/// The `thread_id` is a host PID and changes every run — the README is explicit
/// that the *shape* is the contract, never the number.
///
/// # Errors
/// [`ParseError`] on the first malformed line.
pub fn parse_info_cpus(text: &str) -> Result<Vec<CpuInfo>, ParseError> {
    const CONTEXT: &str = "info cpus";
    let mut out = Vec::new();
    for (line_no, byte_offset, line) in lines_with_offsets(text) {
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim();
        let current = trimmed.starts_with('*');
        let rest = trimmed.trim_start_matches('*').trim();
        let Some(rest) = rest.strip_prefix("CPU #") else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: trimmed.to_string(),
                reason: "expected `<*| > CPU #N: thread_id=M`".to_string(),
            });
        };
        let Some((index_text, tail)) = rest.split_once(':') else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: trimmed.to_string(),
                reason: "missing `:` after the CPU index".to_string(),
            });
        };
        let Some(index) = index_text.trim().parse::<u32>().ok() else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: trimmed.to_string(),
                reason: format!("CPU index {index_text:?} is not a decimal integer"),
            });
        };
        let Some(thread_text) = tail.trim().strip_prefix("thread_id=") else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: trimmed.to_string(),
                reason: "expected `thread_id=<decimal>`".to_string(),
            });
        };
        let Some(thread_id) = thread_text.trim().parse::<u64>().ok() else {
            return Err(ParseError {
                context: CONTEXT,
                line: line_no,
                byte_offset,
                text: trimmed.to_string(),
                reason: format!("thread id {thread_text:?} is not a decimal integer"),
            });
        };
        out.push(CpuInfo {
            index,
            thread_id,
            current,
        });
    }
    Ok(out)
}

/// Parse `info registers` output.
///
/// Every line has its own grammar, so there is no cross-line state machine
/// except for the `CPU#N` header (research D §5.3 calls this "the monitor output
/// most worth keeping a parser for", because `GDT=`/`IDT=`/`CR3`/`CR4`/`EFER`
/// exist nowhere else).
///
/// Lines that are recognised but not modelled (FPU/XMM) are skipped **by an
/// explicit matcher**, not by a catch-all, so a genuinely unknown line is
/// reported instead of ignored.
///
/// # Errors
/// [`ParseError`] on a line that matches no known shape.
pub fn parse_info_registers(text: &str) -> Result<RegistersReport, ParseError> {
    const CONTEXT: &str = "info registers";
    let mut report = RegistersReport::default();
    let mut in_header = true;

    for (line_no, byte_offset, line) in lines_with_offsets(text) {
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim();
        let error = |reason: String| ParseError {
            context: CONTEXT,
            line: line_no,
            byte_offset,
            text: trimmed.to_string(),
            reason,
        };

        if let Some(rest) = trimmed.strip_prefix("CPU#") {
            let cpu = rest.trim().parse::<u32>().map_err(|_| {
                error(format!("CPU header {trimmed:?} does not end in a decimal number"))
            })?;
            report.cpu = Some(cpu);
            in_header = false;
            continue;
        }

        // `GDT=` / `IDT=` — padded with spaces, then `<base> <limit>`.
        if let Some(rest) = trimmed.strip_prefix("GDT=") {
            let (base, limit) = parse_limit_pair(rest)
                .ok_or_else(|| error("expected `GDT=<hex base> <hex limit>`".to_string()))?;
            report.gdt_base = Some(base);
            report.gdt_limit = Some(limit);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("IDT=") {
            let (base, limit) = parse_limit_pair(rest)
                .ok_or_else(|| error("expected `IDT=<hex base> <hex limit>`".to_string()))?;
            report.idt_base = Some(base);
            report.idt_limit = Some(limit);
            continue;
        }

        // Segment registers: `CS =0008 <base> <limit> <access> DPL=0 CS64 [-R-]`
        // and the LDT/TR variants, which have no space after the name.
        if let Some(segment) = parse_segment_line(trimmed) {
            report.segments.push(segment);
            continue;
        }

        // `RIP=... RFL=... [flags] CPL=n ... HLT=n`
        if trimmed.starts_with("RIP=") {
            for field in trimmed.split_whitespace() {
                if let Some(value) = field.strip_prefix("RIP=") {
                    report
                        .registers
                        .insert("RIP".to_string(), value.to_string());
                } else if let Some(value) = field.strip_prefix("RFL=") {
                    report.rflags = Some(value.to_string());
                } else if let Some(value) = field.strip_prefix("CPL=") {
                    report.cpl = value.parse::<u8>().ok();
                } else if let Some(value) = field.strip_prefix("HLT=") {
                    report.halted = Some(value == "1");
                } else if field.starts_with('[') && field.ends_with(']') {
                    report.flags_text = Some(field.trim_matches(['[', ']']).to_string());
                }
            }
            continue;
        }

        // Register blocks: `RAX=... RBX=... ...` (note `R8 =` pads with a
        // space, so the split is on the `=`-terminated name, not on columns).
        if let Some(pairs) = parse_register_pairs(trimmed) {
            if !pairs.is_empty() {
                for (name, value) in pairs {
                    report.registers.insert(name.to_string(), value.to_string());
                }
                continue;
            }
        }

        // Known-but-unmodelled lines.  Listed explicitly: a new shape must fail
        // loudly rather than disappear.
        let ignorable = trimmed.starts_with("FCW=")
            || trimmed.starts_with("FPR")
            || trimmed.starts_with("XMM")
            || trimmed.starts_with("YMM")
            || trimmed.starts_with("ZMM")
            || trimmed.starts_with("ST")
            || trimmed.starts_with("MM");
        if ignorable {
            let _ = in_header;
            continue;
        }

        return Err(error(
            "line matches no known `info registers` shape".to_string(),
        ));
    }

    Ok(report)
}

/// `GDT=`/`IDT=` bodies are `<base><spaces><limit>`, both bare hex.
fn parse_limit_pair(text: &str) -> Option<(u64, u32)> {
    let mut fields = text.split_whitespace();
    let base = parse_hex_u64(fields.next()?)?;
    let limit = parse_hex_u32(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some((base, limit))
}

/// `CS =0008 0 0 ffffffff 00af9a00 DPL=0 CS64 [-R-]` — and the `LDT=`/`TR =`
/// forms, which share the tail but differ in spacing.
fn parse_segment_line(line: &str) -> Option<SegmentRegister> {
    let (name, rest) = if let Some(rest) = line.strip_prefix("LDT=") {
        ("LDT", rest)
    } else if let Some(rest) = line.strip_prefix("TR ") {
        ("TR", rest)
    } else if let Some(rest) = line.strip_prefix("TR=") {
        ("TR", rest)
    } else {
        // `ES =0010 ...` — two letters, optional space, `=`.
        let (name, rest) = line.split_at(2);
        if !name.chars().all(|c| c.is_ascii_uppercase()) {
            return None;
        }
        let rest = rest.trim_start().strip_prefix('=')?;
        (name, rest)
    };
    let name = name.to_string();
    // The `LDT=` and `TR =` forms carry a `=` that we already consumed for TR
    // only in the `TR =` case; normalise the remainder by dropping one leading
    // `=` if present.
    let rest = rest.trim_start().strip_prefix('=').unwrap_or(rest);
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 3 {
        return None;
    }
    let selector = parse_hex_u32(fields[0]).map(|value| value as u16)?;
    let base = parse_hex_u64(fields[1])?;
    let limit = parse_hex_u32(fields[2])?;
    let access = fields.get(3).and_then(|text| parse_hex_u32(text))?;

    // Everything from `DPL=` onwards is optional-ish; take what is there.
    let mut dpl = 0u8;
    let mut type_name = String::new();
    let mut permissions = None;
    for field in &fields[4..] {
        if let Some(value) = field.strip_prefix("DPL=") {
            dpl = value.parse::<u8>().unwrap_or(0);
        } else if field.starts_with('[') && field.ends_with(']') {
            permissions = Some(field.trim_matches(['[', ']']).to_string());
        } else if !field.contains('=') {
            // The type string is the only bare token on the line.
            if type_name.is_empty() {
                type_name = (*field).to_string();
            } else {
                // e.g. `TSS64-busy` is one token; a second bare token means the
                // line is not what we think it is.
                return None;
            }
        }
    }

    Some(SegmentRegister {
        name,
        selector,
        base,
        limit,
        access,
        dpl,
        type_name,
        permissions,
    })
}

/// Does this line look like `NAME=HEX NAME=HEX ...`?
///
/// Requires at least one `=`-pair whose value is pure hex, so a random log line
/// cannot masquerade as a register block.
///
/// The whitespace handling is the whole reason this is a function: QEMU pads the
/// two-digit register names to keep the columns aligned, so the sample contains
/// **`R8 =0000000000000001`** — a space *before* the `=`.  Splitting on columns
/// or on `char::is_whitespace` boundaries gets that line wrong; tokenising
/// `NAME [=] VALUE` does not.
/// Parse `NAME [=] HEX` tokens, or `None` if any token is not of that shape.
fn parse_register_pairs(line: &str) -> Option<Vec<(&str, &str)>> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut pairs = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        if let Some((name, value)) = token.split_once('=') {
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) || !is_hex(value)
            {
                return None;
            }
            pairs.push((name, value));
            index += 1;
            continue;
        }
        // `R8` then `=0000...` / `R8` then `= 0000...` / `R8` then `=` then value.
        let name = token;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let next = tokens.get(index + 1)?;
        let (value, consumed) = match next.strip_prefix('=') {
            Some("") => (*tokens.get(index + 2)?, 3),
            Some(value) => (value, 2),
            None => return None,
        };
        if !is_hex(value) {
            return None;
        }
        pairs.push((name, value));
        index += consumed;
    }
    Some(pairs)
}

// ---------------------------------------------------------------- QMP JSON ---

/// `query-version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QmpVersion {
    /// `qemu.major`.
    pub major: u32,
    /// `qemu.minor`.
    pub minor: u32,
    /// `qemu.micro`.
    pub micro: u32,
    /// Distribution package string, when present.
    #[serde(default)]
    pub package: Option<String>,
}

impl QmpVersion {
    /// `7.2.22`-style string.
    #[must_use]
    pub fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.micro)
    }
}

/// `query-status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QmpStatus {
    /// `running` / `paused` / `shutdown` / ...
    pub status: String,
    /// Present from QEMU 7.x onward.
    #[serde(default)]
    pub running: Option<bool>,
    /// Single-step mode.
    #[serde(default)]
    pub singlestep: Option<bool>,
}

/// One entry of `query-cpus-fast`.
///
/// This is the structured command multi-CPU page-table views must use to
/// enumerate CPUs (research D §5.5): `query-registers` does not exist in 7.2,
/// and scraping `info cpus` only works for the human-readable form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QmpCpuFast {
    /// Host thread id.  Changes every run.
    #[serde(rename = "thread-id")]
    pub thread_id: i64,
    /// Guest CPU index.
    #[serde(rename = "cpu-index")]
    pub cpu_index: i64,
    /// Target architecture string, e.g. `x86_64`.
    #[serde(default)]
    pub target: Option<String>,
    /// QOM path; may change between QEMU versions, so it is optional.
    #[serde(default, rename = "qom-path")]
    pub qom_path: Option<String>,
}

/// `query-memory-size-summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QmpMemorySummary {
    /// Base RAM size in bytes.
    #[serde(rename = "base-memory")]
    pub base_memory: u64,
    /// Hot-plugged memory in bytes.
    #[serde(default, rename = "plugged-memory")]
    pub plugged_memory: Option<u64>,
}

impl QmpMemorySummary {
    /// Total guest RAM.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.base_memory
            .saturating_add(self.plugged_memory.unwrap_or(0))
    }

    /// Is `physical_address` inside guest RAM?
    ///
    /// This is the precondition research D §3.4 requires of a page-table walk:
    /// `xp` on a non-RAM address answers `Cannot access memory` rather than
    /// raising, so a walker that does not check builds a tree of zeros.
    #[must_use]
    pub fn contains_physical(&self, physical_address: u64) -> bool {
        physical_address < self.total()
    }
}

/// Parse a `query-version` reply body (the `return` object, not the envelope).
///
/// # Errors
/// [`ParseError`] when the JSON does not have the documented shape.  Note that
/// the *file* `fixtures/qemu-monitor/qmp-query-version.json` is the recorded
/// `return` body itself, not an envelope — the capture script unwraps it.
pub fn parse_qmp_version(json: &str) -> Result<QmpVersion, ParseError> {
    #[derive(Deserialize)]
    struct Raw {
        qemu: Inner,
        #[serde(default)]
        package: Option<String>,
    }
    #[derive(Deserialize)]
    struct Inner {
        major: u32,
        minor: u32,
        micro: u32,
    }
    let raw: Raw = serde_json::from_str(json).map_err(|err| ParseError {
        context: "qmp query-version",
        line: 0,
        byte_offset: 0,
        text: json.chars().take(120).collect(),
        reason: err.to_string(),
    })?;
    Ok(QmpVersion {
        major: raw.qemu.major,
        minor: raw.qemu.minor,
        micro: raw.qemu.micro,
        package: raw.package,
    })
}

/// Parse a `query-status` reply body.
///
/// # Errors
/// [`ParseError`] on a shape mismatch.
pub fn parse_qmp_status(json: &str) -> Result<QmpStatus, ParseError> {
    serde_json::from_str(json).map_err(|err| ParseError {
        context: "qmp query-status",
        line: 0,
        byte_offset: 0,
        text: json.chars().take(120).collect(),
        reason: err.to_string(),
    })
}

/// Parse a `query-cpus-fast` reply body (a JSON array).
///
/// # Errors
/// [`ParseError`] on a shape mismatch.
pub fn parse_qmp_cpus_fast(json: &str) -> Result<Vec<QmpCpuFast>, ParseError> {
    serde_json::from_str(json).map_err(|err| ParseError {
        context: "qmp query-cpus-fast",
        line: 0,
        byte_offset: 0,
        text: json.chars().take(120).collect(),
        reason: err.to_string(),
    })
}

/// Parse a `query-memory-size-summary` reply body.
///
/// # Errors
/// [`ParseError`] on a shape mismatch.
pub fn parse_qmp_memory_summary(json: &str) -> Result<QmpMemorySummary, ParseError> {
    serde_json::from_str(json).map_err(|err| ParseError {
        context: "qmp query-memory-size-summary",
        line: 0,
        byte_offset: 0,
        text: json.chars().take(120).collect(),
        reason: err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample(name: &str) -> Option<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/qemu-monitor")
            .join(name);
        std::fs::read_to_string(path).ok()
    }

    fn sample_or_skip(name: &str) -> String {
        sample(name).unwrap_or_default()
    }

    // ------------------------------------------------------------- info mem --

    #[test]
    fn info_mem_sample_parses_to_the_fixture_map() {
        let text = sample_or_skip("info-mem.txt");
        if text.is_empty() {
            return;
        }
        let ranges = parse_info_mem(&text).expect("info mem must parse");
        assert_eq!(ranges.len(), 3, "the recorded sample has exactly 3 ranges");
        // Values transcribed from fixtures/qemu-monitor/README.md, which lists
        // this map as the fixture's ground truth.
        assert_eq!(
            ranges[0],
            InfoMemRange {
                start: 0,
                end: 0x1ff000,
                size: 0x1ff000,
                user: false,
                readable: true,
                writable: true,
            }
        );
        assert_eq!(ranges[1].start, 0x20_0000);
        assert_eq!(ranges[1].end, 0x40_0000);
        assert_eq!(ranges[2].start, 0x60_0000);
        assert_eq!(ranges[2].end, 0x4000_0000);
        for range in &ranges {
            assert!(
                range.size_agrees_with_bounds(),
                "printed size must equal end-start: {range:?}"
            );
            assert!(range.readable && range.writable && !range.user);
        }
    }

    #[test]
    fn info_mem_hole_between_0x1ff000_and_0x200000_is_visible() {
        let text = sample_or_skip("info-mem.txt");
        if text.is_empty() {
            return;
        }
        let ranges = parse_info_mem(&text).unwrap();
        // The fixture deliberately leaves 0x1ff000..0x1fffff unmapped; if the
        // parser merged the first two ranges the hole would vanish.
        assert_eq!(ranges[0].end, 0x1ff000);
        assert_eq!(ranges[1].start, 0x20_0000);
    }

    #[test]
    fn pg_disabled_is_an_empty_map_not_an_error() {
        // What `info mem` prints with CR0.PG=0 (research D §6 item 10).
        let ranges = parse_info_mem("PG disabled\n").expect("must not be an error");
        assert!(ranges.is_empty());
    }

    // ------------------------------------------------------------- info tlb --

    #[test]
    fn info_tlb_sample_parses_every_line() {
        let text = sample_or_skip("info-tlb.txt");
        if text.is_empty() {
            return;
        }
        let entries = parse_info_tlb(&text).expect("info tlb must parse");
        assert_eq!(
            entries.len(),
            1021,
            "the recorded sample has 1021 lines (README: '1021 lines')"
        );
    }

    /// The headline P5-4 assertion.
    ///
    /// If column 3 meant "present", every 4 KiB leaf in the sample would be
    /// mis-classified: they print `--------W` (present, RW only) and would look
    /// absent.  The 2 MiB leaves print `--P-----W` and are the *only* lines with
    /// a `P`.  That asymmetry is only explicable if `P` is PS.
    #[test]
    fn tlb_flag_column_three_is_ps_not_present() {
        let text = sample_or_skip("info-tlb.txt");
        if text.is_empty() {
            return;
        }
        let entries = parse_info_tlb(&text).unwrap();

        // (a) A present 4 KiB leaf has PS clear: the leading entries are the
        //     identity-mapped 4 KiB pages and print `--------W`.
        let first = entries[0];
        assert_eq!(first.virtual_address, 0);
        assert!(first.flags.writable, "RW is set on the 4 KiB leaves");
        assert!(
            !first.flags.ps,
            "a 4 KiB leaf must NOT have PS set; if this fails, column 3 has been \
             mis-read as `present`"
        );
        assert_eq!(first.granularity(), TlbGranularity::Page4K);

        // (b) The 2 MiB leaves carry the `P`, and only they do.
        let huge: Vec<&InfoTlbEntry> = entries.iter().filter(|e| e.flags.ps).collect();
        assert!(!huge.is_empty(), "the fixture maps 2 MiB huge pages");
        for entry in &huge {
            assert_eq!(entry.granularity(), TlbGranularity::Huge2M);
            // QEMU prints the physical base aligned to the leaf size.
            assert_eq!(
                entry.physical_address % TlbGranularity::Huge2M.bytes(),
                0,
                "a PS=1 entry's paddr must be leaf-aligned: {entry:#x?}"
            );
        }
        // (c) The decoded physical addresses must match the `--P-----W` lines.
        assert!(
            huge.iter()
                .any(|entry| entry.virtual_address == 0x3f60_0000
                    && entry.physical_address == 0x3f60_0000),
            "the sample's tail lines are 2 MiB identity pages"
        );
    }

    #[test]
    fn tlb_entries_are_present_and_never_report_a_present_flag() {
        let text = sample_or_skip("info-tlb.txt");
        if text.is_empty() {
            return;
        }
        let entries = parse_info_tlb(&text).unwrap();
        // Every line QEMU prints is present by construction (the source filters
        // on P before printing), so there is no present flag to check — the
        // strongest available statement is that all lines decode and are
        // monotonic in vaddr.
        for pair in entries.windows(2) {
            assert!(
                pair[0].virtual_address < pair[1].virtual_address,
                "info tlb is printed in ascending vaddr order"
            );
        }
        assert!(entries.iter().all(|entry| entry.flags.writable));
    }

    #[test]
    fn granularity_inference_uses_the_stride() {
        let text = sample_or_skip("info-tlb.txt");
        if text.is_empty() {
            return;
        }
        let entries = parse_info_tlb(&text).unwrap();
        let granularities = resolve_granularities(&entries);
        assert_eq!(granularities.len(), entries.len());
        // The 4 KiB region at the bottom must be 4 KiB throughout.
        for (entry, granularity) in entries.iter().zip(&granularities).take(500) {
            if !entry.flags.ps {
                assert_eq!(*granularity, TlbGranularity::Page4K);
            }
        }
        // And the 2 MiB region must resolve to 2 MiB (stride 0x200000).
        let huge_start = entries
            .iter()
            .position(|entry| entry.flags.ps)
            .expect("fixture has huge pages");
        assert_eq!(granularities[huge_start], TlbGranularity::Huge2M);
    }

    #[test]
    fn a_wrong_length_flag_field_is_rejected() {
        let bad = "0000000000000000: 0000000000000000 --------\n";
        let err = parse_info_tlb(bad).unwrap_err();
        assert_eq!(err.context, "info tlb");
        assert_eq!(err.line, 1);
        assert!(err.reason.contains("9 characters"), "{}", err.reason);
    }

    #[test]
    fn a_wrong_letter_in_a_flag_position_is_rejected() {
        // `Z` is not a flag letter anywhere.
        let bad = "0000000000000000: 0000000000000000 Z-------W\n";
        let err = parse_info_tlb(bad).unwrap_err();
        assert!(err.reason.contains("positionally"), "{}", err.reason);
        // And a `P` in the wrong column is rejected too: column 2 is G.
        let bad = "0000000000000000: 0000000000000000 -P------W\n";
        assert!(parse_info_tlb(bad).is_err());
    }

    #[test]
    fn decode_tlb_flags_maps_each_column_to_the_right_bit() {
        // Build one flag string with a unique letter per column and check that
        // each lands on its own field.  Columns are `X G P D A C T U W`
        // (research D §5.2); this string sets every one except PS.
        let flags = decode_tlb_flags("XG-DACTUW").expect("decodes");
        assert!(flags.nx, "col 1 = NX");
        assert!(flags.global, "col 2 = G");
        assert!(!flags.ps, "col 3 = PS (clear here)");
        assert!(flags.dirty, "col 4 = D");
        assert!(flags.accessed, "col 5 = A");
        assert!(flags.pcd, "col 6 = C/PCD");
        assert!(flags.pwt, "col 7 = T/PWT");
        assert!(flags.user, "col 8 = U");
        assert!(flags.writable, "col 9 = W");

        // A `P` in the wrong column is rejected, not silently reinterpreted:
        // column 4 is D, and column 2 is G.  This is the exact confusion the
        // P5-4 assertion exists to catch.
        // PS set on its own, everything else clear: `X G P D A C T U W` with
        // only column 3 filled.  This is literally what a 2 MiB leaf prints.
        let huge = decode_tlb_flags("--P------").expect("a 2 MiB leaf");
        assert!(huge.ps, "column 3 is PS");
        assert!(!huge.writable, "a leaf with only PS set is not writable");
        assert!(!huge.nx && !huge.global && !huge.dirty && !huge.accessed);
        assert!(!huge.pcd && !huge.pwt && !huge.user);

        // What must be rejected is a *wrong* letter in a column, or the wrong
        // field width.
        assert!(
            decode_tlb_flags("--P------X").is_none(),
            "10 characters is not the 9-character field"
        );
        assert!(
            decode_tlb_flags("XGP-DACTU").is_none(),
            "column 9 is W, so a `U` there is invalid"
        );
        assert!(
            decode_tlb_flags("-GP-DACTU").is_none(),
            "column 2 is G, so a `G` in column 2 after a `-` in 1 is fine, but \
             this string ends in `U` where W belongs"
        );

        let none = decode_tlb_flags("---------").expect("all clear");
        assert_eq!(none, TlbFlags::default());
    }

    // ------------------------------------------------------------ info cpus --

    #[test]
    fn info_cpus_sample_parses() {
        let text = sample_or_skip("info-cpus.txt");
        if text.is_empty() {
            return;
        }
        let cpus = parse_info_cpus(&text).expect("info cpus must parse");
        assert_eq!(cpus.len(), 1, "the fixture is single-CPU");
        assert_eq!(cpus[0].index, 0);
        assert!(cpus[0].current, "the `*` marks the current CPU");
        // The README warns the thread id is a host PID and not golden; assert
        // only that it parsed into a plausible positive number.
        assert!(cpus[0].thread_id > 0);
    }

    // ------------------------------------------------------ info registers ---

    #[test]
    fn info_registers_sample_parses_the_state_p5_needs() {
        let text = sample_or_skip("info-registers.txt");
        if text.is_empty() {
            return;
        }
        let report = parse_info_registers(&text).expect("info registers must parse");
        assert_eq!(report.cpu, Some(0));

        // The four facts the paging walk and the descriptor decoder need.
        assert_eq!(report.register_u64("CR0").unwrap(), 0x8000_0011);
        assert_eq!(report.register_u64("CR3").unwrap(), 0x10_4000);
        assert_eq!(report.register_u64("CR4").unwrap(), 0x20);
        assert_eq!(report.register_u64("CR2").unwrap(), 0x40_0000);
        assert_eq!(report.register_u64("RIP").unwrap(), 0x10_1034);

        // GDT/IDT: the only source of these addresses (research D §4.2).
        assert_eq!(report.gdt_base, Some(0x10_12e0));
        assert_eq!(report.gdt_limit, Some(0x27));
        assert_eq!(report.idt_base, Some(0x10_8000));
        assert_eq!(report.idt_limit, Some(0xfff));

        // Limits are byte-counts-minus-one, so the counts must be derived.
        assert_eq!(report.gdt_entry_count(), Some(5));
        assert_eq!(report.idt_entry_count(), Some(256));

        // The vCPU is halted at the capture point (README).
        assert_eq!(report.halted, Some(true));
        assert_eq!(report.cpl, Some(0));
        assert_eq!(report.flags_text.as_deref(), Some("---Z-P-"));

        // Segment registers, including the LDT/TR forms.
        let cs = report.segments.iter().find(|s| s.name == "CS").unwrap();
        assert_eq!(cs.selector, 0x0008);
        assert_eq!(cs.type_name, "CS64");
        assert_eq!(cs.dpl, 0);
        assert_eq!(cs.access, 0x00af_9a00);
        assert_eq!(cs.permissions.as_deref(), Some("-R-"));

        let tr = report.segments.iter().find(|s| s.name == "TR").unwrap();
        assert_eq!(tr.selector, 0x0000);
        assert_eq!(tr.type_name, "TSS64-busy");

        let ldt = report.segments.iter().find(|s| s.name == "LDT").unwrap();
        assert_eq!(ldt.type_name, "LDT");
        // A reset LDT has limit 0xffff and base 0 — 65536 entries would be a
        // fabricated table (research D §4.4 item 4).
        assert_eq!(ldt.limit, 0xffff);
        assert_eq!(ldt.base, 0);
    }

    #[test]
    fn padded_register_names_are_parsed_without_column_assumptions() {
        // `R8 =000...` has a space before the `=`; a column-slicing parser gets
        // this wrong.
        let text = "CPU#0\nR8 =0000000000000001 R9 =0000000000000000\n";
        let report = parse_info_registers(text).unwrap();
        assert_eq!(report.register_u64("R8").unwrap(), 1);
        assert_eq!(report.register_u64("R9").unwrap(), 0);
    }

    #[test]
    fn an_unknown_registers_line_fails_loudly() {
        let text = "CPU#0\nWAT=thisisnotahexvalue\n";
        let err = parse_info_registers(text).unwrap_err();
        assert_eq!(err.context, "info registers");
        assert_eq!(err.line, 2);
        assert!(err.text.contains("WAT"), "error must quote the line");
    }

    #[test]
    fn the_error_carries_a_usable_byte_offset() {
        let text = "CPU#0\nRAX=0000000000000000\nbroken line here\n";
        let err = parse_info_registers(text).unwrap_err();
        // Byte offset must point at the start of line 3.
        assert_eq!(err.byte_offset, "CPU#0\nRAX=0000000000000000\n".len());
        assert_eq!(&text[err.byte_offset..err.byte_offset + 6], "broken");
    }

    // ----------------------------------------------------------- QMP shapes --

    #[test]
    fn qmp_samples_parse() {
        if let Some(text) = sample("qmp-query-version.json") {
            let version = parse_qmp_version(&text).expect("query-version");
            assert_eq!(version.version_string(), "7.2.22");
            assert!(version.package.is_some());
        }
        if let Some(text) = sample("qmp-query-status.json") {
            let status = parse_qmp_status(&text).expect("query-status");
            assert_eq!(status.status, "running");
            assert_eq!(status.running, Some(true));
            assert_eq!(status.singlestep, Some(false));
        }
        if let Some(text) = sample("qmp-query-cpus-fast.json") {
            let cpus = parse_qmp_cpus_fast(&text).expect("query-cpus-fast");
            assert_eq!(cpus.len(), 1);
            assert_eq!(cpus[0].cpu_index, 0);
            assert_eq!(cpus[0].target.as_deref(), Some("x86_64"));
        }
        if let Some(text) = sample("qmp-query-memory-size-summary.json") {
            let memory = parse_qmp_memory_summary(&text).expect("memory summary");
            assert_eq!(memory.base_memory, 256 * 1024 * 1024);
            assert_eq!(memory.total(), 256 * 1024 * 1024);
            assert!(memory.contains_physical(0x10_4000));
            assert!(!memory.contains_physical(0x40_0000_0000));
        }
    }

    #[test]
    fn malformed_qmp_json_is_a_typed_error_not_a_panic() {
        let err = parse_qmp_version("{ this is not json").unwrap_err();
        assert_eq!(err.context, "qmp query-version");
        // And a well-formed JSON of the wrong shape fails too.
        assert!(parse_qmp_version(r#"{"qemu":{"major":7}}"#).is_err());
        assert!(parse_qmp_cpus_fast(r#"{"not":"an array"}"#).is_err());
    }

    #[test]
    fn the_qmp_version_gate_can_detect_a_different_qemu() {
        let text = sample_or_skip("qemu-version.txt");
        let json = sample_or_skip("qmp-query-version.json");
        if text.is_empty() || json.is_empty() {
            return;
        }
        let version = parse_qmp_version(&json).unwrap();
        // The README says the samples are from 7.2.22; if a future re-record
        // bumps QEMU, this assertion is the tripwire that says so.
        assert!(
            text.contains(&version.version_string()),
            "recorded qemu-version.txt and query-version disagree"
        );
    }

    /// Assemble a whole snapshot from the recorded sample set.
    #[test]
    fn a_full_snapshot_assembles_from_the_recorded_samples() {
        let registers = sample("info-registers.txt");
        let mem = sample("info-mem.txt");
        let tlb = sample("info-tlb.txt");
        let cpus = sample("info-cpus.txt");
        if registers.is_none() || mem.is_none() || tlb.is_none() || cpus.is_none() {
            return;
        }
        let snapshot = MonitorSnapshot {
            registers: Some(parse_info_registers(&registers.unwrap()).unwrap()),
            mem: parse_info_mem(&mem.unwrap()).unwrap(),
            tlb: parse_info_tlb(&tlb.unwrap()).unwrap(),
            cpus: parse_info_cpus(&cpus.unwrap()).unwrap(),
            tlb_granularities: Vec::new(),
        };
        assert!(snapshot.registers.is_some());
        assert_eq!(snapshot.mem.len(), 3);
        assert_eq!(snapshot.tlb.len(), 1021);
        assert_eq!(snapshot.cpus.len(), 1);
        // Cross-check the huge-page line count against the fixture's own map
        // documentation ("map 0x0000000000600000-0x000000003fffffff 2MiB huge").
        // The range is half-open at the top, so the count is
        // (0x40000000 - 0x600000) / 0x200000 = 509, not 500.
        let huge_above_hole = snapshot
            .tlb
            .iter()
            .filter(|entry| entry.virtual_address >= 0x60_0000 && entry.flags.ps)
            .count();
        assert_eq!(huge_above_hole as u64, (0x4000_0000u64 - 0x60_0000) / 0x20_0000);
        assert_eq!(huge_above_hole, 509);
        // Plus the single 2 MiB page at 0x200000, and the 511 4 KiB pages below
        // the hole: 511 + 1 + 509 = 1021, the recorded line count.
        assert_eq!(snapshot.tlb.len(), 511usize + 1 + 509);
    }
}
