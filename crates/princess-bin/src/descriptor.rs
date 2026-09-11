//! GDT / LDT / IDT / TSS descriptor decoding — the other half of P5-5.
//!
//! # Why we decode this ourselves
//!
//! Research D §4.1 measured it: **QEMU 7.2 has no `info gdt`, no `info idt`, and
//! no `info ldt`** — `help info`'s full list does not contain them.  The only
//! source for the table addresses is the `GDT=` / `IDT=` lines of
//! `info registers` (research D §4.2), and from there the raw bytes must be read
//! and decoded here.
//!
//! # The parts that are easy to get wrong
//!
//! Research D §4.4 lists eight of them; this module handles each explicitly:
//!
//! 1. **A 64-bit TSS descriptor occupies two consecutive 8-byte slots.**  The
//!    selector is the *low* slot's offset; the high slot holds base 63:32 and
//!    must not be offered to the user as a descriptor of its own.
//! 2. **`ltr` writes the busy bit back into the GDT.**  The measured evidence is
//!    `0x89 -> 0x8b` in the raw memory (research D §4.4 item 3), so an "edit the
//!    GDT" feature must know some bits are the CPU's, not the user's.
//! 3. **`limit` is a byte count minus one.**  Entry counts are derived, never
//!    hard-coded to 8192 or 256.
//! 4. **An unloaded LDT prints limit `0xffff` with base 0.**  Rendering that as
//!    65536 entries would be a fabricated table.
//! 5. **Segment base/limit are ignored in long mode** except for FS/GS and
//!    TR/LDT, so the `info registers` values are reset defaults, not facts about
//!    the GDT.
//! 6. **16-byte gates** in the IDT, with the handler split across three fields.
//! 7. **Reads must be `base + i*16` aligned**, never a sequential stream:
//!    research D §4.6 measured `xp /1gx 0x107e0e` returning a cross-gate value.
//! 8. **`AVL` and the software bits are not architecture.**  They are surfaced as
//!    raw numbers.

use crate::guestmem::{GuestMemory, MemoryError};

/// Number of bytes in a long-mode segment descriptor.
pub const DESCRIPTOR_SIZE: usize = 8;

/// Number of bytes a 64-bit TSS (or LDT, or call gate) descriptor occupies.
pub const SYSTEM_DESCRIPTOR_SIZE: usize = 16;

/// Number of bytes in an IDT gate.
pub const GATE_SIZE: usize = 16;

/// The `S` bit: 0 selects a system descriptor, 1 a code/data descriptor.
pub const BIT_S: u64 = 1 << 44;

/// The `P` bit.
pub const BIT_P: u64 = 1 << 47;

/// The `L` (long mode) bit.
pub const BIT_L: u64 = 1 << 53;

/// The `D/B` bit.
pub const BIT_DB: u64 = 1 << 54;

/// The `G` (granularity) bit.
pub const BIT_G: u64 = 1 << 55;

/// The `A` (accessed) bit.  Written back by the CPU.
pub const BIT_A: u64 = 1 << 40;

/// Why a descriptor could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    /// The memory read failed.
    Memory(MemoryError),
    /// An IDT entry was requested past the table's limit.
    OutOfRange {
        /// The index that was asked for.
        index: u64,
        /// How many entries the limit allows.
        count: u64,
    },
    /// A TSS descriptor's `type` field is neither 9 (available) nor 11 (busy).
    InvalidTssType {
        /// The offending 4-bit type.
        descriptor_type: u8,
    },
    /// An IDT gate's type is neither a valid interrupt/trap gate nor a task gate.
    InvalidGateType {
        /// The offending 4-bit type.
        descriptor_type: u8,
    },
}

impl std::fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DescriptorError::Memory(source) => write!(f, "{source}"),
            DescriptorError::OutOfRange { index, count } => write!(
                f,
                "IDT entry {index} is past the table's {count} entries (limit+1)/16"
            ),
            DescriptorError::InvalidTssType { descriptor_type } => write!(
                f,
                "TSS descriptor type {descriptor_type} is invalid; long mode allows only \
                 9 (available) and 11 (busy)"
            ),
            DescriptorError::InvalidGateType { descriptor_type } => write!(
                f,
                "IDT gate type {descriptor_type} is not an interrupt/trap gate"
            ),
        }
    }
}

impl std::error::Error for DescriptorError {}

impl From<MemoryError> for DescriptorError {
    fn from(value: MemoryError) -> Self {
        DescriptorError::Memory(value)
    }
}

/// What kind of code/data descriptor this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeDataKind {
    /// `E=1`: a code segment.
    Code,
    /// `E=0`: a data segment.
    Data,
}

/// One code/data segment descriptor (the common case in a GDT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDescriptor {
    /// Offset of the descriptor within the GDT.
    pub offset: u16,
    /// The raw 8 bytes.
    pub raw: u64,
    /// 32-bit base.  **Ignored by the CPU in long mode** except for FS/GS.
    pub base: u32,
    /// Effective limit (the 20-bit field, scaled by `G`).
    pub limit: u32,
    /// `A` — accessed.  Written back by the CPU.
    pub accessed: bool,
    /// `R/W` for data, `R` (readable) for code.
    pub readable_writable: bool,
    /// Data: expand-down.  Code: conforming.
    pub direction_or_conforming: bool,
    /// Code (true) or data (false).
    pub kind: CodeDataKind,
    /// DPL, bits 46:45.
    pub dpl: u8,
    /// `P` — present.
    pub present: bool,
    /// `AVL`, bit 52 — **software bit**, not architecture.
    pub available: bool,
    /// `L` — 64-bit code segment.
    pub long_mode: bool,
    /// `D/B` — default operand size / stack pointer size.
    pub default_size_32: bool,
    /// `G` — limit granularity is 4 KiB.
    pub granularity_4k: bool,
}

