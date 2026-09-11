//! Walking the x86_64 page table from `CR3`, one level at a time — P5-5.
//!
//! # Why we walk it ourselves
//!
//! Research D §6 items 5, 6 and 10 rule out every shortcut:
//!
//! | shortcut | why it fails |
//! |---|---|
//! | `info mem` as the data source | 3 permission bits only; **returns empty under LA57** (QEMU upstream bug, §3.6); does not distinguish granularity |
//! | `info tlb` as the data source | granularity only inferable from the stride; output grows with mapped bytes (1021 lines here); leaf permissions only, not effective ones (§3.8) |
//! | any cached "page-table snapshot" | the **A/D bits are written back by the CPU at run time** (§3.7) — a snapshot is stale the moment you take it |
//!
//! So the walk reads the raw entries through [`GuestMemory`] and decodes every
//! bit field itself.
//!
//! # The four guards research D insists on
//!
//! 1. **`CR0.PG` must be 1** before the walk means anything.  With paging off,
//!    `CR3` is not a page-table pointer and a walk produces confident nonsense
//!    (research D §3.2, §6 item 10).  [`PageTableWalk::new`] takes a [`Cr0`] and
//!    refuses.
//! 2. **`CR3`'s low 12 bits are not part of the address.**  With `CR4.PCIDE=1`
//!    they are the PCID.  Both fixtures happen to have `PCID=0`, and research D
//!    §6 item 9 is explicit that this must not be assumed — [`Cr3::table_address`]
//!    masks them off unconditionally.
//! 3. **Every table address must be inside guest RAM.**  `xp` answers
//!    `Cannot access memory` for anything else, and a walker that ignores that
//!    builds a tree of zeros (research D §3.4, §6 item 22).
//! 4. **Cycles must be detected.**  Self-mapping a PML4 slot at itself is a
//!    standard kernel trick, and a naive walk recurses forever.
//!
//! # Large pages
//!
//! `PS` (`bit 7`) terminates the walk early: 1 GiB at the PDPTE (depth 2) and
//! 2 MiB at the PDE (depth 3).  The physical address then comes from
//! **bits 51:30 or 51:21**, *not* from 51:12 — a 2 MiB leaf's `0x2000e7 & 0x…F000`
//! is `0x200000` (research D §3.5), and masking it with the 4 KiB mask would give
//! the same answer here only by luck of alignment.  [`PageTableEntry::leaf_physical_address`]
//! uses the size-correct mask and [`WalkLevel::bits_consumed`] reports how much
//! of the VA became offset.

use princess_core::{PrincessError, Result};

use crate::guestmem::{GuestMemory, MemoryError};

/// Bits 51:12 — the physical address field of a page-table entry.
///
/// Masking with this also strips `CR3`'s PCID, which is why it is applied to
/// `CR3` itself and not only to entries.
pub const ENTRY_ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// `PS` — page size.  At the PDE this means 2 MiB; at the PDPTE, 1 GiB.
pub const BIT_PS: u64 = 1 << 7;

/// Present.
pub const BIT_PRESENT: u64 = 1 << 0;

/// No-execute / XD.
pub const BIT_NX: u64 = 1 << 63;

/// `CR0`, reduced to the bit that decides whether a walk is meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cr0(pub u64);

impl Cr0 {
    /// `PG` — paging enable, bit 31.
    #[must_use]
    pub fn paging_enabled(self) -> bool {
        self.0 & (1 << 31) != 0
    }

    /// `PE` — protection enable, bit 0.
    #[must_use]
    pub fn protection_enabled(self) -> bool {
        self.0 & 1 != 0
    }

    /// `WP` — write protect, bit 16.
    #[must_use]
    pub fn write_protect(self) -> bool {
        self.0 & (1 << 16) != 0
    }
}

/// `CR4`, reduced to the bits that change the walk's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cr4(pub u64);

impl Cr4 {
    /// `PAE`, bit 5.  Required for long mode.
    #[must_use]
    pub fn pae(self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// `PCIDE`, bit 17.  When set, `CR3[11:0]` carries a process-context id.
    #[must_use]
    pub fn pcide(self) -> bool {
        self.0 & (1 << 17) != 0
    }

    /// `LA57`, bit 12.  When set the walk gains a PML5 level.
    #[must_use]
    pub fn la57(self) -> bool {
        self.0 & (1 << 12) != 0
    }

    /// `SMEP`, bit 20.
    #[must_use]
    pub fn smep(self) -> bool {
        self.0 & (1 << 20) != 0
    }

    /// `SMAP`, bit 21.
    #[must_use]
    pub fn smap(self) -> bool {
        self.0 & (1 << 21) != 0
    }

    /// `PKE`, bit 22.  Enables the protection-key field in entries.
    #[must_use]
    pub fn pke(self) -> bool {
        self.0 & (1 << 22) != 0
    }
}

/// `EFER` (MSR `0xC0000080`), reduced to the long-mode bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Efer(pub u64);

impl Efer {
    /// `SCE`, bit 0 — `syscall`/`sysret` enable.
    #[must_use]
    pub fn sce(self) -> bool {
        self.0 & 1 != 0
    }

    /// `LME`, bit 8 — long mode enable.
    #[must_use]
    pub fn lme(self) -> bool {
        self.0 & (1 << 8) != 0
    }

    /// `LMA`, bit 10 — long mode active.
    ///
    /// Research D §3.2 item 3: when this is clear the machine is in 32-bit
    /// PAE or non-PAE mode, whose tables are a **different structure** (4-byte
    /// entries, 1024 per table).  The walker says so rather than pretending the
    /// 4-level layout applies.
    #[must_use]
    pub fn lma(self) -> bool {
        self.0 & (1 << 10) != 0
    }

    /// `NXE`, bit 11 — no-execute enable.  Without it, bit 63 is reserved.
    #[must_use]
    pub fn nxe(self) -> bool {
        self.0 & (1 << 11) != 0
    }
}

/// `CR3` split into its address and its PCID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cr3(pub u64);

impl Cr3 {
    /// The physical address of the top-level table, with the PCID masked off.
    ///
    /// Unconditional: research D §6 item 9 is explicit that "both fixtures had
    /// PCID=0" is not a reason to skip the mask.
    #[must_use]
    pub fn table_address(self) -> u64 {
        self.0 & ENTRY_ADDRESS_MASK
    }

    /// The process-context id in bits 11:0.  Meaningless unless `CR4.PCIDE=1`,
    /// which the caller must check via [`Cr4::pcide`].
    #[must_use]
    pub fn pcid(self) -> u16 {
        (self.0 & 0xFFF) as u16
    }

    /// Did this value actually have a non-zero low 12 bits?
    ///
    /// Surfaced so the UI can say "this CR3 carries a PCID" instead of quietly
    /// masking something the user might be trying to read.
    #[must_use]
    pub fn has_low_bits_set(self) -> bool {
        self.pcid() != 0
    }
}

/// Which level of the walk an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `PML5` — present only under LA57.
    Pml5,
    /// `PML4` — the top level under LA48.
    Pml4,
    /// `PDPT` — 1 GiB leaves live here.
    Pdpt,
    /// `PD` — 2 MiB leaves live here.
    Pd,
    /// `PT` — 4 KiB leaves; never has `PS` set.
    Pt,
}

