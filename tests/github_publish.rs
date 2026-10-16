use std::{
    fs,
    num::NonZeroU32,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

use buf_review::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store},
};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn user() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "reviewer".into(),
    }
}

fn location(range: bool) -> DiffLocation {
    DiffLocation {
        revision: DiffRevision {
            base: "selected-base".into(),
            head: "selected-head".into(),
        },
        position: CommentPosition {
            path: "src/lib.rs".into(),
            range: range.then(|| DiffRange {
                start: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(3).unwrap(),
                },
                end: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(7).unwrap(),
                },
            }),
        },
    }
}

fn create(id: &str, target: CommentTarget) -> Event {
    Event::CreateComment {
        comment: DraftComment {
            content: CommentContent {
                id: CommentId(id.into()),
                body: format!("comment {id}\nsecond line"),
            },
            author: user(),
            target,
        },
    }
}

fn fixture() -> TempDir {
    let project = tempdir().unwrap();
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Test".into(),
            body: String::new(),
            author: Some(user()),
            status: PullRequestStatus::Open,
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "src/lib.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            viewed: false,
        }],
        comments: vec![
            ReadComment {
                content: CommentContent {
                    id: CommentId("issue".into()),
                    body: "issue body".into(),
                },
                author: Some(user()),
                target: CommentTarget::PullRequest,
            },
            ReadComment {
                content: CommentContent {
                    id: CommentId("root".into()),
                    body: "root body".into(),
                },
                author: Some(user()),
                target: CommentTarget::Diff(ReadDiffLocation {
                    original: ReadCommentAnchor {
                        revision: ReadCommentRevision::Commit {
                            head: "head".into(),
                        },
                        position: location(true).position,
                    },
                    current: None,
                    subject: Some(CommentSubject::Line),
                    diff_hunk: None,
                    outdated: None,
                }),
            },
            ReadComment {
                content: CommentContent {
                    id: CommentId("reply".into()),
                    body: "reply body".into(),
                },
                author: Some(user()),
                target: CommentTarget::Reply(CommentId("root".into())),
            },
        ],
        reactions: vec![CommentReaction {
            comment: CommentId("issue".into()),
            author: Some(user().id),
            reaction: "👍".into(),
        }],
        reviews: vec![ReadReview {
            id: Some(ReviewResourceId("old-review".into())),
            author: Some(user()),
            state: ReadReviewState::Submitted(ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            }),
        }],
    };
    let mut store = Store::open(project.path()).unwrap();
    let mapping = |resource: &str, core_id: &str, remote_id: &str| IdMapping {
        resource: resource.into(),
        core_id: core_id.into(),
        remote_id: remote_id.into(),
    };
    store
        .import_review(
            &ReviewId("review".into()),
            &user(),
            &read,
            &ReviewBackend {
                source: ReviewSource {
                    backend: "github".into(),
                    locator: "https://github.com/owner/repo/pull/1".into(),
                },
                mappings: vec![
                    mapping("user", "reviewer", "7"),
                    mapping("pull_request", "pull", "1"),
                    mapping("issue_comment", "issue", "10"),
                    mapping("review_comment", "root", "20"),
                    mapping("review_comment", "reply", "21"),
                    mapping("review", "old-review", "30"),
                ],
            },
        )
        .unwrap();
    store
        .create_batch(&BatchId("batch".into()), &ReviewId("review".into()))
        .unwrap();
    let executable = project.path().join("gh");
    fs::write(&executable, r#"#!/bin/sh
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
printf '{"endpoint":"%s","method":"%s","body":%s}\n' "$endpoint" "$method" "$body" >> "$PUBLISH_LOG"
if [ -n "$PUBLISH_FAIL" ]; then
  case "$endpoint" in *"$PUBLISH_FAIL"*) printf 'forced gh failure' >&2; exit 1 ;; esac
