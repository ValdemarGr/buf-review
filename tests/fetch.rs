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

fn connection(nodes: Vec<Value>, has_next: bool, cursor: Option<&str>) -> Value {
    json!({"totalCount":nodes.len(),"nodes":nodes,"pageInfo":{"hasNextPage":has_next,"endCursor":cursor}})
}

impl FakeGh {
    fn new() -> Self {
        let this = Self {
            directory: tempdir().unwrap(),
        };
        fs::create_dir(this.directory.path().join("bin")).unwrap();
        fs::create_dir(this.directory.path().join("project")).unwrap();
        let executable = this.directory.path().join("bin/gh");
        fs::write(&executable, r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_GH_REQUESTS"
case "$2" in
 graphql)
   body=$(cat)
   printf '%s\n' "$body" >> "$FAKE_GH_REQUESTS.query"
   case "$body" in
     *ReviewSnapshot*) fixture=snapshot ;;
     *ReviewFilePage*) fixture=graphql ;;
     *CommentReactions*) fixture=review_reactions ;;
     *ReviewIssueCommentPage*) fixture=issue_next ;;
     *ReviewReviewPage*) fixture=review_next ;;
     *ReviewThreadPage*) fixture=thread_next ;;
     *ThreadComments*) fixture=comment_next ;;
     *) printf 'Unexpected GraphQL query' >&2; exit 1 ;;
   esac ;;
 repos/acme/repo/pulls/7) fixture=pr ;;
 repos/acme/repo/pulls/comments/201) fixture=anchor ;;
 repos/acme/repo/pulls/7/files*) fixture=files ;;
 *) printf 'Unexpected endpoint: %s\n' "$2" >&2; exit 1 ;;
