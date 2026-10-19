use std::{fs, num::NonZeroU32, os::unix::fs::PermissionsExt, process::Command};

use buf_review::{
    github,
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store},
    workspace::Action,
};
use serde_json::Value;
use tempfile::tempdir;
use uuid::Uuid;

const SCHEMA_REJECTION: &str = "gh: Invalid request.\n\nNo subschema in \"oneOf\" matched.\n\"positioning\" wasn't supplied.\n\"position\" wasn't supplied.\n\"in_reply_to\" wasn't supplied.\n\"subject_type\" is not a permitted key.\n\"line\" is not a permitted key. (HTTP 422)";
const ANCHOR_REJECTION: &str = "gh: Validation Failed: line must be part of the diff (HTTP 422)";

#[test]
fn malformed_inline_request_can_retry_without_relocation() {
    run("schema_rejection");
}
#[test]
fn persisted_schema_rejection_recovers_without_clearing_real_anchor_rejections() {
    run("schema_recovery");
}
#[test]
fn invalid_diff_location_still_requires_explicit_relocation() {
    run("anchor_rejection");
}

#[test]
fn grouped_submission_keeps_standalone_queued_and_exact_historical_head() {
    run("grouped");
}
#[test]
fn mixed_review_heads_rejected_before_provider_writes() {
    run("mixed");
}
#[test]
fn unknown_create_blocks_blind_retry_and_keeps_work() {
    run("unknown");
}
#[test]
fn current_account_mismatch_blocks_workspace_publication() {
    run("account");
}
#[test]
fn repository_list_filters_and_sorts_all_pages() {
    run("listing");
}
#[test]
fn prepared_review_retries_without_duplicate_roots() {
    run("retry");
}
#[test]
fn removed_prepared_drafts_do_not_get_submitted() {
    run("drop");
}
#[test]
fn submitted_review_survives_progress_failure() {
    run("progress");
}
#[test]
fn uncertain_submission_reconciles_without_second_submit() {
    run("uncertain_submit");
}
#[test]
fn summaries_edit_and_react_using_review_resources() {
    run("summary");
}
#[test]
fn uncertain_review_allows_unrelated_standalone_and_progress() {
    run("uncertain_review");
}
#[test]
fn relocated_prepared_roots_and_replies_replace_only_private_versions() {
    run("relocate");
}
#[test]
fn relocated_head_rebuilds_known_private_review_at_new_commit() {
    run("relocate_head");
}
#[test]
fn grouped_reply_targets_published_standalone_before_refetch() {
    run("receipt_reply");
}
#[test]
fn lost_progress_response_reconciles_and_does_not_resubmit_review() {
    run("uncertain_progress");
}
#[test]
fn uncertain_delete_uses_canonical_published_identity() {
    run("alias_delete");
}
#[test]
fn uncertain_edit_uses_canonical_published_identity() {
    run("alias_edit");
}
#[test]
fn uncertain_reaction_uses_canonical_published_identity() {
    run("alias_reaction");
}
#[test]
fn resumed_attempt_rechecks_remote_body_before_writing() {
    run("resumed_conflict");
}
#[test]
fn provider_receipts_merge_concurrent_unrelated_saves() {
    run("concurrent");
}

fn run(case: &str) {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("gh");
    fs::write(&executable,r#"#!/bin/sh
if [ "$1" = repo ]; then printf '{"nameWithOwner":"owner/repo"}'; exit; fi
endpoint=$2
shift 2
method=GET
input=no
while [ "$#" -gt 0 ]; do
  case "$1" in
    --method) method=$2; shift 2 ;;
    --input) input=yes; shift 2 ;;
    *) shift ;;
  esac
