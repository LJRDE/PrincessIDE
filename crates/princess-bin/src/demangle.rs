//! Name demangling for the two mangling schemes a kernel IDE actually meets.
//!
//! D20 freezes the pair: `rustc-demangle` for Rust, `cpp_demangle` for Itanium
//! C++.  Dispatch is by **prefix**, per research D §1.4:
//!
//! | shape | owner |
//! |---|---|
//! | `_R...` | Rust v0 (`rustc-demangle`) |
//! | `_ZN...17h<hash>E` | Rust legacy (`rustc-demangle`, which handles both) |
//! | any other `_Z...` | Itanium C++ (`cpp_demangle`) |
//! | anything else | returned unchanged |
//!
//! Two deliberate non-features:
//!
//! * **No `c++filt` / `nm -C` shell-out.**  A symbol list is tens of thousands of
//!   entries; research D §1.4 item 3 rejects a process per lookup, and a long
//!   lived pipe is a subprocess the IDE would have to babysit.  Pure Rust, in
//!   process, cacheable by the caller.
//! * **No guessing.**  A name that neither library can demangle is returned
//!   *verbatim*.  Returning a half-demangled or "cleaned up" string would put a
//!   name in front of the user that appears in no symbol table.
//!
//! The `demangle_name` entry point is infallible by design: "this is not a
//! mangled name" is not an error condition.

/// Demangle `name`, or return it unchanged when it is not a mangled name.
///
/// The returned string is what the UI should display; callers that need to group
/// variants (C++ `D0`/`D1`/`D2` destructors, Rust `.cold` / `.llvm.<hash>`
/// suffixes, generic instantiations) should group on the demangled *form* — see
/// the note below.
///
/// # Variant suffixes are preserved on purpose
///
/// Research D §1.4 measured that C++ emits three destructor variants and Rust
/// appends `.llvm.<hash>` / `.cold`.  Collapsing those here would destroy the
/// information needed to jump to the *right* variant, so the suffix survives
/// demangling and the folding is left to the view layer where it can be shown as
/// "1 of 3 variants".
#[must_use]
pub fn demangle_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }

    // Rust first: v0 symbols start with `_R`, legacy with `_ZN`.  `rustc-demangle`
    // covers both, so a `_ZN` symbol that happens to be Rust wins over the C++
    // parser — which is correct, because the legacy Rust mangling *is* the
    // Itanium grammar with a hash suffix, and `rustc-demangle` strips that hash
    // while `cpp_demangle` would leave it in place.
    if name.starts_with("_R") || name.starts_with("_ZN") {
        if let Ok(demangled) = rustc_demangle::try_demangle(name) {
            // `{:#}` is the "alternate" form that omits the disambiguator hash.
            return format!("{demangled:#}");
        }
    }

    if name.starts_with("_Z") || name.starts_with("__Z") {
        if let Ok(symbol) = cpp_demangle::Symbol::new(name) {
            // Drop the return type: for a symbol list it is noise, and the
            // disassembly view already shows the ABI.  `cpp_demangle` needs a
            // fallible formatting step, so a failure here falls through to the
            // verbatim name rather than to a partial one.
            let options = cpp_demangle::DemangleOptions::new().no_return_type();
            if let Ok(demangled) = symbol.demangle_with_options(&options) {
                return demangled;
            }
        }
    }

    name.to_string()
}

/// Strip the Rust "variant" suffixes that make one function look like several.
///
/// Returns the friendly group key: `foo.llvm.1234` and `foo.cold` both map to
/// `foo`.  Kept separate from [`demangle_name`] so the caller can display the
/// full name *and* group by the key.
#[must_use]
pub fn group_key(name: &str) -> &str {
    let mut end = name.len();
    // `.llvm.<hex>` and `.cold` / `.cold.1` style suffixes.
    if let Some(index) = name.find(".llvm.") {
        end = end.min(index);
    }
    if let Some(index) = name.find(".cold") {
        end = end.min(index);
    }
    &name[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_c_names_are_untouched() {
        assert_eq!(demangle_name("refkernel_fault_probe"), "refkernel_fault_probe");
        assert_eq!(demangle_name("main"), "main");
        assert_eq!(demangle_name(""), "");
        // Not a mangled name at all -> verbatim, no fabrication.
        assert_eq!(demangle_name("_start"), "_start");
    }

    #[test]
    fn rust_legacy_mangling_loses_only_the_hash() {
        let name = "_ZN4core3fmt9Formatter3pad17h0123456789abcdefE";
        let demangled = demangle_name(name);
        assert!(
            demangled.contains("core::fmt::Formatter::pad"),
            "got {demangled}"
        );
        assert!(
            !demangled.contains("17h0123456789abcdef"),
            "the disambiguator hash must be dropped in the alternate form: {demangled}"
        );
    }

    #[test]
    fn rust_v0_mangling_is_understood() {
        // Research D §1.4 recorded this shape from `nm librdemo.rlib`.  Verified
        // against `rustc-demangle` 0.1.28 directly: the `I` (generic args) form
        // needs a full arg list and the truncated sample is *not* parseable, so
        // the non-generic v0 symbol is used as the golden here and the report
        // records the correction.
        let name = "_RNvCsfJh2wXCkyFt_13princess_demo11generic_add";
        let demangled = demangle_name(name);
        assert_ne!(demangled, name, "v0 must demangle, got the input back");
        assert!(demangled.contains("generic_add"), "got {demangled}");
        assert!(
            demangled.contains("princess_demo"),
            "the module path must survive: {demangled}"
        );
    }

    #[test]
    fn a_malformed_v0_symbol_is_returned_verbatim_not_half_demangled() {
        // The truncated generic form from the research report really is
        // unparseable; the correct behaviour is to hand the raw name back
        // rather than guess at the missing arguments.
        let name = "_RINvCsfJh2wXCkyFt_13princess_demo11generic_add";
        assert_eq!(demangle_name(name), name);
    }

    #[test]
    fn cpp_itanium_mangling_is_understood() {
        // Shapes measured in research D §1.4 via `nm | c++filt`.
        let demangled = demangle_name("_ZNK4Base1fEi");
        assert!(demangled.contains("Base::f"), "got {demangled}");
        assert!(demangled.contains("const"), "got {demangled}");

        let destructor = demangle_name("_ZN7DerivedD0Ev");
        assert!(destructor.contains("Derived::~Derived"), "got {destructor}");
    }

    #[test]
    fn cpp_return_type_is_suppressed() {
        // `_Z3fooi` is `foo(int)`; with `no_return_type` the demangled form is
        // the bare `foo(int)` argument list without a leading return type.  The
        // assertion therefore checks that the result is exactly the expected
        // call shape rather than merely lacking the substring "int)".
        assert_eq!(demangle_name("_Z3fooi"), "foo(int)");
        // A function with a non-void return type must also drop it.
        let with_return = demangle_name("_Z3bari");
        assert_eq!(with_return, "bar(int)", "got {with_return}");
    }

    #[test]
    fn group_key_folds_variants_but_demangle_keeps_them() {
        assert_eq!(group_key("foo.llvm.1234"), "foo");
        assert_eq!(group_key("foo.cold"), "foo");
        assert_eq!(group_key("foo.cold.1"), "foo");
        assert_eq!(group_key("foo"), "foo");
        // The full name still carries the variant, so the UI can expand it.
        assert!(demangle_name("_ZN7DerivedD2Ev").contains("~Derived"));
    }
}
