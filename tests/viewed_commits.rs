use std::{fs, path::Path, process::Command};

use buf_review::{model::*, store::Store, workspace::Action};
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
            title: "Viewed commit versions".into(),
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
fn marks_preserve_distinct_commit_versions_through_refresh_and_reopen() {
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

    let mut store = Store::open(project.path()).unwrap();
    let mut workspace = store.workspace(&batch).unwrap().unwrap();
    assert_eq!(workspace.batch.write.events.len(), 3);
    let commits = workspace
        .batch
        .write
        .events
        .iter()
        .map(|record| match &record.event {
            Event::SetFileViewed { commit, .. } => commit.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commits,
        vec![
            Some(head_a.clone()),
            Some(head_b.clone()),
            Some(head_b.clone())
        ]
    );
    assert!(workspace.batch.write.events.iter().all(|event| matches!(
        &event.event,
        Event::SetFileViewed {
            commit: Some(_),
            ..
        }
    )));
    assert!(!workspace.projected().unwrap().files[0].viewed);
    assert!(workspace.pending().unwrap().operations.is_empty());

    workspace = store
        .compare_saved(&batch, workspace.generation, &head_a)
        .unwrap();
    assert!(
        workspace.projected().unwrap().files[0].viewed,
        "{workspace:?}"
    );
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
fn raw_mark_records_its_active_comparison_commit() {
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
    assert!(
        matches!(&recovered.batch.write.events[0].event, Event::SetFileViewed { commit: Some(commit), .. } if commit == "head")
    );
    assert!(recovered.projected().unwrap().files[0].viewed);
    assert_eq!(recovered.batch.write.events.len(), 1);
}

#[test]
fn raw_mark_uses_selected_comparison_commit() {
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
    assert!(
        matches!(&recovered.batch.write.events[0].event, Event::SetFileViewed { commit: Some(commit), .. } if commit == "another-head")
    );
    assert!(recovered.projected().unwrap().files[0].viewed);
}
