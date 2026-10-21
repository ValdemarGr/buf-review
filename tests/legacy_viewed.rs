use std::{fs, path::Path, process::Command};

use buf_review::{model::*, store::Store, workspace::Action};
use rusqlite::Connection;
use tempfile::tempdir;

fn git(project: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(project)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn commit(project: &Path, contents: &str) -> String {
    fs::write(project.join("file.rs"), contents).unwrap();
    git(project, &["add", "file.rs"]);
    git(
        project,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            contents,
        ],
    );
    git(project, &["rev-parse", "HEAD"])
}

fn baseline(base: &str, head: &str) -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Legacy viewed marks".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: base.into(),
                head: head.into(),
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
    }
}

#[test]
fn repeated_legacy_marks_recover_distinct_original_commits_through_refresh() {
    let project = tempdir().unwrap();
    git(project.path(), &["init", "-q"]);
    let base = commit(project.path(), "base");
    let head_a = commit(project.path(), "A");
    let head_b = commit(project.path(), "B");
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let mut store = Store::open(project.path()).unwrap();
    let review_a = ReviewId("snapshot-a".into());
    let review_b = ReviewId("snapshot-b".into());
    store
        .create_review(&review_a, &reviewer, &baseline(&base, &head_a))
        .unwrap();
    let mut workspace = store.open_workspace(&review_a).unwrap();
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
    store
        .create_review(&review_b, &reviewer, &baseline(&base, &head_b))
        .unwrap();
    workspace = store
        .refresh_workspace(&workspace.id, workspace.generation, &review_b)
        .unwrap();
    let original_operation = workspace.pending().unwrap().operations[0].id.clone();
    workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert!(!workspace.projected().unwrap().files[0].viewed);
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
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Viewed {
                path: "file.rs".into(),
                viewed: false,
            },
        )
        .unwrap();
    let batch = workspace.id.clone();
    drop(store);

    // Reproduce pre-stamp event storage without rewriting comparison history.
    let connection = Connection::open(project.path().join(".review/state.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE write_events SET event=json_remove(event,'$.commit') WHERE batch_id=?1",
            [&batch],
        )
        .unwrap();
    drop(connection);

    let mut store = Store::open(project.path()).unwrap();
    let mut workspace = store.workspace(&batch).unwrap().unwrap();
    assert_eq!(workspace.batch.write.events.len(), 3);
    assert_eq!(workspace.legacy_viewed_commits.get(&0), Some(&head_a));
    assert_eq!(workspace.legacy_viewed_commits.get(&1), Some(&head_b));
    assert_eq!(workspace.legacy_viewed_commits.get(&2), Some(&head_b));
    assert!(
        workspace
            .batch
            .write
            .events
            .iter()
            .all(|event| matches!(event, Event::SetFileViewed { commit: None, .. }))
    );
    assert!(!workspace.projected().unwrap().files[0].viewed);
    assert!(workspace.pending().unwrap().operations.is_empty());

    workspace = store
        .compare_saved(&batch, workspace.generation, &head_a)
        .unwrap();
    assert!(workspace.projected().unwrap().files[0].viewed);
    assert_eq!(workspace.pending().unwrap().operations.len(), 1);
    assert_eq!(
        workspace.pending().unwrap().operations[0].id,
        original_operation
    );
    workspace = store
        .compare_saved(&batch, workspace.generation, &head_b)
        .unwrap();
    assert!(!workspace.projected().unwrap().files[0].viewed);
    assert!(workspace.pending().unwrap().operations.is_empty());
    assert_eq!(workspace.batch.write.events.len(), 3);
}

#[test]
fn raw_legacy_mark_at_its_original_baseline_remains_readable() {
    let project = tempdir().unwrap();
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("snapshot".into());
    store
        .create_review(&review, &reviewer, &baseline("base", "head"))
        .unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    let event = Event::SetFileViewed {
        path: "file.rs".into(),
        viewed: true,
        commit: None,
    };
    store.append_event(&workspace.batch.id, &event).unwrap();
    let recovered = store.workspace(&workspace.id).unwrap().unwrap();
    assert_eq!(
        recovered.legacy_viewed_commits.get(&0).map(String::as_str),
        Some("head")
    );
    assert!(recovered.projected().unwrap().files[0].viewed);
    assert_eq!(recovered.batch.write.events, vec![event]);
}

#[test]
fn stale_legacy_mark_without_origin_evidence_is_not_assigned_current_head() {
    let project = tempdir().unwrap();
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("snapshot".into());
    store
        .create_review(&review, &reviewer, &baseline("base", "head"))
        .unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    let workspace = store
        .update_workspace(&workspace.id, workspace.generation, |workspace| {
            workspace.comparison.head = "another-head".into();
            Ok(())
        })
        .unwrap();
    let event = Event::SetFileViewed {
        path: "file.rs".into(),
        viewed: true,
        commit: None,
    };
    store.append_event(&workspace.batch.id, &event).unwrap();
    let recovered = store.workspace(&workspace.id).unwrap().unwrap();
    assert!(!recovered.legacy_viewed_commits.contains_key(&0));
    assert!(
        recovered
            .pending()
            .unwrap_err()
            .to_string()
            .contains("viewed mark commit unavailable")
    );
    assert_eq!(recovered.batch.write.events, vec![event]);
}
