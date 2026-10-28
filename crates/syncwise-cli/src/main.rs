use std::{
    collections::HashMap,
    env,
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use syncwise_engine::{
    CopyProgress, CopyReport, CopyState, Decision, MatchingOptions, Plan, PlanItemKind,
    copy_approved_items, generate_plan, inspect_recovery, match_files_with_cancellation,
    resume_run, run_snapshot_path, scan_directory, validate_folder_pair,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return Ok(());
    };
    if command == "--help" || command == "-h" {
        print_usage();
        return Ok(());
    }

    let cancellation = Arc::new(AtomicBool::new(false));
    let signal = cancellation.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;

    if command == "resume" {
        let snapshot = args
            .next()
            .ok_or("resume requires a saved run snapshot path")?;
        let mut apply = false;
        for argument in args {
            if argument != "--apply" || apply {
                return Err(format!("unknown or repeated resume option: {argument}").into());
            }
            apply = true;
        }
        let report = resume_run(Path::new(&snapshot), !apply, &cancellation, print_progress)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if !apply {
            eprintln!("Dry run only. Resume with --apply to retry unfinished approved items.");
        }
        check_outcome(&report)?;
        return Ok(());
    }

    let Some(source) = args.next() else {
        print_usage();
        return Err("a source folder is required".into());
    };
    let Some(destination) = args.next() else {
        print_usage();
        return Err("a destination folder is required".into());
    };

    let mut apply = false;
    let mut journal_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--apply" => apply = true,
            "--journal" => {
                journal_path = Some(PathBuf::from(
                    args.next().ok_or("--journal requires a path")?,
                ));
            }
            _ => return Err(format!("unknown option: {argument}").into()),
        }
    }

    match command.as_str() {
        "match" => {
            if apply || journal_path.is_some() {
                return Err("match is read-only and does not accept --apply or --journal".into());
            }
            validate_folder_pair(Path::new(&source), Path::new(&destination))?;
            let source_scan = scan_directory(Path::new(&source), &cancellation, |_| {})?;
            let destination_scan = scan_directory(Path::new(&destination), &cancellation, |_| {})?;
            let matching = match_files_with_cancellation(
                &source_scan.files,
                &destination_scan.files,
                &MatchingOptions::default(),
                &cancellation,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "matching": matching,
                    "sourceScanIssues": source_scan.issues,
                    "destinationScanIssues": destination_scan.issues,
                }))?
            );
        }
        "plan" => {
            if apply || journal_path.is_some() {
                return Err("plan does not accept --apply or --journal".into());
            }
            let plan = create_plan(Path::new(&source), Path::new(&destination), &cancellation)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        "copy" => {
            let plan = create_plan(Path::new(&source), Path::new(&destination), &cancellation)?;
            if apply && !plan.can_execute {
                let blocking_issues = plan
                    .issues
                    .iter()
                    .filter(|issue| issue.blocking)
                    .map(|issue| issue.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("Plan is blocked: {blocking_issues}").into());
            }
            let decisions = approve_missing_files(&plan);
            let journal_path = match journal_path {
                Some(path) => path,
                None => default_journal_path(&plan.destination_root)?,
            };
            let report = copy_approved_items(
                &plan,
                &decisions,
                !apply,
                &cancellation,
                &journal_path,
                print_progress,
            )?;
            let snapshot_path = apply.then(|| run_snapshot_path(&journal_path, &report.run_id));
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "plan": plan,
                    "report": &report,
                    "snapshotPath": snapshot_path,
                    "journalPath": journal_path,
                }))?
            );
            if !apply {
                eprintln!("Dry run only. Re-run with --apply to copy approved missing files.");
            }
            check_outcome(&report)?;
        }
        "recover" => {
            if apply || journal_path.is_some() {
                return Err(
                    "recover accepts the destination and journal path as positional arguments"
                        .into(),
                );
            }
            let report = inspect_recovery(Path::new(&source), Path::new(&destination))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        _ => return Err(format!("unknown command: {command}").into()),
    }

    Ok(())
}

fn create_plan(
    source: &Path,
    destination: &Path,
    cancellation: &AtomicBool,
) -> Result<Plan, Box<dyn Error>> {
    let source_scan = scan_directory(source, cancellation, |_| {})?;
    let destination_scan = scan_directory(destination, cancellation, |_| {})?;
    Ok(generate_plan(&source_scan, &destination_scan)?)
}

fn default_journal_path(destination_root: &str) -> Result<PathBuf, Box<dyn Error>> {
    let directories = directories::ProjectDirs::from("com", "SyncWise", "SyncWise")
        .ok_or("Local application data directory is unavailable; specify --journal explicitly.")?;
    let key = blake3::hash(destination_root.as_bytes())
        .to_hex()
        .to_string();
    Ok(directories
        .data_local_dir()
        .join("runs")
        .join(key)
        .join("journal.jsonl"))
}

fn approve_missing_files(plan: &Plan) -> HashMap<String, Decision> {
    plan.items
        .iter()
        .filter(|item| item.kind == PlanItemKind::Copy)
        .map(|item| (item.id.clone(), Decision::Approve))
        .collect()
}

fn print_progress(progress: CopyProgress) {
    let percent = progress
        .bytes_written
        .saturating_mul(100)
        .checked_div(progress.total_bytes)
        .unwrap_or(100)
        .min(100);
    eprintln!("{}: {percent}%", progress.item_id);
}

fn check_outcome(report: &CopyReport) -> Result<(), Box<dyn Error>> {
    if !report.dry_run
        && (report.cancelled
            || report
                .items
                .iter()
                .any(|item| matches!(item.state, CopyState::Failed | CopyState::NeedsRescan)))
    {
        return Err(
            "The run did not finish successfully. Review its report before retrying.".into(),
        );
    }
    Ok(())
}

fn print_usage() {
    eprintln!("SyncWise CLI");
    eprintln!("  syncwise-cli match <source> <destination> (read-only matching report)");
    eprintln!("  syncwise-cli plan <source> <destination>");
    eprintln!("  syncwise-cli copy <source> <destination> [--apply] [--journal <path>]");
    eprintln!("  syncwise-cli recover <destination> <journal>");
    eprintln!("  syncwise-cli resume <saved-run.syncwise-run.json> [--apply]");
    eprintln!("  copy is dry-run unless --apply is explicitly provided.");
}
