use super::publish::request;
use super::*;
use crate::core::{Change, CommitState, Operation};
use crate::{
    store::{OperationResult, Store, StoreError},
    workspace::{Action, Destination, Workspace, WorkspaceError},
};
use serde_json::{Value, json};
use std::path::Path;

/// Fetches missing commits together using the saved, validated provider source.
pub fn fetch_workspace_commits(
    project: &Path,
    commits: &[String],
    backend: &ReviewBackend,
) -> Result<(), Box<dyn std::error::Error>> {
    if backend.source.backend != "github" {
        return Err(FetchError::Response("workspace provider is not GitHub".into()).into());
    }
    let address = Address::from_url(&Url::parse(&backend.source.locator)?)?;
    crate::diff::fetch_missing_commits(
        project,
        commits,
        &format!("https://github.com/{}/{}.git", address.owner, address.repo),
    )?;
    Ok(())
}

/// The current repository is resolved by gh using its existing remote selection.
pub fn repository() -> Result<String, FetchError> {
    repository_at(Path::new("."))
}

pub(super) fn repository_at(project: &Path) -> Result<String, FetchError> {
    let output = Command::new("gh")
        .current_dir(project)
        .args(["repo", "view", "--json", "nameWithOwner"])
        .output()?;
    if !output.status.success() {
        return Err(FetchError::Gh(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        name_with_owner: String,
    }
    let repository: Repository = serde_json::from_slice(&output.stdout)?;
    if repository.name_with_owner.split('/').count() != 2 {
        return Err(FetchError::Response("repository must be OWNER/NAME".into()));
    }
    Ok(repository.name_with_owner)
}

pub fn current_user() -> Result<User, FetchError> {
    let user: RestUser = api("user", &["--method", "GET"])?;
    Ok(Identities::default().user(user))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRow {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub assignees: Vec<String>,
    pub state: PullRequestStatus,
    pub updated_at: String,
}

#[derive(Deserialize)]
struct ListedPull {
    number: u64,
    title: String,
    html_url: String,
    user: Option<RestUser>,
    assignees: Vec<RestUser>,
    state: String,
    merged_at: Option<String>,
    updated_at: String,
}

impl ListedPull {
    fn into_row(self) -> PullRequestRow {
        PullRequestRow {
            number: self.number,
            title: self.title,
            url: self.html_url,
            author: self.user.map(|user| user.login),
            assignees: self.assignees.into_iter().map(|user| user.login).collect(),
            state: if self.merged_at.is_some() {
                PullRequestStatus::Merged
            } else if self.state == "closed" {
                PullRequestStatus::Closed
            } else {
                PullRequestStatus::Open
            },
            updated_at: self.updated_at,
        }
    }
}

pub fn pull_requests(
    repository: &str,
    state: &str,
    assignee: Option<&str>,
) -> Result<Vec<PullRequestRow>, FetchError> {
    if !matches!(state, "open" | "closed" | "merged" | "all") {
        return Err(FetchError::Response(
            "state must be open, closed, merged or all".into(),
        ));
    }
    let assignee = match assignee {
        Some("all") | None => None,
        Some("@me") => Some(current_user()?.name),
        Some(login) => Some(login.into()),
    };
    // Pull-request listing has no search-result ceiling. Paginate before applying
    // the assignee filter, rather than silently truncating GitHub search at 1000.
    let remote_state = if state == "merged" { "closed" } else { state };
    let pulls: Vec<ListedPull> = list(&format!(
        "repos/{repository}/pulls?state={remote_state}&sort=updated&direction=desc&per_page=100"
    ))?;
    let mut rows: Vec<_> = pulls
        .into_iter()
        .map(ListedPull::into_row)
        .filter(|pull| {
            !(state == "merged" && pull.state != PullRequestStatus::Merged
                || state == "closed" && pull.state != PullRequestStatus::Closed
                || assignee.as_ref().is_some_and(|login| {
                    !pull
                        .assignees
                        .iter()
                        .any(|user| user.eq_ignore_ascii_case(login))
                }))
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.number.cmp(&right.number))
    });
    Ok(rows)
}

/// Finds open PRs for the configured source branch without guessing from HEAD's SHA.
pub fn branch_pull_requests(project: &Path) -> Result<Vec<PullRequestRow>, FetchError> {
    let Some((head_repository, branch)) = branch_source(project)? else {
        return Ok(vec![]);
    };
    #[derive(Deserialize)]
    struct Repository {
        full_name: String,
    }
    #[derive(Deserialize)]
    struct Head {
        #[serde(rename = "ref")]
        branch: String,
        repo: Option<Repository>,
    }
    #[derive(Deserialize)]
    struct Pull {
        #[serde(flatten)]
        pull: ListedPull,
        head: Option<Head>,
    }
    let repository = repository_at(project)?;
    let pulls: Vec<Pull> = list(&format!(
        "repos/{repository}/pulls?state=open&sort=updated&direction=desc&per_page=100"
    ))?;
    let mut rows = pulls
        .into_iter()
        .filter(|pull| {
            pull.pull.state == "open"
                && pull.pull.merged_at.is_none()
                && pull.head.as_ref().is_some_and(|head| {
                    head.branch == branch
                        && head.repo.as_ref().is_some_and(|repo| {
                            repo.full_name.eq_ignore_ascii_case(&head_repository)
                        })
                })
        })
        .map(|pull| pull.pull.into_row())
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.number.cmp(&right.number))
    });
    Ok(rows)
}

