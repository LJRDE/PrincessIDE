//! Random-access hex reading over arbitrarily large images, with a hard memory
//! bound — the P5-3 deliverable.
//!
//! # The measured reason this module exists
//!
//! Research D §1.5 and §2 ran the experiment on an 8 GiB image: 20 000 random
//! 4 KiB reads.
//!
//! | strategy | per access | **RSS delta** |
//! |---|---|---|
//! | whole-file `mmap`, random access | 83.6 µs | **+1.12 GB** |
//! | `pread`, 1 byte at a time | 2.1 µs | 0 |
//! | **`pread` + LRU(256 × 4 KiB) — this module** | **4.0 µs** | **+104 kB** |
//!
//! The `mmap` blow-up is not the mapping (that is O(1)) but the kernel's
//! fault-around readahead: 16 pages = 64 KiB *per random fault*, so 20 000 random
//! touches pull ~1.28 GB of page cache into the process's RSS.  On this 3.8 GiB
//! host that is an instant problem (D21/D22), which is why `pread` plus a bounded
//! window cache is the mandated strategy and why there is **no `memmap2`**
//! dependency in this crate at all — not even as a fallback.
//!
//! # The two other mmap hazards this design sidesteps
//!
//! 1. **SIGBUS.**  A file truncated underneath an `mmap` kills the process with
//!    an uncatchable signal.  The IDE opens QEMU disk images, which QEMU is
//!    actively growing.  `pread` on a shrunk file is simply a short read, which
//!    [`HexReader::read_range`] returns as such.
//! 2. **Windows delete semantics.**  A mapped file cannot be deleted or
//!    truncated the way it can on Unix.  Tauri targets Windows too; `pread`
//!    behaves the same everywhere.
//!
//! # Bounded, and provably so
//!
//! The cache holds at most `window_size × capacity` bytes and nothing else.  At
//! the defaults that is `4096 × 256 = 1 MiB`, whatever the image size — see the
//! `memory_bound_is_independent_of_image_size` test, which drives a 2 GiB image
//! and asserts the byte count directly.

use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use princess_core::{PrincessError, Result};

/// Default window size: 4 KiB, one disk page (research D §2.3).
pub const DEFAULT_WINDOW_SIZE: usize = 4096;

/// Default window capacity: 256 windows = 1 MiB.
///
/// Research D §2.3 measured this as sufficient for "sequential scroll plus a
/// little look-back" while keeping the RSS delta at ~104 kB.
pub const DEFAULT_WINDOW_CAPACITY: usize = 256;

/// Tunables for a [`HexReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexWindowOptions {
    /// Bytes per cached window.  Clamped to at least 1.
    pub window_size: usize,
    /// Maximum number of windows held at once.  Clamped to at least 1.
    pub capacity: usize,
}

impl Default for HexWindowOptions {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_WINDOW_SIZE,
            capacity: DEFAULT_WINDOW_CAPACITY,
        }
    }
}

impl HexWindowOptions {
    /// The hard memory bound this configuration implies, in bytes.
    #[must_use]
    pub fn byte_bound(&self) -> usize {
        self.window_size.max(1) * self.capacity.max(1)
    }
}

/// Observable statistics, so a stress test can *prove* the bound was respected
/// instead of asserting it from the design document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageStats {
    /// `pread` calls issued (cache misses; one per window load).
    pub syscalls: u64,
    /// Window lookups served from the cache.
    pub hits: u64,
    /// Window lookups that had to load.
    pub misses: u64,
    /// Windows currently resident.
    pub resident_windows: usize,
    /// Bytes currently resident: `resident_windows * window_size`.
    pub resident_bytes: usize,
    /// Total bytes handed to callers across all `read_range` calls.
    pub bytes_served: u64,
    /// Total bytes actually read from the file (≤ `bytes_served` thanks to the
    /// cache, and less again when a read ran off the end of the file).
    pub bytes_read: u64,
    /// Short reads caused by reading past a growing/shrinking image's end.
    pub short_reads: u64,
}

/// One rendered hex row: 16 bytes plus the ASCII gutter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexLine {
    /// File offset of the first byte in `bytes`.
    pub offset: u64,
    /// The bytes themselves (fewer than 16 only on the final short row).
    pub bytes: Vec<u8>,
    /// Canonical `offset: bb bb bb ...` text (`..` for absent bytes).
    pub hex: String,
    /// ASCII gutter: printable bytes verbatim, everything else `.`.
    pub ascii: String,
    /// True when this row is short because the image ended, not because a read
    /// failed.  The UI renders the two differently: one is "end of file", the
    /// other is "could not read".
    pub at_eof: bool,
}

