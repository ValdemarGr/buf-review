//! Original comment context comes from Git; snapshots and receipts retain anchors.
use crate::{diff, github, model::*, store::Store, workspace::Workspace};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, error::Error, path::Path};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProvenance {
    Recorded,
    Reconstructed,
}
#[derive(Debug, Clone, Serialize)]
struct Comparison {
    revision: DiffRevision,
    provenance: ContextProvenance,
}
pub fn original_anchor(
    comments: &[crate::workspace::CommentView],
    id: &CommentId,
) -> Result<Option<(ReadCommentAnchor, Option<String>)>> {
    let mut id = id;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        if !seen.insert(id.clone()) {
            return Err(std::io::Error::other("reply ancestry contains a cycle").into());
        }
        let row = comments
            .iter()
            .find(|row| &row.comment.content().id == id)
            .ok_or_else(|| std::io::Error::other("comment does not exist"))?;
        match &row.comment {
            crate::core::ProjectedComment::Read(comment) => match &comment.target {
                CommentTarget::Diff(location) => {
                    return Ok(Some((
                        location.original.clone(),
                        location.diff_hunk.clone(),
                    )));
                }
                CommentTarget::Reply(parent) => id = parent,
                CommentTarget::PullRequest => return Ok(None),
            },
            crate::core::ProjectedComment::Draft(comment) => match &comment.target {
                CommentTarget::Diff(location) => {
                    return Ok(Some((
                        ReadCommentAnchor {
                            revision: ReadCommentRevision::Comparison(location.revision.clone()),
                            position: location.position.clone(),
                        },
                        None,
                    )));
                }
                CommentTarget::Reply(parent) => id = parent,
                CommentTarget::PullRequest => return Ok(None),
            },
        }
    }
}

fn base_path(
    store: &Store,
    workspace: &Workspace,
    revision: &DiffRevision,
    path: &str,
) -> Result<Option<String>> {
    let files = if revision == &workspace.comparison {
        Some(workspace.comparison_files.clone())
    } else if revision == &workspace.review.read.pull_request.revision {
        Some(workspace.review.read.files.clone())
    } else {
        let mut files = None;
        for id in store.review_ids()? {
            if let Some(review) = store.get_review(&id)?
                && review.read.pull_request.id == workspace.review.read.pull_request.id
                && &review.read.pull_request.revision == revision
            {
                files = Some(review.read.files);
                break;
            }
        }
        files
    };
    Ok(files
        .and_then(|files| files.into_iter().find(|file| file.path == path))
        .map(|file| file.previous_path.unwrap_or_else(|| path.into())))
}

