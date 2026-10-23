use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::*;

/// Event shapes are shared; `Operation` and `CommitState` distinguish processed
/// writes from the original, ordered log that backends never consume.
pub use crate::model::Event as Change;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub change: Change,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitState {
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", content = "comment", rename_all = "snake_case")]
pub enum ProjectedComment {
    Read(ReadComment),
    Draft(DraftComment),
}

impl ProjectedComment {
    pub fn content(&self) -> &CommentContent {
        match self {
            Self::Read(comment) => &comment.content,
            Self::Draft(comment) => &comment.content,
        }
    }

    fn content_mut(&mut self) -> &mut CommentContent {
        match self {
            Self::Read(comment) => &mut comment.content,
            Self::Draft(comment) => &mut comment.content,
        }
    }

    fn parent(&self) -> Option<&CommentId> {
        match self {
            Self::Read(ReadComment {
                target: CommentTarget::Reply(parent),
                ..
            })
            | Self::Draft(DraftComment {
                target: CommentTarget::Reply(parent),
                ..
            }) => Some(parent),
            _ => None,
        }
    }

    fn is_diff(&self) -> bool {
        matches!(
            self,
            Self::Read(ReadComment {
                target: CommentTarget::Diff(_),
                ..
            }) | Self::Draft(DraftComment {
                target: CommentTarget::Diff(_),
                ..
            })
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedState {
    pub pull_request: PullRequest,
    pub files: Vec<ChangedFile>,
    pub comments: Vec<ProjectedComment>,
    pub reactions: Vec<CommentReaction>,
    pub reviews: Vec<ReadReview>,
    pub conclusion: Option<ReviewConclusion>,
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("batch {0:?} belongs to another review")]
    ReviewMismatch(BatchId),
    #[error("comment {0:?} does not exist")]
    MissingComment(CommentId),
    #[error("comment {0:?} already exists or was previously created")]
    DuplicateComment(CommentId),
    #[error("file {0:?} is not in the review")]
    MissingFile(String),
    #[error("review resource {0:?} does not exist")]
    MissingReview(ReviewResourceId),
    #[error("invalid state: {0}")]
    Invalid(String),
}

type ReactionKey = (CommentId, Option<UserId>, String);

struct Projection {
    state: ProjectedState,
    comments: BTreeMap<CommentId, ProjectedComment>,
    // Snapshot order followed by creation events in persisted log sequence.
    comment_order: Vec<CommentId>,
    identities: BTreeSet<CommentId>,
    file_positions: BTreeMap<String, usize>,
    reactions: BTreeMap<ReactionKey, Vec<(usize, CommentReaction)>>,
    next_reaction: usize,
}

impl Projection {
    fn new(review: &Review) -> Result<Self, CoreError> {
        let mut comments = BTreeMap::new();
        for comment in &review.read.comments {
            if comments
                .insert(
                    comment.content.id.clone(),
                    ProjectedComment::Read(comment.clone()),
                )
                .is_some()
            {
                return Err(CoreError::DuplicateComment(comment.content.id.clone()));
            }
        }
        Ok(Self {
            identities: comments.keys().cloned().collect(),
            comment_order: review
                .read
                .comments
                .iter()
                .map(|comment| comment.content.id.clone())
                .collect(),
            comments,
            file_positions: review
                .read
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.path.clone(), index))
                .collect(),
            reactions: review.read.reactions.iter().enumerate().fold(
                BTreeMap::new(),
                |mut reactions, (index, reaction)| {
                    reactions
                        .entry((
                            reaction.comment.clone(),
                            reaction.author.clone(),
                            reaction.reaction.clone(),
                        ))
                        .or_insert_with(Vec::new)
                        .push((index, reaction.clone()));
                    reactions
                },
            ),
            next_reaction: review.read.reactions.len(),
            state: ProjectedState {
                pull_request: review.read.pull_request.clone(),
                files: review.read.files.clone(),
                comments: Vec::new(),
                reactions: Vec::new(),
                reviews: review.read.reviews.clone(),
                conclusion: None,
            },
        })
    }

    fn apply(&mut self, reviewer: &UserId, change: &Change) -> Result<(), CoreError> {
        match change {
            Change::CreateComment { comment } => {
                if !self.identities.insert(comment.content.id.clone()) {
                    return Err(CoreError::DuplicateComment(comment.content.id.clone()));
                }
                if comment.content.id.0.is_empty() {
                    return Err(CoreError::Invalid("empty comment identity".into()));
                }
                if let CommentTarget::Diff(location) = &comment.target {
                    validate_location(location)?;
                }
                if let CommentTarget::Reply(parent) = &comment.target {
                    let root = self.root(parent)?;
                    if !self.comments[&root].is_diff() {
                        return Err(CoreError::Invalid("replies require a diff thread".into()));
                    }
                }
                self.comments.insert(
                    comment.content.id.clone(),
                    ProjectedComment::Draft(comment.clone()),
                );
                self.comment_order.push(comment.content.id.clone());
            }
            Change::EditComment { comment, body } => {
                self.comments
                    .get_mut(comment)
                    .ok_or_else(|| CoreError::MissingComment(comment.clone()))?
                    .content_mut()
                    .body = body.clone();
            }
            Change::DeleteComment { comment } => {
                self.comments
                    .remove(comment)
                    .ok_or_else(|| CoreError::MissingComment(comment.clone()))?;
                self.reactions.retain(|(target, _, _), _| target != comment);
            }
            Change::SetFileViewed { path, viewed, .. } => {
                let index = self
                    .file_positions
                    .get(path)
                    .ok_or_else(|| CoreError::MissingFile(path.clone()))?;
                self.state.files[*index].viewed = *viewed;
            }
            Change::SetReviewConclusion { conclusion } => {
                self.state.conclusion = Some(conclusion.clone());
            }
            Change::DismissReview { review, .. } => {
                let resource = self
                    .state
                    .reviews
                    .iter_mut()
                    .find(|resource| resource.id.as_ref() == Some(review))
                    .ok_or_else(|| CoreError::MissingReview(review.clone()))?;
                let body = match &resource.state {
                    ReadReviewState::Submitted(conclusion) => conclusion.body.clone(),
                    ReadReviewState::Inactive { body, .. } => body.clone(),
                };
                resource.state = ReadReviewState::Inactive {
                    state: InactiveReviewState::Dismissed,
                    body,
                };
            }
            Change::SetCommentReaction {
                comment,
                reaction,
                present,
            } => {
                if !self.comments.contains_key(comment) {
                    return Err(CoreError::MissingComment(comment.clone()));
                }
                if reaction.is_empty() {
                    return Err(CoreError::Invalid("empty reaction".into()));
                }
                let key = (comment.clone(), Some(reviewer.clone()), reaction.clone());
                self.reactions.remove(&key);
                if *present {
                    self.reactions.insert(
                        key,
                        vec![(
                            self.next_reaction,
                            CommentReaction {
                                comment: comment.clone(),
                                author: Some(reviewer.clone()),
                                reaction: reaction.clone(),
                            },
                        )],
                    );
                    self.next_reaction += 1;
                }
            }
        }
        Ok(())
    }

    fn apply_confirmed(
        &mut self,
        reviewer: &UserId,
        operations: &[Operation],
    ) -> Result<(), CoreError> {
        for operation in operations {
            // A fetch can already contain a write whose receipt arrived while
            // the snapshot was being read.
            if let Event::CreateComment { comment } = &operation.change
                && self.comments.contains_key(&comment.content.id)
            {
                continue;
            }
            self.apply(reviewer, &operation.change)?;
        }
        Ok(())
    }

    fn root(&self, comment: &CommentId) -> Result<CommentId, CoreError> {
        let mut current = comment;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current) {
                return Err(CoreError::Invalid("cyclic comment replies".into()));
            }
            let item = self
                .comments
                .get(current)
                .ok_or_else(|| CoreError::MissingComment(current.clone()))?;
            match item.parent() {
                Some(parent) => current = parent,
                None => return Ok(current.clone()),
            }
        }
    }

    fn validate(&self) -> Result<(), CoreError> {
        for comment in self.comments.values() {
            if comment.parent().is_some() && matches!(comment, ProjectedComment::Draft(_)) {
                // Fetched replies remain independent remote resources after a
                // root is deleted. New replies still require a live diff root.
                let root = self.root(&comment.content().id)?;
                if !self.comments[&root].is_diff() {
                    return Err(CoreError::Invalid("replies require a diff thread".into()));
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> ProjectedState {
        let mut reactions = self.reactions.into_values().flatten().collect::<Vec<_>>();
        reactions.sort_by_key(|(index, _)| *index);
        self.state.reactions = reactions
            .into_iter()
            .map(|(_, reaction)| reaction)
            .collect();
        self.state.comments = self
            .comment_order
            .into_iter()
            .filter_map(|id| self.comments.remove(&id))
            .collect();
        self.state
    }
}

fn validate_location(location: &DiffLocation) -> Result<(), CoreError> {
    if location.revision.base.is_empty()
        || location.revision.head.is_empty()
        || location.position.path.is_empty()
    {
        return Err(CoreError::Invalid(
            "diff comments require base, head and path".into(),
        ));
    }
    if let Some(range) = &location.position.range
        && (range.start.side != range.end.side || range.start.line > range.end.line)
    {
        return Err(CoreError::Invalid(
            "diff range must be ordered on one side".into(),
        ));
    }
    Ok(())
}

/// Check ordered events without requiring the final thread structure yet.
/// Deleting a parent before its replies is a valid staging sequence.
pub(crate) fn validate_events(
    review: &Review,
    batch: &Batch,
    overlay: &[Operation],
) -> Result<(), CoreError> {
    if batch.review != review.id {
        return Err(CoreError::ReviewMismatch(batch.id.clone()));
    }
    let mut projection = Projection::new(review)?;
    projection.apply_confirmed(&review.reviewer.id, overlay)?;
    for record in &batch.write.events {
        if !record.cancelled {
            projection.apply(&review.reviewer.id, &record.event)?;
        }
    }
    Ok(())
}

pub fn project(review: &Review, batch: &Batch) -> Result<ProjectedState, CoreError> {
    let eligible = batch
        .write
        .events
        .iter()
        .map(|record| record.id.clone())
        .collect();
    derive(review, batch, &BTreeSet::new(), &eligible, &[]).map(|(projected, _)| projected)
}

/// Derive remaining writes against confirmed effects with the shared planner.
pub fn prepare(
    review: &Review,
    batch: &Batch,
    receipts: &[Operation],
) -> Result<CommitState, CoreError> {
    let acknowledged = receipts
        .iter()
        .map(|operation| operation.id.clone())
        .collect();
    let eligible = batch
        .write
        .events
        .iter()
        .map(|record| record.id.clone())
        .collect();
    derive(review, batch, &acknowledged, &eligible, receipts).map(|(_, pending)| pending)
}

/// Derive the effective local intent and its projected state in one pass.
/// Receipt identities consume the coalesced operation, never its predecessors.
pub(crate) fn derive(
    review: &Review,
    batch: &Batch,
    acknowledged: &BTreeSet<String>,
    viewed_eligible: &BTreeSet<String>,
    overlay: &[Operation],
) -> Result<(ProjectedState, CommitState), CoreError> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Key {
        Comment(CommentId),
        Viewed(String),
        Reaction(CommentId, String),
        Conclusion,
        Dismiss(ReviewResourceId),
    }
    if batch.review != review.id {
        return Err(CoreError::ReviewMismatch(batch.id.clone()));
    }
    let mut intent = BTreeMap::<Key, (usize, Option<Operation>)>::new();
    let mut created = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    for (index, record) in batch.write.events.iter().enumerate() {
        if record.id.is_empty() || !event_ids.insert(&record.id) {
            return Err(CoreError::Invalid(
                "missing or duplicate event identity".into(),
            ));
        }
        let source = index + 1;
        let operation = Operation {
            id: record.id.clone(),
            change: record.event.clone(),
        };
        let key = match &record.event {
            Event::CreateComment { comment } => {
                if !created.insert(comment.content.id.clone()) {
                    return Err(CoreError::DuplicateComment(comment.content.id.clone()));
                }
                Key::Comment(comment.content.id.clone())
            }
            Event::EditComment { comment, .. } | Event::DeleteComment { comment } => {
                Key::Comment(comment.clone())
            }
            Event::SetFileViewed { path, .. } => {
                if !acknowledged.contains(&operation.id) && !viewed_eligible.contains(&record.id) {
                    continue;
                }
                Key::Viewed(path.clone())
            }
            Event::SetCommentReaction {
                comment, reaction, ..
            } => Key::Reaction(comment.clone(), reaction.clone()),
            Event::SetReviewConclusion { .. } => Key::Conclusion,
            Event::DismissReview { review, .. } => Key::Dismiss(review.clone()),
        };
        if record.cancelled {
            if matches!(key, Key::Viewed(_) | Key::Reaction(..) | Key::Conclusion) {
                intent.insert(key, (source, None));
            }
            continue;
        }
        match &record.event {
            Event::EditComment { body, .. } => {
                if let Some((_, Some(previous))) = intent.get_mut(&key)
                    && let Event::CreateComment { comment } = &mut previous.change
                {
                    comment.content.body = body.clone();
                    continue;
                }
                return Err(CoreError::Invalid(
                    "only unpublished drafts can be edited".into(),
                ));
            }
            Event::DeleteComment { .. } => {
                if intent.get(&key).is_some_and(|(_, previous)| {
                    previous.as_ref().is_some_and(|operation| {
                        matches!(operation.change, Event::CreateComment { .. })
                    })
                }) {
                    intent.insert(key, (source, None));
                    continue;
                }
                return Err(CoreError::Invalid(
                    "only unpublished drafts can be deleted".into(),
                ));
            }
            _ => {}
        }
        intent.insert(key, (source, Some(operation)));
    }
    let deleted = intent
        .iter()
        .filter_map(|(key, (_, operation))| match key {
            Key::Comment(comment) if operation.is_none() => Some(comment.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut operations = intent
        .into_values()
        .filter_map(|(source, operation)| operation.map(|operation| (source, operation)))
        .collect::<Vec<_>>();
    operations.sort_by_key(|(source, operation)| {
        (
            matches!(operation.change, Event::SetReviewConclusion { .. }),
            *source,
        )
    });
    let mut projection = Projection::new(review)?;
    projection.apply_confirmed(&review.reviewer.id, overlay)?;
    let mut pending = Vec::new();
    for (_, operation) in operations {
        if acknowledged.contains(&operation.id) {
            continue;
        }
        if let Event::SetCommentReaction { comment, .. } = &operation.change
            && (deleted.contains(comment) || !projection.comments.contains_key(comment))
        {
            continue;
        }
        let differs = match &operation.change {
            Event::CreateComment { comment } => {
                !projection.comments.contains_key(&comment.content.id)
            }
            Event::EditComment { .. } | Event::DeleteComment { .. } => {
                return Err(CoreError::Invalid("uncoalesced comment operation".into()));
            }
            Event::SetFileViewed { path, viewed, .. } => {
                let index = projection
                    .file_positions
                    .get(path)
                    .ok_or_else(|| CoreError::MissingFile(path.clone()))?;
                projection.state.files[*index].viewed != *viewed
            }
            Event::SetCommentReaction {
                comment,
                reaction,
                present,
            } => {
                projection.reactions.contains_key(&(
                    comment.clone(),
                    Some(review.reviewer.id.clone()),
                    reaction.clone(),
                )) != *present
            }
            Event::SetReviewConclusion { .. } => true,
            Event::DismissReview { review, .. } => projection.state.reviews.iter().any(|item| {
                item.id.as_ref() == Some(review)
                    && !matches!(
                        item.state,
                        ReadReviewState::Inactive {
                            state: InactiveReviewState::Dismissed,
                            ..
                        }
                    )
            }),
        };
        if differs {
            projection.apply(&review.reviewer.id, &operation.change)?;
            pending.push(operation);
        }
    }
    projection.validate()?;
    Ok((
        projection.finish(),
        CommitState {
            operations: pending,
        },
    ))
}