esac
if [ "${FAKE_GH_FAIL:-}" = "$fixture" ]; then printf 'Fixture request failed: %s\n' "$fixture" >&2; exit 1; fi
cat "$FAKE_GH_FIXTURES/$fixture.json"
"#).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        this.write(
            "pr",
            json!({"base":{"sha":"base-sha"},"head":{"sha":"head-sha"},"changed_files":2}),
        );
        this.write("files", json!([[{"filename":"src/new.rs","status":"modified","additions":12,"deletions":3}],[{"filename":"src/old.rs","previous_filename":"src/original.rs","status":"renamed","additions":0,"deletions":8}]]));
        let user = json!({"databaseId":1,"login":"reviewer"});
        let author = json!({"databaseId":2,"login":"author"});
        let empty = connection(vec![], false, None);
        let reaction =
            |id: &str, content: &str, user: Value| json!({"id":id,"content":content,"user":user});
        let root = json!({"id":"RC_201","fullDatabaseId":"201","body":"Current range","createdAt":"2026-01-01T00:00:01Z","author":user,"replyTo":null,"pullRequestReview":{"fullDatabaseId":"301"},"commit":{"oid":"head-sha"},"originalCommit":{"oid":"old-head"},"path":"src/new.rs","line":5,"startLine":3,"originalLine":4,"originalStartLine":2,"subjectType":"LINE","diffHunk":"@@ -1 +1 @@","outdated":false,"state":"SUBMITTED","reactions":connection(vec![reaction("r201","HEART",Value::Null)],false,None)});
        let mut reply = root.clone();
        reply["id"] = json!("RC_202");
        reply["fullDatabaseId"] = json!("202");
        reply["body"] = json!("Reply");
        reply["createdAt"] = json!("2026-01-01T00:00:02Z");
        reply["author"] = author.clone();
        reply["replyTo"] = json!({"fullDatabaseId":"201"});
        reply["reactions"] = empty.clone();
        let outdated = json!({"id":"RC_203","fullDatabaseId":"203","body":"Outdated comment","createdAt":"2026-01-01T00:00:03Z","author":null,"replyTo":null,"pullRequestReview":{"fullDatabaseId":"302"},"commit":{"oid":"head-sha"},"originalCommit":{"oid":"older-head"},"path":"src/old.rs","line":null,"startLine":null,"originalLine":7,"originalStartLine":null,"subjectType":"LINE","diffHunk":"@@ -7 +7 @@","outdated":true,"state":"SUBMITTED","reactions":empty});
        let review = |id: &str, num: &str, state: &str, body: &str, user: Value| json!({"id":id,"fullDatabaseId":num,"state":state,"body":body,"author":user,"submittedAt":"2026-01-02T00:00:00Z","reactions":empty});
        let mut files = connection(
            vec![json!({"path":"src/new.rs","changeType":"MODIFIED","viewerViewedState":"VIEWED"})],
            true,
            Some("files-next"),
        );
        files["totalCount"] = json!(2);
        this.write("snapshot",json!({"data":{"viewer":user,"repository":{"pullRequest":{
            "id":"PR_70","fullDatabaseId":"70","title":"Review this","body":"PR description","author":author,"state":"OPEN","baseRefOid":"base-sha","headRefOid":"head-sha","headRefName":"feature/review","baseRefName":"master","headRepository":{"url":"https://github.com/contributor/repo"},"changedFiles":2,"files":files,
            "comments":connection(vec![json!({"id":"IC_101","fullDatabaseId":"101","body":"PR-wide comment","createdAt":"2026-01-01T00:00:00Z","author":null,"reactions":connection(vec![reaction("r101","THUMBS_UP",user.clone())],false,None)})],false,None),
            "reviews":connection(vec![review("R_301","301","APPROVED","Looks good",user.clone()),review("R_302","302","DISMISSED","Old objections",Value::Null),review("R_303","303","PENDING","Unsubmitted",author.clone())],false,None),
            "reviewThreads":connection(vec![json!({"id":"T201","diffSide":"RIGHT","startDiffSide":"RIGHT","comments":connection(vec![root,reply],false,None)}),json!({"id":"T203","diffSide":"LEFT","startDiffSide":null,"comments":connection(vec![outdated],false,None)})],false,None)
        }}}}));
        let mut files = connection(
            vec![
                json!({"path":"src/old.rs","changeType":"RENAMED","viewerViewedState":"UNVIEWED"}),
            ],
            false,
            None,
        );
        files["totalCount"] = json!(2);
        this.write(
            "graphql",
            json!({"data":{"repository":{"pullRequest":{"files":files}}}}),
        );
        this.write(
            "review_reactions",
            json!({"data":{"node":{"reactions":empty}}}),
        );
        this
    }

    fn read(&self, fixture: &str) -> Value {
        serde_json::from_slice(
            &fs::read(self.directory.path().join(format!("{fixture}.json"))).unwrap(),
        )
        .unwrap()
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
        review.read.pull_request.source,
        Some(BranchSource {
            repository: "https://github.com/contributor/repo".into(),
            branch: "feature/review".into(),
        })
    );
    let queries = fs::read_to_string(fake.directory.path().join("requests.query")).unwrap();
    assert!(queries.contains("headRefName baseRefName headRepository{url}"));
    assert!(
        matches!(store.pull_request(&review.read.pull_request.id).unwrap().unwrap().source,
        PullRequestSource::Remote { branches: Some(BranchPair { source, sink }), .. }
        if source.branch == "feature/review" && sink.branch == "master"
            && sink.repository == "https://github.com/acme/repo")
    );
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
                stats: Some(FileStats {
                    additions: 12,
                    deletions: 3,
                }),
                viewed: true
            },
            ChangedFile {
                path: "src/old.rs".into(),
                previous_path: Some("src/original.rs".into()),
                change: FileChange::Renamed,
                stats: Some(FileStats {
                    additions: 0,
                    deletions: 8,
                }),
                viewed: false
            },
        ]
    );
    assert_eq!(review.read.comments.len(), 6);
    assert!(review.read.comments.iter().all(|comment| !comment.draft));
    assert_eq!(
        review.read.comments[4].content.id.0,
        mapped(&backend, "review_summary", "301")
    );
    assert_eq!(review.read.comments[4].content.body, "Looks good");
    assert_eq!(review.read.comments[5].content.body, "Old objections");
    assert!(
        review
            .read
            .comments
            .iter()
            .all(|comment| comment.content.body != "Unsubmitted")
    );
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
        if !mapping.resource.starts_with("created_at:") {
            assert_ne!(mapping.core_id, mapping.remote_id);
        }
        Uuid::parse_str(&mapping.core_id).unwrap();
    }
    let second = fetched_id(fake.fetch(URL, None));
    assert_eq!(id, second);
    assert_eq!(
        store.get_review(&second).unwrap().unwrap().read,
        review.read
    );
    assert_eq!(store.review_backend(&second).unwrap().unwrap(), backend);
    assert!(store.batch_ids(&second).unwrap().is_empty());
    assert_eq!(store.review_ids().unwrap(), vec![id.clone()]);

    let mut changed = fake.read("snapshot");
    changed["data"]["repository"]["pullRequest"]["comments"]["nodes"][0]["body"] =
        json!("Remote edit without a new commit");
    fake.write("snapshot", changed);
    let edited = fetched_id(fake.fetch(URL, None));
    assert_ne!(edited, id);
    let edited_review = store.get_review(&edited).unwrap().unwrap();
    assert_eq!(
        edited_review.read.pull_request.revision,
        review.read.pull_request.revision
    );
    assert!(edited_review.read.comments.iter().any(|comment| {
        comment.content.id == issue.content.id
            && comment.content.body == "Remote edit without a new commit"
    }));
    assert_eq!(store.get_review(&id).unwrap().unwrap(), review);
    assert_eq!(store.review_backend(&edited).unwrap().unwrap(), backend);
    assert!(store.batch_ids(&edited).unwrap().is_empty());
    assert_eq!(store.review_ids().unwrap().len(), 2);
}

