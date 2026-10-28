# Advisory Filename Matching

Milestone 5 implements name-based candidate review for general files, with audio-aware filename parsing. It does not read audio tags, calculate hashes, inspect duration, approve copies, or delete files.

## Usage

```powershell
cargo run -p syncwise-cli -- match "C:\SyncWiseSamples\Source" "D:\SyncWiseSamples\Destination"
```

`match` scans metadata and prints JSON with matching candidates, groups, work counters, and per-side scan issues. It is read-only and supports Ctrl+C. `plan` also includes the matching report: occupied destination paths remain conflicts, plausible name matches become `possible_duplicate`, and unmatched files remain proposed copies. A candidate's `suggested` flag is a display/assignment hint, not an approval. CLI `copy --apply` still selects only missing-file Copy items; all suggestions need an explicit review decision.

Rust is the source of truth for the report. Engine tests generate TypeScript contracts, including `MatchingOptions`, `MatchingReport`, `CandidateMatch`, `MatchLabel`, and `MatchGroup`. Existing saved plans without the optional matching report remain deserializable.

## Pipeline

1. Keep original paths for display and filesystem operations. Comparison uses the filename stem, Unicode NFC normalization, full Unicode case folding, punctuation/space normalization, and one leading track-number prefix (`01 -` or `01.`).
2. Recognize ` by ` and spaced dash separators (including en/em dashes). Dash-separated names retain both artist/title orientations because filenames do not reliably specify their order.
3. Extract remix, live, acoustic, instrumental, remaster/remastered, radio edit, demo, cover, feat/ft/featuring, version, sped up, and slowed tokens. Differing version sets cap confidence at Uncertain; they never prove a recording is the same.
4. Build an inverted destination index over exact core titles, words, and Unicode character trigrams. Audio extensions share a family; general files compare only within the same extension. Retrieve the top 20 candidates by shared-key votes, giving exact-title keys priority.
5. Score candidates using Jaro-Winkler core-title similarity (weight 0.50), filename-artist similarity when available (0.20), and size plausibility when both sizes are nonzero (0.10). Duration's planned 0.20 weight is excluded because duration evidence is not implemented yet. Exclude missing signals and renormalize the available weights. Size is evidence only, not a byte-identity claim.
6. Emit the retained candidates with scores and evidence/penalty reasons. Greedy best-score assignment suggests at most one source per destination and vice versa. Sources choosing the same best destination are explicitly grouped for review; no candidate is discarded merely because another source has a better score.

Deterministic path ordering breaks score/retrieval ties. Matching can be cancelled between indexed/scored files. Scoring work is bounded by source count times the configurable candidate limit (default 20, maximum 100). Retrieval work also depends on filename length and posting-list length, rather than a strict O(n*K) bound for the whole algorithm.

## Evidence Tiers

The Milestone 5 gate uses a deliberately conservative name-only tier definition:

| Tier | Current Rule |
| --- | --- |
| Identical | Not emitted. Requires same size and a confirmed full hash in Milestone 8. |
| Very likely same recording | Not emitted. Requires agreeing audio tags and duration in Milestone 8. |
| Likely same song | Score at least 0.94, exact normalized core-title and filename-artist agreement, and no version conflict. Still requires review. |
| Uncertain | Title and weighted score at least 0.78, but missing/conflicting artist evidence, differing versions, or fuzzy-only title evidence. Requires review; no automatic bulk approval. |
| No candidate | Nothing retained above the review threshold. This does not prove that the folders contain no duplicate content. |

An absent strongest tier is reported as unavailable, not as measured 100% precision on an empty sample. Fuzzy-only matches intentionally remain Uncertain until a broader corpus supports stronger labeling. These restrictions protect precision without dropping lower-confidence suggestions from recall evaluation.

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
| Identical / Very likely | Unavailable at this stage | No unsupported strong claims |
| Actions authorized by matching scores | None; CLI regression verifies suggestions stay unapproved | Zero |

The one missed positive is a generated double track-number prefix (`01. 01 - ...`). The parser removes one prefix only. Other fixtures cover artist ordering/suffixes, title typos, version traps, different artists sharing a title, Unicode, missing metadata, encoding/size differences, same-name/different-bytes cases, general files, and same-artist near-title negatives. No audio duration is measured by this corpus evaluator; duration-specific scoring remains Milestone 8 work.

Evaluation exercises candidate retrieval and scoring on each labeled pair, not just scoring a preselected candidate. The five variants of each seed are correlated: 350 cases are not 350 independent recordings. This is a regression gate, not a held-out real-library accuracy estimate. Separate mixed-library tests cover many-to-one grouping, unique suggested assignments, deterministic ordering, retrieval caps, cancellation, and planner/copy policy. Expand the seed families and add independent real-world holdouts before making broader accuracy claims.

## Scaling and Limits

```powershell
cargo test -p syncwise-engine --release matching::tests::matches_twenty_thousand_files_per_side_within_twenty_seconds -- --ignored --nocapture
```

The local release-mode 20,000-by-20,000 synthetic matching check completed in 14.10 seconds and scored 400,000 pairs, versus 400,000,000 all-pairs comparisons. The test is opt-in because timing depends on hardware; it does not include scanning, serialization, or UI rendering.

Posting lists are capped at 256 entries by default (configurable up to 4096) to prevent common words/trigrams from degenerating into all-pairs retrieval. If any list is capped, `postingListsTruncated` is true. Common-title libraries can therefore miss candidates, even exact-title candidates beyond the cap. A report without a candidate is never proof of absence. Raise limits deliberately for such libraries; do not treat these scores as content verification.

Name parsing is heuristic. Version words inside genuine titles or artist names may be interpreted as version markers; separator conventions are ambiguous; unsupported file extensions do not cross-match. Different encodings/tags can describe the same recording but have different bytes. Candidate explanations expose what was available and what was missing. Stronger audio and hash evidence is intentionally deferred rather than guessed.