impl Level {
    /// Depth from the top of *this* walk, counting the first level as 0.
    #[must_use]
    pub fn depth(self) -> usize {
        match self {
            Level::Pml5 => 0,
            Level::Pml4 => 1,
            Level::Pdpt => 2,
            Level::Pd => 3,
            Level::Pt => 4,
        }
    }

    /// The canonical name as an OS developer writes it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Level::Pml5 => "PML5",
            Level::Pml4 => "PML4",
            Level::Pdpt => "PDPT",
            Level::Pd => "PD",
            Level::Pt => "PT",
        }
    }

    /// The VA bits this level's index is taken from, as `(high, low)` inclusive.
    #[must_use]
    pub fn index_bits(self) -> (u32, u32) {
        match self {
            Level::Pml5 => (56, 48),
            Level::Pml4 => (47, 39),
            Level::Pdpt => (38, 30),
            Level::Pd => (29, 21),
            Level::Pt => (20, 12),
        }
    }

    /// Which levels may legally carry `PS=1`.
    ///
    /// `PML5E` and `PML4E` must have `PS=0`; the `PTE` must too.  Only `PDPTE`
    /// (1 GiB) and `PDE` (2 MiB) may set it (Intel SDM Vol.3 §4.5, quoted in
    /// research D §3.3).  A walk that sees `PS` anywhere else reports it rather
    /// than silently folding it into a page size.
    #[must_use]
    pub fn huge_page_allowed(self) -> bool {
        matches!(self, Level::Pdpt | Level::Pd)
    }

    /// The leaf size this level produces when `PS` is set.
    #[must_use]
    pub fn huge_page_size(self) -> Option<u64> {
        match self {
            Level::Pdpt => Some(1024 * 1024 * 1024),
            Level::Pd => Some(2 * 1024 * 1024),
            _ => None,
        }
    }
}

/// A decoded page-table entry.
///
/// Every architectural bit research D §3.3 lists is decoded, including the ones
/// the IDE is *not* supposed to present as configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTableEntry {
    /// The raw 64-bit value.
    pub raw: u64,
    /// `P` — present.
    pub present: bool,
    /// `R/W` — writable.
    pub writable: bool,
    /// `U/S` — user accessible.
    pub user: bool,
    /// `PWT` — write-through.
    pub pwt: bool,
    /// `PCD` — cache disable.
    pub pcd: bool,
    /// `A` — accessed.  **Written by the CPU at run time**; never cache this.
    pub accessed: bool,
    /// `D` — dirty.  Meaningful only on a leaf; also CPU-written.
    pub dirty: bool,
    /// `PS` — page size, i.e. "this is a huge-page leaf".
    pub page_size: bool,
    /// `G` — global.
    pub global: bool,
    /// `AVL`, bits 11:9.  **Software bits.**  Research D §3.3 is explicit that
    /// the IDE must not render these as architectural flags (Linux stores
    /// `_PAGE_SOFT_DIRTY` here); they are surfaced as a raw number only.
    pub available: u8,
    /// Bits 51:12, always present regardless of leaf size — useful for showing
    /// what the hardware would use at 4 KiB granularity.
    pub address_bits: u64,
    /// `IGNORED`/reserved bits 62:52.  Non-zero means a `#PF` with the `RSVD`
    /// error-code bit set, which is a bug worth surfacing.
    pub reserved: u16,
    /// Protection key, bits 62:59 (meaningful only when `CR4.PKE=1`).
    pub protection_key: u8,
    /// Bit 63 — `XD`/`NX`.  Only effective when `EFER.NXE=1`.
    pub no_execute: bool,
}

impl PageTableEntry {
    /// Decode a raw entry.
    #[must_use]
    pub fn decode(raw: u64) -> Self {
        Self {
            raw,
            present: raw & BIT_PRESENT != 0,
            writable: raw & (1 << 1) != 0,
            user: raw & (1 << 2) != 0,
            pwt: raw & (1 << 3) != 0,
            pcd: raw & (1 << 4) != 0,
            accessed: raw & (1 << 5) != 0,
            dirty: raw & (1 << 6) != 0,
            page_size: raw & BIT_PS != 0,
            global: raw & (1 << 8) != 0,
            available: ((raw >> 9) & 0b111) as u8,
            address_bits: raw & ENTRY_ADDRESS_MASK,
            // Bits 58:52 are ignored/reserved (they must be 0 or the CPU raises
            // `#PF` with the RSVD error bit); bits 62:59 are the protection key
            // and are only meaningful when `CR4.PKE=1`.  Intel SDM Vol.3 §4.5,
            // tabulated in research D §3.3.
            reserved: ((raw >> 52) & 0x7F) as u16,
            protection_key: ((raw >> 59) & 0xF) as u8,
            no_execute: raw & BIT_NX != 0,
        }
    }

    /// Are any reserved bits (62:52) non-zero?
    ///
    /// When true the CPU takes a `#PF` with the reserved-bit error code, so a
    /// page that "looks mapped" is not — worth flagging loudly.
    #[must_use]
    pub fn reserved_bits_set(self) -> bool {
        self.reserved != 0
    }

    /// The physical address of the pointed-to table (or leaf) at 4 KiB
    /// granularity.  Correct for a non-leaf entry and for a 4 KiB leaf.
    #[must_use]
    pub fn table_physical_address(self) -> u64 {
        self.address_bits
    }

    /// The physical address of a leaf, using the mask its **size** requires.
    ///
    /// 4 KiB → bits 51:12; 2 MiB → 51:21; 1 GiB → 51:30.  `level` supplies the
    /// size.  Returns `None` when `PS` is not set, because then this entry is
    /// not a leaf and has no page address at all — answering with the 4 KiB mask
    /// would be a plausible-looking wrong number.
    #[must_use]
    pub fn leaf_physical_address(self, level: Level) -> Option<u64> {
        if !self.page_size {
            return None;
        }
        Some(match level {
            Level::Pdpt => self.raw & 0x000F_FFFF_C000_0000,
            Level::Pd => self.raw & 0x000F_FFFF_FFE0_0000,
            _ => self.table_physical_address(),
        })
    }
}

/// The leaf size a completed walk resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafSize {
    /// 4 KiB, reached through all levels.
    Page4K,
    /// 2 MiB, terminated at the PDE.
    Huge2M,
    /// 1 GiB, terminated at the PDPTE.
    Huge1G,
}

impl LeafSize {
    /// Size in bytes.
    #[must_use]
    pub fn bytes(self) -> u64 {
        match self {
            LeafSize::Page4K => 4096,
            LeafSize::Huge2M => 2 * 1024 * 1024,
            LeafSize::Huge1G => 1024 * 1024 * 1024,
        }
    }

    /// How many low VA bits are the page offset.
    #[must_use]
    pub fn offset_bits(self) -> u32 {
        match self {
            LeafSize::Page4K => 12,
            LeafSize::Huge2M => 21,
            LeafSize::Huge1G => 30,
        }
    }

    /// The `Level` that this size is terminated at.
    #[must_use]
    pub fn terminating_level(self) -> Level {
        match self {
            LeafSize::Page4K => Level::Pt,
            LeafSize::Huge2M => Level::Pd,
            LeafSize::Huge1G => Level::Pdpt,
        }
    }
}

