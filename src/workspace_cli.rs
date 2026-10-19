//! Canonical process boundary for local action lists, reads and publication.
use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::{
    github,
    model::*,
    store::{BatchRoute, Store, StoreError},
    workspace::{Action, Workspace, WorkspaceError},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
struct Bound {
    workspace: String,
    generation: Option<u64>,
}

#[derive(Deserialize)]
struct Open {
    url: String,
}

#[derive(Serialize, Deserialize)]
struct PreparedRefresh {
    workspace: String,
    generation: u64,
    #[serde(flatten)]
    outcome: RefreshOutcome,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RefreshOutcome {
    Snapshot { review: ReviewId },
    Error { error: String },
    Deferred { deferred: bool },
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default = "open_state")]
    state: String,
    assignee: Option<String>,
}

#[derive(Serialize)]
struct BrowserPullRequest {
    id: PullRequestId,
    title: String,
    url: Option<String>,
    number: Option<u64>,
    source: Option<PullRequestSource>,
}

#[derive(Serialize)]
struct BrowserBatch {
    id: String,
    reviewer: User,
    active: bool,
    stats: crate::sessions::BatchStats,
}

#[derive(Serialize)]
struct BatchGroup {
    pr: BrowserPullRequest,
    sessions: Vec<BrowserBatch>,
}

#[derive(Serialize)]
struct SessionBrowser {
    pr: Option<String>,
    groups: Vec<BatchGroup>,
}

fn open_state() -> String {
    "open".into()
}

/// Resolves the working-tree root without checking out or modifying files.
pub fn repository_root(directory: &Path) -> Result<PathBuf> {
    let result = Command::new("git")
        .current_dir(directory)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other("Review requires a Git working tree").into());
    }
    Ok(PathBuf::from(String::from_utf8(result.stdout)?.trim_end()))
}