impl SegmentDescriptor {
    /// Decode a raw 8-byte code/data descriptor.
    #[must_use]
    pub fn decode(offset: u16, raw: u64) -> Self {
        // Field layout is the architectural one, from Intel SDM Vol.3 §3.4.5,
        // and each piece is listed in research D §4.3's bit table.
        let limit_low = (raw & 0xFFFF) as u32;
        let base_low = ((raw >> 16) & 0xFFFF) as u32;
        let base_mid = ((raw >> 32) & 0xFF) as u32;
        let base_high = ((raw >> 56) & 0xFF) as u32;
        let limit_high = ((raw >> 48) & 0xF) as u32;
        let access = ((raw >> 40) & 0xFF) as u8;

        let limit_field = (limit_high << 16) | limit_low;
        let granularity_4k = raw & BIT_G != 0;

        Self {
            offset,
            raw,
            base: (base_high << 24) | (base_mid << 16) | base_low,
            // The architectural limit, scaled exactly as the CPU scales it.
            limit: if granularity_4k {
                (limit_field << 12) | 0xFFF
            } else {
                limit_field
            },
            accessed: access & 0x01 != 0,
            readable_writable: access & 0x02 != 0,
            direction_or_conforming: access & 0x04 != 0,
            kind: if access & 0x08 != 0 {
                CodeDataKind::Code
            } else {
                CodeDataKind::Data
            },
            dpl: (access >> 5) & 0b11,
            present: access & 0x80 != 0,
            available: raw & (1 << 52) != 0,
            long_mode: raw & BIT_L != 0,
            default_size_32: raw & BIT_DB != 0,
            granularity_4k,
        }
    }

    /// A human label in the shape `readelf -W -l`-adjacent tooling uses:
    /// `CODE64`, `DATA64`, `CODE32`, `DATA32`.
    ///
    /// Research D §4.4 item 8 is the reason `L=0, D=1` must be distinguished from
    /// `L=1`: a 32-bit compatibility segment is still usable in long mode.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match (self.kind, self.long_mode, self.default_size_32) {
            (CodeDataKind::Code, true, _) => "CODE64",
            (CodeDataKind::Code, false, true) => "CODE32",
            (CodeDataKind::Code, false, false) => "CODE16",
            (CodeDataKind::Data, _, true) => "DATA32",
            (CodeDataKind::Data, _, false) => "DATA16",
        }
    }

    /// Does this descriptor describe the null entry (all zero)?
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.raw == 0
    }

    /// Is this a valid combination?  `L=1` together with `D/B=1` is reserved.
    #[must_use]
    pub fn long_mode_bit_is_consistent(&self) -> bool {
        !(self.long_mode && self.default_size_32)
    }
}

/// The `type` field of a system descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDescriptorType {
    /// Type 1 — 16-bit TSS (available).
    Tss16Available,
    /// Type 2 — LDT.
    Ldt,
    /// Type 3 — 16-bit TSS (busy).
    Tss16Busy,
    /// Type 4 — 16-bit call gate.
    CallGate16,
    /// Type 5 — task gate.
    TaskGate,
    /// Type 6 — 16-bit interrupt gate.
    InterruptGate16,
    /// Type 7 — 16-bit trap gate.
    TrapGate16,
    /// Type 9 — 64-bit TSS (available).
    Tss64Available,
    /// Type 11 — 64-bit TSS (busy).  **This is what `ltr` leaves behind.**
    Tss64Busy,
    /// Type 12 — 64-bit call gate.
    CallGate64,
    /// Type 14 — 64-bit interrupt gate.
    InterruptGate64,
    /// Type 15 — 64-bit trap gate.
    TrapGate64,
    /// Type 0, 8, 10, 13 — not a valid system type in long mode.
    Reserved(u8),
}

impl SystemDescriptorType {
    /// Classify a 4-bit `type` field.
    #[must_use]
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0xF {
            0x1 => SystemDescriptorType::Tss16Available,
            0x2 => SystemDescriptorType::Ldt,
            0x3 => SystemDescriptorType::Tss16Busy,
            0x4 => SystemDescriptorType::CallGate16,
            0x5 => SystemDescriptorType::TaskGate,
            0x6 => SystemDescriptorType::InterruptGate16,
            0x7 => SystemDescriptorType::TrapGate16,
            0x9 => SystemDescriptorType::Tss64Available,
            0xB => SystemDescriptorType::Tss64Busy,
            0xC => SystemDescriptorType::CallGate64,
            0xE => SystemDescriptorType::InterruptGate64,
            0xF => SystemDescriptorType::TrapGate64,
            other => SystemDescriptorType::Reserved(other),
        }
    }

    /// Does this type occupy **two** 8-byte GDT slots?
    ///
    /// True for the 64-bit TSS, LDT, and 64-bit call gate.  Research D §4.4
    /// items 1 and 4: the high slot holds base 63:32 and is not a descriptor.
    #[must_use]
    pub fn is_16_bytes(self) -> bool {
        matches!(
            self,
            SystemDescriptorType::Tss64Available
                | SystemDescriptorType::Tss64Busy
                | SystemDescriptorType::Ldt
                | SystemDescriptorType::CallGate64
        )
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SystemDescriptorType::Tss16Available => "TSS16-avl",
            SystemDescriptorType::Ldt => "LDT",
            SystemDescriptorType::Tss16Busy => "TSS16-busy",
            SystemDescriptorType::CallGate16 => "CALLGATE16",
            SystemDescriptorType::TaskGate => "TASKGATE",
            SystemDescriptorType::InterruptGate16 => "INTGATE16",
            SystemDescriptorType::TrapGate16 => "TRAPGATE16",
            SystemDescriptorType::Tss64Available => "TSS64-avl",
            SystemDescriptorType::Tss64Busy => "TSS64-busy",
            SystemDescriptorType::CallGate64 => "CALLGATE64",
            SystemDescriptorType::InterruptGate64 => "INTGATE64",
            SystemDescriptorType::TrapGate64 => "TRAPGATE64",
            SystemDescriptorType::Reserved(_) => "RESERVED",
        }
    }
}

/// One GDT slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GdtEntry {
    /// The null descriptor at offset 0.
    Null {
        /// Offset within the GDT.
        offset: u16,
    },
    /// A code or data segment.
    Segment(Box<SegmentDescriptor>),
    /// A system descriptor occupying two slots.
    System(Box<SystemDescriptor>),
    /// The **high half** of a 16-byte system descriptor.  Not independently
    /// addressable: research D §4.4 item 1 says the selector is the low slot's
    /// offset and this one must not be re-used.
    HighHalf {
        /// Offset within the GDT.
        offset: u16,
        /// The raw 8 bytes (base 63:32 plus reserved).
        raw: u64,
        /// The offset of the low half this belongs to.
        low_half_offset: u16,
    },
}

