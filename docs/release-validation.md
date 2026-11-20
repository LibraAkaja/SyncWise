# Windows Release Validation Matrix

Use this matrix with disposable folders and media only. It is a test procedure, not a certification. Do not mark a case passed based on a unit test when the row requires a desktop or physical-device check.

## Record the test environment

- SyncWise commit/build:
- Windows version:
- SyncWise build type (debug/release/installer):
- Source volume/filesystem:
- Destination volume/filesystem:
- Removable device model/capacity (if applicable):
- Tester/date:
- Evidence location (logs, screenshots, hashes; exclude private library data):

For each applicable case, record **Pass**, **Fail**, or **Not run**, plus notes and evidence. A failed safety case blocks release until fixed and rerun. Keep test data disposable and confirm the source tree is unchanged before and after each copy/recovery scenario.

## Desktop workflow

| Case | Procedure | Expected result | Status / evidence |
| --- | --- | --- | --- |
| Folder selection | Use both native folder pickers with valid sibling folders. | Canonical paths appear with correct FROM/TO labels; scan can start. | Not run |
| Invalid/overlapping roots | Select the same folder, then nested source/destination folders. | Validation refuses each overlapping pair; no scan or copy starts. | Not run |
| Saved profile | Save a folder pair, restart the app, load the profile. | Paths persist locally and are revalidated before scanning; deleted/unavailable paths show an error. | Not run |
| Scan cancellation | Start scanning a disposable large tree and cancel promptly. | UI remains responsive, cancellation is reported, and no review plan is prepared from partial results. | Not run |
| Unreadable/locked entry | Include an entry that cannot be read while scanning. | Scan continues and reports the affected path; it does not silently treat an incomplete scan as a complete approved plan. | Not run |
| Review and confirmation | Review a mix of missing files, candidates, and conflicts using keyboard only. Attempt confirmation with a blocking preflight issue. | Decisions are explicit; blockers cannot be acknowledged away; sync starts only after the current revision is confirmed. | Not run |
| Theme and accessible controls | Exercise system/light/dark themes, keyboard focus, filters, empty states, and a screen reader. | Text and controls remain legible; focus and status updates are perceivable; actions do not require a pointer. | Not run |
| Error and retry report | Cause a disposable destination failure, then resolve it and retry. | Per-file failures are visible; retry skips completed items and does not replace existing files. | Not run |

## Filesystem and safety cases

| Case | Procedure | Expected result | Status / evidence |
| --- | --- | --- | --- |
| Same name, different content | Put different bytes at the same relative path on each side. | Existing destination bytes remain unchanged; item is a conflict, never an overwrite or identical match. | Not run |
| Destination appears after preview | Create the destination path after reviewing the plan but before copy/finalization. | Finalization does not replace it; run reports a conflict/skip and remains recoverable. | Not run |
| Source changes after scan | Change a source file after planning and before copy or resume. | Item is marked `NeedsRescan`; changed bytes are not copied under the old approval. | Not run |
| Cancellation during copy | Cancel while a large disposable file is copying. | No truncated final-name file is exposed; temp/journal recovery is available; completed files remain done. | Not run |
| NTFS unplug/reconnect | With disposable data and local journal, unplug NTFS media during a transfer; reconnect at the same path and resume. | Unfinished work is retryable, completed files are not recopied, existing files are not replaced, and source bytes are unchanged. | Not run |
| exFAT unplug/reconnect | Repeat the NTFS procedure on exFAT media. | Same safety outcomes as NTFS; record any filesystem-specific limitations. | Not run |
| Reserved/invalid names | Include Windows reserved names, trailing dot/space, and invalid destination components. | Unsafe paths are blocked or reported, never crash or escape the chosen destination. | Not run |
| Unicode and long paths | Include composed/decomposed Unicode names and deeply nested long paths. | Paths complete correctly or produce explicit per-path errors; no silent path substitution. | Not run |
| Source immutability | Hash the complete source tree before and after scan, plan, copy, failure, and resume checks. | Source tree content and names are unchanged. | Not run |
| Network activity | Monitor a full local scan/review/copy run with network monitoring. | No outbound network requests are made by SyncWise during the run. | Not run |

## Performance and release artifacts

| Check | Procedure | Target / evidence | Status |
| --- | --- | --- | --- |
| Scan benchmark | Run the documented 20,000-files-per-side release benchmark on SSD. | Under 15 seconds; record CPU, disk, OS, and elapsed time. | Not run |
| Matching benchmark | Run the documented 20,000-by-20,000 blocked-matching release benchmark without full-library hashing. | Under 20 seconds; record candidate count and elapsed time. | Not run |
| Review responsiveness | Load 20,000 result rows and exercise scroll, keyboard navigation, and cancel. | Smooth bounded rendering; cancellation responds within 1 second. Record observed timings. | Not run |
| Memory | Measure a 50,000-file index workload. | Peak process memory below 500 MB; record measurement method. | Not run |
| Copy throughput | Copy a large disposable set and compare against a raw copy on the same media. | Within roughly 10% of raw drive throughput; record verification setting and filesystem. | Not run |
| Windows installer | Install the CI-produced installer on a clean Windows account/VM, launch, scan, and uninstall. | Installer builds, launches, and removes cleanly; record artifact hash and Windows version. | Not run |

Performance results are environment-specific. The existing ignored Rust benchmarks are automated measurements, but do not replace the desktop, media, installer, or screen-reader checks above.
