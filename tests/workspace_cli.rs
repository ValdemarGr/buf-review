use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use buf_review::{
    model::*,
    store::{IdMapping, OperationResult, ReviewBackend, ReviewSource, Store},
    workspace::{Action, Destination},
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn fixture() -> (TempDir, String) {
    let root = tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["config", "user.name", "Reviewer"]);
    git(
        root.path(),
        &["config", "user.email", "reviewer@example.invalid"],
    );
    fs::write(root.path().join("file.rs"), "before\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "--quiet", "-m", "Base"]);
    let base = git(root.path(), &["rev-parse", "HEAD"]);
    fs::write(root.path().join("file.rs"), "after\n").unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Head"]);
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let revision = DiffRevision { base, head };
    let reviewer = User {
        id: UserId(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"https://github.com/user/10")
                .to_string(),
        ),
        name: "Reviewer".into(),
    };
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId(
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_URL,
                    b"https://github.com/pull_request/20",
                )
                .to_string(),
            ),
            title: "Example PR".into(),
            body: "Review this change".into(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: revision.clone(),
        },
        files: vec![ChangedFile {
            path: "file.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
            viewed: false,
        }],
        comments: vec![ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("root".into()),
                body: "Please fix this".into(),
            },
            author: Some(User {
                id: UserId("other".into()),
                name: "Other".into(),
            }),
            target: CommentTarget::Diff(ReadDiffLocation {
                original: ReadCommentAnchor {
                    revision: ReadCommentRevision::Comparison(revision),
                    position: CommentPosition {
                        path: "file.rs".into(),
                        range: Some(DiffRange {
                            start: DiffLine {
                                side: DiffSide::Head,
                                line: 1.try_into().unwrap(),
                            },
                            end: DiffLine {
                                side: DiffSide::Head,
                                line: 1.try_into().unwrap(),
                            },
                        }),
                    },
                },
                current: None,
                subject: Some(CommentSubject::Line),
                diff_hunk: Some("@@ -1 +1 @@\n-before\n+after".into()),
                outdated: Some(false),
            }),
        }],
        reactions: vec![],
        reviews: vec![],
    };
    let mut store = Store::open(root.path()).unwrap();
    let review = ReviewId("snapshot".into());
    store
        .import_review(
            &review,
            &reviewer,
            &read,
            &ReviewBackend {
                source: ReviewSource {
                    backend: "github".into(),
                    locator: "https://github.com/example/repo/pull/1".into(),
                },
                mappings: vec![
                    IdMapping {
                        resource: "user".into(),
                        core_id: reviewer.id.0.clone(),
                        remote_id: "10".into(),
                    },
                    IdMapping {
                        resource: "pull_request".into(),
                        core_id: read.pull_request.id.0.clone(),
                        remote_id: "20".into(),
                    },
                    IdMapping {
                        resource: "review_comment".into(),
                        core_id: "root".into(),
                        remote_id: "11".into(),
                    },
                ],
            },
        )
        .unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    (root, workspace.id)
}

fn command(root: &Path, request: &Value) -> std::process::Output {
    command_with_path(root, request, None)
}

fn command_with_path(
    root: &Path,
    request: &Value,
    path: Option<&std::ffi::OsStr>,
) -> std::process::Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_pr"));
    process
        .arg("workspace")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        process.env("PATH", path);
    }
    let mut child = process.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
fn entry_provider(root: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    git(root, &["branch", "-M", "entry-feature"]);
    git(
        root,
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
    );
    git(root, &["config", "branch.entry-feature.remote", "origin"]);
    git(
        root,
        &[
            "config",
            "branch.entry-feature.merge",
            "refs/heads/entry-feature",
        ],
    );
    git(root, &["config", "push.default", "simple"]);
    let bin = root.join("entry-bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("gh");
    fs::write(&executable, r#"#!/usr/bin/env python3
import json, pathlib, sys
root = pathlib.Path(__file__).resolve().parents[1]
mode = (root / 'entry-case').read_text().strip() if (root / 'entry-case').exists() else 'unique'
with (root / 'entry-requests.jsonl').open('a') as stream:
    stream.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1] == 'repo':
    print(json.dumps({'nameWithOwner':'example/repo'}))
elif sys.argv[1:3] == ['api','user']:
    print(json.dumps({'id':10,'login':'Reviewer'}))
elif sys.argv[1:3] == ['api','repos/example/repo/pulls?state=open&sort=updated&direction=desc&per_page=100']:
    if mode == 'error':
        print('entry lookup unavailable', file=sys.stderr)
        sys.exit(1)
    def pull(number):
        return {'number':number,'title':'Example PR','html_url':f'https://github.com/example/repo/pull/{number}',
                'user':{'id':10,'login':'Reviewer'},'assignees':[],'state':'open','merged_at':None,'updated_at':'2026-01-01',
                'head':{'ref':'different-feature' if mode == 'zero' else 'entry-feature','repo':{'full_name':'example/repo'}}}
    print(json.dumps([[pull(1)], [pull(2)] if mode == 'many' else []]))
else:
    print('unexpected entry provider request: ' + repr(sys.argv[1:]), file=sys.stderr)
    sys.exit(1)
"#).unwrap();
    fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
}

fn api(root: &Path, request: Value) -> Value {
    let output = command(root, &request);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[cfg(unix)]
#[test]
fn branch_entry_routes_cached_batches_without_selecting_until_explicit_selection() {
    let (root, first) = fixture();
    let path = entry_provider(root.path());
    let call = |request| {
        let output = command_with_path(root.path(), &request, Some(&path));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let mut store = Store::open(root.path()).unwrap();
    let initial = store.workspace(&first).unwrap().unwrap();
    let mut other = initial.review.clone();
    other.id = ReviewId("entry-other-snapshot".into());
    other.read.pull_request.id = PullRequestId("entry-other-pr".into());
    let mut backend = store.review_backend(&initial.review.id).unwrap().unwrap();
    backend.source.locator = "https://github.com/example/repo/pull/2".into();
    store
        .import_review(&other.id, &other.reviewer, &other.read, &backend)
        .unwrap();
    let active_other = store.open_workspace(&other.id).unwrap();
    let initial_count = store.batch_workspaces().unwrap().len();
    drop(store);

    let entered = call(json!({"op":"entry"}));
    assert_eq!(entered, json!({"route":"batch","batch":first}));
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        active_other.id
    );
    assert_eq!(
        Store::open(root.path())
            .unwrap()
            .batch_workspaces()
            .unwrap()
            .len(),
        initial_count
    );
    assert_eq!(
        call(json!({"op":"select","batch":first}))["workspace"]["id"],
        first
    );
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        first
    );

    // A fully delivered batch is history; routing creates one new unselected candidate.
    let mut store = Store::open(root.path()).unwrap();
    let mut workspace = store.workspace(&first).unwrap().unwrap();
    workspace = store
        .mutate_workspace(
            &first,
            workspace.generation,
            Action::Viewed {
                path: "file.rs".into(),
                viewed: true,
            },
        )
        .unwrap();
    let operations = workspace.publication_plan().unwrap().operations;
    workspace = store
        .mutate_workspace(
            &first,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Standalone,
                operations: operations.clone(),
            },
        )
        .unwrap();
    workspace = store
        .mutate_workspace(
            &first,
            workspace.generation,
            Action::RecordResult {
                result: OperationResult {
                    operation: operations[0].clone(),
                    mappings: vec![],
                    response: json!({"confirmed":true}),
                },
            },
        )
        .unwrap();
    store
        .mutate_workspace(
            &first,
            workspace.generation,
            Action::FinishPublication {
                success: true,
                uncertain: false,
                error: None,
            },
        )
        .unwrap();
    drop(store);
    let created = call(json!({"op":"route","url":"https://github.com/example/repo/pull/1"}));
    assert_eq!(created["route"], "batch");
    let candidate = created["batch"].as_str().unwrap();
    assert_ne!(candidate, first);
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        first
    );
    assert_eq!(
        call(json!({"op":"route","url":"https://github.com/example/repo/pull/1"})),
        created
    );

    let mut store = Store::open(root.path()).unwrap();
    let second = store.create_batch_workspace(&initial.review.id).unwrap();
    let count_before_browser = store.batch_workspaces().unwrap().len();
    drop(store);
    let browser = call(json!({"op":"route","url":"https://github.com/example/repo/pull/1"}));
    assert_eq!(browser["route"], "sessions");
    let groups = browser["sessions"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]["pr"]["url"],
        "https://github.com/example/repo/pull/1"
    );
    let sessions = groups[0]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 3);
    assert!(
        sessions
            .iter()
            .any(|row| row["id"] == first && row["stats"]["unfinished"] == false)
    );
    assert!(
        sessions
            .iter()
            .any(|row| row["id"] == candidate && row["stats"]["unfinished"] == true)
    );
    assert!(sessions.iter().any(|row| row["id"] == second.id));
    let store = Store::open(root.path()).unwrap();
    assert_eq!(
        store.batch_workspaces().unwrap().len(),
        count_before_browser
    );
    assert_eq!(store.last_active_workspace().unwrap().unwrap().id, first);
}

#[cfg(unix)]
#[test]
fn branch_entry_falls_back_for_zero_multiple_or_detached_and_does_not_mutate_on_lookup_failure() {
    let (root, active) = fixture();
    let path = entry_provider(root.path());
    let count = Store::open(root.path())
        .unwrap()
        .batch_workspaces()
        .unwrap()
        .len();
    let nested = root.path().join("entry-subdirectory");
    fs::create_dir(&nested).unwrap();
    for (case, expected_rows) in [("zero", 1), ("many", 2), ("detached", 1), ("error", 0)] {
        fs::write(
            root.path().join("entry-case"),
            if case == "detached" { "unique" } else { case },
        )
        .unwrap();
        if case == "detached" {
            git(root.path(), &["checkout", "--quiet", "--detach"]);
        }
        if case == "error" {
            git(root.path(), &["checkout", "--quiet", "entry-feature"]);
        }
        let output = command_with_path(&nested, &json!({"op":"entry"}), Some(&path));
        if case == "error" {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("entry lookup unavailable"));
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["route"], "prs", "{case}: {response}");
            assert_eq!(
                response["pull_requests"].as_array().unwrap().len(),
                expected_rows
            );
            assert!(response.get("batch").is_none() && response.get("workspace").is_none());
        }
        let store = Store::open(root.path()).unwrap();
        assert_eq!(
            store.batch_workspaces().unwrap().len(),
            count,
            "{case} created a batch"
        );
        assert_eq!(
            store.last_active_workspace().unwrap().unwrap().id,
            active,
            "{case} changed active batch"
        );
        assert!(!nested.join(".review").exists());
    }
}