pub fn run(directory: &Path, request: Value) -> Result<Value> {
    let project = repository_root(directory)?;
    let op = request
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("workspace request requires op"))?
        .to_owned();
    if op == "list" {
        let query: Listing = serde_json::from_value(request)?;
        let repository = github::repository()?;
        let assignee = query.assignee.as_deref().filter(|s| *s != "all");
        return Ok(
            json!({"pull_requests":github::pull_requests(&repository,&query.state,assignee)?}),
        );
    }
    let mut store = Store::open(&project)?;
    if op == "pr" {
        let base = request["base"]
            .as_str()
            .ok_or_else(|| io::Error::other("Review pr requires a base branch"))?;
        let branches = github::pr_branches(&project, base)?;
        if let Some(url) = github::matching_pr_url(&branches)? {
            if let Some(intent) = store.intent_for_branches(&branches)?
                && let Some(remote) = store.pull_request_for_locator(&url)?
                && intent.id != remote.id
            {
                return Err(io::Error::other("remote PR already belongs to another local PR object; both sessions were preserved").into());
            }
            return route_pr(&mut store, &url, Some(&branches));
        }
        let object = store.create_or_use_intent(&branches, "", "")?;
        let reviewer = github::current_user()?;
        let mut read = github::local_read(&project, &object)?;
        read.pull_request.author = Some(reviewer.clone());
        let review = store.cache_review(&reviewer, &read)?;
        return route_review(&mut store, &review, true);
    }
    if op == "sessions" {
        return Ok(json!({"sessions": sessions_browser(
            &store, request["pr"].as_str(), request["workspace"].as_str()
        )?}));
    }
    if op == "select" {
        let batch = request["batch"]
            .as_str()
            .ok_or_else(|| io::Error::other("selection requires a batch ID"))?;
        let workspace = store.select_batch(&BatchId(batch.into()))?;
        return Ok(json!({
            "workspace": store.view_workspace(&workspace)?,
            "fresh": request["fresh_review"].as_str() == Some(workspace.review.id.0.as_str()),
        }));
    }
    if op == "entry" {
        let pulls = github::branch_pull_requests(&project)?;
        return if pulls.len() == 1 {
            route_pr(&mut store, &pulls[0].url, None)
        } else {
            let repository = github::repository()?;
            Ok(
                json!({"route":"prs", "pull_requests":github::pull_requests(&repository,"open",None)?}),
            )
        };
    }
    if op == "route" {
        let input: Open = serde_json::from_value(request)?;
        return route_pr(&mut store, &input.url, None);
    }
    if op == "new" {
        let mut fresh_review = None;
        let review = if let Some(url) = request["url"].as_str() {
            let (review, fresh) = review_for_pr(&mut store, url, None)?;
            if fresh {
                fresh_review = Some(review.clone());
            }
            review
        } else if let Some(id) = request["workspace"].as_str() {
            store
                .workspace(id)?
                .ok_or_else(|| io::Error::other("batch does not exist"))?
                .review
                .id
        } else if let Some(pr) = request["pr"].as_str() {
            let active = store.last_active_workspace()?;
            let mut candidates = store
                .batch_workspaces()?
                .into_iter()
                .filter(|workspace| {
                    workspace.review.read.pull_request.id.0 == pr
                        || workspace.locator.as_deref() == Some(pr)
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|workspace| {
                active
                    .as_ref()
                    .is_none_or(|active| active.review.reviewer.id != workspace.review.reviewer.id)
            });
            candidates
                .into_iter()
                .next()
                .ok_or_else(|| io::Error::other("no cached snapshot for selected PR"))?
                .review
                .id
        } else {
            store
                .last_active_workspace()?
                .ok_or_else(|| io::Error::other("select a PR before creating a batch"))?
                .review
                .id
        };
        let workspace = store.create_batch_workspace(&review)?;
        let selected = store.select_batch(&BatchId(workspace.id))?;
        return Ok(json!({
            "workspace":store.view_workspace(&selected)?,
            "fresh":fresh_review.as_ref() == Some(&selected.review.id),
        }));
    }
    if op == "active" {
        return match store.last_active_workspace()? {
            Some(workspace) => Ok(json!({"workspace":store.view_workspace(&workspace)?})),
            None => Ok(json!({"workspace":null})),
        };
    }
    if op == "open" {
        let input: Open = serde_json::from_value(request)?;
        let route = route_pr(&mut store, &input.url, None)?;
        if let Some(id) = route["batch"].as_str() {
            let workspace = store.select_batch(&BatchId(id.into()))?;
            return Ok(json!({
                "workspace":store.view_workspace(&workspace)?,
                "fresh": route["fresh_review"].as_str() == Some(workspace.review.id.0.as_str()),
            }));
        }
        return Ok(route);
    }
    let bound: Bound = serde_json::from_value(request.clone())?;
    if op == "actions" {
        let actions: Vec<Action> = serde_json::from_value(request["actions"].clone())?;
        let result =
            store.mutate_workspace_actions(&bound.workspace, generation(&bound)?, actions)?;
        let current = store.view_workspace(&result.workspace)?;
        let mut response = json!({
            "comment": if result.applied > 0 { current.saved_comment.clone() } else { None },
            "workspace": current,
            "applied": result.applied,
        });
        if let Some((index, error)) = result.failure {
            response["failed_index"] = json!(index);
            response["error"] = json!(error.to_string());
        }
        return Ok(response);
    }
    let workspace = store
        .workspace(&bound.workspace)?
        .ok_or_else(|| io::Error::other("workspace does not exist"))?;
    match op.as_str() {
        "get" => Ok(json!({"workspace":store.view_workspace(&workspace)?})),
        "fetch_objects" => crate::context::prepare(&mut store, &project, &workspace),
        "code" => {
            let path = request["path"]
                .as_str()
                .ok_or_else(|| io::Error::other("code requires a file path"))?;
            Ok(json!({"code":crate::context::code(&project, &workspace, path)?}))
        }
        "historical_code" => {
            let id: CommentId = serde_json::from_value(request["comment"].clone())?;
            crate::context::historical_code(&store, &project, &workspace, &id)
        }
        "context" => {
            let id: CommentId = serde_json::from_value(request["comment"].clone())?;
            Ok(json!({"context":crate::context::read(&store, &project, &workspace, &id)?}))
        }
        "log" => Ok(json!({"log":crate::log::project(store.workspace_log(&bound.workspace)?)?})),
        "history" => {
            let details = store.publication_history(&workspace)?;
            if let Some(id) = request["publication"].as_str() {
                let detail = details
                    .into_iter()
                    .find(|detail| detail.summary.id == id)
                    .ok_or_else(|| io::Error::other("publication does not exist"))?;
                Ok(json!({"publication":detail}))
            } else {
                Ok(
                    json!({"publications":details.into_iter().map(|detail| detail.summary).collect::<Vec<_>>()}),
                )
            }
        }
        "fetch_refresh" => Ok(json!({"prepared_refresh": fetch_refresh(&mut store, &workspace)})),
        "apply_refresh" => {
            generation(&bound)?;
            let prepared: PreparedRefresh =
                serde_json::from_value(request["prepared_refresh"].clone())?;
            apply_refresh(
                &mut store,
                &bound.workspace,
                &prepared,
                request["compare_latest"].as_bool().unwrap_or(false),
            )
        }
        "refresh" | "compare_latest" => {
            generation(&bound)?;
            let prepared = fetch_refresh(&mut store, &workspace);
            apply_refresh(
                &mut store,
                &bound.workspace,
                &prepared,
                op == "compare_latest",
            )
        }
        "publish" | "submit" => {
            let result = github::publish_workspace(
                &mut store,
                &bound.workspace,
                generation(&bound)?,
                op == "submit",
            );
            match result {
                Ok(_) => Ok(json!({"workspace":store.workspace_view(&bound.workspace)?})),
                Err(error) => Ok(
                    json!({"workspace":store.workspace_view(&bound.workspace)?,"error":error.to_string()}),
                ),
            }
        }
        _ => Err(io::Error::other(format!("unknown workspace operation: {op}")).into()),
    }
}