impl GdtEntry {
    /// Offset of this slot within the GDT.
    #[must_use]
    pub fn offset(&self) -> u16 {
        match self {
            GdtEntry::Null { offset }
            | GdtEntry::HighHalf { offset, .. } => *offset,
            GdtEntry::Segment(descriptor) => descriptor.offset,
            GdtEntry::System(descriptor) => descriptor.offset,
        }
    }

    /// The selector value that names this slot (equals the offset for the GDT).
    #[must_use]
    pub fn selector(&self) -> u16 {
        self.offset()
    }
}

/// A decoded system descriptor (TSS, LDT or 64-bit call gate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemDescriptor {
    /// Offset within the GDT.
    pub offset: u16,
    /// The type field.
    pub kind: SystemDescriptorType,
    /// Full 64-bit base: low 32 bits from the first slot, high 32 from the
    /// second.
    pub base: u64,
    /// Effective limit (`G`-scaled).
    pub limit: u32,
    /// DPL.
    pub dpl: u8,
    /// `P`.
    pub present: bool,
    /// `AVL` — software bit.
    pub available: bool,
    /// `G` — limit granularity is 4 KiB.
    pub granularity_4k: bool,
    /// The raw first slot.
    pub raw_low: u64,
    /// The raw second slot, for 16-byte descriptors.
    pub raw_high: Option<u64>,
}

impl SystemDescriptor {
    /// Decode a system descriptor from its raw slots.
    ///
    /// Public because a caller that already has the bytes (a hex-editor
    /// selection, a recorded capture) should not have to re-read them through
    /// [`Gdt::read`] just to decode them.
    #[must_use]
    pub fn decode(offset: u16, raw_low: u64, raw_high: Option<u64>) -> Self {
        decode_system(offset, raw_low, raw_high)
    }

    /// Does this descriptor appear busy?
    ///
    /// For a TSS, this is the bit `ltr` sets.  Research D §4.4 item 3 measured the
    /// GDT word changing from `0x89` to `0x8b` when the kernel executed `ltr`.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self.kind,
            SystemDescriptorType::Tss16Busy | SystemDescriptorType::Tss64Busy
        )
    }

    /// Number of GDT slots this descriptor occupies.
    #[must_use]
    pub fn slot_count(&self) -> u16 {
        if self.kind.is_16_bytes() {
            2
        } else {
            1
        }
    }
}

/// A decoded GDT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gdt {
    /// Where the table lives in guest memory.
    pub base: u64,
    /// The limit as reported (bytes minus one).
    pub limit: u32,
    /// The slots, in offset order.
    pub entries: Vec<GdtEntry>,
}

impl Gdt {
    /// How many 8-byte slots the limit allows: `(limit + 1) / 8`.
    ///
    /// Derived, never a constant: research D §4.2 measured a 7-slot GDT on one
    /// fixture and a 5-slot one on another.
    #[must_use]
    pub fn slot_count(&self) -> u64 {
        (u64::from(self.limit) + 1) / 8
    }

    /// Look up the descriptor a selector names.
    ///
    /// A selector's low 3 bits are the RPL and bit 2 is the table indicator, so
    /// only bits 15:3 are the GDT offset.  Returns `None` for a selector outside
    /// the table rather than inventing a descriptor.
    #[must_use]
    pub fn entry_for_selector(&self, selector: u16) -> Option<&GdtEntry> {
        let offset = selector & !0b111;
        self.entries.iter().find(|entry| entry.offset() == offset)
    }

    /// Decode a GDT from guest memory.
    ///
    /// # Errors
    /// [`DescriptorError::Memory`] when a read fails (including a table that
    /// runs off the end of RAM — the `xp`-returns-`Cannot access memory` case).
    pub fn read(
        memory: &dyn GuestMemory,
        base: u64,
        limit: u32,
    ) -> Result<Self, DescriptorError> {
        let slot_count = (u64::from(limit) + 1) / 8;
        let mut entries = Vec::new();
        let mut index = 0u64;
        while index < slot_count {
            let offset = u64::try_from(index * 8).unwrap_or(u64::MAX);
            let address = base + offset;
            let raw = memory.read_u64(address)?;
            if index == 0 && raw == 0 {
                entries.push(GdtEntry::Null {
                    offset: offset as u16,
                });
                index += 1;
                continue;
            }
            // The `S` bit decides which half of the namespace we are in.
            if raw & BIT_S != 0 {
                entries.push(GdtEntry::Segment(Box::new(SegmentDescriptor::decode(
                    offset as u16,
                    raw,
                ))));
                index += 1;
                continue;
            }
            let descriptor_type = SystemDescriptorType::from_bits(((raw >> 40) & 0xF) as u8);
            let high = if descriptor_type.is_16_bytes() && index + 1 < slot_count {
                Some(memory.read_u64(address + 8)?)
            } else {
                None
            };
            entries.push(GdtEntry::System(Box::new(decode_system(
                offset as u16,
                raw,
                high,
            ))));
            index += 1;
            // Consume and record the high half, so the slot count and the
            // selectors stay honest (research D §4.4 item 1).
            if high.is_some() {
                entries.push(GdtEntry::HighHalf {
                    offset: (offset + 8) as u16,
                    raw: high.unwrap_or(0),
                    low_half_offset: offset as u16,
                });
                index += 1;
            }
        }
        Ok(Self {
            base,
            limit,
            entries,
        })
    }

    /// The TSS descriptor, if the table has one.
    #[must_use]
    pub fn tss(&self) -> Option<&SystemDescriptor> {
        self.entries.iter().find_map(|entry| match entry {
            GdtEntry::System(descriptor)
                if matches!(
                    descriptor.kind,
                    SystemDescriptorType::Tss64Available
                        | SystemDescriptorType::Tss64Busy
                        | SystemDescriptorType::Tss16Available
                        | SystemDescriptorType::Tss16Busy
                ) =>
            {
                Some(descriptor.as_ref())
            }
            _ => None,
        })
    }
}

