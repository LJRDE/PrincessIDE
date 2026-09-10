//! NDJSON event output: stdout plus an optional `--record` tee.
//!
//! Events go to stdout, one JSON object per line, flushed per event so a UI (or
//! `tail -f`) sees them live.  Human-readable progress goes to stderr, never to
//! stdout — mixing the two would make the stream unparseable.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use princess_core::event::EventRecorder;
use princess_core::{PrincessError, Result};

/// A `Write` sink that fans one NDJSON stream out to stdout and, when
/// requested, to a recording file (the P3 replay fixture is produced this way).
pub struct EventOutput {
    record: Option<(PathBuf, File)>,
}

impl EventOutput {
    /// Open the sink.  `record` is created/truncated eagerly, so a bad path
    /// fails before any work starts.
    pub fn new(record: Option<&Path>) -> Result<Self> {
        let record = match record {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(|err| {
                            PrincessError::internal(format!(
                                "cannot create {}: {err}",
                                parent.display()
                            ))
                        })?;
                    }
                }
                let file = File::create(path).map_err(|err| {
                    PrincessError::internal(format!("cannot create {}: {err}", path.display()))
                })?;
                Some((path.to_path_buf(), file))
            }
            None => None,
        };
        Ok(Self { record })
    }

    /// Path being recorded, when `--record` was used.
    pub fn record_path(&self) -> Option<&Path> {
        self.record.as_ref().map(|(path, _)| path.as_path())
    }

    /// Wrap this sink in an [`EventRecorder`].
    pub fn into_recorder(self) -> EventRecorder<Self> {
        EventRecorder::new(self)
    }
}

impl Write for EventOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(buf)?;
            lock.flush()?;
        }
        if let Some((_, file)) = self.record.as_mut() {
            file.write_all(buf)?;
            file.flush()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()?;
        if let Some((_, file)) = self.record.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

/// Print a human-readable line to stderr unless `--quiet` was given.
pub fn human(quiet: bool, text: impl AsRef<str>) {
    if !quiet {
        eprintln!("{}", text.as_ref());
    }
}
