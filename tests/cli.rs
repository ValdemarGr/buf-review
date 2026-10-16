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
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "file.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
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

#[test]
fn explicit_batches_preserve_log_while_plans_collapse_cancelled_changes() {
    let project = tempdir().unwrap();
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
    for event in &events {
        let input = serde_json::to_string(event).unwrap();
        let retained: Event = serde_json::from_str(&success(run(
            project.path(),
            &["stage", &first],
            Some(&input),
        )))
        .unwrap();
        assert_eq!(&retained, event);
    }
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
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .create_review(&review, &reviewer(), &baseline())
        .unwrap();
    store.create_batch(&batch, &review).unwrap();
    for input in [
        "{",
        "{\"kind\":\"set_file_viewed\",\"path\":\"file.rs\",\"viewed\":true} {}",
    ] {
        let result = run(project.path(), &["stage", "batch"], Some(input));
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
fn partial_commit_retry_skips_success_and_later_edit_uses_saved_mapping() {
    use buf_review::store::{ReviewBackend, ReviewSource};
    use std::{fs, os::unix::fs::PermissionsExt};

    let project = tempdir().unwrap();
    let tools = tempdir().unwrap();
    let executable = tools.path().join("gh");
    fs::write(
        &executable,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
body=$(cat)
printf '%s\n' "$body" >> "$FAKE_GH_LOG"
case "$*" in
  *"--method POST"*)
    case "$body" in
      *'"body":"First"'*) printf '{"id":101}\n' ;;
      *'"body":"Second"'*)
        if [ -f "$FAKE_GH_FAIL" ]; then printf 'second failed\n' >&2; exit 1; fi
        printf '{"id":102}\n'
        ;;
      *) printf 'unexpected create\n' >&2; exit 1 ;;
    esac
    ;;
  *"repos/acme/repo/issues/comments/101"*"--method PATCH"*) printf '{"id":101}\n' ;;
  *) printf 'unexpected request\n' >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let log = tools.path().join("requests");
    let fail = tools.path().join("fail");
    fs::write(&fail, "").unwrap();
    let path = std::env::join_paths(
        std::iter::once(tools.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let commit = || {
        Command::new(env!("CARGO_BIN_EXE_pr"))
            .current_dir(project.path())
            .args(["commit", "batch"])
            .env("PATH", &path)
            .env("FAKE_GH_LOG", &log)
            .env("FAKE_GH_FAIL", &fail)
            .output()
            .unwrap()
    };
    let id = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .import_review(
            &id,
            &reviewer(),
            &baseline(),
            &ReviewBackend {
                source: ReviewSource {
                    backend: "github".into(),
                    locator: "https://github.com/acme/repo/pull/7".into(),
                },
                mappings: Vec::new(),
            },
        )
        .unwrap();
    store.create_batch(&batch, &id).unwrap();
    for (id, body) in [("first", "First"), ("second", "Second")] {
        store
            .append_event(
                &batch,
                &Event::CreateComment {
                    comment: DraftComment {
                        content: CommentContent {
                            id: CommentId(id.into()),
                            body: body.into(),
                        },
                        author: reviewer(),
                        target: CommentTarget::PullRequest,
                    },
                },
            )
            .unwrap();
    }
    let failure = commit();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    assert!(
        String::from_utf8(failure.stderr)
            .unwrap()
            .contains("second failed")
    );
    let receipts = store.operation_results(&batch).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].mappings[0].core_id, "first");
    assert_eq!(receipts[0].mappings[0].remote_id, "101");
    fs::remove_file(&fail).unwrap();
    let successful: Value = serde_json::from_str(&success(commit())).unwrap();
    assert_eq!(successful.as_array().unwrap().len(), 2);
    let requests = fs::read_to_string(&log).unwrap();
    assert_eq!(requests.matches("\"body\":\"First\"").count(), 1);
    assert_eq!(requests.matches("\"body\":\"Second\"").count(), 2);
    let before = requests;
    success(commit());
    assert_eq!(fs::read_to_string(&log).unwrap(), before);
    store
        .append_event(
            &batch,
            &Event::EditComment {
                comment: CommentId("first".into()),
                body: "Updated first".into(),
            },
        )
        .unwrap();
    let updated: Value = serde_json::from_str(&success(commit())).unwrap();
    assert_eq!(updated.as_array().unwrap().len(), 3);
    let requests = fs::read_to_string(&log).unwrap();
    assert!(requests.contains("repos/acme/repo/issues/comments/101"));
    assert_eq!(requests.matches("--method PATCH").count(), 1);
    assert_eq!(
        store.get_batch(&batch).unwrap().unwrap().write.events.len(),
        3
    );
}

