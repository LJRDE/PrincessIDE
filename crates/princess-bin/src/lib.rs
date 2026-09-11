//! # princess-bin — the P5 binary / low-level visualization backend
//!
//! Six capabilities, one crate, one frozen technology stack (D20):
//!
//! | module | capability | acceptance |
//! |---|---|---|
//! | [`elf`] | ELF header / sections / segments / symbols / entry point | P5-1 |
//! | [`source`] | address → `file:line` and, crucially, **address range → line range** | P5-2 |
//! | [`disasm`] | `iced-x86` disassembly, emitted **side by side with source lines** | P5-2 |
//! | [`hex`] | `pread` + bounded LRU window cache over arbitrarily large images | P5-3 |
//! | [`monitor`] | HMP text and QMP JSON parsing of the recorded samples | P5-4 |
//! | [`pagetable`] | walking the live 4-level page table from `CR3` | P5-5 |
//! | [`descriptor`] | GDT / LDT / IDT / TSS descriptor decoding | P5-5 |
//! | [`guestmem`] | the physical-memory source the two above read through | P5-5 |
//!
//! ## The one design decision that shapes everything: ranges, not points
//!
//! [`BinProvider::source_line_for_address`](princess_core::BinProvider::source_line_for_address)
//! answers a *point* query: one address, one line.  Research D §1.2 is explicit
//! that a point answer is **actively misleading** on optimised code — `-O2`
//! attributes an inlined body to the *callee's* line, not the call site — and
//! D20 turns that into a hard UI requirement: the disassembly view shows
//! "instruction + source line" **side by side**, never a lone line number.
//!
//! A UI cannot build that from point queries alone; it would have to probe every
//! instruction and would still not know where a line *starts*.  So [`source`]
//! exposes the underlying **address ↔ line interval** relation directly
//! ([`source::LineTable::line_range_for_address`] returns `{start, end}` of
//! addresses belonging to the same line, and [`source::LineTable::segments`]
//! enumerates the whole mapping), and [`disasm::disassemble_with_source`] uses it
//! to emit one [`disasm::SourceRow`] per source line covering the requested
//! range.  The `BinProvider` point method stays as the contract's thin wrapper.
//!
//! ## Memory discipline (D22)
//!
//! Nothing here ever reads a whole image into memory on the caller's behalf:
//!
//! * the ELF layer `mmap`s the *meta* file only when it is small, and otherwise
//!   reads it whole but bounded (see [`elf::MAX_ARTIFACT_BYTES`]);
//! * [`hex::HexReader`] never holds more than `window_size × capacity` bytes
//!   (1 MiB at the default 256 × 4 KiB) regardless of file size;
//! * [`source`] keeps **exactly one** `addr2line::Context` per open artifact and
//!   builds it lazily, because `Context::new` is eager and can cost hundreds of
//!   MB on a large `vmlinux` (research D §1.2, item 14 of §6).
//!
//! ## Fail loud
//!
//! Every "I could not answer" path is a typed error, never a plausible-looking
//! default: an unparseable file is `E_INTERNAL`, a bad address is
//! `E_NOT_FOUND`, a page-table walk that leaves RAM is an explicit
//! [`pagetable::WalkError`], and a monitor line that does not match its grammar
//! is a [`monitor::ParseError`] carrying the offending offset in the *byte view*
//! of the sample.  There is no "return 0 and hope".

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod demangle;
pub mod descriptor;
pub mod disasm;
pub mod elf;
pub mod guestmem;
pub mod hex;
pub mod monitor;
pub mod pagetable;
pub mod provider;
pub mod source;

pub use demangle::{demangle_name, group_key};
pub use disasm::{DisassemblerOptions, DisassemblyView, SourceRow, Syntax};
pub use elf::{read_entry_point, read_sections, read_segments, read_symbols, ElfHeader, SegmentInfo};
pub use guestmem::{GuestMemory, GuestRam, MemoryError};
pub use hex::{HexLine, HexReader, HexWindowOptions, ImageStats};
pub use monitor::{
    CpuInfo, InfoMemRange, InfoTlbEntry, MonitorSnapshot, ParseError, QmpCpuFast, QmpMemorySummary,
    QmpStatus, QmpVersion, RegistersReport, SegmentRegister, TlbFlags,
};
pub use pagetable::{
    Canonicality, Cr0, Cr4, Efer, LeafSize, PageTableEntry, PageTableWalk, WalkError, WalkLevel,
};
pub use provider::ElfBinProvider;
pub use source::{LineTable, LineRange};
