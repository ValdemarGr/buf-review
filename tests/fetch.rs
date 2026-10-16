use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};

use buf_review::{
    model::*,
    store::{ReviewBackend, Store},
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use uuid::Uuid;

const URL: &str = "https://github.com/acme/repo/pull/7";

struct FakeGh {
    directory: TempDir,
}

impl FakeGh {
    fn new() -> Self {
        let this = Self {
            directory: tempdir().unwrap(),
        };
        fs::create_dir(this.directory.path().join("bin")).unwrap();
        fs::create_dir(this.directory.path().join("project")).unwrap();
        let executable = this.directory.path().join("bin/gh");
        fs::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_GH_REQUESTS"
endpoint=
paginate=false
slurp=false
for argument do
    case "$argument" in
        /user|user|graphql|repos/*) endpoint=$argument ;;
        --paginate) paginate=true ;;
        --slurp) slurp=true ;;
    esac
done
case "$endpoint" in
    /user|user) fixture=user ;;
    graphql) fixture=graphql ;;
    repos/acme/repo/pulls/7) fixture=pr ;;
    repos/acme/repo/pulls/7/files*) fixture=files ;;
    repos/acme/repo/issues/7/comments*) fixture=issue_comments ;;
    repos/acme/repo/pulls/7/comments*) fixture=review_comments ;;
    repos/acme/repo/pulls/7/reviews*) fixture=reviews ;;
    repos/acme/repo/issues/comments/101/reactions*) fixture=reactions101 ;;
    repos/acme/repo/pulls/comments/201/reactions*) fixture=reactions201 ;;
    repos/acme/repo/pulls/comments/202/reactions*) fixture=reactions202 ;;
    repos/acme/repo/pulls/comments/203/reactions*) fixture=reactions203 ;;
    *) printf 'Unexpected endpoint: %s\n' "$endpoint" >&2; exit 1 ;;
esac
case "$fixture" in
    user|pr) ;;
    *)
        if [ "$paginate" != true ] || [ "$slurp" != true ]; then
            printf 'Missing pagination flags for %s\n' "$fixture" >&2
            exit 1
        fi
        ;;
esac
if [ "${FAKE_GH_FAIL:-}" = "$fixture" ]; then
    printf 'Fixture request failed: %s\n' "$fixture" >&2
    exit 1
fi
cat "$FAKE_GH_FIXTURES/$fixture.json"
"#,
        )
        .unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let reviewer = json!({"id":1,"node_id":"U_1","login":"reviewer"});
        let author = json!({"id":2,"node_id":"U_2","login":"author"});
        this.write("user", reviewer.clone());
        this.write(
            "pr",
            json!({
                "id":70,"node_id":"PR_70","number":7,"title":"Review this",
            "body":"PR description","user":author,"state":"open","merged":false,"changed_files":2,
                "base":{"sha":"base-sha"},"head":{"sha":"head-sha"}
            }),
        );
        this.write(
            "files",
            json!([
                [{"filename":"src/new.rs","status":"modified"}],
                [{"filename":"src/old.rs","previous_filename":"src/original.rs","status":"renamed"}]
            ]),
        );
        this.write(
            "issue_comments",
            json!([
                [{"id":101,"node_id":"IC_101","body":"PR-wide comment","user":null}],[]
            ]),
        );
        this.write("review_comments", json!([
            [{"id":201,"node_id":"RC_201","body":"Current range","user":reviewer,
              "path":"src/new.rs","commit_id":"head-sha","original_commit_id":"old-head",
              "line":5,"start_line":3,"side":"RIGHT","start_side":"RIGHT",
              "original_line":4,"original_start_line":2,"original_side":"RIGHT",
              "original_start_side":"RIGHT","subject_type":"line","diff_hunk":"@@ -1 +1 @@"}],
            [{"id":202,"node_id":"RC_202","body":"Reply","user":author,"in_reply_to_id":201,
              "path":"src/new.rs","commit_id":"head-sha","original_commit_id":"old-head",
              "line":5,"start_line":3,"side":"RIGHT","start_side":"RIGHT",
              "original_line":4,"original_start_line":2,"subject_type":"line","diff_hunk":"@@ -1 +1 @@"},
             {"id":203,"node_id":"RC_203","body":"Outdated comment","user":null,
              "path":"src/old.rs","commit_id":"head-sha","original_commit_id":"older-head",
              "line":null,"start_line":null,"side":"LEFT","original_line":7,
              "original_start_line":null,"subject_type":"line","diff_hunk":"@@ -7 +7 @@"}]
        ]));
        this.write("reviews", json!([
            [{"id":301,"node_id":"R_301","state":"APPROVED","body":"Looks good","user":reviewer}],
            [{"id":302,"node_id":"R_302","state":"DISMISSED","body":"Old objections","user":null},
             {"id":303,"node_id":"R_303","state":"PENDING","body":"Unsubmitted","user":author}]
        ]));
        this.write(
            "reactions101",
            json!([[{"id":1001,"content":"+1","user":reviewer}],[]]),
        );
        this.write(
            "reactions201",
            json!([[{"id":1002,"content":"heart","user":null}]]),
        );
        this.write("reactions202", json!([[]]));
        this.write("reactions203", json!([[]]));
        this.write("graphql", json!([
            {"data":{"repository":{"pullRequest":{"baseRefOid":"base-sha","headRefOid":"head-sha",
             "files":{"totalCount":2,"nodes":[{"path":"src/new.rs","viewerViewedState":"VIEWED"}],
             "pageInfo":{"hasNextPage":true,"endCursor":"next"}}}}}},
            {"data":{"repository":{"pullRequest":{"baseRefOid":"base-sha","headRefOid":"head-sha",
             "files":{"totalCount":2,"nodes":[{"path":"src/old.rs","viewerViewedState":"UNVIEWED"}],
             "pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}
        ]));
        this
    }

    fn write(&self, fixture: &str, value: Value) {
        fs::write(
            self.directory.path().join(format!("{fixture}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    fn fetch(&self, url: &str, fail: Option<&str>) -> Output {
        let path = std::env::join_paths(std::iter::once(self.directory.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_pr"));
        command
            .args(["fetch", url])
            .current_dir(self.directory.path().join("project"))
            .env("PATH", path)
            .env("FAKE_GH_FIXTURES", self.directory.path())
            .env("FAKE_GH_REQUESTS", self.directory.path().join("requests"))
            .env_remove("FAKE_GH_FAIL");
        if let Some(fail) = fail {
            command.env("FAKE_GH_FAIL", fail);
        }
        command.output().unwrap()
    }

    fn store(&self) -> Store {
        Store::open(self.directory.path().join("project")).unwrap()
    }

    fn assert_no_store(&self) {
        assert!(!self.directory.path().join("project/.review").exists());
    }
}

fn fetched_id(output: Output) -> ReviewId {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    Uuid::parse_str(stdout.trim()).unwrap();
    ReviewId(stdout.trim().into())
}

fn mapped(backend: &ReviewBackend, resource: &str, remote_id: &str) -> String {
    backend
        .mappings
        .iter()
        .find(|mapping| mapping.resource == resource && mapping.remote_id == remote_id)
        .unwrap_or_else(|| {
            panic!(
                "Missing mapping {resource}/{remote_id}: {:?}",
                backend.mappings
            )
        })
        .core_id
        .clone()
}

#[test]
fn fetch_normalizes_all_pages_and_retains_read_history_without_creating_a_batch() {
    let fake = FakeGh::new();
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let review = store.get_review(&id).unwrap().unwrap();
    let backend = store.review_backend(&id).unwrap().unwrap();
    assert_eq!(backend.source.backend, "github");
    assert_eq!(backend.source.locator, URL);
    let reviewer = User {
        id: UserId(mapped(&backend, "user", "1")),
        name: "reviewer".into(),
    };
    let author = User {
        id: UserId(mapped(&backend, "user", "2")),
        name: "author".into(),
    };
    assert_eq!(review.reviewer, reviewer);
    assert_eq!(
        review.read.pull_request.id.0,
        mapped(&backend, "pull_request", "70")
    );
    assert_eq!(review.read.pull_request.author, Some(author.clone()));
    assert_eq!(review.read.pull_request.status, PullRequestStatus::Open);
    assert_eq!(review.read.pull_request.title, "Review this");
    assert_eq!(review.read.pull_request.body, "PR description");
    assert_eq!(
        review.read.pull_request.revision,
        DiffRevision {
            base: "base-sha".into(),
            head: "head-sha".into()
        }
    );
    assert_eq!(
        review.read.files,
        vec![
            ChangedFile {
                path: "src/new.rs".into(),
                previous_path: None,
                change: FileChange::Modified,
                viewed: true
            },
            ChangedFile {
                path: "src/old.rs".into(),
                previous_path: Some("src/original.rs".into()),
                change: FileChange::Renamed,
                viewed: false
            },
        ]
    );
    assert_eq!(review.read.comments.len(), 4);
    let issue = &review.read.comments[0];
    assert_eq!(issue.content.id.0, mapped(&backend, "issue_comment", "101"));
    assert_eq!(issue.content.body, "PR-wide comment");
    assert_eq!(issue.author, None);
    assert_eq!(issue.target, CommentTarget::PullRequest);
    let parent = &review.read.comments[1];
    assert_eq!(
        parent.content.id.0,
        mapped(&backend, "review_comment", "201")
    );
    assert_eq!(parent.author, Some(reviewer.clone()));
    let CommentTarget::Diff(location) = &parent.target else {
        panic!("Expected diff anchor")
    };
    assert_eq!(
        location.original.revision,
        ReadCommentRevision::Commit {
            head: "old-head".into()
        }
    );
    assert_eq!(location.original.position.path, "src/new.rs");
    let original_range = location.original.position.range.as_ref().unwrap();
    assert_eq!(
        (
            original_range.start.line.get(),
            original_range.end.line.get()
        ),
        (2, 4)
    );
    assert_eq!(original_range.start.side, DiffSide::Head);
    let current = location.current.as_ref().unwrap();
    assert_eq!(
        current.revision,
        ReadCommentRevision::Commit {
            head: "head-sha".into()
        }
    );
    let current_range = current.position.range.as_ref().unwrap();
    assert_eq!(
        (current_range.start.line.get(), current_range.end.line.get()),
        (3, 5)
    );
    assert_eq!(location.subject, Some(CommentSubject::Line));
    assert_eq!(location.diff_hunk.as_deref(), Some("@@ -1 +1 @@"));
    assert_eq!(
        review.read.comments[2].target,
        CommentTarget::Reply(parent.content.id.clone())
    );
    let outdated = &review.read.comments[3];
    assert_eq!(outdated.author, None);
    let CommentTarget::Diff(location) = &outdated.target else {
        panic!("Expected old diff anchor")
    };
    assert!(location.current.is_none());
    assert_eq!(
        location.original.revision,
        ReadCommentRevision::Commit {
            head: "older-head".into()
        }
    );
    assert_eq!(
        location
            .original
            .position
            .range
            .as_ref()
            .unwrap()
            .start
            .side,
        DiffSide::Base
    );
    assert_eq!(
        review.read.reactions,
        vec![
            CommentReaction {
                comment: issue.content.id.clone(),
                author: Some(reviewer.id.clone()),
                reaction: "👍".into()
            },
            CommentReaction {
                comment: parent.content.id.clone(),
                author: None,
                reaction: "❤️".into()
            },
        ]
    );
    assert_eq!(
        review.read.reviews,
        vec![
            ReadReview {
                id: Some(ReviewResourceId(mapped(&backend, "review", "301"))),
                author: Some(reviewer),
                state: ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Approve,
                    body: Some("Looks good".into())
                })
            },
            ReadReview {
                id: Some(ReviewResourceId(mapped(&backend, "review", "302"))),
                author: None,
                state: ReadReviewState::Inactive {
                    state: InactiveReviewState::Dismissed,
                    body: Some("Old objections".into())
                }
            },
            ReadReview {
                id: Some(ReviewResourceId(mapped(&backend, "review", "303"))),
                author: Some(author),
                state: ReadReviewState::Inactive {
                    state: InactiveReviewState::Pending,
                    body: Some("Unsubmitted".into())
                }
            },
        ]
    );
    assert!(store.batch_ids(&id).unwrap().is_empty());
    for mapping in &backend.mappings {
        assert_ne!(mapping.core_id, mapping.remote_id);
        Uuid::parse_str(&mapping.core_id).unwrap();
    }
    let second = fetched_id(fake.fetch(URL, None));
    assert_ne!(id, second);
    assert_eq!(
        store.get_review(&second).unwrap().unwrap().read,
        review.read
    );
    assert_eq!(store.review_backend(&second).unwrap().unwrap(), backend);
    assert!(store.batch_ids(&second).unwrap().is_empty());
    assert_eq!(store.review_ids().unwrap().len(), 2);
}

#[test]
fn late_request_failure_leaves_existing_snapshot_and_mappings_unchanged() {
    let fake = FakeGh::new();
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let snapshot = store.get_review(&id).unwrap();
    let backend = store.review_backend(&id).unwrap();
    let failed = fake.fetch(URL, Some("reactions203"));
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("Fixture request failed: reactions203")
    );
    assert_eq!(store.review_ids().unwrap(), vec![id.clone()]);
    assert_eq!(store.get_review(&id).unwrap(), snapshot);
    assert_eq!(store.review_backend(&id).unwrap(), backend);
}

#[test]
fn graphql_errors_fail_before_creating_local_state() {
    let fake = FakeGh::new();
    fake.write(
        "graphql",
        json!([{"errors":[{"message":"Forbidden viewed-file query"}]}]),
    );
    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Forbidden viewed-file query"));
    fake.assert_no_store();
}

#[test]
fn missing_viewed_file_data_fails_instead_of_saving_a_partial_snapshot() {
    let fake = FakeGh::new();
    fake.write(
        "graphql",
        json!([
            {"data":{"repository":{"pullRequest":{"baseRefOid":"base-sha","headRefOid":"head-sha",
             "files":{"totalCount":2,"nodes":[{"path":"src/new.rs","viewerViewedState":"VIEWED"}],
             "pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}
        ]),
    );
    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("viewed-file"));
    fake.assert_no_store();
}

#[test]
fn duplicate_rest_file_paths_fail_even_when_file_counts_match_viewed_data() {
    let fake = FakeGh::new();
    fake.write(
        "files",
        json!([
            [{"filename":"src/new.rs","status":"modified"}],
            [{"filename":"src/new.rs","status":"modified"}]
        ]),
    );
    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains("changed file counts or viewed-file paths disagree")
    );
    fake.assert_no_store();
}

#[test]
fn invalid_pull_request_urls_fail_without_invoking_gh_or_creating_state() {
    let fake = FakeGh::new();
    for url in [
        "https://example.com/acme/repo/pull/7",
        "https://github.com/acme/repo/issues/7",
        "https://github.com/acme/repo/pull/no-number",
    ] {
        let failed = fake.fetch(url, None);
        assert!(!failed.status.success(), "Accepted invalid URL {url}");
        assert!(failed.stdout.is_empty());
    }
    assert!(!fake.directory.path().join("requests").exists());
    fake.assert_no_store();
}

#[test]
fn pull_requests_beyond_github_file_limit_fail_without_saving_truncated_state() {
    let fake = FakeGh::new();
    let mut pull: Value =
        serde_json::from_slice(&fs::read(fake.directory.path().join("pr.json")).unwrap()).unwrap();
    pull["changed_files"] = json!(3001);
    fake.write("pr", pull);

    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("3000"));
    fake.assert_no_store();
}
