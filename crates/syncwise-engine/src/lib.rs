use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use walkdir::{DirEntry, WalkDir};

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
    #[ts(type = "number")]
    pub size_bytes: u64,
    #[ts(type = "number | null")]
    pub modified_time_unix_ms: Option<i64>,
    pub normalized_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ScanIssueCode {
    Unreadable,
    SymbolicLinkSkipped,
    PathNotRepresentable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScanIssue {
    pub relative_path: Option<String>,
    pub code: ScanIssueCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScanProgress {
    #[ts(type = "number")]
    pub files_scanned: u64,
    #[ts(type = "number")]
    pub directories_scanned: u64,
    #[ts(type = "number")]
    pub issues_found: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ScanResult {
    pub root: String,
    pub files: Vec<FileEntry>,
    pub issues: Vec<ScanIssue>,
    pub cancelled: bool,
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
    #[ts(type = "number")]
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
    ScanRootUnavailable,
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

/// Index regular files without following links or reading file contents.
pub fn scan_directory(
    root: &Path,
    cancellation: &AtomicBool,
    mut on_progress: impl FnMut(ScanProgress),
) -> Result<ScanResult, SyncError> {
    let root = fs::canonicalize(root).map_err(|_| {
        SyncError::new(
            SyncErrorCode::ScanRootUnavailable,
            "The folder no longer exists or cannot be accessed.",
        )
    })?;
    if !root.is_dir() {
        return Err(SyncError::new(
            SyncErrorCode::ScanRootUnavailable,
            "The selected scan root is not a folder.",
        ));
    }
    let root_text = root.to_str().ok_or_else(|| {
        SyncError::new(
            SyncErrorCode::ScanRootUnavailable,
            "The selected folder path cannot be represented safely.",
        )
    })?;

    let mut result = ScanResult {
        root: root_text.to_string(),
        files: Vec::new(),
        issues: Vec::new(),
        cancelled: false,
    };
    let mut directories_scanned = 0_u64;
    let mut walker = WalkDir::new(&root).follow_links(false).into_iter();

    while let Some(entry) = walker.next() {
        if cancellation.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let issue_path = error
                    .path()
                    .and_then(|path| relative_path_text(path, &root));
                result.issues.push(ScanIssue {
                    relative_path: issue_path,
                    code: ScanIssueCode::Unreadable,
                    message: "This folder or file could not be read; scanning continued.".into(),
                });
                continue;
            }
        };

        if entry.path() == root {
            continue;
        }

        let relative_path = match relative_path_text(entry.path(), &root) {
            Some(path) => path,
            None => {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                result.issues.push(ScanIssue {
                    relative_path: None,
                    code: ScanIssueCode::PathNotRepresentable,
                    message: "A path could not be represented as Unicode and was skipped.".into(),
                });
                continue;
            }
        };

        if is_ignored_entry(&entry, &relative_path) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }

        if entry.file_type().is_symlink() {
            result.issues.push(ScanIssue {
                relative_path: Some(relative_path),
                code: ScanIssueCode::SymbolicLinkSkipped,
                message: "Symbolic links and junctions are not followed.".into(),
            });
            continue;
        }

        if entry.file_type().is_dir() {
            directories_scanned += 1;
        } else if entry.file_type().is_file() {
            match entry.metadata() {
                Ok(metadata) => {
                    let name = entry.file_name().to_string_lossy();
                    let modified_time_unix_ms = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64);
                    result.files.push(FileEntry {
                        relative_path,
                        size_bytes: metadata.len(),
                        modified_time_unix_ms,
                        normalized_name: normalize_name(&name),
                    });
                }
                Err(_) => result.issues.push(ScanIssue {
                    relative_path: Some(relative_path),
                    code: ScanIssueCode::Unreadable,
                    message: "This file could not be read; scanning continued.".into(),
                }),
            }
        }

        if (result.files.len() as u64 + directories_scanned + result.issues.len() as u64)
            .is_multiple_of(64)
        {
            on_progress(ScanProgress {
                files_scanned: result.files.len() as u64,
                directories_scanned,
                issues_found: result.issues.len() as u64,
            });
        }
    }

    on_progress(ScanProgress {
        files_scanned: result.files.len() as u64,
        directories_scanned,
        issues_found: result.issues.len() as u64,
    });
    Ok(result)
}

