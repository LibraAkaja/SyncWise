use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::{
    AudioMetadata, FileEntry, ScanResult, SyncError, SyncErrorCode,
    audio::{full_hash_file, partial_hash_file},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MatchLabel {
    Identical,
    VeryLikelySameSong,
    LikelySameSong,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum HashEvidence {
    SizeMismatch,
    PartialHashMismatch,
    FullHashMismatch,
    Identical,
    Unavailable,
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
    #[ts(optional)]
    pub audio_tag_similarity: Option<f64>,
    #[ts(optional)]
    pub duration_similarity: Option<f64>,
    #[serde(default)]
    #[ts(optional)]
    pub hash_evidence: Option<HashEvidence>,
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
    #[serde(default)]
    pub within_side_duplicates: WithinSideDuplicateReport,
    #[ts(type = "number")]
    pub candidate_comparisons: u64,
    pub posting_lists_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WithinSideDuplicateGroup {
    #[ts(type = "number")]
    pub size_bytes: u64,
    pub relative_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WithinSideDuplicateReport {
    pub source: Vec<WithinSideDuplicateGroup>,
    pub destination: Vec<WithinSideDuplicateGroup>,
    #[ts(type = "number")]
    pub unavailable_files: u64,
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
        within_side_duplicates: WithinSideDuplicateReport::default(),
        candidate_comparisons: comparisons,
        posting_lists_truncated,
    })
}

pub fn match_scan_files_with_cancellation(
    source: &ScanResult,
    destination: &ScanResult,
    options: &MatchingOptions,
    cancellation: &AtomicBool,
) -> Result<MatchingReport, SyncError> {
    let mut report =
        match_files_with_cancellation(&source.files, &destination.files, options, cancellation)?;
    let source_by_path: HashMap<_, _> = source
        .files
        .iter()
        .map(|file| (file.relative_path.as_str(), file))
        .collect();
    let destination_by_path: HashMap<_, _> = destination
        .files
        .iter()
        .map(|file| (file.relative_path.as_str(), file))
        .collect();
    let mut source_partial = HashMap::new();
    let mut destination_partial = HashMap::new();
    let mut source_full = HashMap::new();
    let mut destination_full = HashMap::new();

    for matches in &mut report.matches {
        let Some(source_file) = source_by_path.get(matches.source_relative_path.as_str()) else {
            continue;
        };
        let source_path = std::path::Path::new(&source.root).join(&source_file.relative_path);
        for candidate in &mut matches.candidates {
            check_cancelled(cancellation)?;
            let Some(destination_file) =
                destination_by_path.get(candidate.destination_relative_path.as_str())
            else {
                continue;
            };
            if source_file.size_bytes != destination_file.size_bytes {
                candidate.hash_evidence = Some(HashEvidence::SizeMismatch);
                candidate
                    .reasons
                    .push("File sizes differ; content hashing was skipped.".into());
                continue;
            }
            let destination_path =
                std::path::Path::new(&destination.root).join(&destination_file.relative_path);
            let partial_match = match (
                cached_hash(
                    &source_path,
                    cancellation,
                    &mut source_partial,
                    partial_hash_file,
                ),
                cached_hash(
                    &destination_path,
                    cancellation,
                    &mut destination_partial,
                    partial_hash_file,
                ),
            ) {
                (Ok(source_hash), Ok(destination_hash)) => source_hash == destination_hash,
                (Err(error), _) | (_, Err(error))
                    if error.code == SyncErrorCode::OperationCancelled =>
                {
                    return Err(error);
                }
                (Err(error), _) | (_, Err(error)) => {
                    candidate.hash_evidence = Some(HashEvidence::Unavailable);
                    candidate.reasons.push(format!(
                        "Content hash evidence unavailable; filename/tag evidence only. {error}"
                    ));
                    continue;
                }
            };
            if !partial_match {
                candidate.hash_evidence = Some(HashEvidence::PartialHashMismatch);
                candidate
                    .reasons
                    .push("Partial content hashes differ; full hashing was skipped.".into());
                continue;
            }
            let identical = match (
                cached_hash(&source_path, cancellation, &mut source_full, full_hash_file),
                cached_hash(
                    &destination_path,
                    cancellation,
                    &mut destination_full,
                    full_hash_file,
                ),
            ) {
                (Ok(source_hash), Ok(destination_hash)) => source_hash == destination_hash,
                (Err(error), _) | (_, Err(error))
                    if error.code == SyncErrorCode::OperationCancelled =>
                {
                    return Err(error);
                }
                (Err(error), _) | (_, Err(error)) => {
                    candidate.hash_evidence = Some(HashEvidence::Unavailable);
                    candidate.reasons.push(format!(
                        "Full content hash evidence unavailable; filename/tag evidence only. {error}"
                    ));
                    continue;
                }
            };
            if identical {
                candidate.label = MatchLabel::Identical;
                candidate.hash_evidence = Some(HashEvidence::Identical);
                candidate
                    .reasons
                    .push("Full BLAKE3 hashes match; file contents are identical.".into());
            } else {
                candidate.hash_evidence = Some(HashEvidence::FullHashMismatch);
                candidate.reasons.push(
                    "Partial hashes matched but full BLAKE3 hashes differ; files are not identical."
                        .into(),
                );
            }
        }
    }

    let mut unavailable_files = 0;
    report.within_side_duplicates.source = find_within_side_duplicates(
        &source.root,
        &source.files,
        cancellation,
        &mut unavailable_files,
        &mut source_partial,
        &mut source_full,
    )?;
    report.within_side_duplicates.destination = find_within_side_duplicates(
        &destination.root,
        &destination.files,
        cancellation,
        &mut unavailable_files,
        &mut destination_partial,
        &mut destination_full,
    )?;
    report.within_side_duplicates.unavailable_files = unavailable_files;
    Ok(report)
}

fn find_within_side_duplicates(
    root: &str,
    files: &[FileEntry],
    cancellation: &AtomicBool,
    unavailable_files: &mut u64,
    partial_hashes: &mut HashMap<std::path::PathBuf, [u8; 32]>,
    full_hashes: &mut HashMap<std::path::PathBuf, [u8; 32]>,
) -> Result<Vec<WithinSideDuplicateGroup>, SyncError> {
    let mut by_size: BTreeMap<u64, Vec<&FileEntry>> = BTreeMap::new();
    for file in files {
        by_size.entry(file.size_bytes).or_default().push(file);
    }

    let mut duplicate_groups = Vec::new();
    for (size, same_size_files) in by_size.into_iter().filter(|(_, group)| group.len() > 1) {
        check_cancelled(cancellation)?;
        let mut by_partial_hash: HashMap<[u8; 32], Vec<&FileEntry>> = HashMap::new();
        for file in same_size_files {
            check_cancelled(cancellation)?;
            let relative = Path::new(&file.relative_path);
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                *unavailable_files = unavailable_files.saturating_add(1);
                continue;
            }
            let path = Path::new(root).join(relative);
            match cached_hash(&path, cancellation, partial_hashes, partial_hash_file) {
                Ok(hash) => by_partial_hash.entry(hash).or_default().push(file),
                Err(error) if error.code == SyncErrorCode::OperationCancelled => return Err(error),
                Err(_) => *unavailable_files = unavailable_files.saturating_add(1),
            }
        }

        for possible_duplicates in by_partial_hash
            .into_values()
            .filter(|group| group.len() > 1)
        {
            let mut by_full_hash: HashMap<[u8; 32], Vec<&FileEntry>> = HashMap::new();
            for file in possible_duplicates {
                check_cancelled(cancellation)?;
                let path = Path::new(root).join(&file.relative_path);
                match cached_hash(&path, cancellation, full_hashes, full_hash_file) {
                    Ok(hash) => by_full_hash.entry(hash).or_default().push(file),
                    Err(error) if error.code == SyncErrorCode::OperationCancelled => {
                        return Err(error);
                    }
                    Err(_) => *unavailable_files = unavailable_files.saturating_add(1),
                }
            }
            for mut identical_files in by_full_hash.into_values().filter(|group| group.len() > 1) {
                identical_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
                duplicate_groups.push(WithinSideDuplicateGroup {
                    size_bytes: size,
                    relative_paths: identical_files
                        .into_iter()
                        .map(|file| file.relative_path.clone())
                        .collect(),
                });
            }
        }
    }
    duplicate_groups.sort_by(|left, right| left.relative_paths.cmp(&right.relative_paths));
    Ok(duplicate_groups)
}

fn cached_hash(
    path: &std::path::Path,
    cancellation: &AtomicBool,
    cache: &mut HashMap<std::path::PathBuf, [u8; 32]>,
    hash: impl Fn(&std::path::Path, &AtomicBool) -> Result<[u8; 32], SyncError>,
) -> Result<[u8; 32], SyncError> {
    if let Some(value) = cache.get(path) {
        return Ok(*value);
    }
    let value = hash(path, cancellation)?;
    cache.insert(path.to_path_buf(), value);
    Ok(value)
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

struct PairScore {
    score: f64,
    title_similarity: f64,
    artist_similarity: Option<f64>,
    audio_tag_similarity: Option<f64>,
    duration_similarity: Option<f64>,
}

fn score_pair(
    source: &FileEntry,
    destination: &FileEntry,
    source_name: &PreparedName,
    destination_name: &PreparedName,
    options: &MatchingOptions,
) -> Option<CandidateMatch> {
    let mut best: Option<PairScore> = None;
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
            let audio_tag_similarity = tag_similarity(
                source.audio_metadata.as_ref(),
                destination.audio_metadata.as_ref(),
            );
            let duration_similarity = duration_similarity(
                source.audio_metadata.as_ref(),
                destination.audio_metadata.as_ref(),
            );
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
            if let Some(similarity) = audio_tag_similarity {
                weighted += similarity * 0.2;
                weight += 0.2;
            }
            if let Some(similarity) = duration_similarity {
                weighted += similarity * 0.1;
                weight += 0.1;
            }
            let score = weighted / weight;
            if best.as_ref().is_none_or(|current| score > current.score) {
                best = Some(PairScore {
                    score,
                    title_similarity: title,
                    artist_similarity: artist,
                    audio_tag_similarity,
                    duration_similarity,
                });
            }
        }
    }
    let PairScore {
        score,
        title_similarity,
        artist_similarity,
        audio_tag_similarity,
        duration_similarity,
    } = best?;
    if title_similarity < options.uncertain_threshold || score < options.uncertain_threshold {
        return None;
    }
    let version_conflict = source_name.versions != destination_name.versions;
    let artist_conflict = artist_similarity.is_some_and(|similarity| similarity < 0.90);
    let audio_tags_agree = audio_tags_agree(
        source.audio_metadata.as_ref(),
        destination.audio_metadata.as_ref(),
    );
    let duration_agrees = durations_agree(
        source.audio_metadata.as_ref(),
        destination.audio_metadata.as_ref(),
    );
    let label = if audio_tags_agree && duration_agrees && !version_conflict {
        MatchLabel::VeryLikelySameSong
    } else if score >= options.likely_threshold
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
    if let Some(similarity) = audio_tag_similarity {
        reasons.push(format!(
            "Audio tag title/artist similarity: {:.1}%.",
            similarity * 100.0
        ));
    } else {
        reasons.push("Audio title/artist tags are missing or incomplete.".into());
    }
    if let Some(similarity) = duration_similarity {
        reasons.push(format!(
            "Audio duration similarity: {:.1}%.",
            similarity * 100.0
        ));
    } else {
        reasons.push("Audio duration evidence is unavailable.".into());
    }
    if label == MatchLabel::VeryLikelySameSong {
        reasons.push(
            "Normalized audio title and artist tags agree and durations are within tolerance; review is still required."
                .into(),
        );
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
        "Hash evidence is unavailable until candidate content checks complete. This suggestion requires review.".into(),
    );
    Some(CandidateMatch {
        destination_relative_path: destination.relative_path.clone(),
        score,
        title_similarity,
        artist_similarity,
        size_plausibility: size,
        audio_tag_similarity,
        duration_similarity,
        hash_evidence: None,
        label,
        reasons,
        suggested: false,
    })
}

