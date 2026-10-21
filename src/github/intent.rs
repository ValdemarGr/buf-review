//! Local PR objects use committed Git objects until publication binds GitHub.
use super::workspace::{
    confirmed_rejection, git_value, github_remote, remote_branch_ref, repository_at,
};
use super::*;
use crate::store::Store;
use crate::workspace::Workspace;
use serde_json::Value;
use std::path::Path;

type Config = BTreeMap<String, Vec<String>>;

fn configuration(project: &Path) -> Result<Config, FetchError> {
    let values = git_value(project, &["config", "--null", "--list"])?
        .ok_or_else(|| FetchError::Response("could not read Git configuration".into()))?;
    let mut config = Config::new();
    for entry in values.split('\0').filter(|entry| !entry.is_empty()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        config
            .entry(key.to_owned())
            .or_default()
            .push(value.to_owned());
    }
    Ok(config)
}

fn setting<'a>(config: &'a Config, key: &str) -> Option<&'a str> {
    config.get(key)?.last().map(String::as_str)
}

/// Resolve all local push identities in one ref enumeration. Missing tracking
/// refs do not prevent an ordinary, not-yet-pushed branch from having an intent.
fn local_sources(project: &Path) -> Result<Vec<(String, BranchSource)>, FetchError> {
    let config = configuration(project)?;
    let refs = git_value(project, &["for-each-ref", "--format=%(refname)%00%(push:remotename)%00%(push:remoteref)%00%(push)%00%(upstream:remotename)%00%(upstream:remoteref)", "refs/"])?
        .ok_or_else(|| FetchError::Response("could not enumerate local branches".into()))?;
    let existing_refs = refs
        .lines()
        .filter_map(|row| row.split('\0').next())
        .collect::<BTreeSet<_>>();
    let remotes = git_value(project, &["remote"])?.unwrap_or_default();
    let remotes = remotes.lines().collect::<Vec<_>>();
    let mut repositories = BTreeMap::<String, Option<String>>::new();
    let mut sources = vec![];
    for row in refs.lines() {
        let fields = row.split('\0').collect::<Vec<_>>();
        let [
            reference,
            native_remote,
            native_ref,
            tracking,
            upstream_remote,
            upstream_ref,
        ] = fields.as_slice()
        else {
            return Err(FetchError::Response("invalid Git branch metadata".into()));
        };
        let Some(branch) = reference.strip_prefix("refs/heads/") else {
            continue;
        };
        let branch_push = format!("branch.{branch}.pushremote");
        let branch_remote = format!("branch.{branch}.remote");
        let explicit_remote =
            setting(&config, &branch_push).or_else(|| setting(&config, "remote.pushdefault"));
        let remote = explicit_remote
            .or_else(|| (!native_remote.is_empty()).then_some(*native_remote))
            .or_else(|| setting(&config, &branch_remote))
            .or_else(|| remotes.contains(&"origin").then_some("origin"))
            .or_else(|| (remotes.len() == 1).then(|| remotes[0]));
        let Some(remote) = remote.filter(|remote| !remote.is_empty() && *remote != ".") else {
            continue;
        };
        let push_specs = config.get(&format!("remote.{remote}.push"));
        let push_ref = if !native_ref.is_empty() {
            Some((*native_ref).to_owned())
        } else if !tracking.is_empty()
            && (setting(&config, "push.default") != Some("matching")
                || existing_refs.contains(tracking))
        {
            config
                .get(&format!("remote.{remote}.fetch"))
                .and_then(|specs| remote_branch_ref(tracking, &specs.join("\n")))
        } else {
            None
        };
        let remote_reference = push_ref.or_else(|| {
            if push_specs.is_some() {
                return None;
            }
            let mode = setting(&config, "push.default").unwrap_or("simple");
            if matches!(mode, "upstream" | "tracking")
                && !upstream_ref.is_empty()
                && remote == *upstream_remote
            {
                Some((*upstream_ref).to_owned())
            } else if mode == "current"
                || mode == "simple"
                    && (upstream_ref.is_empty()
                        || remote != *upstream_remote
                        || *upstream_ref == format!("refs/heads/{branch}"))
            {
                Some(format!("refs/heads/{branch}"))
            } else {
                None
            }
        });
        let Some(destination) = remote_reference
            .and_then(|reference| reference.strip_prefix("refs/heads/").map(str::to_owned))
        else {
            continue;
        };
        let repository = if let Some(repository) = repositories.get(remote) {
            repository.clone()
        } else {
            let urls = git_value(project, &["remote", "get-url", "--push", "--all", remote])?;
            let repository = urls.and_then(|urls| {
                let mut repositories = urls
                    .lines()
                    .map(github_remote)
                    .collect::<Option<BTreeSet<_>>>()?;
                (repositories.len() == 1)
                    .then(|| repositories.pop_first())
                    .flatten()
            });
            repositories.insert(remote.to_owned(), repository.clone());
            repository
        };
        if let Some(repository) = repository {
            sources.push((
                (*reference).to_owned(),
                BranchSource {
                    repository: format!("https://github.com/{repository}"),
                    branch: destination,
                },
            ));
        }
    }
    Ok(sources)
}

