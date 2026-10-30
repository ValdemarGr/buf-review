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
    creating: Vec<Operation>,
    #[serde(default)]
    mappings: Vec<IdMapping>,
    #[serde(default)]
    failures: BTreeMap<String, Value>,
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
    apply_publication_group(store, workspace, vec![action])
}

fn apply_publication_group(
    store: &mut Store,
    workspace: &mut Workspace,
    actions: Vec<Action>,
) -> Result<(), WorkspacePublishError> {
    let attempt = workspace
        .attempt
        .as_ref()
        .ok_or_else(|| unsupported("publication attempt disappeared"))?
        .id
        .clone();
    *workspace = store.mutate_publication_group(&workspace.id, &attempt, actions)?;
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
    backend.mappings = backend
        .mappings
        .into_iter()
        .chain(
            preparation
                .prepared
                .iter()
                .flat_map(|result| result.mappings.iter().cloned()),
        )
        .chain(preparation.mappings.iter().cloned())
        .map(|mapping| ((mapping.resource.clone(), mapping.core_id.clone()), mapping))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect();
    Ok(backend)
}

fn publication_levels(
    workspace: &Workspace,
    backend: &ReviewBackend,
    operations: &[Operation],
) -> Result<Vec<Vec<Operation>>, PublishError> {
    let available = backend
        .mappings
        .iter()
        .filter(|mapping| {
            matches!(
                mapping.resource.as_str(),
                "issue_comment" | "review_comment" | "review_summary"
            )
        })
        .map(|mapping| CommentId(mapping.core_id.clone()))
        .collect::<BTreeSet<_>>();
    let comments = operations
        .iter()
        .filter(|operation| {
            !workspace.has_result(&operation.id)
                && matches!(operation.change, Change::CreateComment { .. })
        })
        .cloned()
        .collect::<Vec<_>>();
    let positions = comments
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| match &operation.change {
            Change::CreateComment { comment } => Some((&comment.content.id, index)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut children = vec![Vec::new(); comments.len()];
    let mut ready = Vec::new();
    for (index, operation) in comments.iter().enumerate() {
        match &operation.change {
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Reply(parent),
                        ..
                    },
            } if !available.contains(parent) => {
                let parent = positions.get(parent).ok_or_else(|| {
                    unsupported("publication comment dependencies cannot be resolved")
                })?;
                children[*parent].push(index);
            }
            _ => ready.push(index),
        }
    }
    let mut levels = Vec::new();
    let mut visited = 0;
    while !ready.is_empty() {
        ready.sort_unstable();
        visited += ready.len();
        levels.push(ready.iter().map(|index| comments[*index].clone()).collect());
        ready = ready
            .into_iter()
            .flat_map(|index| std::mem::take(&mut children[index]))
            .collect();
    }
    if visited != comments.len() {
        return Err(unsupported(
            "publication comment dependencies cannot be resolved",
        ));
    }
    let (mut viewed, mut reactions, mut dismissals, mut conclusions) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for operation in operations
        .iter()
        .filter(|operation| !workspace.has_result(&operation.id))
    {
        match operation.change {
            Change::SetFileViewed { .. } => viewed.push(operation.clone()),
            Change::SetCommentReaction { .. } => reactions.push(operation.clone()),
            Change::DismissReview { .. } => dismissals.push(operation.clone()),
            Change::SetReviewConclusion { .. } => conclusions.push(operation.clone()),
            _ => {}
        }
    }
    levels.extend(
        [viewed, reactions, dismissals, conclusions]
            .into_iter()
            .filter(|batch| !batch.is_empty()),
    );
    Ok(levels)
}