pub(super) fn git_value(project: &Path, args: &[&str]) -> Result<Option<String>, FetchError> {
    let output = Command::new("git")
        .current_dir(project)
        .args(args)
        .output()?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn branch_source(project: &Path) -> Result<Option<(String, String)>, FetchError> {
    let Some(reference) = git_value(project, &["symbolic-ref", "--quiet", "HEAD"])? else {
        return Ok(None);
    };
    let Some(branch) = reference.strip_prefix("refs/heads/") else {
        return Ok(None);
    };
    let explicit_push = git_value(
        project,
        &["config", "--get", &format!("branch.{branch}.pushRemote")],
    )?
    .is_some()
        || git_value(project, &["config", "--get", "remote.pushDefault"])?.is_some();
    let Some(refs) = git_value(
        project,
        &[
            "for-each-ref",
            "--format=%(push:remotename)%00%(push:remoteref)%00%(push)%00%(upstream:remotename)%00%(upstream:remoteref)",
            &reference,
        ],
    )?
    else {
        return Ok(None);
    };
    let refs = refs.split('\0').collect::<Vec<_>>();
    let [
        push_remote,
        push_ref,
        push_tracking,
        upstream_remote,
        upstream_ref,
    ] = refs.as_slice()
    else {
        return Ok(None);
    };
    let explicit_push = explicit_push
        || git_value(
            project,
            &["config", "--get-all", &format!("remote.{push_remote}.push")],
        )?
        .is_some()
        || git_value(
            project,
            &[
                "config",
                "--get-all",
                &format!("remote.{push_remote}.pushurl"),
            ],
        )?
        .is_some();
    let push_ref = if !push_ref.is_empty() {
        Some((*push_ref).to_owned())
    } else if !push_remote.is_empty() && !push_tracking.is_empty() {
        git_value(
            project,
            &[
                "config",
                "--get-all",
                &format!("remote.{push_remote}.fetch"),
            ],
        )?
        .and_then(|specs| remote_branch_ref(push_tracking, &specs))
    } else {
        None
    };
    if let Some(push_ref) = push_ref
        && let Some(identity) = remote_branch(project, push_remote, &push_ref, true)?
    {
        return Ok(Some(identity));
    }
    if explicit_push {
        return Ok(None);
    }
    remote_branch(project, upstream_remote, upstream_ref, false)
}

pub(super) fn remote_branch_ref(tracking: &str, specs: &str) -> Option<String> {
    let mut candidates = specs
        .lines()
        .filter_map(|spec| {
            let (source, destination) = spec.trim_start_matches('+').split_once(':')?;
            let mapped = if let Some((prefix, suffix)) = destination.split_once('*') {
                let middle = tracking.strip_prefix(prefix)?.strip_suffix(suffix)?;
                let (prefix, suffix) = source.split_once('*')?;
                format!("{prefix}{middle}{suffix}")
            } else if tracking == destination {
                source.to_owned()
            } else {
                return None;
            };
            mapped.starts_with("refs/heads/").then_some(mapped)
        })
        .collect::<BTreeSet<_>>();
    if candidates.len() == 1 {
        candidates.pop_first()
    } else {
        None
    }
}

fn remote_branch(
    project: &Path,
    remote: &str,
    reference: &str,
    push: bool,
) -> Result<Option<(String, String)>, FetchError> {
    let Some(branch) = reference
        .strip_prefix("refs/heads/")
        .filter(|branch| !branch.is_empty())
    else {
        return Ok(None);
    };
    if remote.is_empty() || remote == "." {
        return Ok(None);
    }
    let args = if push {
        vec!["remote", "get-url", "--push", "--all", remote]
    } else {
        vec!["remote", "get-url", "--all", remote]
    };
    let Some(urls) = git_value(project, &args)? else {
        return Ok(None);
    };
    let mut repositories = urls
        .lines()
        .map(github_remote)
        .collect::<Option<BTreeSet<_>>>();
    Ok(repositories
        .as_mut()
        .filter(|repos| repos.len() == 1)
        .and_then(BTreeSet::pop_first)
        .map(|repository| (repository, branch.to_owned())))
}

pub(super) fn github_remote(remote: &str) -> Option<String> {
    let path = if let Some(url) = Url::parse(remote)
        .ok()
        .filter(|url| url.host_str().is_some())
    {
        if !url.host_str()?.eq_ignore_ascii_case("github.com") {
            return None;
        }
        url.path().to_owned()
    } else {
        let (host, path) = remote.split_once(':')?;
        if !host.rsplit('@').next()?.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path.to_owned()
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts = path.split('/').collect::<Vec<_>>();
    let [owner, repo] = parts.as_slice() else {
        return None;
    };
    parts
        .iter()
        .all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
        .then(|| format!("{owner}/{repo}").to_ascii_lowercase())
}

pub(super) fn graphql_data<'a>(
    response: &'a Value,
    field: &str,
) -> Result<&'a Value, PublishError> {
    if response
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(PublishError::Unsupported(format!(
            "GitHub GraphQL request failed: {response}"
        )));
    }
    response
        .pointer(&format!("/data/{field}"))
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            PublishError::Unsupported(format!("GitHub response lacks {field}: {response}"))
        })
}

pub(super) fn graphql_reaction(value: &str) -> Result<&'static str, PublishError> {
    match value {
        "👍" => Ok("THUMBS_UP"),
        "👎" => Ok("THUMBS_DOWN"),
        "😄" => Ok("LAUGH"),
        "🎉" => Ok("HOORAY"),
        "😕" => Ok("CONFUSED"),
        "❤️" => Ok("HEART"),
        "🚀" => Ok("ROCKET"),
        "👀" => Ok("EYES"),
        _ => Err(PublishError::Unsupported(format!(
            "unsupported reaction: {value}"
        ))),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReview {
    pub id: u64,
    pub node_id: String,
    pub head: String,
}

fn unsupported(message: impl Into<String>) -> PublishError {
    PublishError::Unsupported(message.into())
}
fn endpoint(backend: &ReviewBackend) -> Result<String, PublishError> {
    if backend.source.backend != "github" {
        return Err(unsupported("workspace provider is not GitHub"));
    }
    let address = Address::from_url(
        &Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?,
    )?;
    Ok(format!(
        "repos/{}/{}/pulls/{}",
        address.owner, address.repo, address.number
    ))
}

pub fn prepare_pending_review(
    review: &Review,
    backend: &ReviewBackend,
) -> Result<PendingReview, PublishError> {
    let response = request(
        &format!("{}/reviews", endpoint(backend)?),
        "POST",
        &json!({"commit_id":review.read.pull_request.revision.head}),
    )?;
    Ok(PendingReview {
        head: review.read.pull_request.revision.head.clone(),
        id: response
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| unsupported("prepared review lacks ID"))?,
        node_id: response
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|node| !node.is_empty())
            .ok_or_else(|| unsupported("prepared review lacks node ID"))?
            .into(),
    })
}

fn root_id(
    review: &Review,
    backend: &ReviewBackend,
    parent: &CommentId,
) -> Result<CommentId, PublishError> {
    let mut parent = parent.clone();
    let mut seen = BTreeSet::new();
    while seen.insert(parent.clone()) {
        match review
            .read
            .comments
            .iter()
            .find(|comment| comment.content.id == parent)
            .map(|comment| &comment.target)
        {
            Some(CommentTarget::Reply(id)) => parent = id.clone(),
            Some(CommentTarget::Diff(_)) => return Ok(parent),
            _ if backend.mappings.iter().any(|mapping| {
                matches!(
                    mapping.resource.as_str(),
                    "review_thread" | "review_comment"
                ) && mapping.core_id == parent.0
            }) =>
            {
                return Ok(parent);
            }
            _ => return Err(unsupported("reply thread identity unavailable")),
        }
    }
    Err(unsupported("cyclic reply ancestry"))
}

