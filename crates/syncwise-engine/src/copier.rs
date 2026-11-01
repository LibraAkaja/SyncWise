use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::available_space;
use serde::{Deserialize, Serialize};
use tempfile::Builder;
use walkdir::WalkDir;

use crate::{
    CopyItemResult, CopyProgress, CopyReport, CopyState, Decision, JournalEntry, Plan, PlanItem,
    PlanItemKind, RecoveryReport, SyncError, SyncErrorCode, planner::is_safe_relative_path,
    validate_folder_pair,
};

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);
const COPY_BUFFER_SIZE: usize = 1024 * 1024;
const TEMP_FILE_MARKER: &str = ".syncwise-part-";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedRun {
    version: u32,
    run_id: String,
    journal_path: PathBuf,
    plan: Plan,
    decisions: HashMap<String, Decision>,
}

pub fn run_snapshot_path(journal_path: &Path, run_id: &str) -> PathBuf {
    journal_path.with_file_name(format!("{run_id}.syncwise-run.json"))
}

/// Retry the original approved plan; existing final files are never replaced.
pub fn resume_run(
    snapshot_path: &Path,
    dry_run: bool,
    cancellation: &AtomicBool,
    on_progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, SyncError> {
    let snapshot: SavedRun = serde_json::from_reader(
        File::open(snapshot_path)
            .map_err(|error| io_error("Could not open the saved run", error))?,
    )
    .map_err(|error| journal_error("Invalid saved run", error))?;
    if snapshot.version != 1 {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Unsupported saved run version.",
        ));
    }
    execute_run(snapshot, dry_run, cancellation, on_progress, false)
}

/// Copy only explicitly approved Copy items. A dry run performs no journal or file writes.
pub fn copy_approved_items(
    plan: &Plan,
    decisions: &HashMap<String, Decision>,
    dry_run: bool,
    cancellation: &AtomicBool,
    journal_path: &Path,
    on_progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, SyncError> {
    copy_approved_items_as(
        new_run_id(),
        plan,
        decisions,
        dry_run,
        cancellation,
        journal_path,
        on_progress,
    )
}

/// Same as `copy_approved_items` with a caller-chosen run ID so the run can be recorded before it starts.
pub fn copy_approved_items_as(
    run_id: String,
    plan: &Plan,
    decisions: &HashMap<String, Decision>,
    dry_run: bool,
    cancellation: &AtomicBool,
    journal_path: &Path,
    on_progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, SyncError> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Run IDs may contain only letters, digits, and hyphens.",
        ));
    }
    execute_run(
        SavedRun {
            version: 1,
            run_id,
            journal_path: journal_path.to_path_buf(),
            plan: plan.clone(),
            decisions: decisions.clone(),
        },
        dry_run,
        cancellation,
        on_progress,
        true,
    )
}

