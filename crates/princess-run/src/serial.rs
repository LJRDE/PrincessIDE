//! Guest serial observation: `log.append(serial.com1)` → `run.fault` (contract §2).
//!
//! The engine only ever reports what the guest actually printed.  Two rules make
//! that honest:
//!
//! 1. **`ripText` is preserved verbatim.**  `0x0000000000100b3d` stays exactly
//!    that string; the parsed `rip: u64` is a convenience, never a replacement.
//!    A fixture whose printed width changes must show up as a changed
//!    `ripText`, not as a silently re-formatted one.
//! 2. **A fault with no address produces no event.**  A garbled
//!    `FAULT_RIP=0xZZ` line yields nothing rather than a guessed RIP.
//!
//! ## `#PF` requires `CR0.PG=1` (D9)
//!
//! A page fault can only exist while paging is enabled.  When the serial stream
//! itself states the paging state (the paging fixture prints
//! `CR0.PG=1 CR0.PE=1 …`), a `#PF` is accepted only if `PG=1` was observed
//! first.  With no paging evidence at all the fault is still *reported* (the
//! guest really did print a `#PF`) but [`PageFaultGate`] records that the
//! precondition was unproven, so the engine can say so instead of asserting a
//! fact it did not verify.

use princess_core::event::RunFaultPayload;
use princess_core::types::{RegisterFile, SymbolicatedLocation};

/// Prefix of the fixed boot banner line, as printed by the fixtures.
pub const BANNER_PREFIX: &str = "PrincessIDE";
/// The reference kernel's banner (D3, frozen).
pub const REFKERNEL_BANNER: &str = "PrincessIDE reference kernel booted";
/// The paging fixture's banner (`20-acceptance.md`).
pub const PAGING_BANNER: &str = "PrincessIDE paging kernel booted";
/// Prefix of the exception report line.
pub const EXCEPTION_PREFIX: &str = "EXCEPTION: vector=";
/// Prefix of the machine-parsable fault line.
pub const FAULT_PREFIX: &str = "FAULT_RIP=";
/// Prefix of the panic line.
pub const PANIC_PREFIX: &str = "PANIC:";
/// The line a `#PF`-enabled fixture prints to prove paging is on (D9).
pub const PAGING_ENABLED_MARKER: &str = "CR0.PG=1";

/// A fault exactly as the guest printed it, before symbolication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFault {
    /// Exception vector number, when the guest printed one.
    pub vector_number: Option<u32>,
    /// Vector mnemonic the guest printed (`#UD`), else the canonical name for
    /// `vector_number`.
    pub vector: String,
    /// Parsed address; the convenience value.
    pub rip: u64,
    /// **The guest's own hex text for the address, verbatim.**
    pub rip_text: String,
    pub error_code: u64,
    pub cs: Option<String>,
    pub rflags: Option<String>,
}

impl RawFault {
    /// Build the contract payload.  `symbolicated` is supplied by the caller
    /// (the run engine consults a symbolizer; this crate never invents one).
    pub fn into_payload(self, symbolicated: Option<SymbolicatedLocation>) -> RunFaultPayload {
        let mut regs = RegisterFile::new();
        regs.insert("RIP".to_string(), self.rip_text.clone());
        if let Some(cs) = &self.cs {
            regs.insert("CS".to_string(), cs.clone());
        }
        if let Some(rflags) = &self.rflags {
            regs.insert("RFLAGS".to_string(), rflags.clone());
        }
        RunFaultPayload {
            vector: self.vector,
            rip: self.rip,
            rip_text: self.rip_text,
            error_code: self.error_code,
            regs,
            symbolicated,
        }
    }

    /// Is this a page fault (vector 14)?
    pub fn is_page_fault(&self) -> bool {
        self.vector_number == Some(14) || self.vector == "#PF"
    }
}

/// Why a `#PF` was or was not trusted (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFaultGate {
    /// `CR0.PG=1` was observed on the serial stream before the `#PF`.
    PagingConfirmed,
    /// The guest reported a `#PF` but the stream never stated `CR0.PG=1`.
    PagingUnproven,
    /// The stream explicitly showed paging off (`CR0.PG=0`).
    PagingDisabled,
    /// Not a page fault at all.
    NotApplicable,
}

