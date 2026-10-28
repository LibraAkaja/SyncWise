use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::{FileEntry, SyncError, SyncErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MatchLabel {
    LikelySameSong,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MatchingOptions {
    pub candidate_limit: usize,
    pub posting_limit: usize,
    pub likely_threshold: f64,
    pub uncertain_threshold: f64,
}

impl Default for MatchingOptions {
    fn default() -> Self {
        Self {
            candidate_limit: 20,
            posting_limit: 256,
            likely_threshold: 0.94,
            uncertain_threshold: 0.78,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct CandidateMatch {
    pub destination_relative_path: String,
    pub score: f64,
    pub title_similarity: f64,
    pub artist_similarity: Option<f64>,
    pub size_plausibility: Option<f64>,
    pub label: MatchLabel,
    pub reasons: Vec<String>,
    pub suggested: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FileMatches {
    pub source_relative_path: String,
    pub candidates: Vec<CandidateMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MatchGroup {
    pub destination_relative_path: String,
    pub source_relative_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct MatchingReport {
    pub matches: Vec<FileMatches>,
    pub groups: Vec<MatchGroup>,
    #[ts(type = "number")]
    pub candidate_comparisons: u64,
    pub posting_lists_truncated: bool,
}

#[derive(Debug, Clone)]
struct NameVariant {
    title: String,
    artist: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedName {
    variants: Vec<NameVariant>,
    versions: BTreeSet<String>,
    family: String,
}

pub fn normalize_title(value: &str) -> String {
    let folded: String = value.nfc().case_fold().collect::<String>().nfc().collect();
    folded
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn prepare(path: &str) -> PreparedName {
    let filename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, extension) = filename
        .rsplit_once('.')
        .map_or((filename, ""), |(stem, extension)| (stem, extension));
    let extension = extension.to_ascii_lowercase();
    let family = if matches!(
        extension.as_str(),
        "mp3" | "flac" | "wav" | "m4a" | "ogg" | "opus" | "aac" | "wma" | "aiff" | "alac"
    ) {
        "audio".to_string()
    } else {
        extension
    };
    let folded: String = stem.nfc().case_fold().collect();
    let folded = folded.replace(['\u{2013}', '\u{2014}'], " - ");
    let stem = strip_track_number(&folded);
    let mut versions = BTreeSet::new();
    let variants = if let Some((title, artist)) = stem.rsplit_once(" by ") {
        vec![NameVariant {
            title: core_title(title, &mut versions),
            artist: Some(normalize_title(artist)),
        }]
    } else if let Some((left, right)) = stem.split_once(" - ") {
        let left_core = core_title(left, &mut versions);
        let right_core = core_title(right, &mut versions);
        vec![
            NameVariant {
                title: left_core.clone(),
                artist: Some(right_core.clone()),
            },
            NameVariant {
                title: right_core,
                artist: Some(left_core),
            },
        ]
    } else {
        vec![NameVariant {
            title: core_title(stem, &mut versions),
            artist: None,
        }]
    };
    PreparedName {
        variants: variants
            .into_iter()
            .filter(|variant| !variant.title.is_empty())
            .collect(),
        versions,
        family,
    }
}

fn strip_track_number(value: &str) -> &str {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return value;
    }
    let remainder = &value[digits..];
    if let Some(rest) = remainder.strip_prefix('.') {
        return rest.trim_start();
    }
    let remainder = remainder.trim_start();
    if let Some(rest) = remainder.strip_prefix('-') {
        return rest.trim_start();
    }
    value
}

fn core_title(value: &str, versions: &mut BTreeSet<String>) -> String {
    let normalized = normalize_title(value);
    let words: Vec<_> = normalized.split_whitespace().collect();
    let mut title = Vec::new();
    let mut position = 0;
    while position < words.len() {
        let word = words[position];
        let phrase = words.get(position + 1).map(|next| format!("{word} {next}"));
        if phrase
            .as_deref()
            .is_some_and(|phrase| matches!(phrase, "radio edit" | "sped up"))
        {
            versions.insert(phrase.unwrap());
            position += 2;
            continue;
        }
        let version = match word {
            "remastered" => Some("remaster"),
            "remix" | "live" | "acoustic" | "instrumental" | "remaster" | "demo" | "cover"
            | "version" | "slowed" => Some(word),
            "feat" | "ft" | "featuring" => Some("feat"),
            _ => None,
        };
        if let Some(version) = version {
            versions.insert(version.into());
        } else {
            title.push(word);
        }
        position += 1;
    }
    title.join(" ")
}

fn blocking_keys(name: &PreparedName) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for variant in &name.variants {
        keys.insert(format!("{}:exact:{}", name.family, variant.title));
        for word in variant.title.split_whitespace() {
            if word.chars().count() > 1 {
                keys.insert(format!("{}:word:{word}", name.family));
            }
        }
        let characters: Vec<_> = variant.title.chars().collect();
        for trigram in characters.windows(3) {
            keys.insert(format!(
                "{}:tri:{}",
                name.family,
                trigram.iter().collect::<String>()
            ));
        }
    }
    keys
}

pub fn match_files(
    source: &[FileEntry],
    destination: &[FileEntry],
    options: &MatchingOptions,
) -> Result<MatchingReport, SyncError> {
    match_files_with_cancellation(source, destination, options, &AtomicBool::new(false))
}

pub fn match_files_with_cancellation(
    source: &[FileEntry],
    destination: &[FileEntry],
    options: &MatchingOptions,
    cancellation: &AtomicBool,
) -> Result<MatchingReport, SyncError> {
    check_cancelled(cancellation)?;
    if options.candidate_limit == 0
        || options.candidate_limit > 100
        || options.posting_limit == 0
        || options.posting_limit > 4096
        || !options.likely_threshold.is_finite()
        || !options.uncertain_threshold.is_finite()
        || !(0.0..=1.0).contains(&options.likely_threshold)
        || !(0.0..=options.likely_threshold).contains(&options.uncertain_threshold)
    {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Invalid matching limits or confidence thresholds.",
        ));
    }
    let mut destination: Vec<_> = destination.iter().collect();
    destination.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut names = Vec::with_capacity(destination.len());
    for file in &destination {
        check_cancelled(cancellation)?;
        names.push(prepare(&file.relative_path));
    }
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    let mut posting_lists_truncated = false;
    for (position, name) in names.iter().enumerate() {
        check_cancelled(cancellation)?;
        for key in blocking_keys(name) {
            let postings = index.entry(key).or_default();
            if postings.len() < options.posting_limit {
                postings.push(position);
            } else {
                posting_lists_truncated = true;
            }
        }
    }
    let mut source: Vec<_> = source.iter().collect();
    source.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut matches = Vec::with_capacity(source.len());
    let mut comparisons = 0;
    for file in source {
        check_cancelled(cancellation)?;
        let source_name = prepare(&file.relative_path);
        let mut votes: HashMap<usize, usize> = HashMap::new();
        for key in blocking_keys(&source_name) {
            if let Some(postings) = index.get(&key) {
                let weight = if key.contains(":exact:") { 1000 } else { 1 };
                for position in postings {
                    *votes.entry(*position).or_default() += weight;
                }
            }
        }
        let mut candidates: Vec<_> = votes.into_iter().collect();
        candidates.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        let mut scored = Vec::new();
        for (position, _) in candidates.into_iter().take(options.candidate_limit) {
            comparisons += 1;
            if let Some(candidate) = score_pair(
                file,
                destination[position],
                &source_name,
                &names[position],
                options,
            ) {
                scored.push(candidate);
            }
        }
        scored.sort_by(|left, right| {
            right.score.total_cmp(&left.score).then(
                left.destination_relative_path
                    .cmp(&right.destination_relative_path),
            )
        });
        matches.push(FileMatches {
            source_relative_path: file.relative_path.clone(),
            candidates: scored,
        });
    }
    let mut top_choices: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut assignments = Vec::new();
    for (source_index, entry) in matches.iter().enumerate() {
        if let Some(best) = entry.candidates.first() {
            top_choices
                .entry(best.destination_relative_path.clone())
                .or_default()
                .push(entry.source_relative_path.clone());
        }
        for (candidate_index, candidate) in entry.candidates.iter().enumerate() {
            assignments.push((candidate.score, source_index, candidate_index));
        }
    }
    assignments.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    let mut assigned_sources = HashSet::new();
    let mut assigned_destinations = HashSet::new();
    for (_, source_index, candidate_index) in assignments {
        check_cancelled(cancellation)?;
        let candidate = &mut matches[source_index].candidates[candidate_index];
        if !assigned_sources.contains(&source_index)
            && !assigned_destinations.contains(&candidate.destination_relative_path)
        {
            candidate.suggested = true;
            assigned_sources.insert(source_index);
            assigned_destinations.insert(candidate.destination_relative_path.clone());
        }
    }
    let groups = top_choices
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(
            |(destination_relative_path, source_relative_paths)| MatchGroup {
                destination_relative_path,
                source_relative_paths,
            },
        )
        .collect();
    Ok(MatchingReport {
        matches,
        groups,
        candidate_comparisons: comparisons,
        posting_lists_truncated,
    })
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), SyncError> {
    if cancellation.load(Ordering::Relaxed) {
        Err(SyncError::new(
            SyncErrorCode::OperationCancelled,
            "Matching was cancelled; no decisions were made.",
        ))
    } else {
        Ok(())
    }
}

fn score_pair(
    source: &FileEntry,
    destination: &FileEntry,
    source_name: &PreparedName,
    destination_name: &PreparedName,
    options: &MatchingOptions,
) -> Option<CandidateMatch> {
    let mut best: Option<(f64, f64, Option<f64>)> = None;
    let size = if source.size_bytes == 0 || destination.size_bytes == 0 {
        None
    } else {
        Some(
            source.size_bytes.min(destination.size_bytes) as f64
                / source.size_bytes.max(destination.size_bytes) as f64,
        )
    };
    for source_variant in &source_name.variants {
        for destination_variant in &destination_name.variants {
            let title = strsim::jaro_winkler(&source_variant.title, &destination_variant.title);
            let artist = source_variant
                .artist
                .as_ref()
                .zip(destination_variant.artist.as_ref())
                .map(|(left, right)| strsim::jaro_winkler(left, right));
            let mut weight = 0.5;
            let mut weighted = title * 0.5;
            if let Some(artist) = artist {
                weighted += artist * 0.2;
                weight += 0.2;
            }
            if let Some(size) = size {
                weighted += size * 0.1;
                weight += 0.1;
            }
            let score = weighted / weight;
            if best.as_ref().is_none_or(|current| score > current.0) {
                best = Some((score, title, artist));
            }
        }
    }
    let (score, title_similarity, artist_similarity) = best?;
    if title_similarity < options.uncertain_threshold || score < options.uncertain_threshold {
        return None;
    }
    let version_conflict = source_name.versions != destination_name.versions;
    let artist_conflict = artist_similarity.is_some_and(|similarity| similarity < 0.90);
    let label = if score >= options.likely_threshold
        && title_similarity >= options.likely_threshold
        && title_similarity == 1.0
        && artist_similarity == Some(1.0)
        && !version_conflict
        && !artist_conflict
    {
        MatchLabel::LikelySameSong
    } else {
        MatchLabel::Uncertain
    };
    let mut reasons = vec![format!(
        "Core-title similarity: {:.1}%.",
        title_similarity * 100.0
    )];
    if let Some(artist) = artist_similarity {
        reasons.push(format!(
            "Filename artist similarity: {:.1}%.",
            artist * 100.0
        ));
    } else {
        reasons.push(
            "Artist evidence is missing from at least one filename; its weight is excluded.".into(),
        );
    }
    if let Some(size) = size {
        reasons.push(format!(
            "Size plausibility: {:.1}% (not content identity).",
            size * 100.0
        ));
    }
    if version_conflict {
        reasons.push(format!(
            "Version words differ: {:?} versus {:?}; confidence capped at Uncertain.",
            source_name.versions, destination_name.versions
        ));
    }
    if artist_conflict {
        reasons.push("Filename artists conflict; confidence capped at Uncertain.".into());
    }
    if title_similarity != 1.0 || artist_similarity != Some(1.0) {
        reasons.push("The name-only Likely tier requires exact normalized title and artist agreement; fuzzy evidence remains Uncertain.".into());
    }
    reasons.push(
        "No hashes, audio tags, or duration evidence. This suggestion requires review.".into(),
    );
    Some(CandidateMatch {
        destination_relative_path: destination.relative_path.clone(),
        score,
        title_similarity,
        artist_similarity,
        size_plausibility: size,
        label,
        reasons,
        suggested: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileEntry {
        FileEntry {
            relative_path: path.into(),
            size_bytes: 100,
            modified_time_unix_ms: None,
            normalized_name: String::new(),
        }
    }

    #[test]
    fn unicode_normalization_is_canonical_and_case_folded() {
        assert_eq!(normalize_title("Stra\u{00df}e"), "strasse");
        assert_eq!(
            normalize_title("Caf\u{00e9}"),
            normalize_title("Cafe\u{0301}")
        );
        assert_eq!(normalize_title(" A...  Title! "), "a title");
    }

    #[test]
    fn handles_track_numbers_artist_order_and_version_traps() {
        let destination = [file("Shape of You - Ed Sheeran.mp3")];
        let source = [
            file("01. Ed Sheeran - Shape of You.flac"),
            file("Shape of You (remix) - Ed Sheeran.mp3"),
        ];
        let report = match_files(&source, &destination, &MatchingOptions::default()).unwrap();
        assert_eq!(
            report.matches[0].candidates[0].label,
            MatchLabel::LikelySameSong
        );
        assert_eq!(report.matches[1].candidates[0].label, MatchLabel::Uncertain);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report
                .matches
                .iter()
                .flat_map(|entry| &entry.candidates)
                .filter(|candidate| candidate.suggested)
                .count(),
            1
        );
    }

    #[test]
    fn title_only_matches_stay_uncertain_and_unrelated_extensions_are_not_compared() {
        let report = match_files(
            &[file("Home.mp3"), file("Report.pdf")],
            &[file("Home.flac"), file("Report.txt")],
            &MatchingOptions::default(),
        )
        .unwrap();
        assert_eq!(report.matches[0].candidates[0].label, MatchLabel::Uncertain);
        assert!(report.matches[1].candidates.is_empty());
        assert!(
            match_files(
                &[],
                &[],
                &MatchingOptions {
                    likely_threshold: f64::NAN,
                    ..MatchingOptions::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn candidate_work_is_bounded_results_are_deterministic_and_cancellable() {
        let destination: Vec<_> = (0..100)
            .map(|index| file(&format!("folder-{index}/Shared Title.mp3")))
            .collect();
        let source = [file("Shared Title.mp3")];
        let options = MatchingOptions {
            candidate_limit: 3,
            posting_limit: 8,
            ..MatchingOptions::default()
        };
        let result = match_files(&source, &destination, &options).unwrap();
        assert!(result.posting_lists_truncated);
        assert_eq!(result.candidate_comparisons, 3);
        assert_eq!(result.matches[0].candidates.len(), 3);
        let mut reversed = destination;
        reversed.reverse();
        assert_eq!(result, match_files(&source, &reversed, &options).unwrap());
        assert_eq!(
            match_files_with_cancellation(&[], &[], &options, &AtomicBool::new(true))
                .unwrap_err()
                .code,
            SyncErrorCode::OperationCancelled
        );
    }

    #[test]
    #[ignore = "manual release-mode 20k-by-20k matching performance gate"]
    fn matches_twenty_thousand_files_per_side_within_twenty_seconds() {
        use std::time::{Duration, Instant};
        let source: Vec<_> = (0..20_000)
            .map(|index| file(&format!("Artist {index:05} - Track {index:05}.mp3")))
            .collect();
        let destination: Vec<_> = (0..20_000)
            .map(|index| file(&format!("Track {index:05} - Artist {index:05}.flac")))
            .collect();
        let started = Instant::now();
        let result = match_files(&source, &destination, &MatchingOptions::default()).unwrap();
        let elapsed = started.elapsed();
        println!(
            "20k matching: {elapsed:?}; scored {} candidate pairs (400,000,000 all-pairs baseline)",
            result.candidate_comparisons
        );
        assert!(result.candidate_comparisons <= 20_000 * 20);
        assert_eq!(result.matches.len(), 20_000);
        assert!(result.matches.iter().all(|entry| {
            entry
                .candidates
                .first()
                .is_some_and(|candidate| candidate.label == MatchLabel::LikelySameSong)
        }));
        assert!(
            elapsed < Duration::from_secs(20),
            "matching exceeded budget: {elapsed:?}"
        );
    }
}
