# Advisory Filename Matching

Milestone 5 introduced name-based candidate review. Milestone 8 now extracts audio metadata when available, adds tag/duration evidence, and performs staged content hashes for name-retrieved, same-size candidates. Metadata parser failures fall back to filename matching. Evidence remains advisory and never approves copies or deletes files.

## Usage

```powershell
cargo run -p syncwise-cli -- match "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination"
```

`match` scans metadata and prints JSON with matching candidates, groups, work counters, and per-side scan issues. It is read-only and supports Ctrl+C. `plan` also includes the matching report: occupied destination paths remain conflicts, plausible name matches become `possible_duplicate`, and unmatched files remain proposed copies. A candidate's `suggested` flag is a display/assignment hint, not an approval. CLI `copy --apply` still selects only missing-file Copy items; all suggestions need an explicit review decision.

Rust is the source of truth for the report. Engine tests generate TypeScript contracts, including `MatchingOptions`, `MatchingReport`, `CandidateMatch`, `MatchLabel`, and `MatchGroup`. Existing saved plans without the optional matching report remain deserializable.

## Pipeline

1. Keep original paths for display and filesystem operations. Comparison uses the filename stem, Unicode NFC normalization, full Unicode case folding, punctuation/space normalization, and one leading track-number prefix (`01 -` or `01.`).
2. Recognize ` by ` and spaced dash separators (including en/em dashes). Dash-separated names retain both artist/title orientations because filenames do not reliably specify their order.
3. Extract remix, live, acoustic, instrumental, remaster/remastered, radio edit, demo, cover, feat/ft/featuring, version, sped up, and slowed tokens. Differing version sets cap name/tag confidence at Uncertain; they never prove a recording is the same.
4. Build an inverted destination index over exact core titles, words, and Unicode character trigrams. Audio extensions share a family; general files compare only within the same extension. Retrieve the top 20 candidates by shared-key votes, giving exact-title keys priority.
5. When a supported audio file can be parsed, retain its track title, artist, album artist, album, and duration. Unsupported, malformed, or untagged audio returns no metadata and leaves filename matching available. A strict `Very likely` tier requires normalized track-title and preferred artist-tag agreement, duration within max(2 seconds, 1.5%), and no filename version conflict.
6. Score retained candidates with core-title similarity (weight 0.50), filename-artist similarity (0.20), and size plausibility (0.10), plus audio tag similarity (0.20) and duration similarity (0.10) when available. Missing signals are excluded and the score is renormalized. These weights are ranking aids, not identity claims.
7. For retrieved candidates with equal file sizes, compare BLAKE3 partial hashes over file length plus the first and last 64 KiB. Only partial matches proceed to full streaming BLAKE3 hashes. Equal full hashes produce `Identical`; equal size or partial hashes alone never do. Hash read failures are shown as unavailable evidence. The matcher caches each successfully computed stage per file during planning.
8. Emit retained candidates with scores and evidence/penalty reasons. Greedy best-score assignment suggests at most one source per destination and vice versa. Sources choosing the same best destination are explicitly grouped for review; no candidate is discarded merely because another source has a better score.

Deterministic path ordering breaks score/retrieval ties. Matching can be cancelled between indexed/scored files. Scoring work is bounded by source count times the configurable candidate limit (default 20, maximum 100). Retrieval work also depends on filename length and posting-list length, rather than a strict O(n*K) bound for the whole algorithm.

## Evidence Tiers

The name-only tiers retain their Milestone 5 thresholds; Milestone 8 adds stronger evidence tiers:

| Tier | Current Rule |
| --- | --- |
| Identical | Equal full BLAKE3 hashes after size and partial-hash stages. Candidate retrieval is still name-based; this tier does not search every file pair. |
| Very likely same recording | Exact normalized audio title and preferred artist tag, duration within max(2 seconds, 1.5%), and no filename version conflict. Still requires review. |
| Likely same song | Score at least 0.94, exact normalized core-title and filename-artist agreement, and no version conflict. Still requires review. |
| Uncertain | Title and weighted score at least 0.78, but missing/conflicting artist evidence, differing versions, or fuzzy-only title evidence. Requires review; no automatic bulk approval. |
| No candidate | Nothing retained above the review threshold. This does not prove that the folders contain no duplicate content. |