fn same_source(left: &BranchSource, right: &BranchSource) -> bool {
    left.branch == right.branch
        && github_remote(&left.repository) == github_remote(&right.repository)
}

pub fn pr_branches(project: &Path, base: &str) -> Result<BranchPair, FetchError> {
    let current = git_value(project, &["symbolic-ref", "--quiet", "HEAD"])?.ok_or_else(|| {
        FetchError::Response("Review pr requires a local branch; HEAD is detached".into())
    })?;
    let source = local_sources(project)?.into_iter().find(|(reference, _)| *reference == current)
        .map(|(_, source)| source)
        .ok_or_else(|| FetchError::Response("cannot determine this branch's GitHub push destination; configure its push remote/ref, then retry".into()))?;
    let check = Command::new("git")
        .current_dir(project)
        .args(["check-ref-format", "--branch", base])
        .output()?;
    if !check.status.success() || base.starts_with('-') {
        return Err(FetchError::Response(
            "Review pr requires a base branch name".into(),
        ));
    }
    if git_value(
        project,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{base}^{{commit}}"),
        ],
    )?
    .is_none()
    {
        return Err(FetchError::Response(format!(
            "local base branch {base} does not exist; fetch/create it first"
        )));
    }
    let sink = BranchSource {
        repository: format!("https://github.com/{}", repository_at(project)?),
        branch: base.to_owned(),
    };
    if same_source(&source, &sink) {
        return Err(FetchError::Response(
            "source and base must be different branches".into(),
        ));
    }
    Ok(BranchPair { source, sink })
}

pub fn local_read(project: &Path, object: &PullRequestObject) -> Result<ReadState, FetchError> {
    let PullRequestSource::LocalIntent { branches } = &object.source else {
        return Err(FetchError::Response(
            "PR already has a remote source".into(),
        ));
    };
    let candidates = local_sources(project)?
        .into_iter()
        .filter(|(_, source)| same_source(source, &branches.source))
        .collect::<Vec<_>>();
    let [(source, _)] = candidates.as_slice() else {
        return Err(FetchError::Response("saved PR source must match exactly one local branch; configure its push destination before refreshing".into()));
    };
    let head = git_value(
        project,
        &["rev-parse", "--verify", &format!("{source}^{{commit}}")],
    )?
    .ok_or_else(|| FetchError::Response("local PR source commit is unavailable".into()))?;
    let base = git_value(
        project,
        &[
            "merge-base",
            &format!("refs/heads/{}", branches.sink.branch),
            &head,
        ],
    )?
    .ok_or_else(|| FetchError::Response("local PR branches have no available merge base".into()))?;
    let revision = DiffRevision { base, head };
    let files = crate::diff::files(project, &revision)
        .map_err(|error| FetchError::Response(error.to_string()))?;
    Ok(ReadState {
        pull_request: PullRequest {
            id: object.id.clone(),
            title: object.title.clone(),
            body: object.body.clone(),
            author: None,
            status: PullRequestStatus::Local,
            source: Some(branches.source.clone()),
            revision,
        },
        files,
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    })
}

#[derive(Deserialize)]
struct RepositoryName {
    full_name: String,
}
#[derive(Deserialize)]
struct RemoteBranch {
    #[serde(rename = "ref")]
    branch: String,
    repo: Option<RepositoryName>,
    sha: String,
}
#[derive(Deserialize)]
struct IntentPull {
    id: u64,
    number: u64,
    html_url: String,
    state: String,
    title: String,
    body: Option<String>,
    head: RemoteBranch,
    base: RemoteBranch,
}

impl IntentPull {
    fn matches(&self, branches: &BranchPair) -> bool {
        self.state == "open"
            && self.number > 0
            && self.id > 0
            && self.head.branch == branches.source.branch
            && self.base.branch == branches.sink.branch
            && self.head.repo.as_ref().is_some_and(|repo| {
                Some(repo.full_name.to_ascii_lowercase())
                    == github_remote(&branches.source.repository)
            })
            && self.base.repo.as_ref().is_some_and(|repo| {
                Some(repo.full_name.to_ascii_lowercase())
                    == github_remote(&branches.sink.repository)
            })
    }
}