#[test]
fn cached_entry_from_subdirectory_and_immutable_code_need_no_provider() {
    let (root, id) = fixture();
    let child = root.path().join("nested");
    fs::create_dir(&child).unwrap();
    fs::write(root.path().join("file.rs"), "dirty\n").unwrap();
    let active = api(&child, json!({"op":"active"}));
    assert_eq!(active["workspace"]["id"], id);
    assert!(!child.join(".review").exists());
    let code = api(&child, json!({"op":"code","workspace":id,"path":"file.rs"}));
    assert_eq!(code["code"]["base_content"], "before\n");
    assert_eq!(code["code"]["head_content"], "after\n");
    let context = api(
        &child,
        json!({"op":"context","workspace":id,"comment":"root"}),
    );
    assert_eq!(context["context"]["content"], "after\n");
    assert_eq!(
        fs::read_to_string(root.path().join("file.rs")).unwrap(),
        "dirty\n"
    );
}

#[test]
fn bound_cache_read_ignores_active_selection_and_needs_no_generation() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let mut read = original.review.read.clone();
    read.pull_request.id = PullRequestId("other-pr".into());
    read.pull_request.title = "Other PR".into();
    let mut backend = store.review_backend(&original.review.id).unwrap().unwrap();
    backend.source.locator = "https://github.com/example/repo/pull/2".into();
    let snapshot = ReviewId("other-snapshot".into());
    store
        .import_review(&snapshot, &original.review.reviewer, &read, &backend)
        .unwrap();
    let other = store.open_workspace(&snapshot).unwrap();
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        other.id
    );
    let fetched = api(root.path(), json!({"op":"get","workspace":id}));
    assert_eq!(fetched["workspace"]["id"], id);
    assert_eq!(fetched["workspace"]["generation"], original.generation);
    assert_eq!(
        fetched["workspace"]["review"]["read"]["pull_request"]["title"],
        "Example PR"
    );
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        other.id
    );
}

#[test]
fn blank_and_nonblank_saves_cancel_then_restore_same_local_comment_identity() {
    let (root, id) = fixture();
    let saved = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"save","body":"Original reply","target":{"kind":"reply","target":"root"}}]}),
    );
    let comment = saved["comment"].as_str().unwrap();
    let deleted = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":saved["workspace"]["generation"],"actions":[{"action":"save","comment":comment,"body":" \n\t"}]}),
    );
    assert!(deleted["comment"].is_null());
    assert!(
        !deleted["workspace"]["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["comment"]["comment"]["id"] == comment)
    );
    let stale = command(
        root.path(),
        &json!({"op":"actions","workspace":id,"generation":saved["workspace"]["generation"],"actions":[{"action":"save","comment":comment,"body":"stale restore"}]}),
    );
    assert!(!stale.status.success());
    let restored = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":deleted["workspace"]["generation"],"actions":[{"action":"save","comment":comment,"body":"Restored reply"}]}),
    );
    assert_eq!(restored["comment"], comment);
    let comments = restored["workspace"]["comments"].as_array().unwrap();
    let row = comments
        .iter()
        .find(|row| row["comment"]["comment"]["id"] == comment)
        .unwrap();
    assert_eq!(row["comment"]["comment"]["body"], "Restored reply");
    assert_eq!(row["pending_delete"], false);
    assert_eq!(
        comments
            .iter()
            .filter(|row| row["comment"]["comment"]["id"] == comment)
            .count(),
        1
    );
    let reopened = api(root.path(), json!({"op":"get","workspace":id}));
    assert_eq!(
        reopened["workspace"]["comments"],
        restored["workspace"]["comments"]
    );
}

#[test]
fn renamed_base_context_uses_recorded_path_after_comparison_moves() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    git(root.path(), &["mv", "file.rs", "renamed.rs"]);
    git(root.path(), &["commit", "--quiet", "-m", "Rename"]);
    read.pull_request.revision = DiffRevision {
        base: read.pull_request.revision.head.clone(),
        head: git(root.path(), &["rev-parse", "HEAD"]),
    };
    read.files[0].path = "renamed.rs".into();
    read.files[0].previous_path = Some("file.rs".into());
    read.files[0].change = FileChange::Renamed;
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.original.revision =
            ReadCommentRevision::Comparison(read.pull_request.revision.clone());
        location.original.position.path = "renamed.rs".into();
        let range = location.original.position.range.as_mut().unwrap();
        range.start.side = DiffSide::Base;
        range.end.side = DiffSide::Base;
    }
    for step in 0..2 {
        if step == 1 {
            fs::write(root.path().join("renamed.rs"), "new head\n").unwrap();
            git(root.path(), &["commit", "--quiet", "-am", "Later"]);
            read.pull_request.revision = DiffRevision {
                base: read.pull_request.revision.head.clone(),
                head: git(root.path(), &["rev-parse", "HEAD"]),
            };
            read.files[0].previous_path = None;
            read.files[0].change = FileChange::Modified;
        }
        let snapshot = ReviewId(format!("renamed-{step}"));
        store
            .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
            .unwrap();
        let generation = store.workspace(&id).unwrap().unwrap().generation;
        let refreshed = store.refresh_workspace(&id, generation, &snapshot).unwrap();
        store
            .mutate_workspace(
                &id,
                refreshed.generation,
                buf_review::workspace::Action::CompareLatest,
            )
            .unwrap();
        let context = api(
            root.path(),
            json!({"op":"context","workspace":id,"comment":"root"}),
        );
        assert_eq!(context["context"]["content"], "after\n");
        assert_eq!(context["context"]["base_content"], "after\n");
        assert_eq!(context["context"]["head_content"], "after\n");
    }
}

#[test]
fn historical_base_diff_compares_recorded_source_to_latest_head_without_mutating_pin() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    let historical_source = "after\nkeep one\nkeep two\nkeep three\nkeep four\n";
    fs::write(root.path().join("file.rs"), historical_source).unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Context"]);
    let original_base = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["mv", "file.rs", "renamed.rs"]);
    fs::write(
        root.path().join("renamed.rs"),
        historical_source.replace("after", "renamed head"),
    )
    .unwrap();
    git(
        root.path(),
        &["commit", "--quiet", "-am", "Rename and edit"],
    );
    let historical = DiffRevision {
        base: original_base,
        head: git(root.path(), &["rev-parse", "HEAD"]),
    };
    read.pull_request.revision = historical.clone();
    read.files[0].path = "renamed.rs".into();
    read.files[0].previous_path = Some("file.rs".into());
    read.files[0].change = FileChange::Renamed;
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.original.revision = ReadCommentRevision::Comparison(historical.clone());
        location.original.position.path = "renamed.rs".into();
        let range = location.original.position.range.as_mut().unwrap();
        range.start.side = DiffSide::Base;
        range.end.side = DiffSide::Base;
    }
    read.comments.push(ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId("reply".into()),
            body: "Same original code".into(),
        },
        author: read.comments[0].author.clone(),
        target: CommentTarget::Reply(CommentId("root".into())),
    });
    let snapshot = ReviewId("historical-rename".into());
    store
        .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    let refreshed = store
        .refresh_workspace(&id, workspace.generation, &snapshot)
        .unwrap();
    let latest_content = historical_source.replace("after", "current after");
    fs::write(root.path().join("renamed.rs"), &latest_content).unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Later head"]);
    read.pull_request.revision.head = git(root.path(), &["rev-parse", "HEAD"]);
    read.files[0].previous_path = None;
    read.files[0].change = FileChange::Modified;
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.outdated = Some(true);
    }
    let latest = ReviewId("current-rename".into());
    store
        .import_review(&latest, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    store
        .refresh_workspace(&id, refreshed.generation, &latest)
        .unwrap();
    drop(store);
    let before = api(root.path(), json!({"op":"get","workspace":id}));
    let log_before = api(root.path(), json!({"op":"log","workspace":id}));
    for comment in ["root", "reply"] {
        let response = api(
            root.path(),
            json!({"op":"historical_code","workspace":id,"comment":comment}),
        );
        assert_eq!(
            response["code"]["revision"],
            json!({"base":historical.base,"head":read.pull_request.revision.head})
        );
        assert_eq!(response["code"]["base_path"], "file.rs");
        assert_eq!(response["code"]["path"], "renamed.rs");
        assert_eq!(response["code"]["base_content"], historical_source);
        assert_eq!(response["code"]["head_content"], latest_content);
        assert_eq!(response["code"]["historical"], true);
        assert_eq!(response["code"]["provenance"], "recorded");
        assert_eq!(
            response["code"]["location"]["range"]["start"]["side"],
            "base"
        );
        assert_eq!(response["code"]["location"]["path"], "renamed.rs");
        assert_eq!(
            response["code"]["location"]["revision"],
            response["code"]["revision"]
        );
        let context = api(
            root.path(),
            json!({"op":"context","workspace":id,"comment":comment}),
        );
        assert_eq!(context["context"]["content"], historical_source);
    }
    assert_eq!(api(root.path(), json!({"op":"get","workspace":id})), before);
    assert_eq!(
        api(root.path(), json!({"op":"log","workspace":id})),
        log_before
    );
    let current = api(
        root.path(),
        json!({"op":"code","workspace":id,"path":"file.rs"}),
    );
    assert_eq!(current["code"]["head_content"], "after\n");
    assert_eq!(
        current["code"]["revision"],
        before["workspace"]["comparison"]
    );
}