/// A windowed, read-only, random-access view of one file.
///
/// Not `Sync`: the cache is a plain `VecDeque` behind `&mut self`.  Callers that
/// need concurrent access own one reader per thread or wrap it themselves; the
/// alternative (a mutex around an LRU on a hot path) buys nothing here because
/// hex scrolling is a single-consumer workload.
#[derive(Debug)]
pub struct HexReader {
    path: PathBuf,
    file: File,
    len: u64,
    options: HexWindowOptions,
    /// Most-recently-used first.
    cache: VecDeque<(u64, Box<[u8]>)>,
    stats: ImageStats,
}

impl HexReader {
    /// Open `path` read-only.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when the file cannot be opened or stat-ed.
    pub fn open(path: &Path) -> Result<Self> {
        Self::with_options(path, HexWindowOptions::default())
    }

    /// Open `path` with explicit cache geometry.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when the file cannot be opened or stat-ed.
    pub fn with_options(path: &Path, options: HexWindowOptions) -> Result<Self> {
        let file = File::open(path).map_err(|err| {
            PrincessError::not_found(format!("cannot open {}: {err}", path.display()))
                .with_detail(err.to_string())
        })?;
        let len = file
            .metadata()
            .map_err(|err| {
                PrincessError::not_found(format!("cannot stat {}: {err}", path.display()))
                    .with_detail(err.to_string())
            })?
            .len();
        Ok(Self {
            path: path.to_path_buf(),
            file,
            len,
            options: HexWindowOptions {
                window_size: options.window_size.max(1),
                capacity: options.capacity.max(1),
            },
            cache: VecDeque::new(),
            stats: ImageStats::default(),
        })
    }

    /// The artifact path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The size recorded when the reader was opened.
    ///
    /// **This is a snapshot.**  A live QEMU image grows; [`Self::refresh_len`]
    /// re-stats it.  Callers that care about a moving target must refresh before
    /// relying on this, and the reader never silently trusts a stale size for
    /// bounds checking — it uses the size only to short-circuit obviously
    /// out-of-range requests, and a genuine short read is still handled.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Is the image zero bytes?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Re-stat the file and update the recorded size.
    ///
    /// Existing cache entries are kept: a window that was valid is still valid
    /// unless the file shrank past it, and the next read of a now-out-of-range
    /// window simply comes back short.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when the file has disappeared.
    pub fn refresh_len(&mut self) -> Result<u64> {
        let len = self
            .file
            .metadata()
            .map_err(|err| {
                PrincessError::not_found(format!("cannot restat {}: {err}", self.path.display()))
                    .with_detail(err.to_string())
            })?
            .len();
        self.len = len;
        Ok(len)
    }

    /// Current cache statistics.
    #[must_use]
    pub fn stats(&self) -> ImageStats {
        ImageStats {
            resident_windows: self.cache.len(),
            resident_bytes: self.cache.len() * self.options.window_size,
            ..self.stats
        }
    }

    /// The configured memory bound in bytes.
    #[must_use]
    pub fn byte_bound(&self) -> usize {
        self.options.byte_bound()
    }

