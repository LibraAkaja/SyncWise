use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    Decision, FileEntry, Plan, PlanIssue, PlanIssueCode, PlanItem, PlanItemKind, SyncError,
    SyncErrorCode, validate_folder_pair,
};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DecisionChange {
    pub item_id: String,
    pub decision: Option<Decision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DecisionState {
    pub revision: u32,
    pub choices: HashMap<String, Decision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReviewPreflight {
    pub plan_id: String,
    pub revision: u32,
    pub approved_count: usize,
    pub pending_count: usize,
    pub rejected_count: usize,
    pub deferred_count: usize,
    pub conflict_count: usize,
    #[ts(type = "number")]
    pub required_bytes: u64,
    #[ts(type = "number | null")]
    pub available_bytes: Option<u64>,
    pub issues: Vec<PlanIssue>,
    pub can_prepare: bool,
}

#[derive(Clone)]
pub struct ReviewSession {
    pub plan: Arc<Plan>,
    state: DecisionState,
    item_positions: Arc<HashMap<String, usize>>,
    rejection_keys: Arc<HashMap<String, String>>,
}

impl ReviewSession {
    pub fn new(plan: Plan, destination: &[FileEntry], remembered: &HashSet<String>) -> Self {
        let destination: HashMap<_, _> = destination
            .iter()
            .map(|file| {
                (
                    file.relative_path.as_str(),
                    (file.size_bytes, file.modified_time_unix_ms),
                )
            })
            .collect();
        let candidates: HashMap<_, _> = plan
            .matching
            .as_ref()
            .map(|report| {
                report
                    .matches
                    .iter()
                    .map(|entry| (entry.source_relative_path.as_str(), &entry.candidates))
                    .collect()
            })
            .unwrap_or_default();
        let mut rejection_keys = HashMap::new();
        let mut choices = HashMap::new();
        for item in &plan.items {
            if !reviewable(item) {
                continue;
            }
            let evidence: Vec<_> = candidates
                .get(item.source_relative_path.as_str())
                .into_iter()
                .flat_map(|entries| entries.iter())
                .map(|candidate| {
                    (
                        &candidate.destination_relative_path,
                        destination.get(candidate.destination_relative_path.as_str()),
                    )
                })
                .collect();
            let key = serde_json::to_string(&(
                &plan.source_root,
                &plan.destination_root,
                &item.source_relative_path,
                item.size_bytes,
                item.source_modified_time_unix_ms,
                evidence,
            ))
            .expect("serializing strings and metadata cannot fail");
            if remembered.contains(&key) {
                choices.insert(item.id.clone(), Decision::Reject);
            }
            rejection_keys.insert(item.id.clone(), key);
        }
        let item_positions = plan
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect();
        Self {
            plan: Arc::new(plan),
            state: DecisionState {
                revision: 0,
                choices,
            },
            item_positions: Arc::new(item_positions),
            rejection_keys: Arc::new(rejection_keys),
        }
    }

    pub fn decisions(&self) -> DecisionState {
        self.state.clone()
    }

    pub fn apply_changes(
        &mut self,
        revision: u32,
        changes: &[DecisionChange],
        bulk: bool,
    ) -> Result<DecisionState, SyncError> {
        if revision != self.state.revision {
            return Err(invalid(
                "The review has changed. Refresh it before applying decisions.",
            ));
        }
        let mut seen = HashSet::new();
        for change in changes {
            let item = self
                .item_positions
                .get(&change.item_id)
                .map(|index| &self.plan.items[*index])
                .ok_or_else(|| invalid("Unknown item ID in the active plan."))?;
            if !seen.insert(&change.item_id) {
                return Err(invalid("Duplicate item ID in a decision batch."));
            }
            if !reviewable(item) {
                return Err(invalid(
                    "Conflicts and errors cannot be approved or decided as copies.",
                ));
            }
            if change.decision == Some(Decision::Approve)
                && item.kind == PlanItemKind::PossibleDuplicate
                && (bulk || changes.len() != 1)
            {
                return Err(invalid(
                    "Name-only duplicate matches require an individual review decision.",
                ));
            }
        }
        let next_revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("Review revision limit reached."))?;
        for change in changes {
            match change.decision {
                Some(decision) => {
                    self.state.choices.insert(change.item_id.clone(), decision);
                }
                None => {
                    self.state.choices.remove(&change.item_id);
                }
            }
        }
        self.state.revision = next_revision;
        Ok(self.decisions())
    }

    pub fn update_memory(&self, memory: &mut HashSet<String>) {
        for (item_id, key) in self.rejection_keys.iter() {
            if self.state.choices.get(item_id) == Some(&Decision::Reject) {
                memory.insert(key.clone());
            } else {
                memory.remove(key);
            }
        }
    }

    pub fn remember_changes(&self, changes: &[DecisionChange], memory: &mut HashSet<String>) {
        for change in changes {
            if let Some(key) = self.rejection_keys.get(&change.item_id) {
                if change.decision == Some(Decision::Reject) {
                    memory.insert(key.clone());
                } else {
                    memory.remove(key);
                }
            }
        }
    }

    pub fn preflight(&self) -> Result<ReviewPreflight, SyncError> {
        validate_folder_pair(
            Path::new(&self.plan.source_root),
            Path::new(&self.plan.destination_root),
        )?;
        let mut issues: Vec<_> = self
            .plan
            .issues
            .iter()
            .filter(|issue| {
                !matches!(
                    issue.code,
                    PlanIssueCode::InsufficientSpace
                        | PlanIssueCode::AvailableSpaceUnknown
                        | PlanIssueCode::DestinationNotWritable
                )
            })
            .cloned()
            .collect();
        let mut summary = ReviewPreflight {
            plan_id: self.plan.id.clone(),
            revision: self.state.revision,
            approved_count: 0,
            pending_count: 0,
            rejected_count: 0,
            deferred_count: 0,
            conflict_count: 0,
            required_bytes: 0,
            available_bytes: None,
            issues: Vec::new(),
            can_prepare: false,
        };
        for item in &self.plan.items {
            if matches!(item.kind, PlanItemKind::Conflict | PlanItemKind::Error) {
                summary.conflict_count += 1;
            }
            if !reviewable(item) {
                continue;
            }
            match self.state.choices.get(&item.id) {
                Some(Decision::Approve) => {
                    summary.approved_count += 1;
                    summary.required_bytes = summary
                        .required_bytes
                        .checked_add(item.size_bytes)
                        .ok_or_else(|| {
                            invalid("Approved file sizes exceed the supported range.")
                        })?;
                    let source = Path::new(&self.plan.source_root).join(&item.source_relative_path);
                    let stale = fs::symlink_metadata(&source)
                        .map(|metadata| {
                            let modified = metadata
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                                .and_then(|duration| i64::try_from(duration.as_millis()).ok());
                            !metadata.file_type().is_file()
                                || metadata.len() != item.size_bytes
                                || modified != item.source_modified_time_unix_ms
                        })
                        .unwrap_or(true);
                    if stale {
                        issues.push(blocking(format!(
                            "{} changed or is unavailable. Scan again.",
                            item.source_relative_path
                        )));
                    }
                    let target = Path::new(&self.plan.destination_root)
                        .join(&item.destination_relative_path);
                    match fs::symlink_metadata(target) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        _ => issues.push(blocking(format!(
                            "{} is occupied or inaccessible. Scan again.",
                            item.destination_relative_path
                        ))),
                    }
                }
                Some(Decision::Reject) => summary.rejected_count += 1,
                Some(Decision::Defer) => summary.deferred_count += 1,
                None => summary.pending_count += 1,
            }
        }
        summary.available_bytes = fs2::available_space(&self.plan.destination_root).ok();
        if summary
            .available_bytes
            .is_none_or(|bytes| bytes < summary.required_bytes)
        {
            issues.push(blocking(
                "Insufficient space or destination free space could not be checked.".into(),
            ));
        }
        match tempfile::NamedTempFile::new_in(&self.plan.destination_root)
            .and_then(|probe| probe.close())
        {
            Ok(()) => {}
            Err(_) => issues.push(blocking("The destination is not writable.".into())),
        }
        summary.can_prepare = summary.approved_count > 0
            && summary.pending_count == 0
            && !issues.iter().any(|issue| issue.blocking);
        summary.issues = issues;
        Ok(summary)
    }
}