#[test]
fn historical_head_diff_follows_rename_to_fetched_head_and_preserves_original_anchor() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    let original_content = "reviewed\nkeep one\nkeep two\nkeep three\nkeep four\n";
    fs::write(root.path().join("file.rs"), original_content).unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Reviewed head"]);
    let original = DiffRevision {
        base: read.pull_request.revision.base.clone(),
        head: git(root.path(), &["rev-parse", "HEAD"]),
    };
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.original.revision = ReadCommentRevision::Comparison(original.clone());
        location.outdated = Some(true);
    }
    read.comments.push(ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId("reply".into()),
            body: "Check the same code".into(),
        },
        author: read.comments[0].author.clone(),
        target: CommentTarget::Reply(CommentId("root".into())),
    });
    let mut missing_base = read.comments[0].clone();
    missing_base.content.id = CommentId("missing-original-base".into());
    if let CommentTarget::Diff(location) = &mut missing_base.target {
        location.original.revision = ReadCommentRevision::Comparison(DiffRevision {
            base: "a".repeat(40),
            head: original.head.clone(),
        });
    }
    read.comments.push(missing_base);
    git(root.path(), &["mv", "file.rs", "renamed.rs"]);
    let latest_content = original_content.replace("reviewed", "fixed");
    fs::write(root.path().join("renamed.rs"), &latest_content).unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Fix and rename"]);
    read.pull_request.revision.head = git(root.path(), &["rev-parse", "HEAD"]);
    read.files[0].path = "renamed.rs".into();
    read.files[0].previous_path = Some("file.rs".into());
    read.files[0].change = FileChange::Renamed;
    let snapshot = ReviewId("fixed-historical-head".into());
    store
        .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    store
        .refresh_workspace(&id, workspace.generation, &snapshot)
        .unwrap();
    drop(store);
    // The local checkout and pinned comparison both differ from the fetched PR head.
    fs::write(root.path().join("renamed.rs"), "local unrelated\n").unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Local only"]);
    let before = api(root.path(), json!({"op":"get","workspace":id}));
    let log_before = api(root.path(), json!({"op":"log","workspace":id}));
    for comment in ["root", "reply"] {
        let response = api(
            root.path(),
            json!({"op":"historical_code","workspace":id,"comment":comment}),
        );
        assert_eq!(
            response["code"]["revision"],
            json!({"base":original.head,"head":read.pull_request.revision.head})
        );
        assert_eq!(response["code"]["base_path"], "file.rs");
        assert_eq!(response["code"]["path"], "renamed.rs");
        assert_eq!(response["code"]["base_content"], original_content);
        assert_eq!(response["code"]["head_content"], latest_content);
        assert_eq!(response["code"]["head_missing"], false);
        assert_eq!(response["code"]["provenance"], "recorded");
        assert_eq!(
            response["code"]["location"]["range"],
            json!({"start":{"side":"base","line":1},"end":{"side":"base","line":1}})
        );
        assert_eq!(response["anchor"]["revision"], json!(original));
        assert_eq!(response["anchor"]["range"]["start"]["side"], "head");
        let context = api(
            root.path(),
            json!({"op":"context","workspace":id,"comment":comment}),
        );
        assert_eq!(context["context"]["content"], original_content);
        assert_eq!(context["context"]["base_content"], "before\n");
        assert_eq!(context["context"]["head_content"], original_content);
    }
    let without_original_base = api(
        root.path(),
        json!({"op":"historical_code","workspace":id,"comment":"missing-original-base"}),
    );
    assert_eq!(
        without_original_base["code"]["base_content"],
        original_content
    );
    assert_eq!(
        without_original_base["code"]["head_content"],
        latest_content
    );
    assert_eq!(without_original_base["code"]["provenance"], "recorded");
    assert_eq!(
        without_original_base["code"]["location"]["range"]["start"]["side"],
        "base"
    );
    assert_eq!(api(root.path(), json!({"op":"get","workspace":id})), before);
    assert_eq!(
        api(root.path(), json!({"op":"log","workspace":id})),
        log_before
    );
}

#[test]
fn historical_diff_keeps_source_when_file_deleted_from_latest_pr_head() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    let original = read.pull_request.revision.clone();
    git(root.path(), &["rm", "--quiet", "file.rs"]);
    git(
        root.path(),
        &["commit", "--quiet", "-m", "Delete reviewed file"],
    );
    read.pull_request.revision.head = git(root.path(), &["rev-parse", "HEAD"]);
    read.files[0].change = FileChange::Deleted;
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.outdated = Some(true);
    }
    let snapshot = ReviewId("deleted-historical-file".into());
    store
        .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    store
        .refresh_workspace(&id, workspace.generation, &snapshot)
        .unwrap();
    drop(store);
    let before = api(root.path(), json!({"op":"get","workspace":id}));
    let response = api(
        root.path(),
        json!({"op":"historical_code","workspace":id,"comment":"root"}),
    );
    assert_eq!(response["code"]["revision"]["base"], original.head);
    assert_eq!(
        response["code"]["revision"]["head"],
        read.pull_request.revision.head
    );
    assert_eq!(response["code"]["base_content"], "after\n");
    assert_eq!(response["code"]["head_content"], "");
    assert_eq!(response["code"]["head_missing"], true);
    assert_eq!(
        response["code"]["location"]["range"]["start"]["side"],
        "base"
    );
    assert_eq!(api(root.path(), json!({"op":"get","workspace":id})), before);
}

#[test]
fn pruned_historical_objects_fall_back_to_recorded_hunk_without_mutating_session() {
    let (root, id) = fixture();
    let before = api(root.path(), json!({"op":"get","workspace":id}));
    // Make the original comparison unreachable: Git, rather than SQLite, owns file bytes.
    let old_branch = git(root.path(), &["branch", "--show-current"]);
    git(
        root.path(),
        &["checkout", "--quiet", "--orphan", "replacement"],
    );
    fs::write(root.path().join("file.rs"), "replacement\n").unwrap();
    git(root.path(), &["add", "file.rs"]);
    git(root.path(), &["commit", "--quiet", "-m", "Replacement"]);
    git(root.path(), &["branch", "-D", &old_branch]);
    git(root.path(), &["reflog", "expire", "--expire=now", "--all"]);
    git(root.path(), &["gc", "--prune=now"]);
    let response = api(
        root.path(),
        json!({"op":"historical_code","workspace":id,"comment":"root"}),
    );
    assert!(response["code"].is_null(), "{response}");
    assert_eq!(response["diff_hunk"], "@@ -1 +1 @@\n-before\n+after");
    assert!(response["anchor"]["revision"]["base"].is_string());
    let context = api(
        root.path(),
        json!({"op":"context","workspace":id,"comment":"root"}),
    );
    assert_eq!(context["context"]["available"], false);
    assert!(context["context"]["content"].is_null());
    assert_eq!(context["context"]["diff_hunk"], response["diff_hunk"]);
    assert_eq!(api(root.path(), json!({"op":"get","workspace":id})), before);
}

#[test]
fn git_code_distinguishes_empty_file_from_absent_anchor() {
    let (root, id) = fixture();
    fs::write(root.path().join("empty.rs"), "").unwrap();
    git(root.path(), &["add", "empty.rs"]);
    git(root.path(), &["commit", "--quiet", "-m", "Add empty file"]);
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    read.pull_request.revision.head = git(root.path(), &["rev-parse", "HEAD"]);
    read.files.push(ChangedFile {
        path: "empty.rs".into(),
        previous_path: None,
        change: FileChange::Added,
        stats: None,
        viewed: false,
    });
    for (id, side) in [
        ("empty-head", DiffSide::Head),
        ("absent-base", DiffSide::Base),
    ] {
        let mut comment = read.comments[0].clone();
        comment.content.id = CommentId(id.into());
        if let CommentTarget::Diff(location) = &mut comment.target {
            location.original.revision =
                ReadCommentRevision::Comparison(read.pull_request.revision.clone());
            location.original.position.path = "empty.rs".into();
            let range = location.original.position.range.as_mut().unwrap();
            range.start.side = side;
            range.end.side = side;
        }
        read.comments.push(comment);
    }
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let snapshot = ReviewId("empty-file".into());
    store
        .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    let refreshed = store
        .refresh_workspace(&id, workspace.generation, &snapshot)
        .unwrap();
    store
        .mutate_workspace(&id, refreshed.generation, Action::CompareLatest)
        .unwrap();
    drop(store);
    let code = api(
        root.path(),
        json!({"op":"code","workspace":id,"path":"empty.rs"}),
    );
    assert_eq!(code["code"]["base_content"], "");
    assert_eq!(code["code"]["head_content"], "");
    assert_eq!(code["code"]["binary"], false);
    for (comment, available, content) in [
        ("empty-head", true, json!("")),
        ("absent-base", false, Value::Null),
    ] {
        let context = api(
            root.path(),
            json!({"op":"context","workspace":id,"comment":comment}),
        );
        assert_eq!(context["context"]["available"], available);
        assert_eq!(context["context"]["content"], content);
    }
}