#[test]
fn unavailable_head_repository_keeps_fetched_review_readable_without_branch_source() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    snapshot["data"]["repository"]["pullRequest"]["headRepository"] = Value::Null;
    fake.write("snapshot", snapshot);
    let id = fetched_id(fake.fetch(URL, None));
    assert_eq!(
        fake.store()
            .get_review(&id)
            .unwrap()
            .unwrap()
            .read
            .pull_request
            .source,
        None
    );
}

#[test]
fn late_request_failure_leaves_existing_snapshot_and_mappings_unchanged() {
    let fake = FakeGh::new();
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let snapshot = store.get_review(&id).unwrap();
    let backend = store.review_backend(&id).unwrap();
    let failed = fake.fetch(URL, Some("pr"));
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Fixture request failed: pr"));
    assert_eq!(store.review_ids().unwrap(), vec![id.clone()]);
    assert_eq!(store.get_review(&id).unwrap(), snapshot);
    assert_eq!(store.review_backend(&id).unwrap(), backend);
}

#[test]
fn graphql_errors_fail_before_creating_local_state() {
    let fake = FakeGh::new();
    fake.write(
        "snapshot",
        json!({"errors":[{"message":"Forbidden snapshot query"}]}),
    );
    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Forbidden snapshot query"));
    fake.assert_no_store();
}

