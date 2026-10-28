use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use syncwise_engine::{
    DecisionState, FolderPair, Plan, ScanProgress, ScanResult, SyncError,
    generate_plan_with_cancellation, scan_directory, validate_folder_pair as validate_folder_roots,
};
use tauri::Manager;
use tauri::{AppHandle, Emitter, State};

mod review;

static NEXT_SCAN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
struct ScanTaskManager {
    tasks: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScanProgressEvent {
    scan_id: String,
    side: String,
    progress: ScanProgress,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScanCompletionEvent {
    scan_id: String,
    source: Option<ScanResult>,
    destination: Option<ScanResult>,
    error: Option<SyncError>,
    cancelled: bool,
    plan: Option<Plan>,
    decisions: Option<DecisionState>,
}

#[tauri::command]
fn validate_folder_pair(
    source_path: String,
    destination_path: String,
) -> Result<FolderPair, SyncError> {
    validate_folder_roots(Path::new(&source_path), Path::new(&destination_path))
}

#[tauri::command]
fn start_scan(
    app: AppHandle,
    tasks: State<'_, ScanTaskManager>,
    source_path: String,
    destination_path: String,
) -> Result<String, SyncError> {
    let folder_pair = validate_folder_roots(Path::new(&source_path), Path::new(&destination_path))?;
    app.state::<Mutex<review::ReviewStore>>()
        .lock()
        .map_err(|_| SyncError {
            code: syncwise_engine::SyncErrorCode::InvalidInput,
            message: "Review state is unavailable.".into(),
        })?
        .session = None;
    let scan_id = format!("scan-{}", NEXT_SCAN_ID.fetch_add(1, Ordering::Relaxed));
    let cancellation = Arc::new(AtomicBool::new(false));
    tasks
        .tasks
        .lock()
        .map_err(|_| SyncError {
            code: syncwise_engine::SyncErrorCode::IoError,
            message: "Scan task manager is unavailable.".into(),
        })?
        .insert(scan_id.clone(), cancellation.clone());

    let task_manager = tasks.inner().clone();
    let event_scan_id = scan_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut source = None;
        let mut destination = None;
        let mut error = None;

        match scan_one(
            &app,
            &event_scan_id,
            "source",
            &folder_pair.source_root,
            &cancellation,
        ) {
            Ok(result) => {
                let source_cancelled = result.cancelled;
                source = Some(result);
                if !source_cancelled {
                    match scan_one(
                        &app,
                        &event_scan_id,
                        "destination",
                        &folder_pair.destination_root,
                        &cancellation,
                    ) {
                        Ok(result) => destination = Some(result),
                        Err(scan_error) => error = Some(scan_error),
                    }
                }
            }
            Err(scan_error) => error = Some(scan_error),
        }

        let mut cancelled = cancellation.load(Ordering::Relaxed)
            || source.as_ref().is_some_and(|result| result.cancelled)
            || destination.as_ref().is_some_and(|result| result.cancelled);
        let mut plan = None;
        let mut decisions = None;
        if !cancelled
            && error.is_none()
            && let (Some(source), Some(destination)) = (&source, &destination)
        {
            let result = generate_plan_with_cancellation(source, destination, &cancellation)
                .and_then(|generated| {
                    let store = app.state::<Mutex<review::ReviewStore>>();
                    let state =
                        review::install(&app, &store, generated.clone(), &destination.files)?;
                    decisions = Some(state);
                    Ok(generated)
                });
            match result {
                Ok(generated) => plan = Some(generated),
                Err(failure) => error = Some(failure),
            }
        }
        if cancellation.load(Ordering::Relaxed) {
            cancelled = true;
            plan = None;
            decisions = None;
            error = None;
        }
        let _ = app.emit(
            "syncwise://scan-complete",
            ScanCompletionEvent {
                scan_id: event_scan_id.clone(),
                source,
                destination,
                error,
                cancelled,
                plan,
                decisions,
            },
        );

        if let Ok(mut tasks) = task_manager.tasks.lock() {
            tasks.remove(&event_scan_id);
        }
    });

    Ok(scan_id)
}

#[tauri::command]
fn cancel_scan(scan_id: String, tasks: State<'_, ScanTaskManager>) -> Result<bool, String> {
    let cancellation = tasks
        .tasks
        .lock()
        .map_err(|_| "Scan task manager is unavailable".to_string())?
        .get(&scan_id)
        .cloned();

    if let Some(cancellation) = cancellation {
        cancellation.store(true, Ordering::Relaxed);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn scan_one(
    app: &AppHandle,
    scan_id: &str,
    side: &str,
    root: &str,
    cancellation: &AtomicBool,
) -> Result<ScanResult, SyncError> {
    scan_directory(Path::new(root), cancellation, |progress| {
        let _ = app.emit(
            "syncwise://scan-progress",
            ScanProgressEvent {
                scan_id: scan_id.into(),
                side: side.into(),
                progress,
            },
        );
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ScanTaskManager::default())
        .manage(Mutex::new(review::ReviewStore::default()))
        .invoke_handler(tauri::generate_handler![
            validate_folder_pair,
            start_scan,
            cancel_scan,
            review::review_decisions,
            review::review_preflight,
            review::reset_review_memory
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