#[test]
fn reconstructed_historical_diff_labels_provenance_and_never_guesses_base_range() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    let original = read.pull_request.revision.clone();
    if let CommentTarget::Diff(location) = &mut read.comments[0].target {
        location.original.revision = ReadCommentRevision::Commit {
            head: original.head.clone(),
        };
        let range = location.original.position.range.as_mut().unwrap();
        range.start.side = DiffSide::Base;
        range.end.side = DiffSide::Base;
        location.outdated = Some(true);
    }
    let mut head_comment = read.comments[0].clone();
    head_comment.content.id = CommentId("original-head".into());
    if let CommentTarget::Diff(location) = &mut head_comment.target {
        let range = location.original.position.range.as_mut().unwrap();
        range.start.side = DiffSide::Head;
        range.end.side = DiffSide::Head;
    }
    read.comments.push(head_comment);
    fs::write(root.path().join("file.rs"), "later\n").unwrap();
    git(root.path(), &["commit", "--quiet", "-am", "Later head"]);
    read.pull_request.revision.head = git(root.path(), &["rev-parse", "HEAD"]);
    let latest_head = read.pull_request.revision.head.clone();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let snapshot = ReviewId("commit-only-history".into());
    store
        .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    let refreshed = store
        .refresh_workspace(&id, workspace.generation, &snapshot)
        .unwrap();
    store
        .mutate_workspace(&id, refreshed.generation, Action::CompareLatest)
        .unwrap();
    drop(store);
    let before = api(root.path(), json!({"op":"get","workspace":id}));
    for comment in ["root", "original-head"] {
        let response = api(
            root.path(),
            json!({"op":"historical_code","workspace":id,"comment":comment}),
        );
        let source_commit = if comment == "root" {
            &original.base
        } else {
            &original.head
        };
        assert_eq!(
            response["code"]["revision"],
            json!({"base":source_commit,"head":latest_head})
        );
        assert_eq!(response["code"]["head_content"], "later\n");
        assert_eq!(
            response["anchor"]["revision"],
            json!({"head":original.head})
        );
        if comment == "root" {
            assert_eq!(response["code"]["base_content"], "before\n");
            assert_eq!(response["code"]["provenance"], "reconstructed");
            assert!(
                response["code"]["location"].is_null(),
                "reconstructed Base location must remain unknown"
            );
        } else {
            assert_eq!(response["code"]["base_content"], "after\n");
            assert_eq!(response["code"]["provenance"], "recorded");
            assert_eq!(
                response["code"]["location"]["revision"],
                response["code"]["revision"]
            );
            assert_eq!(
                response["code"]["location"]["range"]["start"]["side"],
                "base"
            );
        }
    }
    let context = api(
        root.path(),
        json!({"op":"context","workspace":id,"comment":"root"}),
    );
    assert_eq!(context["context"]["available"], false);
    assert!(context["context"]["content"].is_null());
    assert_eq!(api(root.path(), json!({"op":"get","workspace":id})), before);
}

#[test]
fn real_cli_preserves_destinations_checklist_and_generation_across_processes() {
    let (root, id) = fixture();
    let started = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"start"}]}),
    );
    let save = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":started["workspace"]["generation"],"actions":[{"action":"save","body":"Review reply","target":{"kind":"reply","target":"root"}}]}),
    );
    let draft = save["comment"].as_str().unwrap();
    assert_eq!(save["workspace"]["todo"], json!(["root"]));
    let left = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":save["workspace"]["generation"],"actions":[{"action":"leave"}]}),
    );
    let standalone = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":left["workspace"]["generation"],"actions":[{"action":"save","body":"Review reply after leave","target":{"kind":"reply","target":"root"}}]}),
    );
    let comments = standalone["workspace"]["comments"].as_array().unwrap();
    assert_eq!(
        comments
            .iter()
            .find(|row| row["comment"]["comment"]["id"] == draft)
            .unwrap()["destination"],
        "review"
    );
    let second = standalone["comment"].as_str().unwrap();
    assert_eq!(
        comments
            .iter()
            .find(|row| row["comment"]["comment"]["id"] == second)
            .unwrap()["destination"],
        "review"
    );
    let done = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":standalone["workspace"]["generation"],"actions":[{"action":"reaction","comment":"root","present":true}]}),
    );
    assert_eq!(done["workspace"]["todo"], json!([]));
    assert_eq!(done["workspace"]["done"], json!(["root"]));
    let stale = command(
        root.path(),
        &json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"start"}]}),
    );
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("generation"));
    let reopened = api(root.path(), json!({"op":"active"}));
    assert_eq!(
        reopened["workspace"]["generation"],
        done["workspace"]["generation"]
    );
    assert_eq!(reopened["workspace"]["wip"]["active"], false);
    assert_eq!(
        reopened["workspace"]["comments"],
        done["workspace"]["comments"]
    );
}

#[cfg(unix)]
#[test]
fn refresh_rebases_concurrent_local_saves_without_generation_errors() {
    refresh_race("save");
}

#[cfg(unix)]
#[test]
fn prepared_refresh_leaves_local_writes_available_and_unchanged_until_apply() {
    refresh_race("prepared");
}

#[test]
fn split_compare_updates_pin_preserves_draft_anchor_and_defers_changed_pin() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&original.review.id).unwrap().unwrap();
    let mut read = original.review.read.clone();
    read.pull_request.revision.head = "c".repeat(40);
    let snapshot = ReviewId("new-comparison".into());
    store
        .import_review(&snapshot, &original.review.reviewer, &read, &backend)
        .unwrap();
    let target = CommentTarget::Diff(DiffLocation {
        revision: original.comparison.clone(),
        position: CommentPosition {
            path: "file.rs".into(),
            range: None,
        },
    });
    let staged = store
        .mutate_workspace(
            &id,
            original.generation,
            Action::Save {
                comment: None,
                target: Some(target.clone()),
                body: "Draft before compare".into(),
                destination: None,
            },
        )
        .unwrap();
    let draft = staged.saved_comment.clone().unwrap();
    let response = api(
        root.path(),
        json!({"op":"apply_refresh","workspace":id,"generation":staged.generation,"compare_latest":true,"prepared_refresh":{"workspace":id,"generation":original.generation,"review":snapshot}}),
    );
    assert!(response["error"].is_null());
    let compared = store.workspace(&id).unwrap().unwrap();
    assert_eq!(compared.comparison, read.pull_request.revision);
    assert!(compared.pending().unwrap().operations.iter().any(|operation| matches!(&operation.change, Event::CreateComment { comment } if comment.content.id == draft && comment.target == target)));

    read.pull_request.revision.head = "d".repeat(40);
    let newer = ReviewId("later-comparison".into());
    store
        .import_review(&newer, &original.review.reviewer, &read, &backend)
        .unwrap();
    let unpinned = store
        .refresh_workspace(&id, compared.generation, &newer)
        .unwrap();
    let moved = store
        .mutate_workspace(&id, unpinned.generation, Action::CompareLatest)
        .unwrap();
    let history = store.workspace_log(&id).unwrap();
    let deferred = api(
        root.path(),
        json!({"op":"apply_refresh","workspace":id,"generation":moved.generation,"compare_latest":true,"prepared_refresh":{"workspace":id,"generation":unpinned.generation,"review":newer}}),
    );
    assert_eq!(deferred["refresh_deferred"], true);
    assert_eq!(store.workspace(&id).unwrap().unwrap(), moved);
    assert_eq!(store.workspace_log(&id).unwrap(), history);
}

#[test]
fn prepared_refresh_rejects_other_sessions_and_defers_superseded_baselines() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&original.review.id).unwrap().unwrap();
    let mut read = original.review.read.clone();
    read.pull_request.title = "Prepared snapshot".into();
    let prepared_id = ReviewId("prepared-refresh".into());
    store
        .import_review(&prepared_id, &original.review.reviewer, &read, &backend)
        .unwrap();
    let prepared = json!({"workspace":id,"generation":original.generation,"review":prepared_id});
    let other = store.create_batch_workspace(&original.review.id).unwrap();
    let rejected = command(
        root.path(),
        &json!({"op":"apply_refresh","workspace":other.id,"generation":other.generation,"prepared_refresh":prepared}),
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("another workspace"));
    assert_eq!(store.workspace(&other.id).unwrap().unwrap(), other);
    let missing = command(
        root.path(),
        &json!({"op":"apply_refresh","workspace":id,"generation":original.generation,"prepared_refresh":{"workspace":id,"generation":999,"review":prepared_id}}),
    );
    assert!(!missing.status.success());
    assert_eq!(store.workspace(&id).unwrap().unwrap(), original);

    read.pull_request.title = "More recent snapshot".into();
    let newer = ReviewId("newer-refresh".into());
    store
        .import_review(&newer, &original.review.reviewer, &read, &backend)
        .unwrap();
    let latest = store
        .refresh_workspace(&id, original.generation, &newer)
        .unwrap();
    let history = store.workspace_log(&id).unwrap();
    for token in [
        prepared,
        json!({"workspace":id,"generation":original.generation,"error":"Obsolete failure"}),
    ] {
        let response = api(
            root.path(),
            json!({"op":"apply_refresh","workspace":id,"generation":latest.generation,"prepared_refresh":token}),
        );
        assert_eq!(response["refresh_deferred"], true);
        assert!(response["error"].is_null());
        assert_eq!(store.workspace(&id).unwrap().unwrap(), latest);
        assert_eq!(store.workspace_log(&id).unwrap(), history);
    }
}

#[test]
fn prepared_refresh_cannot_cross_pr_or_reviewer_identity_or_new_receipts() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&original.review.id).unwrap().unwrap();
    for (review_id, foreign_pr) in [("foreign-pr", true), ("foreign-reviewer", false)] {
        let mut read = original.review.read.clone();
        let mut reviewer = original.review.reviewer.clone();
        let mut backend = backend.clone();
        if foreign_pr {
            read.pull_request.id = PullRequestId("different-pr".into());
            backend.source.locator = "https://github.com/example/repo/pull/999".into();
            for mapping in &mut backend.mappings {
                if mapping.resource == "pull_request" {
                    mapping.core_id = read.pull_request.id.0.clone();
                    mapping.remote_id = "9990".into();
                }
            }
        } else {
            reviewer.id = UserId("different-reviewer".into());
        }
        let snapshot = ReviewId(review_id.into());
        store
            .import_review(&snapshot, &reviewer, &read, &backend)
            .unwrap();
        let response = command(
            root.path(),
            &json!({"op":"apply_refresh","workspace":id,"generation":original.generation,"prepared_refresh":{"workspace":id,"generation":original.generation,"review":snapshot}}),
        );
        assert!(!response.status.success());
        assert_eq!(store.workspace(&id).unwrap().unwrap(), original);
    }
    let mut latest = store
        .mutate_workspace(
            &id,
            original.generation,
            Action::Viewed {
                path: "file.rs".into(),
                viewed: true,
            },
        )
        .unwrap();
    let operation = latest.publication_plan().unwrap().operations.remove(0);
    store
        .record_operation_result(
            &latest.batch.id,
            &operation,
            &[],
            &json!({"confirmed":true}),
        )
        .unwrap();
    latest = store.workspace(&id).unwrap().unwrap();
    let response = api(
        root.path(),
        json!({"op":"apply_refresh","workspace":id,"generation":latest.generation,"prepared_refresh":{"workspace":id,"generation":original.generation,"review":original.review.id}}),
    );
    assert_eq!(response["refresh_deferred"], true);
    assert_eq!(store.workspace(&id).unwrap().unwrap(), latest);
    assert!(latest.view().unwrap().files[0].viewed);
}

