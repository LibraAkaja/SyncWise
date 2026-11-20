use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ts_rs::TS;

use crate::{
    CopyReport, CopyState, DecisionChange, JournalEntry, Plan, PlanItem, ScanResult, SyncError,
    SyncErrorCode,
};

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE scans (
        id INTEGER PRIMARY KEY,
        source_root TEXT NOT NULL,
        destination_root TEXT NOT NULL,
        created_ms INTEGER NOT NULL,
        source_files INTEGER NOT NULL,
        destination_files INTEGER NOT NULL
    );
    CREATE TABLE file_index (
        scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
        side TEXT NOT NULL CHECK (side IN ('source', 'destination')),
        relative_path TEXT NOT NULL,
        size_bytes INTEGER NOT NULL,
        modified_ms INTEGER,
        normalized_name TEXT NOT NULL,
        PRIMARY KEY (scan_id, side, relative_path)
    ) WITHOUT ROWID;
    CREATE TABLE candidates (
        scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
        source_relative_path TEXT NOT NULL,
        destination_relative_path TEXT NOT NULL,
        score REAL NOT NULL,
        label TEXT NOT NULL,
        reasons TEXT NOT NULL,
        PRIMARY KEY (scan_id, source_relative_path, destination_relative_path)
    ) WITHOUT ROWID;
    CREATE TABLE decisions (
        scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE,
        item_id TEXT NOT NULL,
        decision TEXT NOT NULL CHECK (decision IN ('approve', 'reject', 'defer')),
        updated_ms INTEGER NOT NULL,
        PRIMARY KEY (scan_id, item_id)
    ) WITHOUT ROWID;
    CREATE TABLE runs (
        run_id TEXT PRIMARY KEY,
        scan_id INTEGER REFERENCES scans(id),
        plan_id TEXT NOT NULL,
        source_root TEXT NOT NULL,
        destination_root TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'needs_attention', 'cancelled', 'abandoned')),
        started_ms INTEGER NOT NULL,
        finished_ms INTEGER,
        journal_path TEXT NOT NULL,
        snapshot_path TEXT NOT NULL,
        error TEXT
    );
    CREATE INDEX runs_status ON runs(status, started_ms);
    CREATE TABLE run_items (
        run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
        item_id TEXT NOT NULL,
        source_relative_path TEXT NOT NULL,
        destination_relative_path TEXT NOT NULL,
        size_bytes INTEGER NOT NULL,
        state TEXT NOT NULL,
        bytes_written INTEGER NOT NULL DEFAULT 0,
        message TEXT,
        PRIMARY KEY (run_id, item_id)
    ) WITHOUT ROWID;
    CREATE TABLE journal (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
        item_id TEXT NOT NULL,
        state TEXT NOT NULL,
        bytes_written INTEGER NOT NULL,
        temp_relative_path TEXT,
        message TEXT
    );
    CREATE INDEX journal_run ON journal(run_id);",
    "ALTER TABLE file_index ADD COLUMN audio_metadata_json TEXT;",
    "CREATE TABLE profiles (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL COLLATE NOCASE UNIQUE,
        source_root TEXT NOT NULL,
        destination_root TEXT NOT NULL,
        updated_ms INTEGER NOT NULL,
        CHECK (length(trim(name)) BETWEEN 1 AND 80),
        CHECK (length(source_root) > 0),
        CHECK (length(destination_root) > 0)
    );",
];