/// One step of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkLevel {
    /// Which table this entry came from.
    pub level: Level,
    /// The index into that table (the extracted VA bits).
    pub index: u16,
    /// The physical address of the table itself.
    pub table_address: u64,
    /// The physical byte offset of this entry within that table.
    pub entry_address: u64,
    /// The decoded entry.
    pub entry: PageTableEntry,
}

impl WalkLevel {
    /// How many low VA bits this level's index consumes, counting from bit 12.
    #[must_use]
    pub fn bits_consumed(self) -> u32 {
        let (high, _) = self.level.index_bits();
        high + 1 - 12
    }
}

/// Why a walk could not complete.
///
/// Each variant maps to a distinct, checkable machine state.  **None of them
/// means "the address is unmapped"** — that is `PageTableWalk::not_present`,
/// which is a successful walk that found `P=0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkError {
    /// `CR0.PG=0`: there is no page table to walk.
    PagingDisabled {
        /// The `CR0` value that was inspected.
        cr0: u64,
    },
    /// `CR0.PG=1` but `EFER.LMA=0`: the machine is in 32-bit PAE or non-PAE
    /// mode, whose tables have a different shape.  Reported rather than walked.
    NotLongMode {
        /// The `EFER` value that was inspected.
        efer: u64,
    },
    /// The virtual address is not canonical, so the CPU would `#GP` before
    /// touching a table.
    NonCanonical {
        /// The address that was requested.
        address: u64,
        /// The canonical width in bits (48 or 57).
        bits: u32,
    },
    /// A table address is outside guest RAM.
    TableNotInRam {
        /// Which level's table.
        level: Level,
        /// The physical address that was not in RAM.
        address: u64,
    },
    /// The memory source failed while reading an entry.
    Memory {
        /// Which level's table was being read.
        level: Level,
        /// The underlying failure.
        source: MemoryError,
    },
    /// `P=0` on the entry at `level`: the mapping stops here.
    NotPresent {
        /// Which level's entry was not present.
        level: Level,
        /// The index within that table.
        index: u16,
        /// The raw entry (`0` in the normal case).
        raw: u64,
    },
    /// The same physical table was reached twice, i.e. the table is
    /// self-referential (or otherwise cyclic).
    Cycle {
        /// The physical address that recurred.
        address: u64,
        /// Which level reached it the second time.
        level: Level,
    },
    /// `PS` was set at a level where it is architecturally reserved
    /// (`PML5E`, `PML4E`, `PTE`).
    ReservedPageSizeBit {
        /// The offending level.
        level: Level,
        /// The raw entry.
        raw: u64,
    },
    /// A leaf's physical address plus the page offset overflowed `u64`, or the
    /// resolved address fell outside RAM.
    LeafOutOfRange {
        /// The resolved physical address.
        address: u64,
    },
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalkError::PagingDisabled { cr0 } => write!(
                f,
                "cannot walk page tables: CR0.PG=0 (CR0={cr0:#018x}); CR3 is not a \
                 page-table pointer while paging is off"
            ),
            WalkError::NotLongMode { efer } => write!(
                f,
                "cannot walk 4-level page tables: EFER.LMA=0 (EFER={efer:#018x}); this \
                 is 32-bit PAE or non-PAE paging, which has a different table layout"
            ),
            WalkError::NonCanonical { address, bits } => write!(
                f,
                "address {address:#018x} is not canonical for {bits}-bit paging; the CPU \
                 would #GP before consulting a page table"
            ),
            WalkError::TableNotInRam { level, address } => write!(
                f,
                "{} table address {address:#018x} is not inside guest RAM",
                level.name()
            ),
            WalkError::Memory { level, source } => {
                write!(f, "reading the {} table failed: {source}", level.name())
            }
            WalkError::NotPresent { level, index, raw } => write!(
                f,
                "{}[{index}] is not present (raw={raw:#018x}); the mapping stops here",
                level.name()
            ),
            WalkError::Cycle { address, level } => write!(
                f,
                "page-table cycle: {} table {address:#018x} was already visited",
                level.name()
            ),
            WalkError::ReservedPageSizeBit { level, raw } => write!(
                f,
                "{} entry {raw:#018x} has PS=1, which is reserved at this level",
                level.name()
            ),
            WalkError::LeafOutOfRange { address } => {
                write!(f, "resolved leaf address {address:#018x} is out of range")
            }
        }
    }
}

impl std::error::Error for WalkError {}

/// The result of a successful walk that ended at `P=0`.
///
/// This is a **successful** outcome with a positive finding, kept separate from
/// [`WalkError`] because "the mapping stops here" and "I could not look" are
/// different answers and the UI must show them differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotPresent {
    /// The levels that did resolve, in order from the top.
    pub levels: Vec<WalkLevel>,
    /// The level whose entry was not present.
    pub level: Level,
    /// The index within that table.
    pub index: u16,
    /// The physical address of the absent entry itself — useful because the UI
    /// wants to show *where* the hole is.
    pub entry_address: u64,
}

/// The result of a completed walk that reached a leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTableWalk {
    /// The address that was requested.
    pub address: u64,
    /// Every level visited, in order from the top.
    pub levels: Vec<WalkLevel>,
    /// The leaf size the walk terminated at.
    pub leaf_size: LeafSize,
    /// Physical address of the page base (the offset is *not* added).
    pub physical_base: u64,
    /// Offset within the page.
    pub page_offset: u64,
    /// The leaf's own permission bits.
    ///
    /// Research D §3.8: these are **not** the effective permissions.  x86 ANDs
    /// `U/S`, `R/W` (and `P`) down the whole path, so a page whose leaf says `U`
    /// under a supervisor-only `PML4E` is not user-accessible at all — QEMU's
    /// `info tlb` shows the leaf bits while `info mem` shows the AND, and the two
    /// disagreeing in the same capture is exactly the confusion this API exists
    /// to prevent.
    pub leaf: PageTableEntry,
    /// Whether the walk is in 5-level mode.
    pub la57: bool,
}

impl PageTableWalk {
    /// The effective `U/S` bit: user-accessible only if **every** level agreed.
    #[must_use]
    pub fn effective_user(&self) -> bool {
        self.levels.iter().all(|level| level.entry.user)
    }

    /// The effective `R/W` bit: writable only if every level agreed.
    #[must_use]
    pub fn effective_writable(&self) -> bool {
        self.levels.iter().all(|level| level.entry.writable)
    }

    /// The effective `NX` bit: non-executable if any level set it.
    ///
    /// NX is *not* ANDed like the permission bits — the SDM makes `NX` at any
    /// level of the path take effect, which is the opposite direction and a
    /// classic source of wrong "this page is executable" UI.
    #[must_use]
    pub fn effective_no_execute(&self) -> bool {
        self.levels.iter().any(|level| level.entry.no_execute)
    }

    /// The resolved physical address, offset included.
    #[must_use]
    pub fn physical_address(&self) -> u64 {
        self.physical_base | self.page_offset
    }

    /// The physical address range this leaf covers.
    #[must_use]
    pub fn physical_range(&self) -> (u64, u64) {
        (
            self.physical_base,
            self.physical_base + self.leaf_size.bytes(),
        )
    }

    /// The virtual address range this leaf covers.
    #[must_use]
    pub fn virtual_range(&self) -> (u64, u64) {
        let base = self.address & !(self.leaf_size.bytes() - 1);
        (base, base + self.leaf_size.bytes())
    }

