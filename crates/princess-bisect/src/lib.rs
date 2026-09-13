//! PrincessIDE M10 — the bisect **orchestrator**, not a version-control system.
//!
//! The value here is not git plumbing: the engine can already build a kernel,
//! boot it under QEMU and attribute the exit.  Wrapping that as a `git bisect
//! run` predicate answers the question kernel developers actually ask — *which
//! commit broke the boot* — while `git` itself stays a thin CLI call (D29.2 ⑥:
//! never touch packfiles or the index format).
//!
//! Scaffolded by the main agent so the two W2 module agents never race on the
//! root `Cargo.toml`; the module owns this directory from here on.
//! See `docs/spec/34-module-dispatch.md` and `.scratch/main/mimo-m10-task.md`.
