//! Symbol demangling — dispatching on the symbol's own prefix.
//!
//! Stack (D20, frozen): **`rustc-demangle` 0.1.28** for Rust legacy/v0 symbols
//! and **`cpp_demangle` 0.5.1** for Itanium C++ symbols.  There is no third
//! demangler and no attempt to guess a language from the file's `DW_AT_language`
//! alone: the prefix decides, which is what the producers actually guarantee.
//!
//! ## Why the dispatch is explicit rather than "try both"
//!
//! `cpp_demangle` accepts a surprising amount of Rust legacy mangling (`_ZN...`)
//! and produces a *plausible but wrong* answer, and `rustc-demangle` happily
//! returns its input unchanged for anything it does not recognise.  "Try one,
//! fall back to the other" therefore silently yields garbage.  The rules below
//! are prefix-driven and the fallback is **identity**, which is always honest:
//! the raw symbol name the ELF actually contains.

use princess_core::{PrincessError, Result};

/// Which demangler produced a [`Demangled`] name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mangling {
    /// Itanium C++ ABI (`_Z...`).
    CppItanium,
    /// Rust legacy (`_ZN...17h<hash>E`) or v0 (`_R...`).
    Rust,
    /// Not mangled — the name is its own display form.
    None,
}

impl Mangling {
    pub const fn as_str(self) -> &'static str {
        match self {
            Mangling::CppItanium => "cpp-itanium",
            Mangling::Rust => "rust",
            Mangling::None => "none",
        }
    }
}

/// One demangling result: the display name and how it was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demangled {
    /// Name to show in the UI.
    pub name: String,
    /// Which demangler ran (or [`Mangling::None`]).
    pub mangling: Mangling,
    /// `true` when the input was not valid in the dialect its prefix implied and
    /// the raw name was kept.  The UI must be able to say "raw" rather than
    /// pretend a failed demangle succeeded.
    pub fallback: bool,
}

/// Demangle `name`, dispatching on its prefix.
///
/// This never fails: a name that cannot be demangled comes back unchanged with
/// [`Demangled::fallback`] set and [`Mangling::None`], which is exactly what the
/// ELF contains.  Use [`demangle_checked`] when a hard error is wanted instead.
pub fn demangle(name: &str) -> Demangled {
    if name.is_empty() {
        return Demangled {
            name: String::new(),
            mangling: Mangling::None,
            fallback: false,
        };
    }

    if let Some(rest) = name.strip_prefix("_R") {
        // Rust v0.  `rustc_demangle` needs the leading `_`.
        let with_underscore = format!("_R{rest}");
        let out = rustc_demangle::try_demangle(&with_underscore);
        return match out {
            Ok(sym) => Demangled {
                name: format!("{sym:#}"),
                mangling: Mangling::Rust,
                fallback: false,
            },
            Err(_) => fallback(name),
        };
    }

    // Rust legacy AND Itanium C++ both use `_ZN...E`.  They are distinguished by
    // the trailing hash that `rustc` appends: `17h` + 16 hex digits.  This is the
    // same discriminator LLVM's `rust-demangle` uses, and it is the only reliable
    // one — a Rust legacy symbol demangled as C++ loses the generic arguments.
    if name.starts_with("_ZN") && name.ends_with('E') && looks_like_rust_legacy(name) {
        if let Ok(sym) = rustc_demangle::try_demangle(name) {
            return Demangled {
                name: format!("{sym:#}"),
                mangling: Mangling::Rust,
                fallback: false,
            };
        }
    }

    if name.starts_with("_Z") || name.starts_with("__Z") || name.starts_with("_GLOBAL__") {
        let parsed = cpp_demangle::Symbol::new(name);
        return match parsed {
            Ok(symbol) => match symbol.demangle() {
                Ok(text) => Demangled {
                    name: text,
                    mangling: Mangling::CppItanium,
                    fallback: false,
                },
                // The prefix promised Itanium mangling and the demangler could
                // not deliver: that is a fallback, not "this was never mangled".
                Err(_) => fallback(name),
            },
            Err(_) => fallback(name),
        };
    }

    // `.` / `$` prefixes used by a few compilers, plus everything else: not
    // mangled.  Strip nothing — the name IS the display name.
    raw(name)
}

/// Like [`demangle`], but an un-demanglable *mangled* name is an error.
///
/// Used by the CLI path where a silent passthrough would hide a real problem;
/// the engine's normal symbolication path uses the lenient [`demangle`].
pub fn demangle_checked(name: &str) -> Result<Demangled> {
    let out = demangle(name);
    if out.fallback && (name.starts_with("_Z") || name.starts_with("_R")) {
        return Err(PrincessError::internal(format!(
            "symbol {name:?} looks mangled but neither rustc-demangle nor cpp_demangle accepted it"
        )));
    }
    Ok(out)
}

