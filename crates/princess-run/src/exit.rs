//! Exit attribution — contract §6 rule 4 and **D9**.
//!
//! The engine is the only thing allowed to decide *why* a run ended; the UI must
//! never guess.  This module assembles the evidence, then hands it to
//! [`princess_core::classify_exit`] — the single implementation — so the answer
//! cannot drift between backends.
//!
//! ## The trap D9 was written for
//!
//! `-no-reboot` makes a triple fault terminate QEMU with **exit code 0**, which
//! is bit-for-bit the same status as an ACPI power-off.  Measured in this
//! workspace:
//!
//! ```text
//! triple-fault probe, -d int,cpu_reset : exit 0, "Triple fault" ×1
//! triple-fault probe, -d int only      : exit 0, "Triple fault" ×0
//! ACPI shutdown probe                  : exit 0, "Triple fault" ×0
//! ```
//!
//! So the exit code is *never* used as fault evidence: the `-D` debug log is.
//! [`QemuEvidence::triple_fault`] is set only from that log (D9).
//!
//! Precedence is fixed by `core` (do not re-derive it here):
//! `cancelled > timeout > triple-fault > guest-shutdown > exit code`.

use std::io::Read;
use std::path::Path;

use princess_core::traits::{classify_exit, ExitFacts};
use princess_core::types::ExitReason;

/// Everything observed about one finished QEMU process, before classification.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QemuEvidence {
    /// Process exit code (`None` when it died from a signal).
    pub exit_code: Option<i32>,
    /// Terminating signal.
    pub signal: Option<i32>,
    /// The engine's deadline fired and the process group was killed.
    pub timed_out: bool,
    /// Cancellation was requested (`run:stop` / `op:cancel`).
    pub cancelled: bool,
    /// `Triple fault` was read from the `-d int,cpu_reset` log (D9).
    pub triple_fault: bool,
    /// The guest's own serial output proves it asked the machine to stop.
    pub guest_shutdown: bool,
}

impl QemuEvidence {
    /// Assemble the facts, applying the one rule that is easy to get wrong: a
    /// clean exit is only a *shutdown* when nothing contradicts it, and a
    /// triple fault is only *known* from the debug log.
    pub fn into_facts(self) -> ExitFacts {
        ExitFacts {
            exit_code: self.exit_code,
            signal: self.signal,
            timed_out: self.timed_out,
            cancelled: self.cancelled,
            guest_powered_off: self.guest_shutdown
                && !self.timed_out
                && !self.cancelled
                && !self.triple_fault,
            triple_fault: self.triple_fault,
        }
    }

    /// The attributed reason, via the engine-wide implementation.
    pub fn reason(&self) -> ExitReason {
        classify_exit(&self.clone().into_facts())
    }

    /// A human-readable justification for the attribution, for the report and
    /// for the `ide` log line.  Never a guess: it names the evidence used.
    pub fn justification(&self) -> String {
        let reason = self.reason();
        let basis = match reason {
            ExitReason::Killed if self.cancelled => "cancellation was requested",
            ExitReason::Killed => match (self.exit_code, self.signal) {
                (_, Some(sig)) => return format!("killed: signal {sig} (no cancellation was requested)"),
                (Some(code), None) => return format!("killed: exit code {code} with no shutdown evidence"),
                (None, None) => "no exit status was available",
            },
            ExitReason::Timeout => "the deadline fired before the machine stopped",
            ExitReason::TripleFault => "`Triple fault` appeared in the -d int,cpu_reset log",
            ExitReason::GuestShutdown => {
                if self.guest_shutdown {
                    "the guest requested power-off and QEMU exited cleanly"
                } else {
                    "a clean exit with no reset and no fault"
                }
            }
        };
        format!("{}: {basis}", reason.as_str())
    }
}

/// Does a QEMU `-d …` debug log contain the one line that means "the CPU could
/// not deliver an exception"?  D9: this text exists **only** in the
/// `cpu_reset` output.
pub fn detect_triple_fault(debug_log: &str) -> bool {
    debug_log.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("triple fault")
    })
}