fi
case "$endpoint" in
  user) printf '{"id":7,"login":"reviewer"}' ;;
  graphql)
    if [ "$PUBLISH_FAIL" = graphql-response ]; then printf '{"errors":[{"message":"denied"}],"data":null}'; exit 0; fi
    if [ "$PUBLISH_FAIL" = null-response ]; then printf '{"data":{"markFileAsViewed":null}}'; exit 0; fi
    case "$body" in
      *unmarkFileAsViewed*) printf '{"data":{"unmarkFileAsViewed":{"clientMutationId":"done"}}}' ;;
      *) printf '{"data":{"markFileAsViewed":{"clientMutationId":"done"}}}' ;;
    esac ;;
  *reactions?per_page=100) printf '[[{"id":40,"content":"+1","user":{"id":99,"login":"other"}},{"id":41,"content":"+1","user":{"id":7,"login":"reviewer"}}]]' ;;
  repos/owner/repo/pulls/1)
    printf '{"node_id":"PR_node"}' ;;
  *)
    if [ "$method" = DELETE ]; then exit 0; fi
    if [ "$method" = GET ]; then
      id=${endpoint##*/}
      root=$(awk -v id="$id" '$1 == id { print $2 }' "$PUBLISH_LOG.parents")
      if [ "$root" = 0 ]; then printf '{"id":%s,"in_reply_to_id":null}' "$id";
      else printf '{"id":%s,"in_reply_to_id":%s}' "$id" "$root"; fi
      exit 0
    fi
    next=$(wc -l < "$PUBLISH_LOG")
    root=0
    case "$endpoint" in */replies) thread=${endpoint%/replies}; root=${thread##*/} ;; esac
    printf '%s %s\n' "$((100 + next))" "$root" >> "$PUBLISH_LOG.parents"
    printf '{"id":%s,"body":"remote result"}' "$((100 + next))" ;;
esac
"#).unwrap();
    fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    project
}

fn stage(project: &Path, events: &[Event]) {
    let mut store = Store::open(project).unwrap();
    for event in events {
        store.append_event(&BatchId("batch".into()), event).unwrap();
    }
}

fn commit(project: &Path, fail: Option<&str>) -> Output {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![project.to_path_buf()];
    paths.extend(std::env::split_paths(&inherited));
    let mut command = Command::new(env!("CARGO_BIN_EXE_pr"));
    command
        .current_dir(project)
        .args(["commit", "batch"])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("PUBLISH_LOG", project.join("requests.jsonl"));
    if let Some(fail) = fail {
        command.env("PUBLISH_FAIL", fail);
    } else {
        command.env_remove("PUBLISH_FAIL");
    }
    command.output().unwrap()
}