done
body=null
if [ "$input" = yes ]; then body=$(cat); fi
printf '{"endpoint":"%s","method":"%s","body":%s}\n' "$endpoint" "$method" "$body" >> "$PROVIDER_LOG"
case "$endpoint" in
 user) if [ "$PROVIDER_CASE" = account ]; then printf '{"id":99,"login":"other"}'; else printf '{"id":7,"login":"reviewer"}'; fi ;;
 *'/pulls?state='*) printf '[[{"number":3,"title":"older","html_url":"https://github.com/owner/repo/pull/3","user":{"id":8,"login":"author"},"assignees":[{"id":7,"login":"reviewer"}],"state":"open","merged_at":null,"updated_at":"2026-01-01"}],[{"number":4,"title":"latest","html_url":"https://github.com/owner/repo/pull/4","user":null,"assignees":[{"id":7,"login":"reviewer"}],"state":"closed","merged_at":"2026-01-02","updated_at":"2026-01-02"},{"number":5,"title":"other","html_url":"https://github.com/owner/repo/pull/5","user":null,"assignees":[],"state":"open","merged_at":null,"updated_at":"2026-01-03"}]]' ;;
 *reviews?per_page=100) printf '[[]]' ;;
 */reviews/100/events)
   if [ "$PROVIDER_CASE" = concurrent ] && [ ! -f "$PROVIDER_ROOT/concurrent" ]; then
     touch "$PROVIDER_ROOT/concurrent"
     "$PROVIDER_TEST_BIN" --exact provider_concurrent_child --nocapture >"$PROVIDER_ROOT/concurrent-log" 2>&1 || { cat "$PROVIDER_ROOT/concurrent-log" >&2; exit 1; }
   fi
   if [ ! -f "$PROVIDER_ROOT/submit" ]; then
     touch "$PROVIDER_ROOT/submit"
     case "$PROVIDER_CASE" in
       retry|drop|relocate|relocate_head) printf 'HTTP 422 Review summary rejected' >&2; exit 1 ;;
       uncertain_submit) printf 'connection lost after submitting' >&2; exit 1 ;;
     esac
   fi
   printf '{"id":100,"node_id":"R100","state":"COMMENTED","body":"Summary"}' ;;
 */reviews/100) printf '{"id":100,"node_id":"R100","state":"COMMENTED","body":"Summary"}' ;;
 */reviews) printf '{"id":100,"node_id":"R100","state":"PENDING"}' ;;
 */pulls/comments/400) printf '{"id":400,"body":"Root","in_reply_to_id":null}' ;;
 */pulls/comments/*)
   if [ "$method" = PATCH ]; then printf 'HTTP 404 private review comments are unavailable through REST update' >&2; exit 1; fi
   if [ "$method" != DELETE ]; then printf '{"id":201,"body":"Root edited"}'; fi ;;
 graphql)
   case "$body" in
    *ReviewSnapshot*)
      if [ "$PROVIDER_CASE" = alias_edit ] && [ -f "$PROVIDER_ROOT/alias" ]; then text=Local; else text=Original; fi
      case "$PROVIDER_CASE" in alias_*) comments="{\"totalCount\":1,\"nodes\":[{\"id\":\"IC300\",\"fullDatabaseId\":\"300\",\"body\":\"$text\",\"createdAt\":\"2026-01-01T00:00:00Z\",\"author\":{\"databaseId\":7,\"login\":\"reviewer\"},\"reactions\":{\"totalCount\":0,\"nodes\":[],\"pageInfo\":{\"hasNextPage\":false,\"endCursor\":null}}}],\"pageInfo\":{\"hasNextPage\":false,\"endCursor\":null}}" ;;
        *) comments='{ "totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}' ;;
      esac
      if [ "$PROVIDER_CASE" = alias_reaction ] && [ -f "$PROVIDER_ROOT/alias" ]; then
        comments='{ "totalCount":1,"nodes":[{"id":"IC300","fullDatabaseId":"300","body":"Original","createdAt":"2026-01-01T00:00:00Z","author":{"databaseId":7,"login":"reviewer"},"reactions":{"totalCount":1,"nodes":[{"id":"R501","content":"THUMBS_UP","user":{"databaseId":7,"login":"reviewer"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}'
      fi
      printf '{"data":{"viewer":{"databaseId":7,"login":"reviewer"},"repository":{"pullRequest":{"id":"PR1","fullDatabaseId":"1","title":"PR","body":"","author":null,"state":"OPEN","baseRefOid":"old-base","headRefOid":"new-head","headRefName":"feature","headRepository":{"url":"https://github.com/acme/repo"},"changedFiles":1,"files":{"totalCount":1,"nodes":[{"path":"file.rs","changeType":"MODIFIED","viewerViewedState":"VIEWED"}],"pageInfo":{"hasNextPage":false,"endCursor":null}},"comments":%s,"reviews":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}},"reviewThreads":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}' "$comments" ;;
    *addPullRequestReviewThreadReply*) printf '{"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"C202","databaseId":202}}}}' ;;
    *addPullRequestReviewThread*)
      if [ "$PROVIDER_CASE" = uncertain_review ]; then printf 'connection lost after preparing comment' >&2; exit 1; fi
      printf '{"data":{"addPullRequestReviewThread":{"thread":{"id":"T201","comments":{"nodes":[{"id":"C201","databaseId":201}]}}}}}' ;;
    *updatePullRequestReviewComment*) printf '{"data":{"updatePullRequestReviewComment":{"pullRequestReviewComment":{"id":"C201","body":"Root edited"}}}}' ;;
    *markFileAsViewed*)
      if [ "$PROVIDER_CASE" = progress ] && [ ! -f "$PROVIDER_ROOT/progress" ]; then touch "$PROVIDER_ROOT/progress"; printf 'HTTP 422 viewed state rejected' >&2; exit 1; fi
      if [ "$PROVIDER_CASE" = uncertain_progress ] && [ ! -f "$PROVIDER_ROOT/progress" ]; then touch "$PROVIDER_ROOT/progress"; printf 'connection lost after setting viewed state' >&2; exit 1; fi
      printf '{"data":{"markFileAsViewed":{"clientMutationId":"done"}}}' ;;
    *addReaction*) printf '{"data":{"addReaction":{"clientMutationId":"done"}}}' ;;
    *reviewThreads*) printf '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"T400","comments":{"nodes":[{"databaseId":400}]}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}' ;;
    null) printf '[{"data":{"repository":{"pullRequest":{"baseRefOid":"old-base","headRefOid":"new-head","files":{"totalCount":1,"nodes":[{"path":"file.rs","viewerViewedState":"VIEWED"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]' ;;
    *) printf 'unexpected GraphQL' >&2; exit 1 ;;
   esac ;;
 */pulls/1) printf '{"id":1,"node_id":"PR1","title":"PR","body":"","user":null,"state":"open","merged":false,"changed_files":1,"base":{"sha":"old-base"},"head":{"sha":"new-head"}}' ;;
 */pulls/1/files*) printf '[[{"filename":"file.rs","status":"modified","additions":12,"deletions":3}]]' ;;
 */pulls/1/comments?*) printf '[[]]' ;;
 */issues/1/comments?*)
   case "$PROVIDER_CASE" in
     alias_*) body=Original; if [ "$PROVIDER_CASE" = alias_edit ] && [ -f "$PROVIDER_ROOT/alias" ]; then body=Local; fi
       printf '[[{"id":300,"node_id":"IC300","body":"%s","user":{"id":7,"login":"reviewer"}}]]' "$body" ;;
     *) printf '[[]]' ;;
   esac ;;
 */issues/comments/300/reactions?*)
   if [ "$PROVIDER_CASE" = alias_reaction ] && [ -f "$PROVIDER_ROOT/alias" ]; then printf '[[{"id":501,"content":"+1","user":{"id":7,"login":"reviewer"}}]]'; else printf '[[]]'; fi ;;
 */issues/comments/300/reactions)
   if [ ! -f "$PROVIDER_ROOT/alias" ]; then touch "$PROVIDER_ROOT/alias"; printf 'lost reaction response' >&2; exit 1; fi
   printf '{"id":501}' ;;
 */issues/comments/300)
   if [ "$method" = GET ]; then
     body=Original
     if [ "$PROVIDER_CASE" = resumed_conflict ]; then body=Changed; fi
     if [ "$PROVIDER_CASE" = alias_edit ] && [ -f "$PROVIDER_ROOT/alias" ]; then body=Local; fi
     printf '{"id":300,"body":"%s"}' "$body"
   elif [ ! -f "$PROVIDER_ROOT/alias" ]; then touch "$PROVIDER_ROOT/alias"; printf 'lost write response' >&2; exit 1
   elif [ "$method" != DELETE ]; then printf '{"id":300,"body":"Local"}'; fi ;;
 */pulls/1/comments)
   if [ "$PROVIDER_CASE" = anchor_rejection ] || { [ "$PROVIDER_CASE" = schema_rejection ] && [ ! -f "$PROVIDER_ROOT/rejected" ]; }; then
     touch "$PROVIDER_ROOT/rejected"
     printf '%s' "$PROVIDER_REJECTION_ERROR" >&2
     exit 1
   fi
   printf '{"id":400,"node_id":"C400"}' ;;
 */issues/1/comments)
   if [ "$PROVIDER_CASE" = unknown ]; then printf 'connection lost after sending' >&2; exit 1; fi
   if [ "$PROVIDER_CASE" = concurrent ] && [ -f "$PROVIDER_ROOT/issue" ]; then printf '{"id":301,"node_id":"C301"}'; exit; fi
   touch "$PROVIDER_ROOT/issue"
   printf '{"id":300,"node_id":"C300"}' ;;
 *) printf 'unexpected endpoint %s' "$endpoint" >&2; exit 1 ;;