fn execute_run(
    snapshot: SavedRun,
    dry_run: bool,
    cancellation: &AtomicBool,
    mut on_progress: impl FnMut(CopyProgress),
    save_snapshot: bool,
) -> Result<CopyReport, SyncError> {
    let plan = &snapshot.plan;
    let decisions = &snapshot.decisions;
    let journal_path = &snapshot.journal_path;
    let run_id = snapshot.run_id.clone();
    if dry_run {
        let history = if save_snapshot {
            Vec::new()
        } else {
            read_journal(journal_path, TailPolicy::Inspect)?
        };
        return Ok(CopyReport {
            run_id: run_id.clone(),
            dry_run: true,
            cancelled: false,
            items: plan
                .items
                .iter()
                .map(|item| {
                    if let Some(done) = history.iter().rev().find(|entry| {
                        entry.run_id == run_id
                            && entry.item_id == item.id
                            && entry.state == CopyState::Done
                    }) {
                        item_result(
                            item,
                            CopyState::Done,
                            done.bytes_written,
                            Some("Completed in the original run; not copied again.".into()),
                        )
                    } else {
                        preview_result(item, decisions)
                    }
                })
                .collect(),
        });
    }
    if !plan.can_execute || plan.issues.iter().any(|issue| issue.blocking) {
        return Err(SyncError::new(
            SyncErrorCode::PlanNotExecutable,
            "The plan has blocking preflight issues and cannot be executed.",
        ));
    }

    let folder_pair = validate_folder_pair(
        Path::new(&plan.source_root),
        Path::new(&plan.destination_root),
    )?;
    if folder_pair.source_root != plan.source_root
        || folder_pair.destination_root != plan.destination_root
    {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Plan roots are not canonical or no longer match the selected folders.",
        ));
    }

    let source_root = PathBuf::from(&folder_pair.source_root);
    let destination_root = PathBuf::from(&folder_pair.destination_root);
    validate_journal_path(journal_path, &source_root)?;

    let journal_parent = journal_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(journal_parent)
        .map_err(|error| io_error("Could not create the journal directory", error))?;
    let lock_path = journal_path.with_extension("lock");
    validate_journal_path(&lock_path, &source_root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| io_error("Could not open the run lock", error))?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
        io_error(
            "Another copy or recovery operation owns this journal",
            error,
        )
    })?;
    let entries = read_journal(
        journal_path,
        if save_snapshot {
            TailPolicy::Strict
        } else {
            TailPolicy::Repair
        },
    )?;
    let completed: HashMap<_, _> = entries
        .iter()
        .filter(|entry| entry.run_id == run_id && entry.state == CopyState::Done)
        .map(|entry| (entry.item_id.as_str(), entry))
        .collect();
    if save_snapshot {
        let snapshot_path = run_snapshot_path(journal_path, &run_id);
        validate_journal_path(&snapshot_path, &source_root)?;
        let canonical_journal_path = fs::canonicalize(journal_parent)
            .map_err(|error| io_error("Could not resolve the journal directory", error))?
            .join(journal_path.file_name().ok_or_else(|| {
                SyncError::new(SyncErrorCode::InvalidInput, "Journal must name a file.")
            })?);
        let mut temporary = Builder::new()
            .prefix(".syncwise-run-")
            .tempfile_in(journal_parent)
            .map_err(|error| io_error("Could not prepare the run snapshot", error))?;
        serde_json::to_writer(
            &mut temporary,
            &serde_json::json!({
                "version": snapshot.version,
                "runId": snapshot.run_id,
                "journalPath": canonical_journal_path,
                "plan": plan,
                "decisions": decisions,
            }),
        )
        .map_err(|error| journal_error("Could not serialize the saved run", error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| io_error("Could not persist the saved run", error))?;
        temporary
            .persist_noclobber(snapshot_path)
            .map_err(|error| io_error("Could not finalize the saved run", error.error))?;
    }

    let required_bytes = plan
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                PlanItemKind::Copy | PlanItemKind::PossibleDuplicate
            ) && decisions.get(&item.id) == Some(&Decision::Approve)
                && !completed.contains_key(item.id.as_str())
        })
        .try_fold(0_u64, |total, item| total.checked_add(item.size_bytes))
        .ok_or_else(|| {
            SyncError::new(
                SyncErrorCode::InvalidInput,
                "Approved copy size exceeds the supported range.",
            )
        })?;
    if required_bytes > 0 {
        let available_bytes = available_space(&destination_root)
            .map_err(|error| io_error("Could not recheck destination free space", error))?;
        if available_bytes < required_bytes {
            return Err(SyncError::new(
                SyncErrorCode::InsufficientSpace,
                "The destination no longer has enough free space for the approved files.",
            ));
        }
    }
    let mut report = CopyReport {
        run_id: run_id.clone(),
        dry_run: false,
        cancelled: false,
        items: Vec::with_capacity(plan.items.len()),
    };

    for item in &plan.items {
        if let Some(done) = completed.get(item.id.as_str()) {
            report.items.push(item_result(
                item,
                CopyState::Done,
                done.bytes_written,
                Some("Completed in the original run; not copied again.".into()),
            ));
            continue;
        }
        if cancellation.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }

        if !matches!(
            item.kind,
            PlanItemKind::Copy | PlanItemKind::PossibleDuplicate
        ) || decisions.get(&item.id) != Some(&Decision::Approve)
        {
            report.items.push(preview_result(item, decisions));
            continue;
        }

        let result = copy_one(
            item,
            &run_id,
            &source_root,
            &destination_root,
            journal_path,
            cancellation,
            &mut on_progress,
        )?;
        on_progress(CopyProgress {
            run_id: run_id.clone(),
            item_id: item.id.clone(),
            state: result.state,
            bytes_written: result.bytes_written,
            total_bytes: item.size_bytes,
        });
        if result.state == CopyState::Cancelled {
            report.cancelled = true;
        }
        report.items.push(result);
        if report.cancelled {
            break;
        }
    }

    Ok(report)
}

#[derive(Clone, Copy)]
enum TailPolicy {
    Strict,
    Inspect,
    Repair,
}

fn read_journal(path: &Path, policy: TailPolicy) -> Result<Vec<JournalEntry>, SyncError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("Could not read the journal", error)),
    };
    let complete_length = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut entries = Vec::new();
    for line in bytes[..complete_length].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        entries.push(serde_json::from_slice(line).map_err(|error| {
            journal_error("The journal contains a corrupt complete record", error)
        })?);
    }
    if complete_length != bytes.len() && matches!(policy, TailPolicy::Strict) {
        return Err(SyncError::new(
            SyncErrorCode::JournalUnavailable,
            "The journal has an incomplete final record. Resume its saved run before starting a new copy.",
        ));
    }
    if matches!(policy, TailPolicy::Repair) && complete_length != bytes.len() {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| io_error("Could not repair the incomplete journal tail", error))?;
        file.set_len(complete_length as u64)
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error("Could not persist journal tail repair", error))?;
    }
    Ok(entries)
}

