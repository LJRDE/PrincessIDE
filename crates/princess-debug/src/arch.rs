//! Architecture self-check — decision **D10**.
//!
//! # The failure this prevents
//!
//! Research B measured that a 32-bit kernel debugged through
//! `qemu-system-x86_64`'s gdbstub produces
//! `Remote 'g' packet reply is too long` followed by **garbage backtraces**.
//! The danger is not the error message — it is that some paths *do* return
//! something: a truncated register read still has plausible-looking bytes, and
//! a stack walk over a mismatched frame layout yields frames that look like
//! real addresses.  D10's requirement is explicit: **detect the mismatch and
//! say so; never return乱数据**.
//!
//! # What can actually be checked
//!
//! GDB reports its view of the target architecture (`show architecture`) and the
//! ELF reports its own class and machine.  Those are three independent facts,
//! and the self-check compares all of them:
//!
//! | fact | source |
//! |---|---|
//! | target arch GDB negotiated | `show architecture` — `currently "i386:x86-64"` |
//! | ELF class | `readelf -h` header parsing (or `e_machine`/`EI_CLASS`) |
//! | pointer width GDB believes | `p sizeof(void*)` |
//!
//! A 32-bit ELF on a 64-bit stub shows up as `elf32` vs `i386:x86-64`; a stub
//! that answered with a truncated `g` packet shows up as an unreadable
//! `show architecture` or a `sizeof(void*)` that contradicts the ELF class.
//!
//! The check is **fail-loud but not paranoid**: if GDB does not report an
//! architecture at all (some stubs are silent) the check reports
//! [`ArchCheck::Unknown`] with the raw text rather than inventing a verdict.

use princess_core::{ErrorCode, PrincessError, Result};

use crate::repl;

/// The ELF class, read from the file header without pulling in an ELF library.
///
/// P4 is not the phase that owns ELF parsing (`princess-symbol`/`princess-bin`
/// do, D20); all the self-check needs is `EI_CLASS` and `e_machine`, which are
/// at fixed offsets in every ELF file.  Reading two bytes directly keeps the
/// debug crate free of a binary-parsing dependency it would otherwise only use
/// for this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfClass {
    Elf32,
    Elf64,
}

impl ElfClass {
    pub const fn pointer_bits(self) -> u32 {
        match self {
            ElfClass::Elf32 => 32,
            ElfClass::Elf64 => 64,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ElfClass::Elf32 => "elf32",
            ElfClass::Elf64 => "elf64",
        }
    }
}

/// What the ELF file itself claims to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfFacts {
    pub class: ElfClass,
    /// Raw `e_machine` value.
    pub machine: u16,
    /// Human spelling for `e_machine`, when known.
    pub machine_name: &'static str,
    /// `EM_386` | `EM_X86_64` | other.
    pub is_x86: bool,
}

/// Read `EI_CLASS` and `e_machine` out of an ELF file.
///
/// Returns `E_NOT_FOUND`/`E_INTERNAL` with evidence rather than guessing when
/// the file is not an ELF at all.
pub fn read_elf_facts(path: &std::path::Path) -> Result<ElfFacts> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path).map_err(|err| {
        PrincessError::new(
            ErrorCode::NotFound,
            format!("cannot open the debug symbols file {}", path.display()),
        )
        .with_detail(err.to_string())
    })?;
    // EI_NIDENT (16) + e_type (2) + e_machine (2) is enough.
    let mut header = [0u8; 20];
    file.read_exact(&mut header).map_err(|err| {
        PrincessError::new(
            ErrorCode::InvalidConfig,
            format!("{} is too small to be an ELF file", path.display()),
        )
        .with_detail(err.to_string())
    })?;

    if &header[0..4] != b"\x7fELF" {
        return Err(PrincessError::new(
            ErrorCode::InvalidConfig,
            format!("{} is not an ELF file (bad magic)", path.display()),
        )
        .with_detail(format!("first 4 bytes: {:02x?}", &header[0..4])));
    }

    let class = match header[4] {
        1 => ElfClass::Elf32,
        2 => ElfClass::Elf64,
        other => {
            return Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!("{} has an unknown ELF class byte", path.display()),
            )
            .with_detail(format!("EI_CLASS = {other}")));
        }
    };

    // e_machine is little-endian for the x86 targets we support; the byte order
    // is EI_DATA (header[5]).  A big-endian ELF is not an x86 kernel and is
    // rejected rather than misread.
    let little_endian = match header[5] {
        1 => true,
        2 => false,
        other => {
            return Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!("{} has an unknown EI_DATA byte", path.display()),
            )
            .with_detail(format!("EI_DATA = {other}")));
        }
    };
    let machine = if little_endian {
        u16::from_le_bytes([header[18], header[19]])
    } else {
        u16::from_be_bytes([header[18], header[19]])
    };

    let (machine_name, is_x86) = match machine {
        3 => ("EM_386", true),
        62 => ("EM_X86_64", true),
        183 => ("EM_AARCH64", false),
        243 => ("EM_RISCV", false),
        _ => ("EM_UNKNOWN", false),
    };

    Ok(ElfFacts {
        class,
        machine,
        machine_name,
        is_x86,
    })
}

