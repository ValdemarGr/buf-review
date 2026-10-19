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
fn publication_preserves_global_order_and_exact_historical_head() {
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
fn successfully_prepared_drafts_cannot_be_deleted() {
    run("drop");
}
#[test]
fn progress_failure_preserves_prepared_comments_and_blocks_conclusion() {
    run("progress");
}
#[test]
fn uncertain_submission_reconciles_without_second_submit() {
    run("uncertain_submit");
}
#[test]
fn summaries_are_immutable_but_reactions_use_review_resources() {
    run("summary");
}
#[test]
fn uncertain_review_blocks_later_operations_without_skipping() {
    run("uncertain_review");
}
#[test]
fn prepared_roots_and_replies_reject_relocation() {
    run("relocate");
}
#[test]
fn prepared_review_retains_original_commit_after_relocation_rejection() {
    run("relocate_head");
}
#[test]
fn reply_targets_prepared_root_receipt_before_refetch() {
    run("receipt_reply");
}
#[test]
fn lost_progress_response_reconciles_before_concluding_review_once() {
    run("uncertain_progress");
}
#[test]
fn fetched_comment_delete_rejected_before_provider_writes() {
    run("alias_delete");
}
#[test]
fn fetched_comment_edit_rejected_before_provider_writes() {
    run("alias_edit");
}
#[test]
fn uncertain_reaction_uses_canonical_published_identity() {
    run("alias_reaction");
}
#[test]
fn remote_body_changes_cannot_be_overwritten_by_local_edit() {
    run("resumed_conflict");
}
#[test]
fn provider_receipts_merge_concurrent_unrelated_saves() {
    run("concurrent");
}
#[test]
fn adjacent_roots_share_native_review_creation_and_keep_individual_receipts() {
    run("bulk");
}
#[test]
fn terminal_conclusion_joins_native_review_creation() {
    run("bulk_conclude");
}
#[test]
fn failed_bulk_receipt_read_retries_without_recreating_review() {
    run("bulk_receipts");
}
#[test]
fn viewed_operation_separates_root_groups_without_reordering() {
    run("bulk_boundary");
}
#[test]
fn failed_group_boundary_blocks_later_roots_until_retry() {
    run("bulk_boundary_failure");
}
#[test]
fn reply_before_root_uses_the_roots_pinned_head_for_pending_review() {
    run("reply_first");
}
#[test]
fn owned_pending_review_refresh_imports_members_and_external_submission_starts_fresh() {
    run("bulk_external");
}
#[test]
fn external_review_submission_preserves_newly_queued_conclusion() {
    run("bulk_external_conclusion");
}
#[test]
fn historical_bulk_receipts_match_original_anchors() {
    run("bulk_old_anchor");
}
#[test]
fn pending_bulk_receipts_recover_single_line_head_anchors() {
    run("bulk_null_head");
}
#[test]
fn pending_bulk_receipts_recover_single_line_base_anchors() {
    run("bulk_null_base");
}
#[test]
fn pending_bulk_receipts_recover_multiline_anchors() {
    run("bulk_null_multiline");
}
#[test]
fn pending_bulk_receipts_find_anchors_on_later_thread_pages() {
    run("bulk_null_pages");
}
#[test]
fn pending_bulk_receipts_reject_mismatched_graphql_anchors() {
    run("bulk_null_mismatch");
}
#[test]
fn failed_pending_anchor_read_retries_existing_review_without_duplicate_writes() {
    run("bulk_null_retry");
}
#[test]
fn fetched_pending_comments_reconcile_with_later_bulk_receipts() {
    run("bulk_refresh_collision");
}
#[test]
fn reply_lookup_failure_is_retryable_without_an_uncertain_write() {
    run("reply_lookup_fail");
}
#[test]
fn legacy_standalone_inline_drafts_require_explicit_move_before_publication() {
    run("legacy_standalone");
}

fn run(case: &str) {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("review.py"), r#"import json, os, pathlib, sys
root = pathlib.Path(os.environ["PROVIDER_ROOT"])
case = os.environ["PROVIDER_CASE"]
if sys.argv[1] == "thread":
    body = json.load(sys.stdin)
    counter = root / "thread-counter"
    count = int(counter.read_text()) if counter.exists() else len(json.loads((root / "review-comments.json").read_text()))
    identity = 201 + count
    counter.write_text(str(count + 1))
    print(json.dumps({"data": {"addPullRequestReviewThread": {"thread": {"id": "T" + str(identity), "comments": {"nodes": [{"id": "C" + str(identity), "databaseId": identity}]}}}}}))
elif sys.argv[1] == "comments":
    if case in ("bulk_receipts", "bulk_refresh_collision") and not (root / "discovery").exists():
        (root / "discovery").touch()
        sys.stderr.write("HTTP 503 review comments temporarily unavailable")
        sys.exit(1)
    comments = json.loads((root / "review-comments.json").read_text())
    if case == "bulk_old_anchor":
        for comment in comments:
            comment["original_commit_id"] = comment["commit_id"]
            comment["original_line"] = comment["line"]
            comment["original_start_line"] = comment.get("start_line")
            comment.update(commit_id="new-head", line=None, start_line=None)
    if case.startswith("bulk_null_"):
        for comment in comments:
            comment["original_commit_id"] = comment["commit_id"]
            comment["position"] = 1
            comment.update(line=None, original_line=None, side=None,
                           start_line=None, original_start_line=None, start_side=None)
    print(json.dumps([comments]))
elif sys.argv[1] == "anchors":
    body = json.load(sys.stdin)
    if case == "bulk_null_retry" and not (root / "anchor-read").exists():
        (root / "anchor-read").touch()
        sys.stderr.write("HTTP 503 review thread anchors temporarily unavailable")
        sys.exit(1)
    comments = json.loads((root / "review-comments.json").read_text())
    nodes = []
    for comment in comments:
        line = comment["line"] + (1 if case == "bulk_null_mismatch" else 0)
        nodes.append(dict(id="T" + str(comment["id"]), diffSide=comment["side"],
                          startDiffSide=comment.get("start_side"),
                          comments=dict(nodes=[dict(id=comment["node_id"], body=comment["body"],
                              path=comment["path"], line=line, originalLine=line,
                              startLine=comment.get("start_line"), originalStartLine=comment.get("start_line"),
                              commit=dict(oid=comment["commit_id"]), originalCommit=dict(oid=comment["commit_id"]))])))
    next_page = case == "bulk_null_pages" and body["variables"].get("cursor") is None
    if case == "bulk_null_pages":
        nodes = nodes[:1] if next_page else nodes[1:]
    print(json.dumps(dict(data=dict(repository=dict(pullRequest=dict(reviewThreads=dict(
        nodes=nodes, pageInfo=dict(hasNextPage=next_page, endCursor="second" if next_page else None))))))))
else:
    body = json.load(sys.stdin)
    if case == "anchor_rejection" or (case == "schema_rejection" and not (root / "rejected").exists()):
        (root / "rejected").touch()
        sys.stderr.write(os.environ["PROVIDER_REJECTION_ERROR"])
        sys.exit(1)
    counter = root / "review-counter"
    review_id = int(counter.read_text()) if counter.exists() else 100
    counter.write_text(str(review_id + 1))
    first_comment = 201 + 100 * (review_id - 100)
    comments = [dict(comment, id=first_comment + index, node_id="C" + str(first_comment + index), commit_id=body["commit_id"])
                for index, comment in enumerate(body.get("comments", []))]
    (root / "review-comments.json").write_text(json.dumps(comments))
    if case == "uncertain_review":
        sys.stderr.write("connection lost after preparing comment")
        sys.exit(1)
    state = {"COMMENT": "COMMENTED", "APPROVE": "APPROVED", "REQUEST_CHANGES": "CHANGES_REQUESTED"}.get(body.get("event"), "PENDING")
    print(json.dumps(dict(id=review_id, node_id="R" + str(review_id), state=state, body=body.get("body", ""))))
"#).unwrap();
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
 */reviews/*/comments?*) python3 "$PROVIDER_ROOT/review.py" comments ;;
 */reviews) printf '%s' "$body" | python3 "$PROVIDER_ROOT/review.py" create ;;
 */pulls/comments/400) printf '{"id":400,"body":"Root","in_reply_to_id":null}' ;;
 */pulls/comments/*)
   if [ "$method" = PATCH ]; then printf 'HTTP 404 private review comments are unavailable through REST update' >&2; exit 1; fi
   if [ "$method" != DELETE ]; then printf '{"id":201,"body":"Root edited"}'; fi ;;
 graphql)
   case "$body" in
    *BulkReviewCommentAnchors*) printf '%s' "$body" | python3 "$PROVIDER_ROOT/review.py" anchors ;;
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
      printf '%s' "$body" | python3 "$PROVIDER_ROOT/review.py" thread ;;
    *updatePullRequestReviewComment*) printf '{"data":{"updatePullRequestReviewComment":{"pullRequestReviewComment":{"id":"C201","body":"Root edited"}}}}' ;;
    *markFileAsViewed*)
      if { [ "$PROVIDER_CASE" = progress ] || [ "$PROVIDER_CASE" = bulk_boundary_failure ]; } && [ ! -f "$PROVIDER_ROOT/progress" ]; then touch "$PROVIDER_ROOT/progress"; printf 'HTTP 422 viewed state rejected' >&2; exit 1; fi
      if [ "$PROVIDER_CASE" = uncertain_progress ] && [ ! -f "$PROVIDER_ROOT/progress" ]; then touch "$PROVIDER_ROOT/progress"; printf 'connection lost after setting viewed state' >&2; exit 1; fi
      printf '{"data":{"markFileAsViewed":{"clientMutationId":"done"}}}' ;;
    *addReaction*) printf '{"data":{"addReaction":{"clientMutationId":"done"}}}' ;;
    *reviewThreads*)
      if [ "$PROVIDER_CASE" = reply_lookup_fail ] && [ ! -f "$PROVIDER_ROOT/thread-lookup" ]; then touch "$PROVIDER_ROOT/thread-lookup"; printf 'HTTP 503 thread lookup unavailable' >&2; exit 1; fi
      printf '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[{"id":"T400","comments":{"nodes":[{"databaseId":400}]}},{"id":"T201","comments":{"nodes":[{"databaseId":201}]}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}' ;;
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
    if matches!(case.as_str(), "reply_first" | "reply_lookup_fail") {
        read.pull_request.revision.head = "new-head".into();
        let CommentTarget::Diff(anchor) = location("old-head") else {
            unreachable!()
        };
        read.comments.push(ReadComment {
            content: CommentContent {
                id: CommentId("existing-root".into()),
                body: "Existing root".into(),
            },
            author: Some(reviewer.clone()),
            draft: false,
            target: CommentTarget::Diff(ReadDiffLocation {
                original: ReadCommentAnchor {
                    revision: ReadCommentRevision::Comparison(anchor.revision),
                    position: anchor.position,
                },
                current: None,
                subject: Some(CommentSubject::Line),
                diff_hunk: None,
                outdated: Some(true),
            }),
        });
        backend.mappings.extend([
            IdMapping {
                resource: "review_comment".into(),
                core_id: "existing-root".into(),
                remote_id: "400".into(),
            },
            IdMapping {
                resource: "review_thread".into(),
                core_id: "existing-root".into(),
                remote_id: "T400".into(),
            },
        ]);
        if case == "reply_lookup_fail" {
            backend
                .mappings
                .retain(|mapping| mapping.resource != "review_thread");
        }
    }
    if case.starts_with("alias_") || case == "resumed_conflict" {
        read.comments.push(ReadComment {
            draft: false,
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
            draft: false,
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
    if case == "legacy_standalone" {
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(location("old-head")),
                    body: "Legacy inline draft".into(),
                    destination: Some(buf_review::workspace::Destination::Standalone),
                },
            )
            .unwrap();
        let comment = workspace.saved_comment.clone().unwrap();
        workspace.destinations.insert(
            comment.clone(),
            buf_review::workspace::Destination::Standalone,
        );
        workspace = store
            .save_workspace(workspace.generation, &workspace)
            .unwrap();
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap_err();
        assert!(error.to_string().contains("move"));
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        assert!(workspace.completed.is_empty());
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap_or_default();
        assert!(!log.contains("\"method\":\"POST\""));
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::MoveToReview {
                    comments: vec![comment],
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert_eq!(workspace.completed.len(), 1);
        assert!(workspace.pending().unwrap().operations.is_empty());
        return;
    }
    if matches!(case.as_str(), "reply_first" | "reply_lookup_fail") {
        workspace.comparison.head = "old-head".into();
        workspace = store
            .save_workspace(workspace.generation, &workspace)
            .unwrap();
        for (target, body) in [
            (
                CommentTarget::Reply(CommentId("existing-root".into())),
                "Reply first",
            ),
            (location("old-head"), "Pinned root after reply"),
        ] {
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(target),
                        body: body.into(),
                        destination: None,
                    },
                )
                .unwrap();
        }
        if case == "reply_lookup_fail" {
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("thread lookup unavailable"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.completed.is_empty());
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert!(!workspace.suspended_attempts[0].uncertain);
            let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert!(!log.contains("AddPullRequestReviewThreadReplyInput"));
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(workspace.completed.len(), 2);
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let created = requests
            .iter()
            .find(|request| {
                request["method"] == "POST"
                    && request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
            })
            .unwrap();
        assert_eq!(
            created["body"]["commit_id"], "old-head",
            "pending review must use later root's saved anchor"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("AddPullRequestReviewThreadReplyInput")))
                .count(),
            1
        );
        return;
    }
    if case.starts_with("bulk") {
        let mut ids = Vec::new();
        for body in ["First bulk root", "Second bulk root"] {
            let mut target = location("old-head");
            if matches!(case.as_str(), "bulk_null_head" | "bulk_null_base") {
                let CommentTarget::Diff(location) = &mut target else {
                    unreachable!()
                };
                let line = DiffLine {
                    side: if case == "bulk_null_base" {
                        DiffSide::Base
                    } else {
                        DiffSide::Head
                    },
                    line: NonZeroU32::new(80).unwrap(),
                };
                location.position.range = Some(DiffRange {
                    start: line.clone(),
                    end: line,
                });
            }
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(target),
                        body: body.into(),
                        destination: None,
                    },
                )
                .unwrap();
            ids.push(workspace.saved_comment.clone().unwrap());
        }
        if case.starts_with("bulk_boundary") {
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
            for body in ["Third bulk root", "Fourth bulk root"] {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Save {
                            comment: None,
                            target: Some(location("old-head")),
                            body: body.into(),
                            destination: None,
                        },
                    )
                    .unwrap();
                ids.push(workspace.saved_comment.clone().unwrap());
            }
        }
        if case == "bulk_conclude" {
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
        }
        let mut resumed_publication = None;
        if matches!(
            case.as_str(),
            "bulk_receipts" | "bulk_null_retry" | "bulk_refresh_collision"
        ) {
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("503"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.completed.is_empty());
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert!(workspace.suspended_attempts[0].uncertain);
            assert_eq!(
                workspace.suspended_attempts[0].provider["pending"]["id"],
                100
            );
            resumed_publication = Some(workspace.suspended_attempts[0].id.clone());
        }
        let mut refreshed_snapshot = None;
        if case == "bulk_refresh_collision" {
            let mut fresh = workspace.review.read.clone();
            fresh.comments = workspace
                .pending()
                .unwrap()
                .operations
                .iter()
                .filter_map(|operation| {
                    let Event::CreateComment { comment } = &operation.change else {
                        return None;
                    };
                    let CommentTarget::Diff(location) = &comment.target else {
                        return None;
                    };
                    Some(ReadComment {
                        content: CommentContent {
                            id: CommentId(format!("fetched-{}", comment.content.id.0)),
                            body: comment.content.body.clone(),
                        },
                        author: Some(comment.author.clone()),
                        draft: true,
                        target: CommentTarget::Diff(ReadDiffLocation {
                            original: ReadCommentAnchor {
                                revision: ReadCommentRevision::Comparison(
                                    location.revision.clone(),
                                ),
                                position: location.position.clone(),
                            },
                            current: None,
                            subject: Some(CommentSubject::Line),
                            diff_hunk: None,
                            outdated: Some(false),
                        }),
                    })
                })
                .collect();
            fresh.comments.push(ReadComment {
                content: CommentContent {
                    id: CommentId("fetched-reply".into()),
                    body: "Reply fetched before receipt recovery".into(),
                },
                author: Some(reviewer.clone()),
                draft: true,
                target: CommentTarget::Reply(fresh.comments[0].content.id.clone()),
            });
            fresh.reactions.push(CommentReaction {
                comment: fresh.comments[0].content.id.clone(),
                author: Some(reviewer.id.clone()),
                reaction: "+1".into(),
            });
            fresh.reviews.push(ReadReview {
                id: Some(ReviewResourceId("fetched-review".into())),
                author: Some(reviewer.clone()),
                state: ReadReviewState::Inactive {
                    state: InactiveReviewState::Pending,
                    body: None,
                },
            });
            let mut fetched_backend = backend.clone();
            fetched_backend.mappings.push(IdMapping {
                resource: "review".into(),
                core_id: "fetched-review".into(),
                remote_id: "100".into(),
            });
            for (index, comment) in fresh.comments.iter().enumerate() {
                for (resource, remote_id) in [
                    ("review_comment", (201 + index).to_string()),
                    ("review_comment_node", format!("C{}", 201 + index)),
                    ("review_member:100", (201 + index).to_string()),
                ] {
                    fetched_backend.mappings.push(IdMapping {
                        resource: resource.into(),
                        core_id: comment.content.id.0.clone(),
                        remote_id,
                    });
                }
            }
            let snapshot = ReviewId("pending-before-receipts".into());
            store
                .import_review(&snapshot, &reviewer, &fresh, &fetched_backend)
                .unwrap();
            workspace = store
                .refresh_workspace(&workspace.id, workspace.generation, &snapshot)
                .unwrap();
            assert!(workspace.completed.is_empty());
            assert_eq!(
                workspace.suspended_attempts[0].id,
                *resumed_publication.as_ref().unwrap()
            );
            refreshed_snapshot = Some((snapshot, fresh));
        }
        if case == "bulk_null_mismatch" {
            for _ in 0..2 {
                let error = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap_err();
                assert!(error.to_string().contains("identity unconfirmed"));
                workspace = store.workspace(&workspace.id).unwrap().unwrap();
                assert!(workspace.completed.is_empty());
                assert_eq!(workspace.suspended_attempts.len(), 1);
                assert!(workspace.suspended_attempts[0].uncertain);
                if let Some(publication) = &resumed_publication {
                    assert_eq!(&workspace.suspended_attempts[0].id, publication);
                }
                resumed_publication = Some(workspace.suspended_attempts[0].id.clone());
            }
            let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            let creates = requests
                .lines()
                .filter(|line| {
                    let request: Value = serde_json::from_str(line).unwrap();
                    request["method"] == "POST"
                        && request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
                })
                .count();
            assert_eq!(
                creates, 1,
                "unconfirmed identity must never recreate review"
            );
            return;
        }
        if case == "bulk_boundary_failure" {
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("viewed state rejected"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert_eq!(workspace.completed.len(), 2);
            let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert!(!log.contains("AddPullRequestReviewThreadInput"));
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        if let Some((snapshot, original)) = refreshed_snapshot {
            assert_eq!(store.get_review(&snapshot).unwrap().unwrap().read, original);
            for id in &ids {
                assert!(
                    workspace
                        .review
                        .read
                        .comments
                        .iter()
                        .any(|comment| comment.content.id == *id)
                );
            }
            assert_eq!(workspace.review.read.comments.len(), 3);
            assert_eq!(
                workspace.review.read.comments[2].target,
                CommentTarget::Reply(ids[0].clone())
            );
            assert_eq!(workspace.review.read.reactions[0].comment, ids[0]);
            let history = store.publication_history(&workspace).unwrap();
            let publication = resumed_publication.as_ref().unwrap();
            let entry = history
                .iter()
                .find(|entry| &entry.summary.id == publication)
                .unwrap();
            assert_eq!(entry.summary.published.total, 2);
        }
        assert_eq!(
            workspace.completed.len(),
            if case == "bulk_refresh_collision" {
                0
            } else if case == "bulk_conclude" {
                3
            } else if case.starts_with("bulk_boundary") {
                5
            } else {
                2
            }
        );
        for (index, id) in ids.iter().enumerate() {
            let result = workspace
                .results
                .iter()
                .find(|result| {
                    matches!(&result.operation.change,
                Event::CreateComment { comment } if &comment.content.id == id)
                })
                .unwrap();
            assert!(
                result
                    .mappings
                    .iter()
                    .any(|mapping| mapping.resource == "review_comment"
                        && mapping.core_id == id.0
                        && mapping.remote_id == (201 + index).to_string())
            );
        }
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let creates = requests
            .iter()
            .filter(|request| {
                request["method"] == "POST"
                    && request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
            })
            .collect::<Vec<_>>();
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0]["body"]["commit_id"], "old-head");
        assert_eq!(creates[0]["body"]["comments"].as_array().unwrap().len(), 2);
        assert_eq!(creates[0]["body"]["comments"][0]["body"], "First bulk root");
        assert_eq!(
            creates[0]["body"]["comments"][1]["body"],
            "Second bulk root"
        );
        let single_line = matches!(case.as_str(), "bulk_null_head" | "bulk_null_base");
        assert_eq!(
            creates[0]["body"]["comments"][0]["line"],
            if single_line { 80 } else { 4 }
        );
        assert_eq!(
            creates[0]["body"]["comments"][0]["start_line"],
            if single_line {
                Value::Null
            } else {
                Value::from(2)
            }
        );
        assert_eq!(
            creates[0]["body"]["comments"][0]["side"],
            if case == "bulk_null_base" {
                "LEFT"
            } else {
                "RIGHT"
            }
        );
        assert_eq!(
            creates[0]["body"]["event"].as_str(),
            (case == "bulk_conclude").then_some("COMMENT")
        );
        assert!(!requests.iter().any(|request| {
            request["endpoint"]
                .as_str()
                .is_some_and(|endpoint| endpoint.ends_with("/events"))
        }));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["endpoint"]
                    == "repos/owner/repo/pulls/1/reviews/100/comments?per_page=100")
                .count(),
            if matches!(
                case.as_str(),
                "bulk_receipts" | "bulk_null_retry" | "bulk_refresh_collision"
            ) {
                2
            } else {
                1
            }
        );
        if case.starts_with("bulk_null_") {
            let anchors = requests
                .iter()
                .filter(|request| {
                    request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("BulkReviewCommentAnchors"))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                anchors.len(),
                if matches!(case.as_str(), "bulk_null_pages" | "bulk_null_retry") {
                    2
                } else {
                    1
                }
            );
            if case == "bulk_null_pages" {
                assert_eq!(anchors[0]["body"]["variables"]["cursor"], Value::Null);
                assert_eq!(anchors[1]["body"]["variables"]["cursor"], "second");
            }
            if let Some(publication) = resumed_publication {
                let history = store.publication_history(&workspace).unwrap();
                let entry = history
                    .iter()
                    .find(|entry| entry.summary.id == publication)
                    .unwrap();
                assert_eq!(entry.summary.planned.total, 2);
                assert_eq!(entry.summary.published.total, 2);
            }
        }
        if case.starts_with("bulk_boundary") {
            let viewed = requests
                .iter()
                .position(|request| {
                    request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("markFileAsViewed"))
                })
                .unwrap();
            let roots = requests
                .iter()
                .enumerate()
                .filter(|(_, request)| {
                    request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("AddPullRequestReviewThreadInput"))
                })
                .collect::<Vec<_>>();
            assert_eq!(roots.len(), 2);
            assert!(viewed < roots[0].0 && roots[0].0 < roots[1].0);
            assert_eq!(
                roots[0].1["body"]["variables"]["input"]["body"],
                "Third bulk root"
            );
            assert_eq!(
                roots[1].1["body"]["variables"]["input"]["body"],
                "Fourth bulk root"
            );
        }
        if case.starts_with("bulk_external") {
            let mut fresh = workspace.review.read.clone();
            fresh.comments = workspace
                .completed
                .iter()
                .filter_map(|operation| {
                    let Event::CreateComment { comment } = &operation.change else {
                        return None;
                    };
                    let CommentTarget::Diff(location) = &comment.target else {
                        return None;
                    };
                    Some(ReadComment {
                        content: CommentContent {
                            id: CommentId(format!("fetched-{}", comment.content.id.0)),
                            body: comment.content.body.clone(),
                        },
                        author: Some(comment.author.clone()),
                        draft: true,
                        target: CommentTarget::Diff(ReadDiffLocation {
                            original: ReadCommentAnchor {
                                revision: ReadCommentRevision::Comparison(
                                    location.revision.clone(),
                                ),
                                position: location.position.clone(),
                            },
                            current: None,
                            subject: Some(CommentSubject::Line),
                            diff_hunk: None,
                            outdated: Some(false),
                        }),
                    })
                })
                .collect();
            fresh.comments.push(ReadComment {
                content: CommentContent {
                    id: CommentId("external-reply".into()),
                    body: "Additional pending reply from another tool".into(),
                },
                author: Some(reviewer.clone()),
                draft: true,
                target: CommentTarget::Reply(fresh.comments[0].content.id.clone()),
            });
            fresh.reviews = vec![ReadReview {
                id: Some(ReviewResourceId("fetched-review".into())),
                author: Some(reviewer.clone()),
                state: ReadReviewState::Inactive {
                    state: InactiveReviewState::Pending,
                    body: None,
                },
            }];
            let mut fetched_backend = backend.clone();
            fetched_backend.mappings.push(IdMapping {
                resource: "review".into(),
                core_id: "fetched-review".into(),
                remote_id: "100".into(),
            });
            for (index, comment) in fresh.comments.iter().enumerate() {
                for resource in ["review_comment", "review_member:100"] {
                    fetched_backend.mappings.push(IdMapping {
                        resource: resource.into(),
                        core_id: comment.content.id.0.clone(),
                        remote_id: (201 + index).to_string(),
                    });
                }
            }
            let pending_snapshot = ReviewId("fetched-pending".into());
            store
                .import_review(&pending_snapshot, &reviewer, &fresh, &fetched_backend)
                .unwrap();
            workspace = store
                .refresh_workspace(&workspace.id, workspace.generation, &pending_snapshot)
                .unwrap();
            let pending = workspace.view().unwrap();
            assert_eq!(pending.comments.len(), 3);
            assert!(
                pending
                    .comments
                    .iter()
                    .all(|comment| comment.fetched && comment.draft)
            );
            assert!(pending.todo.is_empty() && workspace.pending().unwrap().operations.is_empty());
            for id in &ids {
                assert!(
                    pending
                        .comments
                        .iter()
                        .any(|comment| comment.comment.content().id == *id)
                );
            }
            assert!(
                pending
                    .comments
                    .iter()
                    .any(|comment| comment.comment.content().body
                        == "Additional pending reply from another tool")
            );
            assert_eq!(workspace.preparation["pending"]["id"], 100);
            if case == "bulk_external_conclusion" {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Summary {
                            conclusion: ReviewConclusion {
                                outcome: ReviewOutcome::Comment,
                                body: Some("Next review summary".into()),
                            },
                        },
                    )
                    .unwrap();
            }
            for comment in &mut fresh.comments {
                comment.draft = false;
            }
            fresh.reviews[0].state = ReadReviewState::Submitted(ReviewConclusion {
                outcome: ReviewOutcome::Comment,
                body: Some("Concluded externally".into()),
            });
            let submitted_snapshot = ReviewId("fetched-submitted".into());
            store
                .import_review(&submitted_snapshot, &reviewer, &fresh, &fetched_backend)
                .unwrap();
            workspace = store
                .refresh_workspace(&workspace.id, workspace.generation, &submitted_snapshot)
                .unwrap();
            let submitted = workspace.view().unwrap();
            assert_eq!(submitted.comments.len(), 3);
            assert!(
                submitted
                    .comments
                    .iter()
                    .all(|comment| comment.fetched && !comment.draft)
            );
            for id in &ids {
                assert!(
                    submitted
                        .comments
                        .iter()
                        .any(|comment| comment.comment.content().id == *id)
                );
            }
            assert_eq!(submitted.todo.len(), 3);
            assert!(workspace.preparation.is_null());
            if case == "bulk_external_conclusion" {
                assert_eq!(
                    workspace
                        .wip
                        .as_ref()
                        .unwrap()
                        .conclusion
                        .as_ref()
                        .unwrap()
                        .body
                        .as_deref(),
                    Some("Next review summary")
                );
            } else {
                assert!(workspace.wip.is_none());
                assert!(workspace.pending().unwrap().operations.is_empty());
            }
            assert!(
                store
                    .get_review(&pending_snapshot)
                    .unwrap()
                    .unwrap()
                    .read
                    .comments
                    .iter()
                    .all(|comment| comment.draft)
            );
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(location("old-head")),
                        body: "New review after external submission".into(),
                        destination: None,
                    },
                )
                .unwrap();
            workspace =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap();
            assert!(workspace.pending().unwrap().operations.is_empty());
            let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert_eq!(
                log.matches(
                    "\"endpoint\":\"repos/owner/repo/pulls/1/reviews\",\"method\":\"POST\""
                )
                .count(),
                2
            );
            if case == "bulk_external" {
                assert_eq!(workspace.preparation["pending"]["id"], 101);
            }
        }
        return;
    }
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
            let rejected = workspace.saved_comment.clone().unwrap();
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::RejectAnchor {
                        comment: rejected.clone(),
                        message: ANCHOR_REJECTION.into(),
                    },
                )
                .unwrap();
            drop(store);
            store = Store::open(&root).unwrap();
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.rejected_anchors.contains_key(&rejected));
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("resolution"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(!workspace.rejected_anchors.contains_key(&draft));
            assert!(workspace.rejected_anchors.contains_key(&rejected));
            assert!(!std::path::Path::new(&std::env::var("PROVIDER_LOG").unwrap()).exists());
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Delete { comment: rejected },
                )
                .unwrap();
        } else {
            let error =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(error.to_string().contains("HTTP 422"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.attempt.is_none());
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert!(!workspace.suspended_attempts[0].uncertain);
            assert_eq!(workspace.pending().unwrap().operations.len(), 1);
            if case == "anchor_rejection" {
                assert!(workspace.rejected_anchors.contains_key(&draft));
                let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                let error = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap_err();
                assert!(error.to_string().contains("resolution"));
                assert_eq!(
                    fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap(),
                    before
                );
                assert!(workspace.completed.is_empty());
                return;
            }
            assert!(workspace.rejected_anchors.is_empty());
            assert!(workspace.last_error.as_deref().unwrap().contains("oneOf"));
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap();
        assert!(!workspace.rejected_anchors.contains_key(&draft));
        assert_eq!(workspace.completed.len(), 1);
        let Event::CreateComment { comment } = &workspace.completed[0].change else {
            panic!("original draft must be created")
        };
        assert_eq!(comment.content.id, draft);
        assert_eq!(comment.content.body, "This is a test");
        assert_eq!(comment.target, target);
        assert!(workspace.pending().unwrap().operations.is_empty());
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
        assert!(
            requests
                .iter()
                .all(
                    |request| request["body"]["comments"][0]["body"] == "This is a test"
                        && request["body"]["comments"][0].get("subject_type").is_none()
                )
        );
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
        if case != "alias_reaction" {
            let before = workspace.clone();
            let error = store
                .mutate_workspace(&workspace.id, workspace.generation, action)
                .unwrap_err();
            assert!(error.to_string().contains("immutable"));
            assert_eq!(store.workspace(&workspace.id).unwrap().unwrap(), before);
            assert!(!std::path::Path::new(&std::env::var("PROVIDER_LOG").unwrap()).exists());
            return;
        }
        workspace = store
            .mutate_workspace(&workspace.id, workspace.generation, action)
            .unwrap();
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap_err();
        assert!(error.to_string().contains("lost reaction response"));
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
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "POST"
                    && request["endpoint"]
                        .as_str()
                        .unwrap()
                        .contains("issues/comments/300"))
                .count(),
            1
        );
        return;
    }
    if case == "summary" {
        let error = store
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
            .unwrap_err();
        assert!(error.to_string().contains("immutable"));
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
        assert!(
            !requests.iter().any(|request| matches!(
                request["method"].as_str(),
                Some("PUT" | "PATCH" | "DELETE")
            ))
        );
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
            let blocked =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                    .unwrap_err();
            assert!(blocked.to_string().contains("uncertain"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert_eq!(workspace.pending().unwrap().operations.len(), 2);
            let after_unrelated = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                .unwrap()
                .matches("\"method\":\"POST\"")
                .count();
            assert_eq!(after_unrelated, before);
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
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::MoveToReview {
                    comments: vec![parent.clone()],
                },
            )
            .unwrap();
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
        assert!(log.contains("T201"));
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
        let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation, false)
                .unwrap_err();
        assert!(error.to_string().contains("uncertain"));
        let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        assert_eq!(
            before.matches("\"method\":\"POST\"").count(),
            after.matches("\"method\":\"POST\"").count()
        );
        assert!(before.contains("issues/1/comments"));
        assert!(!after.contains("markFileAsViewed"));
        assert!(workspace.wip.is_some());
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
                let error = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Save {
                            comment: Some(parent.clone()),
                            target: None,
                            body: "Root edited".into(),
                            destination: None,
                        },
                    )
                    .unwrap_err();
                assert!(error.to_string().contains("immutable"));
            }
            "drop" => {
                for comment in [reply_id.clone(), parent.clone()] {
                    let error = store
                        .mutate_workspace(
                            &workspace.id,
                            workspace.generation,
                            Action::Delete { comment },
                        )
                        .unwrap_err();
                    assert!(error.to_string().contains("immutable"));
                }
            }
            "relocate" | "relocate_head" => {
                let head = if case == "relocate_head" {
                    "relocated-head"
                } else {
                    "old-head"
                };
                let CommentTarget::Diff(mut anchor) = location(head) else {
                    unreachable!()
                };
                anchor.position.range.as_mut().unwrap().start.line = NonZeroU32::new(9).unwrap();
                anchor.position.range.as_mut().unwrap().end.line = NonZeroU32::new(9).unwrap();
                let error = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Relocate {
                            comment: parent.clone(),
                            location: anchor,
                        },
                    )
                    .unwrap_err();
                assert!(error.to_string().contains("immutable"), "{error}");
            }
            "progress" => {
                assert!(workspace.wip.is_some());
                assert!(workspace.attempt.is_none());
                assert!(
                    !fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                        .unwrap()
                        .contains("/reviews/100/events")
                );
                workspace = github::publish_workspace(
                    &mut store,
                    &workspace.id,
                    workspace.generation,
                    false,
                )
                .unwrap();
                assert!(workspace.pending().unwrap().operations.is_empty());
                assert!(workspace.wip.is_none());
                let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                assert_eq!(log.matches("/reviews/100/events").count(), 1);
                return;
            }
            "uncertain_progress" => {
                assert!(workspace.wip.is_some());
                assert_eq!(workspace.suspended_attempts.len(), 1);
                assert!(
                    !fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                        .unwrap()
                        .contains("/reviews/100/events")
                );
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
                assert!(workspace.wip.is_none());
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
        if case == "concurrent" { 1 } else { 0 }
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
    assert_eq!(
        log.matches("\"endpoint\":\"repos/owner/repo/issues/1/comments\",\"method\":\"POST\"")
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
    assert_eq!(prepared["body"]["comments"][0]["body"], "Root");
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request["method"].as_str(), Some("PATCH" | "DELETE")))
    );
    if case == "uncertain_submit" {
        assert_eq!(log.matches("/reviews/100/events").count(), 1);
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
