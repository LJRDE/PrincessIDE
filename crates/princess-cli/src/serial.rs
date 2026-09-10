//! Guest serial observation (`log.append` stream `serial.com1` + `run.fault`).
//!
//! The reference kernel's panic report is a **contract with the fixture**
//! (`docs/p0-toolchain-report.md` §6) and must not drift:
//!
//! ```text
//! PrincessIDE reference kernel booted
//! [refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)
//! [refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000000046 error=0x0000000000000000
//! [refkernel] PANIC: unhandled CPU exception, halting
//! ```
//!
//! This module turns those lines into engine facts.  It only ever reports what
//! the guest actually printed: `rip`, `cs`, `rflags` and `error` come from the
//! `FAULT_RIP` line verbatim, and a fault without a `FAULT_RIP` line produces no
//! `run.fault` event rather than a guessed address.

use princess_core::event::RunFaultPayload;
use princess_core::types::{RegisterFile, SymbolicatedLocation};

/// The fixed boot banner the acceptance tests assert on.
pub const BANNER: &str = "PrincessIDE reference kernel booted";
/// Prefix of the exception report line.
pub const EXCEPTION_PREFIX: &str = "EXCEPTION: vector=";
/// Prefix of the machine-parsable fault line.
pub const FAULT_PREFIX: &str = "FAULT_RIP=";
/// Prefix of the panic line.
pub const PANIC_PREFIX: &str = "PANIC:";

/// A fault as reported on the serial line, before symbolication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFault {
    pub vector_number: u32,
    /// Vector mnemonic as printed by the guest (`#UD`).
    pub vector: String,
    pub rip: u64,
    pub rip_text: String,
    pub error_code: u64,
    pub cs: String,
    pub rflags: String,
}

impl RawFault {
    /// Build the `run.fault` payload; `symbolicated` is filled by the caller.
    pub fn into_payload(self, symbolicated: Option<SymbolicatedLocation>) -> RunFaultPayload {
        let mut regs = RegisterFile::new();
        regs.insert("RIP".to_string(), self.rip_text.clone());
        regs.insert("CS".to_string(), self.cs.clone());
        regs.insert("RFLAGS".to_string(), self.rflags.clone());
        RunFaultPayload {
            vector: self.vector,
            rip: self.rip,
            rip_text: self.rip_text,
            error_code: self.error_code,
            regs,
            symbolicated,
        }
    }
}

/// Something worth emitting an event for, found on one serial line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialObservation {
    /// The fixed boot banner appeared.
    Banner,
    /// The guest reported an unhandled CPU exception with a fault RIP.
    Fault(RawFault),
    /// The guest reached its panic path.
    Panic,
}

/// Incremental observer for the guest's serial text.
#[derive(Debug, Default, Clone)]
pub struct SerialObserver {
    banner_seen: bool,
    panic_seen: bool,
    last_vector: Option<(u32, String)>,
    fault: Option<RawFault>,
}

