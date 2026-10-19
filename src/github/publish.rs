use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    process::Stdio,
};

use serde_json::{Value, json};

use super::*;
use crate::core::{Change, CommitState, Operation};

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
                if conclusion.outcome != ReviewOutcome::Approve {
                    nonempty(
                        conclusion.body.as_deref().unwrap_or_default(),
                        "COMMENT and REQUEST_CHANGES review body",
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

/// Publishes one core-processed operation. The caller persists its returned
/// metadata before attempting the next operation.
pub fn publish(
    review: &Review,
    backend: &ReviewBackend,
    operation: &Operation,
) -> Result<PublishResult, PublishError> {
    let address = address(backend)?;
    let repository = format!("repos/{}/{}", address.owner, address.repo);
    let pull = format!("{repository}/pulls/{}", address.number);
    let mut mappings = Vec::new();
    let response = match &operation.change {
        Change::CreateComment { comment } => {
            let (endpoint, body, resource) = match &comment.target {
                CommentTarget::PullRequest => (
                    format!("{repository}/issues/{}/comments", address.number),
                    json!({"body": comment.content.body}),
                    "issue_comment",
                ),
                CommentTarget::Diff(location) => {
                    let mut body = json!({"body": comment.content.body,
                        "commit_id": location.revision.head, "path": location.position.path});
                    match &location.position.range {
                        Some(range) => {
                            body["line"] = json!(range.end.line);
                            body["side"] = json!(side(range.end.side));
                            if range.start != range.end {
                                body["start_line"] = json!(range.start.line);
                                body["start_side"] = json!(side(range.start.side));
                            }
                        }
                        None => body["subject_type"] = json!("file"),
                    }
                    (format!("{pull}/comments"), body, "review_comment")
                }
                CommentTarget::Reply(parent) => {
                    let (resource, id) = comment_remote(backend, parent)?;
                    if resource != "review_comment" {
                        return Err(unsupported("replies require a diff comment"));
                    }
                    let root = match read_root(review, parent)? {
                        Some(root) => remote(backend, "review_comment", &root.0)?,
                        None => {
                            // A parent published in an earlier operation need not
                            // be present in the immutable fetched snapshot.
                            let response: RemoteReplyParent = api(
                                &format!("{repository}/pulls/comments/{id}"),
                                &["--method", "GET"],
                            )?;
                            if response.id != id || response.in_reply_to_id == Some(0) {
                                return Err(unsupported(
                                    "reply target response has an invalid resource identity",
                                ));
                            }
                            response.in_reply_to_id.unwrap_or(id)
                        }
                    };
                    (
                        format!("{pull}/comments/{root}/replies"),
                        json!({"body": comment.content.body}),
                        "review_comment",
                    )
                }
            };
            let response = request(&endpoint, "POST", &body)?;
            mappings.push(created_mapping(resource, &comment.content.id.0, &response)?);
            response
        }
        Change::EditComment { comment, body } => {
            let (resource, id) = comment_remote(backend, comment)?;
            if resource == "review_summary" {
                request(
                    &format!("{pull}/reviews/{id}"),
                    "PUT",
                    &json!({"body":body}),
                )?
            } else {
                request(
                    &comment_endpoint(&repository, backend, comment)?,
                    "PATCH",
                    &json!({"body":body}),
                )?
            }
        }
        Change::DeleteComment { comment } => {
            if comment_remote(backend, comment)?.0 == "review_summary" {
                return Err(unsupported("submitted review summaries cannot be deleted"));
            }
            request(
                &comment_endpoint(&repository, backend, comment)?,
                "DELETE",
                &Value::Null,
            )?
        }
        Change::SetFileViewed { path, viewed } => {
            let pull_response: Value = api(&pull, &["--method", "GET"])?;
            let node = pull_response
                .get("node_id")
                .and_then(Value::as_str)
                .ok_or_else(|| unsupported("pull request response lacks node_id"))?;
            let mutation = if *viewed {
                "markFileAsViewed"
            } else {
                "unmarkFileAsViewed"
            };
            let query = format!(
                "mutation($input: {}Input!) {{ {mutation}(input: $input) {{ clientMutationId }} }}",
                if *viewed {
                    "MarkFileAsViewed"
                } else {
                    "UnmarkFileAsViewed"
                }
            );
            let response = request(
                "graphql",
                "POST",
                &json!({"query":query,"variables":{"input":{
                "pullRequestId":node,"path":path,"clientMutationId":operation.id}}}),
            )?;
            if response
                .get("errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| !errors.is_empty())
                || response
                    .pointer(&format!("/data/{mutation}"))
                    .is_none_or(Value::is_null)
            {
                return Err(unsupported(format!(
                    "viewed-file mutation failed: {response}"
                )));
            }
            response
        }
        Change::SetReviewConclusion { conclusion } => {
            let event = match conclusion.outcome {
                ReviewOutcome::Comment => "COMMENT",
                ReviewOutcome::Approve => "APPROVE",
                ReviewOutcome::RequestChanges => "REQUEST_CHANGES",
            };
            let mut body =
                json!({"event": event,"commit_id":review.read.pull_request.revision.head});
            if let Some(text) = &conclusion.body {
                body["body"] = json!(text);
            }
            let response = request(&format!("{pull}/reviews"), "POST", &body)?;
            mappings.push(created_mapping("review", &operation.id, &response)?);
            response
        }
        Change::DismissReview { review, message } => {
            let id = remote(backend, "review", &review.0)?;
            request(
                &format!("{pull}/reviews/{id}/dismissals"),
                "PUT",
                &json!({"message":message,"event":"DISMISS"}),
            )?
        }
        Change::SetCommentReaction {
            comment,
            reaction,
            present,
        } => {
            if comment_remote(backend, comment)?.0 == "review_summary" {
                let node = backend
                    .mappings
                    .iter()
                    .find(|mapping| {
                        mapping.resource == "review_node" && mapping.core_id == comment.0
                    })
                    .ok_or_else(|| {
                        unsupported("review summary lacks its provider node identity")
                    })?;
                let mutation = if *present {
                    "addReaction"
                } else {
                    "removeReaction"
                };
                let content = super::workspace::graphql_reaction(reaction)?;
                let query = format!(
                    "mutation($input: {}Input!) {{ {mutation}(input:$input) {{ clientMutationId }} }}",
                    if *present {
                        "AddReaction"
                    } else {
                        "RemoveReaction"
                    }
                );
                let response = request(
                    "graphql",
                    "POST",
                    &json!({"query":query,"variables":{"input":{"subjectId":node.remote_id,"content":content,"clientMutationId":operation.id}}}),
                )?;
                super::workspace::graphql_data(&response, mutation)?;
                return Ok(PublishResult { mappings, response });
            }
            let endpoint = format!(
                "{}/reactions",
                comment_endpoint(&repository, backend, comment)?
            );
            let content = reaction_content(reaction)?;
            if *present {
                request(&endpoint, "POST", &json!({"content":content}))?
            } else {
                let user: RestUser = api("user", &["--method", "GET"])?;
                if user.id != remote(backend, "user", &review.reviewer.id.0)? {
                    return Err(unsupported(
                        "authenticated user differs from the fetched reviewer",
                    ));
                }
                let reactions: Vec<RemoteReaction> = list(&format!("{endpoint}?per_page=100"))?;
                match reactions.into_iter().find(|reaction| {
                    reaction.content == content
                        && reaction
                            .user
                            .as_ref()
                            .is_some_and(|author| author.id == user.id)
                }) {
                    Some(reaction) => request(
                        &format!("{endpoint}/{}", reaction.id),
                        "DELETE",
                        &Value::Null,
                    )?,
                    None => Value::Null,
                }
            }
        }
    };
    Ok(PublishResult { mappings, response })
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

fn read_root(review: &Review, comment: &CommentId) -> Result<Option<CommentId>, PublishError> {
    let targets = review
        .read
        .comments
        .iter()
        .map(|comment| (comment.content.id.clone(), Target::from(&comment.target)))
        .collect();
    if !review
        .read
        .comments
        .iter()
        .any(|value| value.content.id == *comment)
    {
        return Ok(None);
    }
    Ok(Some(root_comment(&targets, comment)?.clone()))
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

fn comment_endpoint(
    repository: &str,
    backend: &ReviewBackend,
    comment: &CommentId,
) -> Result<String, PublishError> {
    let (resource, id) = comment_remote(backend, comment)?;
    Ok(format!(
        "{repository}/{}/comments/{id}",
        if resource == "issue_comment" {
            "issues"
        } else {
            "pulls"
        }
    ))
}

fn created_mapping(
    resource: &str,
    core_id: &str,
    response: &Value,
) -> Result<IdMapping, PublishError> {
    let id = response
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| unsupported("successful create response lacks a resource ID"))?;
    Ok(IdMapping {
        resource: resource.into(),
        core_id: core_id.into(),
        remote_id: id.to_string(),
    })
}

fn side(value: DiffSide) -> &'static str {
    match value {
        DiffSide::Base => "LEFT",
        DiffSide::Head => "RIGHT",
    }
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

#[derive(Deserialize)]
struct RemoteReaction {
    id: u64,
    content: String,
    user: Option<RestUser>,
}

#[derive(Deserialize)]
struct RemoteReplyParent {
    id: u64,
    in_reply_to_id: Option<u64>,
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