fn reviewable(item: &PlanItem) -> bool {
    matches!(
        item.kind,
        PlanItemKind::Copy | PlanItemKind::PossibleDuplicate
    )
}

fn invalid(message: &str) -> SyncError {
    SyncError::new(SyncErrorCode::InvalidInput, message)
}

fn blocking(message: String) -> PlanIssue {
    PlanIssue {
        code: PlanIssueCode::InvalidRelativePath,
        message,
        blocking: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{generate_plan, scan_directory};
    use std::sync::atomic::AtomicBool;

    fn fixture() -> (tempfile::TempDir, ReviewSession) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("missing.txt"), b"new").unwrap();
        fs::write(source.join("Lina - Winter.mp3"), b"song").unwrap();
        fs::write(destination.join("Winter - Lina.mp3"), b"song").unwrap();
        let source = scan_directory(&source, &AtomicBool::new(false), |_| {}).unwrap();
        let destination = scan_directory(&destination, &AtomicBool::new(false), |_| {}).unwrap();
        let plan = generate_plan(&source, &destination).unwrap();
        (
            root,
            ReviewSession::new(plan, &destination.files, &HashSet::new()),
        )
    }

    #[test]
    fn rejects_unknown_stale_and_bulk_duplicate_approvals_atomically() {
        let (_root, mut review) = fixture();
        let duplicate = review
            .plan
            .items
            .iter()
            .find(|item| item.kind == PlanItemKind::PossibleDuplicate)
            .unwrap()
            .id
            .clone();
        let changes = [DecisionChange {
            item_id: duplicate,
            decision: Some(Decision::Approve),
        }];
        assert!(review.apply_changes(0, &changes, true).is_err());
        assert_eq!(review.decisions().revision, 0);
        assert!(
            review
                .apply_changes(
                    0,
                    &[DecisionChange {
                        item_id: "unknown".into(),
                        decision: None
                    }],
                    false
                )
                .is_err()
        );
        review.apply_changes(0, &changes, false).unwrap();
        assert!(review.apply_changes(0, &changes, false).is_err());
    }

    #[test]
    fn remembers_rejections_and_revalidates_approved_paths() {
        let (root, mut review) = fixture();
        let changes: Vec<_> = review
            .plan
            .items
            .iter()
            .map(|item| DecisionChange {
                item_id: item.id.clone(),
                decision: Some(if item.kind == PlanItemKind::Copy {
                    Decision::Approve
                } else {
                    Decision::Reject
                }),
            })
            .collect();
        review.apply_changes(0, &changes, false).unwrap();
        assert!(review.preflight().unwrap().can_prepare);
        let mut memory = HashSet::new();
        review.update_memory(&mut memory);
        let destination = scan_directory(
            &root.path().join("destination"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let next = ReviewSession::new((*review.plan).clone(), &destination.files, &memory);
        assert_eq!(
            next.decisions()
                .choices
                .values()
                .filter(|choice| **choice == Decision::Reject)
                .count(),
            1
        );
        fs::write(root.path().join("destination/missing.txt"), b"collision").unwrap();
        assert!(!review.preflight().unwrap().can_prepare);
    }
}
