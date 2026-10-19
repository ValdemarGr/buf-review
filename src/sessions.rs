//! Batch summaries and publication history, shared by editor clients.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    core::Operation,
    model::Event,
    store::{BatchHistoryEntry, OperationResult},
    workspace::{Destination, PublicationAttempt, Workspace, WorkspaceError},
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub comments: usize,
    pub edits: usize,
    pub deletions: usize,
    pub reactions: usize,
    pub files: usize,
    pub conclusions: usize,
    pub total: usize,
}

impl<'a> FromIterator<&'a Operation> for Counts {
    fn from_iter<T: IntoIterator<Item = &'a Operation>>(operations: T) -> Self {
        operations
            .into_iter()
            .fold(Self::default(), |mut counts, op| {
                match &op.change {
                    Event::CreateComment { .. } => counts.comments += 1,
                    Event::EditComment { .. } => counts.edits += 1,
                    Event::DeleteComment { .. } => counts.deletions += 1,
                    Event::SetCommentReaction { .. } => counts.reactions += 1,
                    Event::SetFileViewed { .. } => counts.files += 1,
                    Event::SetReviewConclusion { .. } | Event::DismissReview { .. } => {
                        counts.conclusions += 1;
                    }
                }
                counts.total += 1;
                counts
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStatus {
    Draft,
    Publishing,
    Published,
    Partial,
    Failed,
    Uncertain,
    Submitted,
    SubmittedProgressPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchStats {
    pub pending: Counts,
    pub published: Counts,
    pub status: PublicationStatus,
    pub unfinished: bool,
    pub conflicts: usize,
}

impl Workspace {
    pub fn stats(&self, pending: &[Operation]) -> BatchStats {
        let mut seen = BTreeSet::new();
        let published: Counts = self
            .results
            .iter()
            .filter(|result| seen.insert(&result.operation.id))
            .map(|result| &result.operation)
            .collect();
        let preparation = self
            .preparation
            .get("pending")
            .is_some_and(|pending| !pending.is_null())
            && self.preparation.get("submitted").and_then(Value::as_bool) != Some(true);
        let unfinished = published.total == 0
            || !pending.is_empty()
            || self.wip.is_some()
            || !self.conflicts.is_empty()
            || !self.rejected_anchors.is_empty()
            || self.attempt.is_some()
            || !self.suspended_attempts.is_empty()
            || preparation;
        let status = if self.attempt.is_some() {
            PublicationStatus::Publishing
        } else if !self.suspended_attempts.is_empty() {
            PublicationStatus::Uncertain
        } else if published.total == 0 {
            PublicationStatus::Draft
        } else if unfinished {
            PublicationStatus::Partial
        } else {
            PublicationStatus::Published
        };
        BatchStats {
            pending: pending.iter().collect(),
            published,
            status,
            unfinished,
            conflicts: self.conflicts.len(),
        }
    }

    pub fn is_unfinished(&self) -> Result<bool, WorkspaceError> {
        Ok(self.stats(&self.pending()?.operations).unfinished)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationSummary {
    pub id: String,
    pub generation: u64,
    pub destination: Option<Destination>,
    pub status: PublicationStatus,
    pub planned: Counts,
    pub published: Counts,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishedOperation {
    pub operation: Operation,
    pub published: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicationDetail {
    pub summary: PublicationSummary,
    pub operations: Vec<PublishedOperation>,
}

/// Publication-only fields from a historical workspace snapshot.
#[derive(Debug, Clone, Deserialize)]
pub struct PublicationHistoryEntry {
    pub generation: u64,
    pub attempt: Option<PublicationAttempt>,
    pub suspended_attempts: Vec<PublicationAttempt>,
    pub last_error: Option<String>,
    pub action: Value,
    pub result: Option<OperationResult>,
}

struct Attempt {
    id: String,
    generation: u64,
    destination: Option<Destination>,
    operations: Vec<Operation>,
    results: BTreeMap<String, OperationResult>,
    finished: Option<(bool, bool)>,
    error: Option<String>,
}

impl Attempt {
    fn new(attempt: &PublicationAttempt, generation: u64) -> Self {
        Self {
            id: attempt.id.clone(),
            generation,
            destination: Some(attempt.destination),
            operations: attempt.operations.clone(),
            results: BTreeMap::new(),
            finished: None,
            error: None,
        }
    }

    fn detail(self, current: &Workspace) -> PublicationDetail {
        let planned: Counts = self.operations.iter().collect();
        let published: Counts = self
            .results
            .values()
            .map(|result| &result.operation)
            .collect();
        let submitted = self
            .results
            .values()
            .any(|result| matches!(result.operation.change, Event::SetReviewConclusion { .. }));
        let status = if current
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.id == self.id)
        {
            PublicationStatus::Publishing
        } else if current
            .suspended_attempts
            .iter()
            .any(|attempt| attempt.id == self.id)
        {
            PublicationStatus::Uncertain
        } else if submitted && published.total < planned.total {
            PublicationStatus::SubmittedProgressPending
        } else if submitted {
            PublicationStatus::Submitted
        } else if planned.total > 0 && published.total == planned.total {
            PublicationStatus::Published
        } else if self.finished.is_some_and(|(_, uncertain)| uncertain) {
            PublicationStatus::Uncertain
        } else if published.total > 0 {
            PublicationStatus::Partial
        } else if self.finished.is_some_and(|(success, _)| !success) {
            PublicationStatus::Failed
        } else {
            PublicationStatus::Uncertain
        };
        let operations = self
            .operations
            .into_iter()
            .map(|operation| {
                let result = self.results.get(&operation.id);
                PublishedOperation {
                    published: result.is_some(),
                    response: result.map(|result| result.response.clone()),
                    operation,
                }
            })
            .collect();
        PublicationDetail {
            summary: PublicationSummary {
                id: self.id,
                generation: self.generation,
                destination: self.destination,
                status,
                planned,
                published,
                error: self.error,
            },
            operations,
        }
    }
}

/// Associate receipts with the attempt that recorded them, including retries
/// of the same operation in a later attempt. Private preparation is not a receipt.
pub fn publication_details(
    history: &[BatchHistoryEntry],
    current: &Workspace,
) -> Vec<PublicationDetail> {
    let entries = history.iter().map(|entry| PublicationHistoryEntry {
        generation: entry.generation,
        attempt: entry.state.attempt.clone(),
        suspended_attempts: entry.state.suspended_attempts.clone(),
        last_error: entry.state.last_error.clone(),
        result: entry
            .action
            .pointer("/result/operation/id")
            .and_then(Value::as_str)
            .and_then(|id| {
                entry
                    .state
                    .results
                    .iter()
                    .find(|result| result.operation.id == id)
            })
            .cloned(),
        action: entry.action.clone(),
    });
    publication_history_details(entries, current)
}

pub fn publication_history_details(
    history: impl IntoIterator<Item = PublicationHistoryEntry>,
    current: &Workspace,
) -> Vec<PublicationDetail> {
    let mut attempts = BTreeMap::<String, Attempt>::new();
    let mut active = None::<String>;
    for entry in history {
        for attempt in entry.attempt.iter().chain(&entry.suspended_attempts) {
            attempts
                .entry(attempt.id.clone())
                .or_insert_with(|| Attempt::new(attempt, entry.generation));
        }
        if entry.action["action"] == "record_result"
            && let Some(id) = entry
                .action
                .pointer("/result/operation/id")
                .and_then(Value::as_str)
            && let Some(attempt) = entry
                .attempt
                .as_ref()
                .and_then(|attempt| attempts.get_mut(&attempt.id))
            && let Some(result) = entry
                .result
                .as_ref()
                .filter(|result| result.operation.id == id)
        {
            attempt.results.insert(id.to_owned(), result.clone());
        }
        if (entry.action["action"] == "finish_publication"
            || entry.action["action"] == "suspend_publication")
            && let Some(attempt) = active.as_ref().and_then(|id| attempts.get_mut(id))
        {
            let success = entry.action["success"].as_bool().unwrap_or(false);
            let uncertain = entry.action["uncertain"]
                .as_bool()
                .unwrap_or(entry.action["action"] == "suspend_publication");
            attempt.finished = Some((success, uncertain));
            attempt.error = entry.action["error"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| entry.last_error.clone());
        }
        active = entry.attempt.as_ref().map(|attempt| attempt.id.clone());
    }
    for attempt in current.attempt.iter().chain(&current.suspended_attempts) {
        attempts
            .entry(attempt.id.clone())
            .or_insert_with(|| Attempt::new(attempt, current.generation));
    }
    let recorded = attempts
        .values()
        .flat_map(|attempt| attempt.results.keys())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let ungrouped = current
        .results
        .iter()
        .filter(|result| {
            !recorded.contains(&result.operation.id) && seen.insert(&result.operation.id)
        })
        .collect::<Vec<_>>();
    let mut details = Vec::new();
    if !ungrouped.is_empty() {
        let counts: Counts = ungrouped.iter().map(|result| &result.operation).collect();
        details.push(PublicationDetail {
            summary: PublicationSummary {
                id: "recorded".into(),
                generation: 0,
                destination: None,
                status: PublicationStatus::Published,
                planned: counts.clone(),
                published: counts,
                error: None,
            },
            operations: ungrouped
                .into_iter()
                .map(|result| PublishedOperation {
                    operation: result.operation.clone(),
                    published: true,
                    response: Some(result.response.clone()),
                })
                .collect(),
        });
    }
    details.extend(
        attempts
            .into_values()
            .map(|attempt| attempt.detail(current)),
    );
    details.sort_by(|left, right| {
        right
            .summary
            .generation
            .cmp(&left.summary.generation)
            .then_with(|| left.summary.id.cmp(&right.summary.id))
    });
    details
}

pub fn publications(history: &[BatchHistoryEntry], current: &Workspace) -> Vec<PublicationSummary> {
    publication_details(history, current)
        .into_iter()
        .map(|detail| detail.summary)
        .collect()
}