/// The negotiated target architecture, as GDB reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetArch {
    /// The name in GDB's own spelling, e.g. `i386:x86-64`.
    pub name: String,
    /// Pointer width in bits, from `p sizeof(void*)`, when GDB answered.
    pub pointer_bits: Option<u32>,
    /// The `show architecture` output, verbatim.
    pub raw: String,
}

impl TargetArch {
    /// Whether GDB negotiated a 64-bit x86 target.
    pub fn is_x86_64(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        // `i386:x86-64` is the canonical name; accept the common aliases too so
        // a future gdb naming change does not turn into a false mismatch.
        name.contains("x86-64") || name.contains("x86_64") || name == "i386:x64-32"
    }

    /// Whether GDB negotiated a 32-bit x86 target.
    pub fn is_i386(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        (name == "i386" || name.starts_with("i386:") && !name.contains("x86-64"))
            || name == "i8086"
    }
}

/// Verdict of the self-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchCheck {
    /// Everything agrees: the ELF and the stub are the same architecture.
    Consistent {
        arch: String,
        pointer_bits: u32,
    },
    /// The ELF and the target disagree — the engine **must not** proceed.
    Mismatch {
        expected: String,
        actual: String,
        /// Why the check fired, in one sentence.
        reason: String,
    },
    /// GDB did not report an architecture; the raw text is kept so a human can
    /// judge.  Not an error, but not a silent pass either.
    Unknown { raw: String },
}

impl ArchCheck {
    pub fn is_consistent(&self) -> bool {
        matches!(self, ArchCheck::Consistent { .. })
    }

    /// Convert a mismatch into the D10 error: explicit, with both sides quoted.
    pub fn require_consistent(self, elf_path: &std::path::Path) -> Result<ArchCheck> {
        match self {
            ArchCheck::Mismatch {
                expected,
                actual,
                reason,
            } => Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!(
                    "architecture mismatch between {} and the debug stub: {reason}",
                    elf_path.display()
                ),
            )
            .with_detail(format!(
                "elf: {expected}\ntarget: {actual}\n\
                 (D10: a mismatched architecture makes gdbstub return truncated \
                  register data and nonsense stack frames; the engine refuses to \
                  debug rather than report either.)"
            ))),
            other => Ok(other),
        }
    }

    /// One-line rendering for logs and reports.
    pub fn summary(&self) -> String {
        match self {
            ArchCheck::Consistent { arch, pointer_bits } => {
                format!("consistent: {arch} ({pointer_bits}-bit target)")
            }
            ArchCheck::Mismatch {
                expected,
                actual,
                reason,
            } => format!("MISMATCH: elf={expected} target={actual} ({reason})"),
            ArchCheck::Unknown { raw } => {
                format!("unknown: gdb did not report an architecture ({})", raw.trim())
            }
        }
    }
}

