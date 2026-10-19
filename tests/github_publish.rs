use std::{
    fs,
    num::NonZeroU32,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};

use buf_review::{
    github,
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
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Test".into(),
            body: String::new(),
            author: Some(user()),
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "src/lib.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
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
case "$endpoint:$method:$body" in
  repos/owner/repo/pulls/1/comments:POST:*'"subject_type":"line"'*)
    printf 'HTTP 422 line comments must omit subject_type' >&2; exit 1 ;;
esac
if [ "$PUBLISH_FAIL" = review-validation ] && [ "$endpoint" = repos/owner/repo/pulls/1/reviews ]; then
  printf '{"message":"Validation Failed","errors":[{"resource":"PullRequestReview","code":"custom","field":"user_id","message":"Can not approve your own pull request"}]}'
  printf 'gh: Validation Failed (HTTP 422)' >&2
  exit 1
fi
if [ -n "$PUBLISH_READY" ] && [ "$endpoint" = repos/owner/repo/issues/1/comments ] && [ "$method" = POST ]; then
  : > "$PUBLISH_READY"
  while [ ! -f "$PUBLISH_GATE" ]; do sleep 0.01; done
fi
if [ -n "$PUBLISH_FAIL" ]; then
  case "$endpoint" in *"$PUBLISH_FAIL"*) printf 'forced gh failure (HTTP 422)' >&2; exit 1 ;; esac
fi
case "$endpoint" in
  user)
    if [ "$PUBLISH_FAIL" = wrong-user ]; then printf '{"id":99,"login":"other"}';
    else printf '{"id":7,"login":"reviewer"}'; fi ;;
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
      if [ "$root" = 0 ]; then root=null; fi
      case "$id" in
        10) content='issue body' ;;
        20) content='root body' ;;
        21) content='reply body' ;;
        *) content=$(cat "$PUBLISH_LOG.body.$id") ;;
      esac
      printf '{"id":%s,"body":%s,"user":{"id":7,"login":"reviewer"},"in_reply_to_id":%s}' "$id" "$(printf '%s' "$content" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" "${root:-null}"
      exit 0
    fi
    if [ "$method" = PATCH ]; then
      id=${endpoint##*/}
      printf '%s' "$body" | python3 -c 'import json,sys; print(json.load(sys.stdin)["body"],end="")' > "$PUBLISH_LOG.body.$id"
      printf '{"id":%s}' "$id"
      exit 0
    fi
    next=$(wc -l < "$PUBLISH_LOG")
    printf '%s' "$body" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("body",""),end="")' > "$PUBLISH_LOG.body.$((100 + next))"
    root=0
    case "$endpoint" in */replies) thread=${endpoint%/replies}; root=${thread##*/} ;; esac
    printf '%s %s\n' "$((100 + next))" "$root" >> "$PUBLISH_LOG.parents"
    printf '{"id":%s,"body":"remote result"}' "$((100 + next))" ;;
esac
"#).unwrap();
    fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    project
}

fn seed_events(project: &Path, events: &[Event]) {
    let mut store = Store::open(project).unwrap();
    for event in events {
        store.append_event(&BatchId("batch".into()), event).unwrap();
    }
}

fn command(project: &Path, fail: Option<&str>, executable: &Path) -> Command {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(project.to_path_buf()).chain(std::env::split_paths(&inherited));
    let mut command = Command::new(executable);
    command
        .current_dir(project)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("PUBLISH_LOG", project.join("requests.jsonl"));
    if let Some(fail) = fail {
        command.env("PUBLISH_FAIL", fail);
    } else {
        command.env_remove("PUBLISH_FAIL");
    }
    command
}

fn publish(project: &Path, fail: Option<&str>) -> Output {
    let workspace = Store::open(project)
        .unwrap()
        .workspace("batch")
        .unwrap()
        .unwrap();
    let mut child = command(project, fail, Path::new(env!("CARGO_BIN_EXE_pr")))
        .arg("workspace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    serde_json::to_writer(
        child.stdin.take().unwrap(),
        &serde_json::json!({
            "op":"publish","workspace":"batch","generation":workspace.generation
        }),
    )
    .unwrap();
    child.wait_with_output().unwrap()
}

fn publication_error(output: &Output) -> String {
    if !output.status.success() {
        return String::from_utf8_lossy(&output.stderr).into_owned();
    }
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("workspace response must be JSON");
    response["error"].as_str().unwrap_or_default().into()
}

fn succeeded(output: &Output) -> bool {
    output.status.success() && publication_error(output).is_empty()
}