/// Prepare current history first; fetch exceptional comment commits together.
pub fn prepare(store: &mut Store, project: &Path, workspace: &Workspace) -> Result<Value> {
    crate::progress::report("prepare_git", "Preparing Git objects", None);
    let mut current = vec![
        workspace.review.read.pull_request.revision.base.clone(),
        diff::resolve(project, "HEAD")?,
        workspace.review.read.pull_request.revision.head.clone(),
    ];
    current.extend(
        workspace
            .batch
            .write
            .events
            .iter()
            .filter_map(|event| match &event.event {
                Event::SetFileViewed {
                    commit: Some(commit),
                    ..
                } => Some(commit.clone()),
                _ => None,
            }),
    );
    let local = store
        .pull_request(&workspace.review.read.pull_request.id)?
        .is_some_and(|pr| matches!(pr.source, PullRequestSource::LocalIntent { .. }));
    crate::progress::report("prepare_current", "Preparing current commits", None);
    if local {
        let missing = diff::missing_commits(project, &current)?;
        if !missing.is_empty() {
            return Err(std::io::Error::other(format!(
                "local review commits unavailable: {}; restore Git objects before opening code",
                missing.join(", ")
            ))
            .into());
        }
    } else {
        let backend = store
            .review_backend(&workspace.review.id)?
            .ok_or_else(|| std::io::Error::other("workspace has no provider metadata"))?;
        github::fetch_workspace_commits(project, &current, &backend)?;
    }
    let mut commits = BTreeSet::new();
    for row in &workspace.view()?.comments {
        let revision = match &row.comment {
            crate::core::ProjectedComment::Read(ReadComment {
                target: CommentTarget::Diff(location),
                ..
            }) => location.original.revision.clone(),
            crate::core::ProjectedComment::Draft(DraftComment {
                target: CommentTarget::Diff(location),
                ..
            }) => ReadCommentRevision::Comparison(location.revision.clone()),
            _ => continue,
        };
        match revision {
            ReadCommentRevision::Comparison(pair) => {
                commits.insert(pair.base);
                commits.insert(pair.head);
            }
            ReadCommentRevision::Commit { head } => {
                commits.insert(head);
                commits.insert(workspace.review.read.pull_request.revision.base.clone());
            }
        }
    }
    let commits = commits.into_iter().collect::<Vec<_>>();
    crate::progress::report("prepare_history", "Preparing historical commits", None);
    let mut response = json!({"revision":workspace.comparison});
    if store
        .pull_request(&workspace.review.read.pull_request.id)?
        .is_some_and(|pr| matches!(pr.source, PullRequestSource::LocalIntent { .. }))
    {
        let missing = diff::missing_commits(project, &[current.as_slice(), &commits].concat())?;
        if missing.iter().any(|commit| current.contains(commit)) {
            return Err(std::io::Error::other(format!(
                "local review commits unavailable: {}; restore Git objects before opening code",
                missing.join(", ")
            ))
            .into());
        }
        if !missing.is_empty() {
            response["warnings"] = json!([format!(
                "local historical commits unavailable: {}",
                missing.join(", ")
            )]);
            response["missing_commits"] = json!(missing);
        }
        crate::progress::report("prepare_comparison", "Preparing comparison", None);
        let current = store.prepare_worktree(&workspace.id)?;
        response["revision"] = json!(current.comparison);
        response["workspace"] = serde_json::to_value(store.view_workspace(&current)?)?;
        crate::progress::report("prepare_git", "Git preparation complete", None);
        return Ok(response);
    }
    let backend = store
        .review_backend(&workspace.review.id)?
        .ok_or_else(|| std::io::Error::other("workspace has no provider metadata"))?;
    if let Err(error) = github::fetch_workspace_commits(project, &commits, &backend) {
        response["warnings"] = json!([error.to_string()]);
        response["missing_commits"] =
            json!(diff::missing_commits(project, &commits).unwrap_or(commits));
    }
    crate::progress::report("prepare_comparison", "Preparing comparison", None);
    let current = store.prepare_worktree(&workspace.id)?;
    response["revision"] = json!(current.comparison);
    response["workspace"] = serde_json::to_value(store.view_workspace(&current)?)?;
    crate::progress::report("prepare_git", "Git preparation complete", None);
    Ok(response)
}

struct OriginalContext {
    anchor: ReadCommentAnchor,
    hunk: Option<String>,
    comparison: Option<Comparison>,
    base_path: Option<String>,
    base: std::result::Result<Option<Vec<u8>>, diff::DiffError>,
    head: std::result::Result<Option<Vec<u8>>, diff::DiffError>,
}

