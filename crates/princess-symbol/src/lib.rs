//! # princess-symbol
//!
//! The PrincessIDE **symbol engine** (P2-B3).  It answers one question well:
//! *given an address, what source is it?* — and it answers it honestly, with a
//! span rather than a bare line number, because that is what the
//! side-by-side **disassembly + source line** view (D20) actually needs.
//!
//! | module | responsibility | spec |
//! |---|---|---|
//! | [`elf`] | ELF container facts with **`object` 0.40.0**: format, entry point, sections, `.symtab`, and the *possibly absent* GNU build-id | D20, P2-4 |
//! | [`dwarf`] | **`gimli` 0.34.0 + `addr2line` 0.27.1**: address → function/file/line **plus address span and line range** | D20, P2-4 |
//! | [`demangle`] | prefix-dispatched **`rustc-demangle` 0.1.28** / **`cpp_demangle` 0.5.1** | D20 |
//! | [`symbol`] | the public engine: [`SymbolIndex`], `run.fault` enrichment, frame-pointer stack recovery | P2-4 |
//! | [`binprovider`] | the [`princess_core::BinProvider`] implementation the engine dispatches through | §5 |
//!
//! ## Scope boundaries (both are deliberate)
//!
//! * **No disassembly.**  Reading instruction bytes and formatting x86 is P5's
//!   job (`iced-x86` 1.21.0 per D20).  This crate supplies the *source rows*
//!   P5 pairs with its instructions, and stops there.
//! * **No DWARF CFI unwinding of a live target.**  P4 owns the debugger; the
//!   stack recovery here is the frame-pointer walk the `-O0` kernel fixtures
//!   actually support, driven by a memory reader the caller supplies.
//!
//! ## The three honesty rules
//!
//! 1. **`buildId` is `None`, not invented.**  Both reference fixtures are linked
//!    with `--build-id=none`; the contract says *宁可 null 也不得编造*.
//! 2. **No address outside DWARF gets a line.**  There is no "nearest symbol"
//!    fallback on the exact path; the coarse symbol-table answer is a separate,
//!    explicitly-labelled API.
//! 3. **`ripText` is never rewritten.**  Enriching `run.fault` only ever adds
//!    `symbolicated`.
//!
//! ## Events
//!
//! The engine pushes everything through [`princess_core::EventSink`]; this crate
//! never touches the recorder.  The one event it feeds is `symbols.indexed`,
//! whose payload it produces via [`SymbolIndex::indexed_event_fields`] so the
//! `buildId: null` rule cannot drift between callers.

pub mod binprovider;
pub mod demangle;
pub mod dwarf;
pub mod elf;
pub mod symbol;

pub use binprovider::ElfSymbolProvider;
pub use demangle::{demangle, demangle_checked, Demangled, Mangling};
pub use dwarf::{DwarfIndex, SourceLine};
pub use elf::{
    find_symbol, read_elf_facts, read_sections, read_symbols, ElfFacts,
};
pub use symbol::{
    explain_missing_symbolication, symbolicate_fault, symbolicate_fault_event, walk_frame_pointer_chain,
    CoarseLocation, StackFrameFacts, SymbolIndex,
};

/// The frozen dependency stack (D20), reported so `doctor`-style tooling and the
/// UI can state which parsers produced an answer without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolStack {
    pub elf_parser: &'static str,
    pub dwarf_decoder: &'static str,
    pub line_resolver: &'static str,
    pub rust_demangler: &'static str,
    pub cpp_demangler: &'static str,
}

/// The one true stack for v1 (D20 froze it; this constant exists so a future
/// change has to be made in exactly one place and will show up in a diff).
pub const SYMBOL_STACK: SymbolStack = SymbolStack {
    elf_parser: "object 0.40.0",
    dwarf_decoder: "gimli 0.34.0",
    line_resolver: "addr2line 0.27.1",
    rust_demangler: "rustc-demangle 0.1.28",
    cpp_demangler: "cpp_demangle 0.5.1",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frozen_stack_is_the_one_d20_requires() {
        assert_eq!(SYMBOL_STACK.elf_parser, "object 0.40.0");
        assert_eq!(SYMBOL_STACK.dwarf_decoder, "gimli 0.34.0");
        assert_eq!(SYMBOL_STACK.line_resolver, "addr2line 0.27.1");
        assert_eq!(SYMBOL_STACK.rust_demangler, "rustc-demangle 0.1.28");
        assert_eq!(SYMBOL_STACK.cpp_demangler, "cpp_demangle 0.5.1");
    }

    /// The public surface the engine and P5 will call must stay nameable as
    /// trait objects where the contract says so.
    #[test]
    fn the_bin_provider_is_object_safe() {
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn princess_core::BinProvider>();
    }
}
