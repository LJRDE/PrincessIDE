//! Hex view model — address-continuous hex dump rows.
//!
//! The hex view consumes raw bytes and produces display rows in the classic
//! hex-dump format: 16 bytes per row with address, hex bytes, and ASCII gutter.
//!
//! # Invariants
//!
//! * Row addresses are strictly contiguous: `rows[i+1].addr == rows[i].addr + 16`
//!   (except the last row, which may be shorter).
//! * `bytes` in each row contains 16 bytes, except the final row which may
//!   contain fewer.
//! * `ascii` has exactly `bytes.len()` characters: printable ASCII (32..=126)
//!   rendered verbatim, everything else replaced with `.`.
//! * Address values are monotonically increasing.

use serde::Serialize;

/// One row in the hex view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HexRow {
    /// The address (file offset) of the first byte in this row.
    pub addr: u64,
    /// The bytes in this row (up to 16).
    pub bytes: Vec<u8>,
    /// ASCII representation: printable bytes verbatim, others as `.`.
    pub ascii: String,
}

/// Hex view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HexModel {
    /// Hex display rows, in address order.
    pub rows: Vec<HexRow>,
    /// Base address of the first row (0 for file-offset mode).
    pub base_addr: u64,
    /// Total size of the data represented.
    pub total_size: usize,
}

/// Builder for [`HexModel`].
///
/// # Usage
///
/// ```rust
/// use princess_view::HexBuilder;
///
/// let data: Vec<u8> = (0..48).collect();
/// let model = HexBuilder::new(&data)
///     .base_addr(0x1000)
///     .build();
/// assert_eq!(model.rows.len(), 3); // 48 bytes = 3 rows of 16
/// assert_eq!(model.rows[0].addr, 0x1000);
/// assert_eq!(model.rows[1].addr, 0x1010);
/// ```
pub struct HexBuilder<'a> {
    data: &'a [u8],
    base_addr: u64,
}

impl<'a> HexBuilder<'a> {
    /// Create a new builder for the given byte data.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            base_addr: 0,
        }
    }

    /// Set the base address for the hex view.
    pub fn base_addr(mut self, addr: u64) -> Self {
        self.base_addr = addr;
        self
    }

    /// Build the view model.
    ///
    /// For empty data, returns a model with zero rows.
    pub fn build(self) -> HexModel {
        let total_size = self.data.len();
        let rows: Vec<HexRow> = self.data
            .chunks(16)
            .enumerate()
            .map(|(i, chunk)| {
                let addr = self.base_addr + (i as u64) * 16;
                let ascii: String = chunk
                    .iter()
                    .map(|&b| if (32..=126).contains(&b) { b as char } else { '.' })
                    .collect();
                HexRow {
                    addr,
                    bytes: chunk.to_vec(),
                    ascii,
                }
            })
            .collect();

        HexModel {
            rows,
            base_addr: self.base_addr,
            total_size,
        }
    }
}

/// Errors that can occur when reading hex data from an external source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexReadError {
    /// The requested address range is out of bounds.
    OutOfRange { addr: u64, size: usize, total: u64 },
    /// The data source returned fewer bytes than expected.
    ShortRead { expected: usize, actual: usize },
    /// The data source could not be opened.
    IoError(String),
}

impl std::fmt::Display for HexReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HexReadError::OutOfRange { addr, size, total } => {
                write!(f, "address range [{:#x}, {:#x}) exceeds total size {:#x}", 
                    addr, *addr + *size as u64, total)
            }
            HexReadError::ShortRead { expected, actual } => {
                write!(f, "short read: expected {} bytes, got {}", expected, actual)
            }
            HexReadError::IoError(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

impl std::error::Error for HexReadError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_produces_empty_model() {
        let model = HexBuilder::new(&[]).build();
        assert_eq!(model.rows.len(), 0);
        assert_eq!(model.total_size, 0);
    }

    #[test]
    fn addresses_are_contiguous() {
        let data: Vec<u8> = (0..64).collect();
        let model = HexBuilder::new(&data).base_addr(0x100).build();
        for i in 0..model.rows.len() - 1 {
            assert_eq!(model.rows[i + 1].addr, model.rows[i].addr + 16);
        }
    }

    #[test]
    fn ascii_renders_printable_and_dots() {
        let data = b"Hello\x00World!";
        let model = HexBuilder::new(data).build();
        assert_eq!(model.rows[0].ascii, "Hello.World!");
    }

    #[test]
    fn last_row_may_be_shorter() {
        let data: Vec<u8> = (0..20).collect();
        let model = HexBuilder::new(&data).build();
        assert_eq!(model.rows.len(), 2);
        assert_eq!(model.rows[0].bytes.len(), 16);
        assert_eq!(model.rows[1].bytes.len(), 4);
    }

    #[test]
    fn negative_out_of_range_address() {
        // This tests the error type construction, not the builder
        // (builder takes raw bytes, no I/O)
        let err = HexReadError::OutOfRange {
            addr: 0x1000,
            size: 256,
            total: 0x100,
        };
        assert!(err.to_string().contains("exceeds total size"));
    }
}
