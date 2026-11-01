# Desktop Execution and Recovery

The desktop execution flow starts only after review preflight has been confirmed for the current decision revision. It copies explicitly approved files through the engine's journaled, no-replace copy path; conflicts and advisory scores never authorize a copy.

## Progress and Reports

- The UI receives throttled per-file byte progress and an overall byte/file summary.
- Cancellation is cooperative. The current temporary copy is discarded safely if it has not been finalized.
- The final report records completed files, bytes written, and files needing attention. Per-file failures are listed, with a bounded list for large runs.
- Failed or cancelled runs can retry only unfinished approved items. Previously completed items are not copied again.

## Local Persistence and Restart

The app-data `syncwise.sqlite` database is managed by versioned SQLite migrations. It stores scans, file indexes, candidate matches, decisions, runs, run items, and a queryable copy of journal entries. The durable JSONL file journal and the original plan snapshot remain the source of truth for resuming a copy.

A run and its approved items are committed to SQLite before copying starts. On startup, SyncWise reads the journals for resumable runs and reconciles each run item and the SQLite journal table before showing recovery actions. Corrupt or unreadable journal state is reported; it is not replaced with an empty journal. Interrupted runs can be inspected without deleting temporary files, resumed, or dismissed. Temporary files are retained for review.

Database and journal files live in the app-data directory, outside the selected source. They contain local paths and metadata; keep the local state private. A run whose status cannot be saved is reported as an error and remains available for recovery if the database can be opened later.

## Verification and Remaining Gate

The engine recovery test simulates an interruption after one completed file, verifies startup reconciliation, and resumes the remaining files. Desktop component tests cover progress, file-level errors, cancellation, retry, and unavailable final reports.

These automated checks do not certify physical removable media. Milestone 4's NTFS/exFAT unplug, reconnect, and resume validation and Milestone 7's end-to-end hardware gate remain open. Use disposable files and media for manual validation; do not treat test results as hardware certification.