fn requests(project: &Path) -> Vec<Value> {
    fs::read_to_string(project.join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn publishes_native_endpoints_exact_anchors_and_persisted_mappings() {
    let project = fixture();
    stage(
        project.path(),
        &[
            create("new-issue", CommentTarget::PullRequest),
            create("new-diff", CommentTarget::Diff(location(true))),
            create("file", CommentTarget::Diff(location(false))),
            create("new-reply", CommentTarget::Reply(CommentId("reply".into()))),
            create(
                "draft-reply",
                CommentTarget::Reply(CommentId("new-diff".into())),
            ),
            Event::EditComment {
                comment: CommentId("root".into()),
                body: "edited\nbody".into(),
            },
            Event::SetCommentReaction {
                comment: CommentId("new-issue".into()),
                reaction: "❤️".into(),
                present: true,
            },
            Event::SetFileViewed {
                path: "src/lib.rs".into(),
                viewed: true,
            },
            Event::SetReviewConclusion {
                conclusion: ReviewConclusion {
                    outcome: ReviewOutcome::RequestChanges,
                    body: Some("please fix".into()),
                },
            },
            Event::DismissReview {
                review: ReviewResourceId("old-review".into()),
                message: Some("superseded".into()),
            },
        ],
    );
    let output = commit(project.path(), None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = requests(project.path());
    let diff = calls
        .iter()
        .find(|call| call["body"]["body"] == "comment new-diff\nsecond line")
        .unwrap();
    assert_eq!(
        diff["body"],
        serde_json::json!({"body":"comment new-diff\nsecond line",
        "commit_id":"selected-head","path":"src/lib.rs","subject_type":"line",
        "line":7,"side":"RIGHT","start_line":3,"start_side":"RIGHT"})
    );
    let file = calls
        .iter()
        .find(|call| call["body"]["subject_type"] == "file")
        .unwrap();
    assert!(file["body"].get("line").is_none());
    let reply = calls
        .iter()
        .find(|call| call["body"]["body"] == "comment new-reply\nsecond line")
        .unwrap();
    assert_eq!(
        reply["endpoint"],
        "repos/owner/repo/pulls/1/comments/20/replies"
    );
    let graph = calls
        .iter()
        .find(|call| call["endpoint"] == "graphql")
        .unwrap();
    assert_eq!(
        graph["body"]["variables"]["input"]["pullRequestId"],
        "PR_node"
    );
    let review = calls
        .iter()
        .find(|call| call["body"]["event"] == "REQUEST_CHANGES")
        .unwrap();
    assert_eq!(review["body"]["commit_id"], "head");
    let dismissal = calls
        .iter()
        .find(|call| call["body"]["event"] == "DISMISS")
        .unwrap();
    assert_eq!(dismissal["body"]["message"], "superseded");
    let mut store = Store::open(project.path()).unwrap();
    assert_eq!(
        store
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .len(),
        10
    );
    let backend = store
        .review_backend(&ReviewId("review".into()))
        .unwrap()
        .unwrap();
    assert!(
        backend
            .mappings
            .iter()
            .any(|mapping| mapping.resource == "review_comment" && mapping.core_id == "new-diff")
    );
    let count = calls.len();
    let retry = commit(project.path(), None);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(requests(project.path()).len(), count);
}

#[test]
fn removal_selects_only_authenticated_reviewers_reaction_and_handles_empty_delete_response() {
    let project = fixture();
    stage(
        project.path(),
        &[
            Event::SetCommentReaction {
                comment: CommentId("issue".into()),
                reaction: "👍".into(),
                present: false,
            },
            Event::DeleteComment {
                comment: CommentId("reply".into()),
            },
        ],
    );
    let output = commit(project.path(), None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = requests(project.path());
    assert!(calls.iter().any(|call| call["method"] == "DELETE"
        && call["endpoint"] == "repos/owner/repo/issues/comments/10/reactions/41"));
    assert!(
        !calls
            .iter()
            .any(|call| call["endpoint"] == "repos/owner/repo/issues/comments/10/reactions/40")
    );
    assert!(calls.iter().any(|call| call["method"] == "DELETE"
        && call["endpoint"] == "repos/owner/repo/pulls/comments/21"));
}

#[test]
fn unsupported_late_operation_fails_preflight_before_any_provider_request() {
    let project = fixture();
    stage(
        project.path(),
        &[
            create("new-issue", CommentTarget::PullRequest),
            Event::SetCommentReaction {
                comment: CommentId("root".into()),
                reaction: "unsupported".into(),
                present: true,
            },
        ],
    );
    let output = commit(project.path(), None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported reaction"));
    assert!(requests(project.path()).is_empty());
    let mut store = Store::open(project.path()).unwrap();
    assert!(
        store
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn retry_uses_completed_create_mapping_after_late_failure_and_accepts_new_draft_reply() {
    let project = fixture();
    stage(
        project.path(),
        &[
            create("draft", CommentTarget::Diff(location(true))),
            Event::SetReviewConclusion {
                conclusion: ReviewConclusion {
                    outcome: ReviewOutcome::Approve,
                    body: None,
                },
            },
        ],
    );
    let failed = commit(project.path(), Some("reviews"));
    assert!(!failed.status.success());
    let mut store = Store::open(project.path()).unwrap();
    assert_eq!(
        store
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .len(),
        1
    );
    stage(
        project.path(),
        &[
            Event::EditComment {
                comment: CommentId("draft".into()),
                body: "changed after publish".into(),
            },
            create(
                "later-reply",
                CommentTarget::Reply(CommentId("draft".into())),
            ),
        ],
    );
    let retried = commit(project.path(), None);
    assert!(
        retried.status.success(),
        "{}",
        String::from_utf8_lossy(&retried.stderr)
    );
    let calls = requests(project.path());
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["method"] == "POST"
                && call["endpoint"] == "repos/owner/repo/pulls/1/comments")
            .count(),
        1
    );
    assert!(
        calls.iter().any(
            |call| call["method"] == "PATCH" && call["body"]["body"] == "changed after publish"
        )
    );
    assert!(
        calls
            .iter()
            .any(|call| call["endpoint"] == "repos/owner/repo/pulls/1/comments/101/replies")
    );
}

#[test]
fn github_requires_review_text_and_dismissal_reason_before_writes() {
    for invalid in [
        Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Comment,
                body: None,
            },
        },
        Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::RequestChanges,
                body: Some(" \n".into()),
            },
        },
        Event::DismissReview {
            review: ReviewResourceId("old-review".into()),
            message: None,
        },
    ] {
        let project = fixture();
        stage(
            project.path(),
            &[create("draft", CommentTarget::PullRequest), invalid],
        );
        let output = commit(project.path(), None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must be nonempty"));
        assert!(requests(project.path()).is_empty());
    }
}

#[test]
fn graphql_errors_or_null_payload_never_record_success() {
    for failure in ["graphql-response", "null-response"] {
        let project = fixture();
        stage(
            project.path(),
            &[
                create("draft", CommentTarget::PullRequest),
                Event::SetFileViewed {
                    path: "src/lib.rs".into(),
                    viewed: true,
                },
            ],
        );
        let output = commit(project.path(), Some(failure));
        assert!(!output.status.success());
        let mut store = Store::open(project.path()).unwrap();
        let results = store.operation_results(&BatchId("batch".into())).unwrap();
        assert!(results.iter().all(|result| !matches!(
            result.operation.change,
            buf_review::core::Change::SetFileViewed { .. }
        )));
        let retry = commit(project.path(), None);
        assert!(
            retry.status.success(),
            "{}",
            String::from_utf8_lossy(&retry.stderr)
        );
        stage(
            project.path(),
            &[Event::SetFileViewed {
                path: "src/lib.rs".into(),
                viewed: false,
            }],
        );
        let unviewed = commit(project.path(), None);
        assert!(
            unviewed.status.success(),
            "{}",
            String::from_utf8_lossy(&unviewed.stderr)
        );
        assert!(requests(project.path()).iter().any(|call| {
            call["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("unmarkFileAsViewed"))
        }));
    }
}

#[test]
fn nested_new_replies_can_target_a_draft_published_by_an_earlier_commit() {
    let project = fixture();
    stage(
        project.path(),
        &[create("draft", CommentTarget::Diff(location(true)))],
    );
    let first = commit(project.path(), None);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    stage(
        project.path(),
        &[
            create(
                "first-reply",
                CommentTarget::Reply(CommentId("draft".into())),
            ),
            create(
                "second-reply",
                CommentTarget::Reply(CommentId("first-reply".into())),
            ),
        ],
    );
    let second = commit(project.path(), None);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let calls = requests(project.path());
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["method"] == "POST"
                && call["endpoint"] == "repos/owner/repo/pulls/1/comments")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["method"] == "POST"
                && call["endpoint"] == "repos/owner/repo/pulls/1/comments/101/replies")
            .count(),
        2
    );
}
