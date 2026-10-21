use std::{fs, num::NonZeroU32, path::Path, process::Command};

use buf_review::{
    context, diff,
    model::*,
    store::Store,
    workspace::{Action, Workspace},
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

fn commit(project: &Path, text: &str) -> String {
    fs::write(project.join("file.rs"), text).unwrap();
    git(project, &["add", "file.rs"]);
    git(project, &["commit", "--quiet", "-m", text]);
    git(project, &["rev-parse", "HEAD"])
}

fn fixture() -> (TempDir, Store, Workspace) {
    let project = tempdir().unwrap();
    git(project.path(), &["init", "--quiet"]);
    git(project.path(), &["config", "user.name", "Reviewer"]);
    git(
        project.path(),
        &["config", "user.email", "reviewer@example.invalid"],
    );
    let base = commit(project.path(), "base\n");
    let head = commit(project.path(), "reviewed\n");
    let revision = DiffRevision { base, head };
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Working tree review".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: revision.clone(),
        },
        files: diff::files(project.path(), &revision).unwrap(),
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    };
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("snapshot".into());
    store.create_review(&review, &reviewer, &read).unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    (project, store, workspace)
}

fn save(revision: &DiffRevision, side: DiffSide) -> Action {
    let range = DiffRange {
        start: DiffLine {
            side,
            line: NonZeroU32::new(1).unwrap(),
        },
        end: DiffLine {
            side,
            line: NonZeroU32::new(1).unwrap(),
        },
    };
    Action::Save {
        comment: None,
        body: "Committed source comment".into(),
        destination: None,
        target: Some(CommentTarget::Diff(DiffLocation {
            revision: revision.clone(),
            position: CommentPosition {
                path: "file.rs".into(),
                range: Some(range),
            },
        })),
    }
}

#[test]
fn runtime_head_and_files_follow_checkout_without_rewriting_snapshot_or_history() {
    let (project, mut store, saved) = fixture();
    let history = store.workspace_log(&saved.id).unwrap();
    let fetched = saved.review.clone();
    let head = commit(project.path(), "local committed head\n");
    fs::write(project.path().join("new.rs"), "local committed file\n").unwrap();
    git(project.path(), &["add", "new.rs"]);
    git(project.path(), &["commit", "--quiet", "-m", "New file"]);
    let latest = git(project.path(), &["rev-parse", "HEAD"]);
    assert_ne!(head, latest);
    store.enable_worktree_context();
    let current = store.workspace(&saved.id).unwrap().unwrap();
    assert_eq!(current.comparison.base, saved.comparison.base);
    assert_eq!(current.comparison.head, latest);
    assert!(
        current
            .comparison_files
            .iter()
            .any(|file| file.path == "new.rs")
    );
    assert_eq!(current.review, fetched);
    assert_eq!(current.generation, saved.generation);
    assert_eq!(current.batch, saved.batch);
    assert_eq!(store.workspace_log(&saved.id).unwrap(), history);
    assert_eq!(
        store
            .workspace_at_generation(&saved.id, saved.generation)
            .unwrap()
            .unwrap()
            .comparison,
        saved.comparison
    );
    assert_eq!(
        context::code(project.path(), &current, "file.rs").unwrap()["head_content"],
        "local committed head\n"
    );
    git(
        project.path(),
        &["checkout", "--quiet", "--detach", &saved.comparison.head],
    );
    let previous = store.workspace(&saved.id).unwrap().unwrap();
    assert_eq!(previous.comparison, saved.comparison);
    assert_eq!(store.workspace_log(&saved.id).unwrap(), history);
}