#[test]
fn missing_viewed_file_data_fails_instead_of_saving_a_partial_snapshot() {
    let fake = FakeGh::new();
    fake.write("graphql",json!({"data":{"repository":{"pullRequest":{"files":{"totalCount":2,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}));
    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("pagination incomplete"));
    fake.assert_no_store();
}

#[test]
fn duplicate_rest_file_paths_fail_even_when_file_counts_match_viewed_data() {
    let fake = FakeGh::new();
    fake.write(
        "files",
        json!([
            [{"filename":"src/new.rs","status":"modified","additions":12,"deletions":3}],
            [{"filename":"src/new.rs","status":"modified","additions":12,"deletions":3}]
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
    let mut snapshot = fake.read("snapshot");
    snapshot["data"]["repository"]["pullRequest"]["changedFiles"] = json!(3001);
    fake.write("snapshot", snapshot);

    let failed = fake.fetch(URL, None);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("3000"));
    fake.assert_no_store();
}

#[test]
fn review_summary_reactions_paginate_and_preserve_current_user() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    for review in snapshot["data"]["repository"]["pullRequest"]["reviews"]["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .take(2)
    {
        review["reactions"] = json!({"totalCount":2,"nodes":[{"id":"up","content":"THUMBS_UP","user":{"databaseId":1,"login":"reviewer"}}],"pageInfo":{"hasNextPage":true,"endCursor":"next"}});
    }
    fake.write("snapshot", snapshot);
    fake.write("review_reactions",json!({"data":{"node":{"reactions":{"totalCount":2,"nodes":[{"id":"heart","content":"HEART","user":null}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}));
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let review = store.get_review(&id).unwrap().unwrap();
    let backend = store.review_backend(&id).unwrap().unwrap();
    for remote in ["301", "302"] {
        let comment = CommentId(mapped(&backend, "review_summary", remote));
        let reactions = review
            .read
            .reactions
            .iter()
            .filter(|reaction| reaction.comment == comment)
            .collect::<Vec<_>>();
        assert_eq!(reactions.len(), 2);
        assert_eq!(reactions[0].reaction, "👍");
        assert_eq!(reactions[0].author, Some(review.reviewer.id.clone()));
        assert_eq!(reactions[1].author, None);
    }
}

#[test]
fn failed_summary_reaction_fetch_never_saves_partial_snapshot() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    snapshot["data"]["repository"]["pullRequest"]["reviews"]["nodes"][0]["reactions"] =
        json!({"totalCount":1,"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"next"}});
    fake.write("snapshot", snapshot);
    let output = fake.fetch(URL, Some("review_reactions"));
    assert!(!output.status.success());
    fake.assert_no_store();
}

#[test]
fn ordinary_snapshot_fetch_uses_three_requests_and_inlines_reactions() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    let second =
        fake.read("graphql")["data"]["repository"]["pullRequest"]["files"]["nodes"][0].clone();
    let files = &mut snapshot["data"]["repository"]["pullRequest"]["files"];
    files["nodes"].as_array_mut().unwrap().push(second);
    files["pageInfo"] = json!({"hasNextPage":false,"endCursor":null});
    fake.write("snapshot", snapshot);
    fetched_id(fake.fetch(URL, None));
    let requests = fs::read_to_string(fake.directory.path().join("requests")).unwrap();
    assert_eq!(requests.lines().count(), 3, "{requests}");
    assert_eq!(
        requests
            .lines()
            .filter(|line| line.starts_with("api graphql "))
            .count(),
        1
    );
    assert!(!requests.contains("/reactions"));
    assert!(!requests.lines().any(|line| line.starts_with("api user ")));
}

#[test]
fn overflow_fetches_each_connection_without_refetching_other_first_pages() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    let pull = &mut snapshot["data"]["repository"]["pullRequest"];
    let mut issue = pull["comments"]["nodes"][0].clone();
    issue["id"] = json!("IC_102");
    issue["fullDatabaseId"] = json!("102");
    issue["author"] = json!({"databaseId":9,"login":"bot[bot]"});
    issue["reactions"] = connection(vec![], false, None);
    pull["comments"]["totalCount"] = json!(2);
    pull["comments"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"issue-next"});
    let mut next = connection(vec![issue], false, None);
    next["totalCount"] = json!(2);
    fake.write(
        "issue_next",
        json!({"data":{"repository":{"pullRequest":{"comments":next}}}}),
    );
    let mut review = pull["reviews"]["nodes"][0].clone();
    review["id"] = json!("R_304");
    review["fullDatabaseId"] = json!("304");
    review["body"] = json!("Another review");
    pull["reviews"]["totalCount"] = json!(4);
    pull["reviews"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"review-next"});
    let mut next = connection(vec![review], false, None);
    next["totalCount"] = json!(4);
    fake.write(
        "review_next",
        json!({"data":{"repository":{"pullRequest":{"reviews":next}}}}),
    );
    let reply = pull["reviewThreads"]["nodes"][0]["comments"]["nodes"]
        .as_array_mut()
        .unwrap()
        .pop()
        .unwrap();
    pull["reviewThreads"]["nodes"][0]["comments"]["pageInfo"] =
        json!({"hasNextPage":true,"endCursor":"comment-next"});
    let mut next = connection(vec![reply], false, None);
    next["totalCount"] = json!(2);
    fake.write("comment_next", json!({"data":{"node":{"comments":next}}}));
    let mut file_thread = pull["reviewThreads"]["nodes"][1].clone();
    file_thread["id"] = json!("T204");
    let file = &mut file_thread["comments"]["nodes"][0];
    file["id"] = json!("RC_204");
    file["fullDatabaseId"] = json!("204");
    file["subjectType"] = json!("FILE");
    file["originalLine"] = Value::Null;
    pull["reviewThreads"]["totalCount"] = json!(3);
    pull["reviewThreads"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"thread-next"});
    let mut next = connection(vec![file_thread], false, None);
    next["totalCount"] = json!(3);
    fake.write(
        "thread_next",
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":next}}}}),
    );
    fake.write("snapshot", snapshot);
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let review = store.get_review(&id).unwrap().unwrap();
    let backend = store.review_backend(&id).unwrap().unwrap();
    assert_eq!(review.read.comments.len(), 9);
    assert_eq!(review.read.reviews.len(), 4);
    let bot = review
        .read
        .comments
        .iter()
        .find(|comment| comment.content.id.0 == mapped(&backend, "issue_comment", "102"))
        .unwrap();
    assert_eq!(bot.author.as_ref().unwrap().name, "bot[bot]");
    let file = review
        .read
        .comments
        .iter()
        .find(|comment| comment.content.id.0 == mapped(&backend, "review_comment", "204"))
        .unwrap();
    let CommentTarget::Diff(anchor) = &file.target else {
        panic!("file anchor lost")
    };
    assert_eq!(anchor.subject, Some(CommentSubject::File));
    assert!(anchor.original.position.range.is_none());
    assert!(anchor.current.as_ref().unwrap().position.range.is_none());
    assert_eq!(
        fs::read_to_string(fake.directory.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        8
    );
}

#[test]
fn inline_and_issue_reaction_overflow_preserves_all_eight_reactions_and_authors() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    let first = json!({"id":"r0","content":"THUMBS_UP","user":{"databaseId":1,"login":"reviewer"}});
    let mut page = connection(vec![first], true, Some("reaction-next"));
    page["totalCount"] = json!(8);
    snapshot["data"]["repository"]["pullRequest"]["comments"]["nodes"][0]["reactions"] =
        page.clone();
    snapshot["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]["nodes"]
        [0]["reactions"] = page;
    let nodes = [
        "THUMBS_DOWN",
        "LAUGH",
        "HOORAY",
        "CONFUSED",
        "HEART",
        "ROCKET",
        "EYES",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, content)| json!({"id":format!("r{}",i+1),"content":content,"user":null}))
    .collect();
    let mut next = connection(nodes, false, None);
    next["totalCount"] = json!(8);
    fake.write(
        "review_reactions",
        json!({"data":{"node":{"reactions":next}}}),
    );
    fake.write("snapshot", snapshot);
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let review = store.get_review(&id).unwrap().unwrap();
    let backend = store.review_backend(&id).unwrap().unwrap();
    for (resource, numeric) in [("issue_comment", "101"), ("review_comment", "201")] {
        let id = mapped(&backend, resource, numeric);
        let reactions = review
            .read
            .reactions
            .iter()
            .filter(|reaction| reaction.comment.0 == id)
            .collect::<Vec<_>>();
        assert_eq!(
            reactions
                .iter()
                .map(|reaction| reaction.reaction.as_str())
                .collect::<Vec<_>>(),
            ["👍", "👎", "😄", "🎉", "😕", "❤️", "🚀", "👀"]
        );
        assert_eq!(reactions[0].author, Some(review.reviewer.id.clone()));
        assert!(
            reactions[1..]
                .iter()
                .all(|reaction| reaction.author.is_none())
        );
    }
}

#[test]
fn pending_comments_preserve_paginated_roots_replies_and_identity_after_submission() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    let pull = &mut snapshot["data"]["repository"]["pullRequest"];
    pull["reviews"]["nodes"][2]["author"] = json!({"databaseId":1,"login":"reviewer"});
    let mut thread = pull["reviewThreads"]["nodes"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    for comment in thread["comments"]["nodes"].as_array_mut().unwrap() {
        comment["state"] = json!("PENDING");
        comment["author"] = json!({"databaseId":1,"login":"reviewer"});
        comment["pullRequestReview"]["fullDatabaseId"] = json!("303");
    }
    let reply = thread["comments"]["nodes"]
        .as_array_mut()
        .unwrap()
        .pop()
        .unwrap();
    thread["comments"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"comment-next"});
    let mut next = connection(vec![reply], false, None);
    next["totalCount"] = json!(2);
    fake.write("comment_next", json!({"data":{"node":{"comments":next}}}));
    pull["reviewThreads"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"thread-next"});
    let mut next = connection(vec![thread], false, None);
    next["totalCount"] = json!(2);
    fake.write(
        "thread_next",
        json!({"data":{"repository":{"pullRequest":{"reviewThreads":next}}}}),
    );
    fake.write("snapshot", snapshot);
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let read = store.get_review(&id).unwrap().unwrap().read;
    let backend = store.review_backend(&id).unwrap().unwrap();
    let parent = CommentId(mapped(&backend, "review_comment", "201"));
    let reply = CommentId(mapped(&backend, "review_comment", "202"));
    let root = read
        .comments
        .iter()
        .find(|comment| comment.content.id == parent)
        .unwrap();
    let child = read
        .comments
        .iter()
        .find(|comment| comment.content.id == reply)
        .unwrap();
    assert!(root.draft && child.draft);
    assert!(matches!(root.target, CommentTarget::Diff(_)));
    assert_eq!(child.target, CommentTarget::Reply(parent.clone()));
    assert!(
        read.comments
            .iter()
            .filter(|comment| comment.content.id != parent && comment.content.id != reply)
            .all(|comment| !comment.draft)
    );
    assert_eq!(mapped(&backend, "review_member:303", "201"), parent.0);
    assert_eq!(mapped(&backend, "review_member:303", "202"), reply.0);
    assert_eq!(read.reactions.len(), 2);
    assert!(
        read.reactions
            .iter()
            .any(|reaction| reaction.comment == parent)
    );
    assert!(store.batch_ids(&id).unwrap().is_empty());
    assert!(read.reviews.iter().any(|review| matches!(
        review.state,
        ReadReviewState::Inactive {
            state: InactiveReviewState::Pending,
            ..
        }
    )));

    let mut snapshot = fake.read("snapshot");
    snapshot["data"]["repository"]["pullRequest"]["reviews"]["nodes"][2]["state"] =
        json!("COMMENTED");
    fake.write("snapshot", snapshot);
    let mut thread_page = fake.read("thread_next");
    thread_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0]["comments"]["nodes"]
        [0]["state"] = json!("SUBMITTED");
    fake.write("thread_next", thread_page);
    let mut comment_page = fake.read("comment_next");
    comment_page["data"]["node"]["comments"]["nodes"][0]["state"] = json!("SUBMITTED");
    fake.write("comment_next", comment_page);
    let submitted = fetched_id(fake.fetch(URL, None));
    assert_ne!(submitted, id);
    let after = store.get_review(&submitted).unwrap().unwrap().read;
    let after_backend = store.review_backend(&submitted).unwrap().unwrap();
    for (before, remote) in [(root, "201"), (child, "202")] {
        let current = after
            .comments
            .iter()
            .find(|comment| comment.content.id == before.content.id)
            .unwrap();
        assert!(!current.draft);
        assert_eq!(current.content, before.content);
        assert_eq!(current.target, before.target);
        assert_eq!(
            mapped(&after_backend, "review_member:303", remote),
            before.content.id.0
        );
    }
    assert_eq!(store.get_review(&id).unwrap().unwrap().read, read);
    assert!(store.batch_ids(&submitted).unwrap().is_empty());
}

#[test]
fn old_read_comments_default_to_submitted() {
    let comment: ReadComment = serde_json::from_value(json!({
        "id":"legacy-comment", "body":"Existing snapshot", "author":null,
        "target":{"kind":"pull_request"}
    }))
    .unwrap();
    assert!(!comment.draft);
}

#[test]
fn missing_graphql_commit_objects_use_only_the_affected_roots_rest_anchor() {
    let fake = FakeGh::new();
    let mut snapshot = fake.read("snapshot");
    let root = &mut snapshot["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
    root["comments"]["nodes"][0]["commit"] = Value::Null;
    root["comments"]["nodes"][0]["originalCommit"] = Value::Null;
    root["startDiffSide"] = Value::Null;
    fake.write("snapshot", snapshot);
    fake.write(
        "anchor",
        json!({"commit_id":"head-sha","original_commit_id":"old-head","start_side":"RIGHT"}),
    );
    let id = fetched_id(fake.fetch(URL, None));
    let mut store = fake.store();
    let review = store.get_review(&id).unwrap().unwrap();
    let backend = store.review_backend(&id).unwrap().unwrap();
    let root = review
        .read
        .comments
        .iter()
        .find(|comment| comment.content.id.0 == mapped(&backend, "review_comment", "201"))
        .unwrap();
    let CommentTarget::Diff(anchor) = &root.target else {
        panic!("anchor lost")
    };
    assert_eq!(
        anchor.original.revision,
        ReadCommentRevision::Commit {
            head: "old-head".into()
        }
    );
    assert_eq!(
        anchor.current.as_ref().unwrap().revision,
        ReadCommentRevision::Commit {
            head: "head-sha".into()
        }
    );
    assert_eq!(
        anchor.original.position.range.as_ref().unwrap().start.side,
        DiffSide::Head
    );
    let request = fs::read_to_string(fake.directory.path().join("requests")).unwrap();
    assert_eq!(
        request
            .matches("repos/acme/repo/pulls/comments/201")
            .count(),
        1
    );
    let before = store.review_ids().unwrap();
    assert!(!fake.fetch(URL, Some("anchor")).status.success());
    assert_eq!(store.review_ids().unwrap(), before);
}

#[test]
fn incomplete_changed_or_malformed_snapshots_never_save_partial_state() {
    for scenario in [
        "duplicate",
        "count_changed",
        "cursor_repeated",
        "revision_changed",
        "missing_id",
        "partial_errors",
        "missing_reaction_user_id",
    ] {
        let fake = FakeGh::new();
        match scenario {
            "duplicate" => {
                let mut page = fake.read("graphql");
                page["data"]["repository"]["pullRequest"]["files"]["nodes"][0]["path"] =
                    json!("src/new.rs");
                fake.write("graphql", page);
            }
            "count_changed" => {
                let mut page = fake.read("graphql");
                page["data"]["repository"]["pullRequest"]["files"]["totalCount"] = json!(3);
                fake.write("graphql", page);
            }
            "cursor_repeated" => {
                let mut page = fake.read("graphql");
                page["data"]["repository"]["pullRequest"]["files"]["pageInfo"] =
                    json!({"hasNextPage":true,"endCursor":"files-next"});
                fake.write("graphql", page);
            }
            "revision_changed" => {
                let mut pull = fake.read("pr");
                pull["head"]["sha"] = json!("changed-head");
                fake.write("pr", pull);
            }
            other => {
                let mut snapshot = fake.read("snapshot");
                match other {
                    "missing_id" => {
                        snapshot["data"]["repository"]["pullRequest"]["comments"]["nodes"][0]["fullDatabaseId"] =
                            Value::Null;
                    }
                    "partial_errors" => {
                        snapshot["errors"] = json!([{"message":"partial result is forbidden"}]);
                    }
                    "missing_reaction_user_id" => {
                        snapshot["data"]["repository"]["pullRequest"]["comments"]["nodes"][0]["reactions"]
                            ["nodes"][0]["user"]["databaseId"] = Value::Null;
                    }
                    _ => unreachable!(),
                }
                fake.write("snapshot", snapshot);
            }
        }
        let failed = fake.fetch(URL, None);
        assert!(!failed.status.success(), "{scenario}");
        assert!(failed.stdout.is_empty());
        fake.assert_no_store();
    }
}