fn thread_node(
    review: &Review,
    backend: &ReviewBackend,
    parent: &CommentId,
) -> Result<String, PublishError> {
    let root = root_id(review, backend, parent)?;
    if let Some(mapping) = backend
        .mappings
        .iter()
        .find(|mapping| mapping.resource == "review_thread" && mapping.core_id == root.0)
    {
        return Ok(mapping.remote_id.clone());
    }
    let mut remote = backend
        .mappings
        .iter()
        .find(|mapping| mapping.resource == "review_comment" && mapping.core_id == root.0)
        .ok_or_else(|| unsupported("reply root lacks provider identity"))?
        .remote_id
        .parse::<u64>()
        .map_err(|_| unsupported("invalid reply root ID"))?;
    if !review
        .read
        .comments
        .iter()
        .any(|comment| comment.content.id == root)
    {
        let pull = endpoint(backend)?;
        let repository = pull.split("/pulls/").next().unwrap();
        let comment: Value = api(
            &format!("{repository}/pulls/comments/{remote}"),
            &["--method", "GET"],
        )?;
        if comment.get("id").and_then(Value::as_u64) != Some(remote) {
            return Err(unsupported(
                "reply target returned an inconsistent identity",
            ));
        }
        if let Some(parent) = comment.get("in_reply_to_id").and_then(Value::as_u64) {
            remote = parent;
        }
    }
    let address = Address::from_url(
        &Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?,
    )?;
    let mut cursor = Value::Null;
    let mut seen = BTreeSet::new();
    loop {
        let response = request(
            "graphql",
            "POST",
            &json!({"query":"query($owner:String!,$name:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$cursor){nodes{id comments(first:1){nodes{databaseId}}} pageInfo{hasNextPage endCursor}}}}}","variables":{"owner":address.owner,"name":address.repo,"number":address.number,"cursor":cursor}}),
        )?;
        let connection = graphql_data(&response, "repository")?
            .pointer("/pullRequest/reviewThreads")
            .ok_or_else(|| unsupported("review thread connection missing"))?;
        for thread in connection
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| unsupported("review threads missing"))?
        {
            if thread
                .pointer("/comments/nodes/0/databaseId")
                .and_then(Value::as_u64)
                == Some(remote)
            {
                return thread
                    .get("id")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .ok_or_else(|| unsupported("review thread lacks node ID"));
            }
        }
        if connection
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Err(unsupported("reply thread no longer exists"));
        }
        cursor = connection
            .pointer("/pageInfo/endCursor")
            .filter(|cursor| cursor.is_string())
            .cloned()
            .ok_or_else(|| unsupported("review thread cursor missing"))?;
        if !seen.insert(cursor.to_string()) {
            return Err(unsupported("review thread pagination repeated a cursor"));
        }
    }
}

pub fn add_pending_comment(
    review: &Review,
    backend: &ReviewBackend,
    pending: &PendingReview,
    operation: &Operation,
) -> Result<PublishResult, PublishError> {
    let Change::CreateComment { comment } = &operation.change else {
        return Err(unsupported("pending review accepts comment creation only"));
    };
    let mut input = json!({"pullRequestReviewId":pending.node_id,"body":comment.content.body,"clientMutationId":operation.id});
    let (mutation, query) = match &comment.target {
        CommentTarget::Diff(location) => {
            input["path"] = json!(location.position.path);
            input["subjectType"] = json!(if location.position.range.is_some() {
                "LINE"
            } else {
                "FILE"
            });
            if let Some(range) = &location.position.range {
                input["line"] = json!(range.end.line);
                input["side"] = json!(if range.end.side == DiffSide::Head {
                    "RIGHT"
                } else {
                    "LEFT"
                });
                if range.start != range.end {
                    input["startLine"] = json!(range.start.line);
                    input["startSide"] = input["side"].clone();
                }
            }
            (
                "addPullRequestReviewThread",
                "mutation($input:AddPullRequestReviewThreadInput!){addPullRequestReviewThread(input:$input){thread{id comments(first:1){nodes{id databaseId}}}}}",
            )
        }
        CommentTarget::Reply(parent) => {
            input["pullRequestReviewThreadId"] = json!(thread_node(review, backend, parent)?);
            (
                "addPullRequestReviewThreadReply",
                "mutation($input:AddPullRequestReviewThreadReplyInput!){addPullRequestReviewThreadReply(input:$input){comment{id databaseId}}}",
            )
        }
        CommentTarget::PullRequest => {
            return Err(unsupported(
                "PR-wide comments cannot belong to a review; use conclusion",
            ));
        }
    };
    let response = request(
        "graphql",
        "POST",
        &json!({"query":query,"variables":{"input":input}}),
    )?;
    let data = graphql_data(&response, mutation)?;
    let created = if mutation == "addPullRequestReviewThread" {
        data.pointer("/thread/comments/nodes/0")
    } else {
        data.get("comment")
    }
    .ok_or_else(|| unsupported("created review comment missing"))?;
    let remote = created
        .get("databaseId")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| unsupported("created review comment ID missing"))?;
    let mut mappings = vec![IdMapping {
        resource: "review_comment".into(),
        core_id: comment.content.id.0.clone(),
        remote_id: remote.to_string(),
    }];
    let node = created
        .get("id")
        .and_then(Value::as_str)
        .filter(|node| !node.is_empty())
        .ok_or_else(|| unsupported("created review comment node ID missing"))?;
    mappings.push(IdMapping {
        resource: "review_comment_node".into(),
        core_id: comment.content.id.0.clone(),
        remote_id: node.into(),
    });
    if mutation == "addPullRequestReviewThread" {
        let thread = data
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| unsupported("created thread ID missing"))?;
        mappings.push(IdMapping {
            resource: "review_thread".into(),
            core_id: comment.content.id.0.clone(),
            remote_id: thread.into(),
        });
    }
    Ok(PublishResult { mappings, response })
}

pub fn submit_pending_review(
    backend: &ReviewBackend,
    pending: &PendingReview,
    conclusion: &ReviewConclusion,
) -> Result<PublishResult, PublishError> {
    let event = match conclusion.outcome {
        ReviewOutcome::Comment => "COMMENT",
        ReviewOutcome::Approve => "APPROVE",
        ReviewOutcome::RequestChanges => "REQUEST_CHANGES",
    };
    let response = request(
        &format!("{}/reviews/{}/events", endpoint(backend)?, pending.id),
        "POST",
        &json!({"event":event,"body":conclusion.body.clone().unwrap_or_default()}),
    )?;
    let expected = match conclusion.outcome {
        ReviewOutcome::Comment => "COMMENTED",
        ReviewOutcome::Approve => "APPROVED",
        ReviewOutcome::RequestChanges => "CHANGES_REQUESTED",
    };
    if response.get("state").and_then(Value::as_str) != Some(expected)
        || response.get("id").and_then(Value::as_u64) != Some(pending.id)
    {
        return Err(unsupported(
            "provider response did not confirm the expected submitted review; reconcile before retry",
        ));
    }
    Ok(PublishResult {
        mappings: Vec::new(),
        response,
    })
}