/// Something worth emitting an event for, found on one serial line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialObservation {
    /// A boot banner appeared (which one is in the payload).
    Banner(String),
    /// The guest reported an unhandled CPU exception with a fault RIP.
    Fault(RawFault),
    /// The guest reached its panic path.
    Panic,
    /// The serial stream stated the paging state.
    PagingState { enabled: bool },
}

/// Incremental observer for guest serial text.
#[derive(Debug, Default, Clone)]
pub struct SerialObserver {
    banner: Option<String>,
    panic_seen: bool,
    paging_enabled: Option<bool>,
    last_vector: Option<(u32, String)>,
    fault: Option<RawFault>,
    fault_count: u64,
    page_fault_gate: Option<PageFaultGate>,
}

impl SerialObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The banner text seen, if any.
    pub fn banner(&self) -> Option<&str> {
        self.banner.as_deref()
    }

    pub fn banner_seen(&self) -> bool {
        self.banner.is_some()
    }

    pub fn panic_seen(&self) -> bool {
        self.panic_seen
    }

    /// The most complete fault seen so far.
    pub fn fault(&self) -> Option<&RawFault> {
        self.fault.as_ref()
    }

    /// How many faults the guest reported.
    pub fn fault_count(&self) -> u64 {
        self.fault_count
    }

    /// Whether paging was ever stated as on (`Some(true)`), off, or never
    /// mentioned (`None`).
    pub fn paging_enabled(&self) -> Option<bool> {
        self.paging_enabled
    }

    /// The verdict for the last `#PF`, when one was seen.
    pub fn page_fault_gate(&self) -> Option<PageFaultGate> {
        self.page_fault_gate
    }

    /// Feed one complete line; returns every observation it carries.
    pub fn feed(&mut self, line: &str) -> Vec<SerialObservation> {
        let mut observations = Vec::new();

        if self.banner.is_none() {
            if let Some(banner) = detect_banner(line) {
                self.banner = Some(banner.clone());
                observations.push(SerialObservation::Banner(banner));
            }
        }

        // Paging state: the fixtures print `CR0.PG=1 …` (and `PAGING_ENABLED`).
        if line.contains(PAGING_ENABLED_MARKER) && self.paging_enabled != Some(true) {
            self.paging_enabled = Some(true);
            observations.push(SerialObservation::PagingState { enabled: true });
        } else if line.contains("CR0.PG=0") && self.paging_enabled != Some(false) {
            self.paging_enabled = Some(false);
            observations.push(SerialObservation::PagingState { enabled: false });
        }

        if let Some(rest) = line.split_once(EXCEPTION_PREFIX).map(|(_, rest)| rest) {
            if let Some(vector) = parse_vector(rest) {
                self.last_vector = Some(vector);
            }
        }

        // `FAULT_ERROR=` carries the #PF error code on a separate line for the
        // paging fixture; keep it so the payload is complete.
        let mut line_error: Option<u64> = None;
        if let Some(rest) = line.split_once("FAULT_ERROR=").map(|(_, rest)| rest) {
            line_error = rest
                .split_whitespace()
                .next()
                .and_then(parse_hex_u64);
        }

        if let Some(rest) = line.split_once(FAULT_PREFIX).map(|(_, rest)| rest) {
            if let Some(mut fault) = parse_fault(rest) {
                if let Some((number, name)) = &self.last_vector {
                    fault.vector_number = Some(*number);
                    fault.vector = name.clone();
                }
                if let Some(error) = line_error {
                    fault.error_code = error;
                }
                self.fault_count += 1;
                self.page_fault_gate = Some(self.gate_for(&fault));
                self.fault = Some(fault.clone());
                observations.push(SerialObservation::Fault(fault));
            }
        }

        if line.contains(PANIC_PREFIX) && !self.panic_seen {
            self.panic_seen = true;
            observations.push(SerialObservation::Panic);
        }

        observations
    }

    /// Apply the D9 precondition to a `#PF`.
    fn gate_for(&self, fault: &RawFault) -> PageFaultGate {
        if !fault.is_page_fault() {
            return PageFaultGate::NotApplicable;
        }
        match self.paging_enabled {
            Some(true) => PageFaultGate::PagingConfirmed,
            Some(false) => PageFaultGate::PagingDisabled,
            None => PageFaultGate::PagingUnproven,
        }
    }
}

