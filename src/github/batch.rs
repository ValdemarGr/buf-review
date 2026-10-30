//! One bounded GraphQL request for independent, ready publication operations.
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{PendingReview, PublishError, Review, ReviewBackend, publish::request};
use crate::{
    core::Operation,
    model::*,
    store::{IdMapping, OperationResult},
};

pub(super) const MAX_OPERATIONS: usize = 8;

pub struct BatchFailure {
    pub operation: Operation,
    pub error: String,
    pub uncertain: bool,
    pub response: Value,
}

#[derive(Default)]
pub struct BatchOutcome {
    pub results: Vec<OperationResult>,
    pub failures: Vec<BatchFailure>,
}

struct Mutation {
    field: &'static str,
    input_type: &'static str,
    selection: &'static str,
    input: Value,
}

fn node<'a>(
    identities: &BTreeMap<(&str, &str), &'a str>,
    resources: &[&str],
    id: &str,
) -> Result<&'a str, PublishError> {
    resources
        .iter()
        .find_map(|resource| {
            identities
                .get(&(*resource, id))
                .copied()
                .filter(|id| !id.is_empty())
        })
        .ok_or_else(|| {
            PublishError::Unsupported(format!("provider node identity unavailable for {id}"))
        })
}

fn prepare(
    review: &Review,
    identities: &BTreeMap<(&str, &str), &str>,
    pending: Option<&PendingReview>,
    operation: &Operation,
) -> Result<Mutation, PublishError> {
    let (field, input_type, selection, mut input) = match &operation.change {
        Event::CreateComment { comment } => match &comment.target {
            CommentTarget::PullRequest => (
                "addComment",
                "AddCommentInput",
                "clientMutationId commentEdge{node{id fullDatabaseId}}",
                json!({"subjectId":node(identities, &["pull_request_node"], &review.read.pull_request.id.0)?, "body":comment.content.body}),
            ),
            CommentTarget::Diff(location) => {
                let pending = pending.ok_or_else(|| {
                    PublishError::Unsupported("inline comments require a pending review".into())
                })?;
                if pending.node_id.is_empty() || pending.head != location.revision.head {
                    return Err(PublishError::Unsupported(
                        "comment and pending review commits differ".into(),
                    ));
                }
                let mut input = json!({"pullRequestReviewId":pending.node_id,"body":comment.content.body,"path":location.position.path,
                    "subjectType":if location.position.range.is_some() { "LINE" } else { "FILE" }});
                if let Some(range) = &location.position.range {
                    input["line"] = json!(range.end.line);
                    input["side"] = json!(if range.end.side == DiffSide::Head {
                        "RIGHT"
                    } else {
                        "LEFT"
                    });
                    if range.start != range.end {
                        input["startLine"] = json!(range.start.line);
                        input["startSide"] = json!(if range.start.side == DiffSide::Head {
                            "RIGHT"
                        } else {
                            "LEFT"
                        });
                    }
                }
                (
                    "addPullRequestReviewThread",
                    "AddPullRequestReviewThreadInput",
                    "clientMutationId thread{id comments(first:1){nodes{id fullDatabaseId}}}",
                    input,
                )
            }
            CommentTarget::Reply(parent) => {
                let pending = pending.ok_or_else(|| {
                    PublishError::Unsupported("replies require a pending review".into())
                })?;
                (
                    "addPullRequestReviewThreadReply",
                    "AddPullRequestReviewThreadReplyInput",
                    "clientMutationId comment{id fullDatabaseId}",
                    json!({"pullRequestReviewId":pending.node_id,"pullRequestReviewThreadId":node(identities, &["review_thread"], &parent.0)?,"body":comment.content.body}),
                )
            }
        },
        Event::SetFileViewed { path, viewed, .. } => (
            if *viewed {
                "markFileAsViewed"
            } else {
                "unmarkFileAsViewed"
            },
            if *viewed {
                "MarkFileAsViewedInput"
            } else {
                "UnmarkFileAsViewedInput"
            },
            "clientMutationId",
            json!({"pullRequestId":node(identities, &["pull_request_node"], &review.read.pull_request.id.0)?,"path":path}),
        ),
        Event::SetCommentReaction {
            comment,
            reaction,
            present,
        } => (
            if *present {
                "addReaction"
            } else {
                "removeReaction"
            },
            if *present {
                "AddReactionInput"
            } else {
                "RemoveReactionInput"
            },
            "clientMutationId",
            json!({"subjectId":node(identities, &["issue_comment_node", "review_comment_node", "review_node"], &comment.0)?,
                "content":super::workspace::graphql_reaction(reaction)?}),
        ),
        Event::DismissReview { review, message } => (
            "dismissPullRequestReview",
            "DismissPullRequestReviewInput",
            "clientMutationId pullRequestReview{id state}",
            json!({"pullRequestReviewId":node(identities, &["review_resource_node", "review_node"], &review.0)?,"message":message.as_deref().unwrap_or_default()}),
        ),
        _ => {
            return Err(PublishError::Unsupported(
                "operation cannot belong to a GraphQL publication level".into(),
            ));
        }
    };
    input["clientMutationId"] = json!(operation.id);
    Ok(Mutation {
        field,
        input_type,
        selection,
        input,
    })
}