    /// A one-line summary of the path, for logs and reports.
    #[must_use]
    pub fn path_summary(&self) -> String {
        self.levels
            .iter()
            .map(|level| format!("{}[{}]", level.level.name(), level.index))
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    /// Can the CPU write to this page, ignoring mode and `CR0.WP`?
    #[must_use]
    pub fn writable(&self) -> bool {
        self.effective_writable()
    }

    /// Can the CPU execute from this page?
    #[must_use]
    pub fn executable(&self) -> bool {
        !self.effective_no_execute()
    }
}

/// How canonical an address is for a given paging width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canonicality {
    /// The address is canonical and can be walked.
    Canonical,
    /// The address is not canonical; the CPU would `#GP`.
    NonCanonical,
}

/// Is `address` canonical for `bits`-bit paging (48 or 57)?
///
/// For 48-bit paging bits 63:47 must all equal bit 47; for 57-bit, bits 63:57
/// must equal bit 56.  Research D §3.4 guard 3: a UI that lets the user type an
/// arbitrary address must check this first, because a non-canonical address
/// never reaches a page table.
#[must_use]
pub fn canonicality(address: u64, bits: u32) -> Canonicality {
    // A 48-bit address is canonical when bits 63:47 are all copies of bit 47;
    // a 57-bit address when bits 63:57 all copy bit 56.  The check is therefore
    // "every bit above the sign bit equals the sign bit", which is exactly
    // "sign-extending the low `bits` bits reproduces the whole value".
    if !(1..=64).contains(&bits) {
        return Canonicality::Canonical;
    }
    let top = bits - 1;
    if top >= 63 {
        return Canonicality::Canonical;
    }
    // Sign-extend the low `bits` bits and compare.  Writing it this way avoids
    // the off-by-one that an explicit mask invites: the mask must cover
    // `63 - top` bits, i.e. `(1 << (64 - top)) - 1`, not `>> top`.
    let sign_extended = ((address << (64 - bits)) as i64 >> (64 - bits)) as u64;
    if sign_extended == address {
        Canonicality::Canonical
    } else {
        Canonicality::NonCanonical
    }
}

/// A configured walker over one memory source.
pub struct Walker<'a> {
    memory: &'a (dyn GuestMemory + 'a),
    cr0: Cr0,
    cr4: Cr4,
    efer: Efer,
}

impl std::fmt::Debug for Walker<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The memory source is a trait object with no `Debug` bound; print the
        // control state, which is what a failing test needs to read.
        f.debug_struct("Walker")
            .field("cr0", &format_args!("{:#x}", self.cr0.0))
            .field("cr4", &format_args!("{:#x}", self.cr4.0))
            .field("efer", &format_args!("{:#x}", self.efer.0))
            .field("ram_size", &self.memory.ram_size())
            .finish()
    }
}

