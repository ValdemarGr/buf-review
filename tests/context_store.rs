use std::num::NonZeroU32;

use buf_review::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store},
    workspace::{Action, Destination},
};
use tempfile::tempdir;

fn comparison() -> DiffRevision {
    DiffRevision {
        base: "original-base".into(),
        head: "original-head".into(),
    }
}

fn user() -> User {
    User {
        id: UserId("me".into()),
        name: "Me".into(),
    }
}

fn baseline() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pr".into()),
            title: "PR".into(),
            body: String::new(),
            author: Some(user()),
            status: PullRequestStatus::Open,
            source: None,
            revision: comparison(),
        },
        files: vec![ChangedFile {
            path: "new.rs".into(),
            previous_path: Some("old.rs".into()),
            change: FileChange::Renamed,
            stats: None,
            viewed: false,
        }],
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    }
}

fn position() -> CommentPosition {
    let line = DiffLine {
        side: DiffSide::Head,
        line: NonZeroU32::new(3).unwrap(),
    };
    CommentPosition {
        path: "new.rs".into(),
        range: Some(DiffRange {
            start: line.clone(),
            end: line,
        }),
    }
}

#[test]
fn local_intent_prepares_and_reads_pinned_code_without_provider_metadata() {
    let project = tempdir().unwrap();
    let git = |arguments: &[&str]| {
        let result = std::process::Command::new("git")
            .current_dir(project.path())
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Review fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(project.path().join("file.rs"), "base\n").unwrap();
    git(&["add", "file.rs"]);
    git(&["commit", "-m", "Base"]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(project.path().join("file.rs"), "head\n").unwrap();
    git(&["commit", "-am", "Head"]);
    let revision = DiffRevision {
        base,
        head: git(&["rev-parse", "HEAD"]),
    };
    let mut store = Store::open(project.path()).unwrap();
    let branches = BranchPair {
        source: BranchSource {
            repository: "https://github.com/a/b".into(),
            branch: "feature".into(),
        },
        sink: BranchSource {
            repository: "https://github.com/a/b".into(),
            branch: "main".into(),
        },
    };
    let pr = store.create_or_use_intent(&branches, "", "").unwrap();
    let mut read = baseline();
    read.pull_request.id = pr.id;
    read.pull_request.revision = revision.clone();
    read.pull_request.status = PullRequestStatus::Local;
    read.files = buf_review::diff::files(project.path(), &revision).unwrap();
    let review = ReviewId("local-review".into());
    store.create_review(&review, &user(), &read).unwrap();
    let workspace = store.open_workspace(&review).unwrap();
    assert!(store.review_backend(&review).unwrap().is_none());
    std::fs::write(project.path().join("file.rs"), "staged\n").unwrap();
    git(&["add", "file.rs"]);
    std::fs::write(project.path().join("file.rs"), "unstaged\n").unwrap();

    let prepared = buf_review::context::prepare(&mut store, project.path(), &workspace).unwrap();
    assert_eq!(prepared["revision"], serde_json::json!(revision));
    assert!(prepared.get("warnings").is_none());
    let code = buf_review::context::code(project.path(), &workspace, "file.rs").unwrap();
    assert_eq!(code["base_content"], "base\n");
    assert_eq!(code["head_content"], "head\n");
    assert_eq!(
        std::fs::read_to_string(project.path().join("file.rs")).unwrap(),
        "unstaged\n"
    );

    let mut obsolete = workspace.clone();
    obsolete.comparison.head = "0".repeat(40);
    let prepared = buf_review::context::prepare(&mut store, project.path(), &obsolete).unwrap();
    assert_eq!(prepared["revision"], serde_json::json!(revision));

    let mut missing = workspace;
    missing.review.read.pull_request.revision.base = "0".repeat(40);
    let error = buf_review::context::prepare(&mut store, project.path(), &missing).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("local review commits unavailable")
    );
}

#[test]
fn confirmed_remote_identity_restores_exact_original_comparison_across_batches() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let source = ReviewSource {
        backend: "github".into(),
        locator: "https://github.com/a/b/pull/1".into(),
    };
    let backend = ReviewBackend {
        source: source.clone(),
        mappings: vec![],
    };
    let original = ReviewId("original".into());
    store
        .import_review(&original, &user(), &baseline(), &backend)
        .unwrap();
    let w = store.open_workspace(&original).unwrap();
    let w = store
        .mutate_workspace(
            &w.id,
            w.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::Diff(DiffLocation {
                    revision: comparison(),
                    position: position(),
                })),
                body: "Written here".into(),
                destination: Some(Destination::Standalone),
            },
        )
        .unwrap();
    let operation = w.publication_plan().unwrap().operations[0].clone();
    let local = w.saved_comment.as_ref().unwrap();
    store
        .record_operation_result(
            &w.batch.id,
            &operation,
            &[IdMapping {
                resource: "review_comment".into(),
                core_id: local.0.clone(),
                remote_id: "42".into(),
            }],
            &serde_json::json!({"id":42}),
        )
        .unwrap();
    drop(store);
    let mut store = Store::open(project.path()).unwrap();
    let current = ReadCommentAnchor {
        revision: ReadCommentRevision::Commit {
            head: "new-head".into(),
        },
        position: position(),
    };
    let imported_location = ReadDiffLocation {
        original: ReadCommentAnchor {
            revision: ReadCommentRevision::Commit {
                head: "original-head".into(),
            },
            position: position(),
        },
        current: Some(current.clone()),
        subject: Some(CommentSubject::Line),
        diff_hunk: Some("recorded hunk".into()),
        outdated: Some(true),
    };
    let mut incoming = baseline();
    incoming.pull_request.revision.head = "new-head".into();
    incoming.comments.push(ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId("remote-derived-id".into()),
            body: "Remote edit".into(),
        },
        author: Some(user()),
        target: CommentTarget::Diff(imported_location.clone()),
    });
    let fetched_backend = ReviewBackend {
        source,
        mappings: vec![IdMapping {
            resource: "review_comment".into(),
            core_id: "remote-derived-id".into(),
            remote_id: "42".into(),
        }],
    };
    let imported = ReviewId("incoming".into());
    store
        .import_review(&imported, &user(), &incoming, &fetched_backend)
        .unwrap();
    let session = store.create_batch_workspace(&imported).unwrap();
    let CommentTarget::Diff(actual) = &session.review.read.comments[0].target else {
        panic!("missing diff target")
    };
    assert_eq!(
        actual.original.revision,
        ReadCommentRevision::Comparison(comparison())
    );
    assert_eq!(actual.original.position, position());
    assert_eq!(actual.current, Some(current));
    assert_eq!(actual.diff_hunk, imported_location.diff_hunk);
    assert_eq!(actual.outdated, Some(true));
    assert_eq!(session.review.read.comments[0].content.body, "Remote edit");
    let mut variant_source = fetched_backend.clone();
    variant_source.source.locator = "https://github.com/A/B/pull/1/".into();
    let case_variant = ReviewId("case-variant-source".into());
    store
        .import_review(&case_variant, &user(), &incoming, &variant_source)
        .unwrap();
    assert_eq!(
        store
            .get_review(&case_variant)
            .unwrap()
            .unwrap()
            .read
            .comments[0]
            .target,
        session.review.read.comments[0].target
    );
    let published = store.workspace(&w.id).unwrap().unwrap();
    let refreshed = store
        .refresh_workspace(&published.id, published.generation, &imported)
        .unwrap();
    assert_eq!(refreshed.review.read.comments[0].content.id, *local);
    assert_eq!(
        refreshed.review.read.comments[0].target,
        session.review.read.comments[0].target
    );

    let mut wrong_position = incoming.clone();
    let CommentTarget::Diff(location) = &mut wrong_position.comments[0].target else {
        unreachable!()
    };
    location.original.position.path = "different.rs".into();
    let mismatch = ReviewId("mismatched-position".into());
    store
        .import_review(&mismatch, &user(), &wrong_position, &fetched_backend)
        .unwrap();
    assert_eq!(
        store.get_review(&mismatch).unwrap().unwrap().read,
        wrong_position
    );

    let mut wrong_source = fetched_backend.clone();
    wrong_source.source.locator = "https://github.com/other/repo/pull/1".into();
    let mut unrelated_read = incoming.clone();
    unrelated_read.pull_request.id = PullRequestId("unrelated-pr".into());
    let unrelated = ReviewId("unrelated-source".into());
    store
        .import_review(&unrelated, &user(), &unrelated_read, &wrong_source)
        .unwrap();
    assert_eq!(
        store.get_review(&unrelated).unwrap().unwrap().read,
        unrelated_read
    );

    let mut unconfirmed = fetched_backend.clone();
    unconfirmed.mappings[0].remote_id = "unconfirmed".into();
    let unknown = ReviewId("unconfirmed-write".into());
    store
        .import_review(&unknown, &user(), &incoming, &unconfirmed)
        .unwrap();
    assert_eq!(store.get_review(&unknown).unwrap().unwrap().read, incoming);
}
