use std::{fs, path::Path, process::Command};

use buf_review::{
    diff,
    model::*,
    store::Store,
    workspace::{Action, Workspace},
    workspace_cli,
};
use serde_json::json;
use tempfile::{TempDir, tempdir};

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().into()
}

fn commit(root: &Path, path: &str, text: &str, subject: &str) -> String {
    fs::write(root.join(path), text).unwrap();
    git(root, &["add", path]);
    git(root, &["commit", "--quiet", "-m", subject]);
    git(root, &["rev-parse", "HEAD"])
}

fn snapshot(root: &Path, base: &str, head: &str) -> ReadState {
    let revision = DiffRevision {
        base: base.into(),
        head: head.into(),
    };
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pr".into()),
            title: "Review".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: revision.clone(),
        },
        files: diff::files(root, &revision).unwrap(),
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    }
}

fn fixture() -> (TempDir, Store, Workspace, String, String) {
    let root = tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["config", "user.name", "Reviewer"]);
    git(
        root.path(),
        &["config", "user.email", "reviewer@example.invalid"],
    );
    commit(root.path(), "a", "base\n", "Base A");
    let base = commit(root.path(), "b", "base\n", "Base B");
    commit(root.path(), "a", "reviewed\n", "Change A");
    let head = commit(root.path(), "b", "reviewed\n", "Change B");
    let mut store = Store::open(root.path()).unwrap();
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let review = ReviewId("first".into());
    store
        .create_review(&review, &reviewer, &snapshot(root.path(), &base, &head))
        .unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    (root, store, workspace, base, head)
}

fn mark(store: &mut Store, workspace: Workspace, path: &str) -> Workspace {
    store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::Viewed {
                path: path.into(),
                viewed: true,
            },
        )
        .unwrap()
}

fn refresh(
    store: &mut Store,
    workspace: Workspace,
    root: &Path,
    base: &str,
    head: &str,
    id: &str,
) -> Workspace {
    let review = ReviewId(id.into());
    store
        .create_review(
            &review,
            &workspace.review.reviewer,
            &snapshot(root, base, head),
        )
        .unwrap();
    store
        .refresh_workspace(&workspace.id, workspace.generation, &review)
        .unwrap()
}

#[test]
fn filtered_unpublished_matches_cli_and_publication_and_saved_history_round_trip() {
    let (root, mut store, workspace, base, old_head) = fixture();
    let workspace = mark(&mut store, workspace, "a");
    let workspace = mark(&mut store, workspace, "b");
    let raw = workspace.batch.write.clone();
    let next = commit(root.path(), "a", "new\n", "New A");
    let workspace = refresh(&mut store, workspace, root.path(), &base, &next, "second");
    assert!(
        store
            .view_workspace(&workspace)
            .unwrap()
            .files
            .iter()
            .all(|file| file.viewed)
    );
    let before = workspace.pending().unwrap();
    let view = store.view_workspace(&workspace).unwrap();
    assert_eq!(view.unpulled_commits.len(), 1);
    assert_eq!(view.unpulled_commits[0].subject, "New A");
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert_eq!(workspace.batch.write, raw);
    let view = store.view_workspace(&workspace).unwrap();
    assert!(
        !view
            .files
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .viewed
    );
    assert!(
        view.files
            .iter()
            .find(|file| file.path == "b")
            .unwrap()
            .viewed
    );
    assert_eq!(view.pending.len(), 1);
    assert_eq!(view.pending[0], before.operations[1]);
    assert_eq!(
        view.pending,
        workspace.publication_plan().unwrap().operations
    );
    let response =
        workspace_cli::run(root.path(), json!({"op":"get", "workspace":workspace.id})).unwrap();
    assert_eq!(
        response["workspace"]["pending"],
        serde_json::to_value(&view.pending).unwrap()
    );
    // Saved comparisons remain readable as history; Git checkout controls the
    // current CLI comparison instead of a public compare operation.
    let restored = store
        .compare_saved(&workspace.id, workspace.generation, &old_head[..12])
        .unwrap();
    assert_eq!(
        restored.comparison,
        DiffRevision {
            base,
            head: old_head
        }
    );
    assert_eq!(restored.batch.write, raw);
    assert!(
        store
            .view_workspace(&restored)
            .unwrap()
            .files
            .iter()
            .all(|file| file.viewed)
    );
    assert_eq!(restored.pending().unwrap().operations, before.operations);
}