impl<'a> Walker<'a> {
    /// Build a walker, checking the preconditions research D requires.
    ///
    /// # Errors
    /// [`WalkError::PagingDisabled`] when `CR0.PG=0`, and
    /// [`WalkError::NotLongMode`] when `EFER.LMA=0`.  Both are structural
    /// problems with asking the question at all, so they are raised here rather
    /// than per-walk.
    pub fn new(
        memory: &'a (dyn GuestMemory + 'a),
        cr0: Cr0,
        cr4: Cr4,
        efer: Efer,
    ) -> std::result::Result<Self, WalkError> {
        if !cr0.paging_enabled() {
            return Err(WalkError::PagingDisabled { cr0: cr0.0 });
        }
        // `EFER.LMA` is only meaningful when the MSR exists; on the fixtures it
        // is present and set.  A zero `EFER` on a machine that has paging but
        // never entered long mode is exactly the 32-bit case we must refuse.
        if !efer.lma() {
            return Err(WalkError::NotLongMode { efer: efer.0 });
        }
        Ok(Self {
            memory,
            cr0,
            cr4,
            efer,
        })
    }

    /// Is the walker in 5-level mode?
    #[must_use]
    pub fn la57(&self) -> bool {
        self.cr4.la57()
    }

    /// The `CR0` under which this walker was built.
    #[must_use]
    pub fn cr0(&self) -> Cr0 {
        self.cr0
    }

    /// The `CR4` under which this walker was built.
    #[must_use]
    pub fn cr4(&self) -> Cr4 {
        self.cr4
    }

    /// The `EFER` under which this walker was built.
    #[must_use]
    pub fn efer(&self) -> Efer {
        self.efer
    }

    /// Walk `cr3` to resolve `address`.
    ///
    /// # Errors
    /// [`WalkError::NotPresent`] when the mapping genuinely stops — note this is
    /// an error *value* but a successful investigation; callers that want to
    /// distinguish it should match on the variant.  Everything else in
    /// [`WalkError`] means the question could not be answered.
    pub fn walk(
        &self,
        cr3: Cr3,
        address: u64,
    ) -> std::result::Result<PageTableWalk, WalkError> {
        let la57 = self.cr4.la57();
        let canonical_bits = if la57 { 57 } else { 48 };
        if canonicality(address, canonical_bits) == Canonicality::NonCanonical {
            return Err(WalkError::NonCanonical {
                address,
                bits: canonical_bits,
            });
        }

        // Build the index chain top-down.  LA57 inserts PML5 at depth 0 and
        // shifts every other index by the same 9 bits it adds (research D §3.2).
        let mut levels_to_visit = Vec::with_capacity(5);
        if la57 {
            levels_to_visit.push(Level::Pml5);
        }
        levels_to_visit.extend([Level::Pml4, Level::Pdpt, Level::Pd, Level::Pt]);

        let mut table = cr3.table_address();
        let mut visited: Vec<u64> = Vec::with_capacity(5);
        let mut levels: Vec<WalkLevel> = Vec::with_capacity(5);

        for level in levels_to_visit {
            // Guard 3: the table must be inside RAM.  `xp` would answer
            // "Cannot access memory" here; answer the same thing, typed.
            if self.memory.ram_size().is_some() && !self.memory.is_ram(table) {
                return Err(WalkError::TableNotInRam {
                    level,
                    address: table,
                });
            }
            // Guard 4: a self-mapping kernel would otherwise loop forever.
            if visited.contains(&table) {
                return Err(WalkError::Cycle {
                    address: table,
                    level,
                });
            }
            visited.push(table);

            let (high, low) = level.index_bits();
            let index = ((address >> low) & ((1u64 << (high - low + 1)) - 1)) as u16;
            // Entries are 8 bytes in long mode.  Research D §0 records the
            // original probe kernel getting this wrong with a 4-byte stride and
            // triple-faulting — the parser must not repeat it.
            let entry_address = table + u64::from(index) * 8;
            let raw = self
                .memory
                .read_u64(entry_address)
                .map_err(|source| WalkError::Memory { level, source })?;
            let entry = PageTableEntry::decode(raw);

            levels.push(WalkLevel {
                level,
                index,
                table_address: table,
                entry_address,
                entry,
            });

            // Guard: `PS` at a level that cannot carry it is an architectural
            // violation, not a 4 KiB page.
            if entry.page_size && !level.huge_page_allowed() {
                return Err(WalkError::ReservedPageSizeBit { level, raw });
            }
            // A huge-page leaf terminates the walk right here.
            if entry.page_size {
                let size = level
                    .huge_page_size()
                    .expect("huge_page_allowed implies a size");
                let leaf_size = if size == 1024 * 1024 * 1024 {
                    LeafSize::Huge1G
                } else {
                    LeafSize::Huge2M
                };
                let physical_base = entry
                    .leaf_physical_address(level)
                    .expect("page_size was just checked");
                let page_offset = address & (size - 1);
                let physical = physical_base
                    .checked_add(page_offset)
                    .ok_or(WalkError::LeafOutOfRange {
                        address: physical_base,
                    })?;
                if self.memory.ram_size().is_some() && !self.memory.is_ram(physical) {
                    return Err(WalkError::LeafOutOfRange { address: physical });
                }
                return Ok(PageTableWalk {
                    address,
                    levels,
                    leaf_size,
                    physical_base,
                    page_offset,
                    leaf: entry,
                    la57,
                });
            }
            if !entry.present {
                // The mapping genuinely stops here.  Surface the entry address
                // too, because the UI wants to show where the hole is.
                return Err(WalkError::NotPresent {
                    level,
                    index,
                    raw,
                });
            }
            table = entry.table_physical_address();
        }

        // Fell all the way through to a 4 KiB leaf.  `entry` is the PTE.
        let leaf = levels
            .last()
            .expect("the chain always has at least one level")
            .entry;
        let physical_base = leaf.table_physical_address();
        let page_offset = address & 0xFFF;
        let physical = physical_base
            .checked_add(page_offset)
            .ok_or(WalkError::LeafOutOfRange {
                address: physical_base,
            })?;
        if self.memory.ram_size().is_some() && !self.memory.is_ram(physical) {
            return Err(WalkError::LeafOutOfRange { address: physical });
        }
        Ok(PageTableWalk {
            address,
            levels,
            leaf_size: LeafSize::Page4K,
            physical_base,
            page_offset,
            leaf,
            la57,
        })
    }

    /// Walk and translate a [`WalkError::NotPresent`] into the structured
    /// [`NotPresent`] finding, keeping the partial path.
    ///
    /// Use this when the UI wants to *display* the hole (with the path that
    /// reached it) rather than merely report a failure.
    ///
    /// # Errors
    /// Every [`WalkError`] except `NotPresent`, passed through unchanged.
    pub fn walk_or_explain(
        &self,
        cr3: Cr3,
        address: u64,
    ) -> std::result::Result<std::result::Result<PageTableWalk, NotPresent>, WalkError> {
        // Re-run the walk, capturing the partial path on a not-present stop.
        match self.walk(cr3, address) {
            Ok(walk) => Ok(Ok(walk)),
            Err(WalkError::NotPresent { level, index, .. }) => {
                // Rebuild the path up to (not including) the missing entry so the
                // UI can show how far the hardware got.
                let mut partial = self.partial_path(cr3, address, level)?;
                let entry_address = partial.last().map_or(0, |last| last.entry_address);
                partial.pop();
                Ok(Err(NotPresent {
                    levels: partial,
                    level,
                    index,
                    entry_address,
                }))
            }
            Err(other) => Err(other),
        }
    }

    /// Walk until `stop` (inclusive) and return the levels visited.
    fn partial_path(
        &self,
        cr3: Cr3,
        address: u64,
        stop: Level,
    ) -> std::result::Result<Vec<WalkLevel>, WalkError> {
        let la57 = self.cr4.la57();
        let mut levels_to_visit = Vec::with_capacity(5);
        if la57 {
            levels_to_visit.push(Level::Pml5);
        }
        levels_to_visit.extend([Level::Pml4, Level::Pdpt, Level::Pd, Level::Pt]);

        let mut table = cr3.table_address();
        let mut visited: Vec<u64> = Vec::with_capacity(5);
        let mut out: Vec<WalkLevel> = Vec::with_capacity(5);
        for level in levels_to_visit {
            if self.memory.ram_size().is_some() && !self.memory.is_ram(table) {
                return Err(WalkError::TableNotInRam {
                    level,
                    address: table,
                });
            }
            if visited.contains(&table) {
                return Err(WalkError::Cycle {
                    address: table,
                    level,
                });
            }
            visited.push(table);
            let (high, low) = level.index_bits();
            let index = ((address >> low) & ((1u64 << (high - low + 1)) - 1)) as u16;
            let entry_address = table + u64::from(index) * 8;
            let raw = self
                .memory
                .read_u64(entry_address)
                .map_err(|source| WalkError::Memory { level, source })?;
            let entry = PageTableEntry::decode(raw);
            out.push(WalkLevel {
                level,
                index,
                table_address: table,
                entry_address,
                entry,
            });
            if level == stop {
                return Ok(out);
            }
            table = entry.table_physical_address();
        }
        Ok(out)
    }
}