fn raw(name: &str) -> Demangled {
    Demangled {
        name: name.to_string(),
        mangling: Mangling::None,
        fallback: false,
    }
}

/// A name whose prefix promised a mangling the demangler could not honour.
fn fallback(name: &str) -> Demangled {
    Demangled {
        name: name.to_string(),
        mangling: Mangling::None,
        fallback: true,
    }
}

/// `...17h<16 hex>E` at the very end — the rustc legacy hash suffix.
fn looks_like_rust_legacy(name: &str) -> bool {
    let body = match name.strip_prefix("_ZN").and_then(|n| n.strip_suffix('E')) {
        Some(body) => body,
        None => return false,
    };
    let idx = match body.rfind("17h") {
        Some(idx) => idx,
        None => return false,
    };
    let hash = &body[idx + 3..];
    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_c_names_are_left_alone() {
        let out = demangle("refkernel_fault_probe");
        assert_eq!(out.name, "refkernel_fault_probe");
        assert_eq!(out.mangling, Mangling::None);
        assert!(!out.fallback);
    }

    #[test]
    fn itanium_cpp_is_demangled() {
        // `std::vector<int>::push_back(int const&)` — a stable, well-known mangling.
        let out = demangle("_ZNSt6vectorIiSaIiEE9push_backERKi");
        assert_eq!(out.mangling, Mangling::CppItanium);
        assert!(!out.fallback, "{out:?}");
        assert!(out.name.contains("push_back"), "{out:?}");
        assert!(out.name.contains("vector"), "{out:?}");
    }

    #[test]
    fn rust_legacy_is_dispatched_by_its_hash_suffix() {
        // A legitimate rustc legacy symbol: `_ZN4core3fmt5write17h1234567890abcdefE`.
        let out = demangle("_ZN4core3fmt5write17h1234567890abcdefE");
        assert_eq!(out.mangling, Mangling::Rust, "{out:?}");
        assert!(!out.fallback);
        assert!(out.name.contains("core::fmt"), "{out:?}");
    }

    #[test]
    fn rust_v0_is_demangled() {
        // `_RNvCs1234_4test3foo` — v0 shape produced by rustc 1.60+.
        let out = demangle("_RNvCs1234_4test3foo");
        assert_eq!(out.mangling, Mangling::Rust, "{out:?}");
        assert!(!out.fallback, "{out:?}");
        assert!(out.name.contains("test::foo"), "{out:?}");
    }

    #[test]
    fn rust_legacy_is_not_handed_to_the_cpp_demangler() {
        // The exact bug the prefix dispatch exists to prevent: cpp_demangle
        // accepts `_ZN...E` and would drop the hash, so the answer must still be
        // tagged Rust.
        let out = demangle("_ZN4core3fmt5write17h1234567890abcdefE");
        assert_ne!(out.mangling, Mangling::CppItanium);
    }

    #[test]
    fn a_broken_mangled_name_falls_back_to_the_raw_text() {
        let out = demangle("_Z!!!!not-a-symbol");
        assert!(out.fallback);
        assert_eq!(out.name, "_Z!!!!not-a-symbol");
        assert_eq!(out.mangling, Mangling::None);
    }

    #[test]
    fn checked_demangle_rejects_a_broken_mangled_name() {
        let err = demangle_checked("_Z!!!!not-a-symbol").unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
    }

    #[test]
    fn checked_demangle_accepts_a_c_name() {
        let out = demangle_checked("kernel_main").unwrap();
        assert_eq!(out.name, "kernel_main");
    }

    #[test]
    fn empty_names_are_not_an_error() {
        let out = demangle("");
        assert_eq!(out.name, "");
        assert_eq!(out.mangling, Mangling::None);
    }

    #[test]
    fn legacy_hash_detection_is_precise() {
        assert!(looks_like_rust_legacy("_ZN4core3fmt5write17h1234567890abcdefE"));
        assert!(!looks_like_rust_legacy("_ZN4core3fmt5writeE"));
        // Short hash -> not rustc legacy.
        assert!(!looks_like_rust_legacy("_ZN4core3fmt5write17h1234E"));
        // Non-hex tail -> not rustc legacy.
        assert!(!looks_like_rust_legacy("_ZN4core3fmt5write17hzzzzzzzzzzzzzzzzE"));
    }
}