#[test]
fn stale_marks_survive_another_refresh_and_restart() {
    let (root, mut store, workspace, base, old_head) = fixture();
    let workspace = mark(&mut store, workspace, "a");
    let raw = workspace.batch.write.clone();
    let next = commit(root.path(), "a", "new\n", "New A");
    let workspace = refresh(&mut store, workspace, root.path(), &base, &next, "second");
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert!(workspace.pending().unwrap().operations.is_empty());
    let workspace = refresh(&mut store, workspace, root.path(), &base, &next, "third");
    assert_eq!(workspace.batch.write, raw);
    drop(store);
    let mut store = Store::open(root.path()).unwrap();
    let restored = store
        .compare_saved(&workspace.id, workspace.generation, &old_head)
        .unwrap();
    assert_eq!(restored.batch.write, raw);
    assert!(
        store
            .view_workspace(&restored)
            .unwrap()
            .files
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .viewed
    );
}

#[test]
fn disappeared_paths_keep_marks_and_saved_base_is_restored() {
    let (root, mut store, workspace, base, old_head) = fixture();
    let workspace = mark(&mut store, workspace, "a");
    let raw = workspace.batch.write.clone();
    let next = commit(root.path(), "b", "latest\n", "New B");
    let workspace = refresh(
        &mut store,
        workspace,
        root.path(),
        &old_head,
        &next,
        "second",
    );
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert_eq!(workspace.batch.write, raw);
    assert_eq!(workspace.view().unwrap().files.len(), 1);
    assert!(workspace.pending().unwrap().operations.is_empty());
    let restored = store
        .compare_saved(&workspace.id, workspace.generation, &old_head)
        .unwrap();
    assert_eq!(restored.comparison.base, base);
    assert!(
        restored
            .view()
            .unwrap()
            .files
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .viewed
    );
}

#[test]
fn reverted_content_still_invalidates_local_viewed_mark() {
    let (root, mut store, workspace, base, _) = fixture();
    let workspace = mark(&mut store, workspace, "a");
    commit(root.path(), "a", "temporary\n", "Temporary A");
    let reverted = commit(root.path(), "a", "reviewed\n", "Revert A");
    let workspace = refresh(
        &mut store,
        workspace,
        root.path(),
        &base,
        &reverted,
        "second",
    );
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert!(
        !workspace
            .view()
            .unwrap()
            .files
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .viewed
    );
    assert!(workspace.publication_plan().unwrap().operations.is_empty());
}

#[test]
fn remote_viewed_flags_are_not_subject_to_local_filtering() {
    let (root, mut store, workspace, base, _) = fixture();
    let next = commit(root.path(), "a", "new\n", "New A");
    let mut read = snapshot(root.path(), &base, &next);
    read.files
        .iter_mut()
        .find(|file| file.path == "a")
        .unwrap()
        .viewed = true;
    let review = ReviewId("remote-viewed".into());
    store
        .create_review(&review, &workspace.review.reviewer, &read)
        .unwrap();
    let workspace = store
        .refresh_workspace(&workspace.id, workspace.generation, &review)
        .unwrap();
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, Action::CompareLatest)
        .unwrap();
    assert!(
        workspace
            .view()
            .unwrap()
            .files
            .iter()
            .find(|file| file.path == "a")
            .unwrap()
            .viewed
    );
    assert!(workspace.pending().unwrap().operations.is_empty());
}

#[test]
fn invalid_saved_comparison_does_not_mutate_workspace() {
    let (_root, mut store, workspace, _, _) = fixture();
    let before = serde_json::to_value(&workspace).unwrap();
    assert!(
        store
            .compare_saved(&workspace.id, workspace.generation, "unknown")
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(store.workspace(&workspace.id).unwrap().unwrap()).unwrap(),
        before
    );
}
