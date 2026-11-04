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
fn committed_tree_paths_preserve_literal_names_and_ignore_local_changes() {
    let project = tempdir().unwrap();
    revisions(project.path());
    let path = "directory/file\twith\nλ";
    fs::create_dir(project.path().join("directory")).unwrap();
    fs::write(project.path().join(path), "committed\n").unwrap();
    git(project.path(), &["--literal-pathspecs", "add", path]);
    git(project.path(), &["commit", "--quiet", "-m", "Literal tree"]);
    let head = git(project.path(), &["rev-parse", "HEAD"]);
    fs::remove_file(project.path().join(path)).unwrap();
    fs::write(project.path().join("untracked"), "local\n").unwrap();
    let paths = diff::tree_paths(project.path(), &head).unwrap();
    assert_eq!(
        paths,
        ["file.rs", ":(glob)*", path]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert!(!project.path().join(path).exists());
    assert_eq!(
        fs::read_to_string(project.path().join("untracked")).unwrap(),
        "local\n"
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

#[test]
fn commit_summaries_show_only_fetched_tail_newest_first() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    git(
        project.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "Newest subject"],
    );
    let fetched = git(project.path(), &["rev-parse", "HEAD"]);
    let summaries =
        diff::missing_commits_summary(project.path(), &revision.base, &fetched).unwrap();
    assert_eq!(
        summaries,
        vec![
            diff::CommitSummary {
                id: fetched.clone(),
                subject: "Newest subject".into()
            },
            diff::CommitSummary {
                id: revision.head.clone(),
                subject: "Head".into()
            },
        ]
    );
    assert!(
        diff::missing_commits_summary(project.path(), &fetched, &fetched)
            .unwrap()
            .is_empty()
    );
    assert!(
        diff::missing_commits_summary(project.path(), &fetched, &revision.head)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn viewed_history_detects_reverts_and_preserves_unrelated_paths_in_both_directions() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    fs::write(project.path().join("file.rs"), "changed again\n").unwrap();
    git(project.path(), &["commit", "--quiet", "-am", "Change file"]);
    fs::write(project.path().join("file.rs"), "after\n").unwrap();
    git(project.path(), &["commit", "--quiet", "-am", "Revert file"]);
    let latest = git(project.path(), &["rev-parse", "HEAD"]);
    assert!(
        diff::files(
            project.path(),
            &DiffRevision {
                base: revision.head.clone(),
                head: latest.clone()
            }
        )
        .unwrap()
        .is_empty()
    );
    let paths = vec!["file.rs".into(), ":(glob)*".into()];
    for (marked, selected) in [(&revision.head, &latest), (&latest, &revision.head)] {
        assert_eq!(
            diff::unchanged_paths(project.path(), marked, selected, &paths).unwrap(),
            [":(glob)*".into()].into()
        );
    }
    assert_eq!(
        diff::unchanged_paths(project.path(), &revision.head, &revision.head, &paths).unwrap(),
        paths.into_iter().collect()
    );
}

#[test]
fn viewed_history_treats_rename_paths_literally_and_preserves_dirty_files() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    let renamed = "renamed\tfile\nλ.rs";
    fs::rename(
        project.path().join(":(glob)*"),
        project.path().join(renamed),
    )
    .unwrap();
    git(project.path(), &["--literal-pathspecs", "add", "."]);
    git(project.path(), &["commit", "--quiet", "-m", "Rename"]);
    let latest = git(project.path(), &["rev-parse", "HEAD"]);
    fs::write(project.path().join("file.rs"), "staged\n").unwrap();
    git(project.path(), &["add", "file.rs"]);
    fs::write(project.path().join("file.rs"), "unstaged\n").unwrap();
    let status = git(project.path(), &["status", "--porcelain"]);
    let paths = vec![":(glob)*".into(), renamed.into(), "file.rs".into()];
    assert_eq!(
        diff::unchanged_paths(project.path(), &revision.head, &latest, &paths).unwrap(),
        ["file.rs".into()].into()
    );
    assert_eq!(git(project.path(), &["status", "--porcelain"]), status);
    assert_eq!(
        fs::read_to_string(project.path().join("file.rs")).unwrap(),
        "unstaged\n"
    );
}

#[test]
fn viewed_history_checks_both_divergent_branches() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    fs::write(project.path().join("file.rs"), "first branch\n").unwrap();
    git(
        project.path(),
        &["commit", "--quiet", "-am", "First branch"],
    );
    let first = git(project.path(), &["rev-parse", "HEAD"]);
    git(
        project.path(),
        &["checkout", "--quiet", "--detach", &revision.head],
    );
    fs::write(project.path().join(":(glob)*"), "second branch\n").unwrap();
    git(
        project.path(),
        &["commit", "--quiet", "-am", "Second branch"],
    );
    let second = git(project.path(), &["rev-parse", "HEAD"]);
    let paths = vec!["file.rs".into(), ":(glob)*".into(), "untouched.rs".into()];
    assert_eq!(
        diff::unchanged_paths(project.path(), &first, &second, &paths).unwrap(),
        ["untouched.rs".into()].into()
    );
}