fn load(
    store: &Store,
    project: &Path,
    workspace: &Workspace,
    id: &CommentId,
) -> Result<Option<OriginalContext>> {
    let Some((anchor, hunk)) = original_anchor(&workspace.view()?.comments, id)? else {
        return Ok(None);
    };
    let comparison = match &anchor.revision {
        ReadCommentRevision::Comparison(pair) => Some(Comparison {
            revision: pair.clone(),
            provenance: ContextProvenance::Recorded,
        }),
        ReadCommentRevision::Commit { head } => diff::merge_base(
            project,
            &DiffRevision {
                base: workspace.review.read.pull_request.revision.base.clone(),
                head: head.clone(),
            },
        )
        .ok()
        .map(|base| Comparison {
            revision: DiffRevision {
                base,
                head: head.clone(),
            },
            provenance: ContextProvenance::Reconstructed,
        }),
    };
    let path = &anchor.position.path;
    let base_path = if let Some(pair) = &comparison {
        base_path(store, workspace, &pair.revision, path)?.or_else(|| {
            diff::renamed_path(project, &pair.revision, path, DiffSide::Head)
                .ok()
                .map(|previous| previous.unwrap_or_else(|| path.clone()))
        })
    } else {
        None
    };
    let head_commit = match &anchor.revision {
        ReadCommentRevision::Comparison(pair) => &pair.head,
        ReadCommentRevision::Commit { head } => head,
    };
    let head = diff::contents(project, head_commit, path);
    let base = match (&comparison, &base_path) {
        (Some(pair), Some(path)) => diff::contents(project, &pair.revision.base, path),
        _ => Err(diff::DiffError::Git(
            "Original comparison base unavailable".into(),
        )),
    };
    Ok(Some(OriginalContext {
        anchor,
        hunk,
        comparison,
        base_path,
        base,
        head,
    }))
}

pub fn read(store: &Store, project: &Path, workspace: &Workspace, id: &CommentId) -> Result<Value> {
    let Some(ctx) = load(store, project, workspace, id)? else {
        return Ok(json!({"available":false,"comment":id,"message":"No code location"}));
    };
    let exact = ctx
        .comparison
        .as_ref()
        .is_some_and(|pair| pair.provenance == ContextProvenance::Recorded);
    let base = if exact { ctx.base.as_ref().ok() } else { None };
    let head = ctx.head.as_ref().ok();
    let base_side = ctx
        .anchor
        .position
        .range
        .as_ref()
        .is_some_and(|range| range.start.side == DiffSide::Base);
    let content = if base_side { base } else { head }.and_then(|content| content.as_deref());
    Ok(json!({
        "available":content.is_some(), "comment":id, "anchor":ctx.anchor,
        "comparison":ctx.comparison,"base_path":ctx.base_path,
        "content":content.map(String::from_utf8_lossy),
        "base_content":base.map(|content| String::from_utf8_lossy(content.as_deref().unwrap_or_default())),
        "head_content":if exact { head.map(|content| String::from_utf8_lossy(content.as_deref().unwrap_or_default())) } else { None },
        "diff_hunk":ctx.hunk,"message":if content.is_none() { Some("Original file contents unavailable; recorded hunk retained") } else { None },
    }))
}

