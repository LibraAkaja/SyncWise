use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use syncwise_engine::{
    CopyOptions, CopyProgress, CopyReport, Decision, Plan, PlanItemKind, RecoveryReport,
    RunHistoryEntry, RunStatus, RunSummary, Store, SyncError, copy_approved_items_as_with_options,
    inspect_recovery, new_run_id, read_run_journal, resume_run, run_snapshot_path,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::review::{DATABASE_FILE, ReviewStore, active, ensure_outside_source, error};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const RESUMABLE: [RunStatus; 3] = [
    RunStatus::Running,
    RunStatus::NeedsAttention,
    RunStatus::Cancelled,
];

#[derive(Default)]
pub struct SyncState {
    active: Mutex<Option<(String, Arc<AtomicBool>)>>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SyncProgressEvent {
    run_id: String,
    item_id: String,
    path: String,
    item_bytes: u64,
    item_total: u64,
    completed_items: u64,
    total_items: u64,
    bytes_done: u64,
    bytes_total: u64,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SyncCompletionEvent {
    run_id: String,
    summary: Option<RunSummary>,
    error: Option<SyncError>,
}

enum Job {
    Fresh {
        plan: Arc<Plan>,
        decisions: HashMap<String, Decision>,
        verify_after_copy: bool,
    },
    Resume,
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, SyncError> {
    app.path()
        .app_data_dir()
        .map_err(|_| error("Local application state directory is unavailable."))
}

fn open_store(app: &AppHandle) -> Result<Store, SyncError> {
    Store::open(&data_dir(app)?.join(DATABASE_FILE))
}

fn claim(state: &SyncState, run_id: &str) -> Result<Arc<AtomicBool>, SyncError> {
    let mut active = state
        .active
        .lock()
        .map_err(|_| error("Sync state is unavailable."))?;
    if active.is_some() {
        return Err(error("A sync is already running."));
    }
    let cancellation = Arc::new(AtomicBool::new(false));
    *active = Some((run_id.into(), cancellation.clone()));
    Ok(cancellation)
}

fn release(state: &SyncState) {
    if let Ok(mut active) = state.active.lock() {
        *active = None;
    }
}

#[tauri::command]
pub fn start_sync(
    app: AppHandle,
    plan_id: String,
    revision: u32,
    verify_after_copy: bool,
    review: State<'_, Mutex<ReviewStore>>,
    sync: State<'_, SyncState>,
) -> Result<String, SyncError> {
    let (plan, decisions, scan_id) = {
        let review = review
            .lock()
            .map_err(|_| error("Review state is unavailable."))?;
        let session = active(&review, &plan_id)?;
        let current = session.decisions();
        if current.revision != revision || review.confirmed_revision != Some(revision) {
            return Err(error("Confirm the current plan before starting the sync."));
        }
        (session.plan.clone(), current.choices, review.scan_id)
    };
    let data_dir = data_dir(&app)?;
    ensure_outside_source(&data_dir, std::path::Path::new(&plan.source_root))?;
    let approved: Vec<_> = plan
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                PlanItemKind::Copy | PlanItemKind::PossibleDuplicate
            ) && decisions.get(&item.id) == Some(&Decision::Approve)
        })
        .collect();
    if approved.is_empty() {
        return Err(error("No approved files to copy."));
    }
    let run_id = new_run_id();
    let journal_path = data_dir.join("runs").join(&run_id).join("journal.jsonl");
    let snapshot_path = run_snapshot_path(&journal_path, &run_id);
    let cancellation = claim(&sync, &run_id)?;
    if let Err(failure) = open_store(&app).and_then(|mut store| {
        store.begin_run(
            &run_id,
            scan_id,
            &plan,
            &approved,
            &journal_path,
            &snapshot_path,
        )
    }) {
        release(&sync);
        return Err(failure);
    }
    let job = Job::Fresh {
        plan: plan.clone(),
        decisions,
        verify_after_copy,
    };
    spawn(
        app,
        run_id.clone(),
        journal_path,
        snapshot_path,
        cancellation,
        job,
    );
    Ok(run_id)
}

#[tauri::command]
pub fn resume_sync(
    app: AppHandle,
    run_id: String,
    sync: State<'_, SyncState>,
) -> Result<String, SyncError> {
    let mut store = open_store(&app)?;
    let run = store
        .get_run(&run_id)?
        .filter(|run| RESUMABLE.contains(&run.status))
        .ok_or_else(|| error("This run cannot be retried."))?;
    let cancellation = claim(&sync, &run_id)?;
    if let Err(failure) = store.restart_run(&run_id) {
        release(&sync);
        return Err(failure);
    }
    spawn(
        app,
        run_id.clone(),
        PathBuf::from(&run.journal_path),
        PathBuf::from(&run.snapshot_path),
        cancellation,
        Job::Resume,
    );
    Ok(run_id)
}

fn spawn(
    app: AppHandle,
    run_id: String,
    journal_path: PathBuf,
    snapshot_path: PathBuf,
    cancellation: Arc<AtomicBool>,
    job: Job,
) {
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = execute(
            &app,
            &run_id,
            &journal_path,
            &snapshot_path,
            &cancellation,
            job,
        );
        let event = match outcome {
            Ok(summary) => SyncCompletionEvent {
                run_id: run_id.clone(),
                summary: Some(summary),
                error: None,
            },
            Err(failure) => {
                let (summary, error) = match open_store(&app)
                    .and_then(|mut store| store.fail_run(&run_id, &failure.message))
                {
                    Ok(summary) => (Some(summary), failure),
                    Err(persist_error) => (
                        None,
                        SyncError {
                            code: syncwise_engine::SyncErrorCode::StoreUnavailable,
                            message: format!(
                                "{} Run status could not be persisted: {}",
                                failure.message, persist_error.message
                            ),
                        },
                    ),
                };
                SyncCompletionEvent {
                    run_id: run_id.clone(),
                    summary,
                    error: Some(error),
                }
            }
        };
        release(&app.state::<SyncState>());
        let _ = app.emit("syncwise://sync-complete", event);
    });
}