fn repository_name(source: &BranchSource) -> Result<String, FetchError> {
    github_remote(&source.repository).ok_or_else(|| {
        FetchError::Response("PR branch repository must be a GitHub repository".into())
    })
}

fn matching_pr(branches: &BranchPair) -> Result<Option<IntentPull>, FetchError> {
    let repository = repository_name(&branches.sink)?;
    let source = repository_name(&branches.source)?;
    let owner = source.split('/').next().expect("validated repository");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("state", "open")
        .append_pair("head", &format!("{owner}:{}", branches.source.branch))
        .append_pair("base", &branches.sink.branch)
        .append_pair("per_page", "100")
        .finish();
    let mut matches = list::<IntentPull>(&format!("repos/{repository}/pulls?{query}"))?
        .into_iter()
        .filter(|pull| pull.matches(branches))
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(FetchError::Response(
            "GitHub returned multiple open PRs for the exact branch pair".into(),
        )),
    }
}

pub fn matching_pr_url(branches: &BranchPair) -> Result<Option<String>, FetchError> {
    matching_pr(branches).map(|pull| pull.map(|pull| pull.html_url))
}

fn require_pushed(
    project: &Path,
    reviewed_head: &str,
    remote_head: &str,
    branches: &BranchPair,
) -> Result<(), FetchError> {
    if reviewed_head != remote_head {
        return Err(FetchError::Response("not at remote head".into()));
    }
    if !matches!(remote_head.len(), 40 | 64)
        || !remote_head.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(FetchError::Response(
            "remote source branch lacks a valid commit identity".into(),
        ));
    }
    crate::diff::fetch_missing_commits(
        project,
        &[remote_head.to_owned()],
        &branches.source.repository,
    )
    .map_err(|error| FetchError::Response(error.to_string()))?;
    let available = Command::new("git")
        .current_dir(project)
        .args(["merge-base", "--is-ancestor", reviewed_head, remote_head])
        .output()?;
    if !available.status.success() {
        return Err(FetchError::Response("reviewed commits are not present on the remote source branch; push with Git/Fugitive before publishing".into()));
    }
    Ok(())
}

fn bind(
    store: &mut Store,
    object: &PullRequestObject,
    pull: IntentPull,
    branches: &BranchPair,
    reviewer: &User,
    remote_user: &RestUser,
) -> Result<(), WorkspacePublishError> {
    let url = Url::parse(&pull.html_url).map_err(|_| FetchError::InvalidUrl)?;
    let address = Address::from_url(&url)?;
    if format!("{}/{}", address.owner, address.repo).to_ascii_lowercase()
        != repository_name(&branches.sink)?
        || address.number as u64 != pull.number
    {
        return Err(FetchError::Response(
            "GitHub PR identity does not match its branch pair".into(),
        )
        .into());
    }
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: pull.html_url,
        },
        mappings: vec![
            IdMapping {
                resource: "pull_request".into(),
                core_id: object.id.0.clone(),
                remote_id: pull.id.to_string(),
            },
            IdMapping {
                resource: "user".into(),
                core_id: reviewer.id.0.clone(),
                remote_id: remote_user.id.to_string(),
            },
        ],
    };
    store.bind_pull_request(
        &object.id,
        object.revision,
        &backend,
        Some(branches),
        Some((&pull.title, pull.body.as_deref().unwrap_or_default())),
    )?;
    Ok(())
}

