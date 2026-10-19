use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use thiserror::Error;

use crate::model::{ChangedFile, DiffRevision, DiffSide, FileChange, FileStats};

#[derive(Debug, Error)]
pub enum DiffError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git: {0}")]
    Git(String),
}

/// Makes the pinned commits available without checking out files or updating refs.
/// Existing local revision names are accepted, but only full object IDs may be fetched.
pub fn fetch_missing(
    project_root: &Path,
    revision: &DiffRevision,
    repository: &str,
) -> Result<(), DiffError> {
    fetch_missing_commits(
        project_root,
        &[revision.base.clone(), revision.head.clone()],
        repository,
    )
}

/// Checks all revisions in one local Git process, preserving their input order.
/// Object existence alone is insufficient: every revision must resolve to a commit.
pub fn missing_commits(project_root: &Path, commits: &[String]) -> Result<Vec<String>, DiffError> {
    let mut seen = HashSet::new();
    let commits: Vec<_> = commits
        .iter()
        .filter(|commit| seen.insert(*commit))
        .collect();
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    for commit in &commits {
        if commit.contains(['\n', '\0']) {
            return Err(DiffError::Git(
                "commit revisions cannot contain newlines or NUL bytes".into(),
            ));
        }
    }
    let input: String = commits
        .iter()
        .map(|commit| format!("{commit}^{{commit}}\n"))
        .collect();
    let mut child = Command::new("git")
        .current_dir(project_root)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["cat-file", "--batch-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let (output, written) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
        let output = child.wait_with_output();
        let written = writer
            .join()
            .map_err(|_| DiffError::Git("Git input writer panicked".into()))?;
        Ok::<_, DiffError>((output, written))
    })?;
    let output = checked(output?)?;
    written?;
    let output = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = output.lines().collect();
    if lines.len() != commits.len() {
        return Err(DiffError::Git(
            "incomplete commit existence response".into(),
        ));
    }
    commits
        .into_iter()
        .zip(lines)
        .filter_map(|(commit, line)| {
            if line.ends_with(" missing") {
                Some(Ok(commit.clone()))
            } else if line.split_whitespace().nth(1) == Some("commit") {
                None
            } else {
                Some(Err(DiffError::Git(format!(
                    "could not check commit {commit}: {line}"
                ))))
            }
        })
        .collect()
}

/// Fetches all missing commit IDs together without updating refs or the worktree.
pub fn fetch_missing_commits(
    project_root: &Path,
    commits: &[String],
    repository: &str,
) -> Result<(), DiffError> {
    let missing = missing_commits(project_root, commits)?;
    if missing.is_empty() {
        return Ok(());
    }
    let pins = missing.join(", ");
    for commit in &missing {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DiffError::Git(format!(
                "missing pinned commit {commit}: only full hexadecimal commit IDs can be fetched"
            )));
        }
    }
    let output = Command::new("git")
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "credential.helper=!gh auth git-credential",
            "fetch",
            "--no-tags",
            "--no-prune",
            "--no-prune-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--no-write-commit-graph",
            "--refmap=",
            "--",
            repository,
        ])
        .args(&missing)
        .output()
        .map_err(|error| {
            DiffError::Git(format!("could not fetch pinned commits {pins}: {error}"))
        })?;
    if !output.status.success() {
        return Err(DiffError::Git(format!(
            "could not fetch pinned commits {pins} from {repository}: {}. Check repository access and retry opening the PR or code",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let unavailable = missing_commits(project_root, &missing)?;
    if !unavailable.is_empty() {
        return Err(DiffError::Git(format!(
            "pinned commits {} are still unavailable after fetch",
            unavailable.join(", ")
        )));
    }
    Ok(())
}

/// Compares local commits without fetching or changing the working tree.
pub fn diff(
    project_root: &Path,
    revision: &DiffRevision,
    path: Option<&str>,
) -> Result<Vec<u8>, DiffError> {
    let base = resolve(project_root, &revision.base)?;
    let head = resolve(project_root, &revision.head)?;
    let mut command = Command::new("git");
    command.current_dir(project_root).args([
        "--literal-pathspecs",
        "-c",
        "core.quotePath=false",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        &base,
        &head,
        "--",
    ]);
    if let Some(path) = path {
        command.arg(path);
    }
    checked(command.output()?).map(|output| output.stdout)
}

/// Lists changed files and line counts between pinned commits, independently of
/// the index and working tree. NUL records preserve literal rename paths.
pub fn files(project_root: &Path, revision: &DiffRevision) -> Result<Vec<ChangedFile>, DiffError> {
    let base = resolve(project_root, &revision.base)?;
    let head = resolve(project_root, &revision.head)?;
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--ignore-submodules=none",
                "--find-renames",
                "--raw",
                "--numstat",
                "-z",
                &base,
                &head,
                "--",
            ])
            .output()?,
    )?;
    let invalid = || DiffError::Git("invalid changed-file metadata from Git".into());
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| DiffError::Git("changed file paths must be valid UTF-8".into()))?;
    let mut records = text.split_terminator('\0');
    let mut files = BTreeMap::new();
    while let Some(record) = records.next() {
        if record.starts_with(':') {
            let status = record.rsplit(' ').next().ok_or_else(invalid)?;
            let change = match status.as_bytes().first() {
                Some(b'A') => FileChange::Added,
                Some(b'M') => FileChange::Modified,
                Some(b'D') => FileChange::Deleted,
                Some(b'R') => FileChange::Renamed,
                Some(b'C') => FileChange::Copied,
                Some(b'T') => FileChange::TypeChanged,
                _ => return Err(invalid()),
            };
            let before = records.next().ok_or_else(invalid)?;
            let (path, previous_path) =
                if matches!(change, FileChange::Renamed | FileChange::Copied) {
                    (records.next().ok_or_else(invalid)?, Some(before.to_owned()))
                } else {
                    (before, None)
                };
            files.insert(
                path.to_owned(),
                ChangedFile {
                    path: path.to_owned(),
                    previous_path,
                    change,
                    stats: None,
                    viewed: false,
                },
            );
        } else {
            let mut columns = record.splitn(3, '\t');
            let additions = columns.next().ok_or_else(invalid)?;
            let deletions = columns.next().ok_or_else(invalid)?;
            let path = columns.next().ok_or_else(invalid)?;
            let path = if path.is_empty() {
                records.next().ok_or_else(invalid)?;
                records.next().ok_or_else(invalid)?
            } else {
                path
            };
            let file = files.get_mut(path).ok_or_else(invalid)?;
            file.stats = match (additions, deletions) {
                ("-", "-") => None,
                _ => Some(FileStats {
                    additions: additions.parse().map_err(|_| invalid())?,
                    deletions: deletions.parse().map_err(|_| invalid())?,
                }),
            };
        }
    }
    Ok(files.into_values().collect())
}