fn validate_journal_path(journal_path: &Path, source_root: &Path) -> Result<(), SyncError> {
    let absolute_path = if journal_path.is_absolute() {
        journal_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| io_error("Could not resolve the journal path", error))?
            .join(journal_path)
    };
    let normalized_path = normalize_path(&absolute_path);
    let mut existing_parent = normalized_path.clone();
    while !existing_parent.exists() {
        if !existing_parent.pop() {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "The journal path cannot be resolved safely.",
            ));
        }
    }
    let canonical_parent = fs::canonicalize(&existing_parent)
        .map_err(|error| io_error("Could not inspect the journal path", error))?;
    if normalized_path.starts_with(source_root) || canonical_parent.starts_with(source_root) {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "The copy journal must not be stored inside the read-only source folder.",
        ));
    }

    let mut component_path = PathBuf::new();
    for component in normalized_path.components() {
        component_path.push(component.as_os_str());
        if !matches!(component, Component::Normal(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&component_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(io_error("Could not inspect the journal path", error)),
        };
        if metadata.file_type().is_symlink() {
            let target = match fs::canonicalize(&component_path) {
                Ok(target) => target,
                Err(_) => {
                    let link_target = fs::read_link(&component_path).map_err(|error| {
                        io_error("Could not resolve the journal symlink", error)
                    })?;
                    let target = if link_target.is_absolute() {
                        link_target
                    } else {
                        component_path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(link_target)
                    };
                    normalize_path(&target)
                }
            };
            if target.starts_with(source_root) {
                return Err(SyncError::new(
                    SyncErrorCode::InvalidInput,
                    "The copy journal must not resolve inside the read-only source folder.",
                ));
            }
        }
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    normalized
}
fn copy_one(
    item: &PlanItem,
    run_id: &str,
    source_root: &Path,
    destination_root: &Path,
    journal_path: &Path,
    cancellation: &AtomicBool,
    on_progress: &mut impl FnMut(CopyProgress),
) -> Result<CopyItemResult, SyncError> {
    if !is_safe_relative_path(&item.source_relative_path)
        || !is_safe_relative_path(&item.destination_relative_path)
    {
        return Ok(item_result(
            item,
            CopyState::Failed,
            0,
            Some("The plan contains an unsafe relative path.".into()),
        ));
    }

    let source_path = source_root.join(&item.source_relative_path);
    let destination_path = destination_root.join(&item.destination_relative_path);
    let mut journal_entry = JournalEntry {
        run_id: run_id.into(),
        item_id: item.id.clone(),
        source_relative_path: item.source_relative_path.clone(),
        destination_relative_path: item.destination_relative_path.clone(),
        state: CopyState::Planned,
        temp_relative_path: None,
        bytes_written: 0,
        message: None,
    };
    append_journal(journal_path, &journal_entry)?;

    if cancellation.load(Ordering::Relaxed) {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::Cancelled,
            0,
            Some("Copy was cancelled before it started.".into()),
        );
    }

    let mut source_file = match open_and_revalidate_source(item, source_root, &source_path) {
        Ok(file) => file,
        Err(state) => {
            let (copy_state, message) = state;
            return finish_item(
                item,
                journal_path,
                journal_entry,
                copy_state,
                0,
                Some(message),
            );
        }
    };
    transition(
        journal_path,
        &mut journal_entry,
        CopyState::Revalidated,
        0,
        None,
    )?;

    let destination_parent = match ensure_destination_parent(destination_root, &destination_path) {
        Ok(parent) => parent,
        Err(error) => {
            return finish_item(
                item,
                journal_path,
                journal_entry,
                CopyState::Failed,
                0,
                Some(error.to_string()),
            );
        }
    };
    if fs::symlink_metadata(&destination_path).is_ok() {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::Skipped,
            0,
            Some("Destination path appeared after planning; it was not replaced.".into()),
        );
    }

    let mut temp_file = match Builder::new()
        .prefix(TEMP_FILE_MARKER)
        .suffix(".tmp")
        .tempfile_in(&destination_parent)
    {
        Ok(file) => file,
        Err(error) => {
            return finish_item(
                item,
                journal_path,
                journal_entry,
                CopyState::Failed,
                0,
                Some(format!(
                    "Could not create a temporary destination file: {error}"
                )),
            );
        }
    };
    journal_entry.temp_relative_path = temp_file
        .path()
        .strip_prefix(destination_root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_string);
    transition(
        journal_path,
        &mut journal_entry,
        CopyState::Copying,
        0,
        None,
    )?;

    let bytes_written = match transfer_chunks(
        &mut source_file,
        &mut temp_file,
        cancellation,
        |bytes_written| {
            on_progress(CopyProgress {
                run_id: run_id.into(),
                item_id: item.id.clone(),
                state: CopyState::Copying,
                bytes_written,
                total_bytes: item.size_bytes,
            });
        },
    ) {
        Ok(bytes) => bytes,
        Err(failure) => {
            return finish_item(
                item,
                journal_path,
                journal_entry,
                failure.state,
                failure.bytes_written,
                Some(failure.message),
            );
        }
    };

    if bytes_written != item.size_bytes {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::NeedsRescan,
            bytes_written,
            Some("The source size changed since the plan was created.".into()),
        );
    }
    if source_is_stale(item, &source_file) || source_path_is_stale(item, source_root, &source_path)
    {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::NeedsRescan,
            bytes_written,
            Some("The source changed while it was being copied.".into()),
        );
    }

    if let Err(error) = temp_file
        .flush()
        .and_then(|()| temp_file.as_file().sync_all())
    {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::Failed,
            bytes_written,
            Some(format!(
                "Could not flush the temporary destination file: {error}"
            )),
        );
    }
    transition(
        journal_path,
        &mut journal_entry,
        CopyState::Flushed,
        bytes_written,
        None,
    )?;

    if cancellation.load(Ordering::Relaxed) {
        return finish_item(
            item,
            journal_path,
            journal_entry,
            CopyState::Cancelled,
            bytes_written,
            Some(
                "Copy was cancelled before finalization; the temporary file was discarded.".into(),
            ),
        );
    }
    transition(
        journal_path,
        &mut journal_entry,
        CopyState::Finalizing,
        bytes_written,
        None,
    )?;

    match temp_file.persist_noclobber(&destination_path) {
        Ok(_) => {
            journal_entry.temp_relative_path = None;
            transition(
                journal_path,
                &mut journal_entry,
                CopyState::Finalized,
                bytes_written,
                None,
            )?;
            finish_item(
                item,
                journal_path,
                journal_entry,
                CopyState::Done,
                bytes_written,
                None,
            )
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            drop(error.file);
            journal_entry.temp_relative_path = None;
            finish_item(
                item,
                journal_path,
                journal_entry,
                CopyState::Skipped,
                0,
                Some("Destination path appeared during copy; it was not replaced.".into()),
            )
        }
        Err(error) => {
            let message = format!(
                "Could not finalize the file without replacement: {}",
                error.error
            );
            drop(error.file);
            journal_entry.temp_relative_path = None;
            finish_item(
                item,
                journal_path,
                journal_entry,
                CopyState::Failed,
                bytes_written,
                Some(message),
            )
        }
    }
}