impl SerialObserver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn banner_seen(&self) -> bool {
        self.banner_seen
    }

    pub fn panic_seen(&self) -> bool {
        self.panic_seen
    }

    /// The most complete fault seen so far, for callers that only need the last
    /// one (the engine keeps the full list in the run loop).
    pub fn fault(&self) -> Option<&RawFault> {
        self.fault.as_ref()
    }

    /// Feed one complete line; returns every observation it carries.
    pub fn feed(&mut self, line: &str) -> Vec<SerialObservation> {
        let mut observations = Vec::new();

        if !self.banner_seen && line.contains(BANNER) {
            self.banner_seen = true;
            observations.push(SerialObservation::Banner);
        }

        if let Some(rest) = line.split_once(EXCEPTION_PREFIX).map(|(_, rest)| rest) {
            if let Some(vector) = parse_vector(rest) {
                self.last_vector = Some(vector);
            }
        }

        if let Some(rest) = line.split_once(FAULT_PREFIX).map(|(_, rest)| rest) {
            if let Some(mut fault) = parse_fault(rest) {
                if let Some((number, name)) = &self.last_vector {
                    fault.vector_number = *number;
                    fault.vector = name.clone();
                }
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
}

/// `0x06 (#UD invalid opcode)` -> `(6, "#UD")`.
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

/// `0x0000000000100b3d cs=0x0008 rflags=0x... error=0x...` -> facts.
fn parse_fault(rest: &str) -> Option<RawFault> {
    let mut fields: Vec<&str> = rest.split_whitespace().collect();
    let rip_text = fields.first()?.to_string();
    let rip = parse_hex_u64(&rip_text)?;
    fields.remove(0);

    let mut cs = String::new();
    let mut rflags = String::new();
    let mut error_code = 0u64;
    for field in fields {
        if let Some(value) = field.strip_prefix("cs=") {
            cs = value.to_string();
        } else if let Some(value) = field.strip_prefix("rflags=") {
            rflags = value.to_string();
        } else if let Some(value) = field.strip_prefix("error=") {
            error_code = parse_hex_u64(value).unwrap_or(0);
        }
    }

    Some(RawFault {
        vector_number: 0,
        vector: String::new(),
        rip,
        rip_text,
        error_code,
        cs,
        rflags,
    })
}

/// Parse `0x...` / `0X...` / bare hex into a u64.
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

/// The x86-64 exception mnemonics the fixture prints (mirrors
/// `fixtures/refkernel/kernel.c:vector_name`).
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

/// Extract a `FAULT_RIP=0x...` address from a captured log (used by
/// `symbolicate --from-log`).
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

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
PrincessIDE reference kernel booted
[refkernel] multiboot: magic=0x36d76289 info=0x00119dc0 (multiboot 2)
[refkernel] cpu: long mode active, identity map 0x0..0x40000000

[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)
[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000000046 error=0x0000000000000000
[refkernel] PANIC: unhandled CPU exception, halting
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
    fn the_fixture_log_yields_banner_fault_and_panic() {
        let (observer, seen) = observe(LOG);
        assert!(observer.banner_seen());
        assert!(observer.panic_seen());
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[0], SerialObservation::Banner);

        let SerialObservation::Fault(fault) = &seen[1] else {
            panic!("expected a fault, got {:?}", seen[1]);
        };
        assert_eq!(fault.rip, 0x10_0b3d);
        assert_eq!(fault.rip_text, "0x0000000000100b3d");
        assert_eq!(fault.vector, "#UD");
        assert_eq!(fault.vector_number, 6);
        assert_eq!(fault.error_code, 0);
        assert_eq!(fault.cs, "0x0008");
        assert_eq!(fault.rflags, "0x0000000000000046");
        assert_eq!(seen[2], SerialObservation::Panic);

        let payload = fault.clone().into_payload(Some(SymbolicatedLocation {
            symbol: "refkernel_fault_probe".into(),
            file: "/root/PrincessIDE/fixtures/refkernel/kernel.c".into(),
            line: 100,
        }));
        assert_eq!(payload.regs.get("RIP").unwrap(), "0x0000000000100b3d");
        assert_eq!(payload.regs.get("CS").unwrap(), "0x0008");
        assert_eq!(payload.symbolicated.unwrap().line, 100);
        assert_eq!(payload.error_code, 0);
    }

    #[test]
    fn each_observation_is_reported_once() {
        let (_, seen) = observe(&format!("{LOG}{LOG}"));
        // Repeating the log must not duplicate banner/panic events.
        assert_eq!(
            seen.iter().filter(|o| **o == SerialObservation::Banner).count(),
            1
        );
        assert_eq!(
            seen.iter().filter(|o| **o == SerialObservation::Panic).count(),
            1
        );
    }

    #[test]
    fn a_page_fault_with_an_error_code_is_parsed() {
        let mut observer = SerialObserver::new();
        observer.feed("[refkernel] EXCEPTION: vector=0x0e (#PF page fault)");
        let observations = observer.feed(
            "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000010002 error=0x0000000000000002",
        );
        let SerialObservation::Fault(fault) = &observations[0] else {
            panic!("expected a fault");
        };
        assert_eq!(fault.vector, "#PF");
        assert_eq!(fault.vector_number, 14);
        assert_eq!(fault.error_code, 2);
    }

    #[test]
    fn a_garbled_fault_line_produces_no_event() {
        let mut observer = SerialObserver::new();
        assert!(observer.feed("[refkernel] FAULT_RIP=0xZZZZ cs=0x0008").is_empty());
        assert!(observer.fault().is_none(), "no address means no fault event");
    }

    #[test]
    fn hex_parsing_rules() {
        assert_eq!(parse_hex_u64("0x10"), Some(16));
        assert_eq!(parse_hex_u64("0X10"), Some(16));
        assert_eq!(parse_hex_u64("ff"), Some(255));
        assert_eq!(parse_hex_u64("0x"), None);
        assert_eq!(parse_hex_u64("0xGG"), None);
        assert_eq!(parse_hex_u64(""), None);
        assert_eq!(parse_hex_u64("0x0000000000100b3d"), Some(0x10_0b3d));
    }

    #[test]
    fn rip_can_be_recovered_from_a_captured_log() {
        assert_eq!(fault_rip_in_log(LOG), Some(0x10_0b3d));
        assert_eq!(fault_rip_in_log("nothing here\n"), None);
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
