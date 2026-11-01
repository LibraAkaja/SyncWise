# Safe Copy and Recovery

The engine/CLI safe-copy workflow and desktop execution/recovery flow use the same journaled, no-replace copy engine. See [execution.md](./execution.md) for desktop progress, SQLite persistence, and startup recovery behavior.

## CLI Workflow

Run from the repository root, using disposable sample folders first:

```powershell
cargo run -p syncwise-cli -- plan "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination"
cargo run -p syncwise-cli -- copy "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination"
```

Both commands produce JSON previews. `copy` is dry-run unless `--apply` is present. Planning performs a short-lived destination write/delete probe, but copies no user files and leaves no probe behind on success. A dry-run copy writes no run journal or snapshot.

After reviewing the missing files, explicitly apply:

```powershell
cargo run -p syncwise-cli -- copy "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination" --apply
cargo run -p syncwise-cli -- copy "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination" --apply --verify
```

The CLI creates a fresh plan for each `copy` invocation. It approves only missing-file `Copy` items; conflicts and advisory duplicate matches are not silently copied or skipped as identical. The engine can accept explicit per-item duplicate decisions from future review clients.

Applied output contains `plan`, `report`, `journalPath`, and `snapshotPath`. Journals and versioned run snapshots default to SyncWise's local application-data directory, outside the removable destination. `--journal <path>` overrides this location; keep it on a reliable local disk and never inside the source folder. The engine rejects source-directed journal paths.

Ctrl+C requests cooperative cancellation during scan/copy. Failed, stale-source, and cancelled applied runs return a nonzero exit status; the JSON report remains available when the engine can produce one.

## Restart and Resume

Use the paths reported by the original applied run:

```powershell
cargo run -p syncwise-cli -- recover "D:\SyncWiseSamples\Destination" "C:\LocalSyncState\journal.jsonl"
cargo run -p syncwise-cli -- resume "C:\LocalSyncState\run-example.syncwise-run.json"
cargo run -p syncwise-cli -- resume "C:\LocalSyncState\run-example.syncwise-run.json" --apply
```

The example state paths are placeholders; use `journalPath`/`snapshotPath` from your output. If the process was killed before printing output, inspect SyncWise's local application-data `runs` directory for saved `*.syncwise-run.json` files. Recovery also lists `savedRunPaths` next to the selected journal.

- Recovery and dry-run resume inspect state without modifying the journal or deleting files.
- Applied resume uses the original plan, scan-time size/mtime, decisions, and run ID. It does not rescan and automatically approve new files.
- Items journaled `Done` are not copied again. A repeated successful resume transfers nothing.
- Failed, cancelled, and interrupted approved items are retried from byte zero into a new temporary file. Partial byte-range continuation is not implemented.
- Changed source files become `NeedsRescan`. Generate and review a new plan rather than bypassing that result.
- A final path occupied after preview or after a crash is skipped, never replaced. If finalization succeeded just before a crash, an unjournaled final file is conservatively treated as a conflict, not assumed to be ours.
- A per-journal exclusive lock prevents concurrent execution/repair. Applied resume truncates only an incomplete trailing record before appending; corrupt newline-terminated records block recovery rather than being silently discarded.
- Orphan temporary files are reported and retained for review. Resume does not automatically delete them or unrelated files. Local snapshots contain absolute paths; keep them private and do not edit them.

## Automated Safety Matrix

Run `cargo test --workspace`. Tests use generated temporary folders, not user libraries.

| Plan Safety Case | Evidence |
| --- | --- |
| 1. Same name, different content | CLI and engine conflict tests preserve destination bytes; no identical label is inferred. |
| 2. Destination appears after preview | Engine test and paused-worker subprocess test exercise the conflict before/during transfer and successful later retry. |
| 3. Source changes after scan | Engine tests and saved-run CLI resume return `NeedsRescan` without creating a final copy. |
| 4. Kill mid-copy | A real subprocess is killed after its first chunk. Only a temporary file remains; CLI recovery finds it and resume completes the approved run. |
| 5. Drive/I/O failure | The production chunk-transfer boundary receives injected `NotConnected` and `StorageFull` writer errors. A real Windows exclusive source lock also verifies Failed journaling, retention of prior Done items, and successful CLI retry after unlock. Physical drive removal is not simulated by these tests. |
| 6. Source unchanged | CLI fixtures hash the complete source tree before/after plan, copy, and resume operations; the killed-worker case also checks the tree hash. |
| 7. Planner never overwrites | `proptest` generates file/directory collision fixtures; copy actions never target occupied paths. Runtime no-clobber finalization independently enforces this. |
| 8. Overlapping roots | Engine tests and CLI refusal cover identical roots; engine tests also cover both nesting directions. |

Additional tests cover nullable numeric bindings, reserved names, cancellation, short-write errors, torn journal tails, corrupt records, source-directed journals, and idempotent resume. Unix-only symlink tests are not run on Windows. The 20,000-files-per-side benchmark remains an opt-in ignored test.

## Hardware Validation Still Required

The automated checks do not certify physical power-loss behavior or a removable filesystem's flush semantics. Before closing the complete hardware safety gate, use disposable data on real NTFS and exFAT media:

1. Copy multiple files with the default local journal and confirm at least one item completes.
2. Unplug the destination during a later large-file transfer.
3. Confirm the command fails without creating a truncated final-name file, and the local snapshot/journal survive.
4. Reconnect at the same path, inspect recovery, and resume the original saved run explicitly.
5. Verify completed files were not recopied, remaining source/destination hashes match, and the source tree is unchanged.

If the mount path/drive letter changes, restore the original root before resume; automatic drive identity remapping is not implemented. Metadata revalidation is not a content hash and cannot detect same-size edits that deliberately preserve timestamps. Hostile concurrent directory/link swaps and full machine power-cut durability are not certified by this test suite. Real-drive testing and broader platform hardening remain release gates.