fn execute(
    app: &AppHandle,
    run_id: &str,
    journal_path: &std::path::Path,
    snapshot_path: &std::path::Path,
    cancellation: &AtomicBool,
    job: Job,
) -> Result<RunSummary, SyncError> {
    let store = open_store(app)?;
    let paths = store.item_paths(run_id)?;
    let (total_items, bytes_total) = store.pending_totals(run_id)?;
    drop(store);
    let mut tracker = ProgressTracker {
        paths,
        total_items,
        bytes_total,
        completed_items: 0,
        bytes_done: 0,
        last_emit: None,
    };
    let on_progress = |progress: CopyProgress| tracker.record(app, progress);
    let report: CopyReport = match job {
        Job::Fresh {
            plan,
            decisions,
            verify_after_copy,
        } => copy_approved_items_as_with_options(
            run_id.into(),
            &plan,
            &decisions,
            CopyOptions {
                dry_run: false,
                verify_after_copy,
            },
            cancellation,
            journal_path,
            on_progress,
        )?,
        Job::Resume => resume_run(snapshot_path, false, cancellation, on_progress)?,
    };
    let journal = read_run_journal(journal_path, run_id)?;
    open_store(app)?.finish_run(run_id, &report, &journal)
}

struct ProgressTracker {
    paths: HashMap<String, String>,
    total_items: u64,
    bytes_total: u64,
    completed_items: u64,
    bytes_done: u64,
    last_emit: Option<Instant>,
}

