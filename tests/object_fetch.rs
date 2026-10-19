use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use buf_review::{
    diff,
    model::*,
    store::{ReviewBackend, ReviewSource, Store},
};
use serde_json::json;
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init(root: &Path) {
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Reviewer"]);
    git(root, &["config", "user.email", "reviewer@example.invalid"]);
}

fn commit(root: &Path, body: &str) -> String {
    fs::write(root.join("file.rs"), body).unwrap();
    git(root, &["add", "file.rs"]);
    git(root, &["commit", "--quiet", "-m", body]);
    git(root, &["rev-parse", "HEAD"])
}

fn remote() -> (TempDir, DiffRevision) {
    let source = tempdir().unwrap();
    init(source.path());
    let base = commit(source.path(), "before\n");
    let head = commit(source.path(), "after\n");
    let bare = tempdir().unwrap();
    git(
        source.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            ".",
            bare.path().to_str().unwrap(),
        ],
    );
    (bare, DiffRevision { base, head })
}

fn batch(root: &Path, revision: DiffRevision) -> String {
    batch_with_comments(root, revision, vec![])
}

fn batch_with_comments(root: &Path, revision: DiffRevision, comments: Vec<ReadComment>) -> String {
    let reviewer = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    let read = ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pr".into()),
            title: "PR".into(),
            body: "".into(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision,
        },
        files: vec![],
        comments,
        reactions: vec![],
        reviews: vec![],
    };
    let mut store = Store::open(root).unwrap();
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
                mappings: vec![],
            },
        )
        .unwrap();
    store.open_workspace(&review).unwrap().id
}

fn comment(id: &str, revision: ReadCommentRevision) -> ReadComment {
    ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId(id.into()),
            body: "Please fix this".into(),
        },
        author: None,
        target: CommentTarget::Diff(ReadDiffLocation {
            original: ReadCommentAnchor {
                revision,
                position: CommentPosition {
                    path: "file.rs".into(),
                    range: None,
                },
            },
            current: None,
            subject: Some(CommentSubject::File),
            diff_hunk: None,
            outdated: Some(true),
        }),
    }
}

fn request(root: &Path, batch: &str, trace: &Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pr"))
        .arg("workspace")
        .current_dir(root)
        .env("GIT_TRACE", trace)
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
            &serde_json::to_vec(&json!({
                "op":"fetch_objects", "workspace":batch
            }))
            .unwrap(),
        )
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn fetches_pinned_objects_without_changing_git_or_workspace_state() {
    let (remote, revision) = remote();
    let local = tempdir().unwrap();
    init(local.path());
    let checkout = commit(local.path(), "unrelated\n");
    git(
        local.path(),
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            remote.path().to_str().unwrap(),
            &revision.base,
        ],
    );
    git(
        local.path(),
        &[
            "config",
            &format!("url.{}.insteadOf", remote.path().display()),
            "https://github.com/example/repo.git",
        ],
    );
    git(
        local.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/repo.git",
        ],
    );
    git(local.path(), &["config", "fetch.prune", "true"]);
    git(local.path(), &["config", "fetch.pruneTags", "true"]);
    git(
        local.path(),
        &["update-ref", "refs/remotes/origin/obsolete", &checkout],
    );
    git(local.path(), &["tag", "keep-me"]);
    fs::write(local.path().join("file.rs"), "staged\n").unwrap();
    git(local.path(), &["add", "file.rs"]);
    fs::write(local.path().join("file.rs"), "unstaged\n").unwrap();
    fs::write(
        local.path().join(".git/FETCH_HEAD"),
        "preserve fetch head\n",
    )
    .unwrap();
    let refs = git(local.path(), &["show-ref"]);
    let branch = git(local.path(), &["symbolic-ref", "HEAD"]);
    let index = fs::read(local.path().join(".git/index")).unwrap();
    let id = batch(local.path(), revision.clone());
    let before = Store::open(local.path())
        .unwrap()
        .workspace(&id)
        .unwrap()
        .unwrap();
    let trace = local.path().join("trace");
    let output = request(local.path(), &id, &trace);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!({"revision":revision})
    );
    assert_eq!(git(local.path(), &["show-ref"]), refs);
    assert_eq!(git(local.path(), &["symbolic-ref", "HEAD"]), branch);
    assert_eq!(git(local.path(), &["rev-parse", "HEAD"]), checkout);
    assert_eq!(fs::read(local.path().join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(local.path().join("file.rs")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(local.path().join(".git/FETCH_HEAD")).unwrap(),
        "preserve fetch head\n"
    );
    assert_eq!(
        Store::open(local.path())
            .unwrap()
            .workspace(&id)
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        diff::contents(local.path(), &revision.head, "file.rs").unwrap(),
        Some(b"after\n".to_vec())
    );
    let log = fs::read_to_string(&trace).unwrap();
    let fetch = log
        .lines()
        .find(|line| line.contains("built-in: git fetch"))
        .unwrap();
    assert!(fetch.contains(&revision.head));
    assert!(!fetch.contains(&revision.base));
    fs::write(&trace, "").unwrap();
    remote.close().unwrap();
    assert!(request(local.path(), &id, &trace).status.success());
    assert!(!fs::read_to_string(&trace).unwrap().contains("git fetch"));
}

#[test]
fn missing_or_malformed_pins_fail_without_repinning() {
    for base in ["0".repeat(40), "--upload-pack=unsafe".into()] {
        let local = tempdir().unwrap();
        init(local.path());
        let head = commit(local.path(), "local\n");
        let remote = tempdir().unwrap();
        git(
            local.path(),
            &[
                "config",
                &format!("url.{}.insteadOf", remote.path().display()),
                "https://github.com/example/repo.git",
            ],
        );
        let revision = DiffRevision {
            base: base.clone(),
            head: head.clone(),
        };
        let id = batch(local.path(), revision.clone());
        let before = Store::open(local.path())
            .unwrap()
            .workspace(&id)
            .unwrap()
            .unwrap();
        let trace = local.path().join("trace");
        fs::write(&trace, "").unwrap();
        let output = request(local.path(), &id, &trace);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(&base), "{error}");
        if base.starts_with('-') {
            assert!(error.contains("full hexadecimal"));
            assert!(!fs::read_to_string(&trace).unwrap().contains("git fetch"));
        } else {
            assert!(
                error.contains("does not appear to be a git repository"),
                "{error}"
            );
            assert!(error.contains("retry"));
        }
        assert_eq!(
            Store::open(local.path())
                .unwrap()
                .workspace(&id)
                .unwrap()
                .unwrap(),
            before
        );
    }
}