const KEPT_UNREFERENCED_SCANS: i64 = 5;
const ATTENTION_ITEM_LIMIT: i64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    NeedsAttention,
    Cancelled,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RunItemSummary {
    pub item_id: String,
    pub source_relative_path: String,
    pub destination_relative_path: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    pub state: CopyState,
    #[ts(type = "number")]
    pub bytes_written: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: String,
    pub plan_id: String,
    pub source_root: String,
    pub destination_root: String,
    pub status: RunStatus,
    #[ts(type = "number")]
    pub started_ms: u64,
    #[ts(type = "number | null")]
    pub finished_ms: Option<u64>,
    pub journal_path: String,
    pub snapshot_path: String,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub total_items: u64,
    #[ts(type = "number")]
    pub done_items: u64,
    #[ts(type = "number")]
    pub bytes_written: u64,
    /// Items that are not Done, capped so very large runs stay cheap to load.
    pub attention_items: Vec<RunItemSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RunHistoryEntry {
    pub run_id: String,
    pub source_root: String,
    pub destination_root: String,
    pub status: RunStatus,
    #[ts(type = "number")]
    pub started_ms: u64,
    #[ts(type = "number | null")]
    pub finished_ms: Option<u64>,
    #[ts(type = "number")]
    pub total_items: u64,
    #[ts(type = "number")]
    pub done_items: u64,
    #[ts(type = "number")]
    pub bytes_written: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedProfile {
    pub id: i64,
    pub name: String,
    pub source_root: String,
    pub destination_root: String,
    pub updated_ms: i64,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, SyncError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                SyncError::new(
                    SyncErrorCode::StoreUnavailable,
                    &format!("Could not create the local database folder: {error}"),
                )
            })?;
        }
        let connection = Connection::open(path).map_err(db_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;",
            )
            .map_err(db_error)?;
        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), SyncError> {
        let version: usize = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        if version > MIGRATIONS.len() {
            return Err(SyncError::new(
                SyncErrorCode::StoreUnavailable,
                "The local database was created by a newer SyncWise version.",
            ));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            let transaction = self.connection.transaction().map_err(db_error)?;
            transaction.execute_batch(migration).map_err(db_error)?;
            transaction
                .pragma_update(None, "user_version", index + 1)
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<usize, SyncError> {
        self.connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)
    }

    pub fn save_profile(
        &mut self,
        name: &str,
        source_root: &str,
        destination_root: &str,
    ) -> Result<SavedProfile, SyncError> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
            || source_root.is_empty()
            || destination_root.is_empty()
        {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "A profile needs a name of 1–80 characters and two folder paths.",
            ));
        }

        self.connection
            .execute(
                "INSERT INTO profiles (name, source_root, destination_root, updated_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(name) DO UPDATE SET
                    name = excluded.name,
                    source_root = excluded.source_root,
                    destination_root = excluded.destination_root,
                    updated_ms = excluded.updated_ms",
                params![name, source_root, destination_root, now_ms()],
            )
            .map_err(db_error)?;
        self.connection
            .query_row(
                "SELECT id, name, source_root, destination_root, updated_ms
                 FROM profiles WHERE name = ?1",
                [name],
                profile_from_row,
            )
            .map_err(db_error)
    }

    pub fn list_profiles(&self) -> Result<Vec<SavedProfile>, SyncError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, name, source_root, destination_root, updated_ms
                 FROM profiles ORDER BY name COLLATE NOCASE, id",
            )
            .map_err(db_error)?;
        let profiles = statement
            .query_map([], profile_from_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(profiles)
    }

    pub fn delete_profile(&mut self, profile_id: i64) -> Result<(), SyncError> {
        let deleted = self
            .connection
            .execute("DELETE FROM profiles WHERE id = ?1", [profile_id])
            .map_err(db_error)?;
        if deleted == 0 {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "The saved profile no longer exists.",
            ));
        }
        Ok(())
    }

    pub fn record_scan(
        &mut self,
        plan: &Plan,
        source: &ScanResult,
        destination: &ScanResult,
    ) -> Result<i64, SyncError> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO scans (source_root, destination_root, created_ms, source_files, destination_files)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    plan.source_root,
                    plan.destination_root,
                    now_ms(),
                    source.files.len() as i64,
                    destination.files.len() as i64
                ],
            )
            .map_err(db_error)?;
        let scan_id = transaction.last_insert_rowid();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO file_index (scan_id, side, relative_path, size_bytes, modified_ms, normalized_name, audio_metadata_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT DO NOTHING",
                )
                .map_err(db_error)?;
            for (side, scan) in [("source", source), ("destination", destination)] {
                for file in &scan.files {
                    insert
                        .execute(params![
                            scan_id,
                            side,
                            file.relative_path,
                            i64::try_from(file.size_bytes).unwrap_or(i64::MAX),
                            file.modified_time_unix_ms,
                            file.normalized_name,
                            serde_json::to_string(&file.audio_metadata).map_err(json_error)?
                        ])
                        .map_err(db_error)?;
                }
            }
            let mut candidate = transaction
                .prepare(
                    "INSERT INTO candidates (scan_id, source_relative_path, destination_relative_path, score, label, reasons)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT DO NOTHING",
                )
                .map_err(db_error)?;
            for entry in plan.matching.iter().flat_map(|report| &report.matches) {
                for found in &entry.candidates {
                    candidate
                        .execute(params![
                            scan_id,
                            entry.source_relative_path,
                            found.destination_relative_path,
                            found.score,
                            enum_text(&found.label)?,
                            serde_json::to_string(&found.reasons).map_err(json_error)?
                        ])
                        .map_err(db_error)?;
                }
            }
        }
        // Old scans that no run references are bulky and never reopened.
        transaction
            .execute(
                "DELETE FROM scans WHERE id <= ?1 AND id NOT IN (SELECT scan_id FROM runs WHERE scan_id IS NOT NULL)",
                [scan_id - KEPT_UNREFERENCED_SCANS],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(scan_id)
    }

    pub fn save_decisions(
        &mut self,
        scan_id: i64,
        changes: &[DecisionChange],
    ) -> Result<(), SyncError> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        for change in changes {
            match &change.decision {
                Some(decision) => transaction
                    .execute(
                        "INSERT INTO decisions (scan_id, item_id, decision, updated_ms) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT (scan_id, item_id) DO UPDATE SET decision = excluded.decision, updated_ms = excluded.updated_ms",
                        params![scan_id, change.item_id, enum_text(decision)?, now_ms()],
                    )
                    .map_err(db_error)?,
                None => transaction
                    .execute(
                        "DELETE FROM decisions WHERE scan_id = ?1 AND item_id = ?2",
                        params![scan_id, change.item_id],
                    )
                    .map_err(db_error)?,
            };
        }
        transaction.commit().map_err(db_error)
    }

    pub fn decision_count(&self, scan_id: i64) -> Result<u64, SyncError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM decisions WHERE scan_id = ?1",
                [scan_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as u64)
            .map_err(db_error)
    }

    /// Record the run before any file is touched so a crash leaves a discoverable 'running' row.
    pub fn begin_run(
        &mut self,
        run_id: &str,
        scan_id: Option<i64>,
        plan: &Plan,
        items: &[&PlanItem],
        journal_path: &Path,
        snapshot_path: &Path,
    ) -> Result<(), SyncError> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO runs (run_id, scan_id, plan_id, source_root, destination_root, status, started_ms, journal_path, snapshot_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8)",
                params![
                    run_id,
                    scan_id,
                    plan.id,
                    plan.source_root,
                    plan.destination_root,
                    now_ms(),
                    path_text(journal_path)?,
                    path_text(snapshot_path)?
                ],
            )
            .map_err(db_error)?;
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO run_items (run_id, item_id, source_relative_path, destination_relative_path, size_bytes, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'planned')",
                )
                .map_err(db_error)?;
            for item in items {
                insert
                    .execute(params![
                        run_id,
                        item.id,
                        item.source_relative_path,
                        item.destination_relative_path,
                        i64::try_from(item.size_bytes).unwrap_or(i64::MAX)
                    ])
                    .map_err(db_error)?;
            }
        }
        transaction.commit().map_err(db_error)
    }

    /// Update items from a finished or retried run and derive the final status from item states.
    pub fn finish_run(
        &mut self,
        run_id: &str,
        report: &CopyReport,
        journal: &[JournalEntry],
    ) -> Result<RunSummary, SyncError> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        for item in &report.items {
            transaction
                .execute(
                    "UPDATE run_items SET state = ?3, bytes_written = ?4, message = ?5 WHERE run_id = ?1 AND item_id = ?2",
                    params![
                        run_id,
                        item.item_id,
                        enum_text(&item.state)?,
                        i64::try_from(item.bytes_written).unwrap_or(i64::MAX),
                        item.message
                    ],
                )
                .map_err(db_error)?;
        }
        transaction
            .execute("DELETE FROM journal WHERE run_id = ?1", [run_id])
            .map_err(db_error)?;
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO journal (run_id, item_id, state, bytes_written, temp_relative_path, message)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(db_error)?;
            for entry in journal {
                insert
                    .execute(params![
                        run_id,
                        entry.item_id,
                        enum_text(&entry.state)?,
                        i64::try_from(entry.bytes_written).unwrap_or(i64::MAX),
                        entry.temp_relative_path,
                        entry.message
                    ])
                    .map_err(db_error)?;
            }
        }
        let unfinished: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM run_items WHERE run_id = ?1 AND state <> 'done'",
                [run_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let status = if unfinished == 0 {
            RunStatus::Completed
        } else if report.cancelled {
            RunStatus::Cancelled
        } else {
            RunStatus::NeedsAttention
        };
        transaction
            .execute(
                "UPDATE runs SET status = ?2, finished_ms = ?3, error = NULL WHERE run_id = ?1",
                params![run_id, enum_text(&status)?, now_ms()],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        self.get_run(run_id)?.ok_or_else(missing_run)
    }

    /// Reconcile a run left in `running` after an app restart from its durable file journal.
    pub fn recover_run(
        &mut self,
        run_id: &str,
        journal: &[JournalEntry],
    ) -> Result<RunSummary, SyncError> {
        if journal.iter().any(|entry| entry.run_id != run_id) {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "The recovery journal contains entries for a different run.",
            ));
        }

        let transaction = self.connection.transaction().map_err(db_error)?;
        let mut latest = std::collections::HashMap::new();
        for entry in journal {
            latest.insert(entry.item_id.as_str(), entry);
        }
        for entry in latest.into_values() {
            let interrupted = matches!(
                entry.state,
                CopyState::Planned
                    | CopyState::Revalidated
                    | CopyState::Copying
                    | CopyState::Flushed
                    | CopyState::Finalizing
                    | CopyState::Verified
                    | CopyState::Finalized
            );
            let message = entry.message.clone().or_else(|| {
                interrupted.then(|| {
                    "The app stopped before this file was fully recorded; retry is safe.".into()
                })
            });
            let updated = transaction
                .execute(
                    "UPDATE run_items SET state = ?3, bytes_written = ?4, message = ?5
                     WHERE run_id = ?1 AND item_id = ?2",
                    params![
                        run_id,
                        entry.item_id,
                        enum_text(&entry.state)?,
                        i64::try_from(entry.bytes_written).unwrap_or(i64::MAX),
                        message
                    ],
                )
                .map_err(db_error)?;
            if updated != 1 {
                return Err(SyncError::new(
                    SyncErrorCode::StoreUnavailable,
                    "The recovery journal references an unknown run item.",
                ));
            }
        }

        transaction
            .execute("DELETE FROM journal WHERE run_id = ?1", [run_id])
            .map_err(db_error)?;
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO journal (run_id, item_id, state, bytes_written, temp_relative_path, message)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(db_error)?;
            for entry in journal {
                insert
                    .execute(params![
                        run_id,
                        entry.item_id,
                        enum_text(&entry.state)?,
                        i64::try_from(entry.bytes_written).unwrap_or(i64::MAX),
                        entry.temp_relative_path,
                        entry.message
                    ])
                    .map_err(db_error)?;
            }
        }
        let unfinished: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM run_items WHERE run_id = ?1 AND state <> 'done'",
                [run_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if unfinished == 0 {
            transaction
                .execute(
                    "UPDATE runs SET status = 'completed', finished_ms = ?2, error = NULL
                     WHERE run_id = ?1 AND status IN ('running', 'needs_attention', 'cancelled')",
                    params![run_id, now_ms()],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        self.get_run(run_id)?.ok_or_else(missing_run)
    }

    /// Mark a run that ended with an error before producing a report (for example, free space was lost).
    pub fn fail_run(&mut self, run_id: &str, message: &str) -> Result<RunSummary, SyncError> {
        self.connection
            .execute(
                "UPDATE runs SET status = 'needs_attention', finished_ms = ?2, error = ?3 WHERE run_id = ?1",
                params![run_id, now_ms(), message],
            )
            .map_err(db_error)?;
        self.get_run(run_id)?.ok_or_else(missing_run)
    }

    /// Re-open a run for a retry; the caller is responsible for excluding it from startup recovery.
    pub fn restart_run(&mut self, run_id: &str) -> Result<(), SyncError> {
        let changed = self
            .connection
            .execute(
                "UPDATE runs SET status = 'running', finished_ms = NULL, error = NULL
                 WHERE run_id = ?1 AND status IN ('running', 'needs_attention', 'cancelled')",
                [run_id],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "This run cannot be retried.",
            ));
        }
        Ok(())
    }

    pub fn abandon_run(&mut self, run_id: &str) -> Result<(), SyncError> {
        let changed = self
            .connection
            .execute(
                "UPDATE runs SET status = 'abandoned', finished_ms = COALESCE(finished_ms, ?2)
                 WHERE run_id = ?1 AND status IN ('running', 'needs_attention', 'cancelled')",
                params![run_id, now_ms()],
            )
            .map_err(db_error)?;
        if changed == 0 {
            return Err(SyncError::new(
                SyncErrorCode::InvalidInput,
                "This run cannot be dismissed.",
            ));
        }
        Ok(())
    }

    /// Items and bytes a run still has to copy.
    pub fn item_paths(
        &self,
        run_id: &str,
    ) -> Result<std::collections::HashMap<String, String>, SyncError> {
        let mut statement = self
            .connection
            .prepare("SELECT item_id, source_relative_path FROM run_items WHERE run_id = ?1")
            .map_err(db_error)?;
        let rows = statement
            .query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_error)?;
        rows.collect::<Result<_, _>>().map_err(db_error)
    }

    /// Items and bytes a run still has to copy.
    pub fn pending_totals(&self, run_id: &str) -> Result<(u64, u64), SyncError> {
        self.connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM run_items WHERE run_id = ?1 AND state <> 'done'",
                [run_id],
                |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, i64>(1)?.max(0) as u64)),
            )
            .map_err(db_error)
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<RunSummary>, SyncError> {
        let head = self
            .connection
            .query_row(
                "SELECT plan_id, source_root, destination_root, status, started_ms, finished_ms, journal_path, snapshot_path, error
                 FROM runs WHERE run_id = ?1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(db_error)?;
        let Some((
            plan_id,
            source_root,
            destination_root,
            status,
            started,
            finished,
            journal,
            snapshot,
            error,
        )) = head
        else {
            return Ok(None);
        };
        let (total, done, bytes): (i64, i64, i64) = self
            .connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(state = 'done'), 0), COALESCE(SUM(bytes_written), 0)
                 FROM run_items WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(db_error)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT item_id, source_relative_path, destination_relative_path, size_bytes, state, bytes_written, message
                 FROM run_items WHERE run_id = ?1 AND state <> 'done' ORDER BY item_id LIMIT ?2",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![run_id, ATTENTION_ITEM_LIMIT], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(db_error)?;
        let mut attention_items = Vec::new();
        for row in rows {
            let (
                item_id,
                source_relative_path,
                destination_relative_path,
                size,
                state,
                written,
                message,
            ) = row.map_err(db_error)?;
            attention_items.push(RunItemSummary {
                item_id,
                source_relative_path,
                destination_relative_path,
                size_bytes: size.max(0) as u64,
                state: parse_enum(&state)?,
                bytes_written: written.max(0) as u64,
                message,
            });
        }
        Ok(Some(RunSummary {
            run_id: run_id.into(),
            plan_id,
            source_root,
            destination_root,
            status: parse_enum(&status)?,
            started_ms: started.max(0) as u64,
            finished_ms: finished.map(|value| value.max(0) as u64),
            journal_path: journal,
            snapshot_path: snapshot,
            error,
            total_items: total.max(0) as u64,
            done_items: done.max(0) as u64,
            bytes_written: bytes.max(0) as u64,
            attention_items,
        }))
    }

    /// Newest runs first, optionally leaving out a run that is executing in this process.
    pub fn list_runs(
        &self,
        statuses: &[RunStatus],
        exclude: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RunSummary>, SyncError> {
        let wanted: HashSet<String> = statuses.iter().map(enum_text).collect::<Result<_, _>>()?;
        let mut statement = self
            .connection
            .prepare("SELECT run_id, status FROM runs ORDER BY started_ms DESC, run_id DESC")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_error)?;
        let mut runs = Vec::new();
        for row in rows {
            let (run_id, status) = row.map_err(db_error)?;
            if runs.len() >= limit || !wanted.contains(&status) || exclude == Some(run_id.as_str())
            {
                continue;
            }
            if let Some(run) = self.get_run(&run_id)? {
                runs.push(run);
            }
        }
        Ok(runs)
    }

    pub fn list_run_history(&self, limit: usize) -> Result<Vec<RunHistoryEntry>, SyncError> {
        let limit = i64::try_from(limit.min(200)).unwrap_or(200);
        let mut statement = self
            .connection
            .prepare(
                "SELECT runs.run_id, runs.source_root, runs.destination_root, runs.status,
                        runs.started_ms, runs.finished_ms, COUNT(run_items.item_id),
                        COALESCE(SUM(run_items.state = 'done'), 0),
                        COALESCE(SUM(run_items.bytes_written), 0), runs.error
                 FROM runs LEFT JOIN run_items ON run_items.run_id = runs.run_id
                 GROUP BY runs.run_id
                 ORDER BY runs.started_ms DESC, runs.run_id DESC
                 LIMIT ?1",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (
                run_id,
                source_root,
                destination_root,
                status,
                started_ms,
                finished_ms,
                total_items,
                done_items,
                bytes_written,
                error,
            ) = row.map_err(db_error)?;
            Ok(RunHistoryEntry {
                run_id,
                source_root,
                destination_root,
                status: parse_enum(&status)?,
                started_ms: started_ms.max(0) as u64,
                finished_ms: finished_ms.map(|value| value.max(0) as u64),
                total_items: total_items.max(0) as u64,
                done_items: done_items.max(0) as u64,
                bytes_written: bytes_written.max(0) as u64,
                error,
            })
        })
        .collect()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn path_text(path: &Path) -> Result<&str, SyncError> {
    path.to_str().ok_or_else(|| {
        SyncError::new(
            SyncErrorCode::InvalidInput,
            "A local state path is not valid Unicode.",
        )
    })
}