/// Read a debug log, tolerating a missing file (an unreadable log is *no
/// evidence*, never a positive detection).
pub fn triple_fault_in_file(path: &Path) -> bool {
    let mut text = String::new();
    match std::fs::File::open(path) {
        Ok(mut file) => {
            // The log can be large; the flag appears in the reset section, and
            // reading the whole thing is what "look at the evidence" means here.
            if file.read_to_string(&mut text).is_err() {
                // Non-UTF-8 bytes are possible in QEMU dumps; retry lossily.
                text.clear();
                let mut raw = Vec::new();
                if std::fs::File::open(path)
                    .and_then(|mut f| f.read_to_end(&mut raw))
                    .is_ok()
                {
                    text = String::from_utf8_lossy(&raw).into_owned();
                }
            }
        }
        Err(_) => return false,
    }
    detect_triple_fault(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(exit_code: Option<i32>) -> QemuEvidence {
        QemuEvidence {
            exit_code,
            ..QemuEvidence::default()
        }
    }

    #[test]
    fn a_clean_exit_without_fault_evidence_is_a_guest_shutdown() {
        let e = evidence(Some(0));
        assert_eq!(e.reason(), ExitReason::GuestShutdown);
        assert!(e.justification().contains("clean exit"), "{}", e.justification());
    }

    /// The D9 trap, pinned as a test: exit 0 + a triple fault in the log is a
    /// triple fault, **not** a shutdown.
    #[test]
    fn exit_zero_with_a_triple_fault_in_the_log_is_not_a_shutdown() {
        let e = QemuEvidence {
            exit_code: Some(0),
            triple_fault: true,
            ..QemuEvidence::default()
        };
        assert_eq!(e.reason(), ExitReason::TripleFault);
        assert!(e.justification().contains("Triple fault"), "{}", e.justification());
        // ... and the shutdown bit must not have been manufactured.
        assert!(!e.into_facts().guest_powered_off);
    }

    #[test]
    fn the_precedence_is_cancelled_then_timeout_then_triple_then_shutdown() {
        let all = QemuEvidence {
            exit_code: Some(0),
            signal: None,
            timed_out: true,
            cancelled: true,
            triple_fault: true,
            guest_shutdown: true,
        };
        assert_eq!(all.reason(), ExitReason::Killed, "cancelled outranks everything");

        let timed = QemuEvidence {
            cancelled: false,
            ..all.clone()
        };
        assert_eq!(timed.reason(), ExitReason::Timeout, "timeout outranks fault");

        let triple = QemuEvidence {
            timed_out: false,
            ..timed
        };
        assert_eq!(triple.reason(), ExitReason::TripleFault);

        let shutdown = QemuEvidence {
            triple_fault: false,
            ..triple
        };
        assert_eq!(shutdown.reason(), ExitReason::GuestShutdown);
    }

    #[test]
    fn a_signal_death_is_killed_and_says_which_signal() {
        let e = QemuEvidence {
            exit_code: None,
            signal: Some(9),
            ..QemuEvidence::default()
        };
        assert_eq!(e.reason(), ExitReason::Killed);
        assert!(e.justification().contains("signal 9"), "{}", e.justification());
    }

    #[test]
    fn a_nonzero_exit_is_killed_and_never_a_shutdown() {
        let e = evidence(Some(1));
        assert_eq!(e.reason(), ExitReason::Killed);
        assert!(!e.clone().into_facts().guest_powered_off);
        assert!(e.justification().contains("exit code 1"), "{}", e.justification());
    }

    /// A guest that printed a shutdown line but timed out is a *timeout*: the
    /// deadline is the more specific truth, and `classify_exit` decides it.
    #[test]
    fn a_timeout_beats_a_guest_shutdown_claim() {
        let e = QemuEvidence {
            exit_code: Some(0),
            guest_shutdown: true,
            timed_out: true,
            ..QemuEvidence::default()
        };
        assert_eq!(e.reason(), ExitReason::Timeout);
    }

    #[test]
    fn a_cancel_beats_a_triple_fault() {
        let e = QemuEvidence {
            exit_code: None,
            signal: Some(9),
            cancelled: true,
            triple_fault: true,
            ..QemuEvidence::default()
        };
        assert_eq!(e.reason(), ExitReason::Killed);
    }

    /// D9 in the parser itself: only the `cpu_reset` channel prints this.
    #[test]
    fn triple_fault_detection_is_line_and_case_insensitive() {
        assert!(detect_triple_fault("Servicing hardware interrupt 0\nTriple fault\nCPU Reset (CPU 0)\n"));
        assert!(detect_triple_fault("qemu: TRIPLE FAULT\n"));
        assert!(detect_triple_fault("triple fault"));
        assert!(!detect_triple_fault(
            "v=06 e=0000 i=0 cpl=0 IP=0008:0000000000100b3d\n"
        ));
        assert!(!detect_triple_fault(""));
        // A near-miss must not fire.
        assert!(!detect_triple_fault("no triplefault here\n"));
    }

    #[test]
    fn a_missing_debug_log_is_no_evidence_not_a_positive() {
        assert!(!triple_fault_in_file(Path::new(
            "/definitely/not/here/qemu-debug.log"
        )));
        let dir = std::env::temp_dir().join(format!("princesside-tf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let hit = dir.join("hit.log");
        std::fs::write(&hit, "…\nTriple fault\nCPU Reset (CPU 0)\n").unwrap();
        assert!(triple_fault_in_file(&hit));

        let miss = dir.join("miss.log");
        std::fs::write(&miss, "v=0e e=0000 i=0\n").unwrap();
        assert!(!triple_fault_in_file(&miss));

        // Non-UTF-8 bytes must not make the reader panic or lie.
        let binary = dir.join("binary.log");
        std::fs::write(&binary, b"\xff\xfe garbage \xff\nTriple fault\n").unwrap();
        assert!(triple_fault_in_file(&binary));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