fn created_mappings(operation: &Operation, payload: &Value) -> Result<Vec<IdMapping>, String> {
    let Event::CreateComment { comment } = &operation.change else {
        return Ok(Vec::new());
    };
    let (resource, created) = match &comment.target {
        CommentTarget::PullRequest => ("issue_comment", payload.pointer("/commentEdge/node")),
        CommentTarget::Diff(_) => (
            "review_comment",
            payload.pointer("/thread/comments/nodes/0"),
        ),
        CommentTarget::Reply(_) => ("review_comment", payload.get("comment")),
    };
    let created = created.ok_or_else(|| "created comment identity missing".to_owned())?;
    let numeric = ["fullDatabaseId", "databaseId"]
        .into_iter()
        .find_map(|field| {
            created
                .get(field)
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                })
                .filter(|id| *id > 0)
        })
        .ok_or_else(|| "created comment database identity missing".to_owned())?;
    let remote_node = created
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "created comment node identity missing".to_owned())?;
    let mut mappings = vec![
        IdMapping {
            resource: resource.into(),
            core_id: comment.content.id.0.clone(),
            remote_id: numeric.to_string(),
        },
        IdMapping {
            resource: format!("{resource}_node"),
            core_id: comment.content.id.0.clone(),
            remote_id: remote_node.into(),
        },
    ];
    if matches!(comment.target, CommentTarget::Diff(_)) {
        let thread = payload
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "created comment thread identity missing".to_owned())?;
        mappings.push(IdMapping {
            resource: "review_thread".into(),
            core_id: comment.content.id.0.clone(),
            remote_id: thread.into(),
        });
    }
    Ok(mappings)
}

fn request_validation(error: &Value) -> bool {
    let code = error
        .pointer("/extensions/code")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str);
    matches!(
        code,
        Some(
            "GRAPHQL_VALIDATION_FAILED"
                | "GRAPHQL_PARSE_FAILED"
                | "VARIABLES_INVALID"
                | "undefinedField"
                | "variableMismatch"
                | "variableNotUsed"
                | "argumentNotAccepted"
                | "selectionMismatch"
                | "missingRequiredArguments"
                | "argumentLiteralsIncompatible"
                | "variableRequiresValidType"
        )
    )
}

fn confirmed_rejection(error: &Value) -> bool {
    request_validation(error)
        || matches!(
            error
                .pointer("/extensions/code")
                .or_else(|| error.get("type"))
                .and_then(Value::as_str),
            Some("FORBIDDEN" | "UNAUTHORIZED" | "NOT_FOUND" | "UNPROCESSABLE")
        )
}

