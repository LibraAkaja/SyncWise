use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{atomic::AtomicBool, mpsc},
    time::Duration,
};

use serde_json::Value;

fn run_cli(arguments: &[&str]) -> Output {
    let source = match arguments.first().copied() {
        Some("copy" | "plan") => arguments.get(1).map(PathBuf::from),
        Some("resume") => arguments
            .get(1)
            .and_then(|path| fs::read(path).ok())
            .and_then(|data| serde_json::from_slice::<Value>(&data).ok())
            .and_then(|run| run["plan"]["sourceRoot"].as_str().map(PathBuf::from)),
        _ => None,
    };
    let before = source.as_deref().map(tree_hash);
    let output = Command::new(env!("CARGO_BIN_EXE_syncwise-cli"))
        .args(arguments)
        .output()
        .unwrap();
    assert_eq!(
        before,
        source.as_deref().map(tree_hash),
        "CLI changed the source tree"
    );
    output
}

fn tree_hash(root: &Path) -> blake3::Hash {
    fn visit(root: &Path, path: &Path, hash: &mut blake3::Hasher) {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            hash.update(
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes(),
            );
            if entry.file_type().unwrap().is_dir() {
                hash.update(b"directory");
                visit(root, &entry.path(), hash);
            } else {
                hash.update(b"file");
                hash.update(&fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut hash = blake3::Hasher::new();
    visit(root, root, &mut hash);
    hash.finalize()
}

fn fixture_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("source");
    let destination = workspace.path().join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    (workspace, source, destination)
}

#[test]
fn copy_command_defaults_to_dry_run_and_creates_no_journal_or_files() {
    let (_workspace, source, destination) = fixture_pair();
    fs::write(source.join("new.txt"), b"new data").unwrap();

    let output = run_cli(&[
        "copy",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["report"]["dryRun"], true);
    assert_eq!(report["report"]["items"][0]["state"], "planned");
    assert!(!destination.join("new.txt").exists());
    assert!(!destination.join(".syncwise").exists());
}

#[test]
fn apply_copies_only_missing_files_and_writes_a_journal() {
    let (workspace, source, destination) = fixture_pair();
    fs::write(source.join("new.txt"), b"new data").unwrap();

    let output = run_cli(&[
        "copy",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        "--apply",
        "--journal",
        workspace
            .path()
            .join("state/journal.jsonl")
            .to_str()
            .unwrap(),
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["report"]["dryRun"], false);
    assert_eq!(report["report"]["items"][0]["state"], "done");
    assert_eq!(report["plan"]["canExecute"], true);
    assert_eq!(fs::read(destination.join("new.txt")).unwrap(), b"new data");
    assert!(workspace.path().join("state/journal.jsonl").is_file());
}

#[test]
fn apply_never_replaces_same_path_destination_bytes() {
    let (_workspace, source, destination) = fixture_pair();
    fs::write(source.join("same.txt"), b"source bytes").unwrap();
    fs::write(destination.join("same.txt"), b"destination bytes").unwrap();

    let output = run_cli(&[
        "copy",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        "--apply",
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(destination.join("same.txt")).unwrap(),
        b"destination bytes"
    );
}

#[test]
fn plan_command_returns_a_preview_without_mutating_the_destination() {
    let (_workspace, source, destination) = fixture_pair();
    fs::write(source.join("new.txt"), b"new data").unwrap();

    let output = run_cli(&[
        "plan",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["canExecute"], true);
    assert_eq!(plan["items"][0]["kind"], "copy");
    assert!(!destination.join("new.txt").exists());
}

#[test]
fn recover_reports_temps_without_deleting_them() {
    let (_workspace, _source, destination) = fixture_pair();
    let journal = destination.join(".syncwise/journal.jsonl");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(
        &journal,
        "{\"runId\":\"run-1\",\"itemId\":\"item-1\",\"sourceRelativePath\":\"file.bin\",\"destinationRelativePath\":\"file.bin\",\"state\":\"copying\",\"tempRelativePath\":\".syncwise-part-orphan.tmp\",\"bytesWritten\":3,\"message\":null}\n",
    )
    .unwrap();
    let temporary = destination.join(".syncwise-part-orphan.tmp");
    fs::write(&temporary, b"part").unwrap();

    let output = run_cli(&[
        "recover",
        destination.to_str().unwrap(),
        journal.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["interruptedItems"].as_array().unwrap().len(), 1);
    assert_eq!(report["orphanedTempPaths"].as_array().unwrap().len(), 1);
    assert!(temporary.is_file());
}

#[test]
fn resume_preserves_completed_files_and_defaults_to_dry_run() {
    let (workspace, source, destination) = fixture_pair();
    fs::write(source.join("file.txt"), b"approved").unwrap();
    let initial = run_cli(&[
        "copy",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
        "--apply",
        "--journal",
        workspace
            .path()
            .join("state/journal.jsonl")
            .to_str()
            .unwrap(),
    ]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let initial: Value = serde_json::from_slice(&initial.stdout).unwrap();
    let snapshot = initial["snapshotPath"].as_str().unwrap();
    let preview = run_cli(&["resume", snapshot]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["items"][0]["state"], "done");
    let applied = run_cli(&["resume", snapshot, "--apply"]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["runId"], initial["report"]["runId"]);
    assert_eq!(report["items"][0]["state"], "done");
    assert_eq!(fs::read(destination.join("file.txt")).unwrap(), b"approved");
}

struct PausedCopy {
    child: Child,
}

impl Drop for PausedCopy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_paused_copy(workspace: &Path) -> PausedCopy {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interruption_worker", "--nocapture"])
        .env("SYNCWISE_TEST_WORKSPACE", workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut worker = PausedCopy { child };
    let stdout = worker.child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_ok_and(|line| line.contains("SYNCWISE-PARTIAL-COPY-READY")) {
                let _ = sender.send(());
            }
        }
    });
    receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("copy worker must signal a partial write within 30 seconds");
    worker
}

fn snapshot_in(workspace: &Path) -> PathBuf {
    fs::read_dir(workspace.join("state"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".syncwise-run.json"))
        .expect("a durable run snapshot must exist before any copying")
}

fn prepare_interruption_pair(source: &Path) {
    fs::write(source.join("a-done.txt"), b"already complete").unwrap();
    fs::write(source.join("b-partial.bin"), vec![7; 3 * 1024 * 1024]).unwrap();
}

#[test]
fn interruption_worker() {
    let Some(workspace) = std::env::var_os("SYNCWISE_TEST_WORKSPACE") else {
        return;
    };
    let workspace = PathBuf::from(workspace);
    let cancellation = AtomicBool::new(false);
    let source =
        syncwise_engine::scan_directory(&workspace.join("source"), &cancellation, |_| {}).unwrap();
    let destination =
        syncwise_engine::scan_directory(&workspace.join("destination"), &cancellation, |_| {})
            .unwrap();
    let plan = syncwise_engine::generate_plan(&source, &destination).unwrap();
    let decisions = plan
        .items
        .iter()
        .map(|item| (item.id.clone(), syncwise_engine::Decision::Approve))
        .collect();
    let mut paused = false;
    let report = syncwise_engine::copy_approved_items(
        &plan,
        &decisions,
        false,
        &cancellation,
        &workspace.join("state/journal.jsonl"),
        |progress| {
            if !paused && progress.item_id == plan.items[1].id {
                paused = true;
                println!("SYNCWISE-PARTIAL-COPY-READY");
                std::io::stdout().flush().unwrap();
                std::io::stdin().read_exact(&mut [0]).unwrap();
            }
        },
    );
    fs::write(
        workspace.join("worker-result.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn process_kill_leaves_only_temp_and_cli_resume_completes_original_run() {
    let (workspace, source, destination) = fixture_pair();
    prepare_interruption_pair(&source);
    let before = fs::read(source.join("b-partial.bin")).unwrap();
    let source_hash = tree_hash(&source);
    let mut worker = start_paused_copy(workspace.path());
    worker.child.kill().unwrap();
    worker.child.wait().unwrap();
    assert!(!destination.join("b-partial.bin").exists());
    assert_eq!(
        fs::read(destination.join("a-done.txt")).unwrap(),
        b"already complete"
    );
    let recovery = run_cli(&[
        "recover",
        destination.to_str().unwrap(),
        workspace
            .path()
            .join("state/journal.jsonl")
            .to_str()
            .unwrap(),
    ]);
    assert!(recovery.status.success());
    let recovery: Value = serde_json::from_slice(&recovery.stdout).unwrap();
    assert_eq!(recovery["interruptedItems"].as_array().unwrap().len(), 1);
    assert_eq!(recovery["orphanedTempPaths"].as_array().unwrap().len(), 1);
    let snapshot = snapshot_in(workspace.path());
    assert_eq!(recovery["savedRunPaths"].as_array().unwrap().len(), 1);
    let retry = run_cli(&["resume", snapshot.to_str().unwrap(), "--apply"]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert!(
        retry["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["state"] == "done")
    );
    assert_eq!(fs::read(destination.join("b-partial.bin")).unwrap(), before);
    assert_eq!(fs::read(source.join("b-partial.bin")).unwrap(), before);
    assert_eq!(tree_hash(&source), source_hash);
}

#[test]
fn late_finalization_conflict_preserves_done_items_and_retries_safely() {
    let (workspace, source, destination) = fixture_pair();
    prepare_interruption_pair(&source);
    let mut worker = start_paused_copy(workspace.path());
    fs::write(destination.join("b-partial.bin"), b"new destination file").unwrap();
    worker.child.stdin.take().unwrap().write_all(b"g").unwrap();
    assert!(worker.child.wait().unwrap().success());
    assert_eq!(
        fs::read(destination.join("b-partial.bin")).unwrap(),
        b"new destination file"
    );
    assert_eq!(
        fs::read(destination.join("a-done.txt")).unwrap(),
        b"already complete"
    );
    fs::remove_file(destination.join("b-partial.bin")).unwrap();
    let retry = run_cli(&[
        "resume",
        snapshot_in(workspace.path()).to_str().unwrap(),
        "--apply",
    ]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert!(
        retry["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["state"] == "done")
    );
    assert_eq!(
        fs::read(destination.join("b-partial.bin")).unwrap(),
        fs::read(source.join("b-partial.bin")).unwrap()
    );
}

#[test]
fn resumed_run_refuses_source_changed_since_original_preview() {
    let (workspace, source, destination) = fixture_pair();
    prepare_interruption_pair(&source);
    let mut worker = start_paused_copy(workspace.path());
    worker.child.kill().unwrap();
    worker.child.wait().unwrap();
    fs::write(source.join("b-partial.bin"), b"changed after preview").unwrap();
    let retry = run_cli(&[
        "resume",
        snapshot_in(workspace.path()).to_str().unwrap(),
        "--apply",
    ]);
    assert!(!retry.status.success());
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["items"][0]["state"], "done");
    assert_eq!(retry["items"][1]["state"], "needs_rescan");
    assert!(!destination.join("b-partial.bin").exists());
}

#[cfg(windows)]
#[test]
fn locked_source_failure_is_journaled_and_retry_succeeds_after_unlock() {
    use std::os::windows::fs::OpenOptionsExt;
    let (workspace, source, destination) = fixture_pair();
    prepare_interruption_pair(&source);
    let mut worker = start_paused_copy(workspace.path());
    worker.child.kill().unwrap();
    worker.child.wait().unwrap();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(source.join("b-partial.bin"))
        .unwrap();
    let snapshot = snapshot_in(workspace.path());
    // The test intentionally holds an exclusive source handle; hash after releasing it.
    let failure = Command::new(env!("CARGO_BIN_EXE_syncwise-cli"))
        .args(["resume", snapshot.to_str().unwrap(), "--apply"])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    let report: Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(report["items"][0]["state"], "done");
    assert_eq!(report["items"][1]["state"], "failed");
    assert!(!destination.join("b-partial.bin").exists());
    drop(locked);
    let retry = run_cli(&["resume", snapshot.to_str().unwrap(), "--apply"]);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["items"][1]["state"], "done");
}

#[test]
fn cli_refuses_overlapping_roots_without_writing_any_state() {
    let (_workspace, source, _destination) = fixture_pair();
    fs::write(source.join("file.txt"), b"untouched").unwrap();
    let output = run_cli(&[
        "copy",
        source.to_str().unwrap(),
        source.to_str().unwrap(),
        "--apply",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must not be the same"));
    assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
}
