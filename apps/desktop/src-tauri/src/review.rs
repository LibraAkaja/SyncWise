use std::{collections::HashSet, fs, path::PathBuf, sync::Mutex};

use syncwise_engine::{
    DecisionChange, DecisionState, FileEntry, Plan, ReviewPreflight, ReviewSession, SyncError,
    SyncErrorCode,
};
use tauri::{AppHandle, Manager, State};

#[derive(Default)]
pub struct ReviewStore {
    pub session: Option<ReviewSession>,
    memory: HashSet<String>,
    memory_path: Option<PathBuf>,
}

pub fn install(
    app: &AppHandle,
    store: &Mutex<ReviewStore>,
    plan: Plan,
    destination: &[FileEntry],
) -> Result<DecisionState, SyncError> {
    let mut store = store
        .lock()
        .map_err(|_| error("Review state is unavailable."))?;
    if store.memory_path.is_none() {
        let path = app
            .path()
            .app_data_dir()
            .map_err(|_| error("Local application state directory is unavailable."))?
            .join("rejected-matches.json");
        store.memory = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data).map_err(|_| {
                error("Saved rejection memory is invalid. Review the local state file.")
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(_) => return Err(error("Saved rejection memory could not be read.")),
        };
        store.memory_path = Some(path);
    }
    let session = ReviewSession::new(plan, destination, &store.memory);
    let decisions = session.decisions();
    store.session = Some(session);
    Ok(decisions)
}

#[tauri::command]
pub fn review_decisions(
    plan_id: String,
    revision: u32,
    changes: Vec<DecisionChange>,
    bulk: bool,
    store: State<'_, Mutex<ReviewStore>>,
) -> Result<DecisionState, SyncError> {
    let mut store = store
        .lock()
        .map_err(|_| error("Review state is unavailable."))?;
    let mut session = active(&store, &plan_id)?.clone();
    let state = session.apply_changes(revision, &changes, bulk)?;
    let mut memory = store.memory.clone();
    session.remember_changes(&changes, &mut memory);
    persist_memory(&store, &memory)?;
    store.memory = memory;
    store.session = Some(session);
    Ok(state)
}

#[tauri::command]
pub fn reset_review_memory(
    plan_id: String,
    store: State<'_, Mutex<ReviewStore>>,
) -> Result<(), SyncError> {
    let mut store = store
        .lock()
        .map_err(|_| error("Review state is unavailable."))?;
    active(&store, &plan_id)?;
    persist_memory(&store, &HashSet::new())?;
    store.memory.clear();
    Ok(())
}

#[tauri::command]
pub async fn review_preflight(
    plan_id: String,
    revision: u32,
    store: State<'_, Mutex<ReviewStore>>,
) -> Result<ReviewPreflight, SyncError> {
    let session = {
        let store = store
            .lock()
            .map_err(|_| error("Review state is unavailable."))?;
        let session = active(&store, &plan_id)?;
        if session.decisions().revision != revision {
            return Err(error(
                "Review decisions have changed. Check preflight again.",
            ));
        }
        session.clone()
    };
    tauri::async_runtime::spawn_blocking(move || session.preflight())
        .await
        .map_err(|_| error("Preflight task failed."))?
}

fn active<'a>(store: &'a ReviewStore, plan_id: &str) -> Result<&'a ReviewSession, SyncError> {
    store
        .session
        .as_ref()
        .filter(|session| session.plan.id == plan_id)
        .ok_or_else(|| error("This review plan is no longer active. Scan the folders again."))
}

fn persist_memory(store: &ReviewStore, memory: &HashSet<String>) -> Result<(), SyncError> {
    let path = store
        .memory_path
        .as_ref()
        .ok_or_else(|| error("Rejection memory has not been initialized."))?;
    let parent = path
        .parent()
        .ok_or_else(|| error("Invalid rejection memory directory."))?;
    let source = store
        .session
        .as_ref()
        .map(|session| PathBuf::from(&session.plan.source_root))
        .ok_or_else(|| error("No active review plan."))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| error("Local state path cannot be resolved safely."))?;
    }
    let resolved =
        fs::canonicalize(existing).map_err(|_| error("Local state path cannot be inspected."))?;
    if resolved.starts_with(&source) {
        return Err(error(
            "Local review state is inside the source folder. Choose a narrower source folder to keep it read-only.",
        ));
    }
    fs::create_dir_all(parent).map_err(|_| error("Could not create the local state directory."))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| error("Could not prepare rejection memory."))?;
    serde_json::to_writer(&mut temporary, memory)
        .map_err(|_| error("Could not serialize rejection memory."))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| error("Could not persist rejection memory."))?;
    temporary
        .persist(path)
        .map_err(|_| error("Could not save rejection memory; decisions were not changed."))?;
    Ok(())
}

fn error(message: &str) -> SyncError {
    SyncError {
        code: SyncErrorCode::InvalidInput,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use syncwise_engine::{generate_plan, scan_directory};

    fn fixture() -> (tempfile::TempDir, ReviewStore) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("file.txt"), b"content").unwrap();
        let source = scan_directory(&source, &AtomicBool::new(false), |_| {}).unwrap();
        let destination = scan_directory(&destination, &AtomicBool::new(false), |_| {}).unwrap();
        let plan = generate_plan(&source, &destination).unwrap();
        let store = ReviewStore {
            session: Some(ReviewSession::new(
                plan,
                &destination.files,
                &HashSet::new(),
            )),
            memory: HashSet::new(),
            memory_path: Some(root.path().join("state/rejected.json")),
        };
        (root, store)
    }

    #[test]
    fn rejection_memory_round_trips_and_replaces_atomically() {
        let (_root, store) = fixture();
        let memory = HashSet::from(["a reviewed pair".to_string()]);
        persist_memory(&store, &memory).unwrap();
        let restored: HashSet<String> =
            serde_json::from_slice(&fs::read(store.memory_path.as_ref().unwrap()).unwrap())
                .unwrap();
        assert_eq!(restored, memory);
        persist_memory(&store, &HashSet::new()).unwrap();
        let restored: HashSet<String> =
            serde_json::from_slice(&fs::read(store.memory_path.as_ref().unwrap()).unwrap())
                .unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn review_state_never_writes_inside_the_source_tree() {
        let (root, mut store) = fixture();
        store.memory_path = Some(root.path().join("source/new-state/rejected.json"));
        assert!(persist_memory(&store, &HashSet::new()).is_err());
        assert!(!root.path().join("source/new-state").exists());
    }
}
