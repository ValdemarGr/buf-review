use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use buf_review::{model::*, store::Store};
use serde_json::{Value, json};
use tempfile::tempdir;
use uuid::Uuid;

fn reviewer() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn baseline() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pr".into()),
            title: "Review".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "file.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
            viewed: false,
        }],
        comments: Vec::new(),
        reactions: Vec::new(),
        reviews: Vec::new(),
    }
}

fn run(project: &Path, arguments: &[&str], input: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pr"))
        .current_dir(project)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn project() -> tempfile::TempDir {
    let project = tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
    project
}

fn workspace(project: &Path, request: Value) -> Value {
    serde_json::from_str(&success(run(
        project,
        &["workspace"],
        Some(&request.to_string()),
    )))
    .unwrap()
}

#[test]
fn explicit_batches_preserve_log_while_plans_collapse_cancelled_changes() {
    let project = project();
    let id = ReviewId("review".into());
    Store::open(project.path())
        .unwrap()
        .create_review(&id, &reviewer(), &baseline())
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&success(run(project.path(), &["reviews"], None))).unwrap(),
        json!(["review"])
    );
    let fetched: Review =
        serde_json::from_str(&success(run(project.path(), &["show", "review"], None))).unwrap();
    assert_eq!(fetched.read, baseline());
    let first = success(run(project.path(), &["batch", "new", "review"], None));
    Uuid::parse_str(&first).unwrap();
    let second = success(run(project.path(), &["batch", "new", "review"], None));
    assert_ne!(first, second);
    let events = [
        Event::SetFileViewed {
            path: "file.rs".into(),
            viewed: true,
        },
        Event::SetFileViewed {
            path: "file.rs".into(),
            viewed: false,
        },
    ];
    let response = workspace(
        project.path(),
        json!({
            "op":"actions", "workspace":first,"generation":0,
            "actions":[
                {"action":"viewed","path":"file.rs","viewed":true},
                {"action":"viewed","path":"file.rs","viewed":false}
            ]
        }),
    );
    assert_eq!(response["applied"], 2);
    assert!(response.get("error").is_none());
    let opened: Batch = serde_json::from_str(&success(run(
        project.path(),
        &["batch", "open", &first],
        None,
    )))
    .unwrap();
    assert_eq!(opened.review, id);
    assert_eq!(opened.write.events, events);
    let second_batch: Batch = serde_json::from_str(&success(run(
        project.path(),
        &["batch", "open", &second],
        None,
    )))
    .unwrap();
    assert!(second_batch.write.events.is_empty());
    let list: Vec<BatchId> = serde_json::from_str(&success(run(
        project.path(),
        &["batch", "list", "review"],
        None,
    )))
    .unwrap();
    assert_eq!(list.len(), 2);
    let plan: Value =
        serde_json::from_str(&success(run(project.path(), &["plan", &first], None))).unwrap();
    assert_eq!(plan["operations"], json!([]));
    let projected: Value =
        serde_json::from_str(&success(run(project.path(), &["state", &first], None))).unwrap();
    assert!(projected.is_object());
    // Planning and projecting leave every staged event intact.
    let reopened: Batch = serde_json::from_str(&success(run(
        project.path(),
        &["batch", "open", &first],
        None,
    )))
    .unwrap();
    assert_eq!(reopened.write.events, events);
}

#[test]
fn bad_json_and_unknown_ids_fail_without_appending_events() {
    let project = project();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .create_review(&review, &reviewer(), &baseline())
        .unwrap();
    store.create_batch(&batch, &review).unwrap();
    for input in [
        "{",
        "{\"op\":\"actions\",\"workspace\":\"batch\",\"generation\":0,\"actions\":[]} {}",
    ] {
        let result = run(project.path(), &["workspace"], Some(input));
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.is_empty());
    }
    assert!(
        store
            .get_batch(&batch)
            .unwrap()
            .unwrap()
            .write
            .events
            .is_empty()
    );
    for arguments in [
        vec!["show", "missing"],
        vec!["batch", "open", "missing"],
        vec!["batch", "new", "missing"],
        vec!["batch", "list", "missing"],
        vec!["state", "missing"],
        vec!["plan", "missing"],
    ] {
        let result = run(project.path(), &arguments, None);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .contains("does not exist")
        );
    }
}