#[derive(Debug, Error)]
pub enum WorkspacePublishError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Provider(#[from] PublishError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Preparation {
    pending: Option<PendingReview>,
    #[serde(default)]
    prepared: Vec<OperationResult>,
    #[serde(default)]
    submitted: bool,
    #[serde(default)]
    submission_response: Value,
    #[serde(default)]
    progress: Vec<Operation>,
    inflight: Option<String>,
}

fn save_preparation(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &Preparation,
) -> Result<(), WorkspacePublishError> {
    apply_publication(
        store,
        workspace,
        Action::SetAttemptProvider {
            provider: serde_json::to_value(preparation).expect("provider preparation serializes"),
        },
    )?;
    Ok(())
}

fn apply_publication(
    store: &mut Store,
    workspace: &mut Workspace,
    action: Action,
) -> Result<(), WorkspacePublishError> {
    let attempt = workspace
        .attempt
        .as_ref()
        .ok_or_else(|| unsupported("publication attempt disappeared"))?
        .id
        .clone();
    *workspace = store.mutate_publication(&workspace.id, &attempt, action)?;
    Ok(())
}

fn backend_for(
    store: &mut Store,
    workspace: &Workspace,
    preparation: &Preparation,
) -> Result<ReviewBackend, WorkspacePublishError> {
    let mut backend = store
        .review_backend(&workspace.review.id)?
        .ok_or_else(|| unsupported("workspace lacks provider metadata"))?;
    for result in &preparation.prepared {
        for mapping in &result.mappings {
            backend.mappings.retain(|existing| {
                existing.resource != mapping.resource || existing.core_id != mapping.core_id
            });
            backend.mappings.push(mapping.clone());
        }
    }
    Ok(backend)
}

/// A read-only last-moment version check protects edits/deletions made against
/// stale snapshots. Provider validation remains authoritative for diff anchors.
fn check_versions(
    review: &Review,
    completed: &[Operation],
    backend: &ReviewBackend,
    operations: &[Operation],
) -> Result<(), PublishError> {
    let pull = endpoint(backend)?;
    let repository = pull.split("/pulls/").next().expect("pull endpoint");
    let mut checked = BTreeSet::new();
    for operation in operations {
        let id = match &operation.change {
            Change::EditComment { comment, .. } | Change::DeleteComment { comment } => {
                Some(comment)
            }
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Reply(parent),
                        ..
                    },
            } => Some(parent),
            _ => None,
        };
        let Some(id) = id else {
            continue;
        };
        if !checked.insert(id.clone()) {
            continue;
        }
        let mut observed = review
            .read
            .comments
            .iter()
            .find(|comment| comment.content.id == *id)
            .map(|comment| {
                (
                    comment.content.body.clone(),
                    comment.author.as_ref().map(|author| author.id.clone()),
                )
            });
        for delivered in completed {
            match &delivered.change {
                Change::CreateComment { comment } if comment.content.id == *id => {
                    observed = Some((
                        comment.content.body.clone(),
                        Some(comment.author.id.clone()),
                    ))
                }
                Change::EditComment { comment, body } if *comment == *id => {
                    if let Some(observed) = &mut observed {
                        observed.0 = body.clone();
                    }
                }
                Change::DeleteComment { comment } if *comment == *id => observed = None,
                _ => {}
            }
        }
        let Some((observed_body, observed_author)) = observed else {
            continue;
        };
        let owned = matches!(
            operation.change,
            Change::EditComment { .. } | Change::DeleteComment { .. }
        );
        if owned
            && observed_author
                .as_ref()
                .is_none_or(|author| *author != review.reviewer.id)
        {
            return Err(unsupported(
                "editing and deletion require verified ownership",
            ));
        }
        let mapping = backend
            .mappings
            .iter()
            .find(|mapping| {
                mapping.core_id == id.0
                    && matches!(
                        mapping.resource.as_str(),
                        "issue_comment" | "review_comment" | "review_summary"
                    )
            })
            .ok_or_else(|| unsupported("comment lacks provider identity"))?;
        let endpoint = match mapping.resource.as_str() {
            "issue_comment" => format!("{repository}/issues/comments/{}", mapping.remote_id),
            "review_summary" => format!("{pull}/reviews/{}", mapping.remote_id),
            _ => format!("{repository}/pulls/comments/{}", mapping.remote_id),
        };
        let remote: Value = api(&endpoint, &["--method", "GET"])?;
        if owned && remote.get("body").and_then(Value::as_str) != Some(observed_body.as_str()) {
            return Err(unsupported(format!(
                "remote comment {} changed; refresh and resolve before publishing",
                id.0
            )));
        }
    }
    Ok(())
}

pub(super) fn confirmed_rejection(error: &PublishError) -> bool {
    match error {
        PublishError::Fetch(FetchError::Gh(message)) => [
            "HTTP 400", "HTTP 401", "HTTP 403", "HTTP 404", "HTTP 405", "HTTP 409", "HTTP 410",
            "HTTP 422",
        ]
        .iter()
        .any(|status| message.contains(status)),
        _ => false,
    }
}

fn is_schema_rejection(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("http 422")
        && (lower.contains("no subschema in \"oneof\" matched")
            || lower.contains("is not a permitted key"))
}

fn fail_attempt(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &mut Preparation,
    error: &PublishError,
) -> Result<(), WorkspacePublishError> {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("http 422")
        && !is_schema_rejection(&message)
        && ["line", "position", "path", "commit"]
            .iter()
            .any(|field| lower.contains(field))
    {
        let rejected = workspace
            .attempt
            .as_ref()
            .and_then(|attempt| {
                attempt.operations.iter().find(|operation| {
                    preparation
                        .inflight
                        .as_ref()
                        .is_some_and(|inflight| inflight.ends_with(&operation.id))
                })
            })
            .and_then(|operation| match &operation.change {
                Change::CreateComment {
                    comment:
                        DraftComment {
                            content,
                            target: CommentTarget::Diff(_),
                            ..
                        },
                } => Some(content.id.clone()),
                _ => None,
            });
        if let Some(comment) = rejected {
            apply_publication(
                store,
                workspace,
                Action::RejectAnchor {
                    comment,
                    message: message.clone(),
                },
            )?;
        }
    }
    let uncertain = !confirmed_rejection(error);
    if !uncertain {
        preparation.inflight = None;
        save_preparation(store, workspace, preparation)?;
    }
    apply_publication(
        store,
        workspace,
        Action::FinishPublication {
            success: false,
            uncertain,
            error: Some(error.to_string()),
        },
    )?;
    Ok(())
}