fn open_and_revalidate_source(
    item: &PlanItem,
    source_root: &Path,
    source_path: &Path,
) -> Result<File, (CopyState, String)> {
    let symlink_metadata = fs::symlink_metadata(source_path).map_err(|_| {
        (
            CopyState::NeedsRescan,
            "The source file no longer exists.".into(),
        )
    })?;
    if !symlink_metadata.file_type().is_file() {
        return Err((
            CopyState::NeedsRescan,
            "The source is no longer a regular file.".into(),
        ));
    }

    let canonical_source = fs::canonicalize(source_path).map_err(|_| {
        (
            CopyState::NeedsRescan,
            "The source file could not be resolved safely.".into(),
        )
    })?;
    if !canonical_source.starts_with(source_root) {
        return Err((
            CopyState::NeedsRescan,
            "The source file resolves outside the selected source folder.".into(),
        ));
    }

    let file = File::open(source_path).map_err(|error| {
        (
            CopyState::Failed,
            format!("The source file could not be opened: {error}"),
        )
    })?;
    if metadata_is_stale(
        item,
        &file.metadata().map_err(|error| {
            (
                CopyState::Failed,
                format!("Could not inspect the opened source file: {error}"),
            )
        })?,
    ) {
        return Err((
            CopyState::NeedsRescan,
            "The source changed since the plan was created.".into(),
        ));
    }
    Ok(file)
}

#[derive(Debug)]
struct TransferFailure {
    state: CopyState,
    bytes_written: u64,
    message: String,
}

fn transfer_chunks(
    source: &mut impl Read,
    destination: &mut impl Write,
    cancellation: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, TransferFailure> {
    let mut bytes_written = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        if cancellation.load(Ordering::Relaxed) {
            return Err(TransferFailure {
                state: CopyState::Cancelled,
                bytes_written,
                message: "Copy was cancelled; incomplete data was not finalized.".into(),
            });
        }
        let count = source.read(&mut buffer).map_err(|error| TransferFailure {
            state: CopyState::Failed,
            bytes_written,
            message: format!("Source read failed: {error}"),
        })?;
        if count == 0 {
            return Ok(bytes_written);
        }
        destination
            .write_all(&buffer[..count])
            .map_err(|error| TransferFailure {
                state: CopyState::Failed,
                bytes_written,
                message: format!("Destination write failed: {error}"),
            })?;
        bytes_written = bytes_written.saturating_add(count as u64);
        on_progress(bytes_written);
    }
}

fn metadata_is_stale(item: &PlanItem, metadata: &fs::Metadata) -> bool {
    metadata.len() != item.size_bytes
        || item.source_modified_time_unix_ms.is_some_and(|planned| {
            modified_unix_ms(metadata).is_none_or(|current| current != planned)
        })
}

fn source_is_stale(item: &PlanItem, source: &File) -> bool {
    source
        .metadata()
        .map(|metadata| metadata_is_stale(item, &metadata))
        .unwrap_or(true)
}

