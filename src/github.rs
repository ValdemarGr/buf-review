use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    process::Command,
};

use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource},
};

mod publish;
pub use publish::{PublishError, PublishResult, preflight, publish};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("expected https://github.com/OWNER/REPO/pull/NUMBER")]
    InvalidUrl,
    #[error("could not run gh: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid GitHub response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("gh failed: {0}")]
    Gh(String),
    #[error("GitHub snapshot is incomplete or inconsistent: {0}")]
    Response(String),
}

pub struct FetchedReview {
    pub reviewer: User,
    pub read: ReadState,
    pub backend: ReviewBackend,
}

/// Fetches provider data without opening or modifying the local store.
pub fn fetch(url: &Url) -> Result<FetchedReview, FetchError> {
    let address = Address::from_url(url)?;
    let endpoint = format!(
        "repos/{}/{}/pulls/{}",
        address.owner, address.repo, address.number
    );
    let pull: RestPullRequest = api(&endpoint, &["--method", "GET"])?;
    if pull.changed_files > 3000 {
        return Err(FetchError::Response(
            "GitHub's files endpoint is limited to 3000 files".into(),
        ));
    }
    let revision = DiffRevision {
        base: pull.base.sha.clone(),
        head: pull.head.sha.clone(),
    };
    let reviewer: RestUser = api("user", &["--method", "GET"])?;
    let files: Vec<RestFile> = list(&format!("{endpoint}/files?per_page=100"))?;
    let viewed = viewed_files(&address, &revision)?;
    let paths: BTreeSet<_> = files.iter().map(|file| &file.filename).collect();
    if files.len() != pull.changed_files
        || paths.len() != files.len()
        || !paths.into_iter().eq(viewed.keys())
    {
        return Err(FetchError::Response(
            "changed file counts or viewed-file paths disagree".into(),
        ));
    }
    let files = files
        .into_iter()
        .map(|file| {
            Ok(ChangedFile {
                viewed: viewed[&file.filename],
                path: file.filename,
                previous_path: file.previous_filename,
                change: match file.status.as_str() {
                    "added" => FileChange::Added,
                    "modified" => FileChange::Modified,
                    "removed" => FileChange::Deleted,
                    "renamed" => FileChange::Renamed,
                    "copied" => FileChange::Copied,
                    "changed" => FileChange::TypeChanged,
                    "unchanged" => FileChange::Unchanged,
                    state => {
                        return Err(FetchError::Response(format!(
                            "unknown file change: {state}"
                        )));
                    }
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let issue_comments: Vec<RestComment> = list(&format!(
        "repos/{}/{}/issues/{}/comments?per_page=100",
        address.owner, address.repo, address.number
    ))?;
    let review_comments: Vec<RestReviewComment> =
        list(&format!("{endpoint}/comments?per_page=100"))?;
    let reviews: Vec<RestReview> = list(&format!("{endpoint}/reviews?per_page=100"))?;
    let mut identities = Identities::default();
    let mut comments = Vec::new();
    let mut reactions = Vec::new();
    for comment in issue_comments {
        let id = CommentId(identities.id("issue_comment", comment.id));
        read_reactions(
            &format!(
                "repos/{}/{}/issues/comments/{}/reactions?per_page=100",
                address.owner, address.repo, comment.id
            ),
            &id,
            &mut identities,
            &mut reactions,
        )?;
        comments.push(ReadComment {
            content: CommentContent {
                id,
                body: comment.body,
            },
            author: comment.user.map(|user| identities.user(user)),
            target: CommentTarget::PullRequest,
        });
    }
    for comment in review_comments {
        let id = CommentId(identities.id("review_comment", comment.comment.id));
        read_reactions(
            &format!(
                "repos/{}/{}/pulls/comments/{}/reactions?per_page=100",
                address.owner, address.repo, comment.comment.id
            ),
            &id,
            &mut identities,
            &mut reactions,
        )?;
        let target = match comment.in_reply_to_id {
            Some(parent) => {
                CommentTarget::Reply(CommentId(identities.id("review_comment", parent)))
            }
            None => {
                let original = ReadCommentAnchor {
                    revision: ReadCommentRevision::Commit {
                        head: comment.original_commit_id,
                    },
                    position: CommentPosition {
                        path: comment.path.clone(),
                        range: read_range(
                            comment.original_line,
                            comment.original_start_line,
                            comment.side,
                            comment.start_side,
                        ),
                    },
                };
                let current_range = read_range(
                    comment.line,
                    comment.start_line,
                    comment.side,
                    comment.start_side,
                );
                let subject = comment.subject_type.or_else(|| {
                    (original.position.range.is_some() || current_range.is_some())
                        .then_some(CommentSubject::Line)
                });
                let current = (current_range.is_some() || subject == Some(CommentSubject::File))
                    .then_some(ReadCommentAnchor {
                        revision: ReadCommentRevision::Commit {
                            head: comment.commit_id,
                        },
                        position: CommentPosition {
                            path: comment.path,
                            range: current_range,
                        },
                    });
                CommentTarget::Diff(ReadDiffLocation {
                    original,
                    current,
                    subject,
                    diff_hunk: comment.diff_hunk,
                    // REST does not provide an explicit outdated flag.
                    outdated: None,
                })
            }
        };
        comments.push(ReadComment {
            content: CommentContent {
                id,
                body: comment.comment.body,
            },
            author: comment.comment.user.map(|user| identities.user(user)),
            target,
        });
    }
    let reviews = reviews
        .into_iter()
        .map(|review| {
            let state = match review.state.as_str() {
                "APPROVED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Approve,
                    body: review.body,
                }),
                "CHANGES_REQUESTED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::RequestChanges,
                    body: review.body,
                }),
                "COMMENTED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Comment,
                    body: review.body,
                }),
                "DISMISSED" => ReadReviewState::Inactive {
                    state: InactiveReviewState::Dismissed,
                    body: review.body,
                },
                "PENDING" => ReadReviewState::Inactive {
                    state: InactiveReviewState::Pending,
                    body: review.body,
                },
                state => {
                    return Err(FetchError::Response(format!(
                        "unknown review state: {state}"
                    )));
                }
            };
            Ok(ReadReview {
                id: Some(ReviewResourceId(identities.id("review", review.id))),
                author: review.user.map(|user| identities.user(user)),
                state,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let latest: RestPullRequest = api(&endpoint, &["--method", "GET"])?;
    if latest.base.sha != revision.base
        || latest.head.sha != revision.head
        || latest.changed_files != pull.changed_files
    {
        return Err(FetchError::Response(
            "PR revisions changed during fetch; fetch again".into(),
        ));
    }
    let status = match (pull.state.as_str(), pull.merged) {
        ("closed", true) => PullRequestStatus::Merged,
        ("closed", false) => PullRequestStatus::Closed,
        ("open", false) => PullRequestStatus::Open,
        _ => {
            return Err(FetchError::Response(format!(
                "unknown PR state: {}",
                pull.state
            )));
        }
    };
    let reviewer = identities.user(reviewer);
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId(identities.id("pull_request", pull.id)),
            title: pull.title,
            body: pull.body.unwrap_or_default(),
            author: pull.user.map(|user| identities.user(user)),
            status,
            revision,
        },
        files,
        comments,
        reactions,
        reviews,
    };
    Ok(FetchedReview {
        reviewer,
        read,
        backend: ReviewBackend {
            source: ReviewSource {
                backend: "github".into(),
                locator: format!(
                    "https://github.com/{}/{}/pull/{}",
                    address.owner, address.repo, address.number
                ),
            },
            mappings: identities.0.into_values().collect(),
        },
    })
}

struct Address {
    owner: String,
    repo: String,
    number: i32,
}

impl Address {
    fn from_url(url: &Url) -> Result<Self, FetchError> {
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(FetchError::InvalidUrl);
        }
        let parts: Vec<_> = url
            .path()
            .trim_end_matches('/')
            .split('/')
            .skip(1)
            .collect();
        let [owner, repo, "pull", number] = parts.as_slice() else {
            return Err(FetchError::InvalidUrl);
        };
        if owner.is_empty() || repo.is_empty() {
            return Err(FetchError::InvalidUrl);
        }
        let number = number
            .parse::<i32>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or(FetchError::InvalidUrl)?;
        Ok(Self {
            owner: (*owner).into(),
            repo: (*repo).into(),
            number,
        })
    }
}