// Run individual provider adapter contracts in an isolated process; never mutate
// the test runner's process-global GitHub environment.
fn adapter(project: &Path, mode: &str, fail: Option<&str>) -> Result<(), String> {
    let output = command(project, fail, &std::env::current_exe().unwrap())
        .args(["--exact", "provider_adapter_worker", "--ignored"])
        .env("ADAPTER_MODE", mode)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value =
        serde_json::from_slice(&fs::read(project.join("adapter-result.json")).unwrap()).unwrap();
    match result["error"].as_str() {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

#[test]
#[ignore]
fn provider_adapter_worker() {
    let project = std::env::current_dir().unwrap();
    let mut store = Store::open(&project).unwrap();
    let workspace = store.workspace("batch").unwrap().unwrap();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let plan = workspace.pending().unwrap();
    let result = github::preflight(&workspace.review, &backend, &plan).and_then(|()| {
        if std::env::var("ADAPTER_MODE").unwrap() == "publish" {
            assert_eq!(
                plan.operations.len(),
                1,
                "adapter tests publish one operation only"
            );
            github::publish(&workspace.review, &backend, &plan.operations[0]).map(|_| ())
        } else {
            Ok(())
        }
    });
    fs::write(
        project.join("adapter-result.json"),
        serde_json::to_vec(&serde_json::json!({
            "error":result.err().map(|error|error.to_string())
        }))
        .unwrap(),
    )
    .unwrap();
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
    seed_events(
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
            Event::DismissReview {
                review: ReviewResourceId("old-review".into()),
                message: Some("superseded".into()),
            },
        ],
    );
    let output = publish(project.path(), None);
    assert!(succeeded(&output), "{}", publication_error(&output));
    let calls = requests(project.path());
    let diff = calls
        .iter()
        .find(|call| call["body"]["body"] == "comment new-diff\nsecond line")
        .unwrap();
    assert_eq!(
        diff["body"],
        serde_json::json!({"body":"comment new-diff\nsecond line",
        "commit_id":"selected-head","path":"src/lib.rs",
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
        9
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
    let retry = publish(project.path(), None);
    assert!(succeeded(&retry), "{}", publication_error(&retry));
    assert_eq!(requests(project.path()).len(), count);
}

#[test]
fn inline_payloads_omit_subject_type_on_both_sides_and_file_payload_keeps_it() {
    let project = fixture();
    let cases = [
        ("head-single", DiffSide::Head, false),
        ("head-range", DiffSide::Head, true),
        ("base-single", DiffSide::Base, false),
        ("base-range", DiffSide::Base, true),
    ];
    let mut events = cases
        .iter()
        .map(|(id, side, multiline)| {
            let mut anchor = location(true);
            let range = anchor.position.range.as_mut().unwrap();
            range.start.side = *side;
            range.end.side = *side;
            if !multiline {
                range.end.line = range.start.line;
            }
            create(id, CommentTarget::Diff(anchor))
        })
        .collect::<Vec<_>>();
    events.push(create("file", CommentTarget::Diff(location(false))));
    seed_events(project.path(), &events);
    let output = publish(project.path(), None);
    assert!(succeeded(&output), "{}", publication_error(&output));
    let calls = requests(project.path());
    for (id, side, multiline) in cases {
        let body = format!("comment {id}\nsecond line");
        let call = calls
            .iter()
            .find(|call| call["body"]["body"] == body)
            .unwrap();
        let side = if side == DiffSide::Head {
            "RIGHT"
        } else {
            "LEFT"
        };
        let mut expected = serde_json::json!({
            "body":body,"commit_id":"selected-head","path":"src/lib.rs",
            "line":if multiline {7} else {3},"side":side
        });
        if multiline {
            expected["start_line"] = serde_json::json!(3);
            expected["start_side"] = serde_json::json!(side);
        }
        assert_eq!(call["body"], expected);
    }
    let file = calls
        .iter()
        .find(|call| call["body"]["body"] == "comment file\nsecond line")
        .unwrap();
    assert_eq!(
        file["body"],
        serde_json::json!({
            "body":"comment file\nsecond line","commit_id":"selected-head",
            "path":"src/lib.rs","subject_type":"file"
        })
    );
}

#[test]
fn validation_failure_keeps_provider_reason_and_structured_error_context() {
    let project = fixture();
    seed_events(
        project.path(),
        &[Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        }],
    );
    let error = adapter(project.path(), "publish", Some("review-validation")).unwrap_err();
    assert!(error.contains("HTTP 422"), "{error}");
    assert!(
        error.contains("Can not approve your own pull request"),
        "{error}"
    );
    assert!(
        error.contains("\"resource\":\"PullRequestReview\""),
        "{error}"
    );
    assert!(error.contains("\"field\":\"user_id\""), "{error}");
    assert!(
        Store::open(project.path())
            .unwrap()
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn removal_selects_only_authenticated_reviewers_reaction_and_handles_empty_delete_response() {
    let project = fixture();
    seed_events(
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
    let output = publish(project.path(), None);
    assert!(succeeded(&output), "{}", publication_error(&output));
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
    seed_events(
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
    let error = adapter(project.path(), "preflight", None).unwrap_err();
    assert!(error.contains("unsupported reaction"));
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
fn different_authenticated_user_blocks_each_write_kind_without_receipts() {
    for event in [
        create("draft", CommentTarget::PullRequest),
        Event::SetCommentReaction {
            comment: CommentId("root".into()),
            reaction: "👍".into(),
            present: true,
        },
        Event::SetFileViewed {
            path: "src/lib.rs".into(),
            viewed: true,
        },
        Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    ] {
        let project = fixture();
        seed_events(project.path(), &[event]);
        let error = adapter(project.path(), "preflight", Some("wrong-user")).unwrap_err();
        assert!(error.contains("authenticated user differs from the fetched reviewer"));
        assert_eq!(
            requests(project.path()),
            vec![serde_json::json!({"endpoint":"user","method":"GET","body":null})]
        );
        let mut store = Store::open(project.path()).unwrap();
        assert!(
            store
                .operation_results(&BatchId("batch".into()))
                .unwrap()
                .is_empty()
        );
        adapter(project.path(), "preflight", None).unwrap();
        assert!(
            store
                .operation_results(&BatchId("batch".into()))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn authenticated_user_lookup_failure_leaves_plan_unpublished() {
    let project = fixture();
    seed_events(
        project.path(),
        &[create("draft", CommentTarget::PullRequest)],
    );
    let output = publish(project.path(), Some("user"));
    assert!(!succeeded(&output));
    assert!(publication_error(&output).contains("forced gh failure"));
    assert_eq!(
        requests(project.path()),
        vec![serde_json::json!({"endpoint":"user","method":"GET","body":null})]
    );
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
    seed_events(
        project.path(),
        &[
            create("draft", CommentTarget::Diff(location(true))),
            Event::DismissReview {
                review: ReviewResourceId("old-review".into()),
                message: Some("superseded".into()),
            },
        ],
    );
    let failed = publish(project.path(), Some("reviews"));
    assert!(!succeeded(&failed));
    let mut store = Store::open(project.path()).unwrap();
    assert_eq!(
        store
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .len(),
        1
    );
    let draft_id = store
        .review_backend(&ReviewId("review".into()))
        .unwrap()
        .unwrap()
        .mappings
        .into_iter()
        .find(|mapping| mapping.core_id == "draft")
        .unwrap()
        .remote_id;
    seed_events(
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
    let retried = publish(project.path(), None);
    assert!(succeeded(&retried), "{}", publication_error(&retried));
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
        calls.iter().any(|call| call["endpoint"]
            == format!("repos/owner/repo/pulls/1/comments/{draft_id}/replies"))
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
        seed_events(
            project.path(),
            &[create("draft", CommentTarget::PullRequest), invalid],
        );
        let error = adapter(project.path(), "preflight", None).unwrap_err();
        assert!(error.contains("must be nonempty"));
        assert!(requests(project.path()).is_empty());
    }
}

#[test]
fn graphql_errors_or_null_payload_never_record_success() {
    for failure in ["graphql-response", "null-response"] {
        let project = fixture();
        seed_events(
            project.path(),
            &[
                create("draft", CommentTarget::PullRequest),
                Event::SetFileViewed {
                    path: "src/lib.rs".into(),
                    viewed: true,
                },
            ],
        );
        let output = publish(project.path(), Some(failure));
        assert!(!succeeded(&output));
        let mut store = Store::open(project.path()).unwrap();
        let results = store.operation_results(&BatchId("batch".into())).unwrap();
        assert!(results.iter().all(|result| !matches!(
            result.operation.change,
            buf_review::core::Change::SetFileViewed { .. }
        )));
        let project = fixture();
        seed_events(
            project.path(),
            &[Event::SetFileViewed {
                path: "src/lib.rs".into(),
                viewed: true,
            }],
        );
        let retry = publish(project.path(), None);
        assert!(succeeded(&retry), "{}", publication_error(&retry));
        seed_events(
            project.path(),
            &[Event::SetFileViewed {
                path: "src/lib.rs".into(),
                viewed: false,
            }],
        );
        let unviewed = publish(project.path(), None);
        assert!(succeeded(&unviewed), "{}", publication_error(&unviewed));
        assert!(requests(project.path()).iter().any(|call| {
            call["body"]["query"]
                .as_str()
                .is_some_and(|query| query.contains("unmarkFileAsViewed"))
        }));
    }
}

#[test]
fn nested_new_replies_can_target_a_draft_published_by_an_earlier_publication() {
    let project = fixture();
    seed_events(
        project.path(),
        &[create("draft", CommentTarget::Diff(location(true)))],
    );
    let first = publish(project.path(), None);
    assert!(succeeded(&first), "{}", publication_error(&first));
    let mut store = Store::open(project.path()).unwrap();
    let draft_id = store
        .review_backend(&ReviewId("review".into()))
        .unwrap()
        .unwrap()
        .mappings
        .into_iter()
        .find(|mapping| mapping.core_id == "draft")
        .unwrap()
        .remote_id;
    seed_events(
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
    let second = publish(project.path(), None);
    assert!(succeeded(&second), "{}", publication_error(&second));
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
                && call["endpoint"]
                    == format!("repos/owner/repo/pulls/1/comments/{draft_id}/replies"))
            .count(),
        2
    );
}

#[test]
fn individual_review_adapter_preserves_outcome_body_and_revision() {
    let project = fixture();
    seed_events(
        project.path(),
        &[Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::RequestChanges,
                body: Some("please fix".into()),
            },
        }],
    );
    adapter(project.path(), "publish", None).unwrap();
    let calls = requests(project.path());
    let review = calls
        .iter()
        .find(|call| call["body"]["event"] == "REQUEST_CHANGES")
        .unwrap();
    assert_eq!(
        review["body"],
        serde_json::json!({"event":"REQUEST_CHANGES","body":"please fix","commit_id":"head"})
    );
}

#[test]
fn concurrent_publication_rejects_second_writer_and_retry_uses_saved_receipt() {
    use std::{
        fs::{File, TryLockError},
        thread,
        time::{Duration, Instant},
    };

    let project = fixture();
    seed_events(
        project.path(),
        &[create("once", CommentTarget::PullRequest)],
    );
    let workspace = Store::open(project.path())
        .unwrap()
        .workspace("batch")
        .unwrap()
        .unwrap();
    let ready = project.path().join("ready");
    let gate = project.path().join("gate");
    let spawn = || {
        let mut child = command(project.path(), None, Path::new(env!("CARGO_BIN_EXE_pr")))
            .arg("workspace")
            .env("PUBLISH_READY", &ready)
            .env("PUBLISH_GATE", &gate)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        serde_json::to_writer(
            child.stdin.take().unwrap(),
            &serde_json::json!({
                "op":"publish","workspace":"batch","generation":workspace.generation
            }),
        )
        .unwrap();
        child
    };
    let mut first = spawn();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        if Instant::now() >= deadline || first.try_wait().unwrap().is_some() {
            fs::write(&gate, "").unwrap();
            first.kill().ok();
            panic!("first publication did not reach paused provider request");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let probe = File::open(project.path().join(".review/publish.lock")).unwrap();
    let lock = probe.try_lock();
    if !matches!(lock, Err(TryLockError::WouldBlock)) {
        fs::write(&gate, "").unwrap();
        first.kill().ok();
        panic!("publication must hold OS lock across provider request");
    }
    let mut second = spawn();
    let deadline = Instant::now() + Duration::from_secs(5);
    while second.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            fs::write(&gate, "").unwrap();
            first.kill().ok();
            second.kill().ok();
            panic!("concurrent publication must reject a locked writer");
        }
        thread::sleep(Duration::from_millis(5));
    }
    fs::write(&gate, "").unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(succeeded(&first), "{}", publication_error(&first));
    assert!(!succeeded(&second));
    assert!(
        publication_error(&second).contains("operation would block"),
        "{}",
        publication_error(&second)
    );
    let calls = requests(project.path());
    assert_eq!(
        calls.iter().filter(|call| call["method"] == "POST").count(),
        1
    );
    let retry = publish(project.path(), None);
    assert!(succeeded(&retry), "{}", publication_error(&retry));
    assert_eq!(requests(project.path()), calls);
    assert_eq!(
        Store::open(project.path())
            .unwrap()
            .operation_results(&BatchId("batch".into()))
            .unwrap()
            .len(),
        1
    );
}