fn preferred_artist(metadata: &AudioMetadata) -> Option<&str> {
    metadata
        .album_artist
        .as_deref()
        .or(metadata.artist.as_deref())
        .map(str::trim)
        .filter(|artist| !artist.is_empty())
}

fn audio_tags_agree(source: Option<&AudioMetadata>, destination: Option<&AudioMetadata>) -> bool {
    let (Some(source), Some(destination)) = (source, destination) else {
        return false;
    };
    let (Some(source_title), Some(destination_title)) =
        (source.title.as_deref(), destination.title.as_deref())
    else {
        return false;
    };
    let (Some(source_artist), Some(destination_artist)) =
        (preferred_artist(source), preferred_artist(destination))
    else {
        return false;
    };
    normalize_title(source_title) == normalize_title(destination_title)
        && normalize_title(source_artist) == normalize_title(destination_artist)
}

fn durations_agree(source: Option<&AudioMetadata>, destination: Option<&AudioMetadata>) -> bool {
    let (Some(source), Some(destination)) = (source, destination) else {
        return false;
    };
    let (Some(source_duration), Some(destination_duration)) =
        (source.duration_ms, destination.duration_ms)
    else {
        return false;
    };
    if source_duration == 0 || destination_duration == 0 {
        return false;
    }
    let difference = source_duration.abs_diff(destination_duration);
    let tolerance = 2_000_u64.max(source_duration.max(destination_duration) / 67);
    difference <= tolerance
}

