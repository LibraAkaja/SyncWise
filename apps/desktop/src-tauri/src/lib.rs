use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::Serialize;
use tauri::{Emitter, State};

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TaskProgress {
    task_id: String,
    progress: u8,
    status: &'static str,
}

#[tauri::command]
fn start_demo_task(app: tauri::AppHandle, tasks: State<'_, TaskManager>) -> Result<String, String> {
    let task_id = format!("task-{}", NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed));
    let cancelled = Arc::new(AtomicBool::new(false));
    tasks
        .tasks
        .lock()
        .map_err(|_| "Task manager is unavailable".to_string())?
        .insert(task_id.clone(), cancelled.clone());

    let task_manager = tasks.inner().clone();
    let task_id_for_task = task_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        for progress in 0..=100 {
            if cancelled.load(Ordering::Relaxed) {
                let _ = app.emit(
                    "syncwise://task-progress",
                    TaskProgress {
                        task_id: task_id_for_task.clone(),
                        progress,
                        status: "cancelled",
                    },
                );
                break;
            }

            let status = if progress == 100 {
                "completed"
            } else {
                "running"
            };
            if app
                .emit(
                    "syncwise://task-progress",
                    TaskProgress {
                        task_id: task_id_for_task.clone(),
                        progress,
                        status,
                    },
                )
                .is_err()
                || progress == 100
            {
                break;
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        if let Ok(mut tasks) = task_manager.tasks.lock() {
            tasks.remove(&task_id_for_task);
        }
    });

    Ok(task_id)
}

#[tauri::command]
fn cancel_demo_task(task_id: String, tasks: State<'_, TaskManager>) -> Result<bool, String> {
    let cancellation = tasks
        .tasks
        .lock()
        .map_err(|_| "Task manager is unavailable".to_string())?
        .get(&task_id)
        .cloned();

    if let Some(cancellation) = cancellation {
        cancellation.store(true, Ordering::Relaxed);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(TaskManager::default())
        .invoke_handler(tauri::generate_handler![start_demo_task, cancel_demo_task])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