fn api<T: DeserializeOwned>(endpoint: &str, args: &[&str]) -> Result<T, FetchError> {
    let output = Command::new("gh")
        .args(["api", endpoint, "--hostname", "github.com"])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(FetchError::Gh(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn list<T: DeserializeOwned>(endpoint: &str) -> Result<Vec<T>, FetchError> {
    let pages: Vec<Vec<T>> = api(endpoint, &["--method", "GET", "--paginate", "--slurp"])?;
    Ok(pages.into_iter().flatten().collect())
}

fn viewed_files(
    address: &Address,
    revision: &DiffRevision,
) -> Result<BTreeMap<String, bool>, FetchError> {
    let query = format!("query={}", include_str!("github/viewed_files.graphql"));
    let owner = format!("owner={}", address.owner);
    let name = format!("name={}", address.repo);
    let number = format!("number={}", address.number);
    let pages: Vec<ViewedPage> = api(
        "graphql",
        &[
            "--paginate",
            "--slurp",
            "-f",
            &query,
            "-f",
            &owner,
            "-f",
            &name,
            "-F",
            &number,
        ],
    )?;
    let mut viewed = BTreeMap::new();
    let mut total_count = None;
    for page in pages {
        if !page.errors.is_empty() {
            return Err(FetchError::Response(
                page.errors
                    .into_iter()
                    .map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        let pull = page
            .data
            .and_then(|data| data.repository)
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| {
                FetchError::Response("GraphQL did not return the pull request".into())
            })?;
        if pull.base_ref_oid != revision.base || pull.head_ref_oid != revision.head {
            return Err(FetchError::Response(
                "PR revisions changed during viewed-file fetch".into(),
            ));
        }
        if total_count.is_some_and(|count| count != pull.files.total_count) {
            return Err(FetchError::Response(
                "viewed-file count changed during pagination".into(),
            ));
        }
        total_count = Some(pull.files.total_count);
        for node in pull.files.nodes {
            let file = node.ok_or_else(|| {
                FetchError::Response("GraphQL returned an unavailable file".into())
            })?;
            let is_viewed = match file.viewer_viewed_state.as_str() {
                "VIEWED" => true,
                "UNVIEWED" | "DISMISSED" => false,
                state => {
                    return Err(FetchError::Response(format!(
                        "unknown viewed state: {state}"
                    )));
                }
            };
            if viewed.insert(file.path, is_viewed).is_some() {
                return Err(FetchError::Response(
                    "duplicate path in viewed-file pages".into(),
                ));
            }
        }
    }
    if total_count != Some(viewed.len()) {
        return Err(FetchError::Response(
            "viewed-file pagination was incomplete".into(),
        ));
    }
    Ok(viewed)
}

fn read_range(
    line: Option<NonZeroU32>,
    start_line: Option<NonZeroU32>,
    side: Option<RestSide>,
    start_side: Option<RestSide>,
) -> Option<DiffRange> {
    let (line, side) = line.zip(side)?;
    let end = DiffLine {
        line,
        side: side.into(),
    };
    let start = match start_line {
        Some(line) => DiffLine {
            line,
            side: start_side?.into(),
        },
        None => end.clone(),
    };
    Some(DiffRange { start, end })
}

fn read_reactions(
    endpoint: &str,
    comment: &CommentId,
    identities: &mut Identities,
    reactions: &mut Vec<CommentReaction>,
) -> Result<(), FetchError> {
    let values: Vec<RestReaction> = list(endpoint)?;
    for value in values {
        let reaction = match value.content.as_str() {
            "+1" => "👍",
            "-1" => "👎",
            "laugh" => "😄",
            "hooray" => "🎉",
            "confused" => "😕",
            "heart" => "❤️",
            "rocket" => "🚀",
            "eyes" => "👀",
            content => return Err(FetchError::Response(format!("unknown reaction: {content}"))),
        };
        reactions.push(CommentReaction {
            comment: comment.clone(),
            author: value.user.map(|user| identities.user(user).id),
            reaction: reaction.into(),
        });
    }
    Ok(())
}

#[derive(Default)]
struct Identities(BTreeMap<(String, u64), IdMapping>);

impl Identities {
    fn id(&mut self, resource: &str, remote_id: u64) -> String {
        self.0
            .entry((resource.into(), remote_id))
            .or_insert_with(|| IdMapping {
                resource: resource.into(),
                core_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("https://github.com/{resource}/{remote_id}").as_bytes(),
                )
                .to_string(),
                remote_id: remote_id.to_string(),
            })
            .core_id
            .clone()
    }

    fn user(&mut self, user: RestUser) -> User {
        User {
            id: UserId(self.id("user", user.id)),
            name: user.login,
        }
    }
}

#[derive(Deserialize)]
struct RestUser {
    id: u64,
    login: String,
}

#[derive(Deserialize)]
struct RestRef {
    sha: String,
}

#[derive(Deserialize)]
struct RestPullRequest {
    id: u64,
    title: String,
    body: Option<String>,
    user: Option<RestUser>,
    state: String,
    merged: bool,
    base: RestRef,
    head: RestRef,
    changed_files: usize,
}

#[derive(Deserialize)]
struct RestFile {
    filename: String,
    previous_filename: Option<String>,
    status: String,
}

#[derive(Deserialize)]
struct RestComment {
    id: u64,
    user: Option<RestUser>,
    body: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
enum RestSide {
    Left,
    Right,
}

impl From<RestSide> for DiffSide {
    fn from(side: RestSide) -> Self {
        match side {
            RestSide::Left => Self::Base,
            RestSide::Right => Self::Head,
        }
    }
}

#[derive(Deserialize)]
struct RestReviewComment {
    #[serde(flatten)]
    comment: RestComment,
    in_reply_to_id: Option<u64>,
    commit_id: String,
    original_commit_id: String,
    path: String,
    diff_hunk: Option<String>,
    line: Option<NonZeroU32>,
    original_line: Option<NonZeroU32>,
    start_line: Option<NonZeroU32>,
    original_start_line: Option<NonZeroU32>,
    side: Option<RestSide>,
    start_side: Option<RestSide>,
    subject_type: Option<CommentSubject>,
}

#[derive(Deserialize)]
struct RestReview {
    id: u64,
    user: Option<RestUser>,
    body: Option<String>,
    state: String,
}

#[derive(Deserialize)]
struct RestReaction {
    user: Option<RestUser>,
    content: String,
}

#[derive(Deserialize)]
struct ViewedPage {
    data: Option<ViewedData>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Deserialize)]
struct ViewedData {
    repository: Option<ViewedRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewedRepository {
    pull_request: Option<ViewedPullRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewedPullRequest {
    base_ref_oid: String,
    head_ref_oid: String,
    files: ViewedFiles,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewedFiles {
    total_count: usize,
    nodes: Vec<Option<ViewedFile>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewedFile {
    path: String,
    viewer_viewed_state: String,
}