#[test]
fn existing_local_revision_names_need_no_network() {
    let local = tempdir().unwrap();
    init(local.path());
    commit(local.path(), "local\n");
    diff::fetch_missing(
        local.path(),
        &DiffRevision {
            base: "HEAD".into(),
            head: "HEAD".into(),
        },
        "unavailable",
    )
    .unwrap();
}

#[test]
fn prepares_pins_then_fetches_only_distinct_missing_historical_commits_together() {
    let source = tempdir().unwrap();
    init(source.path());
    let base = commit(source.path(), "base\n");
    let ancestor = commit(source.path(), "reviewed ancestor\n");
    let head = commit(source.path(), "current head\n");
    let revision = DiffRevision {
        base: base.clone(),
        head,
    };
    git(
        source.path(),
        &["checkout", "--quiet", "-b", "already-local", &base],
    );
    let already_local = commit(source.path(), "already local historical head\n");
    git(
        source.path(),
        &["checkout", "--quiet", "-b", "historical-one", &base],
    );
    let missing_one = commit(source.path(), "historical head one\n");
    git(
        source.path(),
        &["checkout", "--quiet", "-b", "historical-two", &base],
    );
    let missing_two = commit(source.path(), "historical head two\n");
    git(
        source.path(),
        &["checkout", "--quiet", "-b", "latest-pr", &revision.head],
    );
    let latest_head = commit(source.path(), "latest fetched PR head\n");
    let remote = tempdir().unwrap();
    git(
        source.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            ".",
            remote.path().to_str().unwrap(),
        ],
    );

    let local = tempdir().unwrap();
    init(local.path());
    commit(local.path(), "local checkout\n");
    git(
        local.path(),
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            remote.path().to_str().unwrap(),
            &already_local,
        ],
    );
    git(
        local.path(),
        &[
            "config",
            &format!("url.{}.insteadOf", remote.path().display()),
            "https://github.com/example/repo.git",
        ],
    );
    fs::write(local.path().join("file.rs"), "staged\n").unwrap();
    git(local.path(), &["add", "file.rs"]);
    fs::write(local.path().join("file.rs"), "unstaged\n").unwrap();
    fs::write(local.path().join(".git/FETCH_HEAD"), "preserve\n").unwrap();
    let comments = vec![
        comment(
            "ancestor",
            ReadCommentRevision::Commit {
                head: ancestor.clone(),
            },
        ),
        comment(
            "local",
            ReadCommentRevision::Commit {
                head: already_local.clone(),
            },
        ),
        comment(
            "missing",
            ReadCommentRevision::Commit {
                head: missing_one.clone(),
            },
        ),
        comment(
            "comparison",
            ReadCommentRevision::Comparison(DiffRevision {
                base: missing_one.clone(),
                head: missing_two.clone(),
            }),
        ),
        comment(
            "duplicate",
            ReadCommentRevision::Commit {
                head: missing_two.clone(),
            },
        ),
        ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("reply".into()),
                body: "Same historical context".into(),
            },
            author: None,
            target: CommentTarget::Reply(CommentId("missing".into())),
        },
    ];
    let id = batch_with_comments(local.path(), revision.clone(), comments);
    let mut store = Store::open(local.path()).unwrap();
    let workspace = store.workspace(&id).unwrap().unwrap();
    let backend = store.review_backend(&workspace.review.id).unwrap().unwrap();
    let mut read = workspace.review.read.clone();
    read.pull_request.revision.head = latest_head.clone();
    let latest = ReviewId("latest-pr".into());
    store
        .import_review(&latest, &workspace.review.reviewer, &read, &backend)
        .unwrap();
    store
        .refresh_workspace(&id, workspace.generation, &latest)
        .unwrap();
    drop(store);
    let state = || {
        (
            git(local.path(), &["show-ref"]),
            git(local.path(), &["symbolic-ref", "HEAD"]),
            fs::read(local.path().join(".git/index")).unwrap(),
            fs::read(local.path().join("file.rs")).unwrap(),
            fs::read(local.path().join(".git/FETCH_HEAD")).unwrap(),
            Store::open(local.path())
                .unwrap()
                .workspace(&id)
                .unwrap()
                .unwrap(),
        )
    };
    let before = state();
    let trace = local.path().join("trace");
    let output = request(local.path(), &id, &trace);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(state(), before);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!({"revision": revision})
    );
    let log = fs::read_to_string(&trace).unwrap();
    let fetches = log
        .lines()
        .filter(|line| line.contains("built-in: git ") && line.contains(" fetch "))
        .collect::<Vec<_>>();
    assert_eq!(fetches.len(), 2, "{log}");
    assert!(fetches[0].contains(&revision.head), "{log}");
    assert!(fetches[0].contains(&latest_head), "{log}");
    assert!(
        !fetches[0].contains(&revision.base),
        "base already arrived with local history: {log}"
    );
    for hash in [&missing_one, &missing_two] {
        assert_eq!(
            fetches[1]
                .split_whitespace()
                .filter(|arg| *arg == hash.as_str())
                .count(),
            1,
            "{log}"
        );
    }
    for hash in [
        &ancestor,
        &already_local,
        &revision.base,
        &revision.head,
        &latest_head,
    ] {
        assert!(
            !fetches[1].contains(hash),
            "already present commit fetched again: {log}"
        );
    }
    for hash in [&ancestor, &already_local, &missing_one, &missing_two] {
        assert!(
            diff::contents(local.path(), hash, "file.rs")
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        diff::contents(local.path(), &latest_head, "file.rs").unwrap(),
        Some(b"latest fetched PR head\n".to_vec())
    );

    fs::write(&trace, "").unwrap();
    remote.close().unwrap();
    let output = request(local.path(), &id, &trace);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fs::read_to_string(&trace).unwrap().contains("git fetch"));
    assert_eq!(state(), before);
}

