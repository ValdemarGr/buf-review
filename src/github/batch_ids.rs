//! Fill provider identity gaps together before sending a mutation batch.
use super::{publish::PublishError, *};
use crate::core::{Change, Operation};
use crate::workspace::Workspace;

pub(super) fn complete(
    workspace: &Workspace,
    backend: &ReviewBackend,
    operations: &[Operation],
    roots: &BTreeMap<CommentId, CommentId>,
) -> Result<Vec<IdMapping>, PublishError> {
    let identities = backend
        .mappings
        .iter()
        .rev()
        .map(|mapping| {
            (
                (mapping.resource.as_str(), mapping.core_id.as_str()),
                mapping.remote_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut needed = BTreeSet::new();
    for operation in operations {
        let (target, resources): (&str, &[&str]) = match &operation.change {
            Change::SetCommentReaction { comment, .. } => (
                &comment.0,
                &["issue_comment_node", "review_comment_node", "review_node"],
            ),
            Change::CreateComment {
                comment:
                    DraftComment {
                        target: CommentTarget::Reply(parent),
                        ..
                    },
            } => {
                let root = roots.get(parent).ok_or_else(|| {
                    PublishError::Unsupported("publication reply root cannot be resolved".into())
                })?;
                (&root.0, &["review_thread"])
            }
            Change::DismissReview { review, .. } => (&review.0, &["review_resource_node"]),
            _ => continue,
        };
        let known = resources.iter().any(|resource| {
            identities
                .get(&(*resource, target))
                .is_some_and(|id| !id.is_empty())
        });
        let published = [
            "issue_comment",
            "review_comment",
            "review",
            "review_summary",
        ]
        .iter()
        .any(|resource| identities.contains_key(&(*resource, target)));
        if !known && published {
            needed.insert(target.to_owned());
        }
    }
    if needed.is_empty() {
        return Ok(Vec::new());
    }
    let mut fresh =
        fetch(&Url::parse(&backend.source.locator).map_err(|_| FetchError::InvalidUrl)?)?;
    super::workspace::canonicalize_fetched(&mut fresh, backend);
    if fresh.reviewer.id != workspace.review.reviewer.id {
        return Err(PublishError::Unsupported(
            "authenticated reviewer changed while resolving batch identities".into(),
        ));
    }
    if fresh.read.pull_request.revision.head != workspace.comparison.head {
        return Err(PublishError::Unsupported("not at remote head".into()));
    }
    Ok(fresh
        .backend
        .mappings
        .into_iter()
        .filter(|mapping| {
            needed.contains(&mapping.core_id)
                && matches!(
                    mapping.resource.as_str(),
                    "issue_comment_node"
                        | "review_comment_node"
                        | "review_node"
                        | "review_resource_node"
                        | "review_thread"
                )
        })
        .collect())
}
