# SyncWise

SyncWise is a Windows-first desktop app for reviewing and copying files from a source folder to a destination folder. It scans and compares files, presents possible matches as advisory evidence, and copies only files the user explicitly approves.

**Safety model:** SyncWise does not delete source files or replace destination files. Similarity scores never authorize a copy. Copy execution uses a journal, same-directory temporary files, revalidation, and no-replace finalization. Optional BLAKE3 verification is available before a temporary copy is finalized.

## Project status

The scanning, review, matching, and journaled copy workflows are implemented, with automated Rust and frontend tests. Physical removable-drive unplug/reconnect behavior on NTFS and exFAT, native desktop click-through, and release packaging still require validation. This is not yet a certified data-recovery or backup product; keep an independent backup of important data.

## Run from source

Requirements: Rust stable, Node.js 22, pnpm 11, and the Windows desktop build prerequisites for Tauri 2.

```powershell
git clone https://github.com/LibraAkaja/SyncWise.git
cd SyncWise
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop tauri dev
```

Choose source and destination folders in the app, scan, review each proposed action, and confirm the plan before starting a sync. Saved folder-pair profiles and run history are stored locally in the app-data directory. See [docs/review.md](./docs/review.md) for the review flow and local-state details.

## CLI preview and copy

The Rust CLI can preview a plan without launching the desktop app. Use disposable folders first:

```powershell
cargo run -p syncwise-cli -- plan "C:\Samples\Source" "D:\Samples\Destination"
cargo run -p syncwise-cli -- copy "C:\Samples\Source" "D:\Samples\Destination"
```

`copy` is a dry run unless `--apply` is passed. Applied copies select missing-file items only; matching suggestions do not count as approval.

```powershell
cargo run -p syncwise-cli -- copy "C:\Samples\Source" "D:\Samples\Destination" --apply --verify
```

Review [docs/safe-copy.md](./docs/safe-copy.md) before applying copies or attempting recovery. It documents journaling, retries, limitations, and the automated safety matrix.

## Development checks

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -j 1
pnpm --dir apps/desktop install --frozen-lockfile
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
```

The labeled filename corpus can be evaluated separately:

```powershell
cargo test -p syncwise-engine --test matching_corpus -- --nocapture
```

Large scan and matching performance checks are opt-in ignored tests; see [docs/matching.md](./docs/matching.md). See [docs/release-validation.md](./docs/release-validation.md) for the manual validation matrix. Do not mark hardware or manual cases passed until they have been run and their evidence recorded.

## Release testing

Before an official release, every item in [docs/release-test-plan.md](./docs/release-test-plan.md) must pass. In short:

1. Automated gate: fmt, clippy, Rust and frontend tests, and the matching corpus.
2. Safety checks: no overwrite, late destination conflicts, `NeedsRescan`, interrupted copies, drive removal, an untouched source, hostile filenames, and no network use.
3. Hardware: unplug tests on NTFS and exFAT drives.
4. Performance: 20k-file scan and matching, memory, cancel latency and copy throughput.
5. Installer: a build and click-through on a clean Windows VM.

Record results in [docs/release-validation.md](./docs/release-validation.md).

## Matching and privacy

Candidate matches and audio tags are advisory, not proof of identity. Exact content identity is established only by equal full BLAKE3 hashes for files actually compared; candidate retrieval does not compare every possible pair. Matching limitations and corpus results are documented in [docs/matching.md](./docs/matching.md).

SyncWise has no account or cloud-sync workflow. Scan, matching, and copy operations are local. Paths and file metadata are stored in local app state and run snapshots; keep that state private. See [docs/architecture.md](./docs/architecture.md) and [docs/execution.md](./docs/execution.md).
