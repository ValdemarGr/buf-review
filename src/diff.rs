use std::{
    path::Path,
    process::{Command, Output},
};

use thiserror::Error;

use crate::model::DiffRevision;

#[derive(Debug, Error)]
pub enum DiffError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git: {0}")]
    Git(String),
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

fn resolve(project_root: &Path, revision: &str) -> Result<String, DiffError> {
    let output = checked(
        Command::new("git")
            .current_dir(project_root)
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