/// Binding precedes comment publication and survives later comment failures.
pub fn resolve_pull_request(
    store: &mut Store,
    project: &Path,
    workspace: &Workspace,
) -> Result<(), WorkspacePublishError> {
    let id = &workspace.review.read.pull_request.id;
    let Some(mut object) = store.pull_request(id)? else {
        return Ok(());
    };
    let PullRequestSource::LocalIntent { branches } = object.source.clone() else {
        let backend = store.review_backend(&workspace.review.id)?;
        let missing_reviewer = backend.as_ref().is_some_and(|backend| {
            !backend.mappings.iter().any(|mapping| {
                mapping.resource == "user" && mapping.core_id == workspace.review.reviewer.id.0
            })
        });
        let local_baseline = workspace.review.read.pull_request.status == PullRequestStatus::Local;
        if missing_reviewer || local_baseline {
            let remote_user: RestUser = api("user", &["--method", "GET"])?;
            let user = Identities::default().user(remote_user.clone());
            if user.id != workspace.review.reviewer.id {
                return Err(FetchError::Response(
                    "authenticated user differs from saved reviewer".into(),
                )
                .into());
            }
            if missing_reviewer {
                store.map_review_reviewer(
                    &workspace.review.id,
                    &user.id,
                    &remote_user.id.to_string(),
                )?;
            }
            if local_baseline {
                let PullRequestSource::Remote {
                    locator,
                    branches: Some(branches),
                } = &object.source
                else {
                    return Err(FetchError::Response(
                        "bound local PR lacks its source branch metadata".into(),
                    )
                    .into());
                };
                let url = Url::parse(locator).map_err(|_| FetchError::InvalidUrl)?;
                let address = Address::from_url(&url)?;
                let pull: Value = api(
                    &format!(
                        "repos/{}/{}/pulls/{}",
                        address.owner, address.repo, address.number
                    ),
                    &["--method", "GET"],
                )?;
                let head = pull
                    .pointer("/head/sha")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        FetchError::Response("remote PR lacks its source commit".into())
                    })?;
                require_pushed(project, &workspace.comparison.head, head, branches)?;
            }
        }
        return Ok(());
    };
    let remote_user: RestUser = api("user", &["--method", "GET"])?;
    let user = Identities::default().user(remote_user.clone());
    if user.id != workspace.review.reviewer.id {
        return Err(
            FetchError::Response("authenticated user differs from saved reviewer".into()).into(),
        );
    }
    if let Some(pull) = matching_pr(&branches)? {
        require_pushed(
            project,
            &workspace.comparison.head,
            &pull.head.sha,
            &branches,
        )?;
        return bind(store, &object, pull, &branches, &user, &remote_user);
    }
    if store.pr_resolution(id)?.is_some() {
        return Err(FetchError::Response("PR creation result is uncertain; no matching open PR found. Inspect GitHub before retrying; no duplicate creation sent".into()).into());
    }
    if object.title.trim().is_empty() {
        return Err(FetchError::Response(
            "PR title is empty; save its title/body before publishing".into(),
        )
        .into());
    }
    let source_repository = repository_name(&branches.source)?;
    let mut branch_url = Url::parse("https://api.github.com/").expect("constant URL");
    branch_url.path_segments_mut().expect("base URL").extend([
        "repos",
        source_repository.split('/').next().unwrap(),
        source_repository.split('/').nth(1).unwrap(),
        "branches",
        &branches.source.branch,
    ]);
    let remote_branch: Value = api(
        branch_url.path().trim_start_matches('/'),
        &["--method", "GET"],
    )
    .map_err(|error| {
        FetchError::Response(format!(
            "remote source branch is unavailable; push with Git/Fugitive before publishing: {error}"
        ))
    })?;
    let head = remote_branch
        .pointer("/commit/sha")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            FetchError::Response(
                "remote source branch is unavailable; push it with Git/Fugitive before publishing"
                    .into(),
            )
        })?;
    require_pushed(project, &workspace.comparison.head, head, &branches)?;
    if workspace.worktree_context
        && crate::diff::resolve(project, "HEAD")
            .map_err(|error| FetchError::Response(error.to_string()))?
            != workspace.comparison.head
    {
        return Err(FetchError::Response(
            "Git HEAD changed while preparing publication; reopen diff".into(),
        )
        .into());
    }
    let resolution = PullRequestResolution {
        branches: branches.clone(),
        title: object.title.clone(),
        body: object.body.clone(),
        draft: true,
    };
    object = store.mark_pr_resolution(id, object.revision, &resolution)?;
    let repository = repository_name(&branches.sink)?;
    let (owner, source_repo) = source_repository
        .split_once('/')
        .expect("validated repository");
    let result = publish::request(
        &format!("repos/{repository}/pulls"),
        "POST",
        &serde_json::json!({
            "title":resolution.title, "body":resolution.body, "head":format!("{owner}:{}", branches.source.branch),
            "head_repo":source_repo, "base":branches.sink.branch, "draft":resolution.draft,
        }),
    );
    match result {
        Ok(response) => {
            let pull: IntentPull = serde_json::from_value(response).map_err(FetchError::from)?;
            if !pull.matches(&branches) {
                return Err(FetchError::Response("created PR response did not confirm the requested branch pair; creation remains uncertain".into()).into());
            }
            bind(store, &object, pull, &branches, &user, &remote_user)
        }
        Err(error) => {
            if confirmed_rejection(&error) {
                // A confirmed rejection cannot become an uncertain creation if
                // the reconciliation read itself fails.
                object = store.clear_pr_resolution(id, object.revision)?;
                // Another client may have created the unique open PR concurrently.
                if let Some(pull) = matching_pr(&branches)? {
                    require_pushed(
                        project,
                        &workspace.comparison.head,
                        &pull.head.sha,
                        &branches,
                    )?;
                    return bind(store, &object, pull, &branches, &user, &remote_user);
                }
            }
            Err(error.into())
        }
    }
}
