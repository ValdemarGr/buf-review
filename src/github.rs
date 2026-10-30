use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    process::Command,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource},
};

pub(crate) mod batch;
mod batch_ids;
mod intent;
mod publish;
mod recovery;
mod snapshot;
mod workspace;
pub use intent::*;
pub use publish::{PublishError, PublishResult, preflight};
pub use workspace::*;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("expected https://github.com/OWNER/REPO/pull/NUMBER")]
    InvalidUrl,
    #[error("could not run gh: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid GitHub response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("gh failed: {0}")]
    Gh(String),
    #[error("GitHub snapshot is incomplete or inconsistent: {0}")]
    Response(String),
}

pub struct FetchedReview {
    pub reviewer: User,
    pub read: ReadState,
    pub backend: ReviewBackend,
    pub branches: Option<BranchPair>,
}

/// Fetches provider data without opening or modifying the local store.
pub fn fetch(url: &Url) -> Result<FetchedReview, FetchError> {
    snapshot::fetch(&Address::from_url(url)?)
}

struct Address {
    owner: String,
    repo: String,
    number: i32,
}

impl Address {
    fn from_url(url: &Url) -> Result<Self, FetchError> {
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(FetchError::InvalidUrl);
        }
        let parts: Vec<_> = url
            .path()
            .trim_end_matches('/')
            .split('/')
            .skip(1)
            .collect();
        let [owner, repo, "pull", number] = parts.as_slice() else {
            return Err(FetchError::InvalidUrl);
        };
        if owner.is_empty() || repo.is_empty() {
            return Err(FetchError::InvalidUrl);
        }
        let number = number
            .parse::<i32>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or(FetchError::InvalidUrl)?;
        Ok(Self {
            owner: (*owner).into(),
            repo: (*repo).into(),
            number,
        })
    }
}

fn api<T: DeserializeOwned>(endpoint: &str, args: &[&str]) -> Result<T, FetchError> {
    let output = Command::new("gh")
        .args(["api", endpoint, "--hostname", "github.com"])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(FetchError::Gh(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn list<T: DeserializeOwned>(endpoint: &str) -> Result<Vec<T>, FetchError> {
    let pages: Vec<Vec<T>> = api(endpoint, &["--method", "GET", "--paginate", "--slurp"])?;
    Ok(pages.into_iter().flatten().collect())
}

fn read_range(
    line: Option<NonZeroU32>,
    start_line: Option<NonZeroU32>,
    side: Option<RestSide>,
    start_side: Option<RestSide>,
) -> Option<DiffRange> {
    let (line, side) = line.zip(side)?;
    let end = DiffLine {
        line,
        side: side.into(),
    };
    let start = match start_line {
        Some(line) => DiffLine {
            line,
            side: start_side?.into(),
        },
        None => end.clone(),
    };
    Some(DiffRange { start, end })
}

#[derive(Default)]
struct Identities(BTreeMap<(String, u64), IdMapping>);

impl Identities {
    fn mapping(&mut self, resource: &str, core_id: &str, remote_id: &str) {
        // Non-numeric provider metadata is kept separately from stable identities.
        let key = (format!("{resource}:{core_id}"), 0);
        self.0.insert(
            key,
            IdMapping {
                resource: resource.into(),
                core_id: core_id.into(),
                remote_id: remote_id.into(),
            },
        );
    }
    fn id(&mut self, resource: &str, remote_id: u64) -> String {
        self.0
            .entry((resource.into(), remote_id))
            .or_insert_with(|| IdMapping {
                resource: resource.into(),
                core_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("https://github.com/{resource}/{remote_id}").as_bytes(),
                )
                .to_string(),
                remote_id: remote_id.to_string(),
            })
            .core_id
            .clone()
    }

    fn user(&mut self, user: RestUser) -> User {
        User {
            id: UserId(self.id("user", user.id)),
            name: user.login,
        }
    }
}

#[derive(Deserialize, Clone)]
struct RestUser {
    id: u64,
    login: String,
}

#[derive(Deserialize)]
struct RestRef {
    sha: String,
}

#[derive(Deserialize)]
struct RestPullRequest {
    base: RestRef,
    head: RestRef,
    changed_files: usize,
}

#[derive(Deserialize)]
struct RestFile {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    #[serde(flatten)]
    stats: FileStats,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
enum RestSide {
    Left,
    Right,
}

impl From<RestSide> for DiffSide {
    fn from(side: RestSide) -> Self {
        match side {
            RestSide::Left => Self::Base,
            RestSide::Right => Self::Head,
        }
    }
}