#[cfg(unix)]
#[test]
fn failed_refresh_preparation_is_read_only_and_queued_error_preserves_staged_work() {
    use std::os::unix::fs::PermissionsExt;
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let log = store.workspace_log(&id).unwrap();
    let snapshots = store.review_ids().unwrap();
    let bin = root.path().join("offline-bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nprintf 'Remote offline' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let output = command_with_path(
        root.path(),
        &json!({"op":"fetch_refresh","workspace":id}),
        Some(&path),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let prepared: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        prepared["prepared_refresh"]["error"]
            .as_str()
            .unwrap()
            .contains("Remote offline")
    );
    assert_eq!(store.workspace(&id).unwrap().unwrap(), original);
    assert_eq!(store.workspace_log(&id).unwrap(), log);
    assert_eq!(store.review_ids().unwrap(), snapshots);
    let staged = store
        .mutate_workspace(
            &id,
            original.generation,
            Action::Viewed {
                path: "file.rs".into(),
                viewed: true,
            },
        )
        .unwrap();
    let response = api(
        root.path(),
        json!({"op":"apply_refresh","workspace":id,"generation":original.generation,"prepared_refresh":prepared["prepared_refresh"]}),
    );
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("Remote offline")
    );
    assert_eq!(response["workspace"]["files"][0]["viewed"], true);
    let latest = store.workspace(&id).unwrap().unwrap();
    assert_eq!(latest.comparison, original.comparison);
    assert_eq!(latest.review, original.review);
    assert_eq!(latest.pending().unwrap(), staged.pending().unwrap());
    assert!(latest.last_error.unwrap().contains("Remote offline"));
    assert_eq!(
        store.workspace_log(&id).unwrap().last().unwrap()["action"]["action"],
        "refresh_error"
    );
    let next = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":response["workspace"]["generation"],"actions":[{"action":"viewed","path":"file.rs","viewed":false}]}),
    );
    assert!(next["error"].is_null());
    assert_eq!(next["workspace"]["files"][0]["viewed"], false);
}

#[cfg(unix)]
#[test]
fn refresh_accepts_stale_generation_for_unchanged_provider_baseline() {
    refresh_race("stale");
}

#[cfg(unix)]
#[test]
fn refresh_defers_during_publication_without_recording_false_error() {
    refresh_race("active");
}

#[cfg(unix)]
#[test]
fn refresh_defers_snapshot_older_than_confirmed_provider_receipts() {
    refresh_race("receipts");
}

#[cfg(unix)]
#[test]
fn older_refresh_does_not_replace_newer_refreshed_baseline() {
    refresh_race("superseded");
}

#[cfg(unix)]
fn refresh_race(scenario: &str) {
    use buf_review::{
        store::OperationResult,
        workspace::{Action, Destination},
    };
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let original = store.workspace(&id).unwrap().unwrap();
    let bin = root.path().join("refresh-bin");
    fs::create_dir(&bin).unwrap();
    let write = |name: &str, value: Value| {
        fs::write(bin.join(name), serde_json::to_vec(&value).unwrap()).unwrap()
    };
    write("user.json", json!({"id":10,"login":"Reviewer"}));
    write(
        "pull.json",
        json!({"id":20,"title":"Fetched PR","body":"Remote description","user":null,"state":"open","merged":false,"changed_files":1,"base":{"sha":original.comparison.base},"head":{"sha":original.comparison.head}}),
    );
    write(
        "files.json",
        json!([[{"filename":"file.rs","status":"modified","additions":12,"deletions":3}]]),
    );
    write(
        "comments.json",
        json!([[{"id":11,"node_id":"RC11","body":"Please fix this","user":{"id":12,"login":"Other"},"path":"file.rs","commit_id":original.comparison.head,"original_commit_id":original.comparison.head,"line":1,"original_line":1,"side":"RIGHT","subject_type":"line"}]]),
    );
    write("empty.json", json!([[]]));
    let empty =
        json!({"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}});
    let comment = json!({"id":"RC11","fullDatabaseId":"11","body":"Please fix this","createdAt":"2026-01-01T00:00:00Z","author":{"databaseId":12,"login":"Other"},"replyTo":null,"pullRequestReview":null,"commit":{"oid":original.comparison.head},"originalCommit":{"oid":original.comparison.head},"path":"file.rs","line":1,"startLine":null,"originalLine":1,"originalStartLine":null,"subjectType":"LINE","diffHunk":"","outdated":false,"state":"SUBMITTED","reactions":empty});
    let thread = json!({"id":"T11","diffSide":"RIGHT","startDiffSide":null,"comments":{"totalCount":1,"nodes":[comment],"pageInfo":{"hasNextPage":false,"endCursor":null}}});
    let pull = json!({"id":"PR20","fullDatabaseId":"20","title":"Fetched PR","body":"Remote description","author":null,"state":"OPEN","baseRefOid":original.comparison.base,"headRefOid":original.comparison.head,"headRefName":"feature","headRepository":{"url":"https://github.com/acme/repo"},"changedFiles":1,"files":{"totalCount":1,"nodes":[{"path":"file.rs","changeType":"MODIFIED","viewerViewedState":"UNVIEWED"}],"pageInfo":{"hasNextPage":false,"endCursor":null}},"comments":empty,"reviews":empty,"reviewThreads":{"totalCount":1,"nodes":[thread],"pageInfo":{"hasNextPage":false,"endCursor":null}}});
    write(
        "snapshot.json",
        json!({"data":{"viewer":{"login":"Reviewer","databaseId":10},"repository":{"pullRequest":pull}}}),
    );
    let gh = bin.join("gh");
    fs::write(&gh,r#"#!/bin/sh
case "$2" in
  graphql)
    cat >/dev/null
    if [ ! -f "$REFRESH_FIXTURES/started" ]; then
      touch "$REFRESH_FIXTURES/started"
      while [ ! -f "$REFRESH_FIXTURES/continue" ]; do sleep 0.01; done
    fi
    fixture=snapshot ;;
  repos/example/repo/pulls/1) fixture=pull ;;
  user) fixture=user ;;
  repos/example/repo/pulls/1/files*) fixture=files ;;
  repos/example/repo/pulls/1/comments*) fixture=comments ;;
  repos/example/repo/issues/1/comments*|repos/example/repo/pulls/1/reviews*|repos/example/repo/pulls/comments/11/reactions*) fixture=empty ;;
  *) printf 'Unexpected fixture endpoint: %s' "$2" >&2; exit 1 ;;
esac
cat "$REFRESH_FIXTURES/$fixture.json"
"#).unwrap();
    fs::set_permissions(gh, fs::Permissions::from_mode(0o755)).unwrap();
    if scenario == "stale" {
        store
            .mutate_workspace(
                &id,
                original.generation,
                Action::Save {
                    comment: None,
                    target: Some(CommentTarget::PullRequest),
                    body: "Saved before refresh".into(),
                    destination: None,
                },
            )
            .unwrap();
    }
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_pr"))
        .arg("workspace")
        .current_dir(root.path())
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REFRESH_FIXTURES", &bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            &serde_json::to_vec(
                &json!({"op":if scenario == "prepared" { "fetch_refresh" } else { "refresh" },"workspace":id,"generation":original.generation}),
            )
            .unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !bin.join("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !bin.join("started").exists() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "refresh never fetched: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut latest = store.workspace(&id).unwrap().unwrap();
    match scenario {
        "save" | "prepared" => {
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::Save {
                        comment: None,
                        target: Some(CommentTarget::PullRequest),
                        body: "Saved during refresh".into(),
                        destination: None,
                    },
                )
                .unwrap();
        }
        "active" => {
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::Summary {
                        conclusion: ReviewConclusion {
                            outcome: ReviewOutcome::Approve,
                            body: None,
                        },
                    },
                )
                .unwrap();
            let operations = latest.prepare(Destination::Review).unwrap().operations;
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::BeginPublication {
                        destination: Destination::Review,
                        operations,
                    },
                )
                .unwrap();
        }
        "receipts" => {
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::Viewed {
                        path: "file.rs".into(),
                        viewed: true,
                    },
                )
                .unwrap();
            let operations = latest.prepare(Destination::Standalone).unwrap().operations;
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::BeginPublication {
                        destination: Destination::Standalone,
                        operations: operations.clone(),
                    },
                )
                .unwrap();
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::RecordResult {
                        result: OperationResult {
                            operation: operations[0].clone(),
                            mappings: vec![],
                            response: json!({"confirmed":true}),
                        },
                    },
                )
                .unwrap();
            latest = store
                .mutate_workspace(
                    &id,
                    latest.generation,
                    Action::FinishPublication {
                        success: true,
                        uncertain: false,
                        error: None,
                    },
                )
                .unwrap();
        }
        "superseded" => {
            let mut read = original.review.read.clone();
            read.pull_request.title = "Newer refresh".into();
            let backend = store.review_backend(&original.review.id).unwrap().unwrap();
            let review = ReviewId("newer-refresh".into());
            store
                .import_review(&review, &original.review.reviewer, &read, &backend)
                .unwrap();
            latest = store
                .refresh_workspace(&id, latest.generation, &review)
                .unwrap();
        }
        _ => {}
    }
    let snapshot_count = store.review_ids().unwrap().len();
    fs::write(bin.join("continue"), "").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let response = if scenario == "prepared" {
        assert_eq!(response["prepared_refresh"]["workspace"], id);
        assert_eq!(
            response["prepared_refresh"]["generation"],
            original.generation
        );
        assert!(response["prepared_refresh"]["review"].is_string());
        assert_eq!(store.workspace(&id).unwrap().unwrap(), latest);
        assert_eq!(store.review_ids().unwrap().len(), snapshot_count + 1);
        assert_eq!(
            store.workspace_log(&id).unwrap().last().unwrap()["action"]["action"],
            "save"
        );
        api(
            root.path(),
            json!({"op":"apply_refresh","workspace":id,"generation":latest.generation,"prepared_refresh":response["prepared_refresh"]}),
        )
    } else {
        response
    };
    assert!(response.get("error").is_none(), "{response}");
    let current = store.workspace(&id).unwrap().unwrap();
    assert!(current.last_error.is_none());
    if matches!(scenario, "save" | "stale" | "prepared") {
        assert_ne!(response["refresh_deferred"], true);
        assert!(current.generation > latest.generation);
        assert_eq!(current.review.read.pull_request.title, "Fetched PR");
        assert!(current.pending().unwrap().operations.iter().any(|operation|matches!(&operation.change,Event::CreateComment{comment} if comment.content.body.starts_with("Saved "))));
    } else {
        assert_eq!(response["refresh_deferred"], true);
        assert_eq!(current.generation, latest.generation);
        assert_eq!(current.review.id, latest.review.id);
        assert_eq!(store.review_ids().unwrap().len(), snapshot_count);
        if scenario == "receipts" {
            assert!(current.view().unwrap().files[0].viewed);
        }
    }
}

