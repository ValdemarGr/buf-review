use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    process::Stdio,
};

use serde_json::Value;

use super::*;
use crate::core::{Change, CommitState};

#[derive(Debug, Error)]
pub enum PublishError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("cannot publish to GitHub: {0}")]
    Unsupported(String),
}

pub struct PublishResult {
    pub mappings: Vec<IdMapping>,
    pub response: Value,
}

#[derive(Clone)]
enum Target {
    PullRequest,
    Diff,
    Reply(CommentId),
}

impl<L> From<&CommentTarget<L>> for Target {
    fn from(target: &CommentTarget<L>) -> Self {
        match target {
            CommentTarget::PullRequest => Self::PullRequest,
            CommentTarget::Diff(_) => Self::Diff,
            CommentTarget::Reply(parent) => Self::Reply(parent.clone()),
        }
    }
}

/// Validates the entire processed plan, then verifies the authenticated reviewer
/// before the first provider write. Empty plans do not contact GitHub.
/// Provider permissions and whether an anchor occurs in GitHub's diff remain
/// server-side checks; preflight never substitutes a different anchor.
pub fn preflight(
    review: &Review,
    backend: &ReviewBackend,
    commit: &CommitState,
) -> Result<(), PublishError> {
    address(backend)?;
    let mut targets: BTreeMap<_, _> = review
        .read
        .comments
        .iter()
        .map(|comment| (comment.content.id.clone(), Target::from(&comment.target)))
        .collect();
    // Successfully published drafts are absent from the immutable snapshot.
    // Their provider kind establishes a native diff thread. Publish resolves
    // its actual root through GitHub when a new reply targets that resource.
    for mapping in &backend.mappings {
        if mapping.resource == "review_comment" {
            targets
                .entry(CommentId(mapping.core_id.clone()))
                .or_insert(Target::Diff);
        }
    }
    let mut available: BTreeSet<_> = backend
        .mappings
        .iter()
        .filter(|mapping| {
            matches!(
                mapping.resource.as_str(),
                "issue_comment" | "review_comment" | "review_summary"
            )
        })
        .map(|mapping| {
            remote(backend, &mapping.resource, &mapping.core_id)?;
            Ok(CommentId(mapping.core_id.clone()))
        })
        .collect::<Result<_, PublishError>>()?;
    for operation in &commit.operations {
        match &operation.change {
            Change::CreateComment { comment } => {
                nonempty(&comment.content.body, "comment body")?;
                if available.contains(&comment.content.id) {
                    return Err(unsupported("comment already has a remote mapping"));
                }
                match &comment.target {
                    CommentTarget::Reply(parent) => {
                        let root = root_comment(&targets, parent)?;
                        if !available.contains(root) || !available.contains(parent) {
                            return Err(unsupported(
                                "reply target has not been published or created earlier in this plan",
                            ));
                        }
                    }
                    CommentTarget::Diff(location) => {
                        nonempty(&location.position.path, "comment path")?;
                        nonempty(&location.revision.base, "comparison base")?;
                        nonempty(&location.revision.head, "comparison head")?;
                    }
                    CommentTarget::PullRequest => {}
                }
                targets.insert(comment.content.id.clone(), Target::from(&comment.target));
                available.insert(comment.content.id.clone());
            }
            Change::EditComment { comment, body } => {
                nonempty(body, "comment body")?;
                require_comment(&available, comment)?;
            }
            Change::DeleteComment { comment } => {
                require_comment(&available, comment)?;
                if comment_remote(backend, comment)?.0 == "review_summary" {
                    return Err(unsupported("submitted review summaries cannot be deleted"));
                }
                available.remove(comment);
            }
            Change::SetCommentReaction {
                comment,
                reaction,
                present,
            } => {
                require_comment(&available, comment)?;
                reaction_content(reaction)?;
                if backend.mappings.iter().any(|mapping| {
                    mapping.resource == "review_summary" && mapping.core_id == comment.0
                }) && !backend.mappings.iter().any(|mapping| {
                    mapping.resource == "review_node"
                        && mapping.core_id == comment.0
                        && !mapping.remote_id.is_empty()
                }) {
                    return Err(unsupported(
                        "review summary lacks its provider node identity",
                    ));
                }
                if !present {
                    remote(backend, "user", &review.reviewer.id.0)?;
                }
            }
            Change::SetFileViewed { path, .. } => {
                if !review.read.files.iter().any(|file| file.path == *path) {
                    return Err(unsupported("viewed-file path is absent from fetched files"));
                }
            }
            Change::SetReviewConclusion { conclusion } => {
                if conclusion.outcome == ReviewOutcome::Comment {
                    nonempty(
                        conclusion.body.as_deref().unwrap_or_default(),
                        "COMMENT review body",
                    )?;
                }
            }
            Change::DismissReview { review, message } => {
                remote(backend, "review", &review.0)?;
                nonempty(
                    message.as_deref().unwrap_or_default(),
                    "review dismissal reason",
                )?;
            }
        }
    }
    if !commit.operations.is_empty() {
        let reviewer = remote(backend, "user", &review.reviewer.id.0)?;
        let user: RestUser = api("user", &["--method", "GET"])?;
        if user.id != reviewer {
            return Err(unsupported(
                "authenticated user differs from the fetched reviewer",
            ));
        }
    }
    Ok(())
}