/// Convenience: parse the register facts out of a parsed monitor report and
/// build a walker, so the common path does not re-implement the bit plumbing.
///
/// # Errors
/// `E_INTERNAL` when a required register is absent or not hexadecimal;
/// [`WalkError`] is returned as an `E_INTERNAL` error whose detail names the
/// condition, because from the IPC boundary's point of view this is a failed
/// command, and the *reason* is what the UI shows.
pub fn walk_from_registers(
    memory: &dyn GuestMemory,
    registers: &crate::monitor::RegistersReport,
    address: u64,
) -> Result<std::result::Result<PageTableWalk, WalkError>> {
    let read = |name: &str| -> Result<u64> {
        registers.register_u64(name).map_err(|reason| {
            PrincessError::internal(format!("cannot walk page tables: {reason}"))
                .with_detail(reason)
        })
    };
    let cr0 = Cr0(read("CR0")?);
    let cr3 = Cr3(read("CR3")?);
    let cr4 = Cr4(read("CR4")?);
    // EFER is absent from `info registers` on some builds; treat "unknown" as
    // long mode *only* if the walker's other evidence says so, and otherwise say
    // explicitly what is missing rather than assuming LMA=1.
    let efer = Efer(match registers.register_u64("EFER") {
        Ok(value) => value,
        Err(_) => {
            return Err(PrincessError::internal(
                "cannot walk page tables: EFER is not reported, so long mode cannot be \
                 confirmed",
            )
            .with_detail("`info registers` did not produce an EFER line".to_string()))
        }
    });

    let walker = Walker::new(memory, cr0, cr4, efer);
    match walker {
        Ok(walker) => Ok(walker.walk(cr3, address)),
        Err(err) => Err(PrincessError::internal(format!(
            "cannot walk page tables from CR3={:#x}: {err}",
            cr3.0
        ))
        .with_detail(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guestmem::GuestRam;

    /// The paging fixture's page tables, exactly as the guest kernel printed
    /// them on the serial line and as `fixtures/qemu-monitor/info-registers.txt`
    /// reports them.
    ///
    /// Ground truth (`fixtures/qemu-monitor/capture-serial.log`):
    /// ```text
    /// PML4[0] = 0x105023 -> phys=0x105000
    /// PDPT[0] = 0x106023 -> phys=0x106000
    /// PD[0]   = 0x107023 -> phys=0x107000
    /// PD[1]   = 0x200083 -> phys=0x200000   (2 MiB huge, PS=1)
    /// PD[2]   = 0x00000000 -> not present
    /// PT[0]   = 0x000003 -> phys=0x0
    /// PT[510] = 0x1fe003 -> phys=0x1fe000
    /// PT[511] = 0x00000000 -> not present
    /// ```
    fn fixture_ram() -> GuestRam {
        let mut ram = GuestRam::new(256 * 1024 * 1024);
        // PML4
        ram.write_u64(0x10_4000, 0x10_5023).unwrap();
        // PDPT
        ram.write_u64(0x10_5000, 0x10_6023).unwrap();
        // PD
        ram.write_u64(0x10_6000, 0x10_7023).unwrap(); // [0] -> PT
        ram.write_u64(0x10_6008, 0x0020_0083).unwrap(); // [1] 2 MiB huge
        ram.write_u64(0x10_6010, 0x0).unwrap(); // [2] absent
        for index in 3..512u64 {
            // [3..511] 2 MiB huge identity pages, as the guest prints.
            ram.write_u64(0x10_6000 + index * 8, (index << 21) | 0x83)
                .unwrap();
        }
        // PT[0..510] 4 KiB identity pages; [511] absent.
        for index in 0..511u64 {
            ram.write_u64(0x10_7000 + index * 8, (index << 12) | 0x3)
                .unwrap();
        }
        ram.write_u64(0x10_7000 + 511 * 8, 0).unwrap();
        ram
    }

    fn fixture_walker(ram: &GuestRam) -> Walker<'_> {
        // CR0=0x80000011 (PG=1), CR4=0x20 (PAE=1), EFER=0x500 (LME|NXE — the
        // sample has LMA clear because the vCPU is halted with paging on but the
        // recorded EFER is the reset-ish value; the fixture's own serial line
        // reports EFER=0xd00.  Use the value the guest itself printed).
        Walker::new(ram, Cr0(0x8000_0011), Cr4(0x20), Efer(0xd00)).expect("walker")
    }

    const FIXTURE_CR3: Cr3 = Cr3(0x10_4000);

    #[test]
    fn cr3_low_bits_are_masked_off() {
        // Research D §6 item 9: never assume PCID is zero.
        let cr3 = Cr3(0x10_4000 | 0xabc);
        assert_eq!(cr3.table_address(), 0x10_4000);
        assert_eq!(cr3.pcid(), 0xabc);
        assert!(cr3.has_low_bits_set());
        // And the fixture's own CR3 genuinely has zero low bits — asserted, not
        // assumed.
        assert_eq!(FIXTURE_CR3.table_address(), 0x10_4000);
        assert!(!FIXTURE_CR3.has_low_bits_set());
    }

    #[test]
    fn a_four_level_walk_resolves_a_4kib_page() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        let walk = walker.walk(FIXTURE_CR3, 0x1234).expect("0x1234 is mapped");
        assert_eq!(walk.leaf_size, LeafSize::Page4K);
        assert_eq!(walk.levels.len(), 4, "PML4 -> PDPT -> PD -> PT");
        assert_eq!(
            walk.path_summary(),
            "PML4[0] -> PDPT[0] -> PD[0] -> PT[1]"
        );
        // 0x1234: PT index 1, offset 0x234; PT[1] = (1<<12)|3 = 0x1003.
        assert_eq!(walk.physical_base, 0x1000);
        assert_eq!(walk.page_offset, 0x234);
        assert_eq!(walk.physical_address(), 0x1234);
        assert_eq!(walk.virtual_range(), (0x1000, 0x2000));
        assert_eq!(walk.physical_range(), (0x1000, 0x2000));

        // Bit-level decoding of every level.
        let pml4e = walk.levels[0].entry;
        assert_eq!(pml4e.raw, 0x10_5023);
        assert!(pml4e.present && pml4e.writable && pml4e.accessed);
        assert!(!pml4e.user && !pml4e.page_size && !pml4e.no_execute);
        assert_eq!(pml4e.table_physical_address(), 0x10_5000);
        assert_eq!(pml4e.available, 0, "bits 11:9 are software bits, not flags");

        let pte = walk.levels[3].entry;
        assert_eq!(pte.raw, 0x1003);
        assert!(pte.present && pte.writable && !pte.accessed && !pte.dirty);
    }

    #[test]
    fn a_walk_through_a_2mib_huge_page_uses_the_size_correct_mask() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        // 0x250000 is inside PD[1] = 0x200083, a 2 MiB leaf.
        let walk = walker.walk(FIXTURE_CR3, 0x25_0000).expect("huge page");
        assert_eq!(walk.leaf_size, LeafSize::Huge2M);
        assert_eq!(walk.leaf_size.bytes(), 0x20_0000);
        assert_eq!(walk.leaf_size.offset_bits(), 21);
        assert_eq!(walk.levels.len(), 3, "the walk stops at the PDE");
        assert_eq!(walk.path_summary(), "PML4[0] -> PDPT[0] -> PD[1]");
        // The mask must be 51:21, not 51:12.
        assert_eq!(walk.physical_base, 0x20_0000);
        assert_eq!(walk.page_offset, 0x5_0000);
        assert_eq!(walk.physical_address(), 0x25_0000);
        assert!(walk.levels[2].entry.page_size, "PS=1");
        assert_eq!(
            walk.levels[2].entry.dirty,
            false,
            "the fixture's huge pages are untouched, so D is clear"
        );
    }

    #[test]
    fn leaf_physical_address_refuses_a_non_leaf() {
        // A non-leaf entry has no page address; answering with the 4 KiB mask
        // would be a plausible-looking wrong number.
        let non_leaf = PageTableEntry::decode(0x10_5023);
        assert_eq!(non_leaf.leaf_physical_address(Level::Pdpt), None);
        let huge = PageTableEntry::decode(0x0020_0083);
        assert_eq!(huge.leaf_physical_address(Level::Pd), Some(0x20_0000));
        // Under the wrong level's mask a 1 GiB leaf would be wrong, which is why
        // the level is a parameter rather than inferred.
        let gigabyte = PageTableEntry::decode(0x4000_0183);
        assert_eq!(
            gigabyte.leaf_physical_address(Level::Pdpt),
            Some(0x4000_0000)
        );
    }

    #[test]
    fn the_deliberate_hole_is_not_present_not_a_fake_zero_entry() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        // PD[2] is deliberately absent: 0x400000..0x5fffff.
        let err = walker.walk(FIXTURE_CR3, 0x40_0000).unwrap_err();
        assert_eq!(
            err,
            WalkError::NotPresent {
                level: Level::Pd,
                index: 2,
                raw: 0
            }
        );
        assert!(err.to_string().contains("not present"));

        // And the hole below the 4 KiB region: PT[511].
        let err = walker.walk(FIXTURE_CR3, 0x1ff000).unwrap_err();
        assert_eq!(
            err,
            WalkError::NotPresent {
                level: Level::Pt,
                index: 511,
                raw: 0
            }
        );
    }

    #[test]
    fn the_hole_can_be_explained_with_its_partial_path() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        let finding = walker
            .walk_or_explain(FIXTURE_CR3, 0x40_0000)
            .expect("the walk itself was answerable")
            .expect_err("but the mapping is absent");
        assert_eq!(finding.level, Level::Pd);
        assert_eq!(finding.index, 2);
        assert_eq!(finding.levels.len(), 2, "PML4 and PDPT resolved");
        assert_eq!(finding.levels[0].level, Level::Pml4);
        assert_eq!(finding.levels[1].level, Level::Pdpt);
        // The absent entry's own address, so the UI can highlight it.
        assert_eq!(finding.entry_address, 0x10_6000 + 2 * 8);
    }

    #[test]
    fn effective_permissions_are_anded_down_the_path() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        let walk = walker.walk(FIXTURE_CR3, 0x1234).unwrap();
        // Every fixture entry is supervisor + writable, so both agree.
        assert!(!walk.effective_user(), "no level sets U/S");
        assert!(walk.effective_writable());
        assert!(walk.executable(), "no level sets NX");

        // Now make the leaf user-accessible but leave the upper levels
        // supervisor-only.  Research D §3.8: the leaf bit is NOT effective.
        let mut ram = fixture_ram();
        ram.write_u64(0x10_7008, 0x1007).unwrap(); // PT[1]: P|RW|US
        let walker = fixture_walker(&ram);
        let walk = walker.walk(FIXTURE_CR3, 0x1234).unwrap();
        assert!(walk.levels[3].entry.user, "the leaf says user");
        assert!(
            !walk.effective_user(),
            "but the path says supervisor, which is what the CPU enforces"
        );
    }

    #[test]
    fn nx_at_any_level_makes_the_page_non_executable() {
        let mut ram = fixture_ram();
        // Set NX on the PDPTE only.
        ram.write_u64(0x10_5000, 0x10_6023 | BIT_NX).unwrap();
        let walker = fixture_walker(&ram);
        let walk = walker.walk(FIXTURE_CR3, 0x1234).unwrap();
        assert!(walk.effective_no_execute());
        assert!(!walk.executable());
        assert!(walk.levels[1].entry.no_execute);
        assert!(!walk.levels[3].entry.no_execute, "the leaf itself is fine");
    }

    #[test]
    fn paging_disabled_is_refused_before_any_table_is_read() {
        let ram = fixture_ram();
        // CR0 = 0x11 -> PG clear.
        let err = Walker::new(&ram, Cr0(0x11), Cr4(0x20), Efer(0xd00)).unwrap_err();
        assert_eq!(err, WalkError::PagingDisabled { cr0: 0x11 });
        assert!(err.to_string().contains("CR0.PG=0"));
    }

    #[test]
    fn a_non_long_mode_machine_is_refused_rather_than_mis_walked() {
        let ram = fixture_ram();
        // PG=1 but LMA=0: 32-bit PAE, whose tables this walker does not model.
        let err = Walker::new(&ram, Cr0(0x8000_0011), Cr4(0x20), Efer(0x0)).unwrap_err();
        assert_eq!(err, WalkError::NotLongMode { efer: 0 });
        assert!(err.to_string().contains("32-bit PAE"));
    }

    #[test]
    fn a_non_canonical_address_is_refused() {
        let ram = fixture_ram();
        let walker = fixture_walker(&ram);
        // 0x0000_8000_0000_0000 has bit 47 set but bits 63:48 clear.
        let err = walker.walk(FIXTURE_CR3, 0x0000_8000_0000_0000).unwrap_err();
        assert_eq!(
            err,
            WalkError::NonCanonical {
                address: 0x0000_8000_0000_0000,
                bits: 48
            }
        );
        // And a properly sign-extended kernel address is canonical.
        assert_eq!(
            canonicality(0xffff_8000_0000_0000, 48),
            Canonicality::Canonical
        );
        assert_eq!(canonicality(0x0000_7fff_ffff_ffff, 48), Canonicality::Canonical);
    }

    #[test]
    fn a_table_address_outside_ram_is_reported_not_read_as_zero() {
        // Research D §6 item 22: a walker that ignores "Cannot access memory"
        // builds a tree of zeros.  Point PML4E at a non-RAM address.
        let mut ram = GuestRam::new(0x20_0000);
        // The address field is bits 51:12, so `0x4000_0023` names table
        // `0x4000_0000`; everything in 52:58 must stay clear or the entry would
        // also be a reserved-bit violation.
        ram.write_u64(0x10_4000, 0x0000_0000_4000_0023).unwrap();
        let walker = Walker::new(&ram, Cr0(0x8000_0011), Cr4(0x20), Efer(0xd00)).unwrap();
        let err = walker.walk(FIXTURE_CR3, 0x1234).unwrap_err();
        assert_eq!(
            err,
            WalkError::TableNotInRam {
                level: Level::Pdpt,
                address: 0x4000_0000
            }
        );
    }

    #[test]
    fn a_cycle_is_detected_instead_of_looping_forever() {
        // Self-mapping PML4[0] -> PML4: the classic kernel trick that would make
        // a naive walker recurse until it runs out of stack.
        let mut ram = GuestRam::new(0x20_0000);
        ram.write_u64(0x10_4000, 0x10_4023).unwrap();
        let walker = Walker::new(&ram, Cr0(0x8000_0011), Cr4(0x20), Efer(0xd00)).unwrap();
        // PML4[0] names 0x104000 — the table we are already reading — so the
        // *second* level is where the repeat is detected.  Reporting the level
        // at which the cycle was caught is the honest answer; the address is
        // what tells the user which table is self-referential.
        let err = walker.walk(FIXTURE_CR3, 0x1234).unwrap_err();
        assert_eq!(
            err,
            WalkError::Cycle {
                address: 0x10_4000,
                level: Level::Pdpt
            }
        );
    }

    #[test]
    fn ps_at_a_level_that_cannot_carry_it_is_reported() {
        let mut ram = fixture_ram();
        // PS on the PML4E is architecturally reserved.
        ram.write_u64(0x10_4000, 0x10_5023 | BIT_PS).unwrap();
        let walker = fixture_walker(&ram);
        let err = walker.walk(FIXTURE_CR3, 0x1234).unwrap_err();
        assert!(matches!(
            err,
            WalkError::ReservedPageSizeBit {
                level: Level::Pml4,
                ..
            }
        ));
    }

    #[test]
    fn reserved_address_bits_are_surfaced() {
        // Bit 52 is reserved; non-zero means a #PF with the RSVD error bit.
        let bad = PageTableEntry::decode(0x0010_0000_0000_0023);
        assert!(bad.reserved_bits_set());
        let good = PageTableEntry::decode(0x10_5023);
        assert!(!good.reserved_bits_set());
    }

    #[test]
    fn protection_key_is_decoded_separately_from_reserved() {
        // Bits 62:59 are the PKRU index when CR4.PKE=1.  Encoding 5 in that
        // field means bits 59 and 61 set, i.e. `0x28` in the 63:56 byte, with
        // 58:52 left clear.
        let with_key = PageTableEntry::decode(0x2800_0000_0000_0023);
        assert_eq!(with_key.protection_key, 0x5);
        assert!(
            !with_key.reserved_bits_set(),
            "59:62 is the protection key, not reserved"
        );
        // And a genuinely reserved bit (52 is inside 58:52) must still be seen
        // even when the protection key is also set.
        let both = PageTableEntry::decode(0x2810_0000_0000_0023);
        assert_eq!(both.protection_key, 0x5);
        assert!(both.reserved_bits_set());
    }

    #[test]
    fn a_1gib_leaf_is_handled_at_the_pdpt() {
        // ptdemo-style layout from research D §3.5: PDPT[1] = 0x40000183
        // (P|RW|G|PS), a 1 GiB identity page at 0x40000000.
        let mut ram = GuestRam::new(2 * 1024 * 1024 * 1024);
        ram.write_u64(0x1000, 0x2023).unwrap(); // PML4[0] -> PDPT @0x2000
        ram.write_u64(0x2000, 0x3023).unwrap(); // PDPT[0] -> PD @0x3000
        ram.write_u64(0x3000, 0x4023).unwrap(); // PD[0] -> PT @0x4000
        ram.write_u64(0x4000, 0x1003).unwrap(); // PT[1]
        ram.write_u64(0x2008, 0x4000_0183).unwrap(); // PDPT[1] = 1 GiB leaf

        let walker = Walker::new(&ram, Cr0(0x8000_0011), Cr4(0x20), Efer(0xd00)).unwrap();
        let walk = walker.walk(Cr3(0x1000), 0x4000_0000 + 0x1234).unwrap();
        assert_eq!(walk.leaf_size, LeafSize::Huge1G);
        assert_eq!(walk.leaf_size.offset_bits(), 30);
        assert_eq!(walk.levels.len(), 2, "PML4 -> PDPT, then PS terminates");
        assert_eq!(walk.physical_base, 0x4000_0000);
        assert_eq!(walk.page_offset, 0x1234);
        assert_eq!(walk.physical_address(), 0x4000_1234);
        assert!(walk.levels[1].entry.global, "G is set on this leaf");
        assert!(walk.levels[1].entry.page_size);
        // The 51:30 mask is what makes this right; the 51:12 mask would have
        // produced 0x40000000 anyway here, but the level check is what the API
        // guarantees.
        assert_eq!(
            walk.levels[1].entry.leaf_physical_address(Level::Pdpt),
            Some(0x4000_0000)
        );
    }

    #[test]
    fn la57_adds_a_pml5_level_and_shifts_the_indexes() {
        // CR4.LA57 = bit 12; EFER.LMA must still be set.
        let mut ram = GuestRam::new(0x100_0000);
        ram.write_u64(0x10_0000, 0x11_0023).unwrap(); // PML5[0] -> PML4 @0x11000
        ram.write_u64(0x11_0000, 0x12_0023).unwrap(); // PML4[0] -> PDPT @0x12000
        ram.write_u64(0x12_0000, 0x13_0023).unwrap(); // PDPT[0] -> PD @0x13000
        ram.write_u64(0x13_0000, 0x14_0023).unwrap(); // PD[0] -> PT @0x14000
        ram.write_u64(0x14_0000, 0x20_0003).unwrap(); // PT[0] -> phys 0x200000
        ram.write_u64(0x14_0010, 0x20_0003).unwrap(); // PT[2] -> phys 0x200000 (0x2000 index)

        let cr4 = Cr4(0x20 | (1 << 12)); // PAE | LA57
        let walker = Walker::new(&ram, Cr0(0x8000_0011), cr4, Efer(0xd00)).unwrap();
        assert!(walker.la57());
        let walk = walker.walk(Cr3(0x10_0000), 0x2000).unwrap();
        assert!(walk.la57);
        assert_eq!(walk.levels.len(), 5, "PML5 -> PML4 -> PDPT -> PD -> PT");
        assert_eq!(walk.levels[0].level, Level::Pml5);
        assert_eq!(walk.path_summary(), "PML5[0] -> PML4[0] -> PDPT[0] -> PD[0] -> PT[2]");
        assert_eq!(walk.physical_base, 0x20_0000);

        // Canonicality boundaries.  Under LA57 the sign bit is 56, so a valid
        // positive address tops out at `0x00FF_FFFF_FFFF_FFFF` and a valid
        // negative one starts at `0xFF00_0000_0000_0000`.  `0x0100_...` is
        // non-canonical *under both widths* — an earlier version of this test
        // asserted otherwise, which is exactly the off-by-one-bit that makes a
        // UI accept an address the CPU would `#GP` on.
        assert_eq!(canonicality(0x00FF_FFFF_FFFF_FFFF, 57), Canonicality::Canonical);
        assert_eq!(canonicality(0xFF00_0000_0000_0000, 57), Canonicality::Canonical);
        assert_eq!(
            canonicality(0x0100_0000_0000_0000, 57),
            Canonicality::NonCanonical
        );
        assert_eq!(
            canonicality(0x0100_0000_0000_0000, 48),
            Canonicality::NonCanonical
        );
        // An address that IS canonical under LA57 but not LA48.
        assert_eq!(canonicality(0x0080_0000_0000_0000, 57), Canonicality::Canonical);
        assert_eq!(
            canonicality(0x0080_0000_0000_0000, 48),
            Canonicality::NonCanonical
        );
    }

    #[test]
    fn level_metadata_matches_the_architectural_tables() {
        assert_eq!(Level::Pml4.index_bits(), (47, 39));
        assert_eq!(Level::Pdpt.index_bits(), (38, 30));
        assert_eq!(Level::Pd.index_bits(), (29, 21));
        assert_eq!(Level::Pt.index_bits(), (20, 12));
        assert_eq!(Level::Pml5.index_bits(), (56, 48));
        assert!(!Level::Pml4.huge_page_allowed());
        assert!(!Level::Pml5.huge_page_allowed());
        assert!(!Level::Pt.huge_page_allowed());
        assert!(Level::Pdpt.huge_page_allowed());
        assert!(Level::Pd.huge_page_allowed());
        assert_eq!(Level::Pdpt.huge_page_size(), Some(1024 * 1024 * 1024));
        assert_eq!(Level::Pd.huge_page_size(), Some(2 * 1024 * 1024));
        assert_eq!(Level::Pt.huge_page_size(), None);
        // bits_consumed counts from bit 12 upward.
        let pd = WalkLevel {
            level: Level::Pd,
            index: 0,
            table_address: 0,
            entry_address: 0,
            entry: PageTableEntry::decode(0),
        };
        assert_eq!(pd.bits_consumed(), 18, "bits 29:12");
    }

    #[test]
    fn register_facts_drive_the_walk() {
        let text = include_str!("../../../fixtures/qemu-monitor/info-registers.txt");
        let registers = crate::monitor::parse_info_registers(text).expect("registers");
        let ram = fixture_ram();
        let result = walk_from_registers(&ram, &registers, 0x1234).expect("walker built");
        let walk = result.expect("0x1234 is mapped in the fixture");
        assert_eq!(walk.physical_address(), 0x1234);
        // The sample's CR3 is the fixture's CR3 — a real cross-check between the
        // recorded capture and the page-table ground truth.
        assert_eq!(walk.path_summary(), "PML4[0] -> PDPT[0] -> PD[0] -> PT[1]");
    }

    #[test]
    fn a_missing_efer_is_an_error_not_an_assumption() {
        let mut registers = crate::monitor::RegistersReport::default();
        registers
            .registers
            .insert("CR0".to_string(), "80000011".to_string());
        registers
            .registers
            .insert("CR3".to_string(), "104000".to_string());
        registers.registers.insert("CR4".to_string(), "20".to_string());
        // No EFER.  Assuming LMA=1 would be inventing a fact.
        let ram = fixture_ram();
        let err = walk_from_registers(&ram, &registers, 0x1234).unwrap_err();
        assert!(
            err.message.contains("EFER"),
            "message must name the missing register: {}",
            err.message
        );
    }
}
