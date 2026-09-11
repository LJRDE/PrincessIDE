//! Address ↔ source-line mapping, and the *interval* form the side-by-side
//! disassembly view actually needs.
//!
//! # Why this module is not just `addr2line::Context`
//!
//! Research D §1.2 and item 15 of §6 are blunt about the failure mode: on `-O2`
//! code `addr2line` attributes an address to the **inlined callee's** line, not
//! the call site, and `-i` does not always recover the chain.  D20 responds by
//! making the UI requirement explicit — the disassembly view must show
//! "instruction + source line" **side by side**.  A lone line number is a
//! misleading answer, not a partial one.
//!
//! Building that view from point queries is not possible: the UI would have to
//! probe every instruction individually, and it would still not know where a
//! line *begins*, so it could not draw the grouping it must draw.  Therefore
//! this module exposes the underlying relation directly:
//!
//! * [`LineTable::line_range_for_address`] — the half-open address interval
//!   `[start, end)` that shares the line of the address you asked about.  This is
//!   exactly "the address range **and** the line range" P5-2 asks for.
//! * [`LineTable::segments`] — the whole mapping as an ordered, coalesced list,
//!   which is what makes grouping O(n) instead of O(n·queries).
//!
//! # Memory discipline (D22)
//!
//! `addr2line::Context::new` is **eager**: it parses every CU's line program and
//! builds in-memory indices at construction time, costing hundreds of MB and
//! many seconds on a large `vmlinux` (research D §1.2, §6 item 14).  So:
//!
//! * the context is built **lazily**, on the first line query;
//! * it is built **once per `LineTable`** and cached for the table's lifetime;
//! * [`LineTable::segments`] is likewise computed once and memoised, so a UI can
//!   call it per repaint without re-walking DWARF.
//!
//! There is deliberately no crate-global cache: an unbounded cache of
//! `addr2line::Context`s is precisely the memory blow-up D22 forbids.  Callers
//! that need more than one artifact at a time own their own LRU.

use std::path::Path;
use std::sync::Arc;

use addr2line::Context;
use gimli::{EndianArcSlice, RunTimeEndian};
use object::{Object, ObjectSection};
use princess_core::{PrincessError, Result, SourceLocation};

use crate::demangle_name;

/// Half-open address interval `[start, end)` belonging to one source line.
///
/// `start == end` never occurs in a well-formed table; rows that would be empty
/// are dropped when segments are built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRange {
    /// First address (inclusive) attributed to the line.
    pub start: u64,
    /// One past the last address attributed to the line.
    pub end: u64,
    /// Absolute source path as DWARF records it.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column, when the line program records one.
    pub column: Option<u32>,
    /// Innermost inlined function covering `[start, end)`, when it differs from
    /// the enclosing function.  `None` means "not inlined / unknown".
    pub inlined_into: Option<String>,
}

impl LineRange {
    /// Does this range contain `address`?
    #[must_use]
    pub fn contains(&self, address: u64) -> bool {
        address >= self.start && address < self.end
    }

    /// Number of bytes covered.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Always false for a well-formed range; present because clippy asks for it
    /// whenever `len()` exists, and `true` is genuinely possible for a
    /// degenerate `start == end` row.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// The point-query projection used by
    /// [`BinProvider::source_line_for_address`](princess_core::BinProvider::source_line_for_address).
    #[must_use]
    pub fn to_location(&self) -> SourceLocation {
        SourceLocation {
            file: self.file.clone(),
            line: self.line,
            column: self.column,
        }
    }
}