fn enum_text<T: Serialize>(value: &T) -> Result<String, SyncError> {
    match serde_json::to_value(value).map_err(json_error)? {
        serde_json::Value::String(text) => Ok(text),
        _ => Err(SyncError::new(
            SyncErrorCode::StoreUnavailable,
            "Unexpected enum representation.",
        )),
    }
}

fn parse_enum<T: DeserializeOwned>(text: &str) -> Result<T, SyncError> {
    serde_json::from_value(serde_json::Value::String(text.into())).map_err(|_| {
        SyncError::new(
            SyncErrorCode::StoreUnavailable,
            "The local database contains an unrecognized value.",
        )
    })
}

fn profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedProfile> {
    Ok(SavedProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        source_root: row.get(2)?,
        destination_root: row.get(3)?,
        updated_ms: row.get(4)?,
    })
}

fn db_error(error: rusqlite::Error) -> SyncError {
    SyncError::new(
        SyncErrorCode::StoreUnavailable,
        &format!("Local database error: {error}"),
    )
}

fn json_error(error: serde_json::Error) -> SyncError {
    SyncError::new(
        SyncErrorCode::StoreUnavailable,
        &format!("Could not encode local database data: {error}"),
    )
}

fn missing_run() -> SyncError {
    SyncError::new(SyncErrorCode::InvalidInput, "The run no longer exists.")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::{Decision, generate_plan, scan_directory};

    struct Fixture {
        _root: tempfile::TempDir,
        db: std::path::PathBuf,
        plan: Plan,
        source: ScanResult,
        destination: ScanResult,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("a.txt"), b"a").unwrap();
        fs::write(source.join("Lina - Winter.mp3"), b"song").unwrap();
        fs::write(destination.join("Winter - Lina.mp3"), b"song").unwrap();
        let source = scan_directory(&source, &AtomicBool::new(false), |_| {}).unwrap();
        let destination = scan_directory(&destination, &AtomicBool::new(false), |_| {}).unwrap();
        let plan = generate_plan(&source, &destination).unwrap();
        Fixture {
            db: root.path().join("state/syncwise.sqlite"),
            _root: root,
            plan,
            source,
            destination,
        }
    }

    #[test]
    fn migrations_are_idempotent_and_newer_databases_are_refused() {
        let fixture = fixture();
        assert_eq!(
            Store::open(&fixture.db).unwrap().schema_version().unwrap(),
            MIGRATIONS.len()
        );
        assert_eq!(
            Store::open(&fixture.db).unwrap().schema_version().unwrap(),
            MIGRATIONS.len()
        );
        Connection::open(&fixture.db)
            .unwrap()
            .pragma_update(None, "user_version", MIGRATIONS.len() + 1)
            .unwrap();
        assert!(Store::open(&fixture.db).is_err());
    }

    #[test]
    fn saved_profiles_validate_upsert_case_insensitively_and_delete() {
        let fixture = fixture();
        let mut store = Store::open(&fixture.db).unwrap();
        let first = store
            .save_profile("  Music  ", "C:\\Music", "D:\\Backup")
            .unwrap();
        let updated = store
            .save_profile("music", "E:\\Music", "F:\\Backup")
            .unwrap();
        assert_eq!(first.id, updated.id);
        assert_eq!(updated.name, "music");
        assert_eq!(updated.source_root, "E:\\Music");
        assert_eq!(store.list_profiles().unwrap(), vec![updated.clone()]);

        assert!(store.save_profile(" ", "C:\\Music", "D:\\Backup").is_err());
        assert!(
            store
                .save_profile(&"x".repeat(81), "C:\\Music", "D:\\Backup")
                .is_err()
        );
        assert!(
            store
                .save_profile("Missing destination", "C:\\Music", "")
                .is_err()
        );

        store.delete_profile(updated.id).unwrap();
        assert!(store.list_profiles().unwrap().is_empty());
        assert!(store.delete_profile(updated.id).is_err());
    }

    #[test]
    fn run_history_is_bounded_and_includes_summary_counts() {
        let fixture = fixture();
        let mut store = Store::open(&fixture.db).unwrap();
        let items: Vec<_> = fixture.plan.items.iter().collect();
        store
            .begin_run(
                "history-run",
                None,
                &fixture.plan,
                &items,
                Path::new("journal"),
                Path::new("snapshot"),
            )
            .unwrap();

        let history = store.list_run_history(usize::MAX).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].run_id, "history-run");
        assert_eq!(history[0].status, RunStatus::Running);
        assert_eq!(history[0].total_items, items.len() as u64);
        assert_eq!(history[0].done_items, 0);
        assert!(store.list_run_history(0).unwrap().is_empty());
    }

    #[test]
    fn records_scan_decisions_and_prunes_only_unreferenced_old_scans() {
        let fixture = fixture();
        let mut store = Store::open(&fixture.db).unwrap();
        let mut source = fixture.source.clone();
        source
            .files
            .iter_mut()
            .find(|file| file.relative_path.ends_with(".mp3"))
            .unwrap()
            .audio_metadata = Some(crate::AudioMetadata {
            title: Some("Winter".into()),
            artist: Some("Lina".into()),
            album_artist: None,
            album: Some("Test Album".into()),
            duration_ms: Some(123_000),
        });
        let first = store
            .record_scan(&fixture.plan, &source, &fixture.destination)
            .unwrap();
        let item = fixture.plan.items[0].id.clone();
        store
            .save_decisions(
                first,
                &[DecisionChange {
                    item_id: item.clone(),
                    decision: Some(Decision::Approve),
                }],
            )
            .unwrap();
        store
            .save_decisions(
                first,
                &[DecisionChange {
                    item_id: "second".into(),
                    decision: Some(Decision::Reject),
                }],
            )
            .unwrap();
        assert_eq!(store.decision_count(first).unwrap(), 2);
        store
            .save_decisions(
                first,
                &[DecisionChange {
                    item_id: "second".into(),
                    decision: None,
                }],
            )
            .unwrap();
        assert_eq!(store.decision_count(first).unwrap(), 1);
        let approved: Vec<_> = fixture.plan.items.iter().collect();
        store
            .begin_run(
                "run-keep",
                Some(first),
                &fixture.plan,
                &approved,
                Path::new("j"),
                Path::new("s"),
            )
            .unwrap();
        for _ in 0..8 {
            store
                .record_scan(&fixture.plan, &fixture.source, &fixture.destination)
                .unwrap();
        }
        assert_eq!(
            store.decision_count(first).unwrap(),
            1,
            "referenced scan was pruned"
        );
        let scans: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM scans", [], |row| row.get(0))
            .unwrap();
        assert_eq!(scans, 1 + KEPT_UNREFERENCED_SCANS);
        let candidates: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM candidates WHERE scan_id = ?1",
                [first],
                |row| row.get(0),
            )
            .unwrap();
        assert!(candidates > 0);
        let audio_metadata_column: String = store
            .connection
            .query_row(
                "SELECT audio_metadata_json FROM file_index WHERE scan_id = ?1 AND side = 'source' AND relative_path LIKE '%.mp3'",
                [first],
                |row| row.get(0),
            )
            .unwrap();
        let audio_metadata: crate::AudioMetadata =
            serde_json::from_str(&audio_metadata_column).unwrap();
        assert_eq!(audio_metadata.title.as_deref(), Some("Winter"));
    }

    #[test]
    fn run_lifecycle_survives_reopening_and_derives_status() {
        let fixture = fixture();
        let items: Vec<_> = fixture.plan.items.iter().collect();
        let mut store = Store::open(&fixture.db).unwrap();
        store
            .begin_run(
                "run-1",
                None,
                &fixture.plan,
                &items,
                Path::new("journal"),
                Path::new("snap"),
            )
            .unwrap();
        drop(store);

        let mut store = Store::open(&fixture.db).unwrap();
        let interrupted = store.list_runs(&[RunStatus::Running], None, 20).unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].total_items, items.len() as u64);
        assert!(
            store
                .list_runs(&[RunStatus::Running], Some("run-1"), 20)
                .unwrap()
                .is_empty()
        );

        let report = CopyReport {
            run_id: "run-1".into(),
            dry_run: false,
            cancelled: false,
            items: vec![
                crate::CopyItemResult {
                    item_id: items[0].id.clone(),
                    state: CopyState::Done,
                    bytes_written: 7,
                    message: None,
                },
                crate::CopyItemResult {
                    item_id: items[1].id.clone(),
                    state: CopyState::Failed,
                    bytes_written: 0,
                    message: Some("disk removed".into()),
                },
            ],
        };
        let summary = store.finish_run("run-1", &report, &[]).unwrap();
        assert_eq!(summary.status, RunStatus::NeedsAttention);
        assert_eq!((summary.done_items, summary.bytes_written), (1, 7));
        assert_eq!(summary.attention_items.len(), 1);
        assert_eq!(
            summary.attention_items[0].message.as_deref(),
            Some("disk removed")
        );

        store.restart_run("run-1").unwrap();
        let mut retried = report.clone();
        retried.items[1].state = CopyState::Done;
        retried.items[1].message = None;
        assert_eq!(
            store.finish_run("run-1", &retried, &[]).unwrap().status,
            RunStatus::Completed
        );
        assert!(store.restart_run("run-1").is_err());
        assert!(store.abandon_run("run-1").is_err());
    }

    #[test]
    fn failed_cancelled_and_abandoned_runs_are_recorded() {
        let fixture = fixture();
        let items: Vec<_> = fixture.plan.items.iter().collect();
        let mut store = Store::open(&fixture.db).unwrap();
        store
            .begin_run(
                "run-a",
                None,
                &fixture.plan,
                &items,
                Path::new("j"),
                Path::new("s"),
            )
            .unwrap();
        let failed = store.fail_run("run-a", "Not enough space.").unwrap();
        assert_eq!(failed.status, RunStatus::NeedsAttention);
        assert_eq!(failed.error.as_deref(), Some("Not enough space."));
        store.abandon_run("run-a").unwrap();
        assert_eq!(
            store.get_run("run-a").unwrap().unwrap().status,
            RunStatus::Abandoned
        );
        assert!(store.get_run("missing").unwrap().is_none());

        store
            .begin_run(
                "run-b",
                None,
                &fixture.plan,
                &items,
                Path::new("j"),
                Path::new("s"),
            )
            .unwrap();
        let cancelled = CopyReport {
            run_id: "run-b".into(),
            dry_run: false,
            cancelled: true,
            items: Vec::new(),
        };
        assert_eq!(
            store.finish_run("run-b", &cancelled, &[]).unwrap().status,
            RunStatus::Cancelled
        );
    }
}
