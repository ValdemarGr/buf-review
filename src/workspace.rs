//! Saved review workspaces compose immutable snapshots with local intent.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    core::{self, CommitState, CoreError, Operation, ProjectedComment},
    model::*,
    store::OperationResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    Standalone,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wip {
    pub id: String,
    pub active: bool,
    pub conclusion: Option<ReviewConclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    Changed,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub comment: CommentId,
    pub previous: ReadComment,
    pub remote: Option<ReadComment>,
    pub kind: ConflictKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Local,
    Remote,
    Edit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationAttempt {
    pub id: String,
    pub wip: Option<String>,
    pub destination: Destination,
    pub operations: Vec<Operation>,
    pub uncertain: bool,
    pub provider: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(skip)]
    pub project_root: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub worktree_context: bool,
    #[serde(skip)]
    pub viewed_versions: Option<BTreeSet<(String, String)>>,
    pub snapshot_result_count: usize,
    pub id: String,
    pub locator: Option<String>,
    pub generation: u64,
    pub review: Review,
    pub comparison: DiffRevision,
    pub comparison_files: Vec<ChangedFile>,
    pub batch: Batch,
    pub destinations: BTreeMap<CommentId, Destination>,
    pub wip: Option<Wip>,
    pub conflicts: BTreeMap<CommentId, Conflict>,
    pub attempt: Option<PublicationAttempt>,
    pub suspended_attempts: Vec<PublicationAttempt>,
    pub preparation: serde_json::Value,
    pub last_error: Option<String>,
    pub saved_comment: Option<CommentId>,
    pub summary_comments: BTreeSet<CommentId>,
    pub results: Vec<OperationResult>,
    pub chronology: BTreeMap<CommentId, String>,
    pub rejected_anchors: BTreeMap<CommentId, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentView {
    pub comment: ProjectedComment,
    pub fetched: bool,
    pub draft: bool,
    pub done: bool,
    pub destination: Option<Destination>,
    pub anchor_error: Option<String>,
    pub conflict: bool,
    pub summary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<PullRequestObject>,
    pub stats: crate::sessions::BatchStats,
    pub publications: Vec<crate::sessions::PublicationSummary>,
    pub id: String,
    pub locator: Option<String>,
    pub generation: u64,
    pub review: Review,
    pub comparison: DiffRevision,
    pub wip: Option<Wip>,
    pub comments: Vec<CommentView>,
    pub files: Vec<ChangedFile>,
    pub pending: Vec<Operation>,
    pub todo: Vec<CommentId>,
    pub done: Vec<CommentId>,
    pub conflicts: Vec<Conflict>,
    pub last_error: Option<String>,
    pub new_commits: bool,
    pub unpulled_commits: Vec<crate::diff::CommitSummary>,
    pub comparison_error: Option<String>,
    pub saved_comment: Option<CommentId>,
    pub attempt: Option<PublicationAttempt>,
    pub suspended_attempts: Vec<PublicationAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    PrDetails {
        revision: u64,
        title: String,
        body: String,
    },
    Start,
    Leave,
    Save {
        comment: Option<CommentId>,
        target: Option<CommentTarget>,
        body: String,
        destination: Option<Destination>,
    },
    Delete {
        comment: CommentId,
    },
    DeleteComments {
        comments: Vec<CommentId>,
    },
    Restore {
        comment: CommentId,
    },
    Undo {
        comments: Vec<CommentId>,
    },
    Viewed {
        path: String,
        viewed: bool,
    },
    Reaction {
        comment: CommentId,
        present: bool,
    },
    Summary {
        conclusion: ReviewConclusion,
    },
    PublishReview {
        conclusion: Option<ReviewConclusion>,
    },
    #[serde(rename = "move")]
    MoveToReview {
        comments: Vec<CommentId>,
    },
    CompareLatest,
    Relocate {
        comment: CommentId,
        location: DiffLocation,
    },
    Resolve {
        comment: CommentId,
        resolution: Resolution,
        body: Option<String>,
    },
    ResumePublication {
        attempt_id: String,
    },
    SuspendPublication,
    BeginPublication {
        destination: Destination,
        operations: Vec<Operation>,
    },
    RecordResult {
        result: OperationResult,
    },
    SetAttemptProvider {
        provider: serde_json::Value,
    },
    FinishPublication {
        success: bool,
        uncertain: bool,
        error: Option<String>,
    },
    RefreshError {
        error: String,
    },
    RejectAnchor {
        comment: CommentId,
        message: String,
    },
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("workspace does not exist: {0}")]
    NotFound(String),
    #[error(
        "workspace changed in another session (expected generation {expected}, found {actual}); reload before saving"
    )]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("{0}")]
    Invalid(String),
}

struct ReceiptIndices {
    operations: BTreeMap<String, usize>,
    acknowledged: BTreeSet<String>,
}

fn parent(comment: &ProjectedComment) -> Option<&CommentId> {
    match comment {
        ProjectedComment::Read(ReadComment {
            target: CommentTarget::Reply(id),
            ..
        })
        | ProjectedComment::Draft(DraftComment {
            target: CommentTarget::Reply(id),
            ..
        }) => Some(id),
        _ => None,
    }
}

impl Workspace {
    pub fn new(review: Review) -> Self {
        let id = Uuid::new_v4().to_string();
        Self {
            project_root: None,
            worktree_context: false,
            viewed_versions: None,
            snapshot_result_count: 0,
            comparison: review.read.pull_request.revision.clone(),
            comparison_files: review.read.files.clone(),
            batch: Batch {
                id: BatchId(id.clone()),
                revision: Default::default(),
                review: review.id.clone(),
                write: WriteState::default(),
            },
            id,
            locator: None,
            generation: 0,
            review,
            destinations: BTreeMap::new(),
            wip: None,
            conflicts: BTreeMap::new(),
            attempt: None,
            suspended_attempts: vec![],
            preparation: serde_json::Value::Null,
            last_error: None,
            saved_comment: None,
            summary_comments: BTreeSet::new(),
            results: vec![],
            chronology: BTreeMap::new(),
            rejected_anchors: BTreeMap::new(),
        }
    }

    pub(crate) fn use_worktree(&mut self) -> Result<(), WorkspaceError> {
        let project = self
            .project_root
            .as_deref()
            .ok_or_else(|| WorkspaceError::Invalid("workspace has no Git working tree".into()))?;
        let revision = DiffRevision {
            base: self.review.read.pull_request.revision.base.clone(),
            head: crate::diff::resolve(project, "HEAD")
                .map_err(|error| WorkspaceError::Invalid(error.to_string()))?,
        };
        let files = crate::diff::files(project, &revision)
            .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
        self.comparison = revision;
        self.comparison_files = files;
        self.worktree_context = true;
        Ok(())
    }

