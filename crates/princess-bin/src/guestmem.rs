//! The physical-memory source a page-table walk reads through.
//!
//! # Why this is a trait and not a `HexReader` call
//!
//! A page-table walk needs eight bytes at a time from *guest physical* memory.
//! In the IDE that memory comes from one of two places depending on context:
//!
//! * **offline / recorded** — a `qemu-monitor` capture plus a saved image, or a
//!   test fixture that replays bytes directly;
//! * **live** — the running VM, via the debug backend's memory read
//!   (`DebugBackend::read_memory`) or QMP `xp`.
//!
//! Both are legitimate and neither is a special case.  Making the walker depend
//! on a trait lets P5 be tested against recorded samples with no QEMU running
//! (which is exactly what acceptance P5-4 and P5-5 require) while still working
//! against a live VM.
//!
//! # The failure mode this module exists to prevent
//!
//! Research D §5.4 and §6 item 22: `xp` on a non-RAM address answers with the
//! *text* `Cannot access memory`, and `xp` on un-written RAM answers with `0`.
//! A walker that treats both as "the entry is zero" builds a **fake tree of
//! zeros** and shows it to the user as if it were the real page table.  So
//! [`GuestMemory::read_u64`] returns a [`MemoryError`] for "outside RAM" and a
//! perfectly ordinary zero for "inside RAM, never written", and the walker
//! propagates the distinction all the way to the UI.

use std::collections::BTreeMap;

/// Why a guest-memory read could not be answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// The address is outside the guest's RAM.
    ///
    /// Distinct from a zero value: this is the `Cannot access memory` case, and
    /// a walker that hits it must stop rather than invent a page-table entry.
    NotInRam {
        /// The physical address that was requested.
        address: u64,
    },
    /// Fewer bytes were available than requested (the region ends mid-read).
    Short {
        /// The physical address that was requested.
        address: u64,
        /// How many bytes were wanted.
        wanted: usize,
        /// How many were available.
        got: usize,
    },
    /// The underlying source itself failed (I/O, a dead debug session, ...).
    Source {
        /// The physical address that was requested.
        address: u64,
        /// The producer's own message, kept verbatim.
        message: String,
    },
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::NotInRam { address } => {
                write!(f, "physical address {address:#x} is outside guest RAM")
            }
            MemoryError::Short {
                address,
                wanted,
                got,
            } => write!(
                f,
                "wanted {wanted} bytes at physical {address:#x}, only {got} available"
            ),
            MemoryError::Source { address, message } => {
                write!(f, "reading physical {address:#x} failed: {message}")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

/// Random-access read of guest physical memory.
pub trait GuestMemory {
    /// Read a little-endian `u64` at `address`.
    ///
    /// # Errors
    /// [`MemoryError`] — see the enum.  A successful read of all-zero bytes is
    /// `Ok(0)`, which the caller must treat as "an entry that is not present"
    /// only after consulting the `P` bit, never as "I could not read".
    fn read_u64(&self, address: u64) -> Result<u64, MemoryError>;

    /// Read `len` bytes at `address`.
    ///
    /// # Errors
    /// [`MemoryError`] as above.
    fn read_bytes(&self, address: u64, len: usize) -> Result<Vec<u8>, MemoryError>;

    /// Total guest RAM size in bytes.
    ///
    /// Used as the `is_ram` precondition research D §3.4 requires.  Returning
    /// `None` means "this source does not know its extent"; the walker then
    /// relies on [`GuestMemory::read_u64`] to report out-of-range, which is a
    /// weaker but still sound check.
    fn ram_size(&self) -> Option<u64> {
        None
    }

    /// Is `address` inside guest RAM?
    ///
    /// The default implementation uses [`GuestMemory::ram_size`]; a source with
    /// a sparse physical map (holes in the middle) should override it.
    fn is_ram(&self, address: u64) -> bool {
        self.ram_size().is_some_and(|size| address < size)
    }
}

/// A `GuestMemory` backed by a literal sparse byte map.
///
/// This is the test and replay source: hand it the exact bytes a recorded
/// capture contained and the walk becomes hermetic — no QEMU, no image file,
/// fully deterministic.  It is also what P5-5's acceptance assertion runs on,
/// because the values it is checked against are the ones the *guest kernel
/// itself* printed on the serial line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestRam {
    /// Address → byte.  Sparse: absent means "inside RAM, reads as zero".
    bytes: BTreeMap<u64, u8>,
    /// Declared RAM size.  Addresses at or above it are `NotInRam`.
    size: u64,
}

