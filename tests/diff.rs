use std::{fs, path::Path, process::Command};

use buf_review::{
    diff::{self, DiffError},
    model::DiffRevision,
};
use tempfile::tempdir;

fn git(project: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(project)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn revisions(project: &Path) -> DiffRevision {
    git(project, &["init", "--quiet"]);
    git(project, &["config", "user.name", "Reviewer"]);
    git(
        project,
        &["config", "user.email", "reviewer@example.invalid"],
    );
    fs::write(project.join("file.rs"), "before\n").unwrap();
    fs::write(project.join(":(glob)*"), "literal before\n").unwrap();
    git(project, &["--literal-pathspecs", "add", "."]);
    git(project, &["commit", "--quiet", "-m", "Base"]);
    let base = git(project, &["rev-parse", "HEAD"]);
    fs::write(project.join("file.rs"), "after\n").unwrap();
    fs::write(project.join(":(glob)*"), "literal after\n").unwrap();
    git(project, &["--literal-pathspecs", "add", "."]);
    git(project, &["commit", "--quiet", "-m", "Head"]);
    let head = git(project, &["rev-parse", "HEAD"]);
    DiffRevision { base, head }
}

#[test]
fn compares_commits_without_touching_dirty_worktree() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    fs::write(project.path().join("file.rs"), "uncommitted\n").unwrap();
    let result = diff::diff(project.path(), &revision, Some("file.rs")).unwrap();
    let result = String::from_utf8(result).unwrap();
    assert!(result.contains("-before\n+after\n"));
    assert!(!result.contains("uncommitted"));
    assert!(!result.contains("literal"));
    assert_eq!(
        fs::read_to_string(project.path().join("file.rs")).unwrap(),
        "uncommitted\n"
    );
}

#[test]
fn pathspec_magic_is_treated_as_literal_filename() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    let result =
        String::from_utf8(diff::diff(project.path(), &revision, Some(":(glob)*")).unwrap())
            .unwrap();
    assert!(result.contains("-literal before\n+literal after\n"));
    assert!(!result.contains("file.rs"));
}

#[test]
fn rejects_missing_and_option_like_revisions() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    for base in ["missing-commit", "--help"] {
        let invalid = DiffRevision {
            base: base.into(),
            head: revision.head.clone(),
        };
        assert!(matches!(
            diff::diff(project.path(), &invalid, None),
            Err(DiffError::Git(_))
        ));
    }
}
