use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::available_space;
use tempfile::NamedTempFile;

use crate::{
    FileEntry, Plan, PlanIssue, PlanIssueCode, PlanItem, PlanItemKind, ScanIssueCode, ScanResult,
    SyncError, SyncErrorCode, normalize_name, validate_folder_pair,
};

static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

/// Build a preview. Similar names are advisory and never become automatic skips.
pub fn generate_plan(source: &ScanResult, destination: &ScanResult) -> Result<Plan, SyncError> {
    if source.cancelled || destination.cancelled {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "A plan cannot be generated from a cancelled scan.",
        ));
    }

    let source_root = fs::canonicalize(&source.root).map_err(|_| {
        SyncError::new(
            SyncErrorCode::InvalidSourcePath,
            "The scanned source folder is no longer available.",
        )
    })?;
    let destination_root = fs::canonicalize(&destination.root).map_err(|_| {
        SyncError::new(
            SyncErrorCode::InvalidDestinationPath,
            "The scanned destination folder is no longer available.",
        )
    })?;
    let folder_pair = validate_folder_pair(&source_root, &destination_root)?;
    if Path::new(&source.root) != Path::new(&folder_pair.source_root)
        || Path::new(&destination.root) != Path::new(&folder_pair.destination_root)
    {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Scanned roots do not match the validated folder pair.",
        ));
    }

    let mut issues = scan_issues(source, true);
    issues.extend(scan_issues(destination, false));

    let mut destination_paths: HashSet<&str> = destination
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    destination_paths.extend(
        destination
            .issues
            .iter()
            .filter_map(|issue| issue.relative_path.as_deref()),
    );

    let mut normalized_destination_names: HashMap<String, Vec<&str>> = HashMap::new();
    for file in &destination.files {
        normalized_destination_names
            .entry(normalized_file_name(file))
            .or_default()
            .push(&file.relative_path);
    }

    let mut source_files: Vec<&FileEntry> = source.files.iter().collect();
    source_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let mut items = Vec::with_capacity(source_files.len());
    let mut required_bytes = 0_u64;
    for (index, file) in source_files.iter().enumerate() {
        let id = format!("item-{:06}", index + 1);
        if !is_safe_relative_path(&file.relative_path) {
            issues.push(PlanIssue {
                code: PlanIssueCode::InvalidRelativePath,
                message: format!("Unsafe relative path was excluded: {}", file.relative_path),
                blocking: true,
            });
            items.push(plan_item(
                id,
                file,
                PlanItemKind::Error,
                Some("The relative path is invalid or cannot be copied safely.".into()),
            ));
            continue;
        }

        let (kind, reason) = if destination_paths.contains(file.relative_path.as_str())
            || !destination_path_available(&destination_root, &file.relative_path)
        {
            (
                PlanItemKind::Conflict,
                Some(
                    "A destination entry already uses this relative path; it will not be replaced."
                        .into(),
                ),
            )
        } else {
            let normalized_name = normalized_file_name(file);
            match normalized_destination_names.get(&normalized_name) {
                Some(matches) if !matches.is_empty() => (
                    PlanItemKind::PossibleDuplicate,
                    Some(format!(
                        "A destination file has the same normalized name: {}. Review manually; names do not prove identical content.",
                        matches[0]
                    )),
                ),
                _ => {
                    required_bytes = match required_bytes.checked_add(file.size_bytes) {
                        Some(total) => total,
                        None => {
                            issues.push(PlanIssue {
                                code: PlanIssueCode::SizeOverflow,
                                message: "The total copy size exceeds the supported range.".into(),
                                blocking: true,
                            });
                            u64::MAX
                        }
                    };
                    (PlanItemKind::Copy, None)
                }
            }
        };

        items.push(plan_item(id, file, kind, reason));
    }

    if let Err(message) = check_destination_writable(&destination_root) {
        issues.push(PlanIssue {
            code: PlanIssueCode::DestinationNotWritable,
            message,
            blocking: true,
        });
    }

    let available_bytes = match available_space(&destination_root) {
        Ok(available) => Some(available),
        Err(_) => {
            issues.push(PlanIssue {
                code: PlanIssueCode::AvailableSpaceUnknown,
                message: "Available destination space could not be determined.".into(),
                blocking: false,
            });
            None
        }
    };
    if available_bytes.is_some_and(|available| available < required_bytes) {
        issues.push(PlanIssue {
            code: PlanIssueCode::InsufficientSpace,
            message: "The destination does not have enough free space for the proposed copies. Check again before syncing.".into(),
            blocking: true,
        });
    }

    let can_execute = issues.iter().all(|issue| !issue.blocking);
    Ok(Plan {
        id: format!("plan-{}", NEXT_PLAN_ID.fetch_add(1, Ordering::Relaxed)),
        source_root: folder_pair.source_root,
        destination_root: folder_pair.destination_root,
        items,
        issues,
        required_bytes,
        available_bytes,
        can_execute,
    })
}