/// Recognise either fixture's banner without hard-coding a single product name
/// into the parser (the banner text itself is still the contract's).
fn detect_banner(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with(BANNER_PREFIX) {
        return None;
    }
    // Only a line that looks like a banner, i.e. does not look like a log line.
    if trimmed.starts_with('[') {
        return None;
    }
    Some(trimmed.to_string())
}

/// `0x06 (#UD invalid opcode)` → `(6, "#UD")`.
fn parse_vector(rest: &str) -> Option<(u32, String)> {
    let token = rest.split_whitespace().next()?;
    let number = parse_hex_u64(token)? as u32;
    let name = match rest.find('(') {
        Some(open) => {
            let after = &rest[open + 1..];
            let candidate = after.split_whitespace().next().unwrap_or("");
            if candidate.starts_with('#') || candidate.chars().all(|c| c.is_ascii_uppercase()) {
                candidate.to_string()
            } else {
                vector_name(number).to_string()
            }
        }
        None => vector_name(number).to_string(),
    };
    Some((number, name))
}

/// `0x0000000000100b3d cs=0x0008 rflags=0x… error=0x…` → facts.
fn parse_fault(rest: &str) -> Option<RawFault> {
    let mut fields: Vec<&str> = rest.split_whitespace().collect();
    let rip_text = fields.first()?.to_string();
    let rip = parse_hex_u64(&rip_text)?;
    fields.remove(0);

    let mut cs = None;
    let mut rflags = None;
    let mut error_code = 0u64;
    for field in fields {
        if let Some(value) = field.strip_prefix("cs=") {
            cs = Some(value.to_string());
        } else if let Some(value) = field.strip_prefix("rflags=") {
            rflags = Some(value.to_string());
        } else if let Some(value) = field.strip_prefix("error=") {
            error_code = parse_hex_u64(value).unwrap_or(0);
        }
    }

    Some(RawFault {
        vector_number: None,
        vector: String::new(),
        rip,
        rip_text,
        error_code,
        cs,
        rflags,
    })
}

/// Parse `0x…` / `0X…` / bare hex into a `u64`.  A malformed token is `None`,
/// never `0`.
pub fn parse_hex_u64(text: &str) -> Option<u64> {
    let trimmed = text.trim().trim_end_matches(',');
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// The x86-64 exception mnemonics the fixtures print.
pub fn vector_name(number: u32) -> &'static str {
    match number {
        0 => "#DE",
        1 => "#DB",
        2 => "NMI",
        3 => "#BP",
        4 => "#OF",
        5 => "#BR",
        6 => "#UD",
        7 => "#NM",
        8 => "#DF",
        10 => "#TS",
        11 => "#NP",
        12 => "#SS",
        13 => "#GP",
        14 => "#PF",
        16 => "#MF",
        17 => "#AC",
        18 => "#MC",
        19 => "#XM",
        21 => "#CP",
        _ => "unknown",
    }
}

