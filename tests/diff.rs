use std::{fs, path::Path, process::Command};

use buf_review::{
    diff::{self, DiffError},
    model::{DiffRevision, FileChange, FileStats},
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
fn committed_file_metadata_preserves_renames_binary_files_and_line_counts() {
    let project = tempdir().unwrap();
    revisions(project.path());
    fs::write(project.path().join("binary"), b"\0before\n").unwrap();
    fs::write(project.path().join("modified"), "before\nkeep\n").unwrap();
    git(project.path(), &["add", "."]);
    git(
        project.path(),
        &["commit", "--quiet", "-m", "Metadata base"],
    );
    let base = git(project.path(), &["rev-parse", "HEAD"]);
    let renamed = "renamed\tfile\nλ.rs";
    fs::rename(
        project.path().join(":(glob)*"),
        project.path().join(renamed),
    )
    .unwrap();
    fs::remove_file(project.path().join("file.rs")).unwrap();
    fs::write(project.path().join("added"), "one\ntwo\n").unwrap();
    fs::write(project.path().join("binary"), b"\0after\n").unwrap();
    fs::write(project.path().join("modified"), "after\nkeep\nmore\n").unwrap();
    git(project.path(), &["--literal-pathspecs", "add", "."]);
    git(
        project.path(),
        &["commit", "--quiet", "-m", "Metadata head"],
    );
    let revision = DiffRevision {
        base,
        head: git(project.path(), &["rev-parse", "HEAD"]),
    };
    fs::write(project.path().join("modified"), "staged changes\n").unwrap();
    git(project.path(), &["add", "modified"]);
    fs::write(project.path().join("modified"), "unstaged changes\n").unwrap();
    fs::write(project.path().join("untracked"), "not reviewed\n").unwrap();
    let before = git(project.path(), &["status", "--porcelain"]);

    let files = diff::files(project.path(), &revision).unwrap();
    assert_eq!(files.len(), 5);
    let file = |path: &str| files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(file("added").change, FileChange::Added);
    assert_eq!(
        file("added").stats,
        Some(FileStats {
            additions: 2,
            deletions: 0
        })
    );
    assert_eq!(file("file.rs").change, FileChange::Deleted);
    assert_eq!(file("modified").change, FileChange::Modified);
    assert_eq!(
        file("modified").stats,
        Some(FileStats {
            additions: 2,
            deletions: 1
        })
    );
    assert_eq!(file("binary").stats, None);
    assert_eq!(file(renamed).change, FileChange::Renamed);
    assert_eq!(file(renamed).previous_path.as_deref(), Some(":(glob)*"));
    assert_eq!(
        file(renamed).stats,
        Some(FileStats {
            additions: 0,
            deletions: 0
        })
    );
    assert!(files.iter().all(|file| !file.viewed));
    assert_eq!(git(project.path(), &["status", "--porcelain"]), before);
    assert_eq!(
        fs::read_to_string(project.path().join("modified")).unwrap(),
        "unstaged changes\n"
    );
    assert!(
        diff::files(
            project.path(),
            &DiffRevision {
                base: revision.head.clone(),
                head: revision.head
            }
        )
        .unwrap()
        .is_empty()
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

#[test]
fn reads_literal_committed_contents_and_distinguishes_absent_files() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    fs::write(project.path().join("file.rs"), "dirty\n").unwrap();
    assert_eq!(
        diff::contents(project.path(), &revision.base, "file.rs").unwrap(),
        Some(b"before\n".to_vec())
    );
    assert_eq!(
        diff::contents(project.path(), &revision.head, ":(glob)*").unwrap(),
        Some(b"literal after\n".to_vec())
    );
    assert_eq!(
        diff::contents(project.path(), &revision.head, "absent.rs").unwrap(),
        None
    );
    assert!(diff::contents(project.path(), "missing", "absent.rs").is_err());
    assert_eq!(
        fs::read_to_string(project.path().join("file.rs")).unwrap(),
        "dirty\n"
    );
}

#[test]
fn checks_large_commit_batches_without_pipe_deadlock_and_preserves_distinct_input_order() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    let expected = (0..4096)
        .rev()
        .map(|id| format!("{id:040x}"))
        .collect::<Vec<_>>();
    let mut commits = vec![revision.base, "HEAD".into()];
    commits.extend(expected.clone());
    commits.extend([expected[0].clone(), revision.head, expected[1].clone()]);
    assert_eq!(
        diff::missing_commits(project.path(), &commits).unwrap(),
        expected
    );
}

#[test]
fn batch_commit_checks_reject_record_separators() {
    let project = tempdir().unwrap();
    revisions(project.path());
    for revision in ["HEAD\nmissing", "HEAD\0missing"] {
        assert!(matches!(
            diff::missing_commits(project.path(), &[revision.into()]),
            Err(DiffError::Git(_))
        ));
    }
}