fn record_grouped(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &Preparation,
    response: &Value,
) -> Result<(), WorkspacePublishError> {
    for result in &preparation.prepared {
        if workspace.has_result(&result.operation.id) {
            continue;
        }
        if workspace
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.operations.contains(&result.operation))
        {
            apply_publication(
                store,
                workspace,
                Action::RecordResult {
                    result: result.clone(),
                },
            )?;
        }
    }
    let conclusion = workspace
        .attempt
        .as_ref()
        .and_then(|attempt| {
            attempt
                .operations
                .iter()
                .find(|operation| matches!(operation.change, Change::SetReviewConclusion { .. }))
        })
        .cloned();
    if let Some(operation) = conclusion {
        if workspace.has_result(&operation.id) {
            return Ok(());
        }
        let mut mappings = vec![];
        if let Some(pending) = &preparation.pending {
            mappings.push(IdMapping {
                resource: "review".into(),
                core_id: operation.id.clone(),
                remote_id: pending.id.to_string(),
            });
        }
        apply_publication(
            store,
            workspace,
            Action::RecordResult {
                result: OperationResult {
                    operation,
                    mappings,
                    response: response.clone(),
                },
            },
        )?;
    }
    Ok(())
}

/// Execute only a frozen selected plan. Every write is preceded by a durable
/// attempt marker; a crash cannot cause an unrecorded write to be blindly sent
/// again. An inconclusive create remains explicitly blocked for reconciliation.
pub fn publish_workspace(
    store: &mut Store,
    id: &str,
    generation: u64,
    submit: bool,
) -> Result<Workspace, WorkspacePublishError> {
    let _publication_lock = store.publication_lock()?;
    let result = publish_workspace_inner(store, id, generation, submit);
    if let Err(error) = &result
        && let Ok(Some(workspace)) = store.workspace(id)
        && workspace.attempt.is_some()
    {
        // Release the execution lease even when reconciliation cannot decide an
        // unknown result. Its scoped marker continues protecting affected work.
        let _ = store.mutate_publication(
            id,
            &workspace.attempt.as_ref().unwrap().id,
            Action::FinishPublication {
                success: false,
                uncertain: workspace.attempt.as_ref().is_some_and(|attempt| {
                    attempt
                        .provider
                        .get("inflight")
                        .is_some_and(Value::is_string)
                }),
                error: Some(error.to_string()),
            },
        );
    }
    result
}