#[test]
fn unavailable_historical_commits_warn_without_mutating_git_or_workspace_state() {
    let (remote, revision) = remote();
    let local = tempdir().unwrap();
    init(local.path());
    commit(local.path(), "local\n");
    git(
        local.path(),
        &[
            "config",
            &format!("url.{}.insteadOf", remote.path().display()),
            "https://github.com/example/repo.git",
        ],
    );
    let unavailable = "0".repeat(40);
    let other_unavailable = "1".repeat(40);
    let id = batch_with_comments(
        local.path(),
        revision.clone(),
        vec![
            comment(
                "missing",
                ReadCommentRevision::Commit {
                    head: unavailable.clone(),
                },
            ),
            comment(
                "also-missing",
                ReadCommentRevision::Commit {
                    head: other_unavailable.clone(),
                },
            ),
        ],
    );
    fs::write(local.path().join(".git/FETCH_HEAD"), "preserve\n").unwrap();
    let state = || {
        (
            git(local.path(), &["show-ref"]),
            git(local.path(), &["symbolic-ref", "HEAD"]),
            fs::read(local.path().join(".git/index")).unwrap(),
            fs::read(local.path().join("file.rs")).unwrap(),
            fs::read(local.path().join(".git/FETCH_HEAD")).unwrap(),
            Store::open(local.path())
                .unwrap()
                .workspace(&id)
                .unwrap()
                .unwrap(),
        )
    };
    let before = state();
    let trace = local.path().join("trace");
    let output = request(local.path(), &id, &trace);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["revision"], json!(revision));
    let warnings = response["warnings"].to_string();
    assert!(warnings.contains(&unavailable), "{response}");
    assert!(warnings.contains(&other_unavailable), "{response}");
    let missing = response["missing_commits"].as_array().unwrap();
    assert!(missing.contains(&json!(unavailable)), "{response}");
    assert!(missing.contains(&json!(other_unavailable)), "{response}");
    assert_eq!(state(), before);
}
