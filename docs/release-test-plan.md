# SyncWise Release Test Plan

Run every step before tagging `v1.0.0`. Record results in [release-validation.md](./release-validation.md).
Rule: any failure in sections 2–4 blocks release. Never test against real data; use disposable copies.

## 1. Automated gate (about 10 min)

From the repo root (use `-j 1` if memory is tight):

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. Matching corpus test, same as CI (see `.github/workflows/ci.yml`).
5. In `apps\desktop`: `pnpm install --frozen-lockfile`, `pnpm test`, `pnpm build`.
6. Confirm CI is green on `main` (windows-latest).

Pass: all commands exit 0.

## 2. Non-negotiable safety checks

Make a small source folder and an empty destination folder on a scratch drive. Hash the source first:
`Get-ChildItem -Recurse -File <src> | Get-FileHash | Export-Clixml before.xml`

| # | Check | How | Pass |
|---|-------|-----|------|
| 1 | No overwrite | Put `a.mp3` in both sides with different content. Scan, approve, copy. | Destination file is unchanged; never labeled identical. |
| 2 | Late destination file | Scan and preview. Before executing, create the planned target in the destination by hand. Execute. | Item is skipped as a conflict; your file is untouched. |
| 3 | Source changed after scan | Scan, then edit or touch a source file. Execute. | Item shows `NeedsRescan`; not copied. |
| 4 | Interrupted copy | Copy a large file (1 GB+). Kill the app mid-copy (Task Manager). Reopen. | No truncated file under its final name; recovery lists the app-owned temp file; retry completes. |
| 5 | Drive removal | Copy many files to a USB drive; unplug mid-run. Replug and reopen. | Completed items stay done; unfinished items are retryable and succeed on retry. |
| 6 | Source untouched | After all tests, re-hash the source and compare with `before.xml`. | Byte-identical. |
| 7 | Planner properties | `cargo test --workspace` (planner and property tests). Also try overlapping source/destination selections in the UI. | No action targets an existing path; overlapping selections are refused. |
| 8 | Hostile names | Make files with Unicode names, emoji, paths over 260 chars, and reserved names (`CON`, `NUL`, trailing dot or space). Scan and copy. | Each completes or is reported as an error; no crash. |
| 9 | No network | Run a full scan, match and copy with Wireshark or `netstat -b`, or with the network adapter disabled. | Zero outbound requests; the app works offline. |

## 3. Hardware validation (manual)

1. Repeat checks 4 and 5 on **NTFS** and **exFAT** USB drives (and FAT32 if supported).
2. Copy files larger than 4 GB to each filesystem and note the behavior.
3. Test removable-drive letter changes between runs.
4. Test a read-only destination and a full destination; the app should report, not crash.

## 4. Functional click-through

On the built installer, not dev mode:

1. Pick folders, scan, view matches, approve a subset, preview, execute, view the run in history.
2. Cancel during scan and during copy; cancel should respond within 1 s.
3. Save, load and delete a folder-pair profile; load one whose folder was deleted (should be revalidated).
4. Decisions memory and reset; theme switching (light, dark, system); keyboard-only navigation.
5. Within-side duplicates view; a non-audio file type; empty folders.
6. CLI: run the documented commands in [safe-copy.md](./safe-copy.md).
7. Accessibility checklist in [accessibility.md](./accessibility.md).

## 5. Performance targets

Generate fixtures with 20,000 and 50,000 files (script or the benchmark tests).

| Target | How to measure | Goal |
|--------|----------------|------|
| Scan/index 20k files per side | Time the scan on an SSD | < 15 s |
| Match 20k × 20k without hashing | `cargo test --release -- --ignored` (benchmarks) | < 20 s |
| 20k-row review table | Scroll in the UI; start a scan and cancel | Smooth; cancel < 1 s |
| Peak memory, 50k files | Task Manager or `Get-Process` peak working set | < 500 MB |
| Copy throughput | Copy a 5 GB set with the app, then with Explorer or robocopy | Within ~10% |

## 6. Installer and clean-machine test

1. `pnpm tauri build` in `apps\desktop`; the output is in `target\release\bundle\`.
2. On a clean Windows 10 and 11 VM, install, launch, run section 4, uninstall.
3. Check that SmartScreen or signing behaves as expected, and that no dev tools are required.
4. Confirm the app name, version and icon are no longer template defaults.
5. Upgrade test: install over the previous build with an existing database; history and profiles survive.

## 7. Release sign-off

1. All boxes in "Non-Negotiable Release Checks" in `milestones.md` are ticked.
2. The `release-validation.md` matrix has no "Not run" entries and the environment is recorded.
3. Performance numbers are recorded; any miss has a written decision.
4. Tag `v1.0.0`, upload the installer to a GitHub Release, and attach checksums.