use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Normalize a filename for basic comparisons.
/// This is an initial implementation, not the final matching algorithm.
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FolderPair {
    pub source_root: String,
    pub destination_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct FileEntry {
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_time_unix_ms: Option<i64>,
    pub normalized_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum PlanItemKind {
    Copy,
    PossibleDuplicate,
    AlreadyPresent,
    Conflict,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: String,
    pub source_relative_path: String,
    pub destination_relative_path: String,
    pub size_bytes: u64,
    pub kind: PlanItemKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub source_root: String,
    pub destination_root: String,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RunState {
    Planned,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SyncErrorCode {
    InvalidSourcePath,
    InvalidDestinationPath,
    SourceNotDirectory,
    DestinationNotDirectory,
    OverlappingFolders,
    InvalidInput,
    IoError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SyncError {
    pub code: SyncErrorCode,
    pub message: String,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SyncError {}

impl SyncError {
    fn new(code: SyncErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }
}

/// Canonicalize and validate selected roots before the app scans or plans them.
pub fn validate_folder_pair(source: &Path, destination: &Path) -> Result<FolderPair, SyncError> {
    let source = fs::canonicalize(source).map_err(|_| {
        SyncError::new(
            SyncErrorCode::InvalidSourcePath,
            "The source folder does not exist or cannot be accessed.",
        )
    })?;
    let destination = fs::canonicalize(destination).map_err(|_| {
        SyncError::new(
            SyncErrorCode::InvalidDestinationPath,
            "The destination folder does not exist or cannot be accessed.",
        )
    })?;

    if !source.is_dir() {
        return Err(SyncError::new(
            SyncErrorCode::SourceNotDirectory,
            "The selected source is not a folder.",
        ));
    }
    if !destination.is_dir() {
        return Err(SyncError::new(
            SyncErrorCode::DestinationNotDirectory,
            "The selected destination is not a folder.",
        ));
    }
    if source.starts_with(&destination) || destination.starts_with(&source) {
        return Err(SyncError::new(
            SyncErrorCode::OverlappingFolders,
            "Source and destination folders must not be the same or contain one another.",
        ));
    }

    Ok(FolderPair {
        source_root: source
            .to_str()
            .ok_or_else(|| {
                SyncError::new(
                    SyncErrorCode::InvalidSourcePath,
                    "The source folder path cannot be represented safely.",
                )
            })?
            .to_string(),
        destination_root: destination
            .to_str()
            .ok_or_else(|| {
                SyncError::new(
                    SyncErrorCode::InvalidDestinationPath,
                    "The destination folder path cannot be represented safely.",
                )
            })?
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn check_folder_pair(source: &Path, destination: &Path) -> Result<FolderPair, SyncError> {
        validate_folder_pair(source, destination)
    }

    #[test]
    fn trims_whitespace_and_ignores_ascii_case() {
        assert_eq!(normalize_name("  Song.MP3  "), "song.mp3");
    }

    #[test]
    fn refuses_identical_and_nested_folders() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        let nested = source.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&destination).unwrap();

        assert_eq!(
            check_folder_pair(&source, &source).unwrap_err().code,
            SyncErrorCode::OverlappingFolders
        );
        assert_eq!(
            check_folder_pair(&source, &nested).unwrap_err().code,
            SyncErrorCode::OverlappingFolders
        );
        assert_eq!(
            check_folder_pair(&nested, &source).unwrap_err().code,
            SyncErrorCode::OverlappingFolders
        );
    }

    #[test]
    fn accepts_distinct_sibling_folders_and_returns_canonical_roots() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();

        let pair = check_folder_pair(&source, &destination).unwrap();
        assert_eq!(
            pair.source_root,
            fs::canonicalize(source).unwrap().to_str().unwrap()
        );
        assert_eq!(
            pair.destination_root,
            fs::canonicalize(destination).unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn reports_missing_source_with_a_stable_error_code() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let destination = root.path().join("destination");
        fs::create_dir_all(&destination).unwrap();

        assert_eq!(
            check_folder_pair(&missing, &destination).unwrap_err().code,
            SyncErrorCode::InvalidSourcePath
        );
    }

    #[test]
    fn domain_types_round_trip_through_json() {
        let plan = Plan {
            id: "plan-1".into(),
            source_root: "C:/music".into(),
            destination_root: "D:/music".into(),
            items: vec![PlanItem {
                id: "item-1".into(),
                source_relative_path: "track.mp3".into(),
                destination_relative_path: "track.mp3".into(),
                size_bytes: 128,
                kind: PlanItemKind::Copy,
            }],
        };
        let serialized = serde_json::to_string(&plan).unwrap();
        let round_tripped: Plan = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_tripped, plan);
        assert!(serialized.contains("sourceRoot"));
        assert!(serialized.contains("copy"));
    }

    #[test]
    fn exports_domain_types_for_the_frontend() {
        let output_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/src/types/generated");
        fs::create_dir_all(&output_directory).unwrap();

        FolderPair::export_all_to(&output_directory).unwrap();
        FileEntry::export_all_to(&output_directory).unwrap();
        Plan::export_all_to(&output_directory).unwrap();
        Decision::export_all_to(&output_directory).unwrap();
        RunState::export_all_to(&output_directory).unwrap();
        SyncError::export_all_to(&output_directory).unwrap();
    }
}