fn tag_similarity(
    source: Option<&AudioMetadata>,
    destination: Option<&AudioMetadata>,
) -> Option<f64> {
    let (Some(source), Some(destination)) = (source, destination) else {
        return None;
    };
    let title = source
        .title
        .as_deref()
        .zip(destination.title.as_deref())
        .map(|(left, right)| strsim::jaro_winkler(&normalize_title(left), &normalize_title(right)));
    let artist = preferred_artist(source)
        .zip(preferred_artist(destination))
        .map(|(left, right)| strsim::jaro_winkler(&normalize_title(left), &normalize_title(right)));
    match (title, artist) {
        (Some(title), Some(artist)) => Some((title + artist) / 2.0),
        (Some(title), None) => Some(title),
        (None, Some(artist)) => Some(artist),
        (None, None) => None,
    }
}

fn duration_similarity(
    source: Option<&AudioMetadata>,
    destination: Option<&AudioMetadata>,
) -> Option<f64> {
    let (Some(source_duration), Some(destination_duration)) = (
        source.and_then(|metadata| metadata.duration_ms),
        destination.and_then(|metadata| metadata.duration_ms),
    ) else {
        return None;
    };
    let longest = source_duration.max(destination_duration);
    (longest > 0)
        .then(|| 1.0 - source_duration.abs_diff(destination_duration) as f64 / longest as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn file(path: &str) -> FileEntry {
        FileEntry {
            relative_path: path.into(),
            size_bytes: 100,
            modified_time_unix_ms: None,
            normalized_name: String::new(),
            audio_metadata: None,
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
    fn matching_uses_tags_and_duration_for_very_likely_tier() {
        let mut source = file("Song - Artist.mp3");
        source.audio_metadata = Some(AudioMetadata {
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            album_artist: None,
            album: Some("Album".into()),
            duration_ms: Some(180_000),
        });
        let mut destination = file("Artist - Song.flac");
        destination.audio_metadata = Some(AudioMetadata {
            title: Some(" song ".into()),
            artist: None,
            album_artist: Some("ARTIST".into()),
            album: Some("Different album".into()),
            duration_ms: Some(181_500),
        });
        let report = match_files(
            &[source.clone()],
            &[destination.clone()],
            &MatchingOptions::default(),
        )
        .unwrap();
        let candidate = &report.matches[0].candidates[0];
        assert_eq!(candidate.label, MatchLabel::VeryLikelySameSong);
        assert_eq!(candidate.audio_tag_similarity, Some(1.0));
        assert!(candidate.duration_similarity.is_some());
        assert!(
            candidate
                .reasons
                .iter()
                .any(|reason| reason.contains("review is still required"))
        );

        destination.audio_metadata.as_mut().unwrap().duration_ms = Some(210_000);
        assert_eq!(
            match_files(&[source], &[destination], &MatchingOptions::default())
                .unwrap()
                .matches[0]
                .candidates[0]
                .label,
            MatchLabel::LikelySameSong
        );
    }

    #[test]
    fn staged_hashes_identify_exact_contents_and_reject_same_size_differences() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        let original = vec![3; 160_000];
        fs::write(source_root.join("Artist - Song.mp3"), &original).unwrap();
        fs::write(destination_root.join("Song - Artist.flac"), &original).unwrap();
        let mut altered = original.clone();
        altered[80_000] = 4;
        fs::write(destination_root.join("Artist - Song.flac"), altered).unwrap();
        let source = crate::scan_directory(&source_root, &AtomicBool::new(false), |_| {}).unwrap();
        let destination =
            crate::scan_directory(&destination_root, &AtomicBool::new(false), |_| {}).unwrap();
        let report = match_scan_files_with_cancellation(
            &source,
            &destination,
            &MatchingOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let candidates = &report.matches[0].candidates;
        assert_eq!(candidates.len(), 2);
        let identical = candidates
            .iter()
            .find(|candidate| candidate.destination_relative_path == "Song - Artist.flac")
            .unwrap();
        assert_eq!(identical.label, MatchLabel::Identical);
        assert_eq!(identical.hash_evidence, Some(HashEvidence::Identical));
        let different = candidates
            .iter()
            .find(|candidate| candidate.destination_relative_path == "Artist - Song.flac")
            .unwrap();
        assert_ne!(different.label, MatchLabel::Identical);
        assert_eq!(
            different.hash_evidence,
            Some(HashEvidence::FullHashMismatch)
        );
    }

    #[test]
    fn scan_matching_reports_exact_within_side_duplicates_only() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("source");
        let destination_root = root.path().join("destination");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&destination_root).unwrap();
        std::fs::write(source_root.join("first.bin"), b"same bytes").unwrap();
        std::fs::write(source_root.join("second.bin"), b"same bytes").unwrap();
        std::fs::write(source_root.join("different.bin"), b"same bytez").unwrap();
        std::fs::write(destination_root.join("copy-a.bin"), b"identical").unwrap();
        std::fs::write(destination_root.join("copy-b.bin"), b"identical").unwrap();

        let source = crate::scan_directory(&source_root, &AtomicBool::new(false), |_| {}).unwrap();
        let destination =
            crate::scan_directory(&destination_root, &AtomicBool::new(false), |_| {}).unwrap();
        let report = match_scan_files_with_cancellation(
            &source,
            &destination,
            &MatchingOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();

        assert_eq!(
            report.within_side_duplicates.source,
            vec![WithinSideDuplicateGroup {
                size_bytes: b"same bytes".len() as u64,
                relative_paths: vec!["first.bin".into(), "second.bin".into()],
            }]
        );
        assert_eq!(
            report.within_side_duplicates.destination,
            vec![WithinSideDuplicateGroup {
                size_bytes: b"identical".len() as u64,
                relative_paths: vec!["copy-a.bin".into(), "copy-b.bin".into()],
            }]
        );
        assert_eq!(report.within_side_duplicates.unavailable_files, 0);
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
    fn old_matching_reports_deserialize_without_within_side_duplicates() {
        let report: MatchingReport = serde_json::from_value(serde_json::json!({
            "matches": [],
            "groups": [],
            "candidateComparisons": 0,
            "postingListsTruncated": false,
        }))
        .unwrap();
        assert_eq!(
            report.within_side_duplicates,
            WithinSideDuplicateReport::default()
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
