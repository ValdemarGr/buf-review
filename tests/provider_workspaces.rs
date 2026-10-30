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
fn viewed_files_reuse_one_pull_request_lookup_and_keep_ordered_receipts() {
    run("viewed_cache");
}
#[test]
fn failed_viewed_write_stops_and_retry_reuses_the_confirmed_lookup() {
    run("viewed_cache_failure");
}
#[test]
fn ready_families_use_bounded_batches_in_dependency_order() {
    run("families");
}
#[test]
fn partial_comment_batch_keeps_later_successes_and_retries_only_rejection() {
    run("comment_partial");
}
#[test]
fn unknown_comment_alias_keeps_other_receipts_and_never_blindly_resends() {
    run("comment_unknown");
}
#[test]
fn internal_alias_failure_after_creation_never_blindly_retries() {
    run("comment_internal");
}
#[test]
fn resource_limited_comment_batch_recovers_after_refresh_and_restart() {
    run("comment_resource");
}
#[test]
fn resource_limited_saved_attempt_recovers_from_its_original_error_log() {
    run("comment_resource_legacy");
}
#[test]
fn missing_comment_identity_stays_blocked_after_resource_failure() {
    run("comment_resource_missing");
}
#[test]
fn ambiguous_remote_comments_block_resource_failure_recovery() {
    run("comment_resource_ambiguous");
}
#[test]
fn changed_pending_review_author_blocks_resource_failure_recovery() {
    run("comment_resource_author");
}
#[test]
fn mismatched_partial_comment_identity_blocks_resource_failure_recovery() {
    run("comment_resource_node");
    run("comment_resource_database");
}
#[test]
fn review_submitted_during_recovery_is_not_recreated() {
    run("comment_resource_submitted");
}
#[test]
fn unknown_pr_wide_comment_is_not_retried_against_pending_review_members() {
    run("comment_resource_wide");
}
#[test]
fn existing_authenticated_pending_review_is_reused_before_batched_comments() {
    run("pending_reuse");
}
#[test]
fn discarded_cached_review_is_replaced_without_resending_receipts() {
    run("pending_discarded");
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
fn uncertain_retry_filters_stale_viewed_changes_after_reconciling_the_write() {
    run("uncertain_stale_viewed");
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
fn adjacent_roots_share_one_batch_and_keep_individual_receipts() {
    run("bulk");
}
#[test]
fn eighteen_roots_use_bounded_batches_before_one_conclusion() {
    run("bulk_large");
}
#[test]
fn terminal_conclusion_follows_batched_comments() {
    run("bulk_conclude");
}
#[test]
fn viewed_progress_follows_all_ready_roots() {
    run("bulk_boundary");
}
#[test]
fn failed_viewed_batch_retries_without_resending_roots() {
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
fn reply_lookup_failure_is_retryable_without_an_uncertain_write() {
    run("reply_lookup_fail");
}
#[test]
fn changed_remote_head_blocks_publication_without_mutating_local_work() {
    run("remote_head_changed");
}
#[test]
fn failed_git_fetch_blocks_publication_before_provider_writes() {
    run("fetch_failure");
}
#[test]
fn changed_checkout_during_remote_head_check_blocks_publication_without_writes() {
    run("checkout_head_changed");
}
#[test]
fn conclusion_without_inline_roots_uses_validated_head_when_cached_pr_is_older() {
    run("runtime_conclusion_head");
}

fn run(case: &str) {
    let directory = tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    if case == "fetch_failure" {
        assert!(
            Command::new("git")
                .args(["remote", "add", "origin"])
                .arg(directory.path().join("missing.git"))
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
    }
    if matches!(case, "checkout_head_changed" | "runtime_conclusion_head") {
        for name in ["base", "validated", "replacement"] {
            fs::write(directory.path().join("file.rs"), name).unwrap();
            for arguments in [
                vec!["add", "file.rs"],
                vec![
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    name,
                ],
            ] {
                assert!(
                    Command::new("git")
                        .args(arguments)
                        .current_dir(directory.path())
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let head = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(directory.path())
                .output()
                .unwrap();
            assert!(head.status.success());
            fs::write(directory.path().join(format!("{name}-head")), &head.stdout).unwrap();
        }
        assert!(
            Command::new("git")
                .args(["branch", "replacement"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let validated =
            fs::read_to_string(directory.path().join(if case == "checkout_head_changed" {
                "validated-head"
            } else {
                "replacement-head"
            }))
            .unwrap();
        assert!(
            Command::new("git")
                .args(["checkout", "--quiet", "--detach", validated.trim()])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(
        directory.path().join("review.py"),
        r#"import json, os, pathlib, sys
root = pathlib.Path(os.environ["PROVIDER_ROOT"])
body = json.load(sys.stdin)
counter = root / "review-counter"
review_id = int(counter.read_text()) if counter.exists() else 100
counter.write_text(str(review_id + 1))
pending = dict(id=review_id, node_id="R" + str(review_id), commit_id=body["commit_id"],
    state="PENDING", user=dict(id=7, login="reviewer"))
(root / "pending-review.json").write_text(json.dumps(pending))
print(json.dumps(pending))
"#,
    )
    .unwrap();
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
 *reviews?per_page=100)
   python3 "$PROVIDER_BATCH_FIXTURE" reviews ;;
 */reviews/*/events)
   if [ "$PROVIDER_CASE" = concurrent ] && [ ! -f "$PROVIDER_ROOT/concurrent" ]; then
     touch "$PROVIDER_ROOT/concurrent"
     "$PROVIDER_TEST_BIN" --exact provider_concurrent_child --nocapture >"$PROVIDER_ROOT/concurrent-log" 2>&1 || { cat "$PROVIDER_ROOT/concurrent-log" >&2; exit 1; }
   fi
   if [ ! -f "$PROVIDER_ROOT/submit" ]; then
     touch "$PROVIDER_ROOT/submit"
     case "$PROVIDER_CASE" in
       retry|drop|relocate|relocate_head) printf 'HTTP 422 Review summary rejected' >&2; exit 1 ;;
       uncertain_submit) rm -f "$PROVIDER_ROOT/pending-review.json"; printf 'connection lost after submitting' >&2; exit 1 ;;
     esac
   fi
   id=${endpoint%/events}; id=${id##*/}
   rm -f "$PROVIDER_ROOT/pending-review.json"
   printf '{"id":%s,"node_id":"R%s","state":"COMMENTED","body":"Summary"}' "$id" "$id" ;;
 */reviews/*) id=${endpoint##*/}; printf '{"id":%s,"node_id":"R%s","state":"COMMENTED","body":"Summary"}' "$id" "$id" ;;
 */reviews) printf '%s' "$body" | python3 "$PROVIDER_ROOT/review.py" create ;;
 */pulls/comments/400) printf '{"id":400,"body":"Root","in_reply_to_id":null}' ;;
 */pulls/comments/*)
   if [ "$method" = PATCH ]; then printf 'HTTP 404 private review comments are unavailable through REST update' >&2; exit 1; fi
   if [ "$method" != DELETE ]; then printf '{"id":201,"body":"Root edited"}'; fi ;;
 graphql)
   case "$body" in
    *PublicationBatch*) printf '%s' "$body" | python3 "$PROVIDER_BATCH_FIXTURE" batch ;;
    *ReviewSnapshot*) printf '%s' "$body" | python3 "$PROVIDER_BATCH_FIXTURE" snapshot ;;
   esac ;;
 */pulls/1)
   head=old-head
   case "$PROVIDER_CASE" in
     remote_head_changed) head=new-head ;;
     checkout_head_changed)
       head=$(cat "$PROVIDER_ROOT/validated-head")
       git -C "$PROVIDER_ROOT" checkout --quiet --detach replacement || exit 1 ;;
     runtime_conclusion_head) head=$(cat "$PROVIDER_ROOT/replacement-head") ;;
     mixed) head=other-head ;;
     uncertain_stale_viewed) head=$(cat "$PROVIDER_ROOT/remote-head") ;;
   esac
   count=$(python3 -c 'import json,os,pathlib; p=pathlib.Path(os.environ["PROVIDER_ROOT"])/"snapshot-seed.json"; print(len(json.loads(p.read_text())["read"]["files"]) if p.exists() else 1)')
   printf '{"id":1,"node_id":"PR1","title":"PR","body":"","user":null,"state":"open","merged":false,"changed_files":%s,"base":{"sha":"old-base"},"head":{"sha":"%s"}}' "$count" "$head" ;;
 */pulls/1/files*) printf '%s' "$body" | python3 "$PROVIDER_BATCH_FIXTURE" files ;;
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
            "PROVIDER_BATCH_FIXTURE",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/publication_batch.py"
            ),
        )
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
    let progress = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["event"] == "progress")
        .collect::<Vec<_>>();
    let counts = |stage| {
        let mut counts = progress
            .iter()
            .filter(|event| event["stage"] == stage)
            .map(|event| {
                (
                    event["completed"].as_u64().unwrap(),
                    event["total"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        counts.dedup();
        counts
    };
    if case == "bulk_large" {
        assert_eq!(
            counts("publish_comments"),
            [(0, 18), (8, 18), (16, 18), (18, 18)]
        );
        assert_eq!(counts("publish_review"), [(0, 1), (1, 1)]);
        assert_eq!(progress.last().unwrap()["stage"], "publish_complete");
    } else if matches!(case, "comment_resource" | "comment_resource_legacy") {
        assert_eq!(counts("publish_comments"), [(0, 8), (6, 8), (7, 8), (8, 8)]);
        assert_eq!(counts("publish_files"), [(0, 1), (1, 1)]);
        assert_eq!(progress.last().unwrap()["stage"], "publish_complete");
    } else if case.starts_with("comment_resource") {
        assert_eq!(counts("publish_comments"), [(0, 8), (6, 8)]);
        assert!(
            !progress
                .iter()
                .any(|event| event["stage"] == "publish_complete")
        );
        assert_eq!(progress.last().unwrap()["stage"], "publish_failed");
    } else if matches!(case, "comment_unknown" | "comment_internal") {
        assert_eq!(counts("publish_comments"), [(0, 3), (2, 3)]);
        assert!(
            !progress
                .iter()
                .any(|event| event["stage"] == "publish_complete")
        );
    } else if case == "comment_partial" {
        assert_eq!(counts("publish_comments"), [(0, 3), (2, 3), (3, 3)]);
    } else if case == "uncertain_stale_viewed" {
        assert_eq!(counts("publish_files"), [(1, 2), (2, 2)]);
    } else if case == "uncertain_submit" {
        assert_eq!(counts("publish_review"), [(0, 1), (1, 1)]);
    }
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

// Reproduce a durable preparation written by the previous publisher. These
// receipt-discovery tests must keep exercising old saved publications after the
// new publisher stops creating comment arrays through the REST review endpoint.
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
    if matches!(
        case.as_str(),
        "viewed_cache" | "viewed_cache_failure" | "families"
    ) {
        read.files = (0..20)
            .map(|index| ChangedFile {
                path: format!("file-{index:02}.rs"),
                ..read.files[0].clone()
            })
            .collect();
    }
    if matches!(
        case.as_str(),
        "checkout_head_changed" | "runtime_conclusion_head"
    ) {
        read.pull_request.revision.base =
            fs::read_to_string(std::path::Path::new(&root).join("base-head"))
                .unwrap()
                .trim()
                .into();
        read.pull_request.revision.head =
            fs::read_to_string(std::path::Path::new(&root).join("validated-head"))
                .unwrap()
                .trim()
                .into();
        store.enable_worktree_context();
    }
    if case == "uncertain_stale_viewed" {
        read.files
            .extend(["keep.rs", "done.rs"].map(|path| ChangedFile {
                path: path.into(),
                ..read.files[0].clone()
            }));
    }
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
    fs::write(
        std::path::Path::new(&root).join("snapshot-seed.json"),
        serde_json::to_vec(&serde_json::json!({"read":read,"mappings":backend.mappings})).unwrap(),
    )
    .unwrap();
    store
        .import_review(&ReviewId("snapshot".into()), &reviewer, &read, &backend)
        .unwrap();
    let mut workspace = store.open_workspace(&ReviewId("snapshot".into())).unwrap();
    if case == "pending_discarded" {
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(location("old-head")),
                    body: "Old draft".into(),
                    destination: None,
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        let first = serde_json::to_value(&workspace.results[0]).unwrap();
        fs::remove_file(std::path::Path::new(&root).join("pending-review.json")).unwrap();
        drop(store);
        let mut store = Store::open(std::path::Path::new(&root)).unwrap();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(location("old-head")),
                    body: "New draft".into(),
                    destination: None,
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert_eq!(workspace.results.len(), 2);
        assert_eq!(serde_json::to_value(&workspace.results[0]).unwrap(), first);
        assert!(workspace.pending().unwrap().operations.is_empty());
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let creates = requests
            .iter()
            .filter(|request| {
                request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
                    && request["method"] == "POST"
            })
            .count();
        assert_eq!(creates, 2);
        let batches = requests
            .iter()
            .filter(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("PublicationBatch"))
            })
            .collect::<Vec<_>>();
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0]["body"]["variables"]["input0"]["body"],
            "Old draft"
        );
        assert_eq!(
            batches[1]["body"]["variables"]["input0"]["body"],
            "New draft"
        );
        assert_eq!(
            batches[1]["body"]["variables"]["input0"]["pullRequestReviewId"],
            "R101"
        );
        return;
    }
    if matches!(
        case.as_str(),
        "families"
            | "comment_partial"
            | "comment_unknown"
            | "comment_internal"
            | "pending_reuse"
            | "comment_resource"
            | "comment_resource_legacy"
            | "comment_resource_missing"
            | "comment_resource_ambiguous"
            | "comment_resource_author"
            | "comment_resource_node"
            | "comment_resource_database"
            | "comment_resource_submitted"
            | "comment_resource_wide"
    ) {
        let count = if case == "families" {
            20
        } else if case.starts_with("comment_resource") {
            8
        } else if case == "pending_reuse" {
            1
        } else {
            3
        };
        let mut roots = Vec::new();
        let mut replies = Vec::new();
        for index in 0..count {
            let mut target = if case == "comment_resource_wide" && index == 7 {
                CommentTarget::PullRequest
            } else {
                location("old-head")
            };
            if let CommentTarget::Diff(location) = &mut target {
                location.position.path = workspace.review.read.files
                    [index % workspace.review.read.files.len()]
                .path
                .clone();
            }
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Save {
                        comment: None,
                        target: Some(target),
                        body: format!("Root {index}"),
                        destination: None,
                    },
                )
                .unwrap();
            roots.push(workspace.saved_comment.clone().unwrap());
            if case == "families" {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Save {
                            comment: None,
                            target: Some(CommentTarget::Reply(roots[index].clone())),
                            body: format!("Reply {index}"),
                            destination: None,
                        },
                    )
                    .unwrap();
                replies.push(workspace.saved_comment.clone().unwrap());
            }
        }
        let paths = workspace
            .review
            .read
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        if case != "pending_reuse" {
            for path in paths {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Viewed { path, viewed: true },
                    )
                    .unwrap();
            }
        }
        if case == "families" {
            store
                .append_event(
                    &workspace.batch.id,
                    &Event::CreateComment {
                        comment: DraftComment {
                            content: CommentContent {
                                id: CommentId("nested-reply".into()),
                                body: "Nested reply".into(),
                            },
                            author: reviewer.clone(),
                            target: CommentTarget::Reply(replies[0].clone()),
                        },
                    },
                )
                .unwrap();
            for comment in roots {
                store
                    .append_event(
                        &workspace.batch.id,
                        &Event::SetCommentReaction {
                            comment,
                            reaction: "👍".into(),
                            present: true,
                        },
                    )
                    .unwrap();
            }
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::MoveToReview {
                        comments: vec![CommentId("nested-reply".into())],
                    },
                )
                .unwrap();
        }
        let raw = workspace.batch.write.clone();
        let planned = workspace.publication_plan().unwrap().operations;
        if case.starts_with("comment_resource") {
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            assert!(
                error.to_string().contains("RESOURCE_LIMITS_EXCEEDED"),
                "{error}"
            );
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert_eq!(workspace.results.len(), 6);
            assert!(workspace.suspended_attempts[0].uncertain);
            assert_eq!(
                workspace.suspended_attempts[0].provider["creating"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert!(
                !std::path::Path::new(&root)
                    .join("batch-viewed.json")
                    .exists()
            );
            if case == "comment_resource_legacy" {
                workspace.suspended_attempts[0]
                    .provider
                    .as_object_mut()
                    .unwrap()
                    .remove("failures");
                workspace = store
                    .save_workspace(workspace.generation, &workspace)
                    .unwrap();
            }
            if case == "comment_resource_missing" {
                let path = std::path::Path::new(&root).join("batch-comments.json");
                let mut remote: Vec<Value> =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                remote.retain(|comment| comment["body"] != "Root 6");
                fs::write(path, serde_json::to_vec(&remote).unwrap()).unwrap();
            }
            if case == "comment_resource_ambiguous" {
                let path = std::path::Path::new(&root).join("batch-comments.json");
                let mut remote: Vec<Value> =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                let mut duplicate = remote.last().unwrap().clone();
                duplicate["identity"] = serde_json::json!(208);
                remote.push(duplicate);
                fs::write(path, serde_json::to_vec(&remote).unwrap()).unwrap();
            }
            if case == "comment_resource_author" {
                let path = std::path::Path::new(&root).join("pending-review.json");
                let mut pending: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                pending["user"]["id"] = serde_json::json!(99);
                fs::write(path, serde_json::to_vec(&pending).unwrap()).unwrap();
            }
            let fresh = github::fetch(&url::Url::parse(&backend.source.locator).unwrap()).unwrap();
            let snapshot = ReviewId("fetched-partial-resource".into());
            store
                .import_review(&snapshot, &fresh.reviewer, &fresh.read, &fresh.backend)
                .unwrap();
            workspace = store
                .refresh_workspace(&workspace.id, workspace.generation, &snapshot)
                .unwrap();
            drop(store);
            store = Store::open(&root).unwrap();
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            let original_receipts = workspace.results.clone();
            let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            if matches!(
                case.as_str(),
                "comment_resource_missing"
                    | "comment_resource_ambiguous"
                    | "comment_resource_author"
                    | "comment_resource_node"
                    | "comment_resource_database"
                    | "comment_resource_submitted"
                    | "comment_resource_wide"
            ) {
                let error =
                    github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                        .unwrap_err();
                assert!(error.to_string().contains("uncertain"), "{error}");
                let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                assert_eq!(
                    before.matches("PublicationBatch").count(),
                    after.matches("PublicationBatch").count()
                );
                assert_eq!(
                    store.workspace(&workspace.id).unwrap().unwrap().results,
                    original_receipts
                );
                return;
            }
            workspace =
                github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
            assert!(workspace.pending().unwrap().operations.is_empty());
            assert_eq!(&workspace.results[..6], original_receipts.as_slice());
            assert_eq!(workspace.results.len(), planned.len());
            assert_eq!(workspace.results[6].operation, planned[6]);
            assert_eq!(workspace.results[7].operation, planned[7]);
            assert_eq!(
                workspace.results[6]
                    .mappings
                    .iter()
                    .find(|mapping| mapping.resource == "review_comment")
                    .unwrap()
                    .remote_id,
                "207"
            );
            let remote: Vec<Value> = serde_json::from_slice(
                &fs::read(std::path::Path::new(&root).join("batch-comments.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(remote.len(), 8);
            for index in 0..8 {
                assert_eq!(
                    remote
                        .iter()
                        .filter(|comment| comment["body"] == format!("Root {index}"))
                        .count(),
                    1
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
                    request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("AddPullRequestReviewThreadInput"))
                })
                .collect::<Vec<_>>();
            assert_eq!(creates.len(), 2);
            assert_eq!(
                creates[0]["body"]["variables"].as_object().unwrap().len(),
                8
            );
            assert_eq!(
                creates[1]["body"]["variables"].as_object().unwrap().len(),
                1
            );
            assert_eq!(creates[1]["body"]["variables"]["input0"]["body"], "Root 7");
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
            return;
        }
        if matches!(
            case.as_str(),
            "comment_partial" | "comment_unknown" | "comment_internal"
        ) {
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert_eq!(workspace.results.len(), 2);
            assert_eq!(workspace.results[0].operation, planned[0]);
            assert_eq!(workspace.results[1].operation, planned[2]);
            assert_eq!(
                workspace.suspended_attempts[0].uncertain,
                case != "comment_partial"
            );
            if case != "comment_partial" {
                assert!(
                    error.to_string().contains(if case == "comment_internal" {
                        "INTERNAL"
                    } else {
                        "identity missing"
                    }),
                    "{error}"
                );
                let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                let error =
                    github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                        .unwrap_err();
                assert!(error.to_string().contains("uncertain"));
                let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
                assert_eq!(
                    before.matches("PublicationBatch").count(),
                    after.matches("PublicationBatch").count()
                );
                assert_eq!(workspace.batch.write, raw);
                return;
            }
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(workspace.batch.write, raw);
        assert_eq!(workspace.results.len(), planned.len());
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let batches = requests
            .iter()
            .filter(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("PublicationBatch"))
            })
            .collect::<Vec<_>>();
        let sizes = batches
            .iter()
            .map(|request| request["body"]["variables"].as_object().unwrap().len())
            .collect::<Vec<_>>();
        assert_eq!(
            sizes,
            if case == "families" {
                vec![8, 8, 4, 8, 8, 4, 1, 8, 8, 4, 8, 8, 4]
            } else if case == "comment_partial" {
                vec![3, 1, 1]
            } else {
                vec![1]
            }
        );
        if case == "comment_partial" {
            assert_eq!(batches[1]["body"]["variables"]["input0"]["body"], "Root 1");
            assert_eq!(workspace.results[2].operation, planned[1]);
        }
        if case == "pending_reuse" {
            assert_eq!(
                batches[0]["body"]["variables"]["input0"]["pullRequestReviewId"],
                "R900"
            );
            assert!(!requests.iter().any(|request| request["endpoint"]
                == "repos/owner/repo/pulls/1/reviews"
                && request["method"] == "POST"));
        }
        assert!(!requests.iter().any(|request| request["method"] == "POST"
            && request["endpoint"].as_str().is_some_and(
                |endpoint| endpoint.ends_with("/comments") || endpoint.ends_with("/replies")
            )));
        return;
    }
    if case == "runtime_conclusion_head" {
        let expected =
            fs::read_to_string(std::path::Path::new(&root).join("replacement-head")).unwrap();
        assert_eq!(workspace.comparison.head, expected.trim());
        assert_ne!(
            workspace.comparison.head,
            workspace.review.read.pull_request.revision.head
        );
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::PublishReview {
                    conclusion: Some(ReviewConclusion {
                        outcome: ReviewOutcome::Comment,
                        body: Some("Summary".into()),
                    }),
                },
            )
            .unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert_eq!(workspace.results.len(), 1);
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let created = requests
            .iter()
            .find(|request| {
                request["endpoint"] == "repos/owner/repo/pulls/1/reviews"
                    && request["method"] == "POST"
            })
            .unwrap();
        assert_eq!(created["body"]["commit_id"], expected.trim());
        assert!(created["body"].get("event").is_none());
        assert!(requests.iter().any(|request| request["endpoint"]
            == "repos/owner/repo/pulls/1/reviews/100/events"
            && request["body"]["event"] == "COMMENT"));
        return;
    }
    if case == "uncertain_stale_viewed" {
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["config", "user.name", "Reviewer"]);
        git(&["config", "user.email", "reviewer@example.invalid"]);
        for path in ["file.rs", "keep.rs", "done.rs"] {
            fs::write(std::path::Path::new(&root).join(path), "original\n").unwrap();
        }
        git(&["add", "file.rs", "keep.rs", "done.rs"]);
        git(&["commit", "--quiet", "-m", "Original"]);
        git(&["update-ref", "refs/tags/old-head", "HEAD"]);
        git(&["update-ref", "refs/tags/old-base", "HEAD"]);
        for path in ["done.rs", "file.rs", "keep.rs"] {
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Viewed {
                        path: path.into(),
                        viewed: true,
                    },
                )
                .unwrap();
        }
        let frozen = workspace.publication_plan().unwrap().operations;
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::BeginPublication {
                    destination: buf_review::workspace::Destination::Review,
                    operations: frozen.clone(),
                },
            )
            .unwrap();
        let publication = workspace.attempt.as_ref().unwrap().id.clone();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::RecordResult {
                    result: buf_review::store::OperationResult {
                        operation: frozen[0].clone(),
                        mappings: vec![],
                        response: serde_json::json!({"confirmed":true}),
                    },
                },
            )
            .unwrap();
        workspace = store.mutate_workspace(&workspace.id, workspace.generation, Action::SetAttemptProvider {
            provider: serde_json::json!({"inflight":"operation batch", "creating":[frozen[1].clone()]}),
        }).unwrap();
        workspace = store
            .mutate_workspace(
                &workspace.id,
                workspace.generation,
                Action::FinishPublication {
                    success: false,
                    uncertain: true,
                    error: Some("lost viewed response".into()),
                },
            )
            .unwrap();
        fs::write(
            std::path::Path::new(&root).join("file.rs"),
            "newer version\n",
        )
        .unwrap();
        git(&["commit", "--quiet", "-am", "Change one reviewed file"]);
        read.pull_request.revision.head = git(&["rev-parse", "HEAD"]);
        fs::write(
            std::path::Path::new(&root).join("remote-head"),
            &read.pull_request.revision.head,
        )
        .unwrap();
        let latest = ReviewId("latest".into());
        store
            .import_review(&latest, &reviewer, &read, &backend)
            .unwrap();
        workspace = store
            .refresh_workspace(&workspace.id, workspace.generation, &latest)
            .unwrap();
        workspace = store
            .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
            .unwrap();
        let history = workspace.batch.write.events.clone();
        assert_eq!(
            workspace
                .get_unpublished()
                .unwrap()
                .1
                .operations
                .into_iter()
                .map(|operation| operation.change)
                .collect::<Vec<_>>(),
            vec![frozen[2].change.clone()]
        );
        assert_eq!(workspace.suspended_attempts[0].id, publication);
        assert_eq!(workspace.suspended_attempts[0].operations, frozen);
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert!(workspace.suspended_attempts.is_empty() && workspace.attempt.is_none());
        assert_eq!(workspace.batch.write.events, history);
        assert_eq!(
            workspace
                .results
                .iter()
                .map(|result| &result.operation)
                .collect::<Vec<_>>(),
            vec![&frozen[0], &frozen[2]]
        );
        assert!(!workspace.has_result(&frozen[1].id));
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let snapshot = requests
            .iter()
            .position(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("ReviewSnapshot"))
            })
            .unwrap();
        let writes = requests
            .iter()
            .enumerate()
            .filter(|(_, request)| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("MarkFileAsViewedInput"))
            })
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 1);
        assert!(snapshot < writes[0].0);
        assert_eq!(
            writes[0].1["body"]["variables"]["input0"]["path"],
            "keep.rs"
        );
        return;
    }
    if matches!(
        case.as_str(),
        "remote_head_changed" | "fetch_failure" | "checkout_head_changed"
    ) {
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
        let before = serde_json::to_value(&workspace).unwrap();
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        if case == "remote_head_changed" {
            assert_eq!(
                error.to_string(),
                "cannot publish to GitHub: not at remote head"
            );
        } else if case == "fetch_failure" {
            assert!(error.to_string().contains("git:"), "{error}");
        } else {
            assert!(
                error
                    .to_string()
                    .contains("Git HEAD changed while preparing publication"),
                "{error}"
            );
        }
        assert_eq!(
            serde_json::to_value(
                Store::open(&root)
                    .unwrap()
                    .workspace(&workspace.id)
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            before
        );
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap_or_default();
        assert!(!log.contains("\"method\":\"POST\""));
        assert!(!log.contains("\"method\":\"PATCH\""));
        assert!(!log.contains("\"method\":\"DELETE\""));
        if case == "fetch_failure" {
            assert!(log.is_empty(), "{log}");
        }
        return;
    }
    if matches!(case.as_str(), "viewed_cache" | "viewed_cache_failure") {
        let paths = workspace
            .review
            .read
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        for path in &paths {
            workspace = store
                .mutate_workspace(
                    &workspace.id,
                    workspace.generation,
                    Action::Viewed {
                        path: path.clone(),
                        viewed: true,
                    },
                )
                .unwrap();
        }
        let operations = workspace.publication_plan().unwrap().operations;
        let events = workspace.batch.write.events.clone();
        let mut expected = operations.clone();
        if case == "viewed_cache_failure" {
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            assert!(
                error.to_string().contains("viewed state rejected"),
                "{error}"
            );
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            let confirmed = operations
                .iter()
                .take(8)
                .enumerate()
                .filter(|(index, _)| *index != 1)
                .map(|(_, operation)| operation.clone())
                .collect::<Vec<_>>();
            assert_eq!(workspace.results.len(), 7);
            assert_eq!(
                workspace
                    .results
                    .iter()
                    .map(|result| result.operation.clone())
                    .collect::<Vec<_>>(),
                confirmed
            );
            assert!(!workspace.suspended_attempts[0].uncertain);
            assert_eq!(workspace.batch.write.events, events);
            expected = confirmed;
            expected.push(operations[1].clone());
            expected.extend_from_slice(&operations[8..]);
            let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let batches = requests
                .iter()
                .filter(|request| {
                    request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("PublicationBatch"))
                })
                .collect::<Vec<_>>();
            assert_eq!(batches.len(), 1);
            assert_eq!(
                batches[0]["body"]["variables"].as_object().unwrap().len(),
                8
            );
            for (index, path) in paths.iter().take(8).enumerate() {
                let input = &batches[0]["body"]["variables"][format!("input{index}")];
                assert_eq!(input["path"], *path);
                assert_eq!(input["clientMutationId"], operations[index].id);
            }
            drop(store);
            store = Store::open(&root).unwrap();
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(
            workspace
                .results
                .iter()
                .map(|result| result.operation.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(workspace.batch.write.events, events);
        assert_eq!(
            workspace
                .results
                .iter()
                .map(|result| &result.operation)
                .collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            workspace.results[0].mappings,
            vec![IdMapping {
                resource: "pull_request_node".into(),
                core_id: "pull".into(),
                remote_id: "PR1".into(),
            }]
        );
        assert!(
            workspace.results[1..]
                .iter()
                .all(|result| result.mappings.is_empty())
        );
        let requests = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["endpoint"] == "repos/owner/repo/pulls/1"
                    && request["method"] == "GET")
                .count(),
            if case == "viewed_cache_failure" { 2 } else { 1 }
        );
        let writes = requests
            .iter()
            .filter(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("MarkFileAsViewedInput"))
            })
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 3);
        assert_eq!(
            writes
                .iter()
                .map(|request| request["body"]["variables"].as_object().unwrap().len())
                .collect::<Vec<_>>(),
            if case == "viewed_cache_failure" {
                vec![8, 8, 5]
            } else {
                vec![8, 8, 4]
            }
        );
        let inputs = writes
            .iter()
            .flat_map(|request| {
                let variables = &request["body"]["variables"];
                (0..variables.as_object().unwrap().len())
                    .map(move |index| &variables[format!("input{index}")])
            })
            .collect::<Vec<_>>();
        let mut sent = operations.clone();
        if case == "viewed_cache_failure" {
            sent.insert(8, operations[1].clone());
        }
        for (input, operation) in inputs.iter().zip(&sent) {
            let Event::SetFileViewed { path, .. } = &operation.change else {
                unreachable!()
            };
            assert_eq!(input["path"], *path);
            assert_eq!(input["pullRequestId"], "PR1");
            assert_eq!(input["clientMutationId"], operation.id);
        }
        assert_eq!(inputs.len(), sent.len());
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
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            assert!(error.to_string().contains("thread lookup unavailable"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.results.is_empty());
            assert_eq!(workspace.suspended_attempts.len(), 1);
            assert!(!workspace.suspended_attempts[0].uncertain);
            let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert!(!log.contains("AddPullRequestReviewThreadReplyInput"));
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(workspace.results.len(), 2);
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
        if case == "bulk_large" {
            for index in 2..18 {
                workspace = store
                    .mutate_workspace(
                        &workspace.id,
                        workspace.generation,
                        Action::Save {
                            comment: None,
                            target: Some(location("old-head")),
                            body: format!("Root {index}"),
                            destination: None,
                        },
                    )
                    .unwrap();
                ids.push(workspace.saved_comment.clone().unwrap());
            }
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
        if matches!(case.as_str(), "bulk_conclude" | "bulk_large") {
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
        if case == "bulk_boundary_failure" {
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            assert!(error.to_string().contains("viewed state rejected"));
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert_eq!(workspace.results.len(), 4);
        }
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(
            workspace.results.len(),
            ids.len()
                + usize::from(
                    matches!(case.as_str(), "bulk_conclude" | "bulk_large")
                        || case.starts_with("bulk_boundary")
                )
        );
        for (index, id) in ids.iter().enumerate() {
            assert!(
                workspace
                    .results
                    .iter()
                    .any(|result| result
                        .mappings
                        .iter()
                        .any(|mapping| mapping.resource == "review_comment"
                            && mapping.core_id == id.0
                            && mapping.remote_id == (201 + index).to_string()))
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
        assert_eq!(
            creates[0]["body"],
            serde_json::json!({"commit_id":"old-head"})
        );
        let batches = requests
            .iter()
            .filter(|request| {
                request["body"]["query"]
                    .as_str()
                    .is_some_and(|query| query.contains("AddPullRequestReviewThreadInput"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            batches
                .iter()
                .map(|request| request["body"]["variables"].as_object().unwrap().len())
                .collect::<Vec<_>>(),
            if case == "bulk_large" {
                vec![8, 8, 2]
            } else {
                vec![ids.len()]
            }
        );
        let sent = batches
            .iter()
            .flat_map(|request| {
                let variables = &request["body"]["variables"];
                (0..variables.as_object().unwrap().len())
                    .map(move |index| &variables[format!("input{index}")])
            })
            .collect::<Vec<_>>();
        for (input, result) in sent.iter().zip(&workspace.results) {
            assert_eq!(input["clientMutationId"], result.operation.id);
        }
        assert_eq!(sent.len(), ids.len());
        assert_eq!(
            batches[0]["body"]["variables"]["input0"]["body"],
            "First bulk root"
        );
        assert_eq!(batches[0]["body"]["variables"]["input0"]["line"], 4);
        assert_eq!(batches[0]["body"]["variables"]["input0"]["startLine"], 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["endpoint"]
                    .as_str()
                    .is_some_and(|endpoint| endpoint.ends_with("/events")))
                .count(),
            usize::from(matches!(case.as_str(), "bulk_conclude" | "bulk_large"))
        );
        if case.starts_with("bulk_external") {
            let mut fresh = workspace.review.read.clone();
            fresh.comments = workspace
                .results
                .iter()
                .map(|result| &result.operation)
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
            fs::remove_file(std::path::Path::new(&root).join("pending-review.json")).unwrap();
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
                github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
    if matches!(case.as_str(), "schema_rejection" | "anchor_rejection") {
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
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        assert!(error.to_string().contains("HTTP 422"));
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        assert!(workspace.attempt.is_none());
        assert_eq!(workspace.suspended_attempts.len(), 1);
        assert!(!workspace.suspended_attempts[0].uncertain);
        assert_eq!(workspace.pending().unwrap().operations.len(), 1);
        if case == "anchor_rejection" {
            assert!(workspace.rejected_anchors.contains_key(&draft));
            let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            let error = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                .unwrap_err();
            assert!(error.to_string().contains("resolution"));
            let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
            assert_eq!(
                after
                    .lines()
                    .filter(|line| line.contains("\"method\":\"POST\""))
                    .count(),
                before
                    .lines()
                    .filter(|line| line.contains("\"method\":\"POST\""))
                    .count()
            );
            assert!(workspace.results.is_empty());
            return;
        }
        assert!(workspace.rejected_anchors.is_empty());
        assert!(workspace.last_error.as_deref().unwrap().contains("oneOf"));
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(!workspace.rejected_anchors.contains_key(&draft));
        assert_eq!(workspace.results.len(), 1);
        let Event::CreateComment { comment } = &workspace.results[0].operation.change else {
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
            .filter(|request: &Value| {
                request["body"]["query"].as_str().is_some_and(|query| {
                    query.contains("PublicationBatch")
                        && query.contains("addPullRequestReviewThread")
                })
            })
            .collect();
        assert_eq!(
            requests.len(),
            if case == "schema_rejection" { 2 } else { 1 }
        );
        assert!(requests.iter().all(|request| {
            request["body"]["variables"]["input0"]["body"] == "This is a test"
                && request["body"]["variables"]["input0"]
                    .get("subject_type")
                    .is_none()
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        assert!(error.to_string().contains("lost reaction response"));
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        workspace =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
                .filter(
                    |request| request["body"]["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("PublicationBatch")
                            && query.contains("addReaction"))
                )
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
        assert!(requests.iter().any(
            |request| request["body"]["variables"]["input0"]["subjectId"] == "R100"
                && request["body"]["variables"]["input0"]["content"] == "THUMBS_UP"
        ));
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        if case == "account" {
            assert!(failure.to_string().contains("differs"));
        } else {
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
            assert!(workspace.suspended_attempts[0].uncertain);
            let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap())
                .unwrap()
                .matches("\"method\":\"POST\"")
                .count();
            let second = github::publish_workspace(&mut store, &workspace.id, workspace.generation)
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
                github::publish_workspace(&mut store, &workspace.id, workspace.generation)
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
        assert!(workspace.view().unwrap().todo.is_empty());
        let log = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        let thread = workspace
            .results
            .iter()
            .flat_map(|result| &result.mappings)
            .find(|mapping| mapping.resource == "review_thread")
            .unwrap();
        assert!(log.contains(&thread.remote_id));
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
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
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
        github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        assert_eq!(workspace.suspended_attempts.len(), 1);
        let before = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        let error =
            github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
        assert!(error.to_string().contains("uncertain"));
        let after = fs::read_to_string(std::env::var("PROVIDER_LOG").unwrap()).unwrap();
        assert_eq!(
            before.matches("\"method\":\"POST\"").count(),
            after.matches("\"method\":\"POST\"").count()
        );
        assert!(before.contains("addComment"));
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
        github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap_err();
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
                workspace =
                    github::publish_workspace(&mut store, &workspace.id, workspace.generation)
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
                workspace =
                    github::publish_workspace(&mut store, &workspace.id, workspace.generation)
                        .unwrap();
                workspace =
                    github::publish_workspace(&mut store, &workspace.id, workspace.generation)
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
    workspace = github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
    let reply_input = reply["body"]["variables"]
        .as_object()
        .unwrap()
        .values()
        .find(|input| input["body"] == "Reply")
        .unwrap();
    assert_eq!(reply_input["pullRequestReviewId"], "R100");
    let root_thread = workspace
        .results
        .iter()
        .flat_map(|result| &result.mappings)
        .find(|mapping| mapping.resource == "review_thread" && mapping.core_id == parent.0)
        .unwrap();
    assert_eq!(
        reply_input["pullRequestReviewThreadId"],
        root_thread.remote_id
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains(":addComment(")))
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
    assert!(
        prepared["body"]["comments"].is_null(),
        "review creation must not publish a special first comment group"
    );
    let roots = requests
        .iter()
        .position(|request| {
            request["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("AddPullRequestReviewThreadInput"))
        })
        .unwrap();
    assert!(
        requests[roots]["body"]["query"]
            .as_str()
            .unwrap()
            .contains(":addComment("),
        "independent PR and inline roots were not sent together"
    );
    assert_eq!(
        requests[roots]["body"]["variables"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    let replies = requests
        .iter()
        .position(|request| {
            request["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("AddPullRequestReviewThreadReplyInput"))
        })
        .unwrap();
    let viewed = requests
        .iter()
        .position(|request| {
            request["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("MarkFileAsViewedInput"))
        })
        .unwrap();
    let concluded = requests
        .iter()
        .position(|request| request["endpoint"] == "repos/owner/repo/pulls/1/reviews/100/events")
        .unwrap();
    assert!(
        roots < replies && replies < viewed && viewed < concluded,
        "publication did not follow dependency phases"
    );
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request["method"].as_str(), Some("PATCH" | "DELETE")))
    );
    if case == "uncertain_submit" {
        assert_eq!(log.matches("/reviews/100/events").count(), 1);
    }
    workspace = github::publish_workspace(&mut store, &workspace.id, workspace.generation).unwrap();
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
