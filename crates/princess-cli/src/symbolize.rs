//! Minimal symbolication: fault RIP → `symbol`, `file`, `line`.
//!
//! **Stopgap, deliberately small.**  The real ELF/DWARF engine is
//! `crates/princess-symbol/` (P2-B, owned by another agent); this module drives
//! GNU `addr2line`/`nm`/`readelf` so the P2 acceptance vehicle can produce real
//! `run.fault.symbolicated` values and real `symbols.indexed` facts today.  It
//! reports only what `addr2line` printed: when the tool cannot map an address it
//! returns `None` and the engine emits `run.fault` **without** a `symbolicated`
//! field rather than inventing a plausible-looking location.

use std::path::{Path, PathBuf};

use princess_core::types::SymbolicatedLocation;
use princess_core::{ErrorCode, PrincessError, Result};

use crate::env::Toolchain;

/// addr2line/nm/readelf driver bound to one ELF file.
#[derive(Debug, Clone)]
pub struct Symbolizer {
    elf: PathBuf,
    addr2line: PathBuf,
    nm: Option<PathBuf>,
    readelf: Option<PathBuf>,
}

impl Symbolizer {
    /// Resolve the tools and check the ELF exists.
    pub fn new(elf: &Path, toolchain: &Toolchain) -> Result<Self> {
        if !elf.is_file() {
            return Err(PrincessError::new(
                ErrorCode::NotFound,
                format!("no ELF with debug info at {}", elf.display()),
            ));
        }
        let addr2line = toolchain.which("addr2line").ok_or_else(|| {
            PrincessError::new(
                ErrorCode::ToolchainMissing,
                "addr2line is required to symbolicate a fault RIP but is not on PATH \
                 (run `bash scripts/doctor.sh` to see what is missing)",
            )
        })?;
        Ok(Self {
            elf: elf.to_path_buf(),
            addr2line,
            nm: toolchain.which("nm"),
            readelf: toolchain.which("readelf"),
        })
    }

    /// `symbol`, `file`, `line` for an address, or `None` when debug info does
    /// not cover it.
    pub fn lookup(&self, address: u64) -> Result<Option<SymbolicatedLocation>> {
        let output = self
            .run(&self.addr2line, &["-f", "-C", "-e", &self.elf.to_string_lossy(), &hex(address)])?;
        Ok(parse_addr2line(&output, &self.elf))
    }

    /// The raw `addr2line` output, for the `symbolicate` subcommand's evidence.
    pub fn raw_lookup(&self, address: u64) -> Result<String> {
        self.run(
            &self.addr2line,
            &["-f", "-C", "-e", &self.elf.to_string_lossy(), &hex(address)],
        )
    }

    /// Number of symbols in the image, or `None` when `nm` cannot read it.
    pub fn symbol_count(&self) -> Option<u64> {
        let nm = self.nm.as_ref()?;
        let output = self.run(nm, &[&self.elf.to_string_lossy()]).ok()?;
        let count = output
            .lines()
            .filter(|line| {
                line.split_whitespace()
                    .next()
                    .map(|token| !token.is_empty() && token.chars().all(|c| c.is_ascii_hexdigit()))
                    .unwrap_or(false)
            })
            .count();
        Some(count as u64)
    }

    /// GNU build-id, or `None` for images linked with `--build-id=none`
    /// (the reference kernel is one of those — `symbols.indexed.buildId` is then
    /// `null` instead of a fabricated value).
    pub fn build_id(&self) -> Option<String> {
        let readelf = self.readelf.as_ref()?;
        let output = self.run(readelf, &["-n", &self.elf.to_string_lossy()]).ok()?;
        output.lines().find_map(|line| {
            let line = line.trim();
            line.strip_prefix("Build ID:").map(|id| id.trim().to_string())
        })
    }

