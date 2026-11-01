use std::{collections::HashMap, fs, sync::atomic::AtomicBool};

use syncwise_engine::{
    CopyState, Decision, RunStatus, Store, copy_approved_items_as, generate_plan, new_run_id,
    read_run_journal, resume_run, run_snapshot_path, scan_directory,
};

#[test]
fn interrupted_run_is_found_after_restart_and_resumed_without_recopying() {
    let root = tempfile::tempdir().unwrap();
    let (source, destination, state) = (
        root.path().join("source"),
        root.path().join("destination"),
        root.path().join("state"),
    );
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    for name in ["a.bin", "b.bin", "c.bin"] {
        fs::write(source.join(name), name.as_bytes()).unwrap();
    }
    let idle = AtomicBool::new(false);
    let source_scan = scan_directory(&source, &idle, |_| {}).unwrap();
    let destination_scan = scan_directory(&destination, &idle, |_| {}).unwrap();
    let plan = generate_plan(&source_scan, &destination_scan).unwrap();
    let decisions: HashMap<_, _> = plan
        .items
        .iter()
        .map(|item| (item.id.clone(), Decision::Approve))
        .collect();

    let run_id = new_run_id();
    let journal = state.join("runs").join(&run_id).join("journal.jsonl");
    let snapshot = run_snapshot_path(&journal, &run_id);
    let db = state.join("syncwise.sqlite");
    let items: Vec<_> = plan.items.iter().collect();
    Store::open(&db)
        .unwrap()
        .begin_run(&run_id, None, &plan, &items, &journal, &snapshot)
        .unwrap();

    // Simulate a kill after one complete file: stop during the next file and skip finish_run.
    let kill = AtomicBool::new(false);
    let mut first_item = None;
    let report = copy_approved_items_as(
        run_id.clone(),
        &plan,
        &decisions,
        false,
        &kill,
        &journal,
        |progress| {
            if let Some(first) = &first_item {
                if *first != progress.item_id {
                    kill.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                first_item = Some(progress.item_id);
            }
        },
    )
    .unwrap();
    assert!(report.cancelled);

    let mut store = Store::open(&db).unwrap();
    let interrupted = store.list_runs(&[RunStatus::Running], None, 5).unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].snapshot_path, snapshot.to_str().unwrap());
    let journal_entries = read_run_journal(&journal, &run_id).unwrap();
    let recovered = store.recover_run(&run_id, &journal_entries).unwrap();
    assert_eq!(recovered.done_items, 1);
    assert_eq!(recovered.status, RunStatus::Running);

    let resumed = resume_run(&snapshot, false, &idle, |_| {}).unwrap();
    let journal_entries = read_run_journal(&journal, &run_id).unwrap();
    let summary = store
        .finish_run(&run_id, &resumed, &journal_entries)
        .unwrap();
    assert_eq!(summary.status, RunStatus::Completed);
    assert_eq!(summary.done_items, 3);
    for name in ["a.bin", "b.bin", "c.bin"] {
        assert_eq!(fs::read(destination.join(name)).unwrap(), name.as_bytes());
    }
    assert!(
        store
            .list_runs(&[RunStatus::Running], None, 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        journal_entries
            .iter()
            .any(|entry| entry.state == CopyState::Done)
    );
}