fn outcome(operations: &[Operation], response: &Value) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    let errors = response
        .get("errors")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let rejected_request = response.get("data").is_none_or(Value::is_null)
        && !errors.is_empty()
        && errors.iter().all(request_validation);
    for (index, operation) in operations.iter().enumerate() {
        let alias = format!("op{index}");
        let payload = response
            .get("data")
            .and_then(|data| data.get(&alias))
            .filter(|value| !value.is_null());
        let alias_errors = errors
            .iter()
            .filter(|error| {
                error.pointer("/path/0").and_then(Value::as_str) == Some(alias.as_str())
            })
            .collect::<Vec<_>>();
        let evidence = json!({
            "data": {(alias.clone()): payload},
            "errors": errors.iter().filter(|error| {
                error.pointer("/path/0").and_then(Value::as_str)
                    .is_none_or(|path| path == alias)
            }).collect::<Vec<_>>()
        });
        if let Some(payload) = payload {
            let confirmed = payload.get("clientMutationId").and_then(Value::as_str)
                == Some(operation.id.as_str());
            let mappings = created_mappings(operation, payload);
            let dismissed = !matches!(operation.change, Event::DismissReview { .. })
                || payload
                    .pointer("/pullRequestReview/state")
                    .and_then(Value::as_str)
                    == Some("DISMISSED");
            let error = match mappings {
                Ok(mappings) if confirmed && dismissed => {
                    outcome.results.push(OperationResult {
                        operation: operation.clone(),
                        mappings,
                        response: json!({"data":{(alias):payload},"errors":alias_errors}),
                    });
                    continue;
                }
                Err(error) => error,
                Ok(_) => format!("publication response did not confirm {alias}: {payload}"),
            };
            outcome.failures.push(BatchFailure {
                operation: operation.clone(),
                error,
                uncertain: true,
                response: evidence,
            });
        } else {
            outcome.failures.push(BatchFailure {
                operation: operation.clone(),
                error: format!(
                    "publication mutation {alias} failed: {}",
                    if alias_errors.is_empty() {
                        json!(errors)
                    } else {
                        json!(alias_errors)
                    }
                ),
                uncertain: !rejected_request
                    && !alias_errors.iter().any(|error| {
                        confirmed_rejection(error)
                            && error
                                .get("path")
                                .and_then(Value::as_array)
                                .is_some_and(|path| path.len() == 1)
                    }),
                response: evidence,
            });
        }
    }
    outcome
}

