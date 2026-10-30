//! Reconcile resource-limited comment writes against their original pending review.
use super::{workspace::WorkspacePublishError, *};
use crate::{core::Operation, store::OperationResult, workspace::Workspace};
use serde_json::{Value, json};

fn uncertain(message: &str) -> PublishError {
    PublishError::Unsupported(format!(
        "publication comment result uncertain: {message}; no duplicate write sent"
    ))
}

fn resource_rejection(response: &Value) -> bool {
    response["errors"].as_array().is_some_and(|errors| {
        errors.iter().any(|error| {
            let code = error
                .pointer("/extensions/code")
                .or_else(|| error.get("type"))
                .and_then(Value::as_str);
            let path = error["path"].as_array();
            code == Some("RESOURCE_LIMITS_EXCEEDED")
                && path.is_some_and(|path| {
                    path.len() == 1
                        && path[0].as_str().is_some_and(|alias| {
                            response["data"].get(alias).is_none_or(Value::is_null)
                        })
                })
        })
    })
}

pub(super) fn comments(
    store: &mut crate::store::Store,
    workspace: &Workspace,
    backend: &ReviewBackend,
    pending: Option<&PendingReview>,
    operations: &[Operation],
    failures: &BTreeMap<String, Value>,
) -> Result<(Vec<OperationResult>, BTreeSet<String>), WorkspacePublishError> {
    let pending = pending.ok_or_else(|| uncertain("saved pending review identity missing"))?;
    let log = store.workspace_log(&workspace.id)?;
    let mut limited = failures
        .iter()
        .filter(|(_, response)| resource_rejection(response))
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let start = log
        .iter()
        .find(|row| {
            row["action"]["action"] == "begin_publication"
                && row["action"]["operations"]
                    .as_array()
                    .is_some_and(|planned| {
                        planned.iter().any(|planned| {
                            operations
                                .iter()
                                .any(|operation| planned["id"] == operation.id)
                        })
                    })
        })
        .and_then(|row| row["generation"].as_u64())
        .ok_or_else(|| uncertain("original publication snapshot unavailable"))?;
    // Existing failed attempts retain the provider's exact per-alias error in
    // the event log, even if a later retry replaced the overview's last error.
    for row in log
        .iter()
        .filter(|row| row["generation"].as_u64() >= Some(start))
    {
        let action = &row["action"];
        let actions = if action["action"] == "publication_group" {
            action["actions"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
        } else {
            std::slice::from_ref(action)
        };
        for error in actions
            .iter()
            .filter(|action| action["action"] == "finish_publication")
            .filter_map(|action| action["error"].as_str())
        {
            for operation in operations {
                let prefix = format!("{}: publication mutation ", operation.id);
                for part in error.split("; ") {
                    let Some((alias, errors)) = part
                        .strip_prefix(&prefix)
                        .and_then(|part| part.split_once(" failed: "))
                    else {
                        continue;
                    };
                    if alias
                        .strip_prefix("op")
                        .and_then(|index| index.parse::<usize>().ok())
                        .is_none()
                    {
                        continue;
                    }
                    let Ok(errors) = serde_json::from_str::<Vec<Value>>(errors) else {
                        continue;
                    };
                    let errors = errors
                        .into_iter()
                        .filter(|error| error["path"] == json!([alias]))
                        .collect::<Vec<_>>();
                    if resource_rejection(&json!({"errors": errors})) {
                        limited.insert(operation.id.clone());
                    }
                }
            }
        }
    }
    if limited.is_empty() {
        return Err(uncertain("provider result requires inspection").into());
    }
    let original = store
        .workspace_at_generation(&workspace.id, start)?
        .filter(|original| {
            original.attempt.as_ref().map(|attempt| &attempt.id)
                == workspace.attempt.as_ref().map(|attempt| &attempt.id)
        })
        .ok_or_else(|| uncertain("original publication identity changed"))?;
    let original_backend = store
        .review_backend(&original.review.id)?
        .ok_or_else(|| uncertain("original provider identities missing"))?;
    let excluded = original_backend
        .mappings
        .iter()
        .chain(workspace.results.iter().flat_map(|result| &result.mappings))
        .filter(|mapping| mapping.resource == "review_comment")
        .map(|mapping| mapping.remote_id.as_str())
        .collect::<BTreeSet<_>>();
    let url = Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?;
    let address = Address::from_url(&url)?;
    let reviews: Vec<Value> = list(&format!(
        "repos/{}/{}/pulls/{}/reviews?per_page=100",
        address.owner, address.repo, address.number
    ))?;
    let user = backend
        .mappings
        .iter()
        .find(|mapping| {
            mapping.resource == "user" && mapping.core_id == workspace.review.reviewer.id.0
        })
        .ok_or_else(|| uncertain("reviewer identity missing"))?;
    if !reviews.iter().any(|review| {
        review["id"].as_u64() == Some(pending.id)
            && review["node_id"].as_str() == Some(&pending.node_id)
            && review["state"] == "PENDING"
            && review["commit_id"].as_str() == Some(&pending.head)
            && review
                .pointer("/user/id")
                .and_then(Value::as_u64)
                .is_some_and(|id| id.to_string() == user.remote_id)
    }) {
        return Err(uncertain("saved review is no longer pending at its original commit").into());
    }
    let mut fresh = fetch(&url)?;
    super::workspace::canonicalize_fetched(&mut fresh, backend);
    if fresh.reviewer.id != workspace.review.reviewer.id
        || fresh.read.pull_request.revision.head != workspace.comparison.head
    {
        return Err(uncertain("reviewer or PR head changed").into());
    }
    if !fresh.read.reviews.iter().any(|review| {
        matches!(
            review.state,
            ReadReviewState::Inactive {
                state: InactiveReviewState::Pending,
                ..
            }
        ) && review.author.as_ref() == Some(&workspace.review.reviewer)
            && review.id.as_ref().is_some_and(|id| {
                fresh.backend.mappings.iter().any(|mapping| {
                    mapping.resource == "review"
                        && mapping.core_id == id.0
                        && mapping.remote_id == pending.id.to_string()
                })
            })
    }) {
        return Err(uncertain("saved review changed while fetching its comments").into());
    }
    let member = format!("review_member:{}", pending.id);
    let identities = fresh
        .backend
        .mappings
        .iter()
        .map(|mapping| {
            (
                (mapping.resource.as_str(), mapping.core_id.as_str()),
                mapping.remote_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut results = Vec::new();
    let mut retryable = BTreeSet::new();
    let mut used = BTreeSet::new();
    for operation in operations {
        let Event::CreateComment { comment } = &operation.change else {
            return Err(uncertain("unsupported operation in comment batch").into());
        };
        if matches!(comment.target, CommentTarget::PullRequest) {
            return Err(uncertain("PR-wide comment result requires inspection").into());
        }
        let candidates = fresh
            .read
            .comments
            .iter()
            .filter(|remote| {
                let key = remote.content.id.0.as_str();
                remote.draft
                    && remote.author.as_ref() == Some(&comment.author)
                    && remote.content.body == comment.content.body
                    && identities.contains_key(&(member.as_str(), key))
                    && identities
                        .get(&("review_comment", key))
                        .is_some_and(|id| !excluded.contains(id))
                    && match (&comment.target, &remote.target) {
                        (CommentTarget::Diff(local), CommentTarget::Diff(remote)) => {
                            let head = match &remote.original.revision {
                                ReadCommentRevision::Comparison(revision) => &revision.head,
                                ReadCommentRevision::Commit { head } => head,
                            };
                            head == &local.revision.head
                                && remote.original.position == local.position
                        }
                        (CommentTarget::Reply(local), CommentTarget::Reply(remote)) => {
                            local == remote
                        }
                        _ => false,
                    }
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() && limited.contains(&operation.id) {
            retryable.insert(operation.id.clone());
            continue;
        }
        let [remote] = candidates.as_slice() else {
            return Err(uncertain("no unique matching comment in saved pending review").into());
        };
        if !used.insert(remote.content.id.clone()) {
            return Err(uncertain("multiple operations match the same remote comment").into());
        }
        let key = remote.content.id.0.as_str();
        if let Some(payloads) = failures
            .get(&operation.id)
            .and_then(|response| response["data"].as_object())
        {
            for payload in payloads.values().filter(|payload| !payload.is_null()) {
                if payload["clientMutationId"]
                    .as_str()
                    .is_some_and(|id| id != operation.id)
                {
                    return Err(uncertain("partial response belongs to another operation").into());
                }
                let returned = match comment.target {
                    CommentTarget::Diff(_) => payload
                        .pointer("/thread/id")
                        .and_then(Value::as_str)
                        .map(|id| ("review_thread", id)),
                    CommentTarget::Reply(_) => payload
                        .pointer("/comment/id")
                        .and_then(Value::as_str)
                        .map(|id| ("review_comment_node", id)),
                    CommentTarget::PullRequest => None,
                };
                if returned.is_some_and(|(resource, id)| {
                    identities.get(&(resource, key)).copied() != Some(id)
                }) {
                    return Err(uncertain(
                        "partial response and fetched comment identities differ",
                    )
                    .into());
                }
                let created = match comment.target {
                    CommentTarget::Diff(_) => payload.pointer("/thread/comments/nodes/0"),
                    CommentTarget::Reply(_) => payload.get("comment"),
                    CommentTarget::PullRequest => None,
                };
                if let Some(created) = created {
                    if created
                        .get("id")
                        .filter(|id| !id.is_null())
                        .is_some_and(|id| {
                            id.as_str() != identities.get(&("review_comment_node", key)).copied()
                        })
                    {
                        return Err(uncertain("partial comment node identity differs").into());
                    }
                    for field in ["fullDatabaseId", "databaseId"] {
                        if let Some(id) = created.get(field).filter(|id| !id.is_null()) {
                            let numeric = id
                                .as_u64()
                                .or_else(|| id.as_str().and_then(|id| id.parse().ok()));
                            let expected = identities
                                .get(&("review_comment", key))
                                .and_then(|id| id.parse::<u64>().ok());
                            if numeric.is_none() || numeric != expected {
                                return Err(
                                    uncertain("partial comment database identity differs").into()
                                );
                            }
                        }
                    }
                }
            }
        }
        let mappings = fresh
            .backend
            .mappings
            .iter()
            .filter(|mapping| {
                mapping.core_id == remote.content.id.0
                    && matches!(
                        mapping.resource.as_str(),
                        "review_comment" | "review_comment_node" | "review_thread"
                    )
            })
            .map(|mapping| IdMapping {
                core_id: comment.content.id.0.clone(),
                ..mapping.clone()
            })
            .collect::<Vec<_>>();
        if !identities.contains_key(&("review_comment_node", key))
            || matches!(comment.target, CommentTarget::Diff(_))
                && !identities.contains_key(&("review_thread", key))
        {
            return Err(uncertain("matching comment provider identities incomplete").into());
        }
        results.push(OperationResult {
            operation: operation.clone(),
            mappings,
            response: json!({"reconciled":true,"review":pending.id,"comment":remote}),
        });
    }
    Ok((results, retryable))
}