#[cfg(unix)]
#[test]
fn headless_neovim_uses_real_cached_cli_without_network_or_source_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let (root, id) = fixture();
    let receipt_comment = {
        let mut store = Store::open(root.path()).unwrap();
        let workspace = store.workspace(&id).unwrap().unwrap();
        let mut read = workspace.review.read.clone();
        let mut owned = read.comments[0].clone();
        owned.content.id = CommentId("own-root".into());
        owned.content.body = "Owned fetched feedback".into();
        owned.author = Some(workspace.review.reviewer.clone());
        read.comments.push(owned);
        let mut backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
        backend.mappings.push(IdMapping {
            resource: "review_comment".into(),
            core_id: "own-root".into(),
            remote_id: "12".into(),
        });
        let snapshot = ReviewId("headless-owned-comment".into());
        store
            .import_review(&snapshot, &workspace.review.reviewer, &read, &backend)
            .unwrap();
        let workspace = store
            .refresh_workspace(&id, workspace.generation, &snapshot)
            .unwrap();
        let workspace = store
            .mutate_workspace(
                &id,
                workspace.generation,
                Action::Save {
                    comment: None,
                    target: Some(CommentTarget::PullRequest),
                    body: "Confirmed comment before refetch".into(),
                    destination: Some(Destination::Standalone),
                },
            )
            .unwrap();
        let receipt_comment = workspace.saved_comment.clone().unwrap();
        let plan = workspace.prepare(Destination::Standalone).unwrap();
        let workspace = store
            .mutate_workspace(
                &id,
                workspace.generation,
                Action::BeginPublication {
                    destination: Destination::Standalone,
                    operations: plan.operations.clone(),
                },
            )
            .unwrap();
        let workspace = store
            .mutate_workspace(
                &id,
                workspace.generation,
                Action::RecordResult {
                    result: OperationResult {
                        operation: plan.operations[0].clone(),
                        mappings: vec![IdMapping {
                            resource: "issue_comment".into(),
                            core_id: receipt_comment.0.clone(),
                            remote_id: "13".into(),
                        }],
                        response: json!({"id":13}),
                    },
                },
            )
            .unwrap();
        store
            .mutate_workspace(
                &id,
                workspace.generation,
                Action::FinishPublication {
                    success: true,
                    uncertain: false,
                    error: None,
                },
            )
            .unwrap();
        receipt_comment
    };
    let bin = root.path().join("fixture-bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\n' 'fixture offline' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new("nvim")
        .current_dir(root.path())
        .args(["--headless", "-u", "NONE", "-c"])
        .arg(format!(
            "lua loadfile({})({})",
            serde_json::to_string(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nvim/run.lua")
            )
            .unwrap(),
            serde_json::to_string(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nvim/real_cli.lua")
            )
            .unwrap(),
        ))
        .env("BUF_REVIEW_RUNTIME", env!("CARGO_MANIFEST_DIR"))
        .env("BUF_REVIEW_PR", env!("CARGO_BIN_EXE_pr"))
        .env("BUF_REVIEW_RECEIPT_COMMENT", &receipt_comment.0)
        .env("PATH", std::env::join_paths(path).unwrap())
        .output()
        .expect("headless integration requires nvim >=0.10; use nix develop");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let view = workspace.view().unwrap();
    assert_eq!(view.done, vec![CommentId("root".into())]);
    assert!(
        view.comments
            .iter()
            .any(|row| row.destination == Some(buf_review::workspace::Destination::Review))
    );
    assert!(workspace.wip.as_ref().is_some_and(|wip| {
        !wip.active
            && wip.conclusion.as_ref().is_some_and(|conclusion| {
                conclusion.body.as_deref() == Some("Saved summary, cancelled submission")
            })
    }));
    assert_eq!(
        fs::read_to_string(root.path().join("file.rs")).unwrap(),
        "after\n"
    );
    assert_eq!(
        workspace.completed.len(),
        1,
        "only seeded synthetic receipt expected"
    );
    assert_eq!(workspace.results.len(), 1, "UI must not publish");
    assert!(
        view.comments
            .iter()
            .any(|row| row.comment.content().id.0 == "own-root" && !row.pending_delete),
        "fetched comments remain immutable"
    );
    assert!(
        view.comments
            .iter()
            .any(|row| row.comment.content().id == receipt_comment
                && !row.pending_delete
                && !row.fetched)
    );
    assert!(!view.todo.contains(&receipt_comment) && !view.done.contains(&receipt_comment));
}

#[test]
fn local_batch_browser_groups_and_selects_the_same_cli_and_editor_batches() {
    let (root, first) = fixture();
    let initial = api(root.path(), json!({"op":"get","workspace":first}));
    let saved = api(
        root.path(),
        json!({"op":"actions","workspace":first,"generation":initial["workspace"]["generation"],"actions":[{"action":"save","body":"First batch comment","target":{"kind":"pull_request"},"destination":"standalone"}]}),
    );
    assert_eq!(saved["workspace"]["stats"]["pending"]["comments"], 1);
    let second = api(root.path(), json!({"op":"new","workspace":first}));
    let second_id = second["workspace"]["id"].as_str().unwrap();
    assert_ne!(first, second_id);
    let mut store = Store::open(root.path()).unwrap();
    let mut other = store.workspace(&first).unwrap().unwrap().review;
    other.id = ReviewId("other-snapshot".into());
    other.read.pull_request.id = PullRequestId("other-pr".into());
    other.read.pull_request.title = "Another PR".into();
    store
        .create_review(&other.id, &other.reviewer, &other.read)
        .unwrap();
    let cli_batch = BatchId("cli-created-batch".into());
    store.create_batch(&cli_batch, &other.id).unwrap();
    drop(store);

    let browser = api(root.path(), json!({"op":"sessions","workspace":first}));
    let groups = browser["sessions"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(groups[1]["sessions"][0]["id"], cli_batch.0);
    let old = groups[0]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == first)
        .unwrap();
    assert_eq!(old["stats"]["pending"]["comments"], 1);
    let filtered = api(root.path(), json!({"op":"sessions","pr":"other-pr"}));
    assert_eq!(filtered["sessions"]["groups"].as_array().unwrap().len(), 1);
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        second_id
    );
    let selected = api(root.path(), json!({"op":"select","batch":cli_batch.0}));
    assert_eq!(selected["workspace"]["id"], cli_batch.0);
    assert_eq!(
        api(root.path(), json!({"op":"active"}))["workspace"]["id"],
        cli_batch.0
    );
    let reordered = api(root.path(), json!({"op":"sessions"}));
    assert_eq!(reordered["sessions"]["groups"][0]["pr"]["id"], "other-pr");
}

#[test]
fn publication_history_attributes_retried_operations_to_their_actual_attempt() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let mut workspace = store.workspace(&id).unwrap().unwrap();
    for action in [
        Action::Save {
            comment: None,
            target: Some(CommentTarget::PullRequest),
            body: "Publish once".into(),
            destination: Some(Destination::Standalone),
        },
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    ] {
        workspace = store
            .mutate_workspace(&id, workspace.generation, action)
            .unwrap();
    }
    let plan = workspace.publication_plan().unwrap().operations;
    assert_eq!(plan.len(), 2);
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Standalone,
                operations: plan.clone(),
            },
        )
        .unwrap();
    let failed = workspace.attempt.as_ref().unwrap().id.clone();
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::FinishPublication {
                success: false,
                uncertain: false,
                error: Some("preflight failed".into()),
            },
        )
        .unwrap();
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::ResumePublication {
                attempt_id: failed.clone(),
            },
        )
        .unwrap();
    let partial = workspace.attempt.as_ref().unwrap().id.clone();
    let comment = plan
        .iter()
        .find(|op| matches!(op.change, Event::CreateComment { .. }))
        .unwrap()
        .clone();
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::RecordResult {
                result: OperationResult {
                    operation: comment,
                    mappings: vec![],
                    response: json!({"confirmed":true}),
                },
            },
        )
        .unwrap();
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::FinishPublication {
                success: false,
                uncertain: false,
                error: Some("progress failed".into()),
            },
        )
        .unwrap();
    let remaining = workspace.publication_plan().unwrap().operations;
    assert_eq!(remaining.len(), 1);
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::ResumePublication {
                attempt_id: partial.clone(),
            },
        )
        .unwrap();
    for operation in remaining {
        workspace = store
            .mutate_workspace(
                &id,
                workspace.generation,
                Action::RecordResult {
                    result: OperationResult {
                        operation,
                        mappings: vec![],
                        response: json!({"confirmed":true}),
                    },
                },
            )
            .unwrap();
    }
    store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::FinishPublication {
                success: true,
                uncertain: false,
                error: None,
            },
        )
        .unwrap();
    drop(store);
    let response = api(root.path(), json!({"op":"get","workspace":id}));
    assert_eq!(response["workspace"]["stats"]["published"]["total"], 2);
    assert_eq!(response["workspace"]["stats"]["pending"]["total"], 0);
    assert_eq!(response["workspace"]["stats"]["unfinished"], false);
    assert!(!response["workspace"]["todo"].as_array().unwrap().is_empty());
    let publications = response["workspace"]["publications"].as_array().unwrap();
    assert_eq!(publications.len(), 1);
    assert_eq!(failed, partial, "retries preserve publication identity");
    let row = publications
        .iter()
        .find(|row| row["id"] == partial)
        .unwrap();
    assert_eq!(row["status"], "published");
    assert_eq!(row["published"]["total"], 2);
    let details = api(
        root.path(),
        json!({"op":"history","workspace":id,"publication":partial}),
    );
    assert_eq!(
        details["publication"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["published"] == true)
            .count(),
        2
    );
}