fn source_path_is_stale(item: &PlanItem, source_root: &Path, source_path: &Path) -> bool {
    let Ok(symlink_metadata) = fs::symlink_metadata(source_path) else {
        return true;
    };
    if !symlink_metadata.file_type().is_file() || metadata_is_stale(item, &symlink_metadata) {
        return true;
    }
    fs::canonicalize(source_path)
        .map(|canonical| !canonical.starts_with(source_root))
        .unwrap_or(true)
}

fn modified_unix_ms(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
}

fn ensure_destination_parent(root: &Path, destination: &Path) -> Result<PathBuf, SyncError> {
    let relative_parent = destination
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
        .ok_or_else(|| {
            SyncError::new(
                SyncErrorCode::InvalidInput,
                "Destination path resolves outside the selected folder.",
            )
        })?;
    let mut current = root.to_path_buf();
    for component in relative_parent.components() {
        let Component::Normal(name) = component else {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "Destination path contains an unsafe component.",
            ));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(SyncError::new(
                    SyncErrorCode::InvalidInput,
                    "A destination parent is not a regular folder.",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(io_error("Could not create a destination subfolder", error));
                    }
                }
                let metadata = fs::symlink_metadata(&current).map_err(|error| {
                    io_error("Could not inspect a destination subfolder", error)
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(SyncError::new(
                        SyncErrorCode::InvalidInput,
                        "A destination parent changed into a non-folder path.",
                    ));
                }
            }
            Err(error) => return Err(io_error("Could not inspect a destination subfolder", error)),
        }
    }
    let canonical = fs::canonicalize(&current)
        .map_err(|error| io_error("Could not resolve the destination parent", error))?;
    if !canonical.starts_with(root) {
        return Err(SyncError::new(
            SyncErrorCode::InvalidInput,
            "Destination parent resolves outside the selected folder.",
        ));
    }
    Ok(canonical)
}

fn finish_item(
    item: &PlanItem,
    journal_path: &Path,
    mut entry: JournalEntry,
    state: CopyState,
    bytes_written: u64,
    message: Option<String>,
) -> Result<CopyItemResult, SyncError> {
    transition(
        journal_path,
        &mut entry,
        state,
        bytes_written,
        message.clone(),
    )?;
    Ok(item_result(item, state, bytes_written, message))
}

fn item_result(
    item: &PlanItem,
    state: CopyState,
    bytes_written: u64,
    message: Option<String>,
) -> CopyItemResult {
    CopyItemResult {
        item_id: item.id.clone(),
        state,
        bytes_written,
        message,
    }
}

fn transition(
    journal_path: &Path,
    entry: &mut JournalEntry,
    state: CopyState,
    bytes_written: u64,
    message: Option<String>,
) -> Result<(), SyncError> {
    entry.state = state;
    entry.bytes_written = bytes_written;
    entry.message = message;
    append_journal(journal_path, entry)
}

fn append_journal(path: &Path, entry: &JournalEntry) -> Result<(), SyncError> {
    let parent = path.parent().ok_or_else(|| {
        SyncError::new(
            SyncErrorCode::JournalUnavailable,
            "The journal path has no parent folder.",
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("Could not create the journal folder", error))?;
    let mut journal = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error("Could not open the copy journal", error))?;
    serde_json::to_writer(&mut journal, entry)
        .map_err(|error| journal_error("Could not serialize a journal entry", error))?;
    journal
        .write_all(b"\n")
        .and_then(|()| journal.flush())
        .and_then(|()| journal.sync_data())
        .map_err(|error| io_error("Could not persist a journal entry", error))
}

/// Report unfinished journal entries and app-owned temporary files without deleting anything.
pub fn inspect_recovery(
    destination_root: &Path,
    journal_path: &Path,
) -> Result<RecoveryReport, SyncError> {
    let mut latest_entries: HashMap<(String, String), JournalEntry> = HashMap::new();
    for entry in read_journal(journal_path, TailPolicy::Inspect)? {
        latest_entries.insert((entry.run_id.clone(), entry.item_id.clone()), entry);
    }

    let interrupted_items = latest_entries
        .into_values()
        .filter(|entry| {
            !matches!(
                entry.state,
                CopyState::Done | CopyState::Skipped | CopyState::NeedsRescan
            )
        })
        .collect();
    let mut orphaned_temp_paths = Vec::new();
    for entry in WalkDir::new(destination_root).follow_links(false) {
        let entry = match entry {
            Ok(entry) if entry.file_type().is_file() => entry,
            Ok(_) => continue,
            Err(_) => continue,
        };
        if entry
            .file_name()
            .to_string_lossy()
            .contains(TEMP_FILE_MARKER)
            && let Ok(relative) = entry.path().strip_prefix(destination_root)
            && let Some(relative) = relative.to_str()
        {
            orphaned_temp_paths.push(relative.to_string());
        }
    }
    orphaned_temp_paths.sort();
    let mut saved_run_paths = Vec::new();
    let parent = journal_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_file())
                || !path.to_string_lossy().ends_with(".syncwise-run.json")
            {
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            let Ok(run) = serde_json::from_reader::<_, SavedRun>(file) else {
                continue;
            };
            if fs::canonicalize(&run.journal_path)
                .ok()
                .zip(fs::canonicalize(journal_path).ok())
                .is_some_and(|(saved, requested)| saved == requested)
                && let Some(path) = path.to_str()
            {
                saved_run_paths.push(path.to_string());
            }
        }
    }
    saved_run_paths.sort();
    Ok(RecoveryReport {
        interrupted_items,
        orphaned_temp_paths,
        saved_run_paths,
    })
}