/// An artifact's address → line mapping.
///
/// Construct with [`LineTable::open`]; the DWARF load is deferred until the first
/// query so that merely opening a binary is cheap.
///
/// # Why the context is behind a `Mutex`
///
/// `princess_core::BinProvider` is declared `Send + Sync` (the engine holds it as
/// a shared trait object), while `addr2line::Context` is neither.  A `Mutex` is
/// what makes the two compatible without weakening either: the lock is held only
/// for the duration of one lookup, and the heavy, slow part — building the
/// context — happens once, under the same lock, with the state published
/// afterwards.
///
/// The alternative the sibling crate took (`princess-symbol` loads DWARF eagerly
/// into a plain index and drops the `Context`) is not available here: this crate
/// promises *lazy* loading and the interval table the disassembly view needs,
/// and `Context` is the only thing that provides both cheaply.
pub struct LineTable {
    path: std::path::PathBuf,
    context: std::sync::Mutex<Option<Context<EndianArcSlice<RunTimeEndian>>>>,
    segments: std::sync::Mutex<Option<Vec<LineRange>>>,
}

impl std::fmt::Debug for LineTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineTable")
            .field("path", &self.path)
            .field("context_loaded", &self.is_loaded())
            .field("segments", &self.segments.lock().map(|guard| guard.as_ref().map(std::vec::Vec::len)).unwrap_or(None))
            .finish()
    }
}

impl LineTable {
    /// Prepare a line table for `path` **without** loading DWARF yet.
    ///
    /// The file is only stat-ed here; a missing file still fails immediately so
    /// that a typo is reported at `open` time rather than at first query.
    ///
    /// # Errors
    /// `E_NOT_FOUND` when the artifact does not exist.
    pub fn open(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path).map_err(|err| {
            PrincessError::not_found(format!("cannot stat {}: {err}", path.display()))
                .with_detail(err.to_string())
        })?;
        if !meta.is_file() {
            return Err(PrincessError::not_found(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            context: std::sync::Mutex::new(None),
            segments: std::sync::Mutex::new(None),
        })
    }

    /// The artifact this table describes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Has the (expensive) DWARF load happened yet?
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.context
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// Force the DWARF load.  Useful for a background warm-up before the UI
    /// asks its first question.
    ///
    /// # Errors
    /// `E_INTERNAL` when the file cannot be read or its DWARF cannot be loaded;
    /// the message distinguishes the two, because "no debug info" and "corrupt
    /// debug info" need different UI treatment.
    pub fn load(&self) -> Result<()> {
        {
            let guard = self.lock_context()?;
            if guard.is_some() {
                return Ok(());
            }
        }
        let bytes = crate::elf::read_bounded(&self.path)?;
        let object = object::File::parse(bytes.as_slice()).map_err(|err| {
            PrincessError::internal(format!(
                "{} is not a parseable object file: {err}",
                self.path.display()
            ))
            .with_detail(err.to_string())
        })?;
        let dwarf = load_dwarf(&object, &self.path)?;
        let context = Context::from_arc_dwarf(Arc::new(dwarf)).map_err(|err| {
            PrincessError::internal(format!(
                "cannot load DWARF line info from {}: {err}",
                self.path.display()
            ))
            .with_detail(err.to_string())
        })?;
        *self.lock_context()? = Some(context);
        Ok(())
    }

    /// Lock the context cell, mapping a poisoned mutex to a typed error.
    ///
    /// A poisoned lock means another thread panicked mid-lookup.  Returning an
    /// error is right: the alternative (`unwrap`) would take the engine down a
    /// second time for the same reason.
    fn lock_context(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<Context<EndianArcSlice<RunTimeEndian>>>>> {
        self.context.lock().map_err(|_| {
            PrincessError::internal(format!(
                "the DWARF line table for {} was poisoned by an earlier panic",
                self.path.display()
            ))
            .with_detail("a previous lookup panicked while holding the lock".to_string())
        })
    }

    /// Lock the segments cell.
    fn lock_segments(&self) -> Result<std::sync::MutexGuard<'_, Option<Vec<LineRange>>>> {
        self.segments.lock().map_err(|_| {
            PrincessError::internal(format!(
                "the line-range table for {} was poisoned by an earlier panic",
                self.path.display()
            ))
            .with_detail("a previous lookup panicked while holding the lock".to_string())
        })
    }

