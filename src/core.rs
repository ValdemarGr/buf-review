use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
    fn content(&self) -> &CommentContent {
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

struct Projection {
    state: ProjectedState,
    comments: BTreeMap<CommentId, ProjectedComment>,
    identities: BTreeSet<CommentId>,
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
            comments,
            state: ProjectedState {
                pull_request: review.read.pull_request.clone(),
                files: review.read.files.clone(),
                comments: Vec::new(),
                reactions: review.read.reactions.clone(),
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
                self.state
                    .reactions
                    .retain(|reaction| &reaction.comment != comment);
            }
            Change::SetFileViewed { path, viewed } => {
                self.state
                    .files
                    .iter_mut()
                    .find(|file| &file.path == path)
                    .ok_or_else(|| CoreError::MissingFile(path.clone()))?
                    .viewed = *viewed;
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
                self.state.reactions.retain(|item| {
                    !(item.comment == *comment
                        && item.author.as_ref() == Some(reviewer)
                        && item.reaction == *reaction)
                });
                if *present {
                    self.state.reactions.push(CommentReaction {
                        comment: comment.clone(),
                        author: Some(reviewer.clone()),
                        reaction: reaction.clone(),
                    });
                }
            }
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

    fn depth(&self, comment: &CommentId) -> usize {
        let mut current = comment;
        let mut seen = BTreeSet::new();
        let mut depth = 0;
        while seen.insert(current) {
            let Some(parent) = self
                .comments
                .get(current)
                .and_then(ProjectedComment::parent)
            else {
                break;
            };
            current = parent;
            depth += 1;
        }
        depth
    }

    fn validate(&self) -> Result<(), CoreError> {
        for comment in self.comments.values() {
            if let Some(parent) = comment.parent() {
                // Preserve incomplete fetched ancestry, but never introduce a
                // dangling thread by deleting a parent known to this batch.
                if !self.comments.contains_key(parent) && self.identities.contains(parent) {
                    return Err(CoreError::MissingComment(parent.clone()));
                }
                if matches!(comment, ProjectedComment::Draft(_)) {
                    let root = self.root(&comment.content().id)?;
                    if !self.comments[&root].is_diff() {
                        return Err(CoreError::Invalid("replies require a diff thread".into()));
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> ProjectedState {
        self.state.comments = self.comments.into_values().collect();
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

fn desired(review: &Review, batch: &Batch) -> Result<Projection, CoreError> {
    if batch.review != review.id {
        return Err(CoreError::ReviewMismatch(batch.id.clone()));
    }
    let mut projection = Projection::new(review)?;
    for event in &batch.write.events {
        projection.apply(&review.reviewer.id, event)?;
    }
    projection.validate()?;
    Ok(projection)
}

pub fn project(review: &Review, batch: &Batch) -> Result<ProjectedState, CoreError> {
    desired(review, batch).map(Projection::finish)
}

/// Compare intended state with successfully delivered writes. A failed or
/// cancelled operation is absent from `completed`; event history stays intact.
pub fn prepare(
    review: &Review,
    batch: &Batch,
    completed: &[Operation],
) -> Result<CommitState, CoreError> {
    let desired = desired(review, batch)?;
    let mut delivered = Projection::new(review)?;
    for operation in completed {
        delivered.apply(&review.reviewer.id, &operation.change)?;
    }
    delivered.validate()?;
    let mut operations = Vec::new();
    let mut create = Vec::new();
    for (id, comment) in &desired.comments {
        match delivered.comments.get(id) {
            None => {
                let ProjectedComment::Draft(comment) = comment else {
                    return Err(CoreError::Invalid(
                        "a deleted fetched comment cannot be recreated".into(),
                    ));
                };
                create.push((desired.depth(id), comment.clone()));
            }
            Some(previous) if previous.content().body != comment.content().body => {
                operations.push(operation(
                    batch,
                    Change::EditComment {
                        comment: id.clone(),
                        body: comment.content().body.clone(),
                    },
                )?);
            }
            _ => {}
        }
    }
    create.sort_by(|(left_depth, left), (right_depth, right)| {
        left_depth
            .cmp(right_depth)
            .then_with(|| left.content.id.cmp(&right.content.id))
    });
    let mut creates = create
        .into_iter()
        .map(|(_, comment)| operation(batch, Change::CreateComment { comment }))
        .collect::<Result<Vec<_>, _>>()?;
    creates.append(&mut operations);
    operations = creates;
    let mut deletes = delivered
        .comments
        .keys()
        .filter(|id| !desired.comments.contains_key(*id))
        .map(|id| (delivered.depth(id), id.clone()))
        .collect::<Vec<_>>();
    deletes.sort_by(|(left_depth, left), (right_depth, right)| {
        right_depth.cmp(left_depth).then_with(|| left.cmp(right))
    });
    for (_, comment) in deletes {
        operations.push(operation(batch, Change::DeleteComment { comment })?);
    }
    for file in &desired.state.files {
        if delivered
            .state
            .files
            .iter()
            .find(|previous| previous.path == file.path)
            .is_some_and(|previous| previous.viewed != file.viewed)
        {
            operations.push(operation(
                batch,
                Change::SetFileViewed {
                    path: file.path.clone(),
                    viewed: file.viewed,
                },
            )?);
        }
    }
    let reactions = |projection: &Projection| {
        projection
            .state
            .reactions
            .iter()
            .filter(|reaction| {
                reaction.author.as_ref() == Some(&review.reviewer.id)
                    && desired.comments.contains_key(&reaction.comment)
            })
            .map(|reaction| (reaction.comment.clone(), reaction.reaction.clone()))
            .collect::<BTreeSet<_>>()
    };
    let wanted = reactions(&desired);
    let sent = reactions(&delivered);
    for (comment, reaction) in wanted.symmetric_difference(&sent) {
        operations.push(operation(
            batch,
            Change::SetCommentReaction {
                comment: comment.clone(),
                reaction: reaction.clone(),
                present: wanted.contains(&(comment.clone(), reaction.clone())),
            },
        )?);
    }
    for resource in &desired.state.reviews {
        if matches!(
            resource.state,
            ReadReviewState::Inactive {
                state: InactiveReviewState::Dismissed,
                ..
            }
        ) && let Some(id) = &resource.id
        {
            let previous = delivered
                .state
                .reviews
                .iter()
                .find(|previous| previous.id.as_ref() == Some(id));
            if previous.is_some_and(|previous| previous.state != resource.state) {
                let message = batch
                    .write
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match event {
                        Event::DismissReview { review, message } if review == id => {
                            Some(message.clone())
                        }
                        _ => None,
                    })
                    .flatten();
                operations.push(operation(
                    batch,
                    Change::DismissReview {
                        review: id.clone(),
                        message,
                    },
                )?);
            }
        }
    }
    if desired.state.conclusion != delivered.state.conclusion
        && let Some(conclusion) = desired.state.conclusion
    {
        operations.push(operation(
            batch,
            Change::SetReviewConclusion { conclusion },
        )?);
    }
    Ok(CommitState { operations })
}

fn operation(batch: &Batch, change: Change) -> Result<Operation, CoreError> {
    let source = batch
        .write
        .events
        .iter()
        .enumerate()
        .rev()
        .find(|(_, event)| match (&change, event) {
            (Change::CreateComment { comment }, Event::CreateComment { comment: candidate }) => {
                comment.content.id == candidate.content.id
            }
            (
                Change::EditComment { comment, .. },
                Event::EditComment {
                    comment: candidate, ..
                },
            )
            | (Change::DeleteComment { comment }, Event::DeleteComment { comment: candidate }) => {
                comment == candidate
            }
            (
                Change::SetFileViewed { path, .. },
                Event::SetFileViewed {
                    path: candidate, ..
                },
            ) => path == candidate,
            (Change::SetReviewConclusion { .. }, Event::SetReviewConclusion { .. }) => true,
            (
                Change::DismissReview { review, .. },
                Event::DismissReview {
                    review: candidate, ..
                },
            ) => review == candidate,
            (
                Change::SetCommentReaction {
                    comment, reaction, ..
                },
                Event::SetCommentReaction {
                    comment: candidate,
                    reaction: candidate_reaction,
                    ..
                },
            ) => comment == candidate && reaction == candidate_reaction,
            _ => false,
        })
        .map(|(index, _)| index + 1)
        .ok_or_else(|| CoreError::Invalid("processed write has no source event".into()))?;
    let kind = match &change {
        Change::CreateComment { .. } => "create_comment",
        Change::EditComment { .. } => "edit_comment",
        Change::DeleteComment { .. } => "delete_comment",
        Change::SetFileViewed { .. } => "set_file_viewed",
        Change::SetReviewConclusion { .. } => "set_review_conclusion",
        Change::DismissReview { .. } => "dismiss_review",
        Change::SetCommentReaction { .. } => "set_comment_reaction",
    };
    let name = serde_json::to_vec(&(&batch.id, source, kind))
        .expect("strings and event sequence serialize");
    Ok(Operation {
        id: Uuid::new_v5(&Uuid::NAMESPACE_URL, &name).to_string(),
        change,
    })
}