fn preview_result(item: &PlanItem, decisions: &HashMap<String, Decision>) -> CopyItemResult {
    if matches!(
        item.kind,
        PlanItemKind::Copy | PlanItemKind::PossibleDuplicate
    ) && decisions.get(&item.id) == Some(&Decision::Approve)
    {
        item_result(
            item,
            CopyState::Planned,
            0,
            Some("Dry run; no files were changed.".into()),
        )
    } else {
        let message = item.reason.clone().or_else(|| {
            Some(match decisions.get(&item.id) {
                Some(Decision::Reject) => "Not copied because the match was rejected.".into(),
                Some(Decision::Defer) => "Not copied because the decision was deferred.".into(),
                _ => "Not copied because it was not explicitly approved.".into(),
            })
        });
        item_result(item, CopyState::Skipped, 0, message)
    }
}

/// Read a run's journal records without modifying the file; an incomplete final record is ignored.
pub fn read_run_journal(path: &Path, run_id: &str) -> Result<Vec<JournalEntry>, SyncError> {
    Ok(read_journal(path, TailPolicy::Inspect)?
        .into_iter()
        .filter(|entry| entry.run_id == run_id)
        .collect())
}

pub fn new_run_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!(
        "run-{timestamp}-{}",
        NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn io_error(context: &str, error: std::io::Error) -> SyncError {
    SyncError::new(SyncErrorCode::IoError, &format!("{context}: {error}"))
}

fn journal_error(context: &str, error: serde_json::Error) -> SyncError {
    SyncError::new(
        SyncErrorCode::JournalUnavailable,
        &format!("{context}: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        fs,
        io::Write,
        sync::atomic::{AtomicBool, Ordering},
    };

    use crate::{
        CopyState, Decision, JournalEntry, Plan, PlanItemKind, copy_approved_items, generate_plan,
        inspect_recovery, scan_directory,
    };

    fn scan(root: &Path) -> crate::ScanResult {
        scan_directory(root, &AtomicBool::new(false), |_| {}).unwrap()
    }

    fn approved_copy_decisions(plan: &Plan) -> HashMap<String, Decision> {
        plan.items
            .iter()
            .filter(|item| item.kind == PlanItemKind::Copy)
            .map(|item| (item.id.clone(), Decision::Approve))
            .collect()
    }

    fn plan_for(source_root: &Path, destination_root: &Path) -> Plan {
        generate_plan(&scan(source_root), &scan(destination_root)).unwrap()
    }

    #[test]
    fn approved_copy_finishes_from_temp_and_journals_done() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(source_root.join("nested")).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("nested/file.bin"), b"approved payload").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let journal = workspace.path().join("state/journal.jsonl");

        let mut progress_states = Vec::new();
        let report = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &journal,
            |progress| progress_states.push(progress.state),
        )
        .unwrap();

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].state, CopyState::Done);
        assert!(progress_states.contains(&CopyState::Copying));
        assert_eq!(progress_states.last(), Some(&CopyState::Done));
        assert_eq!(
            fs::read(destination_root.join("nested/file.bin")).unwrap(),
            b"approved payload"
        );
        assert_eq!(
            fs::read(source_root.join("nested/file.bin")).unwrap(),
            b"approved payload"
        );
        let journal_text = fs::read_to_string(journal).unwrap();
        assert!(journal_text.contains("\"state\":\"done\""));
        assert!(journal_text.contains("\"state\":\"finalizing\""));
        assert_eq!(
            fs::read_dir(destination_root.join("nested"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn resumes_cancelled_run_skipping_done_items_and_repairs_torn_tail() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("a.txt"), b"done").unwrap();
        fs::write(source_root.join("b.bin"), vec![3; COPY_BUFFER_SIZE * 2]).unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let journal = workspace.path().join("state/journal.jsonl");
        let cancellation = AtomicBool::new(false);
        let partial = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &cancellation,
            &journal,
            |progress| {
                if progress.item_id == plan.items[1].id {
                    cancellation.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert_eq!(partial.items[0].state, CopyState::Done);
        assert_eq!(partial.items[1].state, CopyState::Cancelled);
        OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"{\"runId\":")
            .unwrap();
        let snapshot = run_snapshot_path(&journal, &partial.run_id);
        let torn_journal = fs::read(&journal).unwrap();
        let preview = resume_run(&snapshot, true, &AtomicBool::new(false), |_| {
            panic!("dry run")
        })
        .unwrap();
        assert_eq!(preview.items[0].state, CopyState::Done);
        assert_eq!(fs::read(&journal).unwrap(), torn_journal);
        inspect_recovery(&destination_root, &journal).unwrap();
        assert_eq!(fs::read(&journal).unwrap(), torn_journal);
        let mut transferred = Vec::new();
        let resumed = resume_run(&snapshot, false, &AtomicBool::new(false), |progress| {
            transferred.push(progress.item_id);
        })
        .unwrap();
        assert_eq!(resumed.run_id, partial.run_id);
        assert!(
            resumed
                .items
                .iter()
                .all(|item| item.state == CopyState::Done)
        );
        assert!(!transferred.contains(&plan.items[0].id));
        assert_eq!(
            fs::read(destination_root.join("b.bin")).unwrap(),
            fs::read(source_root.join("b.bin")).unwrap()
        );
        for line in fs::read_to_string(&journal).unwrap().lines() {
            serde_json::from_str::<JournalEntry>(line).unwrap();
        }
        let before = fs::read(&journal).unwrap();
        resume_run(&snapshot, false, &AtomicBool::new(false), |_| {
            panic!("already done")
        })
        .unwrap();
        assert_eq!(fs::read(&journal).unwrap(), before);
    }

    #[test]
    fn injected_disconnect_and_storage_full_stop_without_success() {
        struct FailingDrive {
            remaining: usize,
            kind: std::io::ErrorKind,
        }
        impl Write for FailingDrive {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::Error::new(self.kind, "injected drive failure"));
                }
                let count = self.remaining.min(data.len());
                self.remaining -= count;
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for kind in [
            std::io::ErrorKind::NotConnected,
            std::io::ErrorKind::StorageFull,
        ] {
            let source = vec![9_u8; COPY_BUFFER_SIZE * 3];
            let mut reader = std::io::Cursor::new(source);
            let mut drive = FailingDrive {
                remaining: COPY_BUFFER_SIZE,
                kind,
            };
            let failure = transfer_chunks(&mut reader, &mut drive, &AtomicBool::new(false), |_| {})
                .unwrap_err();
            assert_eq!(failure.state, CopyState::Failed);
            assert_eq!(failure.bytes_written, COPY_BUFFER_SIZE as u64);
            assert!(failure.message.contains("injected drive failure"));
        }
    }

    #[test]
    fn new_run_never_truncates_an_existing_non_journal_file() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("file.txt"), b"content").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let journal = workspace.path().join("user-data.txt");
        fs::write(&journal, b"this is not a journal").unwrap();
        let result = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &journal,
            |_| {},
        );
        assert_eq!(result.unwrap_err().code, SyncErrorCode::JournalUnavailable);
        assert_eq!(fs::read(&journal).unwrap(), b"this is not a journal");
        assert!(!destination_root.join("file.txt").exists());
    }

    #[test]
    fn corrupt_complete_journal_record_is_not_silently_discarded() {
        let workspace = tempfile::tempdir().unwrap();
        let journal = workspace.path().join("journal.jsonl");
        fs::write(&journal, b"not-json\n").unwrap();
        for policy in [TailPolicy::Inspect, TailPolicy::Repair] {
            assert!(read_journal(&journal, policy).is_err());
            assert_eq!(fs::read(&journal).unwrap(), b"not-json\n");
        }
    }

    #[test]
    fn destination_appearing_after_preview_is_skipped_without_replacement() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("same.txt"), b"source bytes").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        fs::write(destination_root.join("same.txt"), b"destination wins").unwrap();

        let report = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &workspace.path().join("journal.jsonl"),
            |_| {},
        )
        .unwrap();

        assert_eq!(report.items[0].state, CopyState::Skipped);
        assert_eq!(
            fs::read(destination_root.join("same.txt")).unwrap(),
            b"destination wins"
        );
    }

    #[test]
    fn changed_source_requires_a_rescan() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("changed.txt"), b"old").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        fs::write(source_root.join("changed.txt"), b"new content").unwrap();

        let report = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &workspace.path().join("journal.jsonl"),
            |_| {},
        )
        .unwrap();

        assert_eq!(report.items[0].state, CopyState::NeedsRescan);
        assert!(!destination_root.join("changed.txt").exists());
    }

    #[test]
    fn explicit_approval_can_copy_a_possible_duplicate_without_replacing() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(destination_root.join("folder")).unwrap();
        fs::write(source_root.join("song.mp3"), b"new recording").unwrap();
        fs::write(
            destination_root.join("folder/song.mp3"),
            b"existing recording",
        )
        .unwrap();
        let plan = plan_for(&source_root, &destination_root);
        assert_eq!(plan.items[0].kind, PlanItemKind::PossibleDuplicate);
        let decisions = HashMap::from([(plan.items[0].id.clone(), Decision::Approve)]);

        let report = copy_approved_items(
            &plan,
            &decisions,
            false,
            &AtomicBool::new(false),
            &workspace.path().join("journal.jsonl"),
            |_| {},
        )
        .unwrap();

        assert_eq!(report.items[0].state, CopyState::Done);
        assert_eq!(
            fs::read(destination_root.join("song.mp3")).unwrap(),
            b"new recording"
        );
        assert_eq!(
            fs::read(destination_root.join("folder/song.mp3")).unwrap(),
            b"existing recording"
        );
    }

    #[test]
    fn cancellation_discards_temp_and_does_not_create_final_path() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        let mut source = File::create(source_root.join("large.bin")).unwrap();
        source
            .write_all(&vec![42_u8; COPY_BUFFER_SIZE * 32])
            .unwrap();
        drop(source);
        let plan = plan_for(&source_root, &destination_root);
        let cancellation = AtomicBool::new(false);

        let report = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &cancellation,
            &workspace.path().join("journal.jsonl"),
            |_| cancellation.store(true, Ordering::Relaxed),
        )
        .unwrap();

        assert!(report.cancelled, "copy report: {report:?}");
        assert_eq!(report.items[0].state, CopyState::Cancelled);
        assert!(!destination_root.join("large.bin").exists());
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
    }

    #[test]
    fn dry_run_does_not_write_journal_or_destination_files() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("dry.txt"), b"dry run").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let journal = workspace.path().join("journal.jsonl");

        let report = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            true,
            &AtomicBool::new(false),
            &journal,
            |_| panic!("dry run must not report transfer progress"),
        )
        .unwrap();

        assert!(report.dry_run);
        assert_eq!(report.items[0].state, CopyState::Planned);
        assert!(!journal.exists());
        assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
    }

    #[test]
    fn no_clobber_finalize_preserves_existing_destination_bytes() {
        let destination_root = tempfile::tempdir().unwrap();
        let destination = destination_root.path().join("existing.txt");
        fs::write(&destination, b"original destination").unwrap();
        let mut temporary = Builder::new().tempfile_in(destination_root.path()).unwrap();
        temporary.write_all(b"replacement attempt").unwrap();

        let error = temporary.persist_noclobber(&destination).unwrap_err();
        assert_eq!(error.error.kind(), std::io::ErrorKind::AlreadyExists);
        drop(error.file);
        assert_eq!(fs::read(destination).unwrap(), b"original destination");
    }

    #[test]
    fn recovery_lists_incomplete_items_and_temps_without_deleting_them() {
        let workspace = tempfile::tempdir().unwrap();
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&destination_root).unwrap();
        let orphan = destination_root.join(".lost.syncwise-part-orphan.tmp");
        fs::write(&orphan, b"partial").unwrap();
        let journal = workspace.path().join("journal.jsonl");
        let mut entry = JournalEntry {
            run_id: "run-1".into(),
            item_id: "item-1".into(),
            source_relative_path: "file.bin".into(),
            destination_relative_path: "file.bin".into(),
            state: CopyState::Copying,
            temp_relative_path: Some(".file.bin.syncwise-part-orphan.tmp".into()),
            bytes_written: 7,
            message: None,
        };
        append_journal(&journal, &entry).unwrap();
        entry.state = CopyState::Done;
        entry.temp_relative_path = None;
        append_journal(&journal, &entry).unwrap();
        entry.item_id = "item-2".into();
        entry.state = CopyState::Copying;
        append_journal(&journal, &entry).unwrap();

        let recovery = inspect_recovery(&destination_root, &journal).unwrap();

        assert_eq!(recovery.interrupted_items.len(), 1);
        assert_eq!(recovery.interrupted_items[0].item_id, "item-2");
        assert_eq!(recovery.orphaned_temp_paths.len(), 1);
        assert!(orphan.exists());
    }

    #[test]
    fn refuses_a_journal_path_inside_the_read_only_source() {
        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("file.txt"), b"content").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let source_journal = source_root.join(".syncwise/journal.jsonl");

        let result = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &source_journal,
            |_| {},
        );

        assert_eq!(result.unwrap_err().code, SyncErrorCode::InvalidInput);
        assert!(!source_root.join(".syncwise").exists());
        assert!(!destination_root.join("file.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_dangling_journal_symlink_pointing_into_the_source() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let source_root = workspace.path().join("source");
        let destination_root = workspace.path().join("destination");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(source_root.join("file.txt"), b"content").unwrap();
        let plan = plan_for(&source_root, &destination_root);
        let journal_link = workspace.path().join("journal-link.jsonl");
        symlink(source_root.join("future-journal.jsonl"), &journal_link).unwrap();

        let result = copy_approved_items(
            &plan,
            &approved_copy_decisions(&plan),
            false,
            &AtomicBool::new(false),
            &journal_link,
            |_| {},
        );

        assert_eq!(result.unwrap_err().code, SyncErrorCode::InvalidInput);
        assert!(!source_root.join("future-journal.jsonl").exists());
        assert!(!destination_root.join("file.txt").exists());
    }
}