/// Parse `show architecture` output.
///
/// Measured gdb 16.3 output:
///
/// ```text
/// The target architecture is set to "auto" (currently "i386:x86-64").
/// ```
pub fn parse_show_architecture(text: &str) -> Option<String> {
    let start = text.find("currently \"")? + "currently \"".len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Parse `p sizeof(void*)` output such as `$1 = 8`.
pub fn parse_sizeof_pointer(text: &str) -> Option<u32> {
    let value = text.split('=').nth(1)?.trim();
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    let bytes: u32 = digits.parse().ok()?;
    Some(bytes * 8)
}

/// Run the full D10 self-check against a live session.
///
/// `elf_path` is the file whose symbols the session loaded; comparing it with
/// what the stub actually speaks is the whole point.
pub fn check(
    transport: &mut crate::transport::Transport,
    elf_path: &std::path::Path,
) -> Result<ArchCheck> {
    let elf = read_elf_facts(elf_path)?;

    let arch_outcome = repl::run(transport, "show architecture")?;
    // `show architecture` failing means we cannot judge — that is Unknown, not
    // a mismatch: inventing a verdict from a failed probe is exactly the
    // "看似合理的假数据" D10 and P4-4 forbid.
    if !arch_outcome.looks_ok() {
        return Ok(ArchCheck::Unknown {
            raw: arch_outcome.text,
        });
    }
    let Some(name) = parse_show_architecture(&arch_outcome.text) else {
        return Ok(ArchCheck::Unknown {
            raw: arch_outcome.text,
        });
    };

    // Pointer width is a second, independent signal.  It catches the case where
    // gdb *says* x86-64 but the stub is answering with 32-bit `g` packets.
    let pointer_bits = repl::run(transport, "p sizeof(void*)")
        .ok()
        .filter(|outcome| outcome.looks_ok())
        .and_then(|outcome| parse_sizeof_pointer(&outcome.text));

    let target = TargetArch {
        name,
        pointer_bits,
        raw: arch_outcome.text.clone(),
    };

    // Rule 1 — pointer width must agree with the ELF class, when we know both.
    if let Some(bits) = target.pointer_bits {
        if bits != elf.class.pointer_bits() {
            return Ok(ArchCheck::Mismatch {
                expected: format!("{} ({} pointer bits)", elf.class.as_str(), elf.class.pointer_bits()),
                actual: format!("{} ({} pointer bits)", target.name, bits),
                reason: format!(
                    "the stub reports a {bits}-bit pointer but the ELF is {}",
                    elf.class.as_str()
                ),
            });
        }
    }

    // Rule 2 — the named architecture must match the ELF machine.
    let agrees = match elf.class {
        ElfClass::Elf64 => target.is_x86_64(),
        ElfClass::Elf32 => target.is_i386(),
    };
    if !agrees {
        return Ok(ArchCheck::Mismatch {
            expected: format!("{} ({})", elf.class.as_str(), elf.machine_name),
            actual: format!("{} ({} pointer bits)", target.name, target.pointer_bits.map_or("unknown".to_string(), |b| b.to_string())),
            reason: format!(
                "the ELF is {} but the stub negotiated {:?}",
                elf.class.as_str(),
                target.name
            ),
        });
    }

    // Rule 3 — an x86 ELF must not be debugged by a stub that is not x86 at all.
    if !elf.is_x86 {
        return Ok(ArchCheck::Mismatch {
            expected: format!("{} ({})", elf.class.as_str(), elf.machine_name),
            actual: target.name.clone(),
            reason: format!(
                "the ELF machine {} is not the x86 target gdb negotiated ({})",
                elf.machine_name, target.name
            ),
        });
    }

    Ok(ArchCheck::Consistent {
        arch: target.name,
        pointer_bits: target.pointer_bits.unwrap_or_else(|| elf.class.pointer_bits()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGING_ELF: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/paging-kernel/build/pagingkernel.elf"
    );

    #[test]
    fn parses_gdb_16_3_show_architecture_output() {
        let text = "The target architecture is set to \"auto\" (currently \"i386:x86-64\").\n";
        assert_eq!(parse_show_architecture(text).as_deref(), Some("i386:x86-64"));
    }

    #[test]
    fn recognizes_both_x86_widths_from_gdb_spellings() {
        let mk = |name: &str, bits: Option<u32>| TargetArch {
            name: name.into(),
            pointer_bits: bits,
            raw: String::new(),
        };
        assert!(mk("i386:x86-64", Some(64)).is_x86_64());
        assert!(mk("i386:x86_64", None).is_x86_64());
        assert!(!mk("i386", Some(32)).is_x86_64());
        assert!(mk("i386", Some(32)).is_i386());
        assert!(mk("i386:intel", None).is_i386());
        assert!(!mk("i386:x86-64", Some(64)).is_i386());
        // A non-x86 target must be neither.
        assert!(!mk("aarch64", None).is_x86_64());
        assert!(!mk("aarch64", None).is_i386());
    }

    #[test]
    fn unparseable_architecture_text_is_unknown_not_a_guess() {
        assert_eq!(parse_show_architecture(""), None);
        assert_eq!(parse_show_architecture("no quotes here"), None);
        assert_eq!(parse_show_architecture("currently \"\""), None);
    }

    #[test]
    fn parses_pointer_size_output() {
        assert_eq!(parse_sizeof_pointer("$1 = 8\n"), Some(64));
        assert_eq!(parse_sizeof_pointer("$3 = 4"), Some(32));
        assert_eq!(parse_sizeof_pointer("no value"), None);
    }

    #[test]
    fn reads_the_real_fixture_elf_header() {
        let facts = read_elf_facts(std::path::Path::new(PAGING_ELF)).expect("read fixture ELF");
        assert_eq!(facts.class, ElfClass::Elf64);
        assert_eq!(facts.machine, 62);
        assert_eq!(facts.machine_name, "EM_X86_64");
        assert!(facts.is_x86);
    }

    #[test]
    fn a_non_elf_file_is_rejected_with_the_magic_as_evidence() {
        let dir = std::env::temp_dir().join(format!("princess-debug-arch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-an-elf.bin");
        std::fs::write(&path, b"MZ\x90\x00 this is a PE file, not ELF").unwrap();
        let err = read_elf_facts(&path).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("bad magic"), "{err}");
        let detail = err.detail.unwrap();
        // The evidence is the raw first four bytes: 4d 5a 90 00 ("MZ\x90\x00").
        assert!(
            detail.contains("4d") && detail.contains("5a"),
            "evidence should show the actual magic bytes, got: {detail}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_elf_is_rejected_rather_than_misread() {
        let dir = std::env::temp_dir().join(format!("princess-debug-arch2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("short.elf");
        std::fs::write(&path, b"\x7fELF\x02").unwrap();
        let err = read_elf_facts(&path).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The D10 scenario itself: a 32-bit ELF against the 64-bit stub.  The check
    /// must *name both sides*, and `require_consistent` must refuse.
    #[test]
    fn elf32_against_a_64_bit_target_is_a_reported_mismatch() {
        let elf = ElfFacts {
            class: ElfClass::Elf32,
            machine: 3,
            machine_name: "EM_386",
            is_x86: true,
        };
        let target = TargetArch {
            name: "i386:x86-64".into(),
            pointer_bits: Some(64),
            raw: String::new(),
        };
        let agrees = match elf.class {
            ElfClass::Elf64 => target.is_x86_64(),
            ElfClass::Elf32 => target.is_i386(),
        };
        assert!(!agrees, "elf32 must not be accepted by an x86-64 target");

        let check = ArchCheck::Mismatch {
            expected: format!("{} ({})", elf.class.as_str(), elf.machine_name),
            actual: target.name.clone(),
            reason: "the ELF is elf32 but the stub negotiated \"i386:x86-64\"".to_string(),
        };
        let err = check
            .clone()
            .require_consistent(std::path::Path::new("/tmp/kernel32.elf"))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        let detail = err.detail.as_deref().unwrap();
        assert!(detail.contains("elf32"), "{detail}");
        assert!(detail.contains("i386:x86-64"), "{detail}");
        assert!(detail.contains("D10"), "{detail}");
    }

    #[test]
    fn consistent_verdicts_pass_through_require_consistent() {
        let check = ArchCheck::Consistent {
            arch: "i386:x86-64".into(),
            pointer_bits: 64,
        };
        assert!(check.clone().require_consistent(std::path::Path::new("/x")).is_ok());
        assert!(check.is_consistent());
        assert!(check.summary().contains("i386:x86-64"));
    }

    #[test]
    fn unknown_is_not_treated_as_a_mismatch() {
        let check = ArchCheck::Unknown {
            raw: "no architecture reported".into(),
        };
        assert!(check.clone().require_consistent(std::path::Path::new("/x")).is_ok());
        assert!(!check.is_consistent());
        assert!(check.summary().starts_with("unknown:"));
    }

    #[test]
    fn pointer_width_alone_catches_a_truncated_g_packet() {
        // gdb *names* x86-64, but the stub's `g` packet only carried 32-bit
        // registers, so `sizeof(void*)` is 4.  Name-matching alone would pass.
        let pointer_bits = parse_sizeof_pointer("$1 = 4\n").unwrap();
        assert_eq!(pointer_bits, 32);
        assert_ne!(pointer_bits, ElfClass::Elf64.pointer_bits());
    }
}
