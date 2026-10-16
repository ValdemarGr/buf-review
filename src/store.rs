use std::{fs, path::Path};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::core::Operation;
use crate::model::{Batch, BatchId, Event, ReadState, Review, ReviewId, User, WriteState};

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
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("batch does not exist: {0:?}")]
    BatchNotFound(BatchId),
}

/// SQLite persistence for fetched snapshots and local event logs.
/// Event interpretation belongs to the core; backend requests belong to adapters.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens `.review/state.sqlite3` under the supplied project root.
    pub fn open(project_root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let directory = project_root.as_ref().join(".review");
        fs::create_dir_all(&directory)?;
        let mut connection = Connection::open(directory.join("state.sqlite3"))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let legacy = transaction.column_exists(Some("main"), "write_events", "review_id")?;
        if legacy {
            transaction.execute_batch("ALTER TABLE write_events RENAME TO legacy_write_events")?;
        }
        transaction.execute_batch(include_str!("schema.sql"))?;
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
        transaction.commit()?;
        Ok(Self { connection })
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
        transaction.commit()?;
        Ok(())
    }

    fn insert_review(
        transaction: &Transaction<'_>,
        id: &ReviewId,
        reviewer: &User,
        read: &ReadState,
    ) -> Result<(), StoreError> {
        let reviewer = serde_json::to_string(reviewer)?;
        let snapshot = serde_json::to_string(read)?;
        transaction.execute(
            "INSERT INTO reviews (review_id, reviewer) VALUES (?1, ?2)",
            (&id.0, reviewer),
        )?;
        transaction.execute(
            "INSERT INTO read_states (review_id, snapshot) VALUES (?1, ?2)",
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
        let transaction = self.connection.transaction()?;
        Self::insert_review(&transaction, id, reviewer, read)?;
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
        Ok(())
    }

    pub fn review_backend(&mut self, id: &ReviewId) -> Result<Option<ReviewBackend>, StoreError> {
        let transaction = self.connection.transaction()?;
        let source = transaction
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
        let backend = source
            .map(|source| {
                let mut statement = transaction.prepare(
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
            .transpose()?;
        transaction.commit()?;
        Ok(backend)
    }

    pub fn get_review(&self, id: &ReviewId) -> Result<Option<Review>, StoreError> {
        let metadata = self
            .connection
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
        self.connection.execute(
            "INSERT INTO batches (batch_id, review_id) VALUES (?1, ?2)",
            (&id.0, &review.0),
        )?;
        Ok(())
    }

    /// Reads the review link and ordered log from one consistent snapshot.
    pub fn get_batch(&mut self, id: &BatchId) -> Result<Option<Batch>, StoreError> {
        let transaction = self.connection.transaction()?;
        let review = transaction
            .query_row(
                "SELECT review_id FROM batches WHERE batch_id = ?1",
                [&id.0],
                |row| row.get::<_, String>(0).map(ReviewId),
            )
            .optional()?;
        let batch = review
            .map(|review| {
                let mut statement = transaction.prepare(
                    "SELECT event FROM write_events WHERE batch_id = ?1 ORDER BY sequence",
                )?;
                let events = statement
                    .query_and_then([&id.0], |row| {
                        let event: String = row.get(0)?;
                        Ok::<Event, StoreError>(serde_json::from_str(&event)?)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<Batch, StoreError>(Batch {
                    id: id.clone(),
                    review,
                    write: WriteState { events },
                })
            })
            .transpose()?;
        transaction.commit()?;
        Ok(batch)
    }

    /// Appends without replacing or collapsing earlier events. Old batches stay
    /// editable; sequence allocation and insertion share a single write.
    pub fn append_event(&mut self, id: &BatchId, event: &Event) -> Result<(), StoreError> {
        let event = serde_json::to_string(event)?;
        let inserted = self.connection.execute(
            "INSERT INTO write_events (batch_id, sequence, event)
             SELECT batch_id,
                    COALESCE((SELECT MAX(sequence) FROM write_events WHERE batch_id = ?1), 0) + 1,
                    ?2
             FROM batches WHERE batch_id = ?1",
            (&id.0, event),
        )?;
        if inserted == 0 {
            return Err(StoreError::BatchNotFound(id.clone()));
        }
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
            .prepare("SELECT batch_id FROM batches WHERE review_id = ?1 ORDER BY batch_id")?;
        let ids = statement
            .query_map([&review.0], |row| row.get(0).map(BatchId))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    /// Reads successful writes in execution order so the core can reconstruct
    /// delivered state without asking a provider to interpret local events.
    pub fn operation_results(
        &mut self,
        batch: &BatchId,
    ) -> Result<Vec<OperationResult>, StoreError> {
        let transaction = self.connection.transaction()?;
        let exists = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM batches WHERE batch_id = ?1)",
            [&batch.0],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(StoreError::BatchNotFound(batch.clone()));
        }
        let results = {
            let mut statement = transaction.prepare(
                "SELECT operation, mappings, response FROM operation_results
                 WHERE batch_id = ?1 ORDER BY sequence",
            )?;
            statement
                .query_and_then([&batch.0], |row| {
                    let operation: String = row.get(0)?;
                    let mappings: String = row.get(1)?;
                    let response: String = row.get(2)?;
                    Ok::<OperationResult, StoreError>(OperationResult {
                        operation: serde_json::from_str(&operation)?,
                        mappings: serde_json::from_str(&mappings)?,
                        response: serde_json::from_str(&response)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
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
        let review = transaction
            .query_row(
                "SELECT review_id FROM batches WHERE batch_id = ?1",
                [&batch.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::BatchNotFound(batch.clone()))?;
        transaction.execute(
            "INSERT INTO operation_results
             (batch_id, operation_id, sequence, operation, mappings, response)
             SELECT ?1, ?2, COALESCE(MAX(sequence), 0) + 1, ?3, ?4, ?5
             FROM operation_results WHERE batch_id = ?1",
            (
                &batch.0,
                &operation.id,
                serde_json::to_string(operation)?,
                serde_json::to_string(mappings)?,
                serde_json::to_string(response)?,
            ),
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO id_mappings (review_id, resource, core_id, remote_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (review_id, resource, core_id)
                 DO UPDATE SET remote_id = excluded.remote_id",
            )?;
            for mapping in mappings {
                statement.execute((
                    &review,
                    &mapping.resource,
                    &mapping.core_id,
                    &mapping.remote_id,
                ))?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}