fn review_for_pr(
    store: &mut Store,
    url: &str,
    branches: Option<&BranchPair>,
) -> Result<(ReviewId, bool)> {
    // Cached metadata locates a baseline; unfinished-batch routing is separate.
    if let Some(cached) = store.cached_workspace(url)?
        && github::current_user().map_or(true, |user| user.id == cached.review.reviewer.id)
    {
        return Ok((cached.review.id, false));
    }
    let fetched = github::fetch(&Url::parse(url)?)?;
    let id = store.cache_import_review(
        &fetched.reviewer,
        &fetched.read,
        &fetched.backend,
        branches.or(fetched.branches.as_ref()),
    )?;
    Ok((id, true))
}

fn route_pr(store: &mut Store, url: &str, branches: Option<&BranchPair>) -> Result<Value> {
    let (review, fresh) = review_for_pr(store, url, branches)?;
    route_review(store, &review, fresh)
}

fn route_review(store: &mut Store, review: &ReviewId, fresh: bool) -> Result<Value> {
    match store.resolve_workspace(review)? {
        BatchRoute::Open(workspace) => {
            let mut route = json!({"route":"batch", "batch":workspace.id});
            if fresh && workspace.review.id == *review {
                route["fresh_review"] = json!(review);
            }
            Ok(route)
        }
        BatchRoute::Choose(workspaces) => {
            let pr = &workspaces[0].review.read.pull_request.id.0;
            Ok(json!({"route":"sessions", "sessions":sessions_browser(store,Some(pr),None)?}))
        }
    }
}

fn sessions_browser(
    store: &Store,
    filter: Option<&str>,
    context: Option<&str>,
) -> Result<SessionBrowser> {
    let active = store.last_active_workspace()?;
    let current = match context {
        Some(id) => store.workspace(id)?,
        None => active.clone(),
    };
    let preferred = current
        .as_ref()
        .map(|workspace| &workspace.review.read.pull_request.id);
    let mut groups = std::collections::BTreeMap::<PullRequestId, BatchGroup>::new();
    for workspace in store.batch_workspaces()? {
        let pull = &workspace.review.read.pull_request;
        let object = store.pull_request(&pull.id)?;
        if filter.is_some_and(|filter| {
            filter != pull.id.0 && workspace.locator.as_deref() != Some(filter)
        }) {
            continue;
        }
        let number = workspace
            .locator
            .as_deref()
            .and_then(|url| url.rsplit('/').next()?.parse::<u64>().ok());
        let stats = workspace.stats(&workspace.pending()?.operations);
        let row = BrowserBatch {
            active: active
                .as_ref()
                .is_some_and(|active| active.id == workspace.id),
            id: workspace.id,
            reviewer: workspace.review.reviewer,
            stats,
        };
        let group = groups.entry(pull.id.clone()).or_insert_with(|| BatchGroup {
            pr: BrowserPullRequest {
                id: pull.id.clone(),
                title: object
                    .as_ref()
                    .map_or_else(|| pull.title.clone(), |object| object.title.clone()),
                url: workspace.locator,
                number,
                source: object.map(|object| object.source),
            },
            sessions: vec![],
        });
        group.sessions.push(row);
    }
    let mut ordered = groups.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (Some(&left.pr.id) != preferred)
            .cmp(&(Some(&right.pr.id) != preferred))
            .then_with(|| left.pr.id.cmp(&right.pr.id))
    });
    for group in &mut ordered {
        group.sessions.sort_by(|left, right| {
            right
                .stats
                .unfinished
                .cmp(&left.stats.unfinished)
                .then_with(|| left.id.cmp(&right.id))
        });
    }
    Ok(SessionBrowser {
        pr: filter.map(str::to_owned),
        groups: ordered,
    })
}