    fn run(&self, program: &Path, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|err| {
                PrincessError::new(
                    ErrorCode::ToolchainMissing,
                    format!("cannot run {}: {err}", program.display()),
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(PrincessError::new(
                ErrorCode::Internal,
                format!(
                    "{} exited with {}",
                    program.display(),
                    output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
                ),
            )
            .with_detail(stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// `0x...` hex text for an address (addr2line's input format).
pub fn hex(address: u64) -> String {
    format!("0x{address:016x}")
}

/// Parse `addr2line -f -C` output: two lines, function then `file:line`.
fn parse_addr2line(output: &str, elf: &Path) -> Option<SymbolicatedLocation> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let symbol = lines.next()?.trim().to_string();
    let location = lines.next()?.trim();
    if symbol == "??" || symbol.is_empty() {
        return None;
    }
    let (file, line) = location.rsplit_once(':')?;
    let line_number: u32 = line.trim().parse().ok()?;
    if file.trim() == "??" || line_number == 0 {
        return None;
    }
    Some(SymbolicatedLocation {
        symbol,
        file: absolutise(file.trim(), elf),
        line: line_number,
    })
}

/// addr2line prints the path recorded in DWARF; when that is relative (no
/// `DW_AT_comp_dir`) resolve it next to the image so the UI always gets an
/// absolute path it can open.
fn absolutise(file: &str, elf: &Path) -> String {
    let path = Path::new(file);
    if path.is_absolute() {
        return file.to_string();
    }
    let base = elf.parent().unwrap_or_else(|| Path::new("."));
    princess_core::config::normalize(&base.join(path))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr2line_output_is_parsed() {
        let location = parse_addr2line(
            "refkernel_fault_probe\n/root/PrincessIDE/fixtures/refkernel/kernel.c:100\n",
            Path::new("/root/PrincessIDE/fixtures/refkernel/build/refkernel.elf"),
        )
        .unwrap();
        assert_eq!(location.symbol, "refkernel_fault_probe");
        assert_eq!(location.file, "/root/PrincessIDE/fixtures/refkernel/kernel.c");
        assert_eq!(location.line, 100);
    }

    #[test]
    fn relative_paths_are_resolved_next_to_the_image() {
        let location = parse_addr2line(
            "kmain\nkernel.c:42\n",
            Path::new("/p/build/k.elf"),
        )
        .unwrap();
        assert_eq!(location.file, "/p/build/kernel.c");
    }

    #[test]
    fn unmappable_addresses_yield_nothing() {
        for output in ["??\n??:0\n", "??\n??:?\n", "", "sym\n\n"] {
            assert!(
                parse_addr2line(output, Path::new("/p/k.elf")).is_none(),
                "unexpected mapping for {output:?}"
            );
        }
    }

    #[test]
    fn hex_format_is_what_the_tools_expect() {
        assert_eq!(hex(0x10_0b3d), "0x0000000000100b3d");
        assert_eq!(hex(0), "0x0000000000000000");
    }

    #[test]
    fn the_reference_kernel_symbolicates_end_to_end() {
        // Uses the real fixture when it has been built; the P0 fixture is
        // committed without build products, so skip when it is absent.
        let elf = Path::new("/root/PrincessIDE/fixtures/refkernel/build/refkernel.elf");
        if !elf.is_file() {
            return;
        }
        let toolchain = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let symbolizer = Symbolizer::new(elf, &toolchain).unwrap();
        let location = symbolizer.lookup(0x10_0b3d).unwrap().unwrap();
        assert_eq!(location.symbol, "refkernel_fault_probe");
        assert_eq!(location.line, 100);
        assert!(location.file.ends_with("fixtures/refkernel/kernel.c"), "{location:?}");
        assert!(symbolizer.symbol_count().unwrap() > 10);
        assert!(symbolizer.build_id().is_none(), "fixture is linked with --build-id=none");
        // A bogus address must not produce a location.
        assert!(symbolizer.lookup(0xdead_beef).unwrap().is_none());
    }
}
