# Architecture Boundaries

SyncWise keeps file operations and decisions in Rust so the UI cannot become an authority for filesystem safety.

- `crates/syncwise-engine` owns domain types, path validation, scanning, planning, matching, and copying. It must not depend on Tauri.
- `crates/syncwise-cli` is a thin harness for exercising engine behavior outside the UI, especially safety-critical planning and copy flows.
- `apps/desktop/src-tauri` owns Tauri commands, native dialogs, capabilities, and progress events. Commands validate IPC input and delegate domain rules to the engine.
- `apps/desktop/src` owns presentation and user decisions. It passes selections and plan item IDs to Rust; it does not read or write user files directly.
- `docs` holds architecture decisions, algorithm notes, test matrices, and benchmarks.
- `fixtures` holds deterministic generators and small labeled matching data. Large generated files are created during tests and are not committed.

Engine types are the source of truth for the Rust-to-TypeScript contract. The engine tests export bindings into `apps/desktop/src/types/generated`; do not edit those generated files by hand.

See [Safe Copy and Recovery](safe-copy.md) for CLI usage, saved-run retry semantics, the automated safety matrix, and outstanding hardware validation.

See [Advisory Filename Matching](matching.md) for candidate indexing, evidence tiers, the labeled-corpus gate, and scaling limits. Matching emits suggestions only; approval remains a separate decision.

See [Review Workflow](review.md) for Rust-owned decisions, local rejection memory, virtualized keyboard review, and the gated preflight confirmation.