fn plan_item(id: String, file: &FileEntry, kind: PlanItemKind, reason: Option<String>) -> PlanItem {
    PlanItem {
        id,
        source_relative_path: file.relative_path.clone(),
        destination_relative_path: file.relative_path.clone(),
        size_bytes: file.size_bytes,
        source_modified_time_unix_ms: file.modified_time_unix_ms,
        kind,
        reason,
    }
}

fn scan_issues(scan: &ScanResult, is_source: bool) -> Vec<PlanIssue> {
    scan.issues
        .iter()
        .map(|issue| {
            let blocking = matches!(
                issue.code,
                ScanIssueCode::Unreadable | ScanIssueCode::PathNotRepresentable
            );
            let location = issue
                .relative_path
                .as_deref()
                .map(|path| format!("{path}: "))
                .unwrap_or_default();
            PlanIssue {
                code: if is_source {
                    PlanIssueCode::SourceScanIncomplete
                } else {
                    PlanIssueCode::DestinationScanIncomplete
                },
                message: format!("{location}{}", issue.message),
                blocking,
            }
        })
        .collect()
}

fn normalized_file_name(file: &FileEntry) -> String {
    let name = file
        .relative_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&file.relative_path);
    normalize_name(name)
}

fn destination_path_available(root: &Path, relative_path: &str) -> bool {
    let target = root.join(relative_path);
    match fs::symlink_metadata(&target) {
        Ok(_) => return false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    let mut parent = target.parent();
    while let Some(path) = parent {
        if path == root {
            return true;
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return false;
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return false;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        parent = path.parent();
    }
    false
}

pub(crate) fn is_safe_relative_path(relative_path: &str) -> bool {
    if relative_path.is_empty()
        || matches!(relative_path.chars().next(), Some('/' | '\\'))
        || relative_path.contains('\0')
    {
        return false;
    }

    relative_path.split(['/', '\\']).all(|component| {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with('.')
            || component.ends_with(' ')
            || component.encode_utf16().count() > 255
            || component.chars().any(|character| {
                character.is_control()
                    || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
            })
        {
            return false;
        }

        let device_name = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        !matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !["COM", "LPT"].iter().any(|prefix| {
                device_name.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
            })
    })
}

fn check_destination_writable(destination: &Path) -> Result<(), String> {
    let probe = NamedTempFile::new_in(destination)
        .map_err(|_| "The destination folder is not writable.".to_string())?;
    probe.close().map_err(|_| {
        "The destination folder could not remove its temporary write-check file.".to_string()
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use std::{fs, path::Path, sync::atomic::AtomicBool};

    use crate::{
        PlanItemKind, ScanIssue, ScanIssueCode, ScanResult, generate_plan, scan_directory,
    };

    fn scan(root: &Path) -> ScanResult {
        scan_directory(root, &AtomicBool::new(false), |_| {}).unwrap()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn generated_plans_never_copy_to_existing_entries(kinds in prop::collection::vec(0_u8..4, 1..16)) {
            let workspace = tempfile::tempdir().unwrap();
            let source = workspace.path().join("source");
            let destination = workspace.path().join("destination");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&destination).unwrap();
            for (index, kind) in kinds.iter().enumerate() {
                let name = format!("item-{index}.dat");
                fs::write(source.join(&name), b"source").unwrap();
                match kind {
                    1 => { fs::write(destination.join(&name), b"different").unwrap(); }
                    2 => { fs::create_dir(destination.join(&name)).unwrap(); }
                    3 => { fs::write(destination.join(&name), b"same size").unwrap(); }
                    _ => {}
                }
            }
            let plan = generate_plan(&scan(&source), &scan(&destination)).unwrap();
            for item in &plan.items {
                if fs::symlink_metadata(destination.join(&item.destination_relative_path)).is_ok() {
                    prop_assert_ne!(&item.kind, &PlanItemKind::Copy);
                }
            }
        }
    }

    #[test]
    fn makes_copy_conflict_and_advisory_duplicate_items_without_modifying_files() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(destination_root.join("renamed")).unwrap();
        fs::write(source_root.join("copy.txt"), b"new file").unwrap();
        fs::write(source_root.join("same.txt"), b"source content").unwrap();
        fs::write(source_root.join("Song.MP3"), b"source track").unwrap();
        fs::write(destination_root.join("same.txt"), b"different bytes").unwrap();
        fs::write(destination_root.join("renamed/song.mp3"), b"other track").unwrap();

        let plan = generate_plan(&scan(&source_root), &scan(&destination_root)).unwrap();

        assert!(plan.can_execute);
        assert_eq!(plan.items.len(), 3);
        assert!(
            plan.items
                .iter()
                .any(|item| item.source_relative_path == "copy.txt"
                    && item.kind == PlanItemKind::Copy)
        );
        assert!(
            plan.items
                .iter()
                .any(|item| item.source_relative_path == "same.txt"
                    && item.kind == PlanItemKind::Conflict)
        );
        let duplicate = plan
            .items
            .iter()
            .find(|item| item.source_relative_path == "Song.MP3")
            .unwrap();
        assert_eq!(duplicate.kind, PlanItemKind::PossibleDuplicate);
        assert!(
            duplicate
                .reason
                .as_deref()
                .unwrap()
                .contains("do not prove identical")
        );
        assert_eq!(plan.required_bytes, b"new file".len() as u64);
        assert_eq!(
            fs::read(destination_root.join("same.txt")).unwrap(),
            b"different bytes"
        );
        assert_eq!(
            fs::read_dir(destination_root.join("renamed"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 2);
    }

    #[test]
    fn blocks_execution_when_scan_is_incomplete_or_destination_path_is_invalid() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();

        let mut source = scan(&source_root);
        source.files.push(crate::FileEntry {
            relative_path: "../escape.txt".into(),
            size_bytes: 5,
            modified_time_unix_ms: None,
            normalized_name: "escape.txt".into(),
        });
        source.issues.push(ScanIssue {
            relative_path: Some("locked.txt".into()),
            code: ScanIssueCode::Unreadable,
            message: "unreadable".into(),
        });
        let plan = generate_plan(&source, &scan(&destination_root)).unwrap();

        assert!(!plan.can_execute);
        assert!(
            plan.items
                .iter()
                .any(|item| item.kind == PlanItemKind::Error)
        );
        assert!(plan.issues.iter().any(|issue| issue.blocking));
    }

    #[test]
    fn planner_never_marks_an_existing_destination_path_as_copy() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();

        for index in 0..64 {
            let filename = format!("file-{index}.dat");
            fs::write(source_root.join(&filename), format!("source-{index}")).unwrap();
            if index % 2 == 0 {
                fs::write(
                    destination_root.join(&filename),
                    format!("different-destination-{index}"),
                )
                .unwrap();
            }
        }

        let plan = generate_plan(&scan(&source_root), &scan(&destination_root)).unwrap();
        for item in &plan.items {
            if destination_root
                .join(&item.destination_relative_path)
                .exists()
            {
                assert_ne!(
                    item.kind,
                    PlanItemKind::Copy,
                    "{}",
                    item.destination_relative_path
                );
            }
        }
    }

    #[test]
    fn flags_windows_reserved_and_invalid_destination_names() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        let mut source = scan(&source_root);
        for (index, relative_path) in ["CON.txt", "folder/file?.txt", "C:/escape.txt", "trailing."]
            .into_iter()
            .enumerate()
        {
            source.files.push(crate::FileEntry {
                relative_path: relative_path.into(),
                size_bytes: 1,
                modified_time_unix_ms: None,
                normalized_name: format!("invalid-{index}"),
            });
        }

        let plan = generate_plan(&source, &scan(&destination_root)).unwrap();

        assert!(!plan.can_execute);
        assert_eq!(
            plan.items
                .iter()
                .filter(|item| item.kind == PlanItemKind::Error)
                .count(),
            4
        );
    }
}