fn address(backend: &ReviewBackend) -> Result<Address, PublishError> {
    if backend.source.backend != "github" {
        return Err(unsupported("review backend is not GitHub"));
    }
    let url = Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?;
    Ok(Address::from_url(&url)?)
}

fn unsupported(message: impl Into<String>) -> PublishError {
    PublishError::Unsupported(message.into())
}

fn nonempty(value: &str, field: &str) -> Result<(), PublishError> {
    if value.trim().is_empty() {
        Err(unsupported(format!("{field} must be nonempty")))
    } else {
        Ok(())
    }
}

fn require_comment(
    available: &BTreeSet<CommentId>,
    comment: &CommentId,
) -> Result<(), PublishError> {
    if available.contains(comment) {
        Ok(())
    } else {
        Err(unsupported(format!(
            "comment {} has no available remote resource",
            comment.0
        )))
    }
}

fn root_comment<'a>(
    targets: &'a BTreeMap<CommentId, Target>,
    mut comment: &'a CommentId,
) -> Result<&'a CommentId, PublishError> {
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(comment) {
            return Err(unsupported("reply ancestry contains a cycle"));
        }
        match targets.get(comment) {
            Some(Target::Diff) => return Ok(comment),
            Some(Target::Reply(parent)) => comment = parent,
            Some(Target::PullRequest) => return Err(unsupported("replies require a diff comment")),
            None => {
                return Err(unsupported(format!(
                    "reply ancestry unavailable for {}",
                    comment.0
                )));
            }
        }
    }
}

fn remote(backend: &ReviewBackend, resource: &str, core_id: &str) -> Result<u64, PublishError> {
    backend
        .mappings
        .iter()
        .find(|mapping| mapping.resource == resource && mapping.core_id == core_id)
        .and_then(|mapping| mapping.remote_id.parse::<u64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            unsupported(format!(
                "missing or invalid {resource} mapping for {core_id}"
            ))
        })
}

fn comment_remote<'a>(
    backend: &'a ReviewBackend,
    comment: &CommentId,
) -> Result<(&'a str, u64), PublishError> {
    let mapping = backend
        .mappings
        .iter()
        .find(|mapping| {
            mapping.core_id == comment.0
                && matches!(
                    mapping.resource.as_str(),
                    "issue_comment" | "review_comment" | "review_summary"
                )
        })
        .ok_or_else(|| unsupported(format!("missing comment mapping for {}", comment.0)))?;
    Ok((
        &mapping.resource,
        remote(backend, &mapping.resource, &comment.0)?,
    ))
}

fn reaction_content(value: &str) -> Result<&'static str, PublishError> {
    match value {
        "👍" => Ok("+1"),
        "👎" => Ok("-1"),
        "😄" => Ok("laugh"),
        "🎉" => Ok("hooray"),
        "😕" => Ok("confused"),
        "❤️" => Ok("heart"),
        "🚀" => Ok("rocket"),
        "👀" => Ok("eyes"),
        _ => Err(unsupported(format!("unsupported reaction: {value}"))),
    }
}

pub(super) fn request(endpoint: &str, method: &str, body: &Value) -> Result<Value, PublishError> {
    let bytes = serde_json::to_vec(body).map_err(FetchError::from)?;
    let mut child = Command::new("gh")
        .args([
            "api",
            endpoint,
            "--hostname",
            "github.com",
            "--method",
            method,
        ])
        .args(["--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(FetchError::from)?;
    let written = child.stdin.take().expect("piped stdin").write_all(&bytes);
    let output = child.wait_with_output().map_err(FetchError::from)?;
    written.map_err(FetchError::from)?;
    if !output.status.success() {
        let mut details = vec![String::from_utf8_lossy(&output.stderr).trim().to_owned()];
        if let Ok(response) = serde_json::from_slice::<Value>(&output.stdout) {
            if endpoint == "graphql"
                && (response.get("data").is_some()
                    || response
                        .get("errors")
                        .and_then(Value::as_array)
                        .is_some_and(|errors| {
                            errors.iter().any(|error| {
                                ["path", "locations", "type", "extensions"]
                                    .iter()
                                    .any(|field| error.get(*field).is_some())
                            })
                        }))
            {
                return Ok(response);
            }
            if let Some(message) = response.get("message").and_then(Value::as_str) {
                details.push(message.into());
            }
            if let Some(errors) = response.get("errors").and_then(Value::as_array) {
                for error in errors {
                    match error {
                        Value::String(message) => details.push(message.clone()),
                        Value::Object(fields) => {
                            let mut fields = fields.clone();
                            let message = fields
                                .remove("message")
                                .and_then(|message| message.as_str().map(str::to_owned));
                            if let Some(message) = message {
                                details.push(message);
                            }
                            if !fields.is_empty() {
                                details.push(Value::Object(fields).to_string());
                            }
                        }
                        other => details.push(other.to_string()),
                    }
                }
            }
        }
        return Err(FetchError::Gh(
            details
                .into_iter()
                .filter(|detail| !detail.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .into());
    }
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        Ok(Value::Null)
    } else {
        Ok(serde_json::from_slice(&output.stdout).map_err(FetchError::from)?)
    }
}