    /// Address → source line, the point form.
    ///
    /// `Ok(None)` means "DWARF loaded fine, but nothing claims this address" —
    /// e.g. a hand-written assembly stub with no line program.  That is a real
    /// answer and is *not* an error.  An error is reserved for "I could not
    /// look", never for "I looked and found nothing".
    ///
    /// # Errors
    /// `E_NOT_FOUND` / `E_INTERNAL` from the DWARF load.
    pub fn location_for_address(&self, address: u64) -> Result<Option<SourceLocation>> {
        self.load()?;
        let path = self.path.clone();
        let guard = self.lock_context()?;
        let context = guard.as_ref().expect("load() guarantees Some");
        let location = context.find_location(address).map_err(|err| {
            PrincessError::internal(format!(
                "DWARF line lookup for {address:#x} in {} failed: {err}",
                path.display()
            ))
            .with_detail(err.to_string())
        })?;
        Ok(location.and_then(|location| {
            // `find_location` happily returns a row with no file and no line
            // (the DWARF "end of sequence" row).  Reporting that as a location
            // would put a fabricated `:0` in front of the user.
            let file = location.file?;
            let line = location.line?;
            Some(SourceLocation {
                file: file.to_string(),
                line,
                column: location.column,
            })
        }))
    }

    /// Demangled function name covering `address`, if DWARF knows one.
    ///
    /// # Errors
    /// `E_NOT_FOUND` / `E_INTERNAL` from the DWARF load, or `E_INTERNAL` when
    /// the frame walk fails.
    pub fn function_for_address(&self, address: u64) -> Result<Option<String>> {
        self.load()?;
        let path = self.path.clone();
        let guard = self.lock_context()?;
        let context = guard.as_ref().expect("load() guarantees Some");
        let mut frames = context.find_frames(address).skip_all_loads().map_err(|err| {
            PrincessError::internal(format!(
                "DWARF frame lookup for {address:#x} in {} failed: {err}",
                path.display()
            ))
            .with_detail(err.to_string())
        })?;
        // The *innermost* frame is the function the address is really in; on
        // optimised code this is the inlined body, which is exactly the case
        // D20 wants the UI to be able to show.
        while let Some(frame) = frames.next().map_err(|err| {
            PrincessError::internal(format!(
                "DWARF frame iteration for {address:#x} in {} failed: {err}",
                path.display()
            ))
            .with_detail(err.to_string())
        })? {
            if let Some(function) = frame.function {
                let raw = function.raw_name().map_err(|err| {
                    PrincessError::internal(format!(
                        "DWARF function name for {address:#x} is unreadable: {err}"
                    ))
                    .with_detail(err.to_string())
                })?;
                return Ok(Some(demangle_name(&raw)));
            }
        }
        Ok(None)
    }

