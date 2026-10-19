use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::core::{CoreError, Operation, validate_events};
use crate::model::{
    Batch, BatchId, BatchRevision, BranchPair, CommentTarget, Event, PullRequestId,
    PullRequestObject, PullRequestResolution, PullRequestSource, ReadCommentRevision, ReadState,
    Review, ReviewId, User, WriteState,
};

#[path = "store_state.rs"]
mod state;

/// Provider metadata is stored separately from the core read and write models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub backend: String,
    pub locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdMapping {
    pub resource: String,
    pub core_id: String,
    pub remote_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewBackend {
    pub source: ReviewSource,
    pub mappings: Vec<IdMapping>,
}

/// A successful provider write and its returned identities. Results are kept
/// separately from both the fetched baseline and the editable event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation: Operation,
    pub mappings: Vec<IdMapping>,
    pub response: Value,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Workspace(#[from] crate::workspace::WorkspaceError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("batch does not exist: {0:?}")]
    BatchNotFound(BatchId),
}

#[derive(Debug, Clone)]
pub enum BatchRoute {
    Open(Box<crate::workspace::Workspace>),
    Choose(Vec<crate::workspace::Workspace>),
}

#[derive(Debug, Clone)]
pub struct BatchHistoryEntry {
    pub generation: u64,
    pub state: crate::workspace::Workspace,
    pub action: Value,
}

#[derive(Debug)]
pub struct WorkspaceActions {
    pub workspace: crate::workspace::Workspace,
    pub applied: usize,
    pub failure: Option<(usize, crate::workspace::WorkspaceError)>,
}

/// SQLite persistence for fetched snapshots and local event logs.
/// Event interpretation belongs to the core; backend requests belong to adapters.
pub struct Store {
    connection: Connection,
    directory: PathBuf,
}

impl Store {
    pub fn project_root(&self) -> &Path {
        self.directory
            .parent()
            .expect("review directory has a project root")
    }

    /// Opens `.review/state.sqlite3` under the supplied project root.
    pub fn open(project_root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let directory = project_root.as_ref().join(".review");
        fs::create_dir_all(&directory)?;
        let mut connection = Connection::open(directory.join("state.sqlite3"))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let compacting = transaction.table_exists(Some("main"), "read_states")?
            || (transaction.table_exists(Some("main"), "workspaces")?
                && transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE json_type(state,'$.review')='object')",
                    [], |row| row.get::<_, bool>(0),
                )?);
        let has_pull_requests = transaction.table_exists(Some("main"), "pull_requests")?;
        let versioned = transaction.column_exists(Some("main"), "write_events", "revision")?;
        let has_batches = transaction.table_exists(Some("main"), "batches")?;
        let legacy = transaction.column_exists(Some("main"), "write_events", "review_id")?;
        if legacy {
            transaction.execute_batch("ALTER TABLE write_events RENAME TO legacy_write_events")?;
        }
        if has_batches && !versioned {
            transaction.execute_batch("ALTER TABLE write_events RENAME TO unversioned_write_events; ALTER TABLE batches ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;")?;
        }
        transaction.execute_batch(include_str!("schema.sql"))?;
        if !has_pull_requests {
            Self::migrate_pull_requests(&transaction)?;
        }
        if has_batches && !versioned {
            transaction.execute_batch("INSERT INTO write_events(batch_id,revision,sequence,event) SELECT batch_id,0,sequence,event FROM unversioned_write_events; DROP TABLE unversioned_write_events;")?;
        }
        if legacy {
            transaction.execute(
                "INSERT INTO batches (batch_id, review_id) SELECT review_id, review_id FROM reviews",
                (),
            )?;
            transaction.execute(
                "INSERT INTO write_events (batch_id, sequence, event)
                 SELECT review_id, sequence, event FROM legacy_write_events",
                (),
            )?;
        }
        transaction.execute("INSERT OR IGNORE INTO batch_revisions(batch_id,revision,review_id,namespace) SELECT batch_id,0,review_id,batch_id FROM batches", [])?;
        Self::migrate_batch_workspaces(&transaction)?;
        Self::migrate_snapshot_storage(&transaction)?;
        Self::migrate_workspace_storage(&transaction)?;
        Self::adopt_native_batches(&transaction)?;
        let violations: i64 =
            transaction.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if violations != 0 {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        transaction.commit()?;
        connection.pragma_update(None, "foreign_keys", true)?;
        // Reclaim replaced checkpoints only after their replacement committed.
        // No historical events, snapshots or receipts are pruned.
        if compacting {
            connection.execute_batch("VACUUM")?;
        }
        Ok(Self {
            connection,
            directory,
        })
    }

    fn migrate_pull_requests(connection: &Connection) -> Result<(), StoreError> {
        let mut statement = connection.prepare(
            "SELECT snapshot, locator FROM read_states JOIN review_sources USING(review_id)
             ORDER BY COALESCE((SELECT MAX(generation) FROM workspaces
                WHERE json_extract(state,'$.review.id')=read_states.review_id),-1) DESC,
                read_states.review_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (snapshot, locator) = row?;
            let read: ReadState = serde_json::from_str(&snapshot)?;
            let pull = read.pull_request;
            let object = PullRequestObject {
                id: pull.id,
                revision: 0,
                source: PullRequestSource::Remote {
                    locator,
                    branches: None,
                },
                title: pull.title,
                body: pull.body,
            };
            connection.execute(
                "INSERT OR IGNORE INTO pull_requests(pull_request_id,object) VALUES (?1,?2)",
                (&object.id.0, serde_json::to_string(&object)?),
            )?;
        }
        Ok(())
    }

    /// Canonicalize pre-session workspace identities without changing receipt IDs.
    fn migrate_batch_workspaces(connection: &Connection) -> Result<(), StoreError> {
        let has_unique: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_index_list('workspaces') WHERE origin='u')",
            [],
            |row| row.get(0),
        )?;
        if !has_unique {
            return Ok(());
        }
        let rows = {
            let mut stmt = connection
                .prepare("SELECT workspace_id,state FROM workspaces ORDER BY workspace_id")?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        // Rebuild the parent under the same name; child FK declarations remain valid.
        connection.execute_batch("CREATE TABLE migrated_workspaces(workspace_id TEXT PRIMARY KEY NOT NULL,reviewer_id TEXT NOT NULL,pull_request_id TEXT NOT NULL,locator TEXT,generation INTEGER NOT NULL CHECK(generation>=0),state TEXT NOT NULL CHECK(json_valid(state))) STRICT;")?;
        for (old_id, state) in rows {
            let mut current: crate::workspace::Workspace = serde_json::from_str(&state)?;
            let canonical = current.batch.id.clone();
            let histories = {
                let mut stmt=connection.prepare("SELECT generation,state,action FROM workspace_history WHERE workspace_id=?1 ORDER BY generation")?;
                stmt.query_map([&old_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
            };
            let mut namespaces = Vec::<String>::new();
            for (_, state, _) in &histories {
                let ws: crate::workspace::Workspace = serde_json::from_str(state)?;
                if !namespaces.contains(&ws.batch.id.0) {
                    namespaces.push(ws.batch.id.0);
                }
            }
            if !namespaces.contains(&canonical.0) {
                namespaces.push(canonical.0.clone());
            }
            // Copy every old immutable log before replacing the canonical pointer.
            let mut logs = Vec::new();
            for namespace in &namespaces {
                if let Some(batch) = Self::read_batch(connection, &BatchId(namespace.clone()))? {
                    logs.push(batch);
                }
            }
            connection.execute("DELETE FROM write_events WHERE batch_id=?1", [&canonical.0])?;
            connection.execute(
                "DELETE FROM batch_revisions WHERE batch_id=?1",
                [&canonical.0],
            )?;
            for (revision, namespace) in namespaces.iter().enumerate() {
                if let Some(batch) = logs.iter().find(|batch| &batch.id.0 == namespace) {
                    connection.execute("INSERT INTO batch_revisions(batch_id,revision,review_id,namespace) VALUES (?1,?2,?3,?4)",(&canonical.0,revision as i64,&batch.review.0,namespace))?;
                    for (index, event) in batch.write.events.iter().enumerate() {
                        connection.execute("INSERT INTO write_events(batch_id,revision,sequence,event) VALUES (?1,?2,?3,?4)",(&canonical.0,revision as i64,(index+1) as i64,serde_json::to_string(event)?))?;
                    }
                }
                if namespace != &canonical.0 {
                    connection.execute(
                        "INSERT OR REPLACE INTO batch_aliases(alias,batch_id) VALUES (?1,?2)",
                        (namespace, &canonical.0),
                    )?;
                    connection.execute("INSERT OR IGNORE INTO operation_results(batch_id,operation_id,sequence,operation,mappings,response) SELECT ?1,operation_id,(SELECT COALESCE(MAX(sequence),0) FROM operation_results WHERE batch_id=?1)+ROW_NUMBER() OVER(ORDER BY sequence),operation,mappings,response FROM operation_results WHERE batch_id=?2",(&canonical.0,namespace))?;
                }
            }
            let revision = namespaces.iter().position(|id| id == &canonical.0).unwrap() as u64;
            current.id = canonical.0.clone();
            current.batch.revision = BatchRevision {
                number: revision,
                namespace: Some(canonical.0.clone()),
            };
            connection.execute(
                "UPDATE batches SET review_id=?2,revision=?3 WHERE batch_id=?1",
                (&canonical.0, &current.batch.review.0, revision as i64),
            )?;
            connection.execute("INSERT INTO migrated_workspaces(workspace_id,reviewer_id,pull_request_id,locator,generation,state) VALUES (?1,?2,?3,?4,?5,?6)",(&canonical.0,&current.review.reviewer.id.0,&current.review.read.pull_request.id.0,&current.locator,current.generation as i64,serde_json::to_string(&current)?))?;
            connection.execute(
                "DELETE FROM workspace_history WHERE workspace_id=?1",
                [&old_id],
            )?;
            for (generation, state, action) in histories {
                let mut ws: crate::workspace::Workspace = serde_json::from_str(&state)?;
                let namespace = ws.batch.id.0.clone();
                ws.id = canonical.0.clone();
                ws.batch.id = canonical.clone();
                ws.batch.revision = BatchRevision {
                    number: namespaces.iter().position(|id| id == &namespace).unwrap() as u64,
                    namespace: Some(namespace),
                };
                connection.execute("INSERT INTO workspace_history(workspace_id,generation,state,action) VALUES (?1,?2,?3,?4)",(&canonical.0,generation as i64,serde_json::to_string(&ws)?,action))?;
            }
            connection.execute(
                "UPDATE workspace_active SET workspace_id=?1 WHERE workspace_id=?2",
                (&canonical.0, &old_id),
            )?;
            connection.execute(
                "UPDATE workspace_publication_lock SET workspace_id=?1 WHERE workspace_id=?2",
                (&canonical.0, &old_id),
            )?;
            if old_id != canonical.0 {
                connection.execute(
                    "INSERT OR REPLACE INTO batch_aliases(alias,batch_id) VALUES (?1,?2)",
                    (&old_id, &canonical.0),
                )?;
            }
        }
        connection.execute_batch(
            "DROP TABLE workspaces; ALTER TABLE migrated_workspaces RENAME TO workspaces;",
        )?;
        Ok(())
    }

    fn read_results(
        connection: &Connection,
        batch: &BatchId,
    ) -> Result<Vec<OperationResult>, StoreError> {
        let mut statement=connection.prepare("SELECT operation,mappings,response FROM operation_results WHERE batch_id=?1 ORDER BY sequence")?;
        statement
            .query_and_then([&batch.0], |row| {
                let operation: String = row.get(0)?;
                let mappings: String = row.get(1)?;
                let response: String = row.get(2)?;
                Ok::<_, StoreError>(OperationResult {
                    operation: serde_json::from_str(&operation)?,
                    mappings: serde_json::from_str(&mappings)?,
                    response: serde_json::from_str(&response)?,
                })
            })?
            .collect()
    }

    fn adopt_native_batches(connection: &Connection) -> Result<(), StoreError> {
        let ids = {
            let mut statement=connection.prepare("SELECT batch_id FROM batches WHERE batch_id NOT IN (SELECT alias FROM batch_aliases) AND batch_id NOT IN (SELECT workspace_id FROM workspaces) ORDER BY batch_id")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in ids {
            let batch = Self::read_batch(connection, &BatchId(id.clone()))?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let review = Self::read_review(connection, &batch.review)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let mut workspace = crate::workspace::Workspace::new(review);
            workspace.id = id;
            workspace.batch = batch;
            workspace.results = Self::read_results(connection, &workspace.batch.id)?;
            workspace.completed = workspace
                .results
                .iter()
                .map(|result| result.operation.clone())
                .collect();
            for event in &workspace.batch.write.events {
                match event {
                    Event::CreateComment { comment } => {
                        workspace.destinations.insert(
                            comment.content.id.clone(),
                            crate::workspace::Destination::Standalone,
                        );
                    }
                    Event::SetReviewConclusion { conclusion } => {
                        workspace.wip = Some(crate::workspace::Wip {
                            id: uuid::Uuid::new_v4().to_string(),
                            active: true,
                            conclusion: Some(conclusion.clone()),
                        });
                    }
                    _ => {}
                }
            }
            if workspace.wip.is_some()
                && workspace.pending().is_ok_and(|pending| {
                    !pending.operations.iter().any(|operation| {
                        matches!(operation.change, Event::SetReviewConclusion { .. })
                    })
                })
            {
                workspace.wip = None;
            }
            Self::insert_workspace(connection, &mut workspace)?;
        }
        Ok(())
    }

    fn canonical_batch(connection: &Connection, id: &BatchId) -> Result<BatchId, StoreError> {
        Ok(connection
            .query_row(
                "SELECT batch_id FROM batch_aliases WHERE alias=?1",
                [&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(BatchId)
            .unwrap_or_else(|| id.clone()))
    }

    /// An OS-held lease prevents concurrent processes from resuming the same
    /// persisted attempt. Dropping the handle releases it, including on exit.
    pub fn publication_lock(&self) -> Result<fs::File, StoreError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("publish.lock"))?;
        file.try_lock().map_err(std::io::Error::from)?;
        Ok(file)
    }

    fn read_pull_request(
        connection: &Connection,
        id: &PullRequestId,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        connection
            .query_row(
                "SELECT object FROM pull_requests WHERE pull_request_id=?1",
                [&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|object| serde_json::from_str(&object).map_err(StoreError::from))
            .transpose()
    }

    fn read_pr_locator(
        connection: &Connection,
        locator: &str,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        connection
            .query_row(
                "SELECT object FROM pull_requests WHERE json_extract(object,'$.source.kind')='remote'
                 AND rtrim(json_extract(object,'$.source.locator'),'/')=rtrim(?1,'/') COLLATE NOCASE",
                [locator],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|object| serde_json::from_str(&object).map_err(StoreError::from))
            .transpose()
    }

    fn read_pr_intent(
        connection: &Connection,
        branches: &BranchPair,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        connection
            .query_row(
                "SELECT object FROM pull_requests WHERE json_extract(object,'$.source.kind')='local_intent'
                 AND json_extract(object,'$.source.branches.source.repository')=?1 COLLATE NOCASE
                 AND json_extract(object,'$.source.branches.source.branch')=?2
                 AND json_extract(object,'$.source.branches.sink.repository')=?3 COLLATE NOCASE
                 AND json_extract(object,'$.source.branches.sink.branch')=?4",
                (&branches.source.repository, &branches.source.branch,
                 &branches.sink.repository, &branches.sink.branch),
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|object| serde_json::from_str(&object).map_err(StoreError::from))
            .transpose()
    }

    fn write_pr(connection: &Connection, object: &PullRequestObject) -> Result<(), StoreError> {
        connection.execute(
            "INSERT INTO pull_requests(pull_request_id,object) VALUES (?1,?2)
             ON CONFLICT(pull_request_id) DO UPDATE SET object=excluded.object",
            (&object.id.0, serde_json::to_string(object)?),
        )?;
        Ok(())
    }

    fn checked_pr(
        connection: &Connection,
        id: &PullRequestId,
        revision: u64,
    ) -> Result<PullRequestObject, StoreError> {
        let object = Self::read_pull_request(connection, id)?.ok_or_else(|| {
            crate::workspace::WorkspaceError::Invalid("PR object does not exist".into())
        })?;
        if object.revision != revision {
            return Err(crate::workspace::WorkspaceError::Invalid(format!(
                "PR details changed in another session (expected revision {revision}, found {}); reopen before saving",
                object.revision
            )).into());
        }
        Ok(object)
    }

    pub fn pull_request(
        &self,
        id: &PullRequestId,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        Self::read_pull_request(&self.connection, id)
    }

    pub fn pull_request_for_locator(
        &self,
        locator: &str,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        Self::read_pr_locator(&self.connection, locator)
    }

    pub fn intent_for_branches(
        &self,
        branches: &BranchPair,
    ) -> Result<Option<PullRequestObject>, StoreError> {
        Self::read_pr_intent(&self.connection, branches)
    }

    /// Create the shared PR identity before any review or batch references it.
    pub fn create_or_use_intent(
        &mut self,
        branches: &BranchPair,
        title: &str,
        body: &str,
    ) -> Result<PullRequestObject, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let object = match Self::read_pr_intent(&transaction, branches)? {
            Some(object) => object,
            None => {
                let object = PullRequestObject {
                    id: PullRequestId(uuid::Uuid::new_v4().to_string()),
                    revision: 0,
                    source: PullRequestSource::LocalIntent {
                        branches: branches.clone(),
                    },
                    title: title.into(),
                    body: body.into(),
                };
                Self::write_pr(&transaction, &object)?;
                object
            }
        };
        transaction.commit()?;
        Ok(object)
    }

    pub fn pr_resolution(
        &self,
        id: &PullRequestId,
    ) -> Result<Option<PullRequestResolution>, StoreError> {
        self.connection
            .query_row(
                "SELECT state FROM pr_resolutions WHERE pull_request_id=?1",
                [&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|state| serde_json::from_str(&state).map_err(StoreError::from))
            .transpose()
    }

    pub fn mark_pr_resolution(
        &mut self,
        id: &PullRequestId,
        revision: u64,
        resolution: &PullRequestResolution,
    ) -> Result<PullRequestObject, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut object = Self::checked_pr(&transaction, id, revision)?;
        if !matches!(&object.source, PullRequestSource::LocalIntent { branches }
            if branches == &resolution.branches)
            || object.title != resolution.title
            || object.body != resolution.body
        {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "PR creation input differs from saved intent".into(),
            )
            .into());
        }
        transaction.execute(
            "INSERT INTO pr_resolutions(pull_request_id,state) VALUES (?1,?2)",
            (&id.0, serde_json::to_string(resolution)?),
        )?;
        object.revision += 1;
        Self::write_pr(&transaction, &object)?;
        transaction.commit()?;
        Ok(object)
    }

    pub fn clear_pr_resolution(
        &mut self,
        id: &PullRequestId,
        revision: u64,
    ) -> Result<PullRequestObject, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut object = Self::checked_pr(&transaction, id, revision)?;
        if transaction.execute(
            "DELETE FROM pr_resolutions WHERE pull_request_id=?1",
            [&id.0],
        )? != 0
        {
            object.revision += 1;
            Self::write_pr(&transaction, &object)?;
        }
        transaction.commit()?;
        Ok(object)
    }

    fn attach_pr_backend(
        connection: &Connection,
        id: &PullRequestId,
        backend: &ReviewBackend,
    ) -> Result<(), StoreError> {
        let reviews = {
            let mut statement = connection.prepare(
                "SELECT read_states.review_id FROM read_states
                 LEFT JOIN review_sources USING(review_id)
                 WHERE json_extract(snapshot,'$.pull_request.id')=?1
                    AND review_sources.review_id IS NULL",
            )?;
            statement
                .query_map([&id.0], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for review in reviews {
            connection.execute(
                "INSERT INTO review_sources(review_id,backend,locator) VALUES (?1,?2,?3)",
                (&review, &backend.source.backend, &backend.source.locator),
            )?;
            for mapping in &backend.mappings {
                let core = if mapping.resource == "pull_request" {
                    &id.0
                } else {
                    &mapping.core_id
                };
                connection.execute(
                    "INSERT INTO id_mappings(review_id,resource,core_id,remote_id) VALUES (?1,?2,?3,?4)",
                    (&review, &mapping.resource, core, &mapping.remote_id),
                )?;
            }
        }
        connection.execute(
            "UPDATE workspaces SET locator=?2 WHERE pull_request_id=?1 AND locator IS NULL",
            (&id.0, &backend.source.locator),
        )?;
        Ok(())
    }

    fn bind_pr(
        connection: &Connection,
        object: &mut PullRequestObject,
        backend: &ReviewBackend,
        branches: Option<&BranchPair>,
    ) -> Result<(), StoreError> {
        match &object.source {
            PullRequestSource::Remote { locator, .. }
                if !locator
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(backend.source.locator.trim_end_matches('/')) =>
            {
                return Err(crate::workspace::WorkspaceError::Invalid(
                    "a bound PR cannot be redirected to a different remote PR".into(),
                )
                .into());
            }
            PullRequestSource::LocalIntent { branches: saved }
                if branches.is_some_and(|incoming| {
                    incoming.source.branch != saved.source.branch
                        || incoming.sink.branch != saved.sink.branch
                        || !incoming
                            .source
                            .repository
                            .eq_ignore_ascii_case(&saved.source.repository)
                        || !incoming
                            .sink
                            .repository
                            .eq_ignore_ascii_case(&saved.sink.repository)
                }) =>
            {
                return Err(crate::workspace::WorkspaceError::Invalid(
                    "resolved PR branches differ from saved intent".into(),
                )
                .into());
            }
            _ => {}
        }
        if let Some(existing) = Self::read_pr_locator(connection, &backend.source.locator)?
            && existing.id != object.id
        {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "remote PR already belongs to another local PR object; both sessions were preserved".into(),
            ).into());
        }
        let saved = match &object.source {
            PullRequestSource::LocalIntent { branches } => Some(branches),
            PullRequestSource::Remote { branches, .. } => branches.as_ref(),
        };
        object.source = PullRequestSource::Remote {
            locator: backend.source.locator.clone(),
            branches: branches.or(saved).cloned(),
        };
        object.revision += 1;
        Self::write_pr(connection, object)?;
        Self::attach_pr_backend(connection, &object.id, backend)?;
        connection.execute(
            "DELETE FROM pr_resolutions WHERE pull_request_id=?1",
            [&object.id.0],
        )?;
        Ok(())
    }

    /// Persist the resolved resource and mappings before any dependent write.
    pub fn bind_pull_request(
        &mut self,
        id: &PullRequestId,
        revision: u64,
        backend: &ReviewBackend,
        branches: Option<&BranchPair>,
        details: Option<(&str, &str)>,
    ) -> Result<PullRequestObject, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut object = Self::checked_pr(&transaction, id, revision)?;
        if let Some((title, body)) = details {
            object.title = title.into();
            object.body = body.into();
        }
        Self::bind_pr(&transaction, &mut object, backend, branches)?;
        transaction.commit()?;
        Ok(object)
    }

    /// Complete an authenticated reviewer's mapping on a locally created
    /// baseline after another reviewer resolved the shared PR object.
    pub fn map_review_reviewer(
        &mut self,
        review: &ReviewId,
        reviewer: &crate::model::UserId,
        remote_id: &str,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let saved =
            Self::read_review(&transaction, review)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if saved.reviewer.id != *reviewer {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "authenticated user differs from saved reviewer".into(),
            )
            .into());
        }
        let attached: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_sources WHERE review_id=?1)",
            [&review.0],
            |row| row.get(0),
        )?;
        if !attached {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "review has no provider metadata".into(),
            )
            .into());
        }
        let existing = transaction.query_row(
            "SELECT remote_id FROM id_mappings WHERE review_id=?1 AND resource='user' AND core_id=?2",
            (&review.0, &reviewer.0), |row| row.get::<_, String>(0),
        ).optional()?;
        match existing {
            Some(existing) if existing != remote_id => {
                return Err(crate::workspace::WorkspaceError::Invalid(
                    "saved reviewer mapping differs from authenticated provider identity".into(),
                )
                .into());
            }
            Some(_) => {}
            None => {
                transaction.execute(
                    "INSERT INTO id_mappings(review_id,resource,core_id,remote_id) VALUES (?1,'user',?2,?3)",
                    (&review.0, &reviewer.0, remote_id),
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Creates a review with its own immutable fetched baseline.
    pub fn create_review(
        &mut self,
        id: &ReviewId,
        reviewer: &User,
        read: &ReadState,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        Self::insert_review(&transaction, id, reviewer, read)?;
        if read.pull_request.status == crate::model::PullRequestStatus::Local
            && Self::read_pull_request(&transaction, &read.pull_request.id)?
                .is_some_and(|object| matches!(object.source, PullRequestSource::Remote { .. }))
        {
            let attached = transaction
                .query_row(
                    "SELECT review_id FROM read_states JOIN review_sources USING(review_id)
                 WHERE json_extract(snapshot,'$.pull_request.id')=?1 LIMIT 1",
                    [&read.pull_request.id.0],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(attached) = attached
                && let Some(backend) = Self::read_backend(&transaction, &ReviewId(attached))?
            {
                Self::attach_pr_backend(&transaction, &read.pull_request.id, &backend)?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Reuse an unchanged local baseline without changing any existing identity.
    pub fn cache_review(
        &mut self,
        reviewer: &User,
        read: &ReadState,
    ) -> Result<ReviewId, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(id) = Self::matching_review(&transaction, reviewer, read, None)? {
            transaction.commit()?;
            return Ok(id);
        }
        let id = ReviewId(uuid::Uuid::new_v4().to_string());
        Self::insert_review(&transaction, &id, reviewer, read)?;
        transaction.commit()?;
        Ok(id)
    }

    fn insert_review(
        transaction: &Transaction<'_>,
        id: &ReviewId,
        reviewer: &User,
        read: &ReadState,
    ) -> Result<(), StoreError> {
        let reviewer = serde_json::to_string(reviewer)?;
        let snapshot = Self::store_value(transaction, &serde_json::to_value(read)?)?;
        transaction.execute(
            "INSERT INTO reviews (review_id, reviewer) VALUES (?1, ?2)",
            (&id.0, reviewer),
        )?;
        transaction.execute(
            "INSERT INTO review_snapshots (review_id, value_id) VALUES (?1, ?2)",
            (&id.0, snapshot),
        )?;
        Ok(())
    }

    /// Saves a complete fetched snapshot and its provider identities together.
    pub fn import_review(
        &mut self,
        id: &ReviewId,
        reviewer: &User,
        read: &ReadState,
        backend: &ReviewBackend,
    ) -> Result<(), StoreError> {
        self.import_review_with_branches(id, reviewer, read, backend, None)
    }

    pub fn import_review_with_branches(
        &mut self,
        id: &ReviewId,
        reviewer: &User,
        read: &ReadState,
        backend: &ReviewBackend,
        branches: Option<&BranchPair>,
    ) -> Result<(), StoreError> {
        self.save_import(Some(id), reviewer, read, backend, branches)
            .map(|_| ())
    }

    /// Cache a fetched baseline only when reviewer, state and provider identities
    /// all match; explicit imports still retain their caller-assigned IDs.
    pub fn cache_import_review(
        &mut self,
        reviewer: &User,
        read: &ReadState,
        backend: &ReviewBackend,
        branches: Option<&BranchPair>,
    ) -> Result<ReviewId, StoreError> {
        self.save_import(None, reviewer, read, backend, branches)
    }

    fn matching_review(
        connection: &Connection,
        reviewer: &User,
        read: &ReadState,
        backend: Option<&ReviewBackend>,
    ) -> Result<Option<ReviewId>, StoreError> {
        let value = Self::store_value(connection, &serde_json::to_value(read)?)?;
        let candidates = {
            let mut statement = connection.prepare(
                "SELECT review_id,reviewer FROM reviews JOIN review_snapshots USING(review_id)
                 WHERE value_id=?1 ORDER BY review_id",
            )?;
            statement
                .query_and_then([value], |row| {
                    let id = ReviewId(row.get(0)?);
                    let saved: String = row.get(1)?;
                    Ok::<_, StoreError>((id, serde_json::from_str::<User>(&saved)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let sorted = |mappings: &[IdMapping]| {
            let mut mappings = mappings.to_vec();
            mappings.sort_by(|left, right| {
                (&left.resource, &left.core_id, &left.remote_id).cmp(&(
                    &right.resource,
                    &right.core_id,
                    &right.remote_id,
                ))
            });
            mappings
        };
        for (id, saved_reviewer) in candidates {
            if &saved_reviewer != reviewer {
                continue;
            }
            let saved = Self::read_backend(connection, &id)?;
            if match (backend, saved) {
                (None, None) => true,
                (Some(incoming), Some(saved)) => {
                    incoming.source == saved.source
                        && sorted(&incoming.mappings) == sorted(&saved.mappings)
                }
                _ => false,
            } {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    fn save_import(
        &mut self,
        id: Option<&ReviewId>,
        reviewer: &User,
        read: &ReadState,
        backend: &ReviewBackend,
        branches: Option<&BranchPair>,
    ) -> Result<ReviewId, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut read = read.clone();
        let mut backend = backend.clone();
        let mut object = match Self::read_pr_locator(&transaction, &backend.source.locator)? {
            Some(object) => object,
            None => match branches
                .map(|branches| Self::read_pr_intent(&transaction, branches))
                .transpose()?
                .flatten()
            {
                Some(mut object) => {
                    Self::bind_pr(&transaction, &mut object, &backend, branches)?;
                    object
                }
                None => PullRequestObject {
                    id: read.pull_request.id.clone(),
                    revision: 0,
                    source: PullRequestSource::Remote {
                        locator: backend.source.locator.clone(),
                        branches: branches.cloned(),
                    },
                    title: read.pull_request.title.clone(),
                    body: read.pull_request.body.clone(),
                },
            },
        };
        if let Some(existing) = Self::read_pull_request(&transaction, &object.id)? {
            let same_resource = match (&existing.source, &object.source) {
                (
                    PullRequestSource::Remote { locator: old, .. },
                    PullRequestSource::Remote { locator: new, .. },
                ) => old
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(new.trim_end_matches('/')),
                _ => existing.source == object.source,
            };
            if !same_resource {
                return Err(crate::workspace::WorkspaceError::Invalid(
                    "PR identity already belongs to a different PR object".into(),
                )
                .into());
            }
        }
        let old = object.clone();
        object.title = read.pull_request.title.clone();
        object.body = read.pull_request.body.clone();
        if let PullRequestSource::Remote {
            branches: saved, ..
        } = &mut object.source
            && branches.is_some()
        {
            *saved = branches.cloned();
        }
        if object != old {
            object.revision += 1;
        }
        Self::write_pr(&transaction, &object)?;
        read.pull_request.id = object.id.clone();
        for mapping in &mut backend.mappings {
            if mapping.resource == "pull_request" {
                mapping.core_id = object.id.0.clone();
            }
        }
        Self::restore_original_comparisons(&transaction, &mut read, &backend)?;
        let id = match id {
            Some(id) => id.clone(),
            None => match Self::matching_review(&transaction, reviewer, &read, Some(&backend))? {
                Some(id) => {
                    transaction.commit()?;
                    return Ok(id);
                }
                None => ReviewId(uuid::Uuid::new_v4().to_string()),
            },
        };
        Self::insert_review(&transaction, &id, reviewer, &read)?;
        transaction.execute(
            "INSERT INTO review_sources (review_id, backend, locator) VALUES (?1, ?2, ?3)",
            (&id.0, &backend.source.backend, &backend.source.locator),
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO id_mappings (review_id, resource, core_id, remote_id)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for mapping in &backend.mappings {
                statement.execute((
                    &id.0,
                    &mapping.resource,
                    &mapping.core_id,
                    &mapping.remote_id,
                ))?;
            }
        }
        transaction.commit()?;
        Ok(id)
    }

    fn restore_original_comparisons(
        connection: &Connection,
        read: &mut ReadState,
        backend: &ReviewBackend,
    ) -> Result<(), StoreError> {
        let mut statement = connection.prepare(
            "SELECT results.operation,json_extract(mapping.value,'$.remote_id')
             FROM operation_results AS results
             JOIN batches ON batches.batch_id=results.batch_id
             JOIN review_sources AS source ON source.review_id=batches.review_id
             JOIN json_each(results.mappings) AS mapping
             WHERE source.backend=?1
                AND (source.locator=?2 OR (?1='github'
                    AND rtrim(source.locator,'/')=rtrim(?2,'/') COLLATE NOCASE))
                AND json_extract(results.operation,'$.change.kind')='create_comment'
                AND json_extract(mapping.value,'$.resource')='review_comment'
                AND json_extract(mapping.value,'$.core_id')=
                    json_extract(results.operation,'$.change.comment.id')
             ORDER BY results.batch_id,results.sequence",
        )?;
        let operations = statement
            .query_map((&backend.source.backend, &backend.source.locator), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
        let mut originals = BTreeMap::new();
        for operation in operations {
            let (operation, remote) = operation?;
            let operation: Operation = serde_json::from_str(&operation)?;
            if let Event::CreateComment { comment } = operation.change
                && let CommentTarget::Diff(location) = comment.target
            {
                originals.entry(remote).or_insert(location);
            }
        }
        for comment in &mut read.comments {
            let CommentTarget::Diff(location) = &mut comment.target else {
                continue;
            };
            let ReadCommentRevision::Commit { head } = &location.original.revision else {
                continue;
            };
            let Some(mapping) = backend.mappings.iter().find(|mapping| {
                mapping.resource == "review_comment" && mapping.core_id == comment.content.id.0
            }) else {
                continue;
            };
            if let Some(original) = originals.get(&mapping.remote_id)
                && original.revision.head == *head
                && original.position == location.original.position
            {
                location.original.revision =
                    ReadCommentRevision::Comparison(original.revision.clone());
            }
        }
        Ok(())
    }

    pub fn review_backend(&mut self, id: &ReviewId) -> Result<Option<ReviewBackend>, StoreError> {
        let transaction = self.connection.transaction()?;
        let backend = Self::read_backend(&transaction, id)?;
        transaction.commit()?;
        Ok(backend)
    }

    fn read_backend(
        connection: &Connection,
        id: &ReviewId,
    ) -> Result<Option<ReviewBackend>, StoreError> {
        let source = connection
            .query_row(
                "SELECT backend, locator FROM review_sources WHERE review_id = ?1",
                [&id.0],
                |row| {
                    Ok(ReviewSource {
                        backend: row.get(0)?,
                        locator: row.get(1)?,
                    })
                },
            )
            .optional()?;
        source
            .map(|source| {
                let mut statement = connection.prepare(
                    "SELECT resource, core_id, remote_id FROM id_mappings
                 WHERE review_id = ?1 ORDER BY resource, core_id",
                )?;
                let mappings = statement
                    .query_map([&id.0], |row| {
                        Ok(IdMapping {
                            resource: row.get(0)?,
                            core_id: row.get(1)?,
                            remote_id: row.get(2)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                Ok::<ReviewBackend, StoreError>(ReviewBackend { source, mappings })
            })
            .transpose()
    }

    pub fn get_review(&self, id: &ReviewId) -> Result<Option<Review>, StoreError> {
        Self::read_review(&self.connection, id)
    }

    fn read_review(connection: &Connection, id: &ReviewId) -> Result<Option<Review>, StoreError> {
        let metadata = connection
            .query_row(
                "SELECT reviewer, snapshot
                 FROM reviews JOIN read_states USING (review_id)
                 WHERE review_id = ?1",
                [&id.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        metadata
            .map(|(reviewer, snapshot)| {
                Ok::<Review, StoreError>(Review {
                    id: id.clone(),
                    reviewer: serde_json::from_str(&reviewer)?,
                    read: serde_json::from_str(&snapshot)?,
                })
            })
            .transpose()
    }

    pub fn create_batch(&mut self, id: &BatchId, review: &ReviewId) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let review =
            Self::read_review(&transaction, review)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let mut workspace = crate::workspace::Workspace::new(review);
        workspace.id = id.0.clone();
        workspace.batch.id = id.clone();
        Self::insert_workspace(&transaction, &mut workspace)?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads the review link and ordered log from one consistent snapshot.
    pub fn get_batch(&mut self, id: &BatchId) -> Result<Option<Batch>, StoreError> {
        let transaction = self.connection.transaction()?;
        let batch = Self::read_batch(&transaction, id)?;
        transaction.commit()?;
        Ok(batch)
    }

    fn read_batch(connection: &Connection, id: &BatchId) -> Result<Option<Batch>, StoreError> {
        let id = Self::canonical_batch(connection, id)?;
        let revision = connection
            .query_row(
                "SELECT revision FROM batches WHERE batch_id=?1",
                [&id.0],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        revision
            .map(|revision| Self::read_revision(connection, &id, revision as u64))
            .transpose()
            .map(Option::flatten)
    }

    fn read_revision(
        connection: &Connection,
        id: &BatchId,
        revision: u64,
    ) -> Result<Option<Batch>, StoreError> {
        let metadata = connection
            .query_row(
                "SELECT review_id,namespace FROM batch_revisions WHERE batch_id=?1 AND revision=?2",
                (&id.0, revision as i64),
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        metadata.map(|(review,namespace)|{
            let mut statement=connection.prepare("SELECT event FROM write_events WHERE batch_id=?1 AND revision=?2 ORDER BY sequence")?;
            let events=statement.query_and_then((&id.0,revision as i64),|row|{
                let event:String=row.get(0)?;
                Ok::<Event,StoreError>(serde_json::from_str(&event)?)
            })?.collect::<Result<Vec<_>,_>>()?;
            Ok(Batch{
                id:id.clone(),revision:BatchRevision{number:revision,namespace:Some(namespace)},
                review:ReviewId(review),write:WriteState{events},
            })
        }).transpose()
    }

    /// Reads an immutable superseded log without moving the canonical batch.
    pub fn batch_revision(&self, id: &BatchId, revision: u64) -> Result<Option<Batch>, StoreError> {
        let id = Self::canonical_batch(&self.connection, id)?;
        Self::read_revision(&self.connection, &id, revision)
    }

    /// Appends without replacing or collapsing earlier events. Old batches stay
    /// editable; ordered replay validation and insertion share one transaction.
    /// Final thread consistency is checked when projecting or preparing a commit.
    pub fn append_event(&mut self, id: &BatchId, event: &Event) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, &id.0)?
            .ok_or_else(|| StoreError::BatchNotFound(id.clone()))?;
        if workspace.attempt.is_some() || !workspace.suspended_attempts.is_empty() {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "reconcile publication before staging raw events".into(),
            )
            .into());
        }
        let generation = workspace.generation;
        workspace.batch.write.events.push(event.clone());
        validate_events(&workspace.review, &workspace.batch)?;
        match event {
            Event::CreateComment { comment } => {
                workspace.destinations.insert(
                    comment.content.id.clone(),
                    crate::workspace::Destination::Standalone,
                );
            }
            Event::SetReviewConclusion { conclusion } => {
                if workspace.wip.is_none() {
                    workspace.wip = Some(crate::workspace::Wip {
                        id: uuid::Uuid::new_v4().to_string(),
                        active: true,
                        conclusion: None,
                    });
                }
                workspace.wip.as_mut().unwrap().conclusion = Some(conclusion.clone());
            }
            _ => {}
        }
        Self::persist_workspace(
            &transaction,
            generation,
            &mut workspace,
            &serde_json::json!({"action":"stage","event":event}),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn review_ids(&self) -> Result<Vec<ReviewId>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT review_id FROM reviews ORDER BY review_id")?;
        let ids = statement
            .query_map([], |row| row.get(0).map(ReviewId))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    pub fn batch_ids(&self, review: &ReviewId) -> Result<Vec<BatchId>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT batch_id FROM batches WHERE review_id = ?1 AND batch_id NOT IN (SELECT alias FROM batch_aliases) ORDER BY batch_id")?;
        let ids = statement
            .query_map([&review.0], |row| row.get(0).map(BatchId))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    /// Reads the full successful-write ledger in execution order. Historical
    /// receipts remain after refresh; Workspace.completed is the current delta.
    pub fn operation_results(
        &mut self,
        batch: &BatchId,
    ) -> Result<Vec<OperationResult>, StoreError> {
        let transaction = self.connection.transaction()?;
        let batch = Self::canonical_batch(&transaction, batch)?;
        let exists = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM batches WHERE batch_id = ?1)",
            [&batch.0],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(StoreError::BatchNotFound(batch.clone()));
        }
        let results = Self::read_results(&transaction, &batch)?;
        transaction.commit()?;
        Ok(results)
    }

    /// Records one completed operation and its provider mappings together.
    /// A duplicate operation cannot overwrite the original successful result.
    pub fn record_operation_result(
        &mut self,
        batch: &BatchId,
        operation: &Operation,
        mappings: &[IdMapping],
        response: &Value,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, &batch.0)?
            .ok_or_else(|| StoreError::BatchNotFound(batch.clone()))?;
        if workspace.has_result(&operation.id) {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        let generation = workspace.generation;
        workspace.completed.push(operation.clone());
        workspace.results.push(OperationResult {
            operation: operation.clone(),
            mappings: mappings.to_vec(),
            response: response.clone(),
        });
        if matches!(operation.change, Event::SetReviewConclusion { .. }) {
            workspace.wip = None;
        }
        Self::persist_workspace(
            &transaction,
            generation,
            &mut workspace,
            &serde_json::json!({"action":"record_result","result":{"operation":operation,"mappings":mappings,"response":response}}),
        )?;
        transaction.commit()?;
        Ok(())
    }
}

impl Store {
    fn read_workspace(
        connection: &Connection,
        id: &str,
    ) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        let id = Self::canonical_batch(connection, &BatchId(id.into()))?;
        connection
            .query_row(
                "SELECT state FROM workspaces WHERE workspace_id = ?1",
                [&id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|state| {
                let mut workspace =
                    Self::hydrate_workspace(connection, serde_json::from_str(&state)?)?;
                workspace.batch = Self::read_batch(connection, &id)?
                    .ok_or_else(|| StoreError::BatchNotFound(id.clone()))?;
                workspace.results = Self::read_results(connection, &id)?;
                workspace.review = Self::read_review(connection, &workspace.batch.review)?
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                Self::load_summary_ids(connection, &mut workspace)?;
                Ok(workspace)
            })
            .transpose()
    }

    pub fn workspace(&self, id: &str) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        Self::read_workspace(&self.connection, id)
    }

    pub fn workspace_at_generation(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        let id = Self::canonical_batch(&self.connection, &BatchId(id.into()))?;
        Self::history_states(&self.connection, &id.0, Some(generation))?
            .into_iter()
            .find(|row| row.generation == generation)
            .map(|row| Self::hydrate_workspace(&self.connection, row.state))
            .transpose()
    }

    pub fn active_workspace(
        &self,
        reviewer: &crate::model::UserId,
    ) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        let id = self
            .connection
            .query_row(
                "SELECT workspace_id FROM workspace_active WHERE reviewer_id = ?1",
                [&reviewer.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| self.workspace(&id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn last_active_workspace(&self) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        let id=self.connection.query_row("SELECT workspace_id FROM workspace_active ORDER BY selected DESC, reviewer_id LIMIT 1",[],|row|row.get::<_,String>(0)).optional()?;
        id.map(|id| self.workspace(&id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn cached_workspace(
        &self,
        locator: &str,
    ) -> Result<Option<crate::workspace::Workspace>, StoreError> {
        let id=self.connection.query_row(
            "SELECT w.workspace_id FROM workspaces w LEFT JOIN workspace_active a USING (reviewer_id)
             WHERE w.locator = ?1 ORDER BY a.selected DESC, w.generation DESC, w.workspace_id LIMIT 1",
            [locator],|row|row.get::<_,String>(0)).optional()?;
        id.map(|id| self.workspace(&id))
            .transpose()
            .map(Option::flatten)
    }

    fn select_workspace(
        connection: &Connection,
        workspace: &crate::workspace::Workspace,
    ) -> Result<(), StoreError> {
        connection.execute(
            "INSERT INTO workspace_active (reviewer_id,workspace_id,selected)
             VALUES (?1,?2,(SELECT COALESCE(MAX(selected),0)+1 FROM workspace_active))
             ON CONFLICT (reviewer_id) DO UPDATE SET workspace_id=excluded.workspace_id, selected=excluded.selected",
            (&workspace.review.reviewer.id.0,&workspace.id))?;
        Ok(())
    }

    fn insert_workspace(
        connection: &Connection,
        workspace: &mut crate::workspace::Workspace,
    ) -> Result<(), StoreError> {
        Self::persist_workspace_batch(connection, workspace)?;
        Self::load_summary_ids(connection, workspace)?;
        let state = Self::compact_workspace(connection, workspace)?;
        connection.execute("INSERT INTO workspaces(workspace_id,reviewer_id,pull_request_id,locator,generation,state) VALUES (?1,?2,?3,?4,?5,?6)",(&workspace.id,&workspace.review.reviewer.id.0,&workspace.review.read.pull_request.id.0,&workspace.locator,workspace.generation as i64,serde_json::to_string(&state)?))?;
        connection.execute("INSERT INTO workspace_history(workspace_id,generation,state,action) VALUES (?1,?2,?3,?4)",(&workspace.id,workspace.generation as i64,Self::state_delta(&serde_json::json!({}), &state)?,r#"{"action":"open"}"#))?;
        Ok(())
    }

    pub fn batch_workspaces(&self) -> Result<Vec<crate::workspace::Workspace>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT workspace_id FROM workspaces ORDER BY pull_request_id,workspace_id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                Self::read_workspace(&self.connection, &id)?
                    .ok_or(rusqlite::Error::QueryReturnedNoRows.into())
            })
            .collect()
    }

    pub fn select_batch(
        &mut self,
        id: &BatchId,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = Self::read_workspace(&transaction, &id.0)?
            .ok_or_else(|| StoreError::BatchNotFound(id.clone()))?;
        Self::select_workspace(&transaction, &workspace)?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn create_batch_workspace(
        &mut self,
        review: &ReviewId,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let review =
            Self::read_review(&transaction, review)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let mut workspace = crate::workspace::Workspace::new(review);
        Self::insert_workspace(&transaction, &mut workspace)?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn resolve_workspace(&mut self, review: &ReviewId) -> Result<BatchRoute, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let review =
            Self::read_review(&transaction, review)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let ids = {
            let mut statement=transaction.prepare("SELECT workspace_id FROM workspaces WHERE reviewer_id=?1 AND pull_request_id=?2 ORDER BY workspace_id")?;
            statement
                .query_map(
                    (&review.reviewer.id.0, &review.read.pull_request.id.0),
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut candidates = Vec::new();
        for id in ids {
            let workspace = Self::read_workspace(&transaction, &id)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            if workspace.is_unfinished()? {
                candidates.push(workspace);
            }
        }
        let route = match candidates.len() {
            0 => {
                let mut workspace = crate::workspace::Workspace::new(review);
                Self::insert_workspace(&transaction, &mut workspace)?;
                BatchRoute::Open(Box::new(workspace))
            }
            1 => BatchRoute::Open(Box::new(candidates.remove(0))),
            _ => BatchRoute::Choose(candidates),
        };
        transaction.commit()?;
        Ok(route)
    }

    pub fn open_workspace(
        &mut self,
        review: &ReviewId,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        match self.resolve_workspace(review)? {
            BatchRoute::Open(workspace) => self.select_batch(&workspace.batch.id),
            BatchRoute::Choose(_) => Err(crate::workspace::WorkspaceError::Invalid(
                "multiple unfinished batches; select a batch".into(),
            )
            .into()),
        }
    }

    pub fn batch_history(&self, id: &BatchId) -> Result<Vec<BatchHistoryEntry>, StoreError> {
        let id = Self::canonical_batch(&self.connection, id)?;
        Self::history_states(&self.connection, &id.0, None)?
            .into_iter()
            .map(|row| {
                Ok(BatchHistoryEntry {
                    generation: row.generation,
                    state: Self::hydrate_workspace(&self.connection, row.state)?,
                    action: row.action,
                })
            })
            .collect()
    }

    fn load_summary_ids(
        connection: &Connection,
        workspace: &mut crate::workspace::Workspace,
    ) -> Result<(), StoreError> {
        workspace.locator = connection
            .query_row(
                "SELECT locator FROM review_sources WHERE review_id=?1",
                [&workspace.review.id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let mut statement = connection.prepare(
            "SELECT core_id FROM id_mappings WHERE review_id=?1 AND resource='review_summary'",
        )?;
        workspace.summary_comments = statement
            .query_map([&workspace.review.id.0], |row| {
                row.get::<_, String>(0).map(crate::model::CommentId)
            })?
            .collect::<Result<_, _>>()?;
        let mut statement=connection.prepare("SELECT core_id,resource FROM id_mappings WHERE review_id=?1 AND resource LIKE 'created_at:%'")?;
        workspace.chronology = statement
            .query_map([&workspace.review.id.0], |row| {
                Ok((
                    crate::model::CommentId(row.get(0)?),
                    row.get::<_, String>(1)?[11..].to_string(),
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    fn persist_workspace_batch(
        connection: &Connection,
        workspace: &mut crate::workspace::Workspace,
    ) -> Result<(), StoreError> {
        let existing = Self::read_batch(connection, &workspace.batch.id)?;
        if let Some(old) = &existing {
            workspace.batch.revision = old.revision.clone();
        }
        let prefix = existing.as_ref().is_none_or(|old| {
            old.review == workspace.batch.review
                && workspace.batch.write.events.starts_with(&old.write.events)
        });
        let start = if prefix {
            existing
                .as_ref()
                .map_or(0, |batch| batch.write.events.len())
        } else {
            workspace.batch.revision.number = existing.as_ref().unwrap().revision.number + 1;
            workspace.batch.revision.namespace = Some(format!(
                "{}:revision:{}",
                workspace.batch.id.0, workspace.batch.revision.number
            ));
            0
        };
        if workspace.batch.revision.namespace.is_none() {
            workspace.batch.revision.namespace = Some(workspace.batch.id.0.clone());
        }
        connection.execute("INSERT INTO batches(batch_id,review_id,revision) VALUES (?1,?2,?3) ON CONFLICT(batch_id) DO UPDATE SET review_id=excluded.review_id,revision=excluded.revision",(&workspace.batch.id.0,&workspace.batch.review.0,workspace.batch.revision.number as i64))?;
        connection.execute("INSERT OR IGNORE INTO batch_revisions(batch_id,revision,review_id,namespace) VALUES (?1,?2,?3,?4)",(&workspace.batch.id.0,workspace.batch.revision.number as i64,&workspace.batch.review.0,&workspace.batch.revision.namespace))?;
        for (index, event) in workspace.batch.write.events.iter().enumerate().skip(start) {
            connection.execute(
                "INSERT INTO write_events(batch_id,revision,sequence,event) VALUES (?1,?2,?3,?4)",
                (
                    &workspace.batch.id.0,
                    workspace.batch.revision.number as i64,
                    (index + 1) as i64,
                    serde_json::to_string(event)?,
                ),
            )?;
        }
        Ok(())
    }

    fn persist_workspace(
        connection: &Connection,
        expected: u64,
        workspace: &mut crate::workspace::Workspace,
        action: &Value,
    ) -> Result<(), StoreError> {
        use crate::workspace::WorkspaceError;
        let actual = Self::read_workspace(connection, &workspace.id)?
            .ok_or_else(|| WorkspaceError::NotFound(workspace.id.clone()))?;
        if actual.generation != expected {
            return Err(WorkspaceError::StaleGeneration {
                expected,
                actual: actual.generation,
            }
            .into());
        }
        if workspace.id != workspace.batch.id.0 || workspace.batch.id != actual.batch.id {
            return Err(WorkspaceError::Invalid("batch identity is immutable".into()).into());
        }
        if workspace.review.reviewer.id != actual.review.reviewer.id
            || workspace.review.read.pull_request.id != actual.review.read.pull_request.id
        {
            return Err(WorkspaceError::Invalid("workspace identity is immutable".into()).into());
        }
        if workspace.attempt.is_some() {
            let owner = connection
                .query_row(
                    "SELECT workspace_id FROM workspace_publication_lock WHERE singleton=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if owner.as_ref().is_some_and(|id| id != &workspace.id) {
                return Err(WorkspaceError::Invalid(
                    "another repository publication is in progress or uncertain".into(),
                )
                .into());
            }
            connection.execute("INSERT OR IGNORE INTO workspace_publication_lock(singleton,workspace_id) VALUES (1,?1)",[&workspace.id])?;
        } else {
            connection.execute(
                "DELETE FROM workspace_publication_lock WHERE workspace_id=?1",
                [&workspace.id],
            )?;
        }
        workspace.generation = expected + 1;
        Self::persist_workspace_batch(connection, workspace)?;
        // Successful results, identities and generation are committed atomically.
        for result in &workspace.results {
            connection.execute("INSERT OR IGNORE INTO operation_results(batch_id,operation_id,sequence,operation,mappings,response)
                SELECT ?1,?2,COALESCE(MAX(sequence),0)+1,?3,?4,?5 FROM operation_results WHERE batch_id=?1",
                (&workspace.batch.id.0,&result.operation.id,serde_json::to_string(&result.operation)?,serde_json::to_string(&result.mappings)?,serde_json::to_string(&result.response)?))?;
            // Historical receipt mappings already belong to their original
            // snapshot. A refresh may give the same remote review its fetched
            // ReviewResourceId; replaying the conclusion operation ID here
            // would collide with that distinct, authoritative resource identity.
            if actual
                .results
                .iter()
                .any(|previous| previous.operation.id == result.operation.id)
            {
                continue;
            }
            for mapping in &result.mappings {
                let source_exists = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_sources WHERE review_id=?1)",
                    [&workspace.review.id.0],
                    |row| row.get::<_, bool>(0),
                )?;
                if source_exists {
                    if mapping.resource == "review" {
                        let fetched=connection.query_row("SELECT core_id FROM id_mappings WHERE review_id=?1 AND resource='review' AND remote_id=?2",(&workspace.review.id.0,&mapping.remote_id),|row|row.get::<_,String>(0)).optional()?;
                        if fetched.as_ref().is_some_and(|id| {
                            workspace.review.read.reviews.iter().any(|review| {
                                review
                                    .id
                                    .as_ref()
                                    .is_some_and(|review_id| &review_id.0 == id)
                            })
                        }) {
                            continue;
                        }
                    }
                    connection.execute("INSERT INTO id_mappings(review_id,resource,core_id,remote_id) VALUES (?1,?2,?3,?4) ON CONFLICT(review_id,resource,core_id) DO UPDATE SET remote_id=excluded.remote_id",
                        (&workspace.review.id.0,&mapping.resource,&mapping.core_id,&mapping.remote_id))?;
                }
            }
        }
        // Relational metadata can hydrate fields beyond the last saved state.
        // Changes must start at the stored generation, not that hydrated view.
        let previous: String = connection.query_row(
            "SELECT state FROM workspaces WHERE workspace_id=?1",
            [&workspace.id],
            |row| row.get(0),
        )?;
        let previous: Value = serde_json::from_str(&previous)?;
        let state = Self::compact_workspace(connection, workspace)?;
        connection.execute(
            "UPDATE workspaces SET generation=?2,state=?3 WHERE workspace_id=?1",
            (
                &workspace.id,
                workspace.generation as i64,
                serde_json::to_string(&state)?,
            ),
        )?;
        connection.execute("INSERT INTO workspace_history(workspace_id,generation,state,action) VALUES (?1,?2,?3,?4)",(&workspace.id,workspace.generation as i64,Self::state_delta(&previous, &state)?,serde_json::to_string(&Self::compact_action(connection, &state, action)?)?))?;
        Ok(())
    }

    pub fn update_workspace<F>(
        &mut self,
        id: &str,
        generation: u64,
        update: F,
    ) -> Result<crate::workspace::Workspace, StoreError>
    where
        F: FnOnce(&mut crate::workspace::Workspace) -> Result<(), crate::workspace::WorkspaceError>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, id)?
            .ok_or_else(|| crate::workspace::WorkspaceError::NotFound(id.into()))?;
        if workspace.generation != generation {
            return Err(crate::workspace::WorkspaceError::StaleGeneration {
                expected: generation,
                actual: workspace.generation,
            }
            .into());
        }
        update(&mut workspace)?;
        Self::persist_workspace(
            &transaction,
            generation,
            &mut workspace,
            &serde_json::json!({"action":"provider_update"}),
        )?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn save_workspace(
        &mut self,
        expected_generation: u64,
        workspace: &crate::workspace::Workspace,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = workspace.clone();
        Self::persist_workspace(
            &transaction,
            expected_generation,
            &mut workspace,
            &serde_json::json!({"action":"provider_update"}),
        )?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn mutate_workspace(
        &mut self,
        id: &str,
        generation: u64,
        action: crate::workspace::Action,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let result = self.apply_workspace_actions(id, generation, vec![action], false)?;
        match result.failure {
            Some((_, error)) => Err(error.into()),
            None => Ok(result.workspace),
        }
    }

    /// Apply local UI actions in order, committing the successful prefix when
    /// a later action fails. Publication journal actions use the provider APIs.
    pub fn mutate_workspace_actions(
        &mut self,
        id: &str,
        generation: u64,
        actions: Vec<crate::workspace::Action>,
    ) -> Result<WorkspaceActions, StoreError> {
        self.apply_workspace_actions(id, generation, actions, true)
    }

    fn apply_workspace_actions(
        &mut self,
        id: &str,
        generation: u64,
        actions: Vec<crate::workspace::Action>,
        local_only: bool,
    ) -> Result<WorkspaceActions, StoreError> {
        use crate::workspace::{Action, WorkspaceError};
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, id)?
            .ok_or_else(|| WorkspaceError::NotFound(id.into()))?;
        if workspace.generation != generation {
            return Err(WorkspaceError::StaleGeneration {
                expected: generation,
                actual: workspace.generation,
            }
            .into());
        }
        let mut applied = 0;
        let mut failure = None;
        'actions: for (index, action) in actions.into_iter().enumerate() {
            if local_only
                && !matches!(
                    &action,
                    Action::PrDetails { .. }
                        | Action::Start
                        | Action::Leave
                        | Action::Save { .. }
                        | Action::Delete { .. }
                        | Action::DeleteComments { .. }
                        | Action::Restore { .. }
                        | Action::Undo { .. }
                        | Action::Viewed { .. }
                        | Action::Reaction { .. }
                        | Action::Summary { .. }
                        | Action::CompareLatest
                        | Action::Relocate { .. }
                        | Action::Resolve { .. }
                )
            {
                failure = Some((
                    index,
                    WorkspaceError::Invalid(
                        "publication journal actions are not local UI actions".into(),
                    ),
                ));
                break;
            }
            if let Action::PrDetails {
                revision,
                title,
                body,
            } = &action
            {
                let updated = (|| -> Result<bool, StoreError> {
                    let mut object = Self::checked_pr(
                        &transaction,
                        &workspace.review.read.pull_request.id,
                        *revision,
                    )?;
                    if !matches!(object.source, PullRequestSource::LocalIntent { .. }) {
                        return Err(WorkspaceError::Invalid(
                            "remote PR details cannot be edited locally".into(),
                        )
                        .into());
                    }
                    let resolving: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM pr_resolutions WHERE pull_request_id=?1)",
                        [&object.id.0],
                        |row| row.get(0),
                    )?;
                    if resolving {
                        return Err(WorkspaceError::Invalid(
                            "PR creation is in progress or uncertain; reconcile before editing details".into(),
                        ).into());
                    }
                    if object.title == *title && object.body == *body {
                        return Ok(false);
                    }
                    object.title = title.clone();
                    object.body = body.clone();
                    object.revision += 1;
                    Self::write_pr(&transaction, &object)?;
                    Ok(true)
                })();
                match updated {
                    Ok(true) => {
                        let generation = workspace.generation;
                        Self::persist_workspace(
                            &transaction,
                            generation,
                            &mut workspace,
                            &serde_json::to_value(&action)?,
                        )?;
                    }
                    Ok(false) => {}
                    Err(StoreError::Workspace(error)) => {
                        failure = Some((index, error));
                        break;
                    }
                    Err(error) => return Err(error),
                }
                applied += 1;
                continue;
            }
            let expanded = match &action {
                Action::DeleteComments { comments } => workspace.delete_actions(comments.clone()),
                Action::Undo { comments } => workspace.undo_actions(comments.clone()),
                _ => Ok(vec![action.clone()]),
            };
            let expanded = match expanded {
                Ok(expanded) => expanded,
                Err(error) => {
                    failure = Some((index, error));
                    break;
                }
            };
            for child in expanded {
                let previous = workspace.clone();
                if let Err(error) = workspace.apply(child.clone()) {
                    failure = Some((index, error));
                    break 'actions;
                }
                if workspace != previous {
                    let recorded = match (&action, &child) {
                        (Action::DeleteComments { .. }, Action::Delete { comment }) => {
                            Action::DeleteComments {
                                comments: vec![comment.clone()],
                            }
                        }
                        (
                            Action::Undo { .. },
                            Action::Delete { comment }
                            | Action::Restore { comment }
                            | Action::Reaction { comment, .. },
                        ) => Action::Undo {
                            comments: vec![comment.clone()],
                        },
                        _ => child,
                    };
                    Self::persist_workspace(
                        &transaction,
                        previous.generation,
                        &mut workspace,
                        &serde_json::to_value(&recorded)?,
                    )?;
                }
            }
            applied += 1;
        }
        transaction.commit()?;
        Ok(WorkspaceActions {
            workspace,
            applied,
            failure,
        })
    }

    /// Merge a provider result into the latest generation without overwriting
    /// unrelated saves made while the network request was in flight.
    pub fn mutate_publication(
        &mut self,
        id: &str,
        attempt_id: &str,
        action: crate::workspace::Action,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, id)?
            .ok_or_else(|| crate::workspace::WorkspaceError::NotFound(id.into()))?;
        if workspace
            .attempt
            .as_ref()
            .is_none_or(|attempt| attempt.id != attempt_id)
        {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "publication attempt changed before recording its result".into(),
            )
            .into());
        }
        let generation = workspace.generation;
        workspace.apply(action.clone())?;
        Self::persist_workspace(
            &transaction,
            generation,
            &mut workspace,
            &serde_json::to_value(action)?,
        )?;
        transaction.commit()?;
        Ok(workspace)
    }

    pub fn workspace_view(&self, id: &str) -> Result<crate::workspace::View, StoreError> {
        let workspace = self
            .workspace(id)?
            .ok_or_else(|| crate::workspace::WorkspaceError::NotFound(id.into()))?;
        self.view_workspace(&workspace)
    }

    /// Render an already loaded workspace without hydrating it a second time.
    pub fn view_workspace(
        &self,
        workspace: &crate::workspace::Workspace,
    ) -> Result<crate::workspace::View, StoreError> {
        let mut view = workspace.view()?;
        view.pr =
            Self::read_pull_request(&self.connection, &workspace.review.read.pull_request.id)?;
        view.publications = self
            .publication_history(workspace)?
            .into_iter()
            .map(|detail| detail.summary)
            .collect();
        Ok(view)
    }

    /// Read only publication lifecycle fields. Routine local saves do not
    /// require decoding historical review snapshots or unrelated provider fields.
    pub fn publication_history(
        &self,
        workspace: &crate::workspace::Workspace,
    ) -> Result<Vec<crate::sessions::PublicationDetail>, StoreError> {
        let history = Self::publication_states(&self.connection, &workspace.id)?
            .into_iter()
            .filter(|row| {
                matches!(
                    row.action["action"].as_str(),
                    Some(
                        "open"
                            | "refresh"
                            | "provider_update"
                            | "begin_publication"
                            | "record_result"
                            | "finish_publication"
                            | "suspend_publication"
                            | "resume_publication"
                    )
                )
            })
            .map(|row| {
                Ok::<_, StoreError>(crate::sessions::PublicationHistoryEntry {
                    generation: row.generation,
                    attempt: serde_json::from_value(row.state["attempt"].clone())?,
                    suspended_attempts: serde_json::from_value(
                        row.state
                            .get("suspended_attempts")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!([])),
                    )?,
                    last_error: serde_json::from_value(row.state["last_error"].clone())?,
                    result: row
                        .action
                        .get("result")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()?,
                    action: row.action,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::sessions::publication_history_details(
            history, workspace,
        ))
    }

    pub fn workspace_log(&self, id: &str) -> Result<Vec<Value>, StoreError> {
        let id = Self::canonical_batch(&self.connection, &BatchId(id.into()))?.0;
        Ok(Self::history_states(&self.connection, &id, None)?
            .into_iter()
            .map(|row| serde_json::json!({"generation":row.generation,"action":row.action}))
            .collect())
    }

    pub fn refresh_workspace(
        &mut self,
        id: &str,
        generation: u64,
        review: &ReviewId,
    ) -> Result<crate::workspace::Workspace, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = Self::read_workspace(&transaction, id)?
            .ok_or_else(|| crate::workspace::WorkspaceError::NotFound(id.into()))?;
        if workspace.generation != generation {
            return Err(crate::workspace::WorkspaceError::StaleGeneration {
                expected: generation,
                actual: workspace.generation,
            }
            .into());
        }
        let mut review =
            Self::read_review(&transaction, review)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        // Remote identities connect new snapshots to fetched and published local IDs.
        let mappings = |review: &ReviewId| -> Result<Vec<IdMapping>, StoreError> {
            let mut statement = transaction
                .prepare("SELECT resource,core_id,remote_id FROM id_mappings WHERE review_id=?1")?;
            Ok(statement
                .query_map([&review.0], |row| {
                    Ok(IdMapping {
                        resource: row.get(0)?,
                        core_id: row.get(1)?,
                        remote_id: row.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        };
        let mut previous = mappings(&workspace.review.id)?;
        let preparations = std::iter::once(&workspace.preparation)
            .chain(
                workspace
                    .suspended_attempts
                    .iter()
                    .filter(|attempt| attempt.destination == crate::workspace::Destination::Review)
                    .map(|attempt| &attempt.provider),
            )
            .collect::<Vec<_>>();
        for preparation in &preparations {
            if let Some(prepared) = preparation.get("prepared").and_then(Value::as_array) {
                for result in prepared {
                    if let Ok(result) = serde_json::from_value::<OperationResult>(result.clone()) {
                        previous.extend(result.mappings);
                    }
                }
            }
        }
        let incoming = mappings(&review.id)?;
        let private_reviews = preparations
            .iter()
            .filter(|preparation| {
                preparation.get("submitted").and_then(Value::as_bool) != Some(true)
            })
            .filter_map(|preparation| preparation.pointer("/pending/id").and_then(Value::as_u64))
            .collect::<std::collections::BTreeSet<_>>();
        let private_ids = incoming
            .iter()
            .filter(|mapping| {
                private_reviews
                    .iter()
                    .any(|id| mapping.resource == format!("review_member:{id}"))
            })
            .map(|mapping| mapping.core_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        review
            .read
            .comments
            .retain(|comment| !private_ids.contains(&comment.content.id.0));
        review
            .read
            .reactions
            .retain(|reaction| !private_ids.contains(&reaction.comment.0));
        let old = previous
            .iter()
            .map(|m| ((m.resource.clone(), m.remote_id.clone()), m.core_id.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let aliases = incoming
            .iter()
            .filter(|mapping| {
                matches!(
                    mapping.resource.as_str(),
                    "review_comment" | "issue_comment" | "review_summary"
                )
            })
            .filter_map(|m| {
                old.get(&(m.resource.clone(), m.remote_id.clone()))
                    .map(|id| (m.core_id.clone(), id.clone()))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for comment in &mut review.read.comments {
            if let Some(id) = aliases.get(&comment.content.id.0) {
                comment.content.id = crate::model::CommentId(id.clone());
            }
            if let crate::model::CommentTarget::Reply(parent) = &mut comment.target
                && let Some(id) = aliases.get(&parent.0)
            {
                parent.0 = id.clone();
            }
        }
        for reaction in &mut review.read.reactions {
            if let Some(id) = aliases.get(&reaction.comment.0) {
                reaction.comment.0 = id.clone();
            }
        }
        if !aliases.is_empty()
            || !private_ids.is_empty()
            || previous.iter().any(|mapping| !incoming.contains(mapping))
        {
            // Keep imported evidence intact; reuse equivalent canonical evidence
            // only when reviewer, snapshot and all provider identities agree.
            let incoming_id = review.id.clone();
            let mut backend = Self::read_backend(&transaction, &incoming_id)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            backend.mappings.clear();
            for mapping in incoming.iter().chain(previous.iter()) {
                let core_id = aliases.get(&mapping.core_id).unwrap_or(&mapping.core_id);
                if !backend.mappings.iter().any(|saved| {
                    saved.resource == mapping.resource
                        && (saved.core_id == *core_id || saved.remote_id == mapping.remote_id)
                }) {
                    backend.mappings.push(IdMapping {
                        resource: mapping.resource.clone(),
                        core_id: core_id.clone(),
                        remote_id: mapping.remote_id.clone(),
                    });
                }
            }
            if let Some(id) =
                Self::matching_review(&transaction, &review.reviewer, &review.read, Some(&backend))?
            {
                review.id = id;
            } else {
                review.id = ReviewId(uuid::Uuid::new_v4().to_string());
                Self::insert_review(&transaction, &review.id, &review.reviewer, &review.read)?;
                transaction.execute(
                    "INSERT INTO review_sources(review_id,backend,locator) VALUES (?1,?2,?3)",
                    (
                        &review.id.0,
                        &backend.source.backend,
                        &backend.source.locator,
                    ),
                )?;
                for mapping in &backend.mappings {
                    transaction.execute("INSERT INTO id_mappings(review_id,resource,core_id,remote_id) VALUES (?1,?2,?3,?4)",(&review.id.0,&mapping.resource,&mapping.core_id,&mapping.remote_id))?;
                }
            }
        }
        workspace.refresh(review)?;
        Self::load_summary_ids(&transaction, &mut workspace)?;
        Self::persist_workspace(
            &transaction,
            generation,
            &mut workspace,
            &serde_json::json!({"action":"refresh"}),
        )?;
        transaction.commit()?;
        Ok(workspace)
    }
}