    fn committed_line_source(&self, location: &DiffLocation) -> Result<(), WorkspaceError> {
        if self.worktree_context && location.position.range.is_some() {
            let project = self.project_root.as_deref().ok_or_else(|| {
                WorkspaceError::Invalid("workspace has no Git working tree".into())
            })?;
            if crate::diff::resolve(project, "HEAD")
                .map_err(|error| WorkspaceError::Invalid(error.to_string()))?
                != location.revision.head
            {
                return Err(WorkspaceError::Invalid(
                    "HEAD changed; reopen the diff before commenting".into(),
                ));
            }
            if location.position.range.as_ref().is_some_and(|range| {
                range.start.side == DiffSide::Head || range.end.side == DiffSide::Head
            }) && crate::diff::local_changes(project, &location.position.path)
                .map_err(|error| WorkspaceError::Invalid(error.to_string()))?
            {
                return Err(WorkspaceError::Invalid(
                    "local changes cannot anchor line comments; commit and reopen the diff".into(),
                ));
            }
        }
        Ok(())
    }

    /// Missing conflicted resources are retained only in the local projection.
    fn projection_review(&self) -> Review {
        let mut review = self.review.clone();
        for reaction in &mut review.read.reactions {
            if is_thumbs_up(&reaction.reaction) {
                reaction.reaction = "👍".into();
            }
        }
        let current_files = self
            .review
            .read
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.viewed))
            .collect::<BTreeMap<_, _>>();
        review.read.files = self
            .comparison_files
            .iter()
            .map(|file| {
                let mut file = file.clone();
                if let Some(viewed) = current_files.get(file.path.as_str()) {
                    file.viewed = *viewed;
                }
                file
            })
            .collect();
        for conflict in self.conflicts.values() {
            if !review
                .read
                .comments
                .iter()
                .any(|c| c.content.id == conflict.comment)
            {
                review.read.comments.push(conflict.previous.clone());
            }
        }
        review
    }

    /// Compute immutable Git facts at the request boundary, outside projection.
    pub(crate) fn prepare_viewed_versions(&mut self) -> Result<(), WorkspaceError> {
        let acknowledged = self
            .results
            .iter()
            .map(|result| result.operation.id.clone())
            .collect::<BTreeSet<_>>();
        let latest_ack = self
            .batch
            .write
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, record)| match &record.event {
                Event::SetFileViewed { path, .. } if acknowledged.contains(&record.id) => {
                    Some((path.as_str(), index))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let paths = self
            .comparison_files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        let mut origins = BTreeMap::<String, BTreeSet<String>>::new();
        for (index, record) in self.batch.write.events.iter().enumerate() {
            let Event::SetFileViewed { path, commit, .. } = &record.event else {
                continue;
            };
            if acknowledged.contains(&record.id)
                || latest_ack
                    .get(path.as_str())
                    .is_some_and(|ack| index <= *ack)
                || !paths.contains(path.as_str())
            {
                continue;
            }
            let Some(commit) = commit else {
                return Err(WorkspaceError::Invalid(
                    "viewed mark commit unavailable".into(),
                ));
            };
            if commit != &self.comparison.head {
                origins
                    .entry(commit.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
        let mut versions = BTreeSet::new();
        for (commit, paths) in origins {
            let project = self.project_root.as_deref().ok_or_else(|| {
                WorkspaceError::Invalid("Git history unavailable for local viewed changes".into())
            })?;
            let paths = paths.into_iter().collect::<Vec<_>>();
            let unchanged =
                crate::diff::unchanged_paths(project, &commit, &self.comparison.head, &paths)
                    .map_err(|error| WorkspaceError::Invalid(error.to_string()))?;
            versions.extend(unchanged.into_iter().map(|path| (commit.clone(), path)));
        }
        self.viewed_versions = Some(versions);
        Ok(())
    }

    /// Derive effective local changes once for presentation and publication.
    pub fn get_unpublished(&self) -> Result<(core::ProjectedState, CommitState), WorkspaceError> {
        let review = self.projection_review();
        let paths = review
            .read
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        let acknowledged = self
            .results
            .iter()
            .map(|result| result.operation.id.clone())
            .collect::<BTreeSet<_>>();
        let latest_ack = self
            .batch
            .write
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, record)| match &record.event {
                Event::SetFileViewed { path, .. } if acknowledged.contains(&record.id) => {
                    Some((path.as_str(), index))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let eligible = self
            .batch
            .write
            .events
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                let Event::SetFileViewed { path, commit, .. } = &record.event else {
                    return None;
                };
                if acknowledged.contains(&record.id) || !paths.contains(path.as_str()) {
                    return None;
                }
                if latest_ack
                    .get(path.as_str())
                    .is_some_and(|ack| index <= *ack)
                {
                    return Some(Ok(record.id.clone()));
                }
                match commit {
                    Some(commit) if commit == &self.comparison.head => Ok(Some(record.id.clone())),
                    Some(commit) => self
                        .viewed_versions
                        .as_ref()
                        .map(|versions| {
                            Ok(versions
                                .contains(&(commit.clone(), path.clone()))
                                .then(|| record.id.clone()))
                        })
                        .unwrap_or_else(|| {
                            Err(WorkspaceError::Invalid(
                                "viewed mark version eligibility unavailable".into(),
                            ))
                        }),
                    None => Err(WorkspaceError::Invalid(
                        "viewed mark commit unavailable".into(),
                    )),
                }
                .transpose()
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let overlay = self
            .results
            .iter()
            .skip(self.snapshot_result_count)
            .map(|result| result.operation.clone())
            .collect::<Vec<_>>();
        let (projected, mut plan) =
            core::derive(&review, &self.batch, &acknowledged, &eligible, &overlay)?;
        if self
            .wip
            .as_ref()
            .and_then(|wip| wip.conclusion.as_ref())
            .is_none()
        {
            plan.operations
                .retain(|operation| !matches!(operation.change, Event::SetReviewConclusion { .. }));
        }
        Ok((projected, plan))
    }

    pub fn pending(&self) -> Result<CommitState, WorkspaceError> {
        self.get_unpublished().map(|(_, pending)| pending)
    }

    fn attempts(&self) -> impl Iterator<Item = &PublicationAttempt> {
        self.attempt.iter().chain(self.suspended_attempts.iter())
    }

    fn frozen_wip(&self) -> bool {
        self.attempts().any(|attempt| {
            (attempt.uncertain
                || self
                    .attempt
                    .as_ref()
                    .is_some_and(|active| active.id == attempt.id))
                && attempt.destination == Destination::Review
                && self
                    .wip
                    .as_ref()
                    .is_some_and(|w| Some(&w.id) == attempt.wip.as_ref())
        })
    }

    fn ensure_wip_mutable(&self) -> Result<(), WorkspaceError> {
        if self.frozen_wip() {
            return Err(WorkspaceError::Invalid("review submission is in progress or uncertain; reconcile before changing its saved contents".into()));
        }
        Ok(())
    }

    fn wip(&mut self) -> &mut Wip {
        self.wip.get_or_insert_with(|| Wip {
            id: Uuid::new_v4().to_string(),
            active: false,
            conclusion: None,
        })
    }

    pub fn projected(&self) -> Result<core::ProjectedState, WorkspaceError> {
        self.get_unpublished().map(|(projected, _)| projected)
    }

    fn find(&self, id: &CommentId) -> Result<ProjectedComment, WorkspaceError> {
        self.projected()?
            .comments
            .into_iter()
            .find(|c| &c.content().id == id)
            .ok_or_else(|| CoreError::MissingComment(id.clone()).into())
    }

    fn owned(&self, id: &CommentId) -> Result<ProjectedComment, WorkspaceError> {
        let item = self.find(id)?;
        let author = match &item {
            ProjectedComment::Read(c) => c.author.as_ref(),
            ProjectedComment::Draft(c) => Some(&c.author),
        };
        if !author.is_some_and(|a| a.id == self.review.reviewer.id) {
            return Err(WorkspaceError::Invalid(
                "editing and deletion require verified comment ownership".into(),
            ));
        }
        Ok(item)
    }

    pub(crate) fn root(&self, id: &CommentId) -> Result<ProjectedComment, WorkspaceError> {
        let mut item = self.find(id)?;
        let mut seen = BTreeSet::new();
        while let Some(id) = parent(&item) {
            if !seen.insert(id.clone()) {
                return Err(WorkspaceError::Invalid("cyclic comment replies".into()));
            }
            item = self.find(id)?;
        }
        Ok(item)
    }

    fn progress_cancelled(&self, event: &Event) -> bool {
        let Some(key) = progress_key(event) else {
            return false;
        };
        if self.attempts().any(|attempt| {
            (attempt.uncertain
                || self
                    .attempt
                    .as_ref()
                    .is_some_and(|active| active.id == attempt.id))
                && attempt.operations.iter().any(|operation| {
                    !self.has_result(&operation.id)
                        && progress_key(&operation.change).as_ref() == Some(&key)
                })
        }) {
            return false;
        }
        let recent = self
            .results
            .iter()
            .skip(self.snapshot_result_count)
            .rev()
            .find(|result| progress_key(&result.operation.change).as_ref() == Some(&key))
            .map(|result| &result.operation.change);
        match event {
            Event::SetFileViewed { path, viewed, .. } => {
                let baseline = match recent {
                    Some(Event::SetFileViewed { viewed, .. }) => Some(*viewed),
                    _ => self
                        .review
                        .read
                        .files
                        .iter()
                        .find(|file| file.path == *path)
                        .map(|file| file.viewed),
                };
                baseline == Some(*viewed)
            }
            Event::SetCommentReaction {
                comment,
                reaction,
                present,
            } => {
                let baseline = match recent {
                    Some(Event::SetCommentReaction { present, .. }) => *present,
                    _ => self.review.read.reactions.iter().any(|item| {
                        item.comment == *comment
                            && item.author.as_ref() == Some(&self.review.reviewer.id)
                            && item.reaction == *reaction
                    }),
                };
                baseline == *present
            }
            _ => false,
        }
    }

    fn append(&mut self, event: Event) -> Result<(), WorkspaceError> {
        let cancelled = self.progress_cancelled(&event);
        let mut record = RecordedEvent::new(event);
        record.cancelled = cancelled;
        self.batch.write.events.push(record);
        self.projected()?;
        Ok(())
    }

    pub(crate) fn is_delivered(&self, id: &CommentId) -> bool {
        self.review
            .read
            .comments
            .iter()
            .any(|comment| &comment.content.id == id)
            || self.results.iter().any(|result| {
                matches!(&result.operation.change,
                Event::CreateComment { comment } if &comment.content.id == id)
            })
    }

    /// Receipts remain authoritative after their effects enter a newer baseline.
    pub fn has_result(&self, operation_id: &str) -> bool {
        self.results
            .iter()
            .any(|result| result.operation.id == operation_id)
    }

    fn comment_mutable(&self, id: &CommentId) -> Result<(), WorkspaceError> {
        if self.destinations.get(id) == Some(&Destination::Review) && !self.is_delivered(id) {
            self.ensure_wip_mutable()?;
        }
        if self.attempts().any(|attempt| {
            (attempt.uncertain || self.attempt.as_ref().is_some_and(|active| active.id == attempt.id))
            && attempt.destination == Destination::Review && attempt.operations.iter().any(|op| {
                !self.has_result(&op.id)
                    && matches!(&op.change,Event::CreateComment{comment} if &comment.content.id==id)
            })
        }) {
            return Err(WorkspaceError::Invalid(
                "draft belongs to a frozen review submission".into(),
            ));
        }
        if self.suspended_attempts.iter().any(|attempt| {
            attempt.uncertain
                && attempt
                    .operations
                    .iter()
                    .any(|op| !self.has_result(&op.id) && operation_comment(&op.change) == Some(id))
        }) {
            return Err(WorkspaceError::Invalid(
                "comment has an uncertain publication result; reconcile it before editing".into(),
            ));
        }
        Ok(())
    }

    fn unsent_comment(&self, id: &CommentId) -> Result<(), WorkspaceError> {
        if self.is_delivered(id)
            || self
                .attempts()
                .map(|attempt| &attempt.provider)
                .chain(std::iter::once(&self.preparation))
                .any(|provider| prepared_comment(provider, id))
        {
            return Err(WorkspaceError::Invalid(
                "published comments are immutable; create a new comment or reply".into(),
            ));
        }
        self.comment_mutable(id)
    }

    pub fn apply(&mut self, action: Action) -> Result<(), WorkspaceError> {
        self.apply_group(&[action])
    }

    pub(crate) fn apply_group(&mut self, actions: &[Action]) -> Result<(), WorkspaceError> {
        let mut candidate = self.clone();
        let mut receipts = None;
        for action in actions {
            if let Action::RecordResult { result } = action {
                let indices = receipts.get_or_insert_with(|| candidate.receipt_indices());
                candidate.record_result(result.clone(), indices)?;
            } else {
                candidate.apply_inner(action.clone())?;
                if matches!(
                    action,
                    Action::BeginPublication { .. } | Action::ResumePublication { .. }
                ) {
                    receipts = None;
                }
            }
        }
        *self = candidate;
        Ok(())
    }

    fn receipt_indices(&self) -> ReceiptIndices {
        ReceiptIndices {
            operations: self
                .attempt
                .iter()
                .flat_map(|attempt| attempt.operations.iter().enumerate())
                .map(|(index, operation)| (operation.id.clone(), index))
                .collect(),
            acknowledged: self
                .results
                .iter()
                .map(|result| result.operation.id.clone())
                .collect(),
        }
    }

    fn record_result(
        &mut self,
        result: OperationResult,
        indices: &mut ReceiptIndices,
    ) -> Result<(), WorkspaceError> {
        self.saved_comment = None;
        let attempt = self
            .attempt
            .as_ref()
            .ok_or_else(|| WorkspaceError::Invalid("no publication attempt".into()))?;
        if indices
            .operations
            .get(&result.operation.id)
            .and_then(|index| attempt.operations.get(*index))
            != Some(&result.operation)
        {
            return Err(WorkspaceError::Invalid(
                "result is not part of this publication attempt".into(),
            ));
        }
        if indices.acknowledged.insert(result.operation.id.clone()) {
            self.results.push(result);
        }
        Ok(())
    }

    /// Preflight the selected objects before ordering their individual deletions.
    pub(crate) fn delete_actions(
        &self,
        comments: Vec<CommentId>,
    ) -> Result<Vec<Action>, WorkspaceError> {
        if comments.is_empty() {
            return Ok(Vec::new());
        }
        let projected = self.projected()?.comments;
        let mut selected = BTreeSet::new();
        let mut deletions = Vec::new();
        for comment in comments
            .into_iter()
            .filter(|id| selected.insert(id.clone()))
        {
            let item = if projected.iter().any(|item| item.content().id == comment) {
                self.owned(&comment)?
            } else {
                let mut restored = self.clone();
                restored.batch.write.events.retain(|event| {
                    !matches!(
                        &event.event, Event::DeleteComment { comment: id } if id == &comment
                    )
                });
                restored.owned(&comment)?
            };
            if self.summary_comments.contains(&comment) {
                return Err(WorkspaceError::Invalid(
                    "submitted review summaries cannot be deleted".into(),
                ));
            }
            let mut depth = 0;
            let mut seen = BTreeSet::new();
            let mut ancestor = parent(&item);
            while let Some(id) = ancestor {
                if !seen.insert(id.clone()) {
                    return Err(WorkspaceError::Invalid("cyclic comment replies".into()));
                }
                depth += 1;
                ancestor = projected
                    .iter()
                    .find(|item| &item.content().id == id)
                    .and_then(parent);
            }
            deletions.push((depth, comment));
        }
        for item in &projected {
            if !self.is_delivered(&item.content().id)
                && parent(item).is_some_and(|parent| selected.contains(parent))
                && !selected.contains(&item.content().id)
            {
                return Err(WorkspaceError::Invalid(format!(
                    "delete unpublished replies first: {}",
                    item.content().id.0
                )));
            }
        }
        deletions.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        Ok(deletions
            .into_iter()
            .map(|(_, comment)| Action::Delete { comment })
            .collect())
    }

    /// Resolve contextual undo against current state, preflighting the whole selection.
    /// Execution guards stay in the individual actions so ordered writes can retain a prefix.
    pub(crate) fn undo_actions(
        &self,
        comments: Vec<CommentId>,
    ) -> Result<Vec<Action>, WorkspaceError> {
        let projected = self.projected()?.comments;
        let mut seen = BTreeSet::new();
        let mut deletions = Vec::new();
        let mut actions = Vec::new();
        for comment in comments.into_iter().filter(|id| seen.insert(id.clone())) {
            let deleted = self.batch.write.events.iter().any(|event| {
                matches!(
                    &event.event, Event::DeleteComment { comment: id } if id == &comment
                )
            });
            let canceled_create = !self.is_delivered(&comment) && deleted
                && self.batch.write.events.iter().any(|event| matches!(
                    &event.event, Event::CreateComment { comment: draft } if draft.content.id == comment
                ));
            if canceled_create {
                let mut restored = self.clone();
                restored.batch.write.events.retain(|event| {
                    !matches!(
                        &event.event, Event::DeleteComment { comment: id } if id == &comment
                    )
                });
                restored.owned(&comment)?;
                actions.push(Action::Delete { comment });
            } else {
                let item = projected
                    .iter()
                    .find(|item| item.content().id == comment)
                    .ok_or_else(|| CoreError::MissingComment(comment.clone()))?;
                if matches!(item, ProjectedComment::Draft(_)) && !self.is_delivered(&comment) {
                    deletions.push(comment);
                } else {
                    if !self
                        .review
                        .read
                        .comments
                        .iter()
                        .any(|item| item.content.id == comment)
                    {
                        return Err(WorkspaceError::Invalid(
                            "progress is available only for fetched comments".into(),
                        ));
                    }
                    actions.push(Action::Reaction {
                        comment,
                        present: false,
                    });
                }
            }
        }
        Ok(self
            .delete_actions(deletions)?
            .into_iter()
            .chain(actions)
            .collect())
    }

    fn apply_inner(&mut self, action: Action) -> Result<(), WorkspaceError> {
        // The store commits only a successful candidate; errors never persist a partial mutation.
        self.saved_comment = None;
        match action {
            Action::PrDetails { .. } => {
                return Err(WorkspaceError::Invalid(
                    "PR details require the canonical store".into(),
                ));
            }
            Action::Start => {
                self.ensure_wip_mutable()?;
                self.wip().active = true;
            }
            Action::Leave => {
                self.ensure_wip_mutable()?;
                if let Some(wip) = &mut self.wip {
                    wip.active = false;
                }
            }
            Action::Save {
                comment: Some(id),
                body,
                target,
                destination,
            } => {
                self.unsent_comment(&id)?;
                if body.trim().is_empty() {
                    // An explicit delete is repeatable; a stale composer save still needs its comment.
                    self.find(&id)?;
                    return self.apply_inner(Action::Delete { comment: id });
                }
                if self.batch.write.events.iter().any(
                    |event| matches!(&event.event,Event::DeleteComment{comment} if *comment==id),
                ) {
                    self.apply_inner(Action::Restore {
                        comment: id.clone(),
                    })?;
                }
                if let Some(destination) = destination {
                    let current = if self.is_delivered(&id) {
                        Destination::Standalone
                    } else {
                        self.destinations
                            .get(&id)
                            .copied()
                            .unwrap_or(Destination::Standalone)
                    };
                    if destination != current {
                        return Err(WorkspaceError::Invalid(
                            "editing preserves the comment publication destination".into(),
                        ));
                    }
                }
                if let Some(target) = target {
                    let CommentTarget::Diff(location) = target else {
                        return Err(WorkspaceError::Invalid(
                            "relocation needs a diff location".into(),
                        ));
                    };
                    self.apply_inner(Action::Relocate {
                        comment: id.clone(),
                        location,
                    })?;
                }
                let item = self.owned(&id)?;
                if self.conflicts.contains_key(&id) {
                    return Err(WorkspaceError::Invalid(
                        "resolve the remote conflict before editing".into(),
                    ));
                }
                if item.content().body != body {
                    self.append(Event::EditComment {
                        comment: id.clone(),
                        body,
                    })?;
                }
                self.saved_comment = Some(id);
            }
            Action::Save {
                comment: None,
                target,
                body,
                destination,
            } => {
                if body.trim().is_empty() {
                    return Ok(());
                }
                let mut target = target.ok_or_else(|| {
                    WorkspaceError::Invalid("new comment needs an explicit target".into())
                })?;
                let mut style = Destination::Review;
                match &target {
                    CommentTarget::PullRequest => {
                        if destination == Some(Destination::Review) {
                            return Err(WorkspaceError::Invalid(
                                "review-wide prose belongs in the conclusion composer".into(),
                            ));
                        }
                        style = Destination::Standalone;
                    }
                    CommentTarget::Reply(id) => {
                        self.find(id)?;
                        if !self.is_delivered(id)
                            && let Some(parent_style) = self.destinations.get(id)
                        {
                            if destination.is_some_and(|d| d != *parent_style) {
                                return Err(WorkspaceError::Invalid(
                                    "unpublished replies must share their parent's destination"
                                        .into(),
                                ));
                            }
                            style = *parent_style;
                        }
                        let root = self.root(id)?;
                        if !matches!(
                            &root,
                            ProjectedComment::Read(ReadComment {
                                target: CommentTarget::Diff(_),
                                ..
                            }) | ProjectedComment::Draft(DraftComment {
                                target: CommentTarget::Diff(_),
                                ..
                            })
                        ) {
                            return Err(WorkspaceError::Invalid("this comment has no inline reply capability; create a standalone PR comment".into()));
                        }
                        // The provider supports root-thread replies, not nested resources.
                        target = CommentTarget::Reply(root.content().id.clone());
                    }
                    CommentTarget::Diff(location) => {
                        if location.revision != self.comparison {
                            return Err(WorkspaceError::Invalid(
                                if self.worktree_context {
                                    "HEAD changed; reopen the diff before commenting"
                                } else {
                                    "new comment must use the pinned comparison"
                                }
                                .into(),
                            ));
                        }
                        if !self
                            .comparison_files
                            .iter()
                            .any(|f| f.path == location.position.path)
                        {
                            return Err(WorkspaceError::Invalid(
                                "comment path is not in the comparison".into(),
                            ));
                        }
                        self.committed_line_source(location)?;
                    }
                }
                if style == Destination::Review {
                    self.ensure_wip_mutable()?;
                    self.wip();
                }
                let id = CommentId(Uuid::new_v4().to_string());
                self.append(Event::CreateComment {
                    comment: DraftComment {
                        content: CommentContent {
                            id: id.clone(),
                            body,
                        },
                        author: self.review.reviewer.clone(),
                        target,
                    },
                })?;
                self.destinations.insert(id.clone(), style);
                self.saved_comment = Some(id);
            }
            Action::MoveToReview { comments } => {
                self.ensure_wip_mutable()?;
                let selected = comments.into_iter().collect::<BTreeSet<_>>();
                if selected.is_empty() {
                    return Err(WorkspaceError::Invalid(
                        "select unsent local inline drafts to move".into(),
                    ));
                }
                for id in &selected {
                    self.unsent_comment(id)?;
                    let item = self.owned(id)?;
                    if !matches!(
                        item,
                        ProjectedComment::Draft(DraftComment {
                            target: CommentTarget::Diff(_) | CommentTarget::Reply(_),
                            ..
                        })
                    ) {
                        return Err(WorkspaceError::Invalid(
                            "only unsent local inline drafts can move into the review".into(),
                        ));
                    }
                }
                for item in &self.projected()?.comments {
                    let Some(parent) = parent(item) else { continue };
                    let id = &item.content().id;
                    let parent_moving = selected.contains(parent);
                    let child_moving = selected.contains(id);
                    if (child_moving
                        && !parent_moving
                        && !self.is_delivered(parent)
                        && self.destinations.get(parent) != Some(&Destination::Review))
                        || (parent_moving
                            && !child_moving
                            && !self.is_delivered(id)
                            && self.destinations.get(id) != Some(&Destination::Review))
                    {
                        return Err(WorkspaceError::Invalid(
                            "move unpublished parents and replies together".into(),
                        ));
                    }
                }
                self.wip();
                for id in selected {
                    self.destinations.insert(id, Destination::Review);
                }
            }
            Action::DeleteComments { comments } => {
                for action in self.delete_actions(comments)? {
                    self.apply_inner(action)?;
                }
            }
            Action::Delete { comment } => {
                self.unsent_comment(&comment)?;
                if self.summary_comments.contains(&comment) {
                    return Err(WorkspaceError::Invalid(
                        "submitted review summaries cannot be deleted".into(),
                    ));
                }
                if self.batch.write.events.iter().any(
                    |event| matches!(&event.event, Event::DeleteComment { comment: id } if id == &comment),
                ) {
                    // Repeated operators can still address a deleted object in the stable buffer.
                    // Restoration validates ownership and publication guards without changing it.
                    let mut restored = self.clone();
                    restored.apply_inner(Action::Restore { comment })?;
                    return Ok(());
                }
                self.owned(&comment)?;
                let dependents = self
                    .projected()?
                    .comments
                    .into_iter()
                    .filter(|c| parent(c) == Some(&comment) && !self.is_delivered(&c.content().id))
                    .map(|c| c.content().id.0.clone())
                    .collect::<Vec<_>>();
                if !dependents.is_empty() {
                    return Err(WorkspaceError::Invalid(format!(
                        "delete unpublished replies first: {}",
                        dependents.join(", ")
                    )));
                }
                if self.conflicts.contains_key(&comment) {
                    return Err(WorkspaceError::Invalid(
                        "resolve remote conflict before deleting".into(),
                    ));
                }
                self.rejected_anchors.remove(&comment);
                self.append(Event::DeleteComment { comment })?;
            }
            Action::Restore { comment } => {
                self.unsent_comment(&comment)?;
                if !self.batch.write.events.iter().any(|record| {
                    matches!(&record.event,
                    Event::CreateComment { comment: draft } if draft.content.id == comment)
                }) {
                    return Err(WorkspaceError::Invalid(
                        "restore requires a local draft".into(),
                    ));
                }
                if self.conflicts.contains_key(&comment) {
                    return Err(WorkspaceError::Invalid(
                        "resolve remote conflict before restoring".into(),
                    ));
                }
                if !self
                    .batch
                    .write
                    .events
                    .iter()
                    .any(|e| matches!(&e.event,Event::DeleteComment{comment:id} if *id==comment))
                {
                    return Err(WorkspaceError::Invalid(
                        "comment has no unsent deletion to restore".into(),
                    ));
                }
                self.batch.write.events.retain(
                    |e| !matches!(&e.event,Event::DeleteComment{comment:id} if *id == comment),
                );
                self.owned(&comment)?;
            }
            Action::Undo { comments } => {
                for action in self.undo_actions(comments)? {
                    self.apply_inner(action)?;
                }
            }
            Action::Viewed { path, viewed } => {
                let previous = self
                    .projected()?
                    .files
                    .into_iter()
                    .find(|f| f.path == path)
                    .ok_or_else(|| CoreError::MissingFile(path.clone()))?;
                if previous.viewed != viewed {
                    let event = Event::SetFileViewed {
                        path,
                        viewed,
                        commit: Some(self.comparison.head.clone()),
                    };
                    self.append(event)?;
                }
            }
            Action::Reaction { comment, present } => {
                if !self
                    .review
                    .read
                    .comments
                    .iter()
                    .any(|c| c.content.id == comment && !c.draft)
                {
                    return Err(WorkspaceError::Invalid(
                        "progress is available only for fetched comments".into(),
                    ));
                }
                self.find(&comment)?;
                let previous = self.projected()?.reactions.iter().any(|r| {
                    r.comment == comment
                        && r.author.as_ref() == Some(&self.review.reviewer.id)
                        && is_thumbs_up(&r.reaction)
                });
                if previous != present {
                    self.append(Event::SetCommentReaction {
                        comment,
                        reaction: "👍".into(),
                        present,
                    })?;
                }
            }
            Action::Summary { conclusion } => {
                self.apply_inner(Action::PublishReview {
                    conclusion: Some(conclusion),
                })?;
            }
            Action::PublishReview { conclusion } => {
                self.ensure_wip_mutable()?;
                if let Some(conclusion) = conclusion {
                    if self.wip().conclusion.as_ref() != Some(&conclusion) {
                        self.wip().conclusion = Some(conclusion.clone());
                        self.append(Event::SetReviewConclusion { conclusion })?;
                    }
                } else if let Some(wip) = &mut self.wip {
                    wip.conclusion = None;
                }
            }
            Action::CompareLatest => {
                self.comparison = self.review.read.pull_request.revision.clone();
                self.comparison_files = self.review.read.files.clone();
            }
            Action::Relocate { comment, location } => {
                self.unsent_comment(&comment)?;
                if self.is_delivered(&comment) {
                    return Err(WorkspaceError::Invalid(
                        "only unpublished root drafts can be relocated".into(),
                    ));
                }
                if location.revision != self.comparison {
                    return Err(WorkspaceError::Invalid(
                        "relocation must use the pinned comparison".into(),
                    ));
                }
                if !self
                    .comparison_files
                    .iter()
                    .any(|f| f.path == location.position.path)
                {
                    return Err(WorkspaceError::Invalid(
                        "relocation path is not in the comparison".into(),
                    ));
                }
                self.committed_line_source(&location)?;
                self.owned(&comment)?;
                let event = self
                    .batch
                    .write
                    .events
                    .iter_mut()
                    .find(
                        |e| matches!(&e.event,Event::CreateComment{comment:c} if c.content.id == comment),
                    )
                    .ok_or_else(|| {
                        WorkspaceError::Invalid("draft creation event unavailable".into())
                    })?;
                let Event::CreateComment { comment: draft } = &mut event.event else {
                    unreachable!()
                };
                if !matches!(draft.target, CommentTarget::Diff(_)) {
                    return Err(WorkspaceError::Invalid(
                        "only inline/file root drafts can be relocated".into(),
                    ));
                }
                draft.target = CommentTarget::Diff(location);
                self.rejected_anchors.remove(&comment);
                self.projected()?;
            }
            Action::Resolve {
                comment,
                resolution,
                body,
            } => {
                self.comment_mutable(&comment)?;
                let conflict = self
                    .conflicts
                    .get(&comment)
                    .cloned()
                    .ok_or_else(|| WorkspaceError::Invalid("comment has no conflict".into()))?;
                match resolution {
                    Resolution::Remote => {
                        self.batch.write.events.retain(|event|!matches!(&event.event,Event::EditComment{comment:id,..}|Event::DeleteComment{comment:id} if *id==comment));
                        if conflict.remote.is_none() {
                            // A reply requires a live root; retain it as a separate blocked conflict.
                            let replies = self.batch.write.events.iter().filter_map(|e| match &e.event {Event::CreateComment {comment:d} if matches!(&d.target,CommentTarget::Reply(p) if p == &comment) => Some(d.content.id.clone()),_=>None}).collect::<Vec<_>>();
                            if !replies.is_empty() {
                                return Err(WorkspaceError::Invalid(format!(
                                    "missing root still has unpublished replies: {}",
                                    replies
                                        .iter()
                                        .map(|c| c.0.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )));
                            }
                        }
                    }
                    Resolution::Local | Resolution::Edit => {
                        if conflict.remote.is_none() {
                            return Err(WorkspaceError::Invalid("remote comment was deleted; discard intent or create a new comment explicitly".into()));
                        }
                        if resolution == Resolution::Edit {
                            if self.batch.write.events.iter().any(|event|matches!(&event.event,Event::DeleteComment{comment:id} if *id==comment)) {return Err(WorkspaceError::Invalid("deletion conflict supports local or remote resolution only".into()));}
                            let body = body.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                                WorkspaceError::Invalid(
                                    "edited resolution needs a nonblank body".into(),
                                )
                            })?;
                            self.batch.write.events.retain(
                                |e| !matches!(&e.event,Event::DeleteComment{comment:id} if *id == comment),
                            );
                            self.batch
                                .write
                                .events
                                .push(RecordedEvent::new(Event::EditComment {
                                    comment: comment.clone(),
                                    body,
                                }));
                        }
                    }
                }
                self.conflicts.remove(&comment);
                self.projected()?;
            }
            Action::ResumePublication { attempt_id } => {
                if self.attempt.is_some() {
                    return Err(WorkspaceError::Invalid(
                        "another publication attempt is active".into(),
                    ));
                }
                let index = self
                    .suspended_attempts
                    .iter()
                    .position(|attempt| attempt.id == attempt_id)
                    .ok_or_else(|| {
                        WorkspaceError::Invalid("suspended attempt does not exist".into())
                    })?;
                if index != 0 {
                    return Err(WorkspaceError::Invalid(
                        "retry the earliest failed publication before later attempts".into(),
                    ));
                }
                let mut attempt = self.suspended_attempts.remove(index);
                if !attempt.uncertain {
                    let prepared = attempt
                        .provider
                        .get("prepared")
                        .map(|value| serde_json::from_value::<Vec<OperationResult>>(value.clone()))
                        .transpose()
                        .map_err(|error| {
                            WorkspaceError::Invalid(format!(
                                "invalid saved review preparation: {error}"
                            ))
                        })?
                        .unwrap_or_default();
                    let applied = |operation: &Operation| {
                        self.has_result(&operation.id)
                            || prepared.iter().any(|result| result.operation == *operation)
                    };
                    let pending = self.pending()?;
                    let remaining = CommitState {
                        operations: pending
                            .operations
                            .into_iter()
                            .filter(|operation| {
                                attempt.operations.iter().any(|previous| {
                                    !applied(previous)
                                        && same_operation_target(
                                            &previous.change,
                                            &operation.change,
                                        )
                                })
                            })
                            .collect(),
                    };
                    self.validate_publication_plan(&remaining)?;
                    let mut operations = attempt
                        .operations
                        .iter()
                        .filter(|operation| applied(operation))
                        .cloned()
                        .collect::<Vec<_>>();
                    operations.extend(remaining.operations);
                    attempt.operations = operations;
                }
                self.attempt = Some(attempt);
            }
            Action::SuspendPublication => {
                self.apply_inner(Action::FinishPublication {
                    success: false,
                    uncertain: true,
                    error: self.last_error.clone(),
                })?;
            }
            Action::BeginPublication {
                destination,
                operations,
            } => {
                if self.attempt.is_some() {
                    return Err(WorkspaceError::Invalid(
                        "publication attempt already exists; reconcile or retry that attempt"
                            .into(),
                    ));
                }
                if !self.suspended_attempts.is_empty() {
                    return Err(WorkspaceError::Invalid(
                        "resume the failed publication before starting another".into(),
                    ));
                }
                let expected = self.publication_plan()?;
                if expected.operations != operations {
                    return Err(WorkspaceError::Invalid(
                        "publication plan no longer matches saved workspace".into(),
                    ));
                }
                self.attempt = Some(PublicationAttempt {
                    id: Uuid::new_v4().to_string(),
                    wip: self.wip.as_ref().map(|w| w.id.clone()),
                    destination,
                    operations,
                    uncertain: false,
                    provider: if destination == Destination::Review {
                        self.preparation.clone()
                    } else {
                        serde_json::Value::Null
                    },
                });
            }
            Action::RecordResult { result } => {
                let mut indices = self.receipt_indices();
                self.record_result(result, &mut indices)?;
            }
            Action::SetAttemptProvider { provider } => {
                self.attempt
                    .as_mut()
                    .ok_or_else(|| WorkspaceError::Invalid("no publication attempt".into()))?
                    .provider = provider;
            }
            Action::FinishPublication {
                success,
                uncertain,
                error,
            } => {
                let attempt = self
                    .attempt
                    .as_ref()
                    .ok_or_else(|| WorkspaceError::Invalid("no publication attempt".into()))?;
                let submitted = attempt.destination == Destination::Review
                    && attempt.operations.iter().any(|op| {
                        matches!(op.change, Event::SetReviewConclusion { .. })
                            && self.has_result(&op.id)
                    });
                if submitted {
                    if self
                        .wip
                        .as_ref()
                        .is_some_and(|w| Some(&w.id) == attempt.wip.as_ref())
                    {
                        self.wip = None;
                    }
                    if self.wip.is_none() {
                        self.preparation = serde_json::Value::Null;
                    }
                } else if !uncertain && attempt.destination == Destination::Review {
                    self.preparation = attempt.provider.clone();
                }
                if !success || uncertain {
                    let mut attempt = self.attempt.take().unwrap();
                    attempt.uncertain = uncertain;
                    self.suspended_attempts.insert(0, attempt);
                } else {
                    self.attempt = None;
                }
                self.last_error = if success { None } else { error };
            }
            Action::RefreshError { error } => {
                self.last_error = Some(error);
            }
            Action::RejectAnchor { comment, message } => {
                self.find(&comment)?;
                self.rejected_anchors.insert(comment, message);
            }
        }
        Ok(())
    }

    /// Select every surviving local operation without skipping blocked intent.
    pub fn publication_plan(&self) -> Result<CommitState, WorkspaceError> {
        let plan = self.pending()?;
        self.validate_publication_plan(&plan)?;
        Ok(plan)
    }

    fn validate_publication_plan(&self, plan: &CommitState) -> Result<(), WorkspaceError> {
        for operation in &plan.operations {
            if let Event::CreateComment {
                comment:
                    DraftComment {
                        content,
                        target: CommentTarget::Diff(_) | CommentTarget::Reply(_),
                        ..
                    },
            } = &operation.change
                && self.destinations.get(&content.id) != Some(&Destination::Review)
            {
                return Err(WorkspaceError::Invalid(format!(
                    "standalone inline draft {} requires explicit adoption with :Review move before publication",
                    content.id.0
                )));
            }
            if let Some(id) = operation_comment(&operation.change) {
                let parent = match &operation.change {
                    Event::CreateComment {
                        comment:
                            DraftComment {
                                target: CommentTarget::Reply(parent),
                                ..
                            },
                    } => Some(parent),
                    _ => None,
                };
                if std::iter::once(id).chain(parent).any(|id| {
                    self.conflicts.contains_key(id) || self.rejected_anchors.contains_key(id)
                }) {
                    return Err(WorkspaceError::Invalid(format!(
                        "publication requires conflict or anchor resolution for {}",
                        id.0
                    )));
                }
            }
            if self
                .suspended_attempts
                .iter()
                .filter(|attempt| attempt.uncertain)
                .any(|attempt| {
                    attempt.operations.iter().any(|blocked| {
                        !self.has_result(&blocked.id)
                            && (blocked.id == operation.id
                                || operation_comment(&blocked.change).is_some_and(|id| {
                                    operation_comment(&operation.change) == Some(id)
                                })
                                || progress_key(&blocked.change).is_some_and(|key| {
                                    progress_key(&operation.change) == Some(key)
                                }))
                    })
                })
            {
                return Err(WorkspaceError::Invalid(
                    "reconcile the uncertain publication before publishing its pending operations"
                        .into(),
                ));
            }
            if let Event::SetReviewConclusion { conclusion } = &operation.change
                && conclusion.outcome != ReviewOutcome::Approve
                && conclusion
                    .body
                    .as_ref()
                    .is_none_or(|body| body.trim().is_empty())
            {
                return Err(WorkspaceError::Invalid(
                    "Comment and Request changes require a nonblank summary".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn view(&self) -> Result<View, WorkspaceError> {
        let (projected, pending) = self.get_unpublished()?;
        let pending = pending.operations;
        let mut comments = projected.comments;
        let mut chronological = self
            .review
            .read
            .comments
            .iter()
            .enumerate()
            .collect::<Vec<_>>();
        chronological.sort_by(|(li, left), (ri, right)| {
            self.chronology
                .get(&left.content.id)
                .cmp(&self.chronology.get(&right.content.id))
                .then_with(|| li.cmp(ri))
                .then_with(|| left.content.id.cmp(&right.content.id))
        });
        let mut order = BTreeMap::new();
        for (index, (_, c)) in chronological.into_iter().enumerate() {
            order.insert(c.content.id.clone(), index);
        }
        let fetched_count = order.len();
        for (index, event) in self.batch.write.events.iter().enumerate() {
            if let Event::CreateComment { comment } = &event.event {
                order
                    .entry(comment.content.id.clone())
                    .or_insert(fetched_count + index);
            }
        }
        let by_id = comments
            .iter()
            .map(|c| (c.content().id.clone(), c))
            .collect::<BTreeMap<_, _>>();
        let mut keys = BTreeMap::new();
        for c in &comments {
            let mut root = c;
            let mut seen = BTreeSet::new();
            while let Some(id) = parent(root) {
                if !seen.insert(id.clone()) {
                    break;
                }
                let Some(found) = by_id.get(id) else {
                    break;
                };
                root = found;
            }
            let location = match root {
                ProjectedComment::Read(ReadComment {
                    target: CommentTarget::Diff(location),
                    ..
                }) => {
                    let anchor=location.current.as_ref().filter(|a| matches!(&a.revision,ReadCommentRevision::Comparison(r) if *r==self.comparison) || matches!(&a.revision,ReadCommentRevision::Commit{head} if *head==self.comparison.head)).unwrap_or(&location.original);
                    Some(&anchor.position)
                }
                ProjectedComment::Draft(DraftComment {
                    target: CommentTarget::Diff(location),
                    ..
                }) => Some(&location.position),
                _ => None,
            };
            let (unlocated, path, side, start, end) = match location {
                Some(p) => {
                    let range = p.range.as_ref();
                    (
                        false,
                        p.path.clone(),
                        range.map_or(0, |r| if r.start.side == DiffSide::Base { 0 } else { 1 }),
                        range.map_or(0, |r| r.start.line.get()),
                        range.map_or(0, |r| r.end.line.get()),
                    )
                }
                None => (true, String::new(), 0, 0, 0),
            };
            keys.insert(
                c.content().id.clone(),
                (
                    unlocated,
                    path,
                    side,
                    start,
                    end,
                    *order.get(&root.content().id).unwrap_or(&usize::MAX),
                    *order.get(&c.content().id).unwrap_or(&usize::MAX),
                    c.content().id.clone(),
                ),
            );
        }
        comments.sort_by(|a, b| keys[&a.content().id].cmp(&keys[&b.content().id]));
        let comments = comments
            .into_iter()
            .map(|comment| {
                let id = &comment.content().id;
                let fetched = self
                    .review
                    .read
                    .comments
                    .iter()
                    .any(|c| &c.content.id == id);
                let done = projected.reactions.iter().any(|reaction| {
                    &reaction.comment == id
                        && reaction.author.as_ref() == Some(&self.review.reviewer.id)
                        && is_thumbs_up(&reaction.reaction)
                });
                CommentView {
                    fetched,
                    draft: match &comment {
                        ProjectedComment::Read(comment) => comment.draft,
                        ProjectedComment::Draft(_) => {
                            !self.is_delivered(id)
                                || self
                                    .attempts()
                                    .map(|attempt| &attempt.provider)
                                    .chain(std::iter::once(&self.preparation))
                                    .any(|provider| {
                                        provider
                                            .get("submitted")
                                            .and_then(serde_json::Value::as_bool)
                                            != Some(true)
                                            && provider
                                                .get("pending")
                                                .is_some_and(|pending| !pending.is_null())
                                            && prepared_comment(provider, id)
                                    })
                        }
                    },
                    done,
                    destination: if !self.is_delivered(id) {
                        self.destinations.get(id).copied()
                    } else {
                        None
                    },
                    anchor_error: self.rejected_anchors.get(id).cloned(),
                    conflict: self.conflicts.contains_key(id),
                    summary: self.summary_comments.contains(id),
                    comment,
                }
            })
            .collect::<Vec<_>>();
        let todo = comments
            .iter()
            .filter(|c| c.fetched && !c.draft && !c.done)
            .map(|c| c.comment.content().id.clone())
            .collect();
        let done = comments
            .iter()
            .filter(|c| c.fetched && !c.draft && c.done)
            .map(|c| c.comment.content().id.clone())
            .collect();
        let mut files = projected.files;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(View {
            pr: None,
            stats: self.stats(&pending),
            publications: vec![],
            id: self.id.clone(),
            locator: self.locator.clone(),
            generation: self.generation,
            review: self.review.clone(),
            comparison: self.comparison.clone(),
            wip: self.wip.clone(),
            comments,
            files,
            pending,
            todo,
            done,
            conflicts: self.conflicts.values().cloned().collect(),
            last_error: self.last_error.clone(),
            new_commits: self.review.read.pull_request.revision.head != self.comparison.head,
            unpulled_commits: vec![],
            comparison_error: None,
            saved_comment: self.saved_comment.clone(),
            attempt: self.attempt.clone(),
            suspended_attempts: self.suspended_attempts.clone(),
        })
    }

    /// Rebase outstanding intent onto a complete fresh snapshot, preserving IDs.
    pub fn refresh(&mut self, review: Review) -> Result<(), WorkspaceError> {
        if review.reviewer.id != self.review.reviewer.id
            || review.read.pull_request.id != self.review.read.pull_request.id
        {
            return Err(WorkspaceError::Invalid(
                "refresh target/reviewer differs from saved workspace".into(),
            ));
        }
        if self.attempt.is_some() {
            return Err(WorkspaceError::Invalid(
                "publication must be reconciled before refreshing saved intent".into(),
            ));
        }
        let pending = self.pending()?.operations;
        let remote = review
            .read
            .comments
            .iter()
            .map(|comment| (comment.content.id.clone(), comment))
            .collect::<BTreeMap<_, _>>();
        let previous = self
            .review
            .read
            .comments
            .iter()
            .map(|comment| (comment.content.id.clone(), comment))
            .collect::<BTreeMap<_, _>>();
        for conflict in self.conflicts.values_mut() {
            conflict.remote = remote
                .get(&conflict.comment)
                .map(|comment| (*comment).clone());
            conflict.kind = if conflict.remote.is_some() {
                ConflictKind::Changed
            } else {
                ConflictKind::Deleted
            };
        }
        for operation in pending {
            let comment = match &operation.change {
                Event::EditComment { comment, body } => {
                    if remote
                        .get(comment)
                        .is_some_and(|new| new.content.body == *body)
                    {
                        self.conflicts.remove(comment);
                        continue;
                    }
                    comment
                }
                Event::DeleteComment { comment } => {
                    if !remote.contains_key(comment) {
                        continue;
                    }
                    comment
                }
                Event::CreateComment { comment } => match &comment.target {
                    CommentTarget::Reply(parent) if !remote.contains_key(parent) => parent,
                    _ => continue,
                },
                _ => continue,
            };
            if let Some(old) = previous.get(comment)
                && remote
                    .get(comment)
                    .is_none_or(|new| new.content.body != old.content.body)
            {
                self.conflicts.insert(
                    comment.clone(),
                    Conflict {
                        comment: comment.clone(),
                        previous: (*old).clone(),
                        remote: remote.get(comment).map(|new| (*new).clone()),
                        kind: if remote.contains_key(comment) {
                            ConflictKind::Changed
                        } else {
                            ConflictKind::Deleted
                        },
                    },
                );
            }
        }
        self.review = review;
        self.batch.review = self.review.id.clone();
        self.snapshot_result_count = self.results.len();
        if self.suspended_attempts.is_empty() {
            self.last_error = None;
        }
        self.saved_comment = None;
        self.projected()?;
        Ok(())
    }
}

pub fn operation_comment(event: &Event) -> Option<&CommentId> {
    match event {
        Event::CreateComment { comment } => Some(&comment.content.id),
        Event::EditComment { comment, .. }
        | Event::DeleteComment { comment }
        | Event::SetCommentReaction { comment, .. } => Some(comment),
        _ => None,
    }
}
fn is_thumbs_up(reaction: &str) -> bool {
    matches!(reaction, "👍" | "+1" | "THUMBS_UP")
}

fn prepared_comment(provider: &serde_json::Value, id: &CommentId) -> bool {
    provider
        .get("prepared")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|prepared| {
            prepared.iter().any(|result| {
                result
                    .pointer("/operation/change/comment/id")
                    .and_then(serde_json::Value::as_str)
                    == Some(id.0.as_str())
            })
        })
}

fn same_operation_target(left: &Event, right: &Event) -> bool {
    match (left, right) {
        (Event::SetReviewConclusion { .. }, Event::SetReviewConclusion { .. }) => true,
        (Event::DismissReview { review: left, .. }, Event::DismissReview { review: right, .. }) => {
            left == right
        }
        (
            Event::CreateComment { .. } | Event::EditComment { .. } | Event::DeleteComment { .. },
            Event::CreateComment { .. } | Event::EditComment { .. } | Event::DeleteComment { .. },
        ) => operation_comment(left) == operation_comment(right),
        _ => progress_key(left).is_some_and(|key| progress_key(right) == Some(key)),
    }
}

fn progress_key(event: &Event) -> Option<(u8, String, String)> {
    match event {
        Event::SetFileViewed { path, .. } => Some((0, path.clone(), String::new())),
        Event::SetCommentReaction {
            comment, reaction, ..
        } => Some((1, comment.0.clone(), reaction.clone())),
        _ => None,
    }
}