fn relative_path_text(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root).ok()?.to_str().map(str::to_string)
}

fn is_ignored_entry(entry: &DirEntry, relative_path: &str) -> bool {
    if relative_path.split(['/', '\\']).any(is_ignored_name) {
        return true;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        entry
            .metadata()
            .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
            .unwrap_or(false)
    }

    #[cfg(not(windows))]
    {
        let _ = entry;
        false
    }
}

fn is_ignored_name(name: &str) -> bool {
    let lower_name = name.to_lowercase();
    name.starts_with('.')
        || matches!(
            lower_name.as_str(),
            "thumbs.db" | "desktop.ini" | ".ds_store"
        )
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
        ScanResult::export_all_to(&output_directory).unwrap();
        ScanProgress::export_all_to(&output_directory).unwrap();

        let file_entry = fs::read_to_string(output_directory.join("FileEntry.ts")).unwrap();
        let scan_progress = fs::read_to_string(output_directory.join("ScanProgress.ts")).unwrap();
        assert!(file_entry.contains("sizeBytes: number"));
        assert!(file_entry.contains("modifiedTimeUnixMs: number | null"));
        assert!(scan_progress.contains("filesScanned: number"));
        assert!(!scan_progress.contains("bigint"));
    }

    #[test]
    fn recursively_indexes_files_and_skips_hidden_entries() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::create_dir_all(root.path().join(".private")).unwrap();
        fs::write(
            root.path().join("nested").join("\u{00fc}nicode.MP3"),
            b"audio",
        )
        .unwrap();
        fs::write(root.path().join(".private/hidden.txt"), b"hidden").unwrap();
        fs::write(root.path().join("Thumbs.db"), b"system").unwrap();

        let cancellation = AtomicBool::new(false);
        let result = scan_directory(root.path(), &cancellation, |_| {}).unwrap();

        assert_eq!(result.files.len(), 1);
        assert_eq!(
            result.files[0].relative_path,
            format!("nested{}\u{00fc}nicode.MP3", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(result.files[0].normalized_name, "\u{00fc}nicode.mp3");
        assert_eq!(result.files[0].size_bytes, 5);
        assert!(result.files[0].modified_time_unix_ms.is_some());
        assert!(result.issues.is_empty());
        assert!(!result.cancelled);
    }

    #[test]
    fn reports_progress_and_stops_when_cancelled() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..200 {
            fs::write(root.path().join(format!("file-{index}.txt")), b"x").unwrap();
        }

        let cancellation = AtomicBool::new(false);
        let mut progress_updates = 0;
        let result = scan_directory(root.path(), &cancellation, |_| {
            progress_updates += 1;
            cancellation.store(true, Ordering::Relaxed);
        })
        .unwrap();

        assert!(result.cancelled);
        assert!(result.files.len() < 200);
        assert!(progress_updates >= 1);
    }

    #[test]
    #[ignore = "manual SSD performance gate; creates 40,000 temporary files"]
    fn scans_twenty_thousand_files_per_side_within_fifteen_seconds() {
        use std::time::{Duration, Instant};

        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for (root, prefix) in [
            (source.path(), "source"),
            (destination.path(), "destination"),
        ] {
            for index in 0..20_000 {
                fs::write(root.join(format!("{prefix}-{index}.dat")), b"").unwrap();
            }
        }

        for root in [source.path(), destination.path()] {
            let started = Instant::now();
            let result = scan_directory(root, &AtomicBool::new(false), |_| {}).unwrap();
            assert_eq!(result.files.len(), 20_000);
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "scanning {} took {:?}",
                root.display(),
                started.elapsed()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn reports_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.txt");
        let link = root.path().join("link.txt");
        fs::write(&target, b"target").unwrap();
        symlink(&target, &link).unwrap();

        let cancellation = AtomicBool::new(false);
        let result = scan_directory(root.path(), &cancellation, |_| {}).unwrap();

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].code, ScanIssueCode::SymbolicLinkSkipped);
        assert_eq!(result.issues[0].relative_path.as_deref(), Some("link.txt"));
    }
}