#[test]
fn dirty_file_metadata_keeps_committed_bytes_and_rejects_only_live_head_line_comments() {
    let (project, mut store, saved) = fixture();
    let mut current = store.prepare_worktree(&saved.id).unwrap();
    let clean = context::code(project.path(), &current, "file.rs").unwrap();
    assert_eq!(clean["local_dirty"], false);
    assert_eq!(clean["local_path"], json!(project.path().join("file.rs")));
    for staged in [false, true] {
        fs::write(project.path().join("file.rs"), "local edit\n").unwrap();
        if staged {
            git(project.path(), &["add", "file.rs"]);
        }
        let before = git(project.path(), &["status", "--porcelain"]);
        let code = context::code(project.path(), &current, "file.rs").unwrap();
        assert_eq!(code["local_dirty"], true);
        assert_eq!(code["head_content"], "reviewed\n");
        let error = store
            .mutate_workspace(
                &current.id,
                current.generation,
                save(&current.comparison, DiffSide::Head),
            )
            .unwrap_err();
        assert!(error.to_string().contains("commit and reopen"), "{error}");
        assert_eq!(
            store.workspace(&current.id).unwrap().unwrap().generation,
            current.generation
        );
        assert_eq!(git(project.path(), &["status", "--porcelain"]), before);
    }
    current = store
        .mutate_workspace(
            &current.id,
            current.generation,
            save(&current.comparison, DiffSide::Base),
        )
        .unwrap();
    let comment = current.saved_comment.clone().unwrap();
    let source = context::historical_code(&store, project.path(), &current, &comment).unwrap();
    assert_eq!(source["code"]["base_content"], "base\n");
    assert_eq!(source["code"]["head_content"], "reviewed\n");
    assert_eq!(source["code"]["local_dirty"], true);
    assert_eq!(source["code"]["revision"]["head"], current.comparison.head);
}

#[test]
fn committing_requires_reopening_before_new_line_comments_and_preserves_old_anchors() {
    let (project, mut store, saved) = fixture();
    let mut opened = store.prepare_worktree(&saved.id).unwrap();
    opened = store
        .mutate_workspace(
            &opened.id,
            opened.generation,
            save(&opened.comparison, DiffSide::Head),
        )
        .unwrap();
    let comment = opened.saved_comment.clone().unwrap();
    let original = opened.comparison.clone();
    let head = commit(project.path(), "new committed source\n");
    let mut stale_base = opened.clone();
    let error = stale_base
        .apply(save(&original, DiffSide::Base))
        .unwrap_err();
    assert!(error.to_string().contains("reopen the diff"), "{error}");
    let error = store
        .mutate_workspace(
            &opened.id,
            opened.generation,
            save(&original, DiffSide::Head),
        )
        .unwrap_err();
    assert!(error.to_string().contains("reopen the diff"), "{error}");
    let reopened = store.workspace(&opened.id).unwrap().unwrap();
    assert_eq!(reopened.comparison.head, head);
    let historical = context::historical_code(&store, project.path(), &reopened, &comment).unwrap();
    assert_eq!(historical["anchor"]["revision"], json!(original));
    assert_eq!(historical["code"]["base_content"], "reviewed\n");
    assert_eq!(historical["code"]["head_content"], "new committed source\n");
    store
        .mutate_workspace(
            &reopened.id,
            reopened.generation,
            save(&reopened.comparison, DiffSide::Head),
        )
        .unwrap();
}

#[test]
fn worktree_mode_reports_missing_base_instead_of_falling_back_to_saved_head() {
    let (project, mut store, mut saved) = fixture();
    saved.review.read.pull_request.revision.base = "f".repeat(40);
    let missing = ReviewId("missing-base".into());
    store
        .create_review(&missing, &saved.review.reviewer, &saved.review.read)
        .unwrap();
    let missing = store.create_batch_workspace(&missing).unwrap();
    store.enable_worktree_context();
    assert!(store.workspace(&missing.id).is_err());
    assert!(store.prepare_worktree(&missing.id).is_err());
    assert_eq!(
        git(project.path(), &["rev-parse", "HEAD"]),
        saved.comparison.head
    );
}

#[test]
fn unavailable_checked_out_head_never_uses_the_saved_comparison() {
    let (project, mut store, saved) = fixture();
    git(
        project.path(),
        &["symbolic-ref", "HEAD", "refs/heads/unborn"],
    );
    assert_eq!(
        store.workspace(&saved.id).unwrap().unwrap().comparison,
        saved.comparison
    );
    store.enable_worktree_context();
    assert!(store.workspace(&saved.id).is_err());
    assert!(store.prepare_worktree(&saved.id).is_err());
    assert_eq!(
        store
            .workspace_at_generation(&saved.id, saved.generation)
            .unwrap()
            .unwrap()
            .comparison,
        saved.comparison
    );
}