/// Resolve every input before sending. Once a request is sent, retain each
/// confirmed alias independently, including successes after a failed alias.
pub fn send(
    review: &Review,
    backend: &ReviewBackend,
    pending: Option<&PendingReview>,
    operations: &[Operation],
) -> Result<BatchOutcome, PublishError> {
    if operations.is_empty() {
        return Ok(BatchOutcome::default());
    }
    if operations.len() > MAX_OPERATIONS {
        return Err(PublishError::Unsupported(format!(
            "publication batch exceeds {MAX_OPERATIONS} operations"
        )));
    }
    if backend.source.backend != "github" {
        return Err(PublishError::Unsupported(
            "review backend is not GitHub".into(),
        ));
    }
    let identities = backend
        .mappings
        .iter()
        .filter(|mapping| !mapping.remote_id.is_empty())
        .rev()
        .map(|mapping| {
            (
                (mapping.resource.as_str(), mapping.core_id.as_str()),
                mapping.remote_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mutations = operations
        .iter()
        .map(|operation| prepare(review, &identities, pending, operation))
        .collect::<Result<Vec<_>, _>>()?;
    let declarations = mutations
        .iter()
        .enumerate()
        .map(|(index, mutation)| format!("$input{index}:{}!", mutation.input_type))
        .collect::<Vec<_>>()
        .join(",");
    let fields = mutations
        .iter()
        .enumerate()
        .map(|(index, mutation)| {
            format!(
                "op{index}:{}(input:$input{index}){{{}}}",
                mutation.field, mutation.selection
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let variables = mutations
        .into_iter()
        .enumerate()
        .map(|(index, mutation)| (format!("input{index}"), mutation.input))
        .collect::<serde_json::Map<_, _>>();
    match request(
        "graphql",
        "POST",
        &json!({"query":format!("mutation PublicationBatch({declarations}){{{fields}}}"),"variables":variables}),
    ) {
        Ok(response) => Ok(outcome(operations, &response)),
        Err(error) => Ok(BatchOutcome {
            results: Vec::new(),
            failures: operations
                .iter()
                .map(|operation| BatchFailure {
                    operation: operation.clone(),
                    error: error.to_string(),
                    uncertain: !super::workspace::confirmed_rejection(&error),
                    response: Value::Null,
                })
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, num::NonZeroU32, os::unix::fs::PermissionsExt, process::Command};

    fn fixture() -> (Review, ReviewBackend, PendingReview) {
        let review: Review = serde_json::from_value(json!({
            "id":"snapshot","reviewer":{"id":"me","name":"Reviewer"},
            "read":{"pull_request":{"id":"pr","title":"PR","body":"","author":null,"status":"open","source":null,"revision":{"base":"base","head":"head"}},
                "files":[],"comments":[],"reactions":[],"reviews":[]}
        })).unwrap();
        let backend = ReviewBackend {
            source: super::super::ReviewSource {
                backend: "github".into(),
                locator: "https://github.com/owner/repo/pull/1".into(),
            },
            mappings: [
                ("pull_request_node", "pr", "PR1"),
                ("review_thread", "parent", "THREAD1"),
                ("review_comment_node", "parent", "COMMENT1"),
                ("review_resource_node", "review", "REVIEW1"),
            ]
            .into_iter()
            .map(|(resource, core_id, remote_id)| IdMapping {
                resource: resource.into(),
                core_id: core_id.into(),
                remote_id: remote_id.into(),
            })
            .collect(),
        };
        (
            review,
            backend,
            PendingReview {
                id: 1,
                node_id: "PENDING1".into(),
                head: "head".into(),
            },
        )
    }

    fn viewed(id: &str) -> Operation {
        Operation {
            id: id.into(),
            change: Event::SetFileViewed {
                path: "file.rs".into(),
                viewed: true,
                commit: Some("head".into()),
            },
        }
    }

    fn create(id: &str, target: CommentTarget) -> Operation {
        Operation {
            id: id.into(),
            change: Event::CreateComment {
                comment: DraftComment {
                    content: CommentContent {
                        id: CommentId(id.into()),
                        body: format!("Body {id}"),
                    },
                    author: User {
                        id: UserId("me".into()),
                        name: "Reviewer".into(),
                    },
                    target,
                },
            },
        }
    }

    fn root(id: &str) -> Operation {
        create(
            id,
            CommentTarget::Diff(DiffLocation {
                revision: DiffRevision {
                    base: "base".into(),
                    head: "head".into(),
                },
                position: CommentPosition {
                    path: "file.rs".into(),
                    range: Some(DiffRange {
                        start: DiffLine {
                            side: DiffSide::Base,
                            line: NonZeroU32::new(2).unwrap(),
                        },
                        end: DiffLine {
                            side: DiffSide::Base,
                            line: NonZeroU32::new(4).unwrap(),
                        },
                    }),
                },
            }),
        )
    }

    #[test]
    fn mutation_inputs_cover_supported_families_without_substituting_identities() {
        let (review, backend, pending) = fixture();
        let identities = backend
            .mappings
            .iter()
            .map(|mapping| {
                (
                    (mapping.resource.as_str(), mapping.core_id.as_str()),
                    mapping.remote_id.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let root_input = prepare(&review, &identities, Some(&pending), &root("root")).unwrap();
        assert_eq!(root_input.field, "addPullRequestReviewThread");
        assert_eq!(
            root_input.input,
            json!({"pullRequestReviewId":"PENDING1","body":"Body root","path":"file.rs","subjectType":"LINE","line":4,"side":"LEFT","startLine":2,"startSide":"LEFT","clientMutationId":"root"})
        );
        let mut file = root("file");
        let Event::CreateComment { comment } = &mut file.change else {
            unreachable!()
        };
        let CommentTarget::Diff(location) = &mut comment.target else {
            unreachable!()
        };
        location.position.range = None;
        let file = prepare(&review, &identities, Some(&pending), &file).unwrap();
        assert_eq!(file.input["subjectType"], "FILE");
        assert!(file.input.get("line").is_none());
        let reply = prepare(
            &review,
            &identities,
            Some(&pending),
            &create("reply", CommentTarget::Reply(CommentId("parent".into()))),
        )
        .unwrap();
        assert_eq!(reply.input["pullRequestReviewThreadId"], "THREAD1");
        let discussion = prepare(
            &review,
            &identities,
            None,
            &create("discussion", CommentTarget::PullRequest),
        )
        .unwrap();
        assert_eq!(discussion.field, "addComment");
        assert_eq!(discussion.input["subjectId"], "PR1");
        let mut unviewed = viewed("unviewed");
        let Event::SetFileViewed { viewed, .. } = &mut unviewed.change else {
            unreachable!()
        };
        *viewed = false;
        assert_eq!(
            prepare(&review, &identities, None, &unviewed)
                .unwrap()
                .field,
            "unmarkFileAsViewed"
        );
        let reaction = Operation {
            id: "reaction".into(),
            change: Event::SetCommentReaction {
                comment: CommentId("parent".into()),
                reaction: "👍".into(),
                present: false,
            },
        };
        let reaction = prepare(&review, &identities, None, &reaction).unwrap();
        assert_eq!(reaction.field, "removeReaction");
        assert_eq!(reaction.input["subjectId"], "COMMENT1");
        let dismiss = Operation {
            id: "dismiss".into(),
            change: Event::DismissReview {
                review: ReviewResourceId("review".into()),
                message: Some("Superseded".into()),
            },
        };
        assert_eq!(
            prepare(&review, &identities, None, &dismiss).unwrap().input["pullRequestReviewId"],
            "REVIEW1"
        );
    }

    #[test]
    fn partial_alias_failure_preserves_later_successes_and_exact_operation_ids() {
        let operations = [viewed("a"), viewed("b"), viewed("c")];
        let result = outcome(
            &operations,
            &json!({"data":{"op0":{"clientMutationId":"a"},"op1":null,"op2":{"clientMutationId":"c"}},
            "errors":[{"path":["op1"],"type":"UNPROCESSABLE","message":"Rejected"}]}),
        );
        assert_eq!(
            result
                .results
                .iter()
                .map(|result| result.operation.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].operation, operations[1]);
        assert!(!result.failures[0].uncertain);
        assert_eq!(
            result.results[1].response["data"]["op2"]["clientMutationId"],
            "c"
        );
    }

    #[test]
    fn incomplete_creation_and_post_write_field_errors_remain_uncertain() {
        let operations = [root("root")];
        for response in [
            json!({"data":{"op0":{"clientMutationId":"root","thread":{"id":"T1","comments":{"nodes":[{"id":"C1"}]}}}}}),
            json!({"data":{"op0":null},"errors":[{"path":["op0","thread","comments"],"message":"Read failed after write"}]}),
            json!({"errors":[{"type":"INTERNAL","message":"Unknown execution result"}]}),
            json!({"data":{"op0":null},"errors":[{"type":"INTERNAL","path":["op0"],"message":"Resolver failed after write"}]}),
            json!({"data":{"op0":null},"errors":[{"path":["op0"],"message":"Unknown execution result"}]}),
        ] {
            let result = outcome(&operations, &response);
            assert!(result.results.is_empty());
            assert!(result.failures[0].uncertain);
        }
        let rejected = outcome(
            &operations,
            &json!({"errors":[{"type":"undefinedField","message":"Invalid field"}]}),
        );
        assert!(!rejected.failures[0].uncertain);
    }

    #[test]
    fn resource_exhaustion_retains_partial_identities_without_allowing_blind_retry() {
        let operations = [root("confirmed"), root("partial"), root("limited")];
        let partial = json!({"clientMutationId":"partial","thread":{"id":"T2","comments":null}});
        let field_error = json!({"path":["op1","thread","comments"],"type":"RESOURCE_LIMITS_EXCEEDED","message":"Read interrupted after write"});
        let mutation_error = json!({"path":["op2"],"type":"RESOURCE_LIMITS_EXCEEDED","message":"Resource limits for this query exceeded."});
        let result = outcome(
            &operations,
            &json!({"data":{
                "op0":{"clientMutationId":"confirmed","thread":{"id":"T1","comments":{"nodes":[{"id":"C1","fullDatabaseId":"11"}]}}},
                "op1":partial,"op2":null
            },"errors":[field_error,mutation_error]}),
        );
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].operation, operations[0]);
        assert_eq!(result.failures.len(), 2);
        assert!(result.failures.iter().all(|failure| failure.uncertain));
        assert_eq!(result.failures[0].operation, operations[1]);
        assert_eq!(
            result.failures[0].response,
            json!({"data":{"op1":partial},"errors":[field_error]})
        );
        assert_eq!(result.failures[1].operation, operations[2]);
        assert_eq!(
            result.failures[1].response,
            json!({"data":{"op2":null},"errors":[mutation_error]})
        );
    }

    #[test]
    fn created_receipts_retain_comment_nodes_and_only_root_thread_mappings() {
        let operations = [
            root("root"),
            create("reply", CommentTarget::Reply(CommentId("parent".into()))),
            create("wide", CommentTarget::PullRequest),
        ];
        let result = outcome(
            &operations,
            &json!({"data":{
                "op0":{"clientMutationId":"root","thread":{"id":"T1","comments":{"nodes":[{"id":"C1","databaseId":11}]}}},
                "op1":{"clientMutationId":"reply","comment":{"id":"C2","databaseId":null,"fullDatabaseId":"4294967296"}},
                "op2":{"clientMutationId":"wide","commentEdge":{"node":{"id":"IC3","databaseId":13}}}
            }}),
        );
        assert!(result.failures.is_empty());
        assert_eq!(result.results.len(), 3);
        assert_eq!(
            result.results[0]
                .mappings
                .iter()
                .map(|mapping| mapping.resource.as_str())
                .collect::<Vec<_>>(),
            ["review_comment", "review_comment_node", "review_thread"]
        );
        assert_eq!(result.results[1].mappings.len(), 2);
        assert_eq!(result.results[1].mappings[0].remote_id, "4294967296");
        assert_eq!(result.results[2].mappings[1].resource, "issue_comment_node");
    }

    #[test]
    fn missing_target_identity_rejects_before_any_request() {
        let (review, mut backend, pending) = fixture();
        backend.mappings.clear();
        assert!(send(&review, &backend, Some(&pending), &[viewed("viewed")]).is_err());
    }

    #[test]
    fn oversized_batch_rejected_before_request_preparation() {
        let (review, mut backend, pending) = fixture();
        backend.mappings.clear();
        let operations = (0..=MAX_OPERATIONS)
            .map(|index| viewed(&format!("viewed-{index}")))
            .collect::<Vec<_>>();
        let error = send(&review, &backend, Some(&pending), &operations)
            .err()
            .expect("oversized request must fail before preparing any mutation");
        assert_eq!(
            error.to_string(),
            format!(
                "cannot publish to GitHub: publication batch exceeds {MAX_OPERATIONS} operations"
            )
        );
    }

    #[test]
    fn transport_preserves_partial_graphql_data_even_when_gh_exits_unsuccessfully() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("gh");
        fs::write(&executable, "#!/bin/sh\ncat > \"$BATCH_REQUEST\"\ncat \"$BATCH_RESPONSE\"\nprintf 'GraphQL request failed' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        for case in ["partial", "lost"] {
            let response = if case == "partial" {
                json!({"data":{"op0":{"clientMutationId":"root","thread":{"id":"T1","comments":{"nodes":[{"id":"C1","databaseId":11}]}}},"op1":null},"errors":[{"path":["op1"],"type":"UNPROCESSABLE","message":"Rejected"}]}).to_string()
            } else {
                String::new()
            };
            fs::write(directory.path().join("response"), response).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "github::batch::tests::transport_child",
                    "--nocapture",
                ])
                .env("BATCH_CHILD", case)
                .env("BATCH_REQUEST", directory.path().join("request"))
                .env("BATCH_RESPONSE", directory.path().join("response"))
                .env("PATH", &path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let request: Value = serde_json::from_str(
                &fs::read_to_string(directory.path().join("request")).unwrap(),
            )
            .unwrap();
            assert!(
                request["query"]
                    .as_str()
                    .unwrap()
                    .contains("mutation PublicationBatch")
            );
            assert_eq!(request["variables"]["input0"]["clientMutationId"], "root");
            assert_eq!(request["variables"]["input1"]["clientMutationId"], "last");
        }
    }

    #[test]
    fn transport_child() {
        let Ok(case) = std::env::var("BATCH_CHILD") else {
            return;
        };
        let (review, backend, pending) = fixture();
        let result = send(
            &review,
            &backend,
            Some(&pending),
            &[root("root"), viewed("last")],
        )
        .unwrap();
        if case == "partial" {
            assert_eq!(result.results.len(), 1);
            assert_eq!(result.results[0].operation.id, "root");
            assert_eq!(result.failures.len(), 1);
            assert!(!result.failures[0].uncertain);
        } else {
            assert!(result.results.is_empty());
            assert_eq!(result.failures.len(), 2);
            assert!(result.failures.iter().all(|failure| failure.uncertain));
            assert!(
                result
                    .failures
                    .iter()
                    .all(|failure| failure.response.is_null())
            );
        }
    }
}