/// Extract a `FAULT_RIP=0x…` address from a captured log.
pub fn fault_rip_in_log(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some((_, rest)) = line.split_once(FAULT_PREFIX) {
            if let Some(rip_text) = rest.split_whitespace().next() {
                if let Some(value) = parse_hex_u64(rip_text) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Recognise a banner anywhere in a captured log (used by tests and by the
/// report tooling); returns the banner text.
pub fn banner_in_log(text: &str) -> Option<String> {
    text.lines().find_map(detect_banner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference fixture's panic tail, verbatim (`build/serial.log`).
    const REFKERNEL_TAIL: &str = "\
[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)
[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000000046 error=0x0000000000000000
[refkernel] PANIC: unhandled CPU exception, halting
";

    /// The paging fixture's panic tail, verbatim (`build/serial.log`).
    const PAGING_TAIL: &str = "\
[pagingkernel] CR0=0x0000000080000011 CR2=0x0000000000000000 CR3=0x0000000000104000
[pagingkernel] CR0.PG=1 CR0.PE=1 CR4.PAE=1
[pagingkernel] PAGING_ENABLED cr3=0x0000000000104000 cr0=0x0000000080000011
[pagingkernel] EXCEPTION: vector=0x0e (#PF page fault)
[pagingkernel] FAULT_RIP=0x00000000001011dd cs=0x0008 rflags=0x0000000000000002
[pagingkernel] FAULT_ERROR=0x0000000000000000
[pagingkernel] FAULT_ADDR=0x0000000000400000
[pagingkernel] PANIC: unhandled CPU exception, halting
";

    fn observe(text: &str) -> (SerialObserver, Vec<SerialObservation>) {
        let mut observer = SerialObserver::new();
        let mut seen = Vec::new();
        for line in text.lines() {
            seen.extend(observer.feed(line));
        }
        (observer, seen)
    }

    #[test]
    fn the_reference_log_yields_banner_fault_and_panic() {
        let log = format!("{REFKERNEL_BANNER}\n{REFKERNEL_TAIL}");
        let (observer, seen) = observe(&log);
        assert!(observer.banner_seen());
        assert!(observer.panic_seen());
        assert_eq!(observer.fault_count(), 1);
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(
            seen[0],
            SerialObservation::Banner(REFKERNEL_BANNER.to_string())
        );

        let SerialObservation::Fault(fault) = &seen[1] else {
            panic!("expected a fault, got {:?}", seen[1]);
        };
        assert_eq!(fault.rip, 0x10_0b3d);
        // D3's frozen assertion: the text is preserved byte for byte.
        assert_eq!(fault.rip_text, "0x0000000000100b3d");
        assert_eq!(fault.vector, "#UD");
        assert_eq!(fault.vector_number, Some(6));
        assert_eq!(fault.error_code, 0);
        assert_eq!(fault.cs.as_deref(), Some("0x0008"));
        assert_eq!(fault.rflags.as_deref(), Some("0x0000000000000046"));
        assert_eq!(seen[2], SerialObservation::Panic);

        let payload = fault.clone().into_payload(Some(SymbolicatedLocation {
            symbol: "refkernel_fault_probe".into(),
            file: "/root/PrincessIDE/fixtures/refkernel/kernel.c".into(),
            line: 100,
        }));
        assert_eq!(payload.regs.get("RIP").unwrap(), "0x0000000000100b3d");
        assert_eq!(payload.symbolicated.unwrap().line, 100);
        assert_eq!(
            observer.page_fault_gate(),
            Some(PageFaultGate::NotApplicable),
            "#UD is not a page fault, so the gate does not apply"
        );
    }

    #[test]
    fn the_paging_log_confirms_pg_before_the_page_fault() {
        let log = format!("{PAGING_BANNER}\n{PAGING_TAIL}");
        let (observer, seen) = observe(&log);
        assert_eq!(observer.banner(), Some(PAGING_BANNER));
        assert_eq!(observer.paging_enabled(), Some(true));
        // D9: the #PF is only accepted because PG=1 was observed first.
        assert_eq!(
            observer.page_fault_gate(),
            Some(PageFaultGate::PagingConfirmed)
        );

        let SerialObservation::Fault(fault) = seen
            .iter()
            .find(|o| matches!(o, SerialObservation::Fault(_)))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(fault.is_page_fault());
        assert_eq!(fault.vector, "#PF");
        assert_eq!(fault.vector_number, Some(14));
        assert_eq!(fault.rip, 0x10_11dd);
        assert_eq!(fault.rip_text, "0x00000000001011dd");
        assert_eq!(fault.error_code, 0);

        // The paging-state observation must precede the fault observation.
        let paging_at = seen
            .iter()
            .position(|o| matches!(o, SerialObservation::PagingState { enabled: true }))
            .unwrap();
        let fault_at = seen
            .iter()
            .position(|o| matches!(o, SerialObservation::Fault(_)))
            .unwrap();
        assert!(paging_at < fault_at, "{seen:?}");
    }

    /// D9's precondition, made observable: a `#PF` with no paging evidence is
    /// still reported (the guest printed it) but explicitly *unproven*.
    #[test]
    fn a_page_fault_without_paging_evidence_is_flagged_unproven() {
        let (observer, _) = observe("[k] EXCEPTION: vector=0x0e (#PF page fault)\n[k] FAULT_RIP=0x0000000000001234 error=0x0000000000000002\n");
        assert_eq!(observer.paging_enabled(), None);
        assert_eq!(
            observer.page_fault_gate(),
            Some(PageFaultGate::PagingUnproven)
        );
    }

    #[test]
    fn paging_explicitly_disabled_is_recorded_as_such() {
        let (observer, _) = observe("[k] CR0.PG=0 CR0.PE=1\n");
        assert_eq!(observer.paging_enabled(), Some(false));
    }

    #[test]
    fn a_non_page_fault_is_not_gated() {
        let (observer, _) = observe(REFKERNEL_TAIL);
        assert_eq!(
            observer.page_fault_gate(),
            Some(PageFaultGate::NotApplicable),
            "#UD is not a page fault"
        );
        assert!(!observer.fault().unwrap().is_page_fault());
    }

    #[test]
    fn each_observation_is_reported_once() {
        let log = format!("{REFKERNEL_BANNER}\n{REFKERNEL_TAIL}{REFKERNEL_TAIL}");
        let (observer, seen) = observe(&log);
        assert_eq!(
            seen.iter()
                .filter(|o| matches!(o, SerialObservation::Banner(_)))
                .count(),
            1
        );
        assert_eq!(
            seen.iter()
                .filter(|o| **o == SerialObservation::Panic)
                .count(),
            1
        );
        // Two FAULT_RIP lines really are two faults, even if identical.
        assert_eq!(observer.fault_count(), 2);
    }

    #[test]
    fn a_garbled_fault_line_produces_no_event() {
        let (observer, seen) = observe("[k] FAULT_RIP=0xZZZZ cs=0x0008\n");
        assert!(seen.is_empty());
        assert!(observer.fault().is_none(), "no address means no fault event");
        assert_eq!(observer.fault_count(), 0);
    }

    #[test]
    fn a_banner_looking_line_that_is_really_a_log_line_is_ignored() {
        // `[refkernel]` lines must never be mistaken for the banner.
        let (observer, _) = observe("[refkernel] PrincessIDE reference kernel booted\n");
        assert!(!observer.banner_seen());
    }

    #[test]
    fn hex_parsing_rules_never_invent_zero() {
        assert_eq!(parse_hex_u64("0x10"), Some(16));
        assert_eq!(parse_hex_u64("0X10"), Some(16));
        assert_eq!(parse_hex_u64("ff"), Some(255));
        assert_eq!(parse_hex_u64("0x0000000000100b3d"), Some(0x10_0b3d));
        assert_eq!(parse_hex_u64("0x"), None);
        assert_eq!(parse_hex_u64("0xGG"), None);
        assert_eq!(parse_hex_u64(""), None);
        assert_eq!(parse_hex_u64("zzz"), None);
    }

    #[test]
    fn rip_can_be_recovered_from_a_captured_log() {
        assert_eq!(fault_rip_in_log(REFKERNEL_TAIL), Some(0x10_0b3d));
        assert_eq!(fault_rip_in_log(PAGING_TAIL), Some(0x10_11dd));
        assert_eq!(fault_rip_in_log("nothing here\n"), None);
    }

    #[test]
    fn banners_are_recognised_per_fixture() {
        assert_eq!(
            banner_in_log(&format!("noise\n{REFKERNEL_BANNER}\n")),
            Some(REFKERNEL_BANNER.to_string())
        );
        assert_eq!(
            banner_in_log(&format!("{PAGING_BANNER}\n")),
            Some(PAGING_BANNER.to_string())
        );
        assert_eq!(banner_in_log("nothing\n"), None);
    }

    #[test]
    fn vector_names_match_the_fixture_table() {
        assert_eq!(vector_name(6), "#UD");
        assert_eq!(vector_name(14), "#PF");
        assert_eq!(vector_name(13), "#GP");
        assert_eq!(vector_name(8), "#DF");
        assert_eq!(vector_name(99), "unknown");
    }
}