    /// The half-open address interval sharing `address`'s source line.
    ///
    /// This is the query the "disassembly + source" view is built on: it answers
    /// both "which line?" and "over which addresses does that line hold?", which
    /// is what lets the UI group instructions under one source row.
    ///
    /// Returns `Ok(None)` when nothing claims the address.
    ///
    /// # Errors
    /// `E_NOT_FOUND` / `E_INTERNAL` from the DWARF load.
    pub fn line_range_for_address(&self, address: u64) -> Result<Option<LineRange>> {
        let segments = self.segments()?;
        // `segments` is sorted by `start` and non-overlapping, so a binary
        // search is exact.  Linear scanning here would make a scroll through a
        // function quadratic.
        let index = match segments.binary_search_by(|segment| {
            if address < segment.start {
                std::cmp::Ordering::Greater
            } else if address >= segment.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(index) => index,
            Err(_) => return Ok(None),
        };
        Ok(Some(segments[index].clone()))
    }

    /// Every mapping row, in address order, coalesced.
    ///
    /// Computed once and memoised.  Rows are built from the DWARF line rows
    /// themselves rather than by probing addresses, so the result is
    /// **complete** — including the holes, which simply do not appear.
    ///
    /// # Errors
    /// `E_NOT_FOUND` / `E_INTERNAL` from the DWARF load, or `E_INTERNAL` when
    /// the row iterator or a file name cannot be decoded.
    pub fn segments(&self) -> Result<Vec<LineRange>> {
        {
            let guard = self.lock_segments()?;
            if let Some(segments) = guard.as_ref() {
                return Ok(segments.clone());
            }
        }
        let built = self.build_segments()?;
        *self.lock_segments()? = Some(built.clone());
        Ok(built)
    }

    fn build_segments(&self) -> Result<Vec<LineRange>> {
        // Pull the compact `(address, file, line, column)` row index out of
        // addr2line.  `find_location_range` hands back the line rows directly —
        // no frame machinery, no per-address probing — which is what makes a
        // complete table affordable.
        let raw_rows: Vec<(u64, String, u32, Option<u32>)> = {
            self.load()?;
            let path = self.path.clone();
            let guard = self.lock_context()?;
            let context = guard.as_ref().expect("load() guarantees Some");
            let iterator = context.find_location_range(0, u64::MAX).map_err(|err| {
                PrincessError::internal(format!(
                    "cannot enumerate DWARF line rows in {}: {err}",
                    path.display()
                ))
                .with_detail(err.to_string())
            })?;
            let mut rows = Vec::new();
            // The *infallible* `Iterator` impl for `LocationRangeIter` maps a
            // decoder error to `None`, i.e. it silently truncates the table —
            // which for a symbolication table means "the rest of the program has
            // no source" and is exactly the kind of quiet wrong answer this
            // crate refuses to produce.  The `fallible-iterator` impl surfaces
            // the error, so that is the one used here.
            let mut iterator = iterator;
            // Fully qualified: both `Iterator::next` and `FallibleIterator::next`
            // are in scope and only the latter can fail.
            while let Some((start, _len, location)) =
                fallible_iterator::FallibleIterator::next(&mut iterator).map_err(|err| {
                PrincessError::internal(format!(
                    "DWARF line row iterator in {} failed: {err}",
                    path.display()
                ))
                    .with_detail(err.to_string())
                })?
            {
                // A row may legitimately name no file (the end-of-sequence
                // row).  Skipping is correct: the address simply has no line,
                // which is a real answer, not a hole to fill with a guess.
                let (Some(file), Some(line)) = (location.file, location.line) else {
                    continue;
                };
                rows.push((start, file.to_string(), line, location.column));
            }
            rows
        };

        // `find_location_range` yields increasing starts within a CU, but the
        // concatenation across many CUs is not globally ordered.  Sort rather
        // than trust the iteration order.
        let mut raw_rows = raw_rows;
        raw_rows.sort_by_key(|row| row.0);

        // Turn point rows into intervals.  Each row is a *start* address; its
        // length is the distance to the next row.  Adjacent rows naming the same
        // line are coalesced so a scroll sees one entry per source line instead
        // of one per statement.
        let mut out: Vec<LineRange> = Vec::with_capacity(raw_rows.len());
        for (index, (start, file, line, column)) in raw_rows.iter().enumerate() {
            let end = raw_rows
                .get(index + 1)
                .map_or_else(|| start.saturating_add(1), |next| next.0)
                .max(start.saturating_add(1));
            match out.last_mut() {
                Some(previous)
                    if previous.end == *start
                        && previous.file == *file
                        && previous.line == *line
                        && previous.column == *column =>
                {
                    previous.end = end;
                }
                _ => out.push(LineRange {
                    start: *start,
                    end,
                    file: file.clone(),
                    line: *line,
                    column: *column,
                    inlined_into: None,
                }),
            }
        }
        Ok(out)
    }
}

/// Assemble a `gimli::Dwarf` from an `object::File`'s debug sections.
///
/// `addr2line` 0.27 has no `Context::new(&object)` convenience constructor (the
/// research report's §1.7 API table was checked against the crate source, and
/// this constructor simply is not there) — the two supported entry points are
/// `from_sections` and `from_dwarf`.  So the sections are pulled explicitly.
///
/// Using `uncompressed_data()` here is not optional: the fixtures are built
/// without compression, but a kernel linked with
/// `--compress-debug-sections=zlib` would otherwise fail to parse **silently**
/// (research D §1.1 item 3), which is the worst possible outcome for a tool whose
/// entire job is to report facts.
fn load_dwarf<'data>(
    object: &object::File<'data, &'data [u8]>,
    path: &Path,
) -> Result<gimli::Dwarf<gimli::EndianArcSlice<gimli::RunTimeEndian>>> {

    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    // One closure, one place where a section load can fail.  `gimli::Dwarf::load`
    // wants a closure over `gimli::SectionId`; `SectionId::name()` yields the
    // canonical `.debug_*` name that `object` looks up.
    let dwarf = gimli::Dwarf::load(|id: gimli::SectionId| -> std::result::Result<
        gimli::EndianArcSlice<gimli::RunTimeEndian>,
        gimli::Error,
    > {
        let name = id.name();
        let data: std::borrow::Cow<'_, [u8]> = match object.section_by_name(name) {
            // `uncompressed_data` transparently handles SHF_COMPRESSED and
            // `.zdebug_*`; skipping it would make a zlib-compressed kernel fail
            // to symbolise *silently* (research D §1.1 item 3).
            Some(section) => section
                .uncompressed_data()
                .map_err(|_| gimli::Error::NoEntryAtGivenOffset(0))?,
            None => std::borrow::Cow::Borrowed(&[]),
        };
        Ok(gimli::EndianArcSlice::new(
            std::sync::Arc::from(&data[..]),
            endian,
        ))
    })
    .map_err(|err| {
        PrincessError::internal(format!(
            "cannot load DWARF sections from {}: {err}",
            path.display()
        ))
        .with_detail(err.to_string())
    })?;
    Ok(dwarf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refkernel() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/refkernel/build/refkernel.elf")
    }

    #[test]
    fn open_defers_the_dwarf_load() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let table = LineTable::open(&path).expect("open");
        assert!(
            !table.is_loaded(),
            "opening a binary must not pay the addr2line::Context::new cost"
        );
    }

    #[test]
    fn fault_address_symbolises_to_the_contract_line() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let table = LineTable::open(&path).expect("open");
        // D3 fixed assertion: fault RIP 0x100b3d is kernel.c:100.
        let location = table
            .location_for_address(0x10_0b3d)
            .expect("lookup")
            .expect("0x100b3d must have a line");
        assert!(
            location.file.ends_with("refkernel/kernel.c"),
            "file was {}",
            location.file
        );
        assert_eq!(location.line, 100);
    }

    #[test]
    fn line_range_covers_the_line_and_stops_at_the_next_one() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let table = LineTable::open(&path).expect("open");
        let range = table
            .line_range_for_address(0x10_0b3d)
            .expect("lookup")
            .expect("range");
        assert_eq!(range.line, 100);
        assert!(range.start <= 0x10_0b3d);
        assert!(range.end > 0x10_0b3d);
        assert!(range.contains(0x10_0b3d));
        assert!(!range.contains(range.end));
        assert!(!range.contains(range.start.saturating_sub(1)));
        assert!(range.len() >= 1);
    }

    #[test]
    fn segments_are_sorted_non_overlapping_and_cover_the_range_query() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let table = LineTable::open(&path).expect("open");
        let segments = table.segments().expect("segments").to_vec();
        assert!(!segments.is_empty(), "refkernel has full DWARF");
        for pair in segments.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "segments must not overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        // Every segment must agree with the point query at its own start.
        for segment in segments.iter().take(64) {
            let point = table
                .line_range_for_address(segment.start)
                .expect("lookup")
                .expect("a segment start must resolve");
            assert_eq!(point.line, segment.line, "at {:#x}", segment.start);
            assert_eq!(point.file, segment.file);
        }
    }

    #[test]
    fn an_address_with_no_line_info_is_none_not_an_error() {
        let path = refkernel();
        if !path.is_file() {
            return;
        }
        let table = LineTable::open(&path).expect("open");
        // Way outside the image.
        assert_eq!(table.location_for_address(0xdead_beef_0000).unwrap(), None);
        assert_eq!(table.line_range_for_address(0xdead_beef_0000).unwrap(), None);
    }
}