/// Resolve planned reply roots once; immutable receipts fill not-yet-fetched parents.
fn publication_roots(
    workspace: &Workspace,
    operations: &[Operation],
) -> Result<BTreeMap<CommentId, CommentId>, PublishError> {
    let mut parents = workspace
        .review
        .read
        .comments
        .iter()
        .map(|comment| {
            (
                &comment.content.id,
                match &comment.target {
                    CommentTarget::Reply(parent) => Some(parent),
                    _ => None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for operation in workspace
        .results
        .iter()
        .map(|result| &result.operation)
        .chain(operations)
    {
        if let Change::CreateComment { comment } = &operation.change {
            parents
                .entry(&comment.content.id)
                .or_insert(match &comment.target {
                    CommentTarget::Reply(parent) => Some(parent),
                    _ => None,
                });
        }
    }
    let mut roots = BTreeMap::<CommentId, CommentId>::new();
    for parent in operations
        .iter()
        .filter_map(|operation| match &operation.change {
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Reply(parent),
                        ..
                    },
            } => Some(parent),
            _ => None,
        })
    {
        let mut current = parent;
        let mut path = BTreeSet::new();
        let root = loop {
            if let Some(root) = roots.get(current) {
                break root.clone();
            }
            if !path.insert(current) {
                return Err(unsupported("cyclic comment replies"));
            }
            match parents.get(current) {
                Some(Some(parent)) => current = parent,
                Some(None) => break current.clone(),
                None => return Err(unsupported("publication reply root cannot be resolved")),
            }
        };
        roots.extend(
            path.into_iter()
                .map(|comment| (comment.clone(), root.clone())),
        );
    }
    Ok(roots)
}

fn send_level(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &mut Preparation,
    backend: &mut ReviewBackend,
    operations: &[Operation],
    roots: &BTreeMap<CommentId, CommentId>,
    pull_request_mapping: &mut Option<IdMapping>,
) -> Result<(), WorkspacePublishError> {
    preparation.mappings.extend(super::batch_ids::complete(
        workspace, backend, operations, roots,
    )?);
    *backend = backend_for(store, workspace, preparation)?;
    backend.mappings.extend(pull_request_mapping.clone());
    let mut inputs = backend.clone();
    let threads = backend
        .mappings
        .iter()
        .filter(|mapping| mapping.resource == "review_thread")
        .rev()
        .map(|mapping| (mapping.core_id.as_str(), mapping.remote_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    for operation in operations {
        if let Change::CreateComment {
            comment:
                DraftComment {
                    target: CommentTarget::Reply(parent),
                    ..
                },
        } = &operation.change
        {
            let root = roots
                .get(parent)
                .ok_or_else(|| unsupported("publication reply root cannot be resolved"))?;
            if let Some(thread) = threads.get(root.0.as_str()) {
                inputs.mappings.push(IdMapping {
                    resource: "review_thread".into(),
                    core_id: parent.0.clone(),
                    remote_id: (*thread).to_owned(),
                });
            }
        }
    }
    preparation.creating = operations.to_vec();
    preparation.inflight = Some("operation batch".into());
    save_preparation(store, workspace, preparation)?;
    let mut outcome = match super::batch::send(
        &workspace.review,
        &inputs,
        preparation.pending.as_ref(),
        operations,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            // Adapter preparation errors occur before any provider mutation.
            preparation.creating.clear();
            preparation.inflight = None;
            apply_publication_group(
                store,
                workspace,
                vec![
                    Action::SetAttemptProvider {
                        provider: serde_json::to_value(&preparation)
                            .expect("provider preparation serializes"),
                    },
                    Action::FinishPublication {
                        success: false,
                        uncertain: false,
                        error: Some(error.to_string()),
                    },
                ],
            )?;
            return Err(error.into());
        }
    };
    if let Some(first) = outcome.results.first_mut() {
        first.mappings.extend(pull_request_mapping.take());
    }
    let mut prepared_ids = preparation
        .prepared
        .iter()
        .map(|result| result.operation.id.clone())
        .collect::<BTreeSet<_>>();
    for result in &outcome.results {
        if prepared_ids.insert(result.operation.id.clone()) {
            preparation.prepared.push(result.clone());
        }
    }
    preparation.creating = outcome
        .failures
        .iter()
        .filter(|failure| failure.uncertain)
        .map(|failure| failure.operation.clone())
        .collect();
    preparation.failures = outcome
        .failures
        .iter()
        .filter(|failure| failure.uncertain)
        .map(|failure| (failure.operation.id.clone(), failure.response.clone()))
        .collect();
    preparation.inflight = (!preparation.creating.is_empty()).then(|| "operation batch".into());
    // Receipts and the remaining unknown-write marker become durable together.
    let mut actions = vec![Action::SetAttemptProvider {
        provider: serde_json::to_value(&preparation).expect("provider preparation serializes"),
    }];
    for result in outcome.results {
        if !workspace.has_result(&result.operation.id) {
            actions.push(Action::RecordResult { result });
        }
    }
    let failure = if outcome.failures.is_empty() {
        None
    } else {
        Some(
            outcome
                .failures
                .iter()
                .map(|failure| format!("{}: {}", failure.operation.id, failure.error))
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    if let Some(message) = &failure {
        for failure in &outcome.failures {
            let lower = failure.error.to_ascii_lowercase();
            if !failure.uncertain
                && lower.contains("http 422")
                && !is_schema_rejection(&failure.error)
                && ["line", "position", "path", "commit"]
                    .iter()
                    .any(|field| lower.contains(field))
                && let Change::CreateComment {
                    comment:
                        DraftComment {
                            content,
                            target: CommentTarget::Diff(_),
                            ..
                        },
                } = &failure.operation.change
            {
                actions.push(Action::RejectAnchor {
                    comment: content.id.clone(),
                    message: failure.error.clone(),
                });
            }
        }
        actions.push(Action::FinishPublication {
            success: false,
            uncertain: !preparation.creating.is_empty(),
            error: Some(message.clone()),
        });
    }
    apply_publication_group(store, workspace, actions)?;
    if let Some(message) = failure {
        return Err(unsupported(message).into());
    }
    *backend = backend_for(store, workspace, preparation)?;
    backend.mappings.extend(pull_request_mapping.clone());
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
    let lower = message.to_ascii_lowercase().replace("\\\"", "\"");
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
            .filter(|operation| preparation.creating.contains(operation))
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

fn record_submission(
    store: &mut Store,
    workspace: &mut Workspace,
    preparation: &Preparation,
    response: &Value,
) -> Result<(), WorkspacePublishError> {
    let mut actions = Vec::new();
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
    if let Some(operation) = conclusion
        && !workspace.has_result(&operation.id)
    {
        let mut mappings = vec![];
        if let Some(pending) = &preparation.pending {
            mappings.push(IdMapping {
                resource: "review".into(),
                core_id: operation.id.clone(),
                remote_id: pending.id.to_string(),
            });
        }
        actions.push(Action::RecordResult {
            result: OperationResult {
                operation,
                mappings,
                response: response.clone(),
            },
        });
    }
    if !actions.is_empty() {
        apply_publication_group(store, workspace, actions)?;
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
) -> Result<Workspace, WorkspacePublishError> {
    crate::progress::report("publish_validate", "Checking publication", None);
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
    crate::progress::report(
        if result.is_ok() {
            "publish_complete"
        } else {
            "publish_failed"
        },
        if result.is_ok() {
            "Publication complete"
        } else {
            "Publication stopped"
        },
        None,
    );
    result
}

fn publication_progress(workspace: &Workspace, operations: &[Operation], change: &Change) {
    let (stage, message) = match change {
        Change::CreateComment { .. } => ("publish_comments", "Comments"),
        Change::SetFileViewed { .. } => ("publish_files", "Files"),
        Change::SetCommentReaction { .. } => ("publish_reactions", "Reactions"),
        Change::DismissReview { .. } => ("publish_dismissals", "Review dismissals"),
        Change::SetReviewConclusion { .. } => ("publish_review", "Review conclusion"),
        _ => return,
    };
    let confirmed = workspace
        .results
        .iter()
        .map(|result| result.operation.id.as_str())
        .collect::<BTreeSet<_>>();
    let family = std::mem::discriminant(change);
    let (completed, total) = operations
        .iter()
        .filter(|operation| std::mem::discriminant(&operation.change) == family)
        .fold((0, 0), |(completed, total), operation| {
            (
                completed + usize::from(confirmed.contains(operation.id.as_str())),
                total + 1,
            )
        });
    crate::progress::report(stage, message, Some((completed, total)));
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
    let validated_head = workspace.comparison.head.clone();
    crate::progress::report("publish_git", "Fetching Git refs", None);
    crate::diff::fetch_all(&project).map_err(|error| FetchError::Response(error.to_string()))?;
    let local_intent = store
        .pull_request(&workspace.review.read.pull_request.id)?
        .is_some_and(|object| matches!(object.source, PullRequestSource::LocalIntent { .. }));
    let mut pull_request_mapping = if local_intent {
        // Local intents validate the remote source head before creating a PR.
        None
    } else {
        crate::progress::report("publish_validate", "Checking remote PR head", None);
        let backend = store
            .review_backend(&workspace.review.id)?
            .ok_or_else(|| unsupported("workspace lacks provider metadata"))?;
        let remote: Value = api(&endpoint(&backend)?, &["--method", "GET"])?;
        let head = remote
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .ok_or_else(|| FetchError::Response("remote PR lacks its source commit".into()))?;
        if head != workspace.comparison.head {
            return Err(unsupported("not at remote head").into());
        }
        let core_id = &workspace.review.read.pull_request.id.0;
        remote
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|node| !node.is_empty())
            .filter(|node| {
                !backend.mappings.iter().any(|mapping| {
                    mapping.resource == "pull_request_node"
                        && mapping.core_id == *core_id
                        && mapping.remote_id == *node
                })
            })
            .map(|node| IdMapping {
                resource: "pull_request_node".into(),
                core_id: core_id.clone(),
                remote_id: node.into(),
            })
    };
    crate::progress::report("publish_prepare", "Preparing pull request", None);
    super::intent::resolve_pull_request(store, &project, &workspace)?;
    workspace = store
        .workspace(id)?
        .ok_or_else(|| WorkspaceError::NotFound(id.into()))?;
    if workspace.comparison.head != validated_head {
        return Err(
            unsupported("Git HEAD changed while preparing publication; reopen diff").into(),
        );
    }
    if workspace.generation != generation {
        return Err(WorkspaceError::StaleGeneration {
            expected: generation,
            actual: workspace.generation,
        }
        .into());
    }
    // A failed publication owns the next retry. Never bypass its uncertain tail.
    if workspace.attempt.is_none()
        && let Some(attempt) = workspace.suspended_attempts.first()
    {
        let attempt_id = attempt.id.clone();
        workspace = store.mutate_workspace(
            id,
            workspace.generation,
            Action::ResumePublication { attempt_id },
        )?;
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
    if let Some(inflight) = preparation.inflight.clone() {
        crate::progress::report(
            "publish_reconcile",
            "Reconciling previous publication",
            None,
        );
        if inflight == "operation batch" {
            let unknown = preparation
                .creating
                .iter()
                .filter(|operation| !workspace.has_result(&operation.id))
                .cloned()
                .collect::<Vec<_>>();
            if unknown
                .iter()
                .any(|operation| matches!(operation.change, Change::CreateComment { .. }))
            {
                let (reconciled, retryable) = super::recovery::comments(
                    store,
                    &workspace,
                    &backend,
                    preparation.pending.as_ref(),
                    &unknown,
                    &preparation.failures,
                )?;
                let resolved = reconciled
                    .iter()
                    .map(|result| result.operation.id.clone())
                    .chain(retryable)
                    .collect::<BTreeSet<_>>();
                preparation.prepared.extend(reconciled.clone());
                preparation.creating.retain(|operation| {
                    !workspace.has_result(&operation.id) && !resolved.contains(&operation.id)
                });
                preparation
                    .failures
                    .retain(|operation, _| !resolved.contains(operation));
                preparation.inflight =
                    (!preparation.creating.is_empty()).then(|| "operation batch".into());
                let actions = std::iter::once(Action::SetAttemptProvider {
                    provider: serde_json::to_value(&preparation)
                        .expect("provider preparation serializes"),
                })
                .chain(
                    reconciled
                        .into_iter()
                        .map(|result| Action::RecordResult { result }),
                )
                .collect();
                apply_publication_group(store, &mut workspace, actions)?;
                if let Some(operation) = unknown.first() {
                    publication_progress(&workspace, &operations, &operation.change);
                }
                if !preparation.creating.is_empty() {
                    return Err(unsupported(
                        "publication comment result uncertain; no duplicate write sent",
                    )
                    .into());
                }
            } else {
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
                let mut reconciled = Vec::new();
                for operation in unknown {
                    let applied = match &operation.change {
                        Change::SetFileViewed { path, viewed, .. } => fresh
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
                        Change::DismissReview { review, .. } => {
                            fresh.read.reviews.iter().any(|item| {
                                item.id.as_ref() == Some(review)
                                    && matches!(
                                        item.state,
                                        ReadReviewState::Inactive {
                                            state: InactiveReviewState::Dismissed,
                                            ..
                                        }
                                    )
                            })
                        }
                        _ => {
                            return Err(unsupported(
                                "uncertain batch contains an unsupported operation",
                            )
                            .into());
                        }
                    };
                    if applied {
                        reconciled.push(OperationResult {
                            operation,
                            mappings: vec![],
                            response: json!({"reconciled":true}),
                        });
                    }
                }
                preparation.prepared.extend(reconciled.clone());
                preparation.creating.clear();
                preparation.failures.clear();
                preparation.inflight = None;
                let actions = std::iter::once(Action::SetAttemptProvider {
                    provider: serde_json::to_value(&preparation)
                        .expect("provider preparation serializes"),
                })
                .chain(
                    reconciled
                        .into_iter()
                        .map(|result| Action::RecordResult { result }),
                )
                .collect();
                apply_publication_group(store, &mut workspace, actions)?;
            }
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
                record_submission(store, &mut workspace, &preparation, &response)?;
                if let Some(operation) = operations.iter().find(|operation| {
                    matches!(operation.change, Change::SetReviewConclusion { .. })
                }) {
                    publication_progress(&workspace, &operations, &operation.change);
                }
            }
            preparation.inflight = None;
            save_preparation(store, &mut workspace, &preparation)?;
        } else if inflight != "create review" {
            return Err(unsupported(format!("publication result uncertain ({inflight}); inspect provider state before retry; no duplicate write sent")).into());
        }
    }
    backend = backend_for(store, &workspace, &preparation)?;
    backend.mappings.extend(pull_request_mapping.clone());
    // Reconcile uncertain writes before deriving the remaining execution view.
    // Keep the frozen plan and raw local history intact; stale viewed changes
    // disappear through the same projection used by the CLI.
    let (_, pending) = workspace.get_unpublished()?;
    let operations = operations
        .iter()
        .filter(|operation| {
            workspace.has_result(&operation.id)
                || !matches!(operation.change, Change::SetFileViewed { .. })
                || pending
                    .operations
                    .iter()
                    .any(|candidate| candidate.change == operation.change)
        })
        .cloned()
        .collect::<Vec<_>>();
    let outstanding = CommitState {
        operations: operations
            .iter()
            .filter(|op| !workspace.has_result(&op.id))
            .cloned()
            .collect(),
    };
    let roots = publication_roots(&workspace, &operations)?;
    preparation.mappings.extend(super::batch_ids::complete(
        &workspace,
        &backend,
        &outstanding.operations,
        &roots,
    )?);
    backend = backend_for(store, &workspace, &preparation)?;
    backend.mappings.extend(pull_request_mapping.clone());
    let levels = publication_levels(&workspace, &backend, &outstanding.operations)?;
    let ordered = CommitState {
        operations: levels.iter().flatten().cloned().collect(),
    };
    preflight(&workspace.review, &backend, &ordered)?;
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
    if heads.len() > 1 {
        return Err(unsupported("review drafts span different head commits; explicitly relocate unsent roots before publication").into());
    }
    let needs_review = outstanding.operations.iter().any(|operation| {
        matches!(
            operation.change,
            Change::CreateComment {
                comment: DraftComment {
                    target: CommentTarget::Diff(_) | CommentTarget::Reply(_),
                    ..
                }
            } | Change::SetReviewConclusion { .. }
        )
    });
    if needs_review {
        crate::progress::report("publish_prepare", "Preparing GitHub review", None);
        let existing: Vec<Value> = list(&format!("{}/reviews?per_page=100", endpoint(&backend)?))?;
        let user = backend
            .mappings
            .iter()
            .find(|mapping| {
                mapping.resource == "user" && mapping.core_id == workspace.review.reviewer.id.0
            })
            .ok_or_else(|| unsupported("reviewer mapping missing"))?;
        let mut pending = existing
            .into_iter()
            .filter(|review| {
                review.get("state").and_then(Value::as_str) == Some("PENDING")
                    && review
                        .pointer("/user/id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id.to_string() == user.remote_id)
            })
            .collect::<Vec<_>>();
        if pending.len() > 1 {
            return Err(
                unsupported("multiple pending provider reviews require reconciliation").into(),
            );
        }
        if let Some(response) = pending.pop() {
            let head = response
                .get("commit_id")
                .and_then(Value::as_str)
                .filter(|head| !head.is_empty())
                .ok_or_else(|| unsupported("pending review lacks commit identity"))?;
            if heads.iter().any(|expected| expected.as_str() != head) {
                return Err(unsupported("comment and pending review commits differ").into());
            }
            preparation.pending = Some(PendingReview {
                id: response
                    .get("id")
                    .and_then(Value::as_u64)
                    .filter(|id| *id > 0)
                    .ok_or_else(|| unsupported("pending review lacks ID"))?,
                node_id: response
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| unsupported("pending review lacks node ID"))?
                    .into(),
                head: head.into(),
            });
        } else {
            if preparation.inflight.as_deref() == Some("create review") {
                return Err(unsupported("pending review creation uncertain; inspect provider state before retry; no duplicate write sent").into());
            }
            // A complete fresh listing confirms that a previously cached draft
            // was submitted or discarded. Existing receipts remain immutable;
            // only unsent work belongs to the replacement review.
            preparation.pending = None;
            let head = heads
                .first()
                .map(|head| (*head).clone())
                .unwrap_or_else(|| {
                    if workspace.worktree_context {
                        workspace.comparison.head.clone()
                    } else {
                        workspace.review.read.pull_request.revision.head.clone()
                    }
                });
            preparation.inflight = Some("create review".into());
            save_preparation(store, &mut workspace, &preparation)?;
            match request(
                &format!("{}/reviews", endpoint(&backend)?),
                "POST",
                &json!({"commit_id":head}),
            ) {
                Ok(response) => {
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
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        }
        preparation.inflight = None;
        save_preparation(store, &mut workspace, &preparation)?;
    }
    for level in levels {
        if let Some(Operation {
            change: Change::SetReviewConclusion { conclusion },
            ..
        }) = level.first()
        {
            let operation = level[0].clone();
            publication_progress(&workspace, &operations, &operation.change);
            let pending = preparation
                .pending
                .as_ref()
                .ok_or_else(|| unsupported("conclusion requires a pending review"))?
                .clone();
            preparation.inflight = Some("submit".into());
            save_preparation(store, &mut workspace, &preparation)?;
            match submit_pending_review(&backend, &pending, conclusion) {
                Ok(mut result) => {
                    result.mappings.extend(pull_request_mapping.take());
                    preparation.submitted = true;
                    let result = OperationResult {
                        operation,
                        mappings: result.mappings,
                        response: result.response,
                    };
                    preparation.prepared.push(result.clone());
                    preparation.inflight = None;
                    apply_publication_group(
                        store,
                        &mut workspace,
                        vec![
                            Action::SetAttemptProvider {
                                provider: serde_json::to_value(&preparation)
                                    .expect("provider preparation serializes"),
                            },
                            Action::RecordResult { result },
                        ],
                    )?;
                    publication_progress(&workspace, &operations, &level[0].change);
                }
                Err(error) => {
                    fail_attempt(store, &mut workspace, &mut preparation, &error)?;
                    return Err(error.into());
                }
            }
        } else {
            for chunk in level.chunks(super::batch::MAX_OPERATIONS) {
                publication_progress(&workspace, &operations, &chunk[0].change);
                let sent = send_level(
                    store,
                    &mut workspace,
                    &mut preparation,
                    &mut backend,
                    chunk,
                    &roots,
                    &mut pull_request_mapping,
                );
                publication_progress(&workspace, &operations, &chunk[0].change);
                sent?;
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

pub(super) fn canonicalize_fetched(fetched: &mut FetchedReview, backend: &ReviewBackend) {
    let aliases = fetched
        .backend
        .mappings
        .iter()
        .filter(|mapping| {
            matches!(
                mapping.resource.as_str(),
                "issue_comment" | "review_comment" | "review_summary" | "review" | "user"
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
    for mapping in &mut fetched.backend.mappings {
        rename(&mut mapping.core_id);
    }
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
    for review in &mut fetched.read.reviews {
        if let Some(id) = &mut review.id {
            rename(&mut id.0);
        }
        if let Some(author) = &mut review.author {
            rename(&mut author.id.0);
        }
    }
}

#[cfg(test)]
mod publication_level_tests {
    use super::*;

    fn fixture() -> (Workspace, ReviewBackend) {
        let review = serde_json::from_value(json!({
            "id":"snapshot","reviewer":{"id":"me","name":"Reviewer"},
            "read":{"pull_request":{"id":"pr","title":"PR","body":"","author":null,"status":"open","source":null,"revision":{"base":"base","head":"head"}},
                "files":[],"comments":[],"reactions":[],"reviews":[]}
        })).unwrap();
        (
            Workspace::new(review),
            ReviewBackend {
                source: ReviewSource {
                    backend: "github".into(),
                    locator: "https://github.com/owner/repo/pull/1".into(),
                },
                mappings: Vec::new(),
            },
        )
    }

    fn create(id: &str, parent: Option<&str>) -> Operation {
        Operation {
            id: id.into(),
            change: Event::CreateComment {
                comment: DraftComment {
                    content: CommentContent {
                        id: CommentId(id.into()),
                        body: id.into(),
                    },
                    author: User {
                        id: UserId("me".into()),
                        name: "Reviewer".into(),
                    },
                    target: parent
                        .map(|parent| CommentTarget::Reply(CommentId(parent.into())))
                        .unwrap_or(CommentTarget::PullRequest),
                },
            },
        }
    }

    #[test]
    fn dependency_levels_keep_input_order_within_each_depth() {
        let (workspace, backend) = fixture();
        let operations = [
            create("c", Some("b")),
            create("a", None),
            create("b", Some("a")),
            create("d", None),
            create("e", Some("d")),
        ];
        let levels = publication_levels(&workspace, &backend, &operations).unwrap();
        let roots = publication_roots(&workspace, &operations).unwrap();
        assert_eq!(roots[&CommentId("b".into())], CommentId("a".into()));
        assert_eq!(roots[&CommentId("d".into())], CommentId("d".into()));
        let ids = levels
            .iter()
            .map(|level| {
                level
                    .iter()
                    .map(|operation| operation.id.as_str())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, [vec!["a", "d"], vec!["b", "e"], vec!["c"]]);
    }

    #[test]
    fn missing_parents_and_cycles_fail_before_execution() {
        let (workspace, backend) = fixture();
        for operations in [
            vec![create("a", Some("missing"))],
            vec![create("a", Some("b")), create("b", Some("a"))],
        ] {
            assert!(publication_levels(&workspace, &backend, &operations).is_err());
            assert!(publication_roots(&workspace, &operations).is_err());
        }
    }

    #[test]
    fn long_reply_chain_is_traversed_without_recursive_search() {
        let (workspace, backend) = fixture();
        let operations = (0..5000)
            .rev()
            .map(|index| {
                create(
                    &index.to_string(),
                    (index > 0).then(|| (index - 1).to_string()).as_deref(),
                )
            })
            .collect::<Vec<_>>();
        let levels = publication_levels(&workspace, &backend, &operations).unwrap();
        let roots = publication_roots(&workspace, &operations).unwrap();
        assert_eq!(roots[&CommentId("4998".into())], CommentId("0".into()));
        assert_eq!(levels.len(), 5000);
        assert!(levels.iter().all(|level| level.len() == 1));
        assert_eq!(levels[0][0].id, "0");
        assert_eq!(levels[4999][0].id, "4999");
    }
}