#[test]
fn invalid_event_is_rejected_and_batch_remains_usable() {
    let project = project();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .create_review(&review, &reviewer(), &baseline())
        .unwrap();
    store.create_batch(&batch, &review).unwrap();
    let output = workspace(
        project.path(),
        json!({
            "op":"actions","workspace":"batch","generation":0,
            "actions":[{"action":"save","comment":"missing","body":"Edit"}]
        }),
    );
    assert_eq!(output["applied"], 0);
    assert!(output["error"].as_str().unwrap().contains("does not exist"));
    assert!(
        store
            .get_batch(&batch)
            .unwrap()
            .unwrap()
            .write
            .events
            .is_empty()
    );

    let valid = Event::SetFileViewed {
        path: "file.rs".into(),
        viewed: true,
    };
    let response = workspace(
        project.path(),
        json!({
            "op":"actions","workspace":"batch","generation":0,
            "actions":[{"action":"viewed","path":"file.rs","viewed":true}]
        }),
    );
    assert_eq!(response["applied"], 1);
    assert!(response.get("error").is_none());
    let retained = store.get_batch(&batch).unwrap().unwrap();
    assert_eq!(retained.write.events, vec![valid]);
    let state: Value =
        serde_json::from_str(&success(run(project.path(), &["state", "batch"], None))).unwrap();
    assert_eq!(state["files"][0]["viewed"], true);
    let plan: Value =
        serde_json::from_str(&success(run(project.path(), &["plan", "batch"], None))).unwrap();
    assert_eq!(plan["operations"].as_array().unwrap().len(), 1);
}

#[test]
fn cli_plan_after_editor_refresh_uses_current_delta_not_historical_receipts() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let old = ReviewId("old".into());
    store.create_review(&old, &reviewer(), &baseline()).unwrap();
    let batch = BatchId("batch".into());
    store.create_batch(&batch, &old).unwrap();
    let comment = DraftComment {
        content: CommentContent {
            id: CommentId("own".into()),
            body: "sent".into(),
        },
        author: reviewer(),
        target: CommentTarget::PullRequest,
    };
    store
        .append_event(
            &batch,
            &Event::CreateComment {
                comment: comment.clone(),
            },
        )
        .unwrap();
    let w = store.workspace(&batch.0).unwrap().unwrap();
    let operation = &w.pending().unwrap().operations[0];
    store
        .record_operation_result(&batch, operation, &[], &json!({"id":1}))
        .unwrap();
    let w = store.workspace(&batch.0).unwrap().unwrap();
    let fresh = ReviewId("fresh".into());
    let mut read = baseline();
    read.comments.push(ReadComment {
        content: comment.content,
        author: Some(reviewer()),
        target: CommentTarget::PullRequest,
    });
    store.create_review(&fresh, &reviewer(), &read).unwrap();
    store
        .refresh_workspace(&w.id, w.generation, &fresh)
        .unwrap();
    let plan: Value =
        serde_json::from_str(&success(run(project.path(), &["plan", &batch.0], None))).unwrap();
    assert_eq!(plan["operations"], json!([]));
    success(run(project.path(), &["state", &batch.0], None));
}

#[test]
fn removed_write_commands_are_rejected_before_opening_the_database() {
    for command in ["stage", "commit"] {
        let project = tempdir().unwrap();
        let result = run(project.path(), &[command, "batch"], Some("{}"));
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("unrecognized subcommand"));
        assert!(!project.path().join(".review").exists());
    }
}