impl GuestRam {
    /// An empty RAM of `size` bytes that reads as all zeros.
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            bytes: BTreeMap::new(),
            size,
        }
    }

    /// Write a byte at `address`.
    ///
    /// # Errors
    /// [`MemoryError::NotInRam`] when the address is outside the declared size,
    /// so a test cannot accidentally set up state the real machine could not
    /// have.
    pub fn write_byte(&mut self, address: u64, value: u8) -> Result<(), MemoryError> {
        if address >= self.size {
            return Err(MemoryError::NotInRam { address });
        }
        self.bytes.insert(address, value);
        Ok(())
    }

    /// Write a little-endian `u64`.
    ///
    /// # Errors
    /// [`MemoryError::NotInRam`] when any byte falls outside the declared size.
    pub fn write_u64(&mut self, address: u64, value: u64) -> Result<(), MemoryError> {
        for index in 0..8u64 {
            self.write_byte(address + index, (value >> (index * 8)) as u8)?;
        }
        Ok(())
    }

    /// Write a slice.
    ///
    /// # Errors
    /// [`MemoryError::NotInRam`] when any byte falls outside the declared size.
    pub fn write_bytes(&mut self, address: u64, data: &[u8]) -> Result<(), MemoryError> {
        for (index, byte) in data.iter().enumerate() {
            self.write_byte(address + index as u64, *byte)?;
        }
        Ok(())
    }
}

impl GuestMemory for GuestRam {
    fn read_u64(&self, address: u64) -> Result<u64, MemoryError> {
        let bytes = self.read_bytes(address, 8)?;
        let mut value = 0u64;
        for (index, byte) in bytes.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        Ok(value)
    }

    fn read_bytes(&self, address: u64, len: usize) -> Result<Vec<u8>, MemoryError> {
        let end = address.saturating_add(len as u64);
        if end > self.size {
            // Distinguish "entirely outside" from "starts inside but runs off
            // the end": the walker reports the first as TableNotInRam and the
            // second as an entry straddling the RAM boundary.
            if address >= self.size {
                return Err(MemoryError::NotInRam { address });
            }
            let available = usize::try_from(self.size - address).unwrap_or(usize::MAX);
            return Err(MemoryError::Short {
                address,
                wanted: len,
                got: available,
            });
        }
        let mut out = Vec::with_capacity(len);
        for offset in 0..len as u64 {
            // Absent entries read as zero: RAM that was never written is zero,
            // not unreadable.
            out.push(*self.bytes.get(&(address + offset)).unwrap_or(&0));
        }
        Ok(out)
    }

    fn ram_size(&self) -> Option<u64> {
        Some(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritten_ram_reads_as_zero() {
        let ram = GuestRam::new(0x1000);
        assert_eq!(ram.read_u64(0x800).unwrap(), 0);
        assert_eq!(ram.read_bytes(0, 16).unwrap(), vec![0u8; 16]);
    }

    #[test]
    fn written_values_round_trip_little_endian() {
        let mut ram = GuestRam::new(0x1000);
        ram.write_u64(0x1000 - 8, 0x0123_4567_89ab_cdef).unwrap();
        assert_eq!(
            ram.read_bytes(0x1000 - 8, 8).unwrap(),
            vec![0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(ram.read_u64(0x1000 - 8).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn a_fully_outside_address_is_not_in_ram_not_a_zero() {
        let ram = GuestRam::new(0x1000);
        let err = ram.read_u64(0x4000_0000).unwrap_err();
        assert_eq!(
            err,
            MemoryError::NotInRam {
                address: 0x4000_0000
            }
        );
        // The message must *say* it is outside RAM rather than reading like a
        // number, since the whole point is that the caller must not mistake this
        // for an entry value of zero.
        let text = format!("{err}");
        assert!(text.contains("outside guest RAM"), "got {text:?}");
        assert!(text.contains("0x40000000"), "got {text:?}");
    }

    #[test]
    fn a_read_straddling_the_ram_end_is_a_short_not_a_zero() {
        let ram = GuestRam::new(0x1000);
        let err = ram.read_u64(0x1000 - 4).unwrap_err();
        assert!(matches!(
            err,
            MemoryError::Short {
                wanted: 8,
                got: 4,
                ..
            }
        ));
    }

    #[test]
    fn writing_outside_ram_is_refused() {
        let mut ram = GuestRam::new(0x1000);
        assert!(ram.write_byte(0x1000, 1).is_err());
        assert!(ram.write_u64(0x1000 - 4, 1).is_err());
    }

    #[test]
    fn is_ram_follows_the_declared_size() {
        let ram = GuestRam::new(0x1000);
        assert!(ram.is_ram(0xfff));
        assert!(!ram.is_ram(0x1000));
    }
}
