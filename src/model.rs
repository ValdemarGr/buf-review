use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// A local domain identity. Backend adapters map provider identities separately.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PullRequestId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewId(pub String);

/// Globally unique within the local store, independently of the review ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommentId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(pub String);

/// Identifies a provider review resource, independently of a local ReviewId.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewResourceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRevision {
    pub base: String,
    pub head: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Base,
    Head,
}

/// A one-based line on either side of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub side: DiffSide,
    pub line: NonZeroU32,
}

/// Both endpoints are inclusive. A single-line comment has equal endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRange {
    pub start: DiffLine,
    pub end: DiffLine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentPosition {
    pub path: String,
    pub range: Option<DiffRange>,
}

/// A write anchor records the exact local comparison. A missing range targets
/// the whole file. Read anchors have their own representation below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLocation {
    pub revision: DiffRevision,
    #[serde(flatten)]
    pub position: CommentPosition,
}

/// Old snapshots retain a known comparison; imports can supply only a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReadCommentRevision {
    Comparison(DiffRevision),
    Commit { head: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentSubject {
    Line,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCommentAnchor {
    pub revision: ReadCommentRevision,
    #[serde(flatten)]
    pub position: CommentPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadDiffLocation {
    #[serde(flatten)]
    pub original: ReadCommentAnchor,
    pub current: Option<ReadCommentAnchor>,
    /// None means the provider did not supply a subject classification.
    pub subject: Option<CommentSubject>,
    pub diff_hunk: Option<String>,
    pub outdated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
pub enum CommentTarget<Location = DiffLocation> {
    PullRequest,
    Diff(Location),
    /// The same identity type is used for fetched comments and local drafts.
    Reply(CommentId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentContent {
    pub id: CommentId,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftComment {
    #[serde(flatten)]
    pub content: CommentContent,
    pub author: User,
    pub target: CommentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadComment {
    #[serde(flatten)]
    pub content: CommentContent,
    pub author: Option<User>,
    pub target: CommentTarget<ReadDiffLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestStatus {
    Local,
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSource {
    pub repository: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchPair {
    pub source: BranchSource,
    pub sink: BranchSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PullRequestSource {
    LocalIntent {
        branches: BranchPair,
    },
    Remote {
        locator: String,
        branches: Option<BranchPair>,
    },
}

/// A PR has a local identity before its provider resource exists. Review
/// snapshots retain their own immutable metadata and comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestObject {
    pub id: PullRequestId,
    pub revision: u64,
    pub source: PullRequestSource,
    pub title: String,
    pub body: String,
}

/// Frozen creation input survives interruption independently of review writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestResolution {
    pub branches: BranchPair,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: PullRequestId,
    pub title: String,
    pub body: String,
    pub author: Option<User>,
    pub status: PullRequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BranchSource>,
    pub revision: DiffRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStats {
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub change: FileChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<FileStats>,
    pub viewed: bool,
}

/// The reaction is represented by its text or emoji, not a provider's enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentReaction {
    pub comment: CommentId,
    pub author: Option<UserId>,
    pub reaction: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConclusion {
    pub outcome: ReviewOutcome,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InactiveReviewState {
    Pending,
    Dismissed,
}

/// Read state preserves reviews whose active outcome is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReadReviewState {
    Submitted(ReviewConclusion),
    Inactive {
        state: InactiveReviewState,
        body: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadReview {
    /// Older snapshots did not retain an identity for review resources.
    pub id: Option<ReviewResourceId>,
    pub author: Option<User>,
    #[serde(alias = "conclusion")]
    pub state: ReadReviewState,
}

/// The fetched baseline for one local review. Staging never changes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    pub pull_request: PullRequest,
    pub files: Vec<ChangedFile>,
    pub comments: Vec<ReadComment>,
    pub reactions: Vec<CommentReaction>,
    #[serde(alias = "conclusions")]
    pub reviews: Vec<ReadReview>,
}

/// Events are retained in order, including changes that will cancel at commit.
/// Comment operations use the same IDs regardless of whether the comment was
/// fetched or created locally. Recreating a deleted comment uses a new ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    CreateComment {
        comment: DraftComment,
    },
    EditComment {
        comment: CommentId,
        body: String,
    },
    DeleteComment {
        comment: CommentId,
    },
    SetFileViewed {
        path: String,
        viewed: bool,
    },
    SetReviewConclusion {
        conclusion: ReviewConclusion,
    },
    DismissReview {
        review: ReviewResourceId,
        /// Providers may require a reason; old logs remain readable.
        #[serde(default)]
        message: Option<String>,
    },
    SetCommentReaction {
        comment: CommentId,
        reaction: String,
        present: bool,
    },
}

/// An event log, not a collapsed set of backend writes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteState {
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub reviewer: User,
    pub read: ReadState,
}

/// An editable local event log. Submission does not complete or exhaust a batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: BatchId,
    #[serde(default)]
    pub revision: BatchRevision,
    pub review: ReviewId,
    pub write: WriteState,
}

/// Internal log version. Legacy operation namespaces remain unchanged on import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRevision {
    pub number: u64,
    pub namespace: Option<String>,
}
