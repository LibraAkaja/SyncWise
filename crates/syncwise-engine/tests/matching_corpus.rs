use std::collections::BTreeMap;

use serde::Deserialize;
use syncwise_engine::{FileEntry, MatchLabel, MatchingOptions, match_files};
use unicode_normalization::UnicodeNormalization;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pair {
    source: String,
    destination: String,
    same: bool,
    category: String,
    source_size: Option<u64>,
    destination_size: Option<u64>,
}

fn file(path: String, size: u64) -> FileEntry {
    FileEntry {
        relative_path: path,
        size_bytes: size,
        modified_time_unix_ms: None,
        normalized_name: String::new(),
        audio_metadata: None,
    }
}

fn variant(path: &str, variant: usize) -> String {
    match variant {
        0 => path.into(),
        1 => format!("01. {path}"),
        2 => path.to_uppercase(),
        3 => path.nfd().collect(),
        _ => path.replace(" - ", " \u{2013} ").replace('_', " "),
    }
}

#[test]
fn evaluate_labeled_matching_corpus() {
    let pairs: Vec<Pair> =
        serde_json::from_str(include_str!("../../../fixtures/matching-pairs.json")).unwrap();
    assert!(
        pairs.len() >= 60,
        "Maintain at least 60 independent labeled seed pairs"
    );
    let options = MatchingOptions::default();
    let mut total = 0;
    let mut positive_count = 0;
    let mut retrieved_positives = 0;
    let mut likely_true = 0;
    let mut likely_false = 0;
    let mut categories: BTreeMap<String, usize> = BTreeMap::new();
    let mut missed = Vec::new();
    for pair in &pairs {
        for variant_index in 0..5 {
            let source = [file(
                variant(&pair.source, variant_index),
                pair.source_size.unwrap_or(100),
            )];
            let destination = [file(
                variant(&pair.destination, variant_index),
                pair.destination_size.unwrap_or(100),
            )];
            let result = match_files(&source, &destination, &options).unwrap();
            let candidate = result.matches[0].candidates.first();
            total += 1;
            *categories.entry(pair.category.clone()).or_default() += 1;
            if pair.same {
                positive_count += 1;
                if candidate.is_some() {
                    retrieved_positives += 1;
                } else {
                    missed.push(format!(
                        "{} -> {} (variant {variant_index})",
                        pair.source, pair.destination
                    ));
                }
            }
            if candidate.is_some_and(|candidate| candidate.label == MatchLabel::LikelySameSong) {
                if pair.same {
                    likely_true += 1;
                } else {
                    likely_false += 1;
                }
            }
        }
    }
    let precision = likely_true as f64 / (likely_true + likely_false).max(1) as f64;
    let recall = retrieved_positives as f64 / positive_count.max(1) as f64;
    println!(
        "{}",
        serde_json::json!({
            "seedPairs": pairs.len(), "evaluatedPairs": total, "positivePairs": positive_count,
            "likelyTrue": likely_true, "likelyFalse": likely_false,
            "likelyPrecision": precision, "duplicateRecall": recall,
            "identical": "unavailable until full hashing", "veryLikely": "unavailable until tags/duration",
            "categories": categories, "missedPositivePairs": missed,
        })
    );
    assert!(total >= 300);
    assert!(
        likely_true > 0,
        "Precision requires a nonempty strong-label sample"
    );
    assert!(
        precision >= 0.95,
        "Likely precision {precision}; false positives {likely_false}"
    );
    assert!(recall >= 0.90, "Recall {recall}; missed {missed:?}");
}
