//! PrincessIDE M11 — declarative plugins only (D29.2 ⑧).
//!
//! This layer reads manifests that *declare* panels, commands, templates and
//! themes, plus the capabilities they need, and rejects anything outside the
//! allow-list.  Loading native code is deliberately out of scope: Rust has no
//! stable ABI, so that would force a WASM or process boundary, and a plugin that
//! can run QEMU and a compiler deserves an auditable permission model first.
//!
//! Scaffolded by the main agent so the two W2 module agents never race on the
//! root `Cargo.toml`; the module owns this directory from here on.
//! See `docs/spec/34-module-dispatch.md` and `.scratch/main/mimo-m11-task.md`.