#[test]
fn failed_progress_keeps_terminal_conclusion_queued_and_unfinished() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let mut workspace = store.workspace(&id).unwrap().unwrap();
    for action in [
        Action::Start,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    ] {
        workspace = store
            .mutate_workspace(&id, workspace.generation, action)
            .unwrap();
    }
    let plan = workspace.publication_plan().unwrap().operations;
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Review,
                operations: plan.clone(),
            },
        )
        .unwrap();
    assert!(matches!(plan[0].change, Event::SetFileViewed { .. }));
    assert!(matches!(plan[1].change, Event::SetReviewConclusion { .. }));
    store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::FinishPublication {
                success: false,
                uncertain: false,
                error: Some("progress failed".into()),
            },
        )
        .unwrap();
    drop(store);
    let response = api(root.path(), json!({"op":"get","workspace":id}));
    assert_eq!(response["workspace"]["stats"]["unfinished"], true);
    assert_eq!(response["workspace"]["stats"]["pending"]["files"], 1);
    assert_eq!(response["workspace"]["publications"][0]["status"], "failed");
    assert_eq!(
        response["workspace"]["publications"][0]["published"]["conclusions"],
        0
    );
}

#[test]
fn workspace_log_api_projects_without_mutating_journal_or_workspace() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::PullRequest),
                body: "First line\nSecond line".into(),
                destination: Some(Destination::Standalone),
            },
        )
        .unwrap();
    let before = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::RefreshError {
                error: "Remote offline\nTry later".into(),
            },
        )
        .unwrap();
    let journal = store.workspace_log(&id).unwrap();
    drop(store);
    let response = api(root.path(), json!({"op":"log","workspace":id}));
    let entries = response["log"].as_array().unwrap();
    assert_eq!(entries.len(), journal.len());
    assert_eq!(entries[0]["generation"], before.generation);
    assert_eq!(entries[0]["level"], "error");
    assert_eq!(
        entries[0]["summary"],
        "Refresh failed · Remote offline Try later"
    );
    assert_eq!(
        entries[1]["summary"],
        "Saved comment · PR · First line Second line"
    );
    for (entry, original) in entries.iter().zip(journal.iter().rev()) {
        let raw = entry["raw"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), *original);
    }
    let store = Store::open(root.path()).unwrap();
    assert_eq!(store.workspace_log(&id).unwrap(), journal);
    assert_eq!(store.workspace(&id).unwrap().unwrap(), before);
}

#[test]
fn ordered_actions_commit_successful_prefix_and_keep_event_history() {
    let (root, id) = fixture();
    let response = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":0,
            "actions":[
                {"action":"viewed","path":"file.rs","viewed":true},
                {"action":"viewed","path":"file.rs","viewed":false},
                {"action":"reaction","comment":"root","present":true},
                {"action":"delete","comment":"missing"},
                {"action":"viewed","path":"file.rs","viewed":true}
            ]
        }),
    );
    assert_eq!(response["applied"], 3);
    assert_eq!(response["failed_index"], 3);
    assert!(response["error"].is_string());
    assert_eq!(response["workspace"]["generation"], 3);
    assert_eq!(response["workspace"]["files"][0]["viewed"], false);
    assert_eq!(response["workspace"]["done"], json!(["root"]));
    let store = Store::open(root.path()).unwrap();
    let saved = store.workspace(&id).unwrap().unwrap();
    assert_eq!(saved.batch.write.events.len(), 3);
    let log = store.workspace_log(&id).unwrap();
    assert_eq!(
        log.iter()
            .skip(1)
            .map(|row| row["action"]["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["viewed", "viewed", "reaction"]
    );
    let pending_ids = saved
        .pending()
        .unwrap()
        .operations
        .into_iter()
        .map(|op| op.id)
        .collect::<Vec<_>>();
    drop(store);
    let read = api(root.path(), json!({"op":"get", "workspace":id}));
    assert_eq!(read["workspace"], response["workspace"]);
    assert_eq!(
        Store::open(root.path())
            .unwrap()
            .workspace(&id)
            .unwrap()
            .unwrap()
            .pending()
            .unwrap()
            .operations
            .into_iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        pending_ids
    );

    let stale = command(
        root.path(),
        &json!({
            "op":"actions", "workspace":id,"generation":0,
            "actions":[{"action":"viewed","path":"file.rs","viewed":true}]
        }),
    );
    assert!(!stale.status.success());
    assert_eq!(
        api(root.path(), json!({"op":"get","workspace":id}))["workspace"],
        response["workspace"]
    );
}

#[test]
fn actions_allow_noops_and_saves_but_reject_publication_journal_injection() {
    let (root, id) = fixture();
    let response = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":0,
            "actions":[
                {"action":"viewed","path":"file.rs","viewed":false},
                {"action":"save","body":"New reply","target":{"kind":"reply","target":"root"}},
                {"action":"begin_publication","destination":"standalone","operations":[]},
                {"action":"viewed","path":"file.rs","viewed":true}
            ]
        }),
    );
    assert_eq!(response["applied"], 2);
    assert_eq!(response["failed_index"], 2);
    assert_eq!(response["workspace"]["generation"], 1);
    assert!(response["comment"].is_string());
    assert_eq!(response["comment"], response["workspace"]["saved_comment"]);
    assert!(response["workspace"]["attempt"].is_null());
    assert_eq!(response["workspace"]["files"][0]["viewed"], false);
    assert_eq!(response["workspace"]["stats"]["pending"]["comments"], 1);
    let injection = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":1,"actions":[{"action":"finish_publication","success":true,"uncertain":false,"error":null}]}),
    );
    assert_eq!(injection["applied"], 0);
    assert_eq!(injection["failed_index"], 0);
    assert!(
        injection["error"]
            .as_str()
            .unwrap()
            .contains("not local UI actions")
    );
    assert_eq!(injection["workspace"], response["workspace"]);
    let empty = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":1,"actions":[]}),
    );
    assert_eq!(empty["applied"], 0);
    assert_eq!(empty["workspace"], response["workspace"]);
}

#[test]
fn scalar_mutations_unknown_operations_and_malformed_lists_leave_state_unchanged() {
    let (root, id) = fixture();
    let before = api(root.path(), json!({"op":"get","workspace":id}))["workspace"].clone();
    let before_log = Store::open(root.path())
        .unwrap()
        .workspace_log(&id)
        .unwrap();
    for request in [
        json!({"op":"save","workspace":id,"generation":0,"body":"Rejected reply","target":{"kind":"reply","target":"root"}}),
        json!({"op":"viewed","workspace":id,"generation":0,"path":"file.rs","viewed":true}),
        json!({"op":"finish_publication","workspace":id,"generation":0,"success":true,"uncertain":false,"error":null}),
        json!({"op":"unknown","workspace":id,"generation":0}),
        json!({"op":"actions","workspace":id,"generation":0}),
        json!({"op":"actions","workspace":id,"generation":0,"actions":null}),
        json!({"op":"actions","workspace":id,"generation":0,"actions":{"action":"leave"}}),
        json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"viewed","path":"file.rs","viewed":true},{"action":"unknown"}]}),
        json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"viewed","path":"file.rs","viewed":true},{"action":"save","body":4}]}),
    ] {
        let rejected = command(root.path(), &request);
        assert!(
            !rejected.status.success(),
            "accepted malformed request: {request}"
        );
        assert_eq!(
            api(root.path(), json!({"op":"get","workspace":id}))["workspace"],
            before,
            "malformed request changed workspace: {request}"
        );
        assert_eq!(
            Store::open(root.path())
                .unwrap()
                .workspace_log(&id)
                .unwrap(),
            before_log,
            "malformed request changed history: {request}"
        );
    }
}

#[test]
fn freshness_is_validated_against_selected_snapshot() {
    let (root, id) = fixture();
    let selected = api(
        root.path(),
        json!({"op":"select","batch":id,"fresh_review":"snapshot"}),
    );
    assert_eq!(selected["fresh"], true);
    let changed = api(
        root.path(),
        json!({"op":"select","batch":id,"fresh_review":"other-snapshot"}),
    );
    assert_eq!(changed["fresh"], false);
    let cached = api(root.path(), json!({"op":"select","batch":id}));
    assert_eq!(cached["fresh"], false);
}

#[test]
fn publication_projection_matches_full_history_through_uncertainty_and_provider_updates() {
    let (root, id) = fixture();
    let mut store = Store::open(root.path()).unwrap();
    let mut workspace = store.workspace(&id).unwrap().unwrap();
    let compare = |store: &Store, workspace: &buf_review::workspace::Workspace| {
        let full = buf_review::sessions::publication_details(
            &store.batch_history(&workspace.batch.id).unwrap(),
            workspace,
        );
        assert_eq!(
            serde_json::to_value(store.publication_history(workspace).unwrap()).unwrap(),
            serde_json::to_value(full).unwrap()
        );
    };
    for action in [
        Action::Save {
            comment: None,
            target: Some(CommentTarget::Reply(CommentId("root".into()))),
            body: "Reply before uncertain publication".into(),
            destination: Some(Destination::Standalone),
        },
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    ] {
        workspace = store
            .mutate_workspace(&id, workspace.generation, action)
            .unwrap();
    }
    let plan = workspace.publication_plan().unwrap().operations;
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Standalone,
                operations: plan.clone(),
            },
        )
        .unwrap();
    compare(&store, &workspace);
    workspace = store
        .update_workspace(&id, workspace.generation, |workspace| {
            workspace.attempt.as_mut().unwrap().provider =
                json!({"prepared":{"private":"not published"}});
            workspace.last_error = Some("connection lost".into());
            Ok(())
        })
        .unwrap();
    compare(&store, &workspace);
    workspace = store
        .mutate_workspace(&id, workspace.generation, Action::SuspendPublication)
        .unwrap();
    compare(&store, &workspace);
    let attempt_id = workspace.suspended_attempts[0].id.clone();
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::ResumePublication { attempt_id },
        )
        .unwrap();
    compare(&store, &workspace);
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::RecordResult {
                result: OperationResult {
                    operation: plan[0].clone(),
                    mappings: vec![],
                    response: json!({"id":777}),
                },
            },
        )
        .unwrap();
    compare(&store, &workspace);
    workspace = store
        .mutate_workspace(
            &id,
            workspace.generation,
            Action::FinishPublication {
                success: false,
                uncertain: false,
                error: Some("file progress failed".into()),
            },
        )
        .unwrap();
    compare(&store, &workspace);
    // Plain CLI receipts have no publication attempt; they remain in the
    // recorded group, while previous attempt receipts keep their attribution.
    store
        .record_operation_result(&workspace.batch.id, &plan[1], &[], &json!({"done":true}))
        .unwrap();
    workspace = store.workspace(&id).unwrap().unwrap();
    compare(&store, &workspace);
    assert_eq!(store.publication_history(&workspace).unwrap().len(), 2);
}