Very Likely does not assert byte identity; different encodings may be the same recording. `Identical` is content identity, not proof that metadata is correct. Fuzzy-only matches intentionally remain Uncertain. Matching tiers never approve a copy, including `Identical`; the user must review and explicitly choose.

## Quality Evidence

Run the same evaluator locally and in Windows CI:

```powershell
cargo test -p syncwise-engine --test matching_corpus -- --nocapture
```

`fixtures/matching-pairs.json` contains 70 hand-labeled seed pairs. The evaluator expands each into five deterministic case/track-prefix/punctuation/Unicode variants: 350 evaluated pairs, including 160 positive cases. Labels are synthetic ground truth; filenames alone cannot establish that real files contain the same recording.

Measured on 2026-10-28:

| Metric | Result | Gate |
| --- | --- | --- |
| Likely precision | 100% (98 true, 0 false) | At least 95% |
| Positive recall at Uncertain or higher | 99.375% (159/160) | At least 90% |
| Identical / Very likely | Controlled engine fixtures; no false `Identical` result for tested same-size different-content files | Stronger tiers require exact hash/tag conditions; not a real-library accuracy estimate |
| Actions authorized by matching scores | None; CLI regression verifies suggestions stay unapproved | Zero |

The one missed positive is a generated double track-number prefix (`01. 01 - ...`). The parser removes one prefix only. Other fixtures cover artist ordering/suffixes, title typos, version traps, different artists sharing a title, Unicode, missing metadata, encoding/size differences, same-name/different-bytes cases, general files, and same-artist near-title negatives. This labeled corpus remains filename-only; focused engine tests cover metadata tiers and identical-hash precision with controlled byte fixtures. A representative real-library audio-tag holdout is still desirable, and the synthetic filename corpus does not measure Very likely or Identical performance.

Evaluation exercises candidate retrieval and scoring on each labeled pair, not just scoring a preselected candidate. The five variants of each seed are correlated: 350 cases are not 350 independent recordings. This is a regression gate, not a held-out real-library accuracy estimate. Separate mixed-library tests cover many-to-one grouping, unique suggested assignments, deterministic ordering, retrieval caps, cancellation, and planner/copy policy. Expand the seed families and add independent real-world holdouts before making broader accuracy claims.

## Scaling and Limits

```powershell
cargo test -p syncwise-engine --release matching::tests::matches_twenty_thousand_files_per_side_within_twenty_seconds -- --ignored --nocapture
```

The local release-mode 20,000-by-20,000 synthetic matching check completed in 14.10 seconds and scored 400,000 pairs, versus 400,000,000 all-pairs comparisons. The test is opt-in because timing depends on hardware; it does not include scanning, serialization, or UI rendering.

Posting lists are capped at 256 entries by default (configurable up to 4096) to prevent common words/trigrams from degenerating into all-pairs retrieval. If any list is capped, `postingListsTruncated` is true. Common-title libraries can therefore miss candidates, even exact-title candidates beyond the cap. A report without a candidate is never proof of absence. Raise limits deliberately for such libraries; do not treat these scores as content verification.

Name parsing is heuristic. Version words inside genuine titles or artist names may be interpreted as version markers; separator conventions are ambiguous; unsupported file extensions do not cross-match. Audio tags may be absent, malformed, or inaccurate; album is retained for explanation but does not raise confidence. Content hashes distinguish different encodings even when they represent the same recording. Hashing is limited to name-retrieved candidates of equal size, so differently named content duplicates may not be found. Candidate explanations expose available evidence. Hashing can read substantial candidate data and is cancellable.