esac
"#).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(directory.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "provider_child", "--nocapture"])
        .env("PROVIDER_CASE", case)
        .env(
            "PROVIDER_REJECTION_ERROR",
            if case == "anchor_rejection" {
                ANCHOR_REJECTION
            } else {
                SCHEMA_REJECTION
            },
        )
        .env("PROVIDER_TEST_BIN", std::env::current_exe().unwrap())
        .env("PROVIDER_ROOT", directory.path())
        .env("PROVIDER_LOG", directory.path().join("log"))
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn location(head: &str) -> CommentTarget {
    CommentTarget::Diff(DiffLocation {
        revision: DiffRevision {
            base: "old-base".into(),
            head: head.into(),
        },
        position: CommentPosition {
            path: "file.rs".into(),
            range: Some(DiffRange {
                start: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(2).unwrap(),
                },
                end: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(4).unwrap(),
                },
            }),
        },
    })
}

#[test]
fn provider_child() {
    let Ok(case) = std::env::var("PROVIDER_CASE") else {
        return;
    };
    if case == "listing" {
        assert_eq!(github::repository().unwrap(), "owner/repo");
        let rows = github::pull_requests("owner/repo", "all", Some("@me")).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.number).collect::<Vec<_>>(),
            vec![4, 3]
        );
        assert_eq!(rows[0].state, PullRequestStatus::Merged);
        let rows = github::pull_requests("owner/repo", "merged", None).unwrap();
        assert_eq!(rows.len(), 1);
        return;
    }
    let root = std::env::var("PROVIDER_ROOT").unwrap();
    let mut store = Store::open(&root).unwrap();
    let reviewer = User {
        id: UserId(Uuid::new_v5(&Uuid::NAMESPACE_URL, b"https://github.com/user/7").to_string()),
        name: "reviewer".into(),
    };
    let mut read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "PR".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "old-base".into(),
                head: "old-head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "file.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
            viewed: false,
        }],
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    };
    let mut backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/owner/repo/pull/1".into(),
        },
        mappings: vec![
            IdMapping {
                resource: "user".into(),
                core_id: reviewer.id.0.clone(),
                remote_id: "7".into(),
            },
            IdMapping {
                resource: "pull_request".into(),
                core_id: "pull".into(),
                remote_id: "1".into(),
            },
        ],
    };
    if case.starts_with("alias_") || case == "resumed_conflict" {
        read.comments.push(ReadComment {
            content: CommentContent {
                id: CommentId("local-alias".into()),
                body: "Original".into(),
            },
            author: Some(reviewer.clone()),
            target: CommentTarget::PullRequest,
        });
        backend.mappings.push(IdMapping {
            resource: "issue_comment".into(),
            core_id: "local-alias".into(),
            remote_id: "300".into(),
        });
    }
    if case == "summary" {
        read.comments.push(ReadComment {
            content: CommentContent {
                id: CommentId("summary".into()),
                body: "Summary".into(),
            },
            author: Some(reviewer.clone()),
            target: CommentTarget::PullRequest,
        });
        backend.mappings.extend([
            IdMapping {
                resource: "review_summary".into(),
                core_id: "summary".into(),
                remote_id: "100".into(),
            },
            IdMapping {
                resource: "review_node".into(),
                core_id: "summary".into(),
                remote_id: "R100".into(),
            },
        ]);
    }
    store
        .import_review(&ReviewId("snapshot".into()), &reviewer, &read, &backend)
        .unwrap();
    let mut workspace = store.open_workspace(&ReviewId("snapshot".into())).unwrap();
    if matches!(
        case.as_str(),
        "schema_rejection" | "schema_recovery" | "anchor_rejection"
    ) {
        let target = location("old-head");
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(target.clone()),
                    body: "This is a test".into(),
                    destination: None,
                },
            )
            .unwrap();
        let draft = workspace.saved_comment.clone().unwrap();
        let mut rejected = None;
        if case == "schema_recovery" {
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::RejectAnchor {
                        comment: draft.clone(),
                        message: format!("gh failed: {SCHEMA_REJECTION}"),
                    },
                )
                .unwrap();
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(target.clone()),
                        body: "Rejected location".into(),
                        destination: None,
                    },
                )
                .unwrap();
            rejected = workspace.saved_comment.clone();
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::RejectAnchor {
                        comment: rejected.clone().unwrap(),
                        message: ANCHOR_REJECTION.into(),
                    },
                )
                .unwrap();
            drop(store);
            store = Store::open(&root).unwrap();
            assert_eq!(store.workspace(&workspace.id).unwrap().unwrap(), workspace);
        } else {
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("HTTP 422"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.attempt.is_none());
            assert!(workspace.suspended_attempts.is_empty());
            assert_eq!(workspace.pending().unwrap().operations.len(), 1);
            if case == "anchor_rejection" {
                assert!(workspace.rejected_anchors.contains_key(&draft));
            } else {
                assert!(workspace.rejected_anchors.is_empty());
                assert!(workspace.last_error.as_deref().unwrap().contains("oneOf"));
            }
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        if case == "anchor_rejection" {
            assert!(workspace.rejected_anchors.contains_key(&draft));
            assert!(workspace.completed.is_empty());
        } else {
            assert!(!workspace.rejected_anchors.contains_key(&draft));
            assert_eq!(workspace.completed.len(), 1);
            let Event::CreateComment { comment } = &workspace.completed[0].change else {
                panic!("original draft must be created");
            };
            assert_eq!(comment.content.id, draft);
            assert_eq!(comment.content.body, "This is a test");
            assert_eq!(comment.target, target);
            assert_eq!(
                workspace.pending().unwrap().operations.len(),
                usize::from(rejected.is_some())
            );
            if let Some(rejected) = rejected {
                assert_eq!(workspace.rejected_anchors.len(), 1);
                assert!(workspace.rejected_anchors.contains_key(&rejected));
                assert!(
                    store
                        .workspace_log(&workspace.id)
                        .unwrap()
                        .iter()
                        .any(|entry| {
                            entry["action"]["message"]
                                .as_str()
                                .is_some_and(|message| message.contains("oneOf"))
                        })
                );
            }
        }
        let requests: Vec<Value> = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|request: &Value| request["method"] == "POST")
            .collect();
        assert_eq!(
            requests.len(),
            if case == "schema_rejection" { 2 } else { 1 }
        );
        assert!(requests.iter().all(|request| {
            request["body"]["body"] == "This is a test"
                && request["body"].get("subject_type").is_none()
        }));
        return;
    }
    if case.starts_with("alias_") || case == "resumed_conflict" {
        let comment = CommentId("local-alias".into());
        let action = match case.as_str() {
            "alias_delete" => Action::Delete { comment },
            "alias_reaction" => Action::Reaction {
                comment,
                present: true,
            },
            _ => Action::Save {
                comment: Some(comment),
                target: None,
                body: "Local".into(),
                destination: None,
            },
        };
        workspace = store
            .mutate_workspace(&workspace.id, workspace.generation, action)
            .unwrap();
        if case == "resumed_conflict" {
            let operations = workspace
                .prepare(buf_review::workspace::Destination::Standalone)
                .unwrap()
                .operations;
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::BeginPublication {
                        destination: buf_review::workspace::Destination::Standalone,
                        operations,
                    },
                )
                .unwrap();
        }
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap_err();
        if case == "resumed_conflict" {
            assert!(error.to_string().contains("changed"));
            let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert!(!log.contains("\"method\":\"PATCH\""));
            return;
        }
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert!(workspace.suspended_attempts.is_empty());
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let method = match case.as_str() {
            "alias_delete" => "DELETE",
            "alias_edit" => "PATCH",
            _ => "POST",
        };
        let expected = if case == "alias_delete" { 2 } else { 1 };
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == method
                    && request["endpoint"]
                        .as_str()
                        .unwrap()
                        .contains("issues/comments/300"))
                .count(),
            expected
        );
        return;
    }
    if case == "summary" {
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: Some(CommentId("summary".into())),
                    target: None,
                    body: "Summary edited".into(),
                    destination: None,
                },
            )
            .unwrap();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Reaction {
                    comment: CommentId("summary".into()),
                    present: true,
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(requests.iter().any(|request| request["endpoint"]
            == "repos/owner/repo/pulls/1/reviews/100"
            && request["method"] == "PUT"
            && request["body"]["body"] == "Summary edited"));
        assert!(
            requests
                .iter()
                .any(
                    |request| request["body"]["variables"]["input"]["subjectId"] == "R100"
                        && request["body"]["variables"]["input"]["content"] == "THUMBS_UP"
                )
        );
        assert!(
            store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Delete {
                        comment: CommentId("summary".into())
                    }
                )
                .is_err()
        );
        return;
    }
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::PullRequest),
                body: "Standalone".into(),
                destination: None,
            },
        )
        .unwrap();
    if case == "unknown" || case == "account" {
        let failure =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap_err();
        if case == "account" {
            assert!(failure.to_string().contains("differs"));
        } else {
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.suspended_attempts[0].uncertain);
            let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                .unwrap()
                .matches("\"method\":\"POST\"")
                .count();
            let second =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(second.to_string().contains("uncertain"));
            let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                .unwrap()
                .matches("\"method\":\"POST\"")
                .count();
            assert_eq!(before, after);
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(location("old-head")),
                        body: "Unrelated".into(),
                        destination: None,
                    },
                )
                .unwrap();
            workspace =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap();
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert!(workspace.attempt.is_none());
            assert_eq!(workspace.pending().unwrap().operations.len(), 1);
        }
        return;
    }
    workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::Start)
        .unwrap();
    if case == "receipt_reply" {
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(location("old-head")),
                    body: "Root".into(),
                    destination: Some(buf_review::workspace::Destination::Standalone),
                },
            )
            .unwrap();
        let parent = workspace.saved_comment.clone().unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(workspace.review.read.comments.is_empty());
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(CommentTarget::Reply(parent)),
                    body: "Grouped reply".into(),
                    destination: None,
                },
            )
            .unwrap();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Summary {
                    conclusion: ReviewConclusion {
                        outcome: ReviewOutcome::Comment,
                        body: Some("Summary".into()),
                    },
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, true)
                .unwrap();
        assert!(workspace.view().unwrap().todo.is_empty());
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        assert!(log.contains("T400"));
        assert!(log.contains("addPullRequestReviewThreadReply"));
        return;
    }

    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(location("old-head")),
                body: "Root".into(),
                destination: None,
            },
        )
        .unwrap();
    let parent = workspace.saved_comment.clone().unwrap();
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::Reply(parent.clone())),
                body: "Reply".into(),
                destination: None,
            },
        )
        .unwrap();
    let reply_id = workspace.saved_comment.clone().unwrap();
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Summary {
                conclusion: ReviewConclusion {
                    outcome: ReviewOutcome::Comment,
                    body: Some("Summary".into()),
                },
            },
        )
        .unwrap();
    if case == "mixed" {
        workspace.comparison.head = "other-head".into();
        workspace = store
            .save_workspace(workspace.generation, &workspace)
            .unwrap();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(location("other-head")),
                    body: "Other root".into(),
                    destination: None,
                },
            )
            .unwrap();
        let failure =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, true)
                .unwrap_err();
        assert!(failure.to_string().contains("different head commits"));
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        assert!(!log.contains("\"method\":\"POST\""));
        return;
    }
    workspace.review.read.pull_request.revision.head = "new-head".into();
    workspace = store
        .save_workspace(workspace.generation, &workspace)
        .unwrap();
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Viewed {
                path: "file.rs".into(),
                viewed: true,
            },
        )
        .unwrap();
    fs::write(
        std::path::Path::new(&root).join("workspace-id"),
        &workspace.id,
    )
    .unwrap();
    if case == "uncertain_review" {
        github::publish_workspace(&mut store, &workspace.id, workspace.generation, true)
            .unwrap_err();
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        assert_eq!(workspace.suspended_attempts.len(), 1);
        assert!(workspace.wip.is_some());
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert_eq!(workspace.suspended_attempts.len(), 1);
        assert!(workspace.wip.is_some());
        assert!(workspace.attempt.is_none());
        assert!(
            workspace
                .prepare(buf_review::workspace::Destination::Standalone)
                .unwrap()
                .operations
                .is_empty()
        );
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        assert!(log.contains("issues/1/comments"));
        assert!(log.contains("markFileAsViewed"));
        return;
    }
    if matches!(
        case.as_str(),
        "retry"
            | "drop"
            | "progress"
            | "uncertain_submit"
            | "relocate"
            | "relocate_head"
            | "uncertain_progress"
    ) {
        github::publish_workspace(&mut store, &workspace.id, workspace.generation, true)
            .unwrap_err();
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        match case.as_str() {
            "retry" => {
                // Existing failed submissions predate typed node mappings. The
                // original provider response still carries the private node.
                for prepared in workspace.preparation["prepared"].as_array_mut().unwrap() {
                    prepared["mappings"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|mapping| mapping["resource"] != "review_comment_node");
                }
                workspace = store
                    .save_workspace(workspace.generation, &workspace)
                    .unwrap();
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Save {
                            comment: Some(parent),
                            target: None,
                            body: "Root edited".into(),
                            destination: None,
                        },
                    )
                    .unwrap();
            }
            "drop" => {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Delete { comment: reply_id },
                    )
                    .unwrap();
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Delete { comment: parent },
                    )
                    .unwrap();
            }
            "relocate" | "relocate_head" => {
                let head = if case == "relocate_head" {
                    "relocated-head"
                } else {
                    "old-head"
                };
                if case == "relocate_head" {
                    workspace.comparison.head = head.into();
                    workspace = store
                        .save_workspace(workspace.generation, &workspace)
                        .unwrap();
                }
                let CommentTarget::Diff(mut anchor) = location(head) else {
                    unreachable!()
                };
                anchor.position.range.as_mut().unwrap().start.line = NonZeroU32::new(9).unwrap();
                anchor.position.range.as_mut().unwrap().end.line = NonZeroU32::new(9).unwrap();
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Relocate {
                            comment: parent,
                            location: anchor,
                        },
                    )
                    .unwrap();
            }
            "progress" => {
                assert!(workspace.wip.is_none());
                assert!(workspace.attempt.is_none());
                workspace = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap();
                assert!(workspace.pending().unwrap().operations.is_empty());
                let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                assert_eq!(log.matches("/reviews/100/events").count(), 1);
                return;
            }
            "uncertain_progress" => {
                assert!(workspace.wip.is_none());
                assert_eq!(workspace.suspended_attempts.len(), 1);
                workspace = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap();
                workspace = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap();
                assert!(workspace.pending().unwrap().operations.is_empty());
                assert!(workspace.suspended_attempts.is_empty());
                let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                assert_eq!(log.matches("/reviews/100/events").count(), 1);
                assert_eq!(log.matches("MarkFileAsViewedInput").count(), 1);
                return;
            }
            _ => {}
        }
    }
    workspace =
        github::publish_workspace(&mut store, &workspace.id, workspace.generation, true).unwrap();
    assert!(workspace.wip.is_none());
    assert!(workspace.attempt.is_none());
    assert_eq!(
        workspace.pending().unwrap().operations.len(),
        if case == "concurrent" { 2 } else { 1 }
    );
    if case == "concurrent" {
        assert!(workspace.pending().unwrap().operations.iter().any(|operation|matches!(&operation.change,Event::CreateComment{comment} if comment.content.body=="Concurrent")));
    }
    let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
    let requests = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let prepared = requests
        .iter()
        .find(|request| {
            request["endpoint"] == "repos/owner/repo/pulls/1/reviews" && request["method"] == "POST"
        })
        .unwrap();
    assert_eq!(prepared["body"]["commit_id"], "old-head");
    let reply = requests
        .iter()
        .find(|request| {
            request["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("AddPullRequestReviewThreadReplyInput"))
        })
        .unwrap();
    assert_eq!(
        reply["body"]["variables"]["input"]["pullRequestReviewId"],
        "R100"
    );
    assert_eq!(
        reply["body"]["variables"]["input"]["pullRequestReviewThreadId"],
        "T201"
    );
    assert!(!log.contains("issues/1/comments"));
    if matches!(case.as_str(), "retry" | "drop" | "uncertain_submit") {
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("AddPullRequestReviewThreadInput!")))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(
                    |request| request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
                        && request["method"] == "POST"
                )
                .count(),
            1
        );
    }
    if case == "retry" {
        let edit = requests
            .iter()
            .find(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("UpdatePullRequestReviewCommentInput"))
            })
            .unwrap();
        assert_eq!(
            edit["body"]["variables"]["input"]["pullRequestReviewCommentId"],
            "C201"
        );
        assert_eq!(edit["body"]["variables"]["input"]["body"], "Root edited");
        assert!(!requests.iter().any(|request| request["method"] == "PATCH"));
    }
    if case == "uncertain_submit" {
        assert_eq!(log.matches("/reviews/100/events").count(), 1);
    }
    if case == "drop" {
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "DELETE")
                .count(),
            2
        );
    }
    if case == "relocate" {
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "DELETE")
                .count(),
            2
        );
    }
    if case == "relocate_head" {
        let creates = requests
            .iter()
            .filter(|request| {
                request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
                    && request["method"] == "POST"
            })
            .collect::<Vec<_>>();
        assert_eq!(creates.len(), 2);
        assert_eq!(creates[1]["body"]["commit_id"], "relocated-head");
    }
    workspace =
        github::publish_workspace(&mut store, &workspace.id, workspace.generation, false).unwrap();
    assert!(workspace.pending().unwrap().operations.is_empty());
}

#[test]
fn provider_concurrent_child() {
    if std::env::var("PROVIDER_CASE").as_deref() != Ok("concurrent") {
        return;
    }
    let root = std::env::var("PROVIDER_ROOT").unwrap();
    let id = fs::read_to_string(std::path::Path::new(&root).join("workspace-id")).unwrap();
    let mut store = Store::open(&root).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::PullRequest),
                body: "Concurrent".into(),
                destination: Some(buf_review::workspace::Destination::Standalone),
            },
        )
        .unwrap();
}