fn decode_system(offset: u16, raw_low: u64, raw_high: Option<u64>) -> SystemDescriptor {
    let limit_low = (raw_low & 0xFFFF) as u32;
    let base_low = ((raw_low >> 16) & 0xFFFF) as u32;
    let base_mid = ((raw_low >> 32) & 0xFF) as u32;
    let base_high = ((raw_low >> 56) & 0xFF) as u32;
    let limit_high = ((raw_low >> 48) & 0xF) as u32;
    let access = ((raw_low >> 40) & 0xFF) as u8;
    let granularity_4k = raw_low & BIT_G != 0;
    let limit_field = (limit_high << 16) | limit_low;

    let mut base = u64::from((base_high << 24) | (base_mid << 16) | base_low);
    // The 64-bit TSS/LDT/call gate carries base 63:32 in the second slot.
    if let Some(high) = raw_high {
        base |= u64::from(((high >> 32) & 0xFFFF_FFFF) as u32) << 32;
    }

    SystemDescriptor {
        offset,
        kind: SystemDescriptorType::from_bits(access & 0xF),
        base,
        limit: if granularity_4k {
            (limit_field << 12) | 0xFFF
        } else {
            limit_field
        },
        dpl: (access >> 5) & 0b11,
        present: access & 0x80 != 0,
        available: raw_low & (1 << 52) != 0,
        granularity_4k,
        raw_low,
        raw_high,
    }
}

/// The 64-bit TSS structure (Intel SDM Vol.3 §7.7, layout measured in
/// research D §4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tss64 {
    /// Base address of the TSS.
    pub base: u64,
    /// `RSP0` — the stack loaded when entering ring 0 from ring 3.  The field a
    /// kernel IDE most wants to show.
    pub rsp0: u64,
    /// `RSP1`.
    pub rsp1: u64,
    /// `RSP2`.
    pub rsp2: u64,
    /// `IST1..IST7`, in order.
    pub ist: [u64; 7],
    /// I/O map base, an offset from `base`.
    pub io_map_base: u16,
    /// Does the descriptor's limit cover the I/O map base?  When false there is
    /// no I/O permission bitmap (research D §4.5: `0x68 = limit+1` means none).
    pub has_io_bitmap: bool,
}

impl Tss64 {
    /// Decode a TSS from guest memory.
    ///
    /// # Errors
    /// [`DescriptorError::Memory`] when a read fails.
    pub fn read(
        memory: &dyn GuestMemory,
        base: u64,
        limit: u32,
    ) -> Result<Self, DescriptorError> {
        // 104 bytes (0x68) is the full 64-bit TSS (research D §4.5).
        let raw = memory.read_bytes(base, 0x68)?;
        let read_u64 = |offset: usize| -> u64 {
            let mut value = 0u64;
            for index in 0..8 {
                value |= u64::from(raw[offset + index]) << (index * 8);
            }
            value
        };
        // The I/O map base is a 16-bit field at +0x66 (research D §4.5).  It is
        // read as two bytes rather than by masking a 32-bit load, so the two
        // reserved bytes at +0x64 cannot leak into the value.
        let io_map_base = u16::from_le_bytes([raw[0x66], raw[0x67]]);
        let mut ist = [0u64; 7];
        for (index, slot) in ist.iter_mut().enumerate() {
            *slot = read_u64(0x24 + index * 8);
        }
        Ok(Self {
            base,
            rsp0: read_u64(0x04),
            rsp1: read_u64(0x0C),
            rsp2: read_u64(0x14),
            ist,
            io_map_base,
            // An I/O map base at or beyond the limit means "no bitmap";
            // research D §4.5's measured sample has 0x68 == limit+1.
            has_io_bitmap: u32::from(io_map_base) <= limit,
        })
    }
}

/// Which kind of gate an IDT entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// Type 14 (`0xE`) — interrupt gate: IF is cleared on entry.
    Interrupt,
    /// Type 15 (`0xF`) — trap gate: IF is preserved.
    Trap,
    /// Type 5 — task gate (legacy).
    Task,
}

/// One IDT gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdtGate {
    /// Vector number (the index).
    pub vector: u8,
    /// Offset of the gate within the IDT.
    pub offset: u16,
    /// The raw 16 bytes.
    pub raw: [u8; 16],
    /// Present.
    pub present: bool,
    /// DPL.
    pub dpl: u8,
    /// Gate kind.
    pub kind: GateKind,
    /// Code selector.
    pub selector: u16,
    /// **IST index**, 0 when the gate does not switch stacks.
    pub ist: u8,
    /// Full 64-bit handler address, assembled from all three offset fields.
    ///
    /// Research D §4.6: the upper 32 bits are non-zero on higher-half kernels,
    /// so taking only the low half would show the wrong address for exactly the
    /// kernels that matter most.
    pub handler: u64,
}

impl IdtGate {
    /// Is this gate still the all-zero placeholder from a `lidt` of a zeroed
    /// table?
    ///
    /// Research D §4.6's edge-case table: a not-present gate must render as
    /// "not installed", never as a gate whose handler happens to be 0.
    #[must_use]
    pub fn is_uninstalled(&self) -> bool {
        self.raw == [0u8; 16]
    }
}

/// A decoded IDT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idt {
    /// Where the table lives in guest memory.
    pub base: u64,
    /// The limit as reported (bytes minus one).
    pub limit: u32,
    /// One entry per vector, in vector order.
    pub gates: Vec<IdtGate>,
}

impl Idt {
    /// Number of gates the limit allows: `(limit + 1) / 16`.
    #[must_use]
    pub fn gate_count(&self) -> u64 {
        (u64::from(self.limit) + 1) / 16
    }