    /// Drop every cached window, returning the resident bytes to the allocator.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache.shrink_to_fit();
    }

    /// Read `len` bytes starting at `offset`.
    ///
    /// **Out-of-range is a short read, not an error** (research D §2.3): the
    /// image may be being grown or truncated under us, and "there were fewer
    /// bytes than you asked for" is a normal state the UI renders as `??`.  Only
    /// a genuine I/O failure is an error.
    ///
    /// # Errors
    /// `E_INTERNAL` when `pread` itself fails for a reason other than a short
    /// read (e.g. the descriptor became invalid).
    pub fn read_range(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut position = offset;
        let end = offset.saturating_add(len as u64);
        // Copy the geometry out of `self` up front: `window()` borrows `self`
        // mutably and returns a slice tied to that borrow.
        let window_size = self.options.window_size as u64;

        while position < end {
            let window_index = position / window_size;
            let window = self.window(window_index)?;
            let within = (position - window_index * window_size) as usize;
            let available = window.len().saturating_sub(within);
            if available == 0 {
                // The file ended exactly at this boundary.
                self.stats.short_reads += 1;
                break;
            }
            let take = available.min((end - position) as usize);
            out.extend_from_slice(&window[within..within + take]);
            position += take as u64;
        }

        self.stats.bytes_served += out.len() as u64;
        if (out.len() as u64) < len as u64 {
            self.stats.short_reads += 1;
        }
        Ok(out)
    }

    /// Read exactly `len` bytes or fail.
    ///
    /// The strict counterpart to [`Self::read_range`], for callers parsing a
    /// structure where a short read means the image is not what it claimed to be
    /// (a page-table entry, an ELF header).  A partial answer there would be a
    /// fabricated fact, so this variant refuses to give one.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when fewer than `len` bytes were available at `offset`,
    /// naming both what was wanted and what was there.
    pub fn read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let bytes = self.read_range(offset, len)?;
        if bytes.len() != len {
            return Err(PrincessError::not_found(format!(
                "{}: wanted {len} bytes at offset {offset:#x}, only {} were available",
                self.path.display(),
                bytes.len()
            ))
            .with_detail(format!(
                "the image is {} bytes long; this is a short read, not corrupt data",
                self.len
            )));
        }
        Ok(bytes)
    }

    /// Render `row_count` hex rows of 16 bytes starting at `first_row`.
    ///
    /// This is the UI-shaped query: the frontend virtualises rows, so it asks
    /// for a row window and gets display-ready strings, never bytes to format
    /// itself.  Row `n` covers bytes `[n*16, n*16+16)`.
    ///
    /// # Errors
    /// `E_INTERNAL` on a genuine I/O failure.
    pub fn read_lines(&mut self, first_row: u64, row_count: u64) -> Result<Vec<HexLine>> {
        const BYTES_PER_ROW: u64 = 16;
        let mut lines = Vec::with_capacity(usize::try_from(row_count).unwrap_or(0));
        for index in 0..row_count {
            let row = first_row.saturating_add(index);
            let offset = row.saturating_mul(BYTES_PER_ROW);
            if offset >= self.len {
                break;
            }
            let wanted = usize::try_from((self.len - offset).min(BYTES_PER_ROW)).unwrap_or(0);
            let bytes = self.read_range(offset, wanted)?;
            let at_eof = bytes.len() < BYTES_PER_ROW as usize;
            lines.push(render_line(offset, bytes, at_eof));
        }
        Ok(lines)
    }

    /// The window containing byte `window_index * window_size`, from cache or
    /// from a single `pread`.
    fn window(&mut self, window_index: u64) -> Result<&[u8]> {
        if let Some(position) = self
            .cache
            .iter()
            .position(|(index, _)| *index == window_index)
        {
            // Move to the front (most recently used).
            if let Some(entry) = self.cache.remove(position) {
                self.cache.push_front(entry);
                self.stats.hits += 1;
            }
            return Ok(&self.cache.front().expect("just pushed").1);
        }

        self.stats.misses += 1;
        let offset = window_index.saturating_mul(self.options.window_size as u64);
        let wanted = self.options.window_size;
        let mut buffer = vec![0u8; wanted];
        // `pread`: no file-position state, so a concurrent writer moving the
        // file does not corrupt this read, and no mmap is involved at all.
        let read = self
            .file
            .read_at(&mut buffer, offset)
            .map_err(|err| {
                PrincessError::internal(format!(
                    "pread({offset:#x}, {wanted}) on {} failed: {err}",
                    self.path.display()
                ))
                .with_detail(err.to_string())
            })?;
        buffer.truncate(read);
        self.stats.syscalls += 1;
        self.stats.bytes_read += read as u64;

        self.cache.push_front((window_index, buffer.into_boxed_slice()));
        while self.cache.len() > self.options.capacity {
            self.cache.pop_back();
        }
        Ok(&self.cache.front().expect("just pushed").1)
    }
}