#[test]
fn concurrent_commits_share_completed_receipt_before_second_publish() {
    use buf_review::store::{ReviewBackend, ReviewSource};
    use std::{
        fs::{self, File, TryLockError},
        os::unix::fs::PermissionsExt,
        thread,
        time::{Duration, Instant},
    };

    let project = tempdir().unwrap();
    let tools = tempdir().unwrap();
    let executable = tools.path().join("gh");
    fs::write(
        &executable,
        r#"#!/bin/sh
cat > "$FAKE_GH_BODY"
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
: > "$FAKE_GH_READY"
while [ ! -f "$FAKE_GH_GATE" ]; do sleep 0.01; done
printf '{"id":101}\n'
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let log = tools.path().join("requests");
    let ready = tools.path().join("ready");
    let gate = tools.path().join("gate");
    let path = std::env::join_paths(
        std::iter::once(tools.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let id = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .import_review(
            &id,
            &reviewer(),
            &baseline(),
            &ReviewBackend {
                source: ReviewSource {
                    backend: "github".into(),
                    locator: "https://github.com/acme/repo/pull/7".into(),
                },
                mappings: Vec::new(),
            },
        )
        .unwrap();
    store.create_batch(&batch, &id).unwrap();
    store
        .append_event(
            &batch,
            &Event::CreateComment {
                comment: DraftComment {
                    content: CommentContent {
                        id: CommentId("draft".into()),
                        body: "Only once".into(),
                    },
                    author: reviewer(),
                    target: CommentTarget::PullRequest,
                },
            },
        )
        .unwrap();
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_pr"))
            .current_dir(project.path())
            .args(["commit", "batch"])
            .env("PATH", &path)
            .env("FAKE_GH_BODY", tools.path().join("body"))
            .env("FAKE_GH_LOG", &log)
            .env("FAKE_GH_READY", &ready)
            .env("FAKE_GH_GATE", &gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = spawn();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        if Instant::now() >= deadline || first.try_wait().unwrap().is_some() {
            fs::write(&gate, "").unwrap();
            first.kill().ok();
            panic!("first commit did not reach paused provider request");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let lock_probe = File::open(project.path().join(".review/state.sqlite3")).unwrap();
    let locked = lock_probe.try_lock();
    if !matches!(locked, Err(TryLockError::WouldBlock)) {
        fs::write(&gate, "").unwrap();
        first.kill().ok();
        panic!("first commit must hold database OS lock across provider request");
    }
    let mut second = spawn();
    let second_running = second.try_wait().unwrap().is_none();
    let receipts = store.operation_results(&batch).unwrap();
    fs::write(&gate, "").unwrap();
    assert!(second_running);
    assert!(receipts.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while first.try_wait().unwrap().is_none() || second.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            first.kill().ok();
            second.kill().ok();
            panic!("concurrent commit processes did not finish");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let first: Value = serde_json::from_str(&success(first.wait_with_output().unwrap())).unwrap();
    let second: Value = serde_json::from_str(&success(second.wait_with_output().unwrap())).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.as_array().unwrap().len(), 1);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    assert_eq!(store.operation_results(&batch).unwrap().len(), 1);
}