fn refresh_still_current(before: &Workspace, latest: &Workspace) -> bool {
    latest.attempt.is_none()
        && latest.review.id == before.review.id
        && latest.locator == before.locator
        && latest.results == before.results
}

/// Fetch immutable provider evidence without changing this session's local intent.
fn fetch_refresh(store: &mut Store, before: &Workspace) -> PreparedRefresh {
    let result = (|| -> Result<Option<ReviewId>> {
        if before.attempt.is_some() {
            return Ok(None);
        }
        let object = store.pull_request(&before.review.read.pull_request.id)?;
        let fetched = if let Some(object) = object
            && matches!(object.source, PullRequestSource::LocalIntent { .. })
        {
            let read = github::local_read(store.project_root(), &object)?;
            let review = store.cache_review(&before.review.reviewer, &read)?;
            return Ok(Some(review));
        } else {
            let backend = store
                .review_backend(&before.review.id)?
                .ok_or_else(|| io::Error::other("workspace has no provider metadata"))?;
            github::fetch(&Url::parse(&backend.source.locator)?)?
        };
        if fetched.reviewer.id != before.review.reviewer.id {
            return Err(io::Error::other("authenticated user differs from saved reviewer").into());
        }
        let latest = store
            .workspace(&before.id)?
            .ok_or_else(|| io::Error::other("workspace does not exist"))?;
        if !refresh_still_current(before, &latest) {
            return Ok(None);
        }
        let review = store.cache_import_review(
            &fetched.reviewer,
            &fetched.read,
            &fetched.backend,
            fetched.branches.as_ref(),
        )?;
        Ok(Some(review))
    })();
    PreparedRefresh {
        workspace: before.id.clone(),
        generation: before.generation,
        outcome: match result {
            Ok(Some(review)) => RefreshOutcome::Snapshot { review },
            Ok(None) => RefreshOutcome::Deferred { deferred: true },
            Err(error) => RefreshOutcome::Error {
                error: error.to_string(),
            },
        },
    }
}

/// Commit fetched metadata in the same ordered local-writer boundary as other actions.
fn apply_refresh(
    store: &mut Store,
    id: &str,
    prepared: &PreparedRefresh,
    compare_latest: bool,
) -> Result<Value> {
    let before = store
        .workspace_at_generation(id, prepared.generation)?
        .ok_or_else(|| io::Error::other("prepared refresh generation does not exist"))?;
    if prepared.workspace != before.id {
        return Err(io::Error::other("prepared refresh belongs to another workspace").into());
    }
    // Metadata rebases onto current local intent, including writes from other editors.
    // Only provider evidence, receipts and the explicit comparison pin can invalidate it.
    for _ in 0..4 {
        let latest = store
            .workspace(id)?
            .ok_or_else(|| io::Error::other("workspace does not exist"))?;
        if !refresh_still_current(&before, &latest)
            || compare_latest && latest.comparison != before.comparison
        {
            return refresh_deferred(store, id);
        }
        let result = (|| -> Result<Value> {
            match &prepared.outcome {
                RefreshOutcome::Deferred { .. } => refresh_deferred(store, id),
                RefreshOutcome::Error { error } => {
                    let workspace = store.mutate_workspace(
                        id,
                        latest.generation,
                        Action::RefreshError {
                            error: error.clone(),
                        },
                    )?;
                    Ok(json!({"workspace":store.view_workspace(&workspace)?,"error":error}))
                }
                RefreshOutcome::Snapshot { review } => {
                    let mut workspace = store.refresh_workspace(id, latest.generation, review)?;
                    if compare_latest {
                        workspace = store.mutate_workspace(
                            id,
                            workspace.generation,
                            Action::CompareLatest,
                        )?;
                    }
                    Ok(json!({"workspace":store.view_workspace(&workspace)?}))
                }
            }
        })();
        match result {
            Err(error)
                if matches!(
                    error.downcast_ref::<StoreError>(),
                    Some(StoreError::Workspace(
                        WorkspaceError::StaleGeneration { .. }
                    ))
                ) =>
            {
                continue;
            }
            result => return result,
        }
    }
    refresh_deferred(store, id)
}

fn refresh_deferred(store: &mut Store, id: &str) -> Result<Value> {
    Ok(json!({"workspace":store.workspace_view(id)?,"refresh_deferred":true}))
}

fn generation(bound: &Bound) -> Result<u64> {
    bound
        .generation
        .ok_or_else(|| io::Error::other("mutation requires workspace generation").into())
}