fn render_line(offset: u64, bytes: Vec<u8>, at_eof: bool) -> HexLine {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(16 * 3 + 8);
    let mut ascii = String::with_capacity(16);
    for index in 0..16 {
        match bytes.get(index) {
            Some(byte) => {
                let _ = write!(hex, "{byte:02x} ");
                ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                });
                let _ = &ascii;
            }
            None => {
                // `??` for a byte that is not in the image, matching the
                // research report's §2.3 "out of range shows ?? not an error".
                hex.push_str("?? ");
                ascii.push(' ');
            }
        }
    }
    HexLine {
        offset,
        bytes,
        hex: hex.trim_end().to_string(),
        ascii,
        at_eof,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempFile {
        path: PathBuf,
        _guard: (),
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_image(name: &str, bytes: &[u8]) -> TempFile {
        let dir = std::env::temp_dir().join("princess-bin-hex");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        TempFile {
            path,
            _guard: (),
        }
    }

    #[test]
    fn reads_within_one_window() {
        let data: Vec<u8> = (0..=255u8).collect();
        let image = temp_image("within.bin", &data);
        let mut reader = HexReader::open(&image.path).unwrap();
        assert_eq!(reader.len(), 256);
        assert_eq!(reader.read_range(10, 20).unwrap(), data[10..30]);
        assert_eq!(reader.read_range(0, 256).unwrap(), data);
    }

    #[test]
    fn reads_across_a_window_boundary_in_one_call() {
        // 3.5 windows of distinct bytes so a boundary-crossing read is
        // unambiguous about where each byte came from.
        let data: Vec<u8> = (0..14_336u32).map(|value| (value % 251) as u8).collect();
        let image = temp_image("cross.bin", &data);
        let mut reader = HexReader::with_options(
            &image.path,
            HexWindowOptions {
                window_size: 4096,
                capacity: 4,
            },
        )
        .unwrap();

        // Straddles the 4096 boundary.
        assert_eq!(
            reader.read_range(4090, 12).unwrap(),
            &data[4090..4102],
            "a read spanning two windows must be byte-exact"
        );
        // Straddles two boundaries at once.
        assert_eq!(
            reader.read_range(4090, 8200).unwrap(),
            &data[4090..4090 + 8200]
        );
        // Exactly a window, starting at a boundary.
        assert_eq!(reader.read_range(8192, 4096).unwrap(), &data[8192..12288]);
    }

    #[test]
    fn reads_at_a_boundary_do_not_double_count_or_skip() {
        let data: Vec<u8> = (0..8192u32).map(|value| (value % 256) as u8).collect();
        let image = temp_image("boundary.bin", &data);
        let mut reader = HexReader::with_options(
            &image.path,
            HexWindowOptions {
                window_size: 4096,
                capacity: 2,
            },
        )
        .unwrap();
        // Every 1-byte read across a 3-window span must reconstruct the file.
        let mut collected = Vec::new();
        for offset in 0..data.len() as u64 {
            collected.extend_from_slice(&reader.read_range(offset, 1).unwrap());
        }
        assert_eq!(collected, data);
    }

    #[test]
    fn past_the_end_is_a_short_read_not_an_error() {
        let data = vec![0xabu8; 100];
        let image = temp_image("short.bin", &data);
        let mut reader = HexReader::open(&image.path).unwrap();
        // Entirely past the end.
        assert_eq!(reader.read_range(1000, 16).unwrap(), Vec::<u8>::new());
        // Straddling the end.
        assert_eq!(reader.read_range(90, 32).unwrap(), vec![0xabu8; 10]);
        assert!(reader.stats().short_reads > 0);
        // The strict variant refuses instead.
        let err = reader.read_exact_at(90, 32).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
    }

    #[test]
    fn an_empty_file_reads_nothing_without_error() {
        let image = temp_image("empty.bin", &[]);
        let mut reader = HexReader::open(&image.path).unwrap();
        assert!(reader.is_empty());
        assert_eq!(reader.read_range(0, 4096).unwrap(), Vec::<u8>::new());
        assert_eq!(reader.read_lines(0, 4).unwrap(), Vec::<HexLine>::new());
    }

    #[test]
    fn the_cache_is_bounded_by_capacity_not_by_file_size() {
        // 2 GiB sparse image: mmap would be a disaster here, pread+LRU is fine.
        let dir = std::env::temp_dir().join("princess-bin-hex");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big-sparse.bin");
        {
            let file = File::create(&path).unwrap();
            file.set_len(2 * 1024 * 1024 * 1024).unwrap();
        }
        let options = HexWindowOptions::default();
        let mut reader = HexReader::with_options(&path, options).unwrap();
        assert_eq!(reader.len(), 2 * 1024 * 1024 * 1024);

        // 20 000 random 4 KiB reads, exactly the research-report workload.
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..20_000 {
            // xorshift64 — deterministic, no dev-dependency needed.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let offset = state % (reader.len() - 4096);
            let bytes = reader.read_range(offset, 4096).unwrap();
            assert_eq!(bytes.len(), 4096);
        }

        let stats = reader.stats();
        assert!(
            stats.resident_bytes <= options.byte_bound(),
            "resident {} bytes must not exceed the {}-byte bound",
            stats.resident_bytes,
            options.byte_bound()
        );
        assert!(
            stats.resident_windows <= options.capacity,
            "resident {} windows must not exceed capacity {}",
            stats.resident_windows,
            options.capacity
        );
        // A sparse file of zeroes must still have produced real preads.
        assert!(stats.syscalls > 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn random_access_reconstructs_the_file_exactly() {
        // Deterministic pseudo-random content, several windows long.
        let mut data = vec![0u8; 20_000];
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for byte in &mut data {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = (state & 0xff) as u8;
        }
        let image = temp_image("random.bin", &data);
        let mut reader = HexReader::with_options(
            &image.path,
            HexWindowOptions {
                window_size: 512,
                capacity: 4, // deliberately small so eviction is exercised
            },
        )
        .unwrap();

        let mut state: u64 = 12345;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let offset = state % (data.len() as u64);
            let len = (state >> 20) as usize % 700;
            let len = len.min(data.len() - offset as usize);
            assert_eq!(
                reader.read_range(offset, len).unwrap(),
                &data[offset as usize..offset as usize + len],
                "offset {offset} len {len}"
            );
        }
        assert!(
            reader.stats().resident_bytes <= 512 * 4,
            "cache must stay within 512*4 bytes"
        );
    }

    #[test]
    fn a_window_larger_than_a_whole_small_file_is_still_safe() {
        let data = vec![1u8, 2, 3];
        let image = temp_image("tiny.bin", &data);
        let mut reader = HexReader::with_options(
            &image.path,
            HexWindowOptions {
                window_size: 1 << 20,
                capacity: 8,
            },
        )
        .unwrap();
        assert_eq!(reader.read_range(0, 3).unwrap(), data);
        // The cached window holds only what was actually read.
        assert_eq!(reader.stats().bytes_read, 3);
    }

    #[test]
    fn hex_lines_render_16_bytes_with_an_ascii_gutter() {
        let mut data = vec![0u8; 20];
        data[..3].copy_from_slice(b"AB\x00");
        let image = temp_image("lines.bin", &data);
        let mut reader = HexReader::open(&image.path).unwrap();
        let lines = reader.read_lines(0, 8).unwrap();
        assert_eq!(lines.len(), 2, "20 bytes is two rows");
        assert_eq!(lines[0].offset, 0);
        assert_eq!(lines[0].hex, "41 42 00 00 00 00 00 00 00 00 00 00 00 00 00 00");
        assert_eq!(
            lines[0].ascii, "AB..............",
            "a NUL is not printable, so it renders as `.` and adds no width"
        );
        assert_eq!(lines[0].ascii.len(), 16, "the gutter is always 16 columns");
        assert!(!lines[0].at_eof);
        // Row 1 is short: 4 bytes then `??` padding.
        assert_eq!(lines[1].offset, 16);
        assert!(lines[1].at_eof);
        assert_eq!(lines[1].bytes.len(), 4);
        assert!(lines[1].hex.ends_with("?? ?? ?? ?? ?? ?? ?? ?? ?? ?? ?? ??"));
        assert_eq!(lines[1].ascii.trim_end().len(), 4);
    }

    #[test]
    fn hex_line_offsets_follow_the_row_index() {
        let data = vec![0u8; 16 * 100];
        let image = temp_image("rowidx.bin", &data);
        let mut reader = HexReader::open(&image.path).unwrap();
        let lines = reader.read_lines(50, 3).unwrap();
        assert_eq!(
            lines.iter().map(|line| line.offset).collect::<Vec<_>>(),
            vec![800, 816, 832]
        );
    }

    #[test]
    fn clearing_the_cache_returns_the_memory() {
        let data = vec![0u8; 40_000];
        let image = temp_image("clear.bin", &data);
        let mut reader = HexReader::open(&image.path).unwrap();
        reader.read_range(0, 40_000).unwrap();
        assert!(reader.stats().resident_windows > 0);
        reader.clear_cache();
        assert_eq!(reader.stats().resident_windows, 0);
        // And reading still works afterwards.
        assert_eq!(reader.read_range(0, 16).unwrap().len(), 16);
    }

    #[test]
    fn opening_a_missing_file_is_not_found() {
        let err = HexReader::open(Path::new("/nonexistent/nope.img")).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::NotFound);
    }
}