#[test]
fn ordered_actions_resolve_contextual_undo_after_each_preceding_write() {
    let (root, id) = fixture();
    let response = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":0,
            "actions":[
                {"action":"reaction","comment":"root","present":true},
                {"action":"undo","comments":["root","root"]},
                {"action":"viewed","path":"file.rs","viewed":true}
            ]
        }),
    );
    assert_eq!(response["applied"], 3);
    assert!(response["error"].is_null());
    assert_eq!(response["workspace"]["done"], json!([]));
    assert_eq!(response["workspace"]["files"][0]["viewed"], true);

    let created = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":response["workspace"]["generation"],
            "actions":[{"action":"save","body":"Disposable draft","target":{"kind":"pull_request"}}]
        }),
    );
    let draft = &created["comment"];
    let preflight = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":created["workspace"]["generation"],
            "actions":[{"action":"undo","comments":[draft,"unknown"]}]
        }),
    );
    assert_eq!(preflight["applied"], 0);
    assert_eq!(preflight["failed_index"], 0);
    assert_eq!(preflight["workspace"], created["workspace"]);
    let canceled = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":created["workspace"]["generation"],
            "actions":[
                {"action":"undo","comments":[draft]},
                {"action":"undo","comments":[draft]},
                {"action":"delete_comments","comments":[draft]},
                {"action":"undo","comments":["unknown"]},
                {"action":"viewed","path":"file.rs","viewed":false}
            ]
        }),
    );
    assert_eq!(canceled["applied"], 3);
    assert_eq!(canceled["failed_index"], 3);
    assert!(canceled["error"].is_string());
    assert_eq!(canceled["workspace"]["files"][0]["viewed"], true);
    assert_eq!(
        canceled["workspace"]["comments"].as_array().unwrap().len(),
        1
    );
    let store = Store::open(root.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    assert!(workspace.completed.is_empty());
    assert!(workspace.results.is_empty());
    assert_eq!(
        store.workspace_log(&id).unwrap().last().unwrap()["action"]["action"],
        "undo"
    );
}

#[test]
fn ordered_actions_can_cancel_replies_before_their_parent() {
    let (root, id) = fixture();
    let comparison = Store::open(root.path())
        .unwrap()
        .workspace(&id)
        .unwrap()
        .unwrap()
        .comparison;
    let parent = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":0,"actions":[{"action":"save","body":"Parent","target":{"kind":"diff","target":{
            "revision":comparison,"path":"file.rs","range":{
                "start":{"side":"head","line":1},"end":{"side":"head","line":1}}}}}]}),
    );
    let parent_id = parent["comment"].as_str().unwrap();
    let child = api(
        root.path(),
        json!({"op":"actions","workspace":id,"generation":parent["workspace"]["generation"],"actions":[{"action":"save","body":"Child","target":{"kind":"reply","target":parent_id}}]}),
    );
    let child_id = child["comment"].as_str().unwrap();
    let failed = api(
        root.path(),
        json!({"op":"actions","workspace":id,
        "generation":child["workspace"]["generation"],
        "actions":[{"action":"delete","comment":parent_id},{"action":"delete","comment":child_id}]}),
    );
    assert_eq!(failed["applied"], 0);
    assert_eq!(failed["failed_index"], 0);
    assert_eq!(failed["workspace"], child["workspace"]);
    let deleted = api(
        root.path(),
        json!({"op":"actions","workspace":id,
        "generation":child["workspace"]["generation"],
        "actions":[{"action":"delete","comment":child_id},{"action":"delete","comment":parent_id}]}),
    );
    assert_eq!(deleted["applied"], 2);
    assert!(deleted["error"].is_null());
    assert_eq!(deleted["workspace"]["stats"]["pending"]["comments"], 0);
    assert_eq!(
        deleted["workspace"]["comments"].as_array().unwrap().len(),
        1
    );
    assert_eq!(deleted["workspace"]["id"], id);
}

#[cfg(unix)]
#[test]
fn cold_routes_and_explicit_new_sessions_report_the_fetched_snapshot_once() {
    use std::os::unix::fs::PermissionsExt;
    let (root, id) = fixture();
    let original = Store::open(root.path())
        .unwrap()
        .workspace(&id)
        .unwrap()
        .unwrap();
    let bin = root.path().join("cold-bin");
    fs::create_dir(&bin).unwrap();
    let empty =
        json!({"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}});
    let pull = json!({"id":"PR30","fullDatabaseId":"30","title":"Cold snapshot","body":"",
        "author":null,"state":"OPEN","baseRefOid":original.comparison.base,
        "headRefOid":original.comparison.head,"headRefName":"feature","headRepository":{"url":"https://github.com/acme/repo"},"changedFiles":1,
        "files":{"totalCount":1,"nodes":[{"path":"file.rs","changeType":"MODIFIED","viewerViewedState":"UNVIEWED"}],
            "pageInfo":{"hasNextPage":false,"endCursor":null}},
        "comments":empty,"reviews":empty,"reviewThreads":empty});
    fs::write(
        bin.join("snapshot.json"),
        serde_json::to_vec(&json!({"data":{
        "viewer":{"login":"Reviewer","databaseId":10},"repository":{"pullRequest":pull}}}))
        .unwrap(),
    )
    .unwrap();
    fs::write(bin.join("pull.json"),serde_json::to_vec(&json!({"id":30,
        "base":{"sha":original.comparison.base},"head":{"sha":original.comparison.head},"changed_files":1})).unwrap()).unwrap();
    fs::write(
        bin.join("files.json"),
        serde_json::to_vec(
            &json!([[{"filename":"file.rs","status":"modified","additions":12,"deletions":3}]]),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(bin.join("user.json"), r#"{"id":10,"login":"Reviewer"}"#).unwrap();
    fs::write(
        bin.join("gh"),
        r#"#!/bin/sh
fixture_dir=$(dirname "$0")
printf '%s\n' "$2" >> "$fixture_dir/calls"
case "$2" in
  graphql)
    python3 -c 'import json,sys; request=json.load(sys.stdin); data=json.load(open(sys.argv[1])); number=request["variables"]["number"]; pull=data["data"]["repository"]["pullRequest"]; pull["fullDatabaseId"]=str(30+number); pull["id"]="PR"+str(30+number); print(json.dumps(data))' "$fixture_dir/snapshot.json"
    exit ;;
  repos/example/repo/pulls/*/files*) fixture=files ;;
  repos/example/repo/pulls/*) fixture=pull ;;
  user) fixture=user ;;
  *) printf 'Unexpected endpoint: %s' "$2" >&2; exit 1 ;;
esac
cat "$fixture_dir/$fixture.json"
"#,
    )
    .unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let call = |request| {
        let output = command_with_path(root.path(), &request, Some(&path));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let route = call(json!({"op":"route","url":"https://github.com/example/repo/pull/2"}));
    assert!(route["fresh_review"].is_string());
    let selected =
        call(json!({"op":"select","batch":route["batch"],"fresh_review":route["fresh_review"]}));
    assert_eq!(selected["fresh"], true);
    assert_eq!(selected["workspace"]["review"]["id"], route["fresh_review"]);
    let created = call(json!({"op":"new","url":"https://github.com/example/repo/pull/3"}));
    assert_eq!(created["fresh"], true);
    let cached = call(json!({"op":"new","url":"https://github.com/example/repo/pull/3"}));
    assert_eq!(cached["fresh"], false);
    assert_ne!(cached["workspace"]["id"], created["workspace"]["id"]);
    let calls = fs::read_to_string(bin.join("calls")).unwrap();
    assert_eq!(calls.lines().filter(|line| *line == "graphql").count(), 2);
}

#[test]
fn queued_conclusion_is_local_and_legacy_submit_cannot_write() {
    let (root, id) = fixture();
    let response = api(
        root.path(),
        json!({
            "op":"actions", "workspace":id, "generation":0,
            "actions":[
                {"action":"save", "target":{"kind":"reply","target":"root"}, "body":"Review reply"},
                {"action":"publish_review", "conclusion":{"outcome":"approve","body":null}}
            ]
        }),
    );
    assert!(response["error"].is_null(), "{response}");
    let workspace = Store::open(root.path())
        .unwrap()
        .workspace(&id)
        .unwrap()
        .unwrap();
    let plan = workspace.publication_plan().unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert!(matches!(
        plan.operations[0].change,
        Event::CreateComment { .. }
    ));
    assert!(matches!(
        plan.operations[1].change,
        Event::SetReviewConclusion { .. }
    ));
    assert!(workspace.results.is_empty() && workspace.attempt.is_none());
    use std::os::unix::fs::PermissionsExt;
    let bin = root.path().join("no-provider");
    fs::create_dir(&bin).unwrap();
    fs::write(
        bin.join("gh"),
        "#!/bin/sh\nprintf 'unexpected provider invocation' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let rejected = command_with_path(
        root.path(),
        &json!({
            "op":"submit", "workspace":id, "generation":workspace.generation
        }),
        Some(&path),
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("only publish may write"));
    assert_eq!(
        Store::open(root.path())
            .unwrap()
            .workspace(&id)
            .unwrap()
            .unwrap(),
        workspace
    );
}