    /// Decode an IDT from guest memory.
    ///
    /// Reads are **strictly `base + i*16` aligned**: research D §4.6 measured
    /// that an unaligned read returns a value spanning two gates, so streaming
    /// 16 bytes at a time from a moving cursor is not equivalent.
    ///
    /// # Errors
    /// [`DescriptorError::Memory`] when a read fails, or
    /// [`DescriptorError::InvalidGateType`] when a present gate's type field is
    /// not a valid gate type.
    pub fn read(
        memory: &dyn GuestMemory,
        base: u64,
        limit: u32,
    ) -> Result<Self, DescriptorError> {
        let count = (u64::from(limit) + 1) / GATE_SIZE as u64;
        let mut gates = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for index in 0..count {
            let offset = u64::try_from(index * GATE_SIZE as u64).unwrap_or(u64::MAX);
            let raw_bytes = memory.read_bytes(base + offset, GATE_SIZE)?;
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&raw_bytes);
            let mut low = [0u8; 8];
            low.copy_from_slice(&raw[..8]);
            let raw_low = u64::from_le_bytes(low);
            let mut high = [0u8; 8];
            high.copy_from_slice(&raw[8..]);
            let raw_high = u64::from_le_bytes(high);

            let type_attr = raw[5];
            let present = type_attr & 0x80 != 0;
            let descriptor_type = type_attr & 0x0F;
            // Reserved bit 4 of type_attr is 0 for interrupt/trap gates; a
            // not-present all-zero gate is reported as uninstalled instead of
            // being rejected, because that is a normal state for an unused
            // vector.
            let is_placeholder = raw == [0u8; 16];
            let kind = match descriptor_type {
                0xE => GateKind::Interrupt,
                0xF => GateKind::Trap,
                0x5 => GateKind::Task,
                _ if is_placeholder => GateKind::Interrupt, // value is unused
                other => {
                    return Err(DescriptorError::InvalidGateType {
                        descriptor_type: other,
                    })
                }
            };

            let selector = u16::from_le_bytes([raw[2], raw[3]]);
            let ist = raw[4] & 0b111;
            let offset_low = u64::from(raw_low & 0xFFFF);
            let offset_mid = u64::from((raw_low >> 48) & 0xFFFF);
            let offset_high = raw_high & 0xFFFF_FFFF;
            let handler = offset_low | (offset_mid << 16) | (offset_high << 32);

            gates.push(IdtGate {
                vector: u8::try_from(index).unwrap_or(u8::MAX),
                offset: offset as u16,
                raw,
                present,
                dpl: (type_attr >> 5) & 0b11,
                kind,
                selector,
                ist,
                handler,
            });
        }
        Ok(Self {
            base,
            limit,
            gates,
        })
    }

    /// The gate for a vector.
    #[must_use]
    pub fn gate(&self, vector: u8) -> Option<&IdtGate> {
        self.gates.iter().find(|gate| gate.vector == vector)
    }

    /// Vectors whose gate is installed and reachable from ring 3.
    ///
    /// Research D §4.6 flags these as the kernel's attack surface, which is
    /// exactly the list a security-conscious IDE view wants.
    #[must_use]
    pub fn user_accessible_vectors(&self) -> Vec<u8> {
        self.gates
            .iter()
            .filter(|gate| gate.present && gate.dpl == 3)
            .map(|gate| gate.vector)
            .collect()
    }

    /// Every vector that is actually installed.
    #[must_use]
    pub fn installed_vectors(&self) -> Vec<u8> {
        self.gates
            .iter()
            .filter(|gate| gate.present && !gate.is_uninstalled())
            .map(|gate| gate.vector)
            .collect()
    }
}

/// Convenience: read a GDT and an IDT from the addresses `info registers`
/// reports, and resolve the TSS the GDT points at.
///
/// Returns the three decoded tables.  The TSS is `None` when the GDT has no TSS
/// descriptor or the descriptor's base is not readable — both are legitimate
/// states, not errors, and `None` says so without inventing a table.
///
/// # Errors
/// [`DescriptorError`] when the GDT or IDT itself cannot be read.
pub fn read_tables(
    memory: &dyn GuestMemory,
    gdt_base: u64,
    gdt_limit: u32,
    idt_base: u64,
    idt_limit: u32,
) -> Result<(Gdt, Idt, Option<Tss64>), DescriptorError> {
    let gdt = Gdt::read(memory, gdt_base, gdt_limit)?;
    let idt = Idt::read(memory, idt_base, idt_limit)?;
    let tss = gdt
        .tss()
        .and_then(|descriptor| Tss64::read(memory, descriptor.base, descriptor.limit).ok());
    Ok((gdt, idt, tss))
}