/// Compare the file referenced by the comment with the selected review head.
/// Reconstructed deleted-side context remains viewable without claiming an exact anchor.
pub fn historical_code(
    store: &Store,
    project: &Path,
    workspace: &Workspace,
    id: &CommentId,
) -> Result<Value> {
    let Some(ctx) = load(store, project, workspace, id)? else {
        return Ok(json!({"code":null,"comment":id,"message":"No code location"}));
    };
    let mut response = json!({"code":null,"anchor":ctx.anchor,"diff_hunk":ctx.hunk,
        "message":"Historical comparison unavailable; recorded hunk retained"});
    let base_side = ctx
        .anchor
        .position
        .range
        .as_ref()
        .is_some_and(|range| range.start.side == DiffSide::Base);
    let source = if base_side {
        ctx.comparison.as_ref().and_then(|pair| {
            Some((
                &pair.revision.base,
                ctx.base_path.as_deref()?,
                ctx.base.as_ref().ok()?.as_deref()?,
                pair.provenance,
            ))
        })
    } else {
        let commit = match &ctx.anchor.revision {
            ReadCommentRevision::Comparison(pair) => &pair.head,
            ReadCommentRevision::Commit { head } => head,
        };
        ctx.head.as_ref().ok().and_then(|head| {
            head.as_deref().map(|content| {
                (
                    commit,
                    ctx.anchor.position.path.as_str(),
                    content,
                    ContextProvenance::Recorded,
                )
            })
        })
    };
    if let Some((commit, historical_path, before, provenance)) = source {
        let revision = DiffRevision {
            base: commit.clone(),
            head: workspace.comparison.head.clone(),
        };
        let Ok(renamed_path) =
            diff::renamed_path(project, &revision, historical_path, DiffSide::Base)
        else {
            return Ok(response);
        };
        let path = renamed_path.as_deref().unwrap_or(historical_path);
        let Ok(head) = diff::contents(project, &revision.head, path) else {
            return Ok(response);
        };
        let after = head.as_deref().unwrap_or_default();
        let original_base = ctx
            .comparison
            .as_ref()
            .and_then(|pair| Some((pair, ctx.base_path.as_deref()?, ctx.base.as_ref().ok()?)));
        let binary = [
            Some(before),
            Some(after),
            original_base.and_then(|(_, _, content)| content.as_deref()),
        ]
        .into_iter()
        .flatten()
        .any(|content| content.contains(&0) || std::str::from_utf8(content).is_err());
        let verified = !binary
            && ctx.anchor.position.range.as_ref().is_some_and(|range| {
                let count = if before.is_empty() {
                    0
                } else {
                    before.split(|byte| *byte == b'\n').count()
                        - usize::from(before.ends_with(b"\n"))
                };
                provenance == ContextProvenance::Recorded
                    && range.start.side == range.end.side
                    && range.start.line <= range.end.line
                    && range.end.line.get() as usize <= count
            });
        let location = verified.then(|| {
            let mut position = ctx.anchor.position.clone();
            position.path = path.into();
            if let Some(range) = &mut position.range {
                range.start.side = DiffSide::Base;
                range.end.side = DiffSide::Base;
            }
            DiffLocation {
                revision: revision.clone(),
                position,
            }
        });
        response["code"] = json!({"path":path,"base_path":historical_path,
            "revision":revision,"base_content":String::from_utf8_lossy(before),"head_content":String::from_utf8_lossy(after),
            "original_base":original_base.map(|(pair, path, content)| json!({
                "commit":pair.revision.base,"path":path,
                "content":String::from_utf8_lossy(content.as_deref().unwrap_or_default()),
                "missing":content.is_none(),"provenance":pair.provenance,
            })),
            "binary":binary,"historical":true,"provenance":provenance,"location":location,"head_missing":head.is_none()});
        if workspace.worktree_context {
            response["code"]["local_path"] = json!(std::fs::canonicalize(project)?.join(path));
            response["code"]["local_dirty"] = json!(diff::local_changes(project, path)?);
        }
        response["message"] = Value::Null;
    }
    Ok(response)
}

pub fn code(project: &Path, workspace: &Workspace, path: &str) -> Result<Value> {
    let file = workspace
        .comparison_files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| std::io::Error::other("file is not in the pinned comparison"))?;
    let base = diff::contents(
        project,
        &workspace.comparison.base,
        file.previous_path.as_deref().unwrap_or(path),
    )?
    .unwrap_or_default();
    let head = diff::contents(project, &workspace.comparison.head, path)?.unwrap_or_default();
    let binary = base.contains(&0)
        || head.contains(&0)
        || std::str::from_utf8(&base).is_err()
        || std::str::from_utf8(&head).is_err();
    let mut code = json!({"path":path,"revision":workspace.comparison,"base_content":String::from_utf8_lossy(&base),"head_content":String::from_utf8_lossy(&head),"binary":binary});
    if workspace.worktree_context {
        code["local_path"] = json!(std::fs::canonicalize(project)?.join(path));
        code["local_dirty"] = json!(diff::local_changes(project, path)?);
    }
    Ok(code)
}