impl ProgressTracker {
    fn record(&mut self, app: &AppHandle, progress: CopyProgress) {
        let copying = progress.state == syncwise_engine::CopyState::Copying;
        if !copying {
            if progress.state == syncwise_engine::CopyState::Done {
                self.completed_items += 1;
                self.bytes_done += progress.total_bytes;
            }
        } else if self
            .last_emit
            .is_some_and(|last| last.elapsed() < PROGRESS_INTERVAL)
        {
            return;
        }
        self.last_emit = Some(Instant::now());
        let bytes_in_flight = if copying { progress.bytes_written } else { 0 };
        let _ = app.emit(
            "syncwise://sync-progress",
            SyncProgressEvent {
                run_id: progress.run_id,
                path: self
                    .paths
                    .get(&progress.item_id)
                    .cloned()
                    .unwrap_or_default(),
                item_id: progress.item_id,
                item_bytes: progress.bytes_written,
                item_total: progress.total_bytes,
                completed_items: self.completed_items,
                total_items: self.total_items,
                bytes_done: self.bytes_done + bytes_in_flight,
                bytes_total: self.bytes_total,
            },
        );
    }
}

#[tauri::command]
pub fn get_run(app: AppHandle, run_id: String) -> Result<RunSummary, SyncError> {
    open_store(&app)?
        .get_run(&run_id)?
        .ok_or_else(|| error("The run no longer exists."))
}

#[tauri::command]
pub fn list_run_history(app: AppHandle) -> Result<Vec<RunHistoryEntry>, SyncError> {
    open_store(&app)?.list_run_history(100)
}

#[tauri::command]
pub fn cancel_sync(run_id: String, sync: State<'_, SyncState>) -> Result<bool, SyncError> {
    let active = sync
        .active
        .lock()
        .map_err(|_| error("Sync state is unavailable."))?;
    Ok(match active.as_ref() {
        Some((active_id, cancellation)) if *active_id == run_id => {
            cancellation.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    })
}

#[tauri::command]
pub async fn list_interrupted_runs(
    app: AppHandle,
    sync: State<'_, SyncState>,
) -> Result<Vec<RunSummary>, SyncError> {
    let running = sync
        .active
        .lock()
        .map_err(|_| error("Sync state is unavailable."))?
        .as_ref()
        .map(|(id, _)| id.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let mut store = open_store(&app)?;
        let mut recovered = HashSet::new();
        loop {
            let runs = store.list_runs(&RESUMABLE, running.as_deref(), 5)?;
            let Some(run) = runs
                .into_iter()
                .find(|run| !recovered.contains(&run.run_id))
            else {
                break;
            };
            recovered.insert(run.run_id.clone());
            let journal = read_run_journal(std::path::Path::new(&run.journal_path), &run.run_id)?;
            store.recover_run(&run.run_id, &journal)?;
        }
        store.list_runs(&RESUMABLE, running.as_deref(), 5)
    })
    .await
    .map_err(|_| error("Startup recovery task failed."))?
}

#[tauri::command]
pub fn abandon_run(
    app: AppHandle,
    run_id: String,
    sync: State<'_, SyncState>,
) -> Result<(), SyncError> {
    if sync
        .active
        .lock()
        .map_err(|_| error("Sync state is unavailable."))?
        .as_ref()
        .is_some_and(|(id, _)| *id == run_id)
    {
        return Err(error("This run is still active."));
    }
    open_store(&app)?.abandon_run(&run_id)
}

/// Read-only report of unfinished journal entries and leftover temporary files; nothing is deleted.
#[tauri::command]
pub async fn inspect_run(app: AppHandle, run_id: String) -> Result<RecoveryReport, SyncError> {
    let run = open_store(&app)?
        .get_run(&run_id)?
        .ok_or_else(|| error("The run no longer exists."))?;
    tauri::async_runtime::spawn_blocking(move || {
        inspect_recovery(
            std::path::Path::new(&run.destination_root),
            std::path::Path::new(&run.journal_path),
        )
    })
    .await
    .map_err(|_| error("Inspection task failed."))?
}
