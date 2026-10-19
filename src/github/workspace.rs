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
    let event = conclusion_event(conclusion);
    let response = request(
        &format!("{}/reviews/{}/events", endpoint(backend)?, pending.id),
        "POST",
        &json!({"event":event,"body":conclusion.body.clone().unwrap_or_default()}),
    )?;
    let expected = conclusion_state(conclusion);
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
    #[serde(default)]
    creating: Vec<Operation>,
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
    if let Action::RecordResult { result } = &action
        && matches!(result.operation.change, Change::CreateComment { .. })
    {
        let backend = store
            .review_backend(&workspace.review.id)?
            .ok_or_else(|| unsupported("workspace lacks provider metadata"))?;
        let fetched_identity = result.mappings.iter().any(|mapping| {
            backend.mappings.iter().any(|existing| {
                existing.resource == mapping.resource
                    && existing.remote_id == mapping.remote_id
                    && existing.core_id != mapping.core_id
                    && workspace
                        .review
                        .read
                        .comments
                        .iter()
                        .any(|comment| comment.content.id.0 == existing.core_id)
            })
        });
        if fetched_identity {
            // A refresh may observe a sent comment before its receipt is known.
            // The durable preparation now supplies its original local identity;
            // reuse normal snapshot reconciliation before installing that receipt.
            apply_publication(store, workspace, Action::SuspendPublication)?;
            *workspace = store.refresh_workspace(
                &workspace.id,
                workspace.generation,
                &workspace.review.id,
            )?;
            *workspace = store.mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::ResumePublication {
                    attempt_id: attempt.clone(),
                },
            )?;
        }
    }
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
            .into_iter()
            .flat_map(|attempt| &attempt.operations)
            .filter(|operation| {
                preparation.creating.contains(operation)
                    || preparation
                        .inflight
                        .as_ref()
                        .is_some_and(|inflight| inflight.ends_with(&operation.id))
            })
            .filter_map(|operation| match &operation.change {
                Change::CreateComment {
                    comment:
                        DraftComment {
                            content,
                            target: CommentTarget::Diff(_),
                            ..
                        },
                } => Some(content.id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for comment in rejected {
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
        preparation.creating.clear();
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
    _legacy_submit: bool,
) -> Result<Workspace, WorkspacePublishError> {
    let _publication_lock = store.publication_lock()?;
    let result = publish_workspace_inner(store, id, generation);
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
    // Old clients misclassified malformed request schemas as rejected anchors.
    if workspace
        .rejected_anchors
        .values()
        .any(|message| is_schema_rejection(message))
    {
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
    // A failed publication owns the next retry. Never bypass its uncertain tail.
    if workspace.attempt.is_none() {
        if let Some(attempt) = workspace.suspended_attempts.first() {
            let attempt_id = attempt.id.clone();
            workspace = store.mutate_workspace(
                id,
                workspace.generation,
                Action::ResumePublication { attempt_id },
            )?;
        }
    }
    if workspace.attempt.is_none() {
        let plan = workspace.publication_plan()?;
        if plan.operations.is_empty() {
            return Ok(workspace);
        }
        workspace = store.mutate_workspace(
            id,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Review,
                operations: plan.operations,
            },
        )?;
    }
    let value = workspace.attempt.as_ref().unwrap().provider.clone();
    let mut preparation: Preparation = if value.is_null() {
        Preparation::default()
    } else {
        serde_json::from_value(value).map_err(FetchError::from)?
    };
    let operations = workspace.attempt.as_ref().unwrap().operations.clone();
    let mut backend = backend_for(store, &workspace, &preparation)?;
    // Legacy private preparation already represents successful provider writes.
    for result in preparation.prepared.clone() {
        if !workspace.has_result(&result.operation.id) && operations.contains(&result.operation) {
            apply_publication(store, &mut workspace, Action::RecordResult { result })?;
        }
    }
    let outstanding = CommitState {
        operations: operations
            .iter()
            .filter(|op| !workspace.has_result(&op.id))
            .cloned()
            .collect(),
    };
    preflight(&workspace.review, &backend, &outstanding)?;
    for operation in &outstanding.operations {
        if matches!(
            operation.change,
            Change::EditComment { .. } | Change::DeleteComment { .. }
        ) {
            return Err(unsupported(
                "successfully sent comments are immutable; add a comment or reply instead",
            )
            .into());
        }
        if let Some(target) = crate::workspace::operation_comment(&operation.change) {
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
            if std::iter::once(target).chain(parent).any(|id| {
                workspace.conflicts.contains_key(id) || workspace.rejected_anchors.contains_key(id)
            }) {
                return Err(unsupported(format!(
                    "publication requires conflict/anchor resolution: {}",
                    target.0
                ))
                .into());
            }
        }
    }
    let heads = outstanding
        .operations
        .iter()
        .filter_map(|op| match &op.change {
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Diff(location),
                        ..
                    },
            } => Some(&location.revision.head),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if heads.len() > 1
        || preparation
            .pending
            .as_ref()
            .is_some_and(|pending| heads.iter().any(|head| **head != pending.head))
    {
        return Err(unsupported("review drafts span different head commits; explicitly relocate unsent roots before publication").into());
    }
    if let Some(inflight) = preparation.inflight.clone() {
        if inflight == "review batch" && preparation.pending.is_some() {
            record_created_review(store, &mut workspace, &mut preparation, &backend)?;
        } else if inflight == "submit" {
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
                    .find_map(|op| match &op.change {
                        Change::SetReviewConclusion { conclusion } => Some(conclusion),
                        _ => None,
                    })
                    .ok_or_else(|| unsupported("prepared review lacks conclusion"))?;
                if state != conclusion_state(conclusion)
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
                .find(|op| op.id == operation_id)
                .ok_or_else(|| unsupported("uncertain operation missing from frozen plan"))?;
            if !workspace.has_result(operation_id) {
                if !matches!(
                    operation.change,
                    Change::SetFileViewed { .. } | Change::SetCommentReaction { .. }
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
                let applied = match &operation.change {
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
                if applied {
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
                }
            }
            preparation.inflight = None;
            save_preparation(store, &mut workspace, &preparation)?;
        } else {
            return Err(unsupported(format!("publication result uncertain ({inflight}); inspect provider state before retry; no duplicate write sent")).into());
        }
    }
    backend = backend_for(store, &workspace, &preparation)?;
    for (index, operation) in operations.iter().enumerate() {
        if workspace.has_result(&operation.id) {
            continue;
        }
        let uses_review = matches!(
            &operation.change,
            Change::CreateComment {
                comment: DraftComment {
                    target: CommentTarget::Diff(_) | CommentTarget::Reply(_),
                    ..
                }
            } | Change::SetReviewConclusion { .. }
        );
        if uses_review && preparation.pending.is_none() {
            // Do not adopt another tool's pending review or its untracked comments.
            let existing: Vec<Value> =
                list(&format!("{}/reviews?per_page=100", endpoint(&backend)?))?;
            let user = backend
                .mappings
                .iter()
                .find(|m| m.resource == "user" && m.core_id == workspace.review.reviewer.id.0)
                .ok_or_else(|| unsupported("reviewer mapping missing"))?;
            if existing.iter().any(|review| {
                review.get("state").and_then(Value::as_str) == Some("PENDING")
                    && review
                        .pointer("/user/id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id.to_string() == user.remote_id)
            }) {
                return Err(unsupported("another tool has a pending provider review; conclude it there before publishing a new local review").into());
            }
            let head = heads
                .first()
                .map(|head| (*head).clone())
                .unwrap_or_else(|| workspace.review.read.pull_request.revision.head.clone());
            let mut body = json!({"commit_id":head});
            let roots = operations[index..].iter().take_while(|op| matches!(&op.change,
                Change::CreateComment { comment: DraftComment { target: CommentTarget::Diff(location), .. } } if location.position.range.is_some() && location.revision.head == head
            )).cloned().collect::<Vec<_>>();
            if !roots.is_empty() {
                body["comments"] = Value::Array(
                    roots
                        .iter()
                        .map(|op| {
                            let Change::CreateComment { comment } = &op.change else {
                                unreachable!()
                            };
                            let CommentTarget::Diff(location) = &comment.target else {
                                unreachable!()
                            };
                            let mut payload = super::publish::inline_payload(comment, location);
                            payload.as_object_mut().unwrap().remove("commit_id");
                            payload
                        })
                        .collect(),
                );
            }
            preparation.creating = roots;
            // Native create-review can also conclude this final contiguous group.
            if let Some(last) = operations.get(index + preparation.creating.len())
                && index + preparation.creating.len() + 1 == operations.len()
                && let Change::SetReviewConclusion { conclusion } = &last.change
            {
                body["event"] = json!(conclusion_event(conclusion));
                body["body"] = json!(conclusion.body.clone().unwrap_or_default());
                preparation.creating.push(last.clone());
            }
            preparation.inflight = Some("review batch".into());
            save_preparation(store, &mut workspace, &preparation)?;
            match request(&format!("{}/reviews", endpoint(&backend)?), "POST", &body) {
                Ok(response) => {
                    // Persist provider identity before any receipt-discovery read.
                    preparation.pending = Some(PendingReview {
                        id: response
                            .get("id")
                            .and_then(Value::as_u64)
                            .filter(|id| *id > 0)
                            .ok_or_else(|| unsupported("created review lacks ID"))?,
                        node_id: response
                            .get("node_id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| unsupported("created review lacks node ID"))?
                            .into(),
                        head,
                    });
                    preparation.submission_response = response.clone();
                    preparation.submitted = body.get("event").is_some();
                    save_preparation(store, &mut workspace, &preparation)?;
                    record_created_review(store, &mut workspace, &mut preparation, &backend)?;
                    backend = backend_for(store, &workspace, &preparation)?;
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
            if workspace.has_result(&operation.id) {
                continue;
            }
        }
        if let Change::CreateComment {
            comment:
                DraftComment {
                    target: CommentTarget::Reply(parent),
                    ..
                },
        } = &operation.change
        {
            // Resolve read-only ancestry before marking the irreversible write.
            let root = root_id(&workspace.review, &backend, parent)?;
            let thread = thread_node(&workspace.review, &backend, parent)?;
            backend
                .mappings
                .retain(|mapping| mapping.resource != "review_thread" || mapping.core_id != root.0);
            backend.mappings.push(IdMapping {
                resource: "review_thread".into(),
                core_id: root.0,
                remote_id: thread,
            });
        }
        preparation.inflight = Some(
            if matches!(operation.change, Change::SetReviewConclusion { .. }) {
                "submit".into()
            } else {
                format!("operation {}", operation.id)
            },
        );
        save_preparation(store, &mut workspace, &preparation)?;
        let result = match &operation.change {
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Diff(_) | CommentTarget::Reply(_),
                        ..
                    },
            } => {
                if preparation.submitted {
                    return Err(
                        unsupported("cannot append root comments to a concluded review").into(),
                    );
                }
                add_pending_comment(
                    &workspace.review,
                    &backend,
                    preparation.pending.as_ref().unwrap(),
                    operation,
                )
            }
            Change::SetReviewConclusion { conclusion } => {
                submit_pending_review(&backend, preparation.pending.as_ref().unwrap(), conclusion)
            }
            _ => publish(&workspace.review, &backend, operation),
        };
        match result {
            Ok(result) => {
                if matches!(operation.change, Change::CreateComment { .. }) && uses_review {
                    preparation.prepared.push(OperationResult {
                        operation: operation.clone(),
                        mappings: result.mappings.clone(),
                        response: result.response.clone(),
                    });
                }
                if matches!(operation.change, Change::SetReviewConclusion { .. }) {
                    preparation.submitted = true;
                    preparation.submission_response = result.response.clone();
                }
                // A persisted successful write is immutable, even while review is pending.
                save_preparation(store, &mut workspace, &preparation)?;
                backend.mappings.extend(result.mappings.clone());
                apply_publication(
                    store,
                    &mut workspace,
                    Action::RecordResult {
                        result: OperationResult {
                            operation: operation.clone(),
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

fn conclusion_event(conclusion: &ReviewConclusion) -> &'static str {
    match conclusion.outcome {
        ReviewOutcome::Comment => "COMMENT",
        ReviewOutcome::Approve => "APPROVE",
        ReviewOutcome::RequestChanges => "REQUEST_CHANGES",
    }
}

fn conclusion_state(conclusion: &ReviewConclusion) -> &'static str {
    match conclusion.outcome {
        ReviewOutcome::Comment => "COMMENTED",
        ReviewOutcome::Approve => "APPROVED",
        ReviewOutcome::RequestChanges => "CHANGES_REQUESTED",
    }
}

/// Pending review comments can expose only legacy diff positions through REST.
/// Read their line/range anchors from the corresponding GraphQL thread roots.
fn hydrate_review_comment_anchors(
    backend: &ReviewBackend,
    comments: &mut [Value],
) -> Result<(), PublishError> {
    let mut missing = comments
        .iter()
        .filter(|comment| {
            comment.get("side").is_none_or(Value::is_null)
                || (comment.get("line").is_none_or(Value::is_null)
                    && comment.get("original_line").is_none_or(Value::is_null))
        })
        .map(|comment| {
            comment
                .get("node_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(String::from)
                .ok_or_else(|| unsupported("created review comment lacks node ID"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if missing.is_empty() {
        return Ok(());
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
            &json!({"query":"query BulkReviewCommentAnchors($owner:String!,$name:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$cursor){nodes{id diffSide startDiffSide comments(first:1){nodes{id body path line startLine originalLine originalStartLine commit{oid} originalCommit{oid}}}} pageInfo{hasNextPage endCursor}}}}}","variables":{"owner":address.owner,"name":address.repo,"number":address.number,"cursor":cursor}}),
        )?;
        let connection = graphql_data(&response, "repository")?
            .pointer("/pullRequest/reviewThreads")
            .ok_or_else(|| unsupported("review thread connection missing"))?;
        for thread in connection
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| unsupported("review threads missing"))?
        {
            let Some(root) = thread.pointer("/comments/nodes/0") else {
                continue;
            };
            let Some(node) = root.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !missing.remove(node) {
                continue;
            }
            let remote = comments
                .iter_mut()
                .find(|comment| comment.get("node_id").and_then(Value::as_str) == Some(node))
                .expect("missing anchors belong to a known REST comment");
            for (rest, graphql) in [
                ("body", "/body"),
                ("path", "/path"),
                ("line", "/line"),
                ("start_line", "/startLine"),
                ("original_line", "/originalLine"),
                ("original_start_line", "/originalStartLine"),
                ("commit_id", "/commit/oid"),
                ("original_commit_id", "/originalCommit/oid"),
            ] {
                remote[rest] = root.pointer(graphql).cloned().unwrap_or(Value::Null);
            }
            remote["side"] = thread.get("diffSide").cloned().unwrap_or(Value::Null);
            remote["start_side"] = thread.get("startDiffSide").cloned().unwrap_or(Value::Null);
        }
        if missing.is_empty()
            || connection
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                == Some(false)
        {
            return Ok(());
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

/// Recover individual identities from the native bulk operation before advancing.
/// The frozen group and review identity survive a failed discovery read.
fn record_created_review(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &mut Preparation,
    backend: &ReviewBackend,
) -> Result<(), WorkspacePublishError> {
    let pending = preparation
        .pending
        .as_ref()
        .ok_or_else(|| unsupported("created review identity unavailable"))?;
    let mut comments: Vec<Value> = if preparation
        .creating
        .iter()
        .any(|op| matches!(op.change, Change::CreateComment { .. }))
    {
        list(&format!(
            "{}/reviews/{}/comments?per_page=100",
            endpoint(backend)?,
            pending.id
        ))?
    } else {
        vec![]
    };
    hydrate_review_comment_anchors(backend, &mut comments)?;
    comments.sort_by_key(|comment| comment.get("id").and_then(Value::as_u64));
    for operation in preparation.creating.clone() {
        let result = match &operation.change {
            Change::CreateComment { comment } => {
                let CommentTarget::Diff(location) = &comment.target else {
                    return Err(unsupported("bulk review contains a non-root comment").into());
                };
                let payload = super::publish::inline_payload(comment, location);
                let index = comments.iter().position(|remote| {
                    let same = ["body", "path", "side", "start_side"].iter().all(|key| {
                        remote.get(key).filter(|value| !value.is_null()) == payload.get(key)
                    });
                    let current = remote.get("commit_id").is_none_or(|commit| commit.as_str() == Some(location.revision.head.as_str()))
                        && ["line", "start_line"].iter().all(|key| remote.get(key).filter(|value| !value.is_null()) == payload.get(key));
                    let original = remote.get("original_commit_id").and_then(Value::as_str) == Some(location.revision.head.as_str())
                        && [("line", "original_line"), ("start_line", "original_start_line")].iter().all(|(key, original)| remote.get(original).filter(|value| !value.is_null()) == payload.get(key));
                    same && (current || original)
                }).ok_or_else(|| unsupported("bulk review comment identity unconfirmed; inspect provider review before retry"))?;
                let response = comments.remove(index);
                let remote = response
                    .get("id")
                    .and_then(Value::as_u64)
                    .filter(|id| *id > 0)
                    .ok_or_else(|| unsupported("created review comment lacks ID"))?;
                let node = response
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|node| !node.is_empty())
                    .ok_or_else(|| unsupported("created review comment lacks node ID"))?;
                OperationResult {
                    operation: operation.clone(),
                    mappings: vec![
                        IdMapping {
                            resource: "review_comment".into(),
                            core_id: comment.content.id.0.clone(),
                            remote_id: remote.to_string(),
                        },
                        IdMapping {
                            resource: "review_comment_node".into(),
                            core_id: comment.content.id.0.clone(),
                            remote_id: node.into(),
                        },
                    ],
                    response,
                }
            }
            Change::SetReviewConclusion { conclusion } => {
                if preparation
                    .submission_response
                    .get("state")
                    .and_then(Value::as_str)
                    != Some(conclusion_state(conclusion))
                {
                    return Err(unsupported("provider response did not confirm the expected submitted review; reconcile before retry").into());
                }
                OperationResult {
                    operation: operation.clone(),
                    mappings: vec![IdMapping {
                        resource: "review".into(),
                        core_id: operation.id.clone(),
                        remote_id: pending.id.to_string(),
                    }],
                    response: preparation.submission_response.clone(),
                }
            }
            _ => return Err(unsupported("invalid native review batch").into()),
        };
        if !workspace.has_result(&operation.id) {
            preparation.prepared.push(result.clone());
            save_preparation(store, workspace, preparation)?;
            apply_publication(store, workspace, Action::RecordResult { result })?;
        }
    }
    preparation.creating.clear();
    preparation.inflight = None;
    save_preparation(store, workspace, preparation)?;
    Ok(())
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
