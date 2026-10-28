# Review Workflow

Milestone 6 connects Setup scans to a Rust-owned review plan. It prepares decisions and confirmation; it does not expose a desktop copy command.

## Available Workflow

- Choosing both folders still validates automatically. A completed scan builds its match/preview plan on the Rust background worker and opens Review. Cancelled or failed scans do not open a review plan.
- The header retains labeled source/destination roots. Full roots are shown again in preflight confirmation.
- Missing, Possible duplicates, Already present, Conflicts, and Errors tabs have counts and source-path search. Plan-level scan/preflight issues appear in Errors as well as file errors. Already present remains empty until content identity is implemented; similar names are not labeled identical.
- The fixed-height TanStack Virtual table renders only a viewport of rows, not the complete library. Arrow keys, Home/End, and Page Up/Down select files; tab categories support arrow navigation. Selection exposes side-by-side paths, candidate scores/reasons, and shared-destination groups.
- Each copyable item has explicit approve, reject, or defer choices. Undo reverses decisions before confirmation. Returning to Setup retains the latest server decision revision.
- Bulk approval is restricted to missing files. Current name-only duplicate tiers cannot be bulk-approved; each needs an individual approval. Bulk defer affects only undecided visible results. Conflicts/errors remain skipped and cannot be approved.
- Rejections are remembered locally for the canonical root pair, source path/size/mtime, and candidate destination paths/size/mtime. Changed evidence no longer reuses the old rejection. Resetting memory affects future scans, not current decisions. There is no cloud synchronization.

## Rust Ownership and Preflight

Frontend decision calls send only plan ID, item IDs, revision, choices, and bulk intent. The server rejects stale revisions, unknown/duplicate IDs, conflict approvals, and batch approval of name-only candidates. Changes are committed only after rejection memory is atomically saved to local application data. The source tree is never a review-state storage location; if app data lies inside a broadly selected source, saving is refused rather than modifying the source.

Preflight runs on a blocking worker, rechecks roots, approved source size/mtime, destination absence, destination writability, and free space for all approved files, including individually approved possible duplicates. The summary includes approved/pending/rejected/deferred/conflict counts and blocking issues. Pending decisions must be decided or deferred, blocking issues must be resolved, and at least one file must be approved before plan confirmation. Checking the acknowledgement cannot override a blocking issue.

The confirmation dialog names both complete roots and requires a checkbox plus an explicit **Confirm plan** action. It reruns preflight before marking preparation confirmed. **Start Sync stays disabled** because physical removable-drive validation is pending and desktop execution belongs to Milestone 7. Confirmation does not write user files, create a copy journal, or authorize a hidden copy call. Writability checks create/remove only a temporary destination probe.

Rejection memory is stored as `rejected-matches.json` in Tauri's app-data directory. It contains paths and metadata, not content; keep it private. SQLite migrations and saved profiles remain Milestones 7/9 work.

## Verification

```powershell
cargo test --workspace -j 1
cargo clippy --workspace --all-targets -- -D warnings
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
```

The UI suite verifies real virtualizer boundedness with 20,000 rows, keyboard selection and preflight preparation, reversible decisions, evidence display, acknowledgement, blocking issues, and absence of copy commands. Windows CI runs these tests alongside existing safety/matching gates.

Rust tests cover revision/ID validation, atomic rejection-memory updates, individually reviewed duplicate approval, live destination conflicts, source-read-only state protection, and remembered rejection restoration. Browser checks exercised the scan-to-review event flow with an isolated mock IPC bridge: 20,000 rows rendered 15 data rows plus a header at 1440x1000, keyboard preparation confirmed without copying, and 390px mobile layout had no page overflow. The table intentionally scrolls horizontally on narrow screens. Screenshots were checked on desktop/mobile.

These are not a native OS-dialog end-to-end test or physical-drive certification. The actual Rust commands compile and are unit-tested; a native scan/review click-through remains a useful manual check. The initial parallel workspace build exhausted the local Windows pagefile; rerunning the complete suite with one Cargo build job passed. The two large scan/matching benchmarks remain opt-in ignored tests.