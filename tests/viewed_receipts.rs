use std::{fs, path::Path, process::Command};

use buf_review::{
    core::Operation,
    model::*,
    store::{OperationResult, Store},
    workspace::{Action, Destination, Workspace},
};
use serde_json::json;
use tempfile::{TempDir, tempdir};

fn git(project: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(project)
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

fn fixture() -> (TempDir, Store, Workspace) {
    let project = tempdir().unwrap();
    git(project.path(), &["init", "--quiet"]);
    git(project.path(), &["config", "user.name", "Reviewer"]);
    git(
        project.path(),
        &["config", "user.email", "reviewer@example.invalid"],
    );
    for path in ["a", "b"] {
        fs::write(project.path().join(path), "base\n").unwrap();
    }
    git(project.path(), &["add", "a", "b"]);
    git(project.path(), &["commit", "--quiet", "-m", "Base"]);
    let base = git(project.path(), &["rev-parse", "HEAD"]);
    for path in ["a", "b"] {
        fs::write(project.path().join(path), "head\n").unwrap();
    }
    git(project.path(), &["commit", "--quiet", "-am", "Head"]);
    let head = git(project.path(), &["rev-parse", "HEAD"]);
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Viewed receipts".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision { base, head },
        },
        files: ["a", "b"]
            .into_iter()
            .map(|path| ChangedFile {
                path: path.into(),
                previous_path: None,
                change: FileChange::Modified,
                stats: None,
                viewed: false,
            })
            .collect(),
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    };
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("initial".into());
    store.create_review(&review, &reviewer, &read).unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    (project, store, workspace)
}

fn mark(store: &mut Store, workspace: Workspace, path: &str, viewed: bool) -> Workspace {
    store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Viewed {
                path: path.into(),
                viewed,
            },
        )
        .unwrap()
}

fn toggles(store: &mut Store, mut workspace: Workspace, raw: bool) -> Workspace {
    for viewed in [true, false, true] {
        if raw {
            store
                .append_event(
                    &workspace.batch.id,
                    &Event::SetFileViewed {
                        path: "a".into(),
                        viewed,
                        commit: None,
                    },
                )
                .unwrap();
            workspace = store.workspace(&workspace.id).unwrap().unwrap();
        } else {
            workspace = mark(store, workspace, "a", viewed);
        }
    }
    workspace
}

fn publish_latest(store: &mut Store, workspace: Workspace) -> (Workspace, Operation) {
    let operations = workspace.publication_plan().unwrap().operations;
    assert_eq!(operations.len(), 1);
    let operation = operations.into_iter().next().unwrap();
    assert!(
        matches!(&operation.change, Event::SetFileViewed { path, viewed: true, .. } if path == "a")
    );
    store
        .record_operation_result(
            &workspace.batch.id,
            &operation,
            &[],
            &json!({"confirmed":true}),
        )
        .unwrap();
    (store.workspace(&workspace.id).unwrap().unwrap(), operation)
}

fn refreshed(store: &mut Store, workspace: Workspace, name: &str) -> Workspace {
    let mut read = workspace.review.read.clone();
    read.files
        .iter_mut()
        .find(|file| file.path == "a")
        .unwrap()
        .viewed = true;
    let review = ReviewId(name.into());
    store
        .create_review(&review, &workspace.review.reviewer, &read)
        .unwrap();
    store
        .refresh_workspace(&workspace.id, workspace.generation, &review)
        .unwrap()
}

#[test]
fn confirmed_latest_viewed_does_not_expose_an_older_opposite_toggle_after_refresh_or_restart() {
    for raw in [false, true] {
        let (project, mut store, workspace) = fixture();
        let workspace = toggles(&mut store, workspace, raw);
        let raw = workspace.batch.write.events.clone();
        let (mut workspace, receipt) = publish_latest(&mut store, workspace);
        assert!(workspace.pending().unwrap().operations.is_empty());
        for cycle in 0..3 {
            workspace = refreshed(&mut store, workspace, &format!("remote-{cycle}"));
            assert!(
                workspace.pending().unwrap().operations.is_empty(),
                "refresh exposed older toggle: {:?}",
                workspace.pending().unwrap()
            );
            assert!(
                workspace
                    .projected()
                    .unwrap()
                    .files
                    .iter()
                    .find(|file| file.path == "a")
                    .unwrap()
                    .viewed
            );
            assert_eq!(workspace.batch.write.events, raw);
            assert_eq!(workspace.results[0].operation, receipt);
            let id = workspace.id.clone();
            drop(store);
            store = Store::open(project.path()).unwrap();
            workspace = store.workspace(&id).unwrap().unwrap();
            assert!(workspace.publication_plan().unwrap().operations.is_empty());
            assert_eq!(workspace.batch.write.events, raw);
            assert_eq!(workspace.results[0].operation, receipt);
        }
    }
}

#[test]
fn explicit_opposite_toggle_after_a_receipt_is_new_unpublished_intent() {
    let (project, mut store, workspace) = fixture();
    let workspace = toggles(&mut store, workspace, false);
    let (workspace, receipt) = publish_latest(&mut store, workspace);
    let workspace = mark(&mut store, workspace, "a", false);
    let raw = workspace.batch.write.events.clone();
    assert_eq!(raw.len(), 4);
    assert!(
        matches!(&raw[3].event, Event::SetFileViewed { path, viewed: false, .. } if path == "a")
    );
    let pending = workspace.pending().unwrap().operations;
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].change, Event::SetFileViewed { path, viewed: false, .. } if path == "a")
    );
    let workspace = refreshed(&mut store, workspace, "remote-still-viewed");
    let id = workspace.id.clone();
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let pending = workspace.publication_plan().unwrap().operations;
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].change, Event::SetFileViewed { path, viewed: false, .. } if path == "a")
    );
    assert_eq!(workspace.batch.write.events, raw);
    assert_eq!(workspace.results[0].operation, receipt);
}

#[test]
fn failed_unrelated_viewed_operation_stays_pending_without_reviving_completed_path() {
    let (project, mut store, workspace) = fixture();
    let workspace = toggles(&mut store, workspace, false);
    let workspace = mark(&mut store, workspace, "b", true);
    let raw = workspace.batch.write.events.clone();
    let operations = workspace.publication_plan().unwrap().operations;
    assert_eq!(operations.len(), 2);
    let mut workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Review,
                operations: operations.clone(),
            },
        )
        .unwrap();
    workspace = store
        .mutate_workspace(
            &workspace.id,
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
    workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::FinishPublication {
                success: false,
                uncertain: false,
                error: Some("b rejected".into()),
            },
        )
        .unwrap();
    let publication = workspace.suspended_attempts[0].id.clone();
    workspace = refreshed(&mut store, workspace, "remote-only-a");
    let id = workspace.id.clone();
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let pending = workspace.pending().unwrap().operations;
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].change, Event::SetFileViewed { path, viewed: true, .. } if path == "b")
    );
    assert_eq!(workspace.results[0].operation, operations[0]);
    assert_eq!(workspace.suspended_attempts[0].id, publication);
    assert_eq!(workspace.batch.write.events, raw);
}