/// Reads a literal file from an available commit. Absence is distinct from a
/// missing commit or Git error; neither case consults the working tree.
pub fn contents(
    project_root: &Path,
    revision: &str,
    path: &str,
) -> Result<Option<Vec<u8>>, DiffError> {
    let commit = resolve(project_root, revision)?;
    let tree = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "--literal-pathspecs",
                "ls-tree",
                "--full-tree",
                "-z",
                &commit,
                "--",
                path,
            ])
            .output()?,
    )?;
    if tree.stdout.is_empty() {
        return Ok(None);
    }
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "blob", &format!("{commit}:{path}")])
            .output()?,
    )?;
    Ok(Some(output.stdout))
}

/// A reconstructed comparison requires one unambiguous common ancestor.
pub fn merge_base(project_root: &Path, revision: &DiffRevision) -> Result<String, DiffError> {
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "merge-base",
                "--all",
                &resolve(project_root, &revision.base)?,
                &resolve(project_root, &revision.head)?,
            ])
            .output()?,
    )?;
    let text = String::from_utf8_lossy(&output.stdout);
    let bases: Vec<_> = text.lines().collect();
    match bases.as_slice() {
        [base] => Ok((*base).into()),
        _ => Err(DiffError::Git(
            "historical comparison has no unique merge base".into(),
        )),
    }
}

/// Map a path from either comparison side using Git's rename identities.
pub fn renamed_path(
    project_root: &Path,
    revision: &DiffRevision,
    path: &str,
    from: DiffSide,
) -> Result<Option<String>, DiffError> {
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--name-status",
                "-z",
                "--find-renames",
                &resolve(project_root, &revision.base)?,
                &resolve(project_root, &revision.head)?,
                "--",
            ])
            .output()?,
    )?;
    let mut fields = output.stdout.split(|byte| *byte == 0);
    while let Some(status) = fields.next().filter(|field| !field.is_empty()) {
        let first = fields
            .next()
            .ok_or_else(|| DiffError::Git("missing changed path".into()))?;
        if matches!(status.first(), Some(b'R' | b'C')) {
            let second = fields
                .next()
                .ok_or_else(|| DiffError::Git("missing renamed path".into()))?;
            let (source, target) = match from {
                DiffSide::Base => (first, second),
                DiffSide::Head => (second, first),
            };
            if source == path.as_bytes() {
                return Ok(Some(String::from_utf8_lossy(target).into_owned()));
            }
        }
    }
    Ok(None)
}

fn resolve(project_root: &Path, revision: &str) -> Result<String, DiffError> {
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
            .env("GIT_NO_LAZY_FETCH", "1")
            .args([
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{revision}^{{commit}}"),
            ])
            .output()?,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn checked(output: Output) -> Result<Output, DiffError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(DiffError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}