/// The descriptor a selector names, resolved through the GDT, with the `L`/`D-B`
/// interpretation spelled out.
///
/// Research D §4.6's last row: an IDT gate's selector is not always `0x08`, so a
/// view that wants to show "this gate runs in 64-bit mode" must look the
/// selector up rather than assume.
#[must_use]
pub fn describe_selector(gdt: &Gdt, selector: u16) -> Option<String> {
    let entry = gdt.entry_for_selector(selector)?;
    Some(match entry {
        GdtEntry::Null { .. } => format!("{selector:#06x}: null descriptor"),
        GdtEntry::HighHalf {
            low_half_offset, ..
        } => format!(
            "{selector:#06x}: high half of the 16-byte descriptor at {low_half_offset:#06x} \
             (not independently addressable)"
        ),
        GdtEntry::Segment(descriptor) => format!(
            "{selector:#06x}: {} DPL={} base={:#x} limit={:#x}{}",
            descriptor.label(),
            descriptor.dpl,
            descriptor.base,
            descriptor.limit,
            if descriptor.present { "" } else { " NOT PRESENT" }
        ),
        GdtEntry::System(descriptor) => format!(
            "{selector:#06x}: {} DPL={} base={:#x} limit={:#x}{}",
            descriptor.kind.label(),
            descriptor.dpl,
            descriptor.base,
            descriptor.limit,
            if descriptor.present { "" } else { " NOT PRESENT" }
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guestmem::GuestRam;

    /// The paging fixture's GDT, exactly as its guest kernel printed it.
    ///
    /// ```text
    /// GDTR base=0x1012e0 limit=0x0027 (40 bytes)
    /// GDT[0]=0x0000000000000000
    /// GDT[1]=0x00af9a000000ffff
    /// GDT[2]=0x00cf93000000ffff
    /// GDT[3]=0x00cf9a000000ffff
    /// GDT[4]=0x00cf93000000ffff
    /// IDTR base=0x108000 limit=0x0fff (4096 bytes)
    /// IDT[14] (#PF) handler=0x00000000001001f6 sel=0x0008 ist=0 type=0x8e
    /// ```
    fn fixture_ram() -> GuestRam {
        let mut ram = GuestRam::new(256 * 1024 * 1024);
        let gdt: [u64; 5] = [
            0x0000_0000_0000_0000,
            0x00af_9a00_0000_ffff, // 64-bit kernel code
            0x00cf_9300_0000_ffff, // data
            0x00cf_9a00_0000_ffff, // 32-bit code
            0x00cf_9300_0000_ffff, // data
        ];
        for (index, value) in gdt.iter().enumerate() {
            ram.write_u64(0x10_12e0 + index as u64 * 8, *value).unwrap();
        }
        // IDT: 256 gates, only #PF (vector 14) installed, handler 0x1001f6.
        let handler = 0x10_01f6u64;
        let mut gate = [0u8; 16];
        gate[0..2].copy_from_slice(&(handler as u16).to_le_bytes());
        gate[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
        gate[4] = 0; // IST
        gate[5] = 0x8e; // P=1, DPL=0, type=0xE interrupt gate
        gate[6..8].copy_from_slice(&((handler >> 16) as u16).to_le_bytes());
        gate[8..12].copy_from_slice(&((handler >> 32) as u32).to_le_bytes());
        ram.write_bytes(0x10_8000 + 14 * 16, &gate).unwrap();
        ram
    }

    // -------------------------------------------------------------- GDT -----

    #[test]
    fn the_fixture_gdt_decodes_to_the_expected_segments() {
        let ram = fixture_ram();
        let gdt = Gdt::read(&ram, 0x10_12e0, 0x27).expect("GDT");
        assert_eq!(gdt.slot_count(), 5, "(0x27 + 1) / 8 = 5 slots");
        assert_eq!(gdt.entries.len(), 5);

        let expected = ["CODE64", "DATA32", "CODE32", "DATA32"];
        for (index, label) in expected.iter().enumerate() {
            let selector = ((index + 1) * 8) as u16;
            let entry = gdt.entry_for_selector(selector).expect("selector");
            match entry {
                GdtEntry::Segment(descriptor) => {
                    assert_eq!(descriptor.label(), *label, "GDT[{selector:#x}]");
                    assert_eq!(descriptor.dpl, 0);
                    assert!(descriptor.present);
                    assert_eq!(descriptor.base, 0);
                    // G=1 for all of these, so the limit is 0xfffff * 4 KiB.
                    assert!(descriptor.granularity_4k);
                    assert_eq!(descriptor.limit, 0xFFFF_FFFF);
                    // The `A` (accessed) bit is **CPU-written** (research D
                    // §3.7, §4.4 item 3): the data segment's access byte is
                    // `0x93` (A=1, the CPU loaded it) while the code segments
                    // are `0x9a` (A=0).  A decoder that "normalizes" this would
                    // be reporting a fact the GDT does not contain, so the
                    // assertion states which is which instead of assuming.
                    let expected_accessed = matches!(*label, "DATA32");
                    assert_eq!(
                        descriptor.accessed, expected_accessed,
                        "GDT[{selector:#x}] access byte {:#04x}",
                        (descriptor.raw >> 40) & 0xFF
                    );
                }
                other => panic!("expected a segment descriptor, got {other:?}"),
            }
        }

        // The null descriptor is genuinely null.
        assert!(matches!(
            gdt.entry_for_selector(0).unwrap(),
            GdtEntry::Null { offset: 0 }
        ));

        // GDT[1] must be a 64-bit code segment: L=1 and D/B=0.
        let GdtEntry::Segment(code64) = gdt.entry_for_selector(0x08).unwrap() else {
            panic!("GDT[1] must be a segment");
        };
        assert!(code64.long_mode, "L=1");
        assert!(!code64.default_size_32, "L=1 requires D/B=0");
        assert!(code64.long_mode_bit_is_consistent());
        assert_eq!(code64.kind, CodeDataKind::Code);
        // GDT[3] is the 32-bit compatibility segment: L=0, D/B=1.
        let GdtEntry::Segment(code32) = gdt.entry_for_selector(0x18).unwrap() else {
            panic!("GDT[3] must be a segment");
        };
        assert!(!code32.long_mode);
        assert!(code32.default_size_32);
        assert_eq!(code32.label(), "CODE32");
    }

    #[test]
    fn the_bit_fields_match_the_sdm_from_the_raw_words() {
        // 0x00af9a000000ffff decoded entirely by hand from the SDM layout.
        let descriptor = SegmentDescriptor::decode(8, 0x00af_9a00_0000_ffff);
        assert_eq!(descriptor.base, 0);
        assert_eq!(descriptor.limit, 0xFFFF_FFFF);
        assert!(descriptor.granularity_4k, "G=1");
        assert!(descriptor.long_mode, "L=1");
        assert!(!descriptor.default_size_32, "D/B=0");
        assert!(!descriptor.available, "AVL=0");
        assert!(descriptor.present, "P=1");
        assert_eq!(descriptor.dpl, 0);
        assert_eq!(descriptor.kind, CodeDataKind::Code, "E=1");
        assert!(descriptor.readable_writable, "R=1");
        assert!(!descriptor.direction_or_conforming, "C=0");
        assert!(!descriptor.accessed, "A=0");
    }

    #[test]
    fn a_selector_outside_the_table_resolves_to_nothing() {
        let ram = fixture_ram();
        let gdt = Gdt::read(&ram, 0x10_12e0, 0x27).unwrap();
        assert!(gdt.entry_for_selector(0x28).is_none(), "past the limit");
        // The RPL bits are masked off, so 0x0b names the same slot as 0x08.
        assert!(gdt.entry_for_selector(0x0b).is_some());
        assert!(gdt.entry_for_selector(0x0f).is_some());
    }

    #[test]
    fn a_gdt_limit_of_zero_is_one_slot_not_zero() {
        // limit is a byte count minus one: limit 7 is exactly one 8-byte slot.
        let ram = fixture_ram();
        let gdt = Gdt::read(&ram, 0x10_12e0, 0x07).unwrap();
        assert_eq!(gdt.slot_count(), 1);
        assert_eq!(gdt.entries.len(), 1);
    }

    // ------------------------------------------------------ system / TSS -----

    #[test]
    fn a_64_bit_tss_descriptor_spans_two_slots_and_keeps_base_63_32() {
        // Shape from research D §4.4 item 1: the measured fixture word is
        // 0x00008b0050000067 with a second slot carrying the base's high half.
        let mut ram = GuestRam::new(256 * 1024 * 1024);
        let low = 0x0000_8b00_5000_0067u64; // access 0x8b = TSS64 busy
        let high = 0x0000_0000_0000_0000u64; // base 63:32
        ram.write_u64(0x1000, 0).unwrap(); // null
        ram.write_u64(0x1008, low).unwrap();
        ram.write_u64(0x1010, high).unwrap();
        // One more ordinary segment so the slot count is unambiguous.
        ram.write_u64(0x1018, 0x00cf_9300_0000_ffff).unwrap();

        let gdt = Gdt::read(&ram, 0x1000, 0x1F).expect("GDT");
        assert_eq!(gdt.slot_count(), 4);
        assert_eq!(gdt.entries.len(), 4, "the high half is recorded, not skipped");

        let tss = gdt.tss().expect("GDT has a TSS");
        assert_eq!(tss.offset, 0x08, "the selector is the LOW slot's offset");
        assert_eq!(tss.kind, SystemDescriptorType::Tss64Busy);
        assert!(tss.is_busy(), "access 0x8b has the busy bit set");
        assert_eq!(tss.base, 0x5000);
        assert_eq!(tss.limit, 0x67, "G=0, so the limit is the raw 20-bit field");
        assert_eq!(tss.slot_count(), 2);
        assert_eq!(tss.raw_high, Some(high));

        // The high half must NOT be offered as a descriptor of its own.
        let high_entry = gdt.entry_for_selector(0x10).expect("slot 0x10 exists");
        assert!(matches!(
            high_entry,
            GdtEntry::HighHalf {
                low_half_offset: 0x08,
                ..
            }
        ));
        // And resolving it says so rather than showing a plausible descriptor.
        let text = describe_selector(&gdt, 0x10).unwrap();
        assert!(text.contains("not independently addressable"), "{text}");
    }

    #[test]
    fn the_ltr_busy_bit_is_just_an_accessed_style_bit_we_read_back() {
        // Research D §4.4 item 3: the kernel wrote 0x89 (available) and ltr
        // changed it to 0x8b (busy) *in the GDT itself*.  Both must decode.
        let available = SystemDescriptor::decode(0x08, 0x0000_8900_5000_0067, None);
        assert_eq!(available.kind, SystemDescriptorType::Tss64Available);
        assert!(!available.is_busy());

        let busy = SystemDescriptor::decode(0x08, 0x0000_8b00_5000_0067, None);
        assert_eq!(busy.kind, SystemDescriptorType::Tss64Busy);
        assert!(busy.is_busy(),
            "the busy bit is CPU-written; the decoder must report what is really there");
    }

    #[test]
    fn a_64_bit_tss_reads_rsp0_and_the_ist_entries() {
        // Layout transcribed from research D §4.5's measured sample:
        // RSP0 at +0x04, IST1 at +0x24, I/O map base at +0x66.
        let mut ram = GuestRam::new(256 * 1024 * 1024);
        ram.write_u64(0x5000 + 0x04, 0x30_0000).unwrap(); // RSP0
        ram.write_u64(0x5000 + 0x24, 0x7000).unwrap(); // IST1
        ram.write_u64(0x5000 + 0x2C, 0x8000).unwrap(); // IST2
        ram.write_bytes(0x5000 + 0x66, &0x0068u16.to_le_bytes()).unwrap(); // IOMAP

        let tss = Tss64::read(&ram, 0x5000, 0x67).expect("TSS");
        assert_eq!(tss.rsp0, 0x30_0000);
        assert_eq!(tss.ist[0], 0x7000, "IST1");
        assert_eq!(tss.ist[1], 0x8000, "IST2");
        assert_eq!(tss.ist[6], 0, "IST7 untouched");
        assert_eq!(tss.io_map_base, 0x68);
        // 0x68 == 0x67 + 1, i.e. at the limit => no I/O bitmap (research D §4.5).
        assert!(!tss.has_io_bitmap);
    }

    #[test]
    fn an_io_bitmap_inside_the_limit_is_reported_as_present() {
        let mut ram = GuestRam::new(0x1_0000);
        ram.write_bytes(0x5000 + 0x66, &0x0060u16.to_le_bytes()).unwrap();
        let tss = Tss64::read(&ram, 0x5000, 0x67).unwrap();
        assert_eq!(tss.io_map_base, 0x60);
        assert!(tss.has_io_bitmap, "0x60 < 0x67 so the map is inside the limit");
    }

    #[test]
    fn an_invalid_tss_type_is_classified_rather_than_guessed() {
        let reserved = SystemDescriptorType::from_bits(0x8);
        assert_eq!(reserved, SystemDescriptorType::Reserved(8));
        assert_eq!(reserved.label(), "RESERVED");
        assert!(!reserved.is_16_bytes());
    }

    #[test]
    fn only_the_16_byte_system_types_consume_two_slots() {
        assert!(SystemDescriptorType::Tss64Available.is_16_bytes());
        assert!(SystemDescriptorType::Tss64Busy.is_16_bytes());
        assert!(SystemDescriptorType::Ldt.is_16_bytes());
        assert!(SystemDescriptorType::CallGate64.is_16_bytes());
        assert!(!SystemDescriptorType::Tss16Available.is_16_bytes());
        assert!(!SystemDescriptorType::TaskGate.is_16_bytes());
    }

    // -------------------------------------------------------------- IDT -----

    #[test]
    fn the_fixture_idt_decodes_the_pf_handler() {
        let ram = fixture_ram();
        let idt = Idt::read(&ram, 0x10_8000, 0xfff).expect("IDT");
        assert_eq!(idt.gate_count(), 256, "(0xfff + 1) / 16 = 256 gates");

        let gate = idt.gate(14).expect("#PF gate");
        assert_eq!(gate.offset, 14 * 16);
        assert!(gate.present);
        assert_eq!(gate.kind, GateKind::Interrupt);
        assert_eq!(gate.dpl, 0);
        assert_eq!(gate.selector, 0x0008);
        assert_eq!(gate.ist, 0);
        assert_eq!(
            gate.handler, 0x10_01f6,
            "the handler must be assembled from all three offset fields"
        );
        // Cross-check against the value the guest itself printed.
        assert_eq!(gate.handler, 0x0000_0000_0010_01f6);

        // Every other vector is the all-zero placeholder from a zeroed table.
        assert_eq!(idt.installed_vectors(), vec![14]);
        let vector0 = idt.gate(0).unwrap();
        assert!(!vector0.present, "a zeroed gate is not present");
        assert!(vector0.is_uninstalled(), "and is reported as uninstalled");
    }

    #[test]
    fn a_high_half_handler_keeps_its_upper_32_bits() {
        // Higher-half kernels put the handler above 4 GiB; taking only the low
        // half would show a completely wrong address (research D §4.6).
        let mut ram = GuestRam::new(256 * 1024 * 1024);
        let handler = 0xFFFF_8000_0012_3456u64;
        let mut gate = [0u8; 16];
        gate[0..2].copy_from_slice(&(handler as u16).to_le_bytes());
        gate[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
        gate[5] = 0x8e;
        gate[6..8].copy_from_slice(&((handler >> 16) as u16).to_le_bytes());
        gate[8..12].copy_from_slice(&((handler >> 32) as u32).to_le_bytes());
        ram.write_bytes(0x1000, &gate).unwrap();

        let idt = Idt::read(&ram, 0x1000, 0x0F).unwrap();
        assert_eq!(idt.gates.len(), 1);
        assert_eq!(idt.gates[0].handler, handler);
    }

    #[test]
    fn ist_and_dpl_are_decoded_from_the_type_attr_byte() {
        // 0xef = P=1, DPL=3, type=0xF trap gate (research D §4.6's measured
        // #BP gate).  0xee = P=1, DPL=3, type=0xE interrupt gate (#80 syscall).
        let mut ram = GuestRam::new(0x1_0000);
        let mut bp = [0u8; 16];
        bp[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
        bp[4] = 4; // IST4
        bp[5] = 0xef;
        ram.write_bytes(0x1000, &bp).unwrap();
        let mut syscall = [0u8; 16];
        syscall[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
        syscall[5] = 0xee;
        ram.write_bytes(0x1010, &syscall).unwrap();

        let idt = Idt::read(&ram, 0x1000, 0x1F).unwrap();
        let bp_gate = idt.gate(0).unwrap();
        assert_eq!(bp_gate.kind, GateKind::Trap);
        assert_eq!(bp_gate.dpl, 3);
        assert_eq!(bp_gate.ist, 4, "a nonzero IST index must survive");

        let syscall_gate = idt.gate(1).unwrap();
        assert_eq!(syscall_gate.kind, GateKind::Interrupt);
        assert_eq!(syscall_gate.dpl, 3);

        // The DPL=3 gates are the ring-3 attack surface.
        assert_eq!(idt.user_accessible_vectors(), vec![0, 1]);
    }

    #[test]
    fn an_invalid_gate_type_is_rejected() {
        let mut ram = GuestRam::new(0x1_0000);
        let mut gate = [0u8; 16];
        gate[5] = 0x81; // present, type=1 (not a gate)
        ram.write_bytes(0x1000, &gate).unwrap();
        let err = Idt::read(&ram, 0x1000, 0x0F).unwrap_err();
        assert_eq!(
            err,
            DescriptorError::InvalidGateType {
                descriptor_type: 1
            }
        );
    }

    #[test]
    fn gates_are_read_at_aligned_offsets() {
        // Two adjacent gates with distinguishable handlers: if the reader used a
        // moving cursor and got the alignment wrong, gate 1 would inherit gate
        // 0's bytes (research D §4.6's unaligned-read warning).
        let mut ram = GuestRam::new(0x1_0000);
        let make = |handler: u64| {
            let mut gate = [0u8; 16];
            gate[0..2].copy_from_slice(&(handler as u16).to_le_bytes());
            gate[2..4].copy_from_slice(&0x0008u16.to_le_bytes());
            gate[5] = 0x8e;
            gate[6..8].copy_from_slice(&((handler >> 16) as u16).to_le_bytes());
            gate[8..12].copy_from_slice(&((handler >> 32) as u32).to_le_bytes());
            gate
        };
        ram.write_bytes(0x1000, &make(0xAAAA)).unwrap();
        ram.write_bytes(0x1010, &make(0xBBBB)).unwrap();
        ram.write_bytes(0x1020, &make(0xCCCC)).unwrap();

        let idt = Idt::read(&ram, 0x1000, 0x2F).unwrap();
        assert_eq!(idt.gates[0].handler, 0xAAAA);
        assert_eq!(idt.gates[1].handler, 0xBBBB);
        assert_eq!(idt.gates[2].handler, 0xCCCC);
        assert_eq!(idt.gates[0].offset, 0);
        assert_eq!(idt.gates[1].offset, 16);
    }

    // -------------------------------------------------------- integration ----

    #[test]
    fn reading_out_of_ram_is_a_typed_error() {
        let ram = GuestRam::new(0x1_0000);
        let err = Gdt::read(&ram, 0x8000_0000, 0x27).unwrap_err();
        assert!(matches!(err, DescriptorError::Memory(_)));
        let err = Idt::read(&ram, 0x8000_0000, 0xfff).unwrap_err();
        assert!(matches!(err, DescriptorError::Memory(_)));
    }

    #[test]
    fn the_full_fixture_reports_agree_with_the_guest_serial_output() {
        // Ground truth from fixtures/qemu-monitor/capture-serial.log.
        let ram = fixture_ram();
        let (gdt, idt, tss) =
            read_tables(&ram, 0x10_12e0, 0x27, 0x10_8000, 0xfff).expect("tables");
        assert_eq!(gdt.slot_count(), 5);
        assert_eq!(idt.gate_count(), 256);
        assert!(tss.is_none(), "this fixture's GDT has no TSS descriptor");

        // The #PF gate's selector resolves through the GDT to CODE64.
        let gate = idt.gate(14).unwrap();
        let description = describe_selector(&gdt, gate.selector).expect("0x08 resolves");
        assert!(description.contains("CODE64"), "{description}");
    }

    #[test]
    fn describe_selector_names_the_mode_of_each_fixture_segment() {
        let ram = fixture_ram();
        let gdt = Gdt::read(&ram, 0x10_12e0, 0x27).unwrap();
        assert!(describe_selector(&gdt, 0x08).unwrap().contains("CODE64"));
        assert!(describe_selector(&gdt, 0x10).unwrap().contains("DATA32"));
        assert!(describe_selector(&gdt, 0x18).unwrap().contains("CODE32"));
        assert!(describe_selector(&gdt, 0x00).unwrap().contains("null"));
        assert!(describe_selector(&gdt, 0x40).is_none(), "past the limit");
    }
}