fn publish_workspace_inner(
    store: &mut Store,
    id: &str,
    generation: u64,
    submit: bool,
) -> Result<Workspace, WorkspacePublishError> {
    let mut workspace = store
        .workspace(id)?
        .ok_or_else(|| WorkspaceError::NotFound(id.into()))?;
    if workspace.generation != generation {
        return Err(WorkspaceError::StaleGeneration {
            expected: generation,
            actual: workspace.generation,
        }
        .into());
    }
    let project = store.project_root().to_path_buf();
    super::intent::resolve_pull_request(store, &project, &workspace)?;
    // PR binding updates provider metadata independently from the immutable
    // review comparison and its local operation generations.
    workspace = store
        .workspace(id)?
        .ok_or_else(|| WorkspaceError::NotFound(id.into()))?;
    if workspace.generation != generation {
        return Err(WorkspaceError::StaleGeneration {
            expected: generation,
            actual: workspace.generation,
        }
        .into());
    }
    if workspace
        .rejected_anchors
        .values()
        .any(|message| is_schema_rejection(message))
    {
        // Older versions mistook malformed request schemas for rejected code
        // locations. Restore only those confirmed failures to the pending plan;
        // keep the saved comment, anchor and original failure history intact.
        workspace = store.update_workspace(id, workspace.generation, |workspace| {
            workspace
                .rejected_anchors
                .retain(|_, message| !is_schema_rejection(message));
            if workspace
                .last_error
                .as_deref()
                .is_some_and(is_schema_rejection)
            {
                workspace.last_error = None;
            }
            Ok(())
        })?;
    }
    let destination = if submit {
        Destination::Review
    } else {
        Destination::Standalone
    };
    if let Some(attempt) = &workspace.attempt {
        let submitted = attempt.provider.get("submitted").and_then(Value::as_bool) == Some(true);
        if attempt.destination != destination && !submitted {
            workspace =
                store.mutate_workspace(id, workspace.generation, Action::SuspendPublication)?;
        }
    }
    if workspace.attempt.is_none() {
        let resume = if submit {
            workspace
                .suspended_attempts
                .iter()
                .find(|attempt| {
                    attempt.destination == Destination::Review
                        && (attempt.wip == workspace.wip.as_ref().map(|wip| wip.id.clone())
                            || attempt.provider.get("submitted").and_then(Value::as_bool)
                                == Some(true))
                })
                .map(|attempt| attempt.id.clone())
        } else if workspace.prepare(destination)?.operations.is_empty() {
            workspace
                .suspended_attempts
                .iter()
                .find(|attempt| {
                    attempt.destination == Destination::Standalone
                        || attempt.provider.get("submitted").and_then(Value::as_bool) == Some(true)
                })
                .map(|attempt| attempt.id.clone())
        } else {
            None
        };
        if let Some(attempt_id) = resume {
            workspace = store.mutate_workspace(
                id,
                workspace.generation,
                Action::ResumePublication { attempt_id },
            )?;
        }
    }
    if workspace.attempt.is_none() {
        let plan = workspace.prepare(destination)?;
        if plan.operations.is_empty() {
            return Ok(workspace);
        }
        let saved: Preparation =
            if destination == Destination::Standalone || workspace.preparation.is_null() {
                Preparation::default()
            } else {
                serde_json::from_value(workspace.preparation.clone()).map_err(FetchError::from)?
            };
        let backend = backend_for(store, &workspace, &saved)?;
        let unprepared = CommitState {
            operations: plan
                .operations
                .iter()
                .filter(|operation| !is_prepared_create(&saved, operation))
                .cloned()
                .collect(),
        };
        preflight(&workspace.review, &backend, &unprepared)?;
        check_versions(
            &workspace.review,
            &workspace.completed,
            &backend,
            &plan.operations,
        )?;
        workspace = store.mutate_workspace(
            id,
            workspace.generation,
            Action::BeginPublication {
                destination,
                operations: plan.operations,
            },
        )?;
    }
    let submit = workspace.attempt.as_ref().unwrap().destination == Destination::Review;
    let value = workspace.attempt.as_ref().unwrap().provider.clone();
    let new_preparation = value.is_null();
    let mut preparation: Preparation = if new_preparation {
        Preparation::default()
    } else {
        serde_json::from_value(value).map_err(FetchError::from)?
    };
    if new_preparation {
        preparation.progress = workspace
            .attempt
            .as_ref()
            .unwrap()
            .operations
            .iter()
            .filter(|operation| {
                matches!(
                    operation.change,
                    Change::SetFileViewed { .. } | Change::SetCommentReaction { .. }
                )
            })
            .cloned()
            .collect();
        save_preparation(store, &mut workspace, &preparation)?;
    } else {
        // Unattempted progress may publish independently while a review remains
        // uncertain. Explicit retry includes only its original pending versions.
        let outstanding = workspace.pending()?.operations;
        let deferred = preparation
            .progress
            .iter()
            .filter(|operation| outstanding.contains(operation))
            .cloned()
            .collect::<Vec<_>>();
        if deferred.iter().any(|operation| {
            !workspace
                .attempt
                .as_ref()
                .unwrap()
                .operations
                .contains(operation)
        }) {
            workspace = store.update_workspace(id, workspace.generation, |workspace| {
                let attempt = workspace.attempt.as_mut().ok_or_else(|| {
                    WorkspaceError::Invalid("publication attempt disappeared".into())
                })?;
                for operation in deferred {
                    if !attempt.operations.contains(&operation) {
                        attempt.operations.push(operation);
                    }
                }
                Ok(())
            })?;
        }
    }
    let mut backend = backend_for(store, &workspace, &preparation)?;
    let operations = workspace.attempt.as_ref().unwrap().operations.clone();
    preflight(
        &workspace.review,
        &backend,
        &CommitState {
            operations: operations
                .iter()
                .filter(|operation| {
                    !workspace.has_result(&operation.id)
                        && !is_prepared_create(&preparation, operation)
                })
                .cloned()
                .collect(),
        },
    )?;
    if let Some(inflight) = preparation.inflight.clone() {
        if inflight == "submit" {
            let pending = preparation.pending.as_ref().ok_or_else(|| {
                unsupported("uncertain submission lacks prepared review identity")
            })?;
            let response: Value = api(
                &format!("{}/reviews/{}", endpoint(&backend)?, pending.id),
                &["--method", "GET"],
            )?;
            let state = response
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| unsupported("review state unavailable during reconciliation"))?;
            if state != "PENDING" {
                let conclusion = operations
                    .iter()
                    .find_map(|operation| match &operation.change {
                        Change::SetReviewConclusion { conclusion } => Some(conclusion),
                        _ => None,
                    })
                    .ok_or_else(|| unsupported("prepared review lacks conclusion"))?;
                let expected = match conclusion.outcome {
                    ReviewOutcome::Approve => "APPROVED",
                    ReviewOutcome::Comment => "COMMENTED",
                    ReviewOutcome::RequestChanges => "CHANGES_REQUESTED",
                };
                if state != expected
                    || response
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        != conclusion.body.as_deref().unwrap_or_default()
                {
                    return Err(unsupported("uncertain review differs from prepared conclusion; inspect provider review before retry").into());
                }
                preparation.submitted = true;
                preparation.submission_response = response.clone();
                record_grouped(store, &mut workspace, &preparation, &response)?;
            }
            preparation.inflight = None;
            save_preparation(store, &mut workspace, &preparation)?;
        } else if let Some(operation_id) = inflight.strip_prefix("operation ") {
            let operation = operations
                .iter()
                .find(|operation| operation.id == operation_id)
                .ok_or_else(|| unsupported("uncertain operation missing from frozen plan"))?;
            if !workspace.has_result(operation_id) {
                if matches!(
                    operation.change,
                    Change::CreateComment { .. }
                        | Change::SetReviewConclusion { .. }
                        | Change::DismissReview { .. }
                ) {
                    return Err(unsupported(format!("publication result uncertain ({inflight}); inspect provider state before retry; no duplicate write sent")).into());
                }
                let mut fresh = fetch(
                    &Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?,
                )?;
                canonicalize_fetched(&mut fresh, &backend);
                if fresh.reviewer.id != workspace.review.reviewer.id {
                    return Err(unsupported(
                        "authenticated reviewer changed during reconciliation",
                    )
                    .into());
                }
                let matches = match &operation.change {
                    Change::EditComment { comment, body } => fresh
                        .read
                        .comments
                        .iter()
                        .find(|item| item.content.id == *comment)
                        .is_some_and(|item| item.content.body == *body),
                    Change::DeleteComment { comment } => !fresh
                        .read
                        .comments
                        .iter()
                        .any(|item| item.content.id == *comment),
                    Change::SetFileViewed { path, viewed } => fresh
                        .read
                        .files
                        .iter()
                        .any(|file| file.path == *path && file.viewed == *viewed),
                    Change::SetCommentReaction {
                        comment,
                        reaction,
                        present,
                    } => {
                        fresh.read.reactions.iter().any(|item| {
                            item.comment == *comment
                                && item.author.as_ref() == Some(&workspace.review.reviewer.id)
                                && item.reaction == *reaction
                        }) == *present
                    }
                    _ => false,
                };
                if matches {
                    apply_publication(
                        store,
                        &mut workspace,
                        Action::RecordResult {
                            result: OperationResult {
                                operation: operation.clone(),
                                mappings: vec![],
                                response: json!({"reconciled":true}),
                            },
                        },
                    )?;
                } else if matches!(
                    operation.change,
                    Change::EditComment { .. } | Change::DeleteComment { .. }
                ) {
                    let conflicted = crate::workspace::operation_comment(&operation.change)
                        .is_some_and(|id| workspace.conflicts.contains_key(id));
                    let checked = if conflicted {
                        Err(unsupported(
                            "remote conflict requires explicit resolution before retry",
                        ))
                    } else {
                        check_versions(
                            &workspace.review,
                            &workspace.completed,
                            &backend,
                            std::slice::from_ref(operation),
                        )
                    };
                    if let Err(error) = checked {
                        preparation.inflight = None;
                        save_preparation(store, &mut workspace, &preparation)?;
                        return Err(error.into());
                    }
                }
            }
            preparation.inflight = None;
            save_preparation(store, &mut workspace, &preparation)?;
        } else {
            return Err(unsupported(format!("publication result uncertain ({inflight}); inspect provider state before retry; no duplicate write sent")).into());
        }
    }
    let blocked = operations
        .iter()
        .filter(|operation| !workspace.has_result(&operation.id))
        .filter_map(|operation| {
            let target = crate::workspace::operation_comment(&operation.change)?;
            let parent = match &operation.change {
                Change::CreateComment {
                    comment:
                        DraftComment {
                            target: CommentTarget::Reply(parent),
                            ..
                        },
                } => Some(parent),
                _ => None,
            };
            (workspace.conflicts.contains_key(target)
                || workspace.rejected_anchors.contains_key(target)
                || parent.is_some_and(|id| {
                    workspace.conflicts.contains_key(id)
                        || workspace.rejected_anchors.contains_key(id)
                }))
            .then_some(target.0.clone())
        })
        .collect::<Vec<_>>();
    if !blocked.is_empty() {
        return Err(unsupported(format!(
            "publication requires conflict/anchor resolution: {}",
            blocked.join(", ")
        ))
        .into());
    }
    if submit && !preparation.submitted {
        let heads = operations
            .iter()
            .filter_map(|operation| match &operation.change {
                Change::CreateComment {
                    comment:
                        DraftComment {
                            target: CommentTarget::Diff(location),
                            ..
                        },
                } => Some(location.revision.head.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if heads.len() > 1 {
            let error = unsupported(
                "review drafts span different head commits; explicitly relocate or delete affected roots before submission",
            );
            for operation in &operations {
                if let Change::CreateComment {
                    comment:
                        DraftComment {
                            content,
                            target: CommentTarget::Diff(_),
                            ..
                        },
                } = &operation.change
                {
                    apply_publication(
                        store,
                        &mut workspace,
                        Action::RejectAnchor {
                            comment: content.id.clone(),
                            message: error.to_string(),
                        },
                    )?;
                }
            }
            apply_publication(
                store,
                &mut workspace,
                Action::FinishPublication {
                    success: false,
                    uncertain: false,
                    error: Some(error.to_string()),
                },
            )?;
            return Err(error.into());
        }
        if let Some(pending) = preparation.pending.as_ref()
            && heads
                .iter()
                .next()
                .is_some_and(|head| *head != pending.head)
        {
            let endpoint = format!("{}/reviews/{}", endpoint(&backend)?, pending.id);
            preparation.inflight = Some("replace relocated pending review".into());
            save_preparation(store, &mut workspace, &preparation)?;
            match request(&endpoint, "DELETE", &Value::Null) {
                Ok(_) => {
                    preparation.pending = None;
                    preparation.prepared.clear();
                    preparation.inflight = None;
                    save_preparation(store, &mut workspace, &preparation)?;
                    backend = backend_for(store, &workspace, &preparation)?;
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        }
        if preparation.pending.is_none() {
            // An existing external pending review must never be adopted.
            let existing: Vec<Value> =
                list(&format!("{}/reviews?per_page=100", endpoint(&backend)?))?;
            let user = backend
                .mappings
                .iter()
                .find(|mapping| {
                    mapping.resource == "user" && mapping.core_id == workspace.review.reviewer.id.0
                })
                .map(|mapping| mapping.remote_id.clone())
                .ok_or_else(|| unsupported("reviewer mapping missing"))?;
            if existing.iter().any(|review| {
                review.get("state").and_then(Value::as_str) == Some("PENDING")
                    && review
                        .pointer("/user/id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id.to_string() == user)
            }) {
                let error = unsupported(
                    "another tool has a pending provider review; submit or discard it there before retrying local review",
                );
                apply_publication(
                    store,
                    &mut workspace,
                    Action::FinishPublication {
                        success: false,
                        uncertain: false,
                        error: Some(error.to_string()),
                    },
                )?;
                return Err(error.into());
            }
            preparation.inflight = Some("prepare review".into());
            save_preparation(store, &mut workspace, &preparation)?;
            let mut review = workspace.review.clone();
            if let Some(head) = heads.into_iter().next() {
                review.read.pull_request.revision.head = head;
            }
            match prepare_pending_review(&review, &backend) {
                Ok(pending) => {
                    preparation.pending = Some(pending);
                    preparation.inflight = None;
                    save_preparation(store, &mut workspace, &preparation)?;
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        }
        let pending = preparation.pending.clone().unwrap();
        // A failed confirmed preparation remains editable locally. Synchronize
        // exactly that known pending review on retry; never create a replacement
        // review or accidentally submit removed/relocated provider drafts.
        let mut replace = BTreeSet::new();
        for previous in &preparation.prepared {
            let Change::CreateComment { comment: old } = &previous.operation.change else {
                continue;
            };
            let desired = operations
                .iter()
                .find_map(|operation| match &operation.change {
                    Change::CreateComment { comment } if comment.content.id == old.content.id => {
                        Some(comment)
                    }
                    _ => None,
                });
            if desired.is_none_or(|new| new.target != old.target) {
                replace.insert(old.content.id.clone());
            }
        }
        loop {
            let count = replace.len();
            for previous in &preparation.prepared {
                if let Change::CreateComment {
                    comment:
                        DraftComment {
                            content,
                            target: CommentTarget::Reply(parent),
                            ..
                        },
                } = &previous.operation.change
                    && replace.contains(parent)
                {
                    replace.insert(content.id.clone());
                }
            }
            if count == replace.len() {
                break;
            }
        }
        for index in (0..preparation.prepared.len()).rev() {
            let previous = &preparation.prepared[index];
            let Change::CreateComment { comment: old } = &previous.operation.change else {
                continue;
            };
            if !replace.contains(&old.content.id) {
                continue;
            }
            let id = old.content.id.clone();
            let remote = previous
                .mappings
                .iter()
                .find(|mapping| mapping.resource == "review_comment")
                .ok_or_else(|| unsupported("prepared comment mapping missing"))?
                .remote_id
                .clone();
            let pull = endpoint(&backend)?;
            let repository = pull.split("/pulls/").next().unwrap();
            preparation.inflight = Some(format!("delete prepared comment {}", id.0));
            save_preparation(store, &mut workspace, &preparation)?;
            match request(
                &format!("{repository}/pulls/comments/{remote}"),
                "DELETE",
                &Value::Null,
            ) {
                Ok(_) => {
                    preparation.prepared.remove(index);
                    preparation.inflight = None;
                    backend.mappings.retain(|mapping| mapping.core_id != id.0);
                    save_preparation(store, &mut workspace, &preparation)?;
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        }
        for operation in operations
            .iter()
            .filter(|operation| matches!(operation.change, Change::CreateComment { .. }))
        {
            if preparation
                .prepared
                .iter()
                .any(|result| result.operation == *operation)
            {
                continue;
            }
            // Prepared draft edits retain their provider identity. Relocations
            // require explicit replacement because GitHub cannot move comments.
            if let Some(index)=preparation.prepared.iter().position(|result|matches!((&result.operation.change,&operation.change),(Change::CreateComment{comment:left},Change::CreateComment{comment:right}) if left.content.id==right.content.id)) {
                let previous=&preparation.prepared[index];
                let (Change::CreateComment{comment:old},Change::CreateComment{comment:new})=(&previous.operation.change,&operation.change) else {unreachable!()};
                if old.target!=new.target {return Err(unsupported("prepared draft anchor changed; prepared provider review must be reconciled before relocation retry").into());}
                let node=previous.mappings.iter().find(|mapping|mapping.resource=="review_comment_node").map(|mapping|mapping.remote_id.as_str()).or_else(||previous.response.pointer("/data/addPullRequestReviewThread/thread/comments/nodes/0/id").and_then(Value::as_str)).or_else(||previous.response.pointer("/data/addPullRequestReviewThreadReply/comment/id").and_then(Value::as_str)).or_else(||previous.response.pointer("/data/updatePullRequestReviewComment/pullRequestReviewComment/id").and_then(Value::as_str)).filter(|node|!node.is_empty()).ok_or_else(||unsupported("prepared review comment lacks its provider node identity"))?.to_owned();
                let body=new.content.body.clone();
                let id=new.content.id.0.clone();
                if !preparation.prepared[index].mappings.iter().any(|mapping|mapping.resource=="review_comment_node") {preparation.prepared[index].mappings.push(IdMapping{resource:"review_comment_node".into(),core_id:id,remote_id:node.clone()});}
                preparation.inflight=Some(format!("edit prepared comment {}",operation.id));save_preparation(store,&mut workspace,&preparation)?;
                let updated=(|| {
                    let response=request("graphql","POST",&json!({"query":"mutation($input:UpdatePullRequestReviewCommentInput!){updatePullRequestReviewComment(input:$input){pullRequestReviewComment{id body}}}","variables":{"input":{"pullRequestReviewCommentId":node,"body":body,"clientMutationId":operation.id}}}))?;
                    if graphql_data(&response,"updatePullRequestReviewComment")?.pointer("/pullRequestReviewComment/id").and_then(Value::as_str)!=Some(node.as_str()) {return Err(unsupported("updated private review comment identity was not confirmed"));}
                    Ok(response)
                })();
                match updated {
                    Ok(response)=>{preparation.prepared[index].operation=operation.clone();preparation.prepared[index].response=response;preparation.inflight=None;save_preparation(store,&mut workspace,&preparation)?;}
                    Err(error)=>{fail_attempt(store,&mut workspace,&mut preparation,&error)?;return Err(error.into());}
                }
                continue;
            }
            preparation.inflight = Some(format!("create comment {}", operation.id));
            save_preparation(store, &mut workspace, &preparation)?;
            match add_pending_comment(&workspace.review, &backend, &pending, operation) {
                Ok(result) => {
                    backend.mappings.extend(result.mappings.clone());
                    preparation.prepared.push(OperationResult {
                        operation: operation.clone(),
                        mappings: result.mappings,
                        response: result.response,
                    });
                    preparation.inflight = None;
                    save_preparation(store, &mut workspace, &preparation)?;
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        }
        let conclusion = operations
            .iter()
            .find_map(|operation| match &operation.change {
                Change::SetReviewConclusion { conclusion } => Some(conclusion),
                _ => None,
            })
            .ok_or_else(|| unsupported("save review conclusion before submitting"))?;
        preparation.inflight = Some("submit".into());
        save_preparation(store, &mut workspace, &preparation)?;
        match submit_pending_review(&backend, &pending, conclusion) {
            Ok(result) => {
                preparation.submitted = true;
                preparation.submission_response = result.response.clone();
                preparation.inflight = None;
                save_preparation(store, &mut workspace, &preparation)?;
                record_grouped(store, &mut workspace, &preparation, &result.response)?;
            }
            Err(error) => {
                fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                return Err(error.into());
            }
        }
    }
    if submit && preparation.submitted {
        record_grouped(
            store,
            &mut workspace,
            &preparation,
            &preparation.submission_response,
        )?;
    }
    for operation in operations {
        if workspace.has_result(&operation.id) {
            continue;
        }
        if matches!(
            operation.change,
            Change::EditComment { .. } | Change::DeleteComment { .. }
        ) {
            check_versions(
                &workspace.review,
                &workspace.completed,
                &backend,
                std::slice::from_ref(&operation),
            )?;
        }
        preparation.inflight = Some(format!("operation {}", operation.id));
        save_preparation(store, &mut workspace, &preparation)?;
        match publish(&workspace.review, &backend, &operation) {
            Ok(result) => {
                backend.mappings.extend(result.mappings.clone());
                apply_publication(
                    store,
                    &mut workspace,
                    Action::RecordResult {
                        result: OperationResult {
                            operation,
                            mappings: result.mappings,
                            response: result.response,
                        },
                    },
                )?;
                preparation.inflight = None;
                save_preparation(store, &mut workspace, &preparation)?;
            }
            Err(error) => {
                fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                return Err(error.into());
            }
        }
    }
    apply_publication(
        store,
        &mut workspace,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    )?;
    Ok(workspace)
}

fn is_prepared_create(preparation: &Preparation, operation: &Operation) -> bool {
    preparation.prepared.iter().any(|previous| matches!((&previous.operation.change,&operation.change), (Change::CreateComment{comment:left},Change::CreateComment{comment:right}) if left.content.id==right.content.id))
}

fn canonicalize_fetched(fetched: &mut FetchedReview, backend: &ReviewBackend) {
    let aliases = fetched
        .backend
        .mappings
        .iter()
        .filter(|mapping| {
            matches!(
                mapping.resource.as_str(),
                "issue_comment" | "review_comment" | "review_summary" | "user"
            )
        })
        .filter_map(|mapping| {
            backend
                .mappings
                .iter()
                .find(|known| {
                    known.resource == mapping.resource && known.remote_id == mapping.remote_id
                })
                .map(|known| (mapping.core_id.clone(), known.core_id.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let rename = |id: &mut String| {
        if let Some(canonical) = aliases.get(id) {
            *id = canonical.clone();
        }
    };
    rename(&mut fetched.reviewer.id.0);
    for comment in &mut fetched.read.comments {
        rename(&mut comment.content.id.0);
        if let Some(author) = &mut comment.author {
            rename(&mut author.id.0);
        }
        if let CommentTarget::Reply(parent) = &mut comment.target {
            rename(&mut parent.0);
        }
    }
    for reaction in &mut fetched.read.reactions {
        rename(&mut reaction.comment.0);
        if let Some(author) = &mut reaction.author {
            rename(&mut author.0);
        }
    }
}