#[test]
fn viewed_history_includes_merge_parent_changes() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    git(project.path(), &["checkout", "--quiet", "-b", "side"]);
    fs::write(project.path().join("file.rs"), "side branch\n").unwrap();
    git(project.path(), &["commit", "--quiet", "-am", "Side change"]);
    let side = git(project.path(), &["rev-parse", "HEAD"]);
    git(
        project.path(),
        &["checkout", "--quiet", "--detach", &revision.head],
    );
    fs::write(project.path().join(":(glob)*"), "main branch\n").unwrap();
    git(project.path(), &["commit", "--quiet", "-am", "Main change"]);
    let marked = git(project.path(), &["rev-parse", "HEAD"]);
    git(
        project.path(),
        &["merge", "--quiet", "--no-ff", "-m", "Merge side", &side],
    );
    let selected = git(project.path(), &["rev-parse", "HEAD"]);
    let paths = vec!["file.rs".into(), ":(glob)*".into()];
    assert!(
        !diff::unchanged_paths(project.path(), &marked, &selected, &paths)
            .unwrap()
            .contains("file.rs")
    );
}

#[test]
fn history_queries_reject_missing_revisions_and_shallow_history() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    for missing in ["missing-commit", "--help"] {
        assert!(diff::missing_commits_summary(project.path(), missing, &revision.head).is_err());
        assert!(
            diff::unchanged_paths(project.path(), missing, &revision.head, &["file.rs".into()])
                .is_err()
        );
    }
    fs::write(
        project.path().join(".git/shallow"),
        format!("{}\n", revision.head),
    )
    .unwrap();
    assert!(diff::missing_commits_summary(project.path(), &revision.head, &revision.head).is_err());
    assert!(
        diff::unchanged_paths(
            project.path(),
            &revision.head,
            &revision.head,
            &["file.rs".into()]
        )
        .is_err()
    );
}

#[test]
fn history_queries_fail_when_an_intermediate_commit_is_missing() {
    let project = tempdir().unwrap();
    let revision = revisions(project.path());
    git(
        project.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "Latest"],
    );
    let latest = git(project.path(), &["rev-parse", "HEAD"]);
    fs::remove_file(
        project
            .path()
            .join(".git/objects")
            .join(&revision.head[..2])
            .join(&revision.head[2..]),
    )
    .unwrap();
    assert!(diff::missing_commits_summary(project.path(), &revision.base, &latest).is_err());
    assert!(
        diff::unchanged_paths(project.path(), &revision.base, &latest, &["file.rs".into()])
            .is_err()
    );
}

#[test]
fn fetch_all_updates_each_remote_without_changing_checkout_or_dirty_worktree() {
    let source = tempdir().unwrap();
    let revision = revisions(source.path());
    let project = tempdir().unwrap();
    git(
        project.path(),
        &["clone", "--quiet", source.path().to_str().unwrap(), "."],
    );
    git(
        project.path(),
        &["remote", "add", "backup", source.path().to_str().unwrap()],
    );
    let branch = git(source.path(), &["branch", "--show-current"]);
    fs::write(project.path().join("file.rs"), "staged\n").unwrap();
    git(project.path(), &["add", "file.rs"]);
    fs::write(project.path().join("file.rs"), "unstaged\n").unwrap();
    let status = git(project.path(), &["status", "--porcelain"]);
    git(
        source.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "Remote change"],
    );
    let remote_head = git(source.path(), &["rev-parse", "HEAD"]);
    diff::fetch_all(project.path()).unwrap();
    for remote in ["origin", "backup"] {
        assert_eq!(
            git(
                project.path(),
                &["rev-parse", &format!("refs/remotes/{remote}/{branch}")]
            ),
            remote_head
        );
    }
    assert_eq!(git(project.path(), &["rev-parse", "HEAD"]), revision.head);
    assert_eq!(git(project.path(), &["status", "--porcelain"]), status);
    assert_eq!(
        fs::read_to_string(project.path().join("file.rs")).unwrap(),
        "unstaged\n"
    );
}
