//! One snapshot query includes ordinary connections; only overflow is fetched again.
use super::*;
use serde_json::{Value, json};

const FRAGMENTS: &str = include_str!("snapshot.graphql");

fn response(query: &str, variables: Value) -> Result<Value, FetchError> {
    // Include only fragments reachable from this operation: unused fragments are
    // invalid GraphQL, and nested limits must stay below GitHub's node budget.
    let mut fragments = String::new();
    let mut needed = query.to_owned();
    let definitions = FRAGMENTS.split("fragment ").skip(1).collect::<Vec<_>>();
    loop {
        let mut added = false;
        for definition in &definitions {
            let name = definition.split_whitespace().next().expect("fragment name");
            if needed.contains(&format!("...{name}"))
                && !fragments.contains(&format!("fragment {name} "))
            {
                let definition = format!("fragment {definition}");
                needed.push_str(&definition);
                fragments.push_str(&definition);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    let value = publish::request(
        "graphql",
        "POST",
        &json!({"query":format!("{query}\n{fragments}"),"variables":variables}),
    )
    .map_err(|error| FetchError::Response(error.to_string()))?;
    if let Some(errors) = value
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        return Err(FetchError::Response(
            errors
                .iter()
                .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    value
        .get("data")
        .filter(|data| !data.is_null())
        .cloned()
        .ok_or_else(|| FetchError::Response("GraphQL snapshot data missing".into()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    total_count: usize,
    nodes: Vec<Option<T>>,
    page_info: PageInfo,
}

fn pages<T: DeserializeOwned>(
    mut page: Connection<T>,
    query: &str,
    mut variables: Value,
    pointer: &str,
    key: impl Fn(&T) -> String,
    progress: Option<(&str, &str)>,
) -> Result<Vec<T>, FetchError> {
    let total = page.total_count;
    if let Some((stage, message)) = progress {
        crate::progress::report(stage, message, Some((0, total)));
    }
    let mut nodes = Vec::new();
    let mut identities = BTreeSet::new();
    let mut cursors = BTreeSet::new();
    loop {
        if page.total_count != total {
            return Err(FetchError::Response(
                "connection count changed during pagination".into(),
            ));
        }
        for node in page.nodes {
            let node = node.ok_or_else(|| {
                FetchError::Response("GraphQL returned an unavailable connection node".into())
            })?;
            if !identities.insert(key(&node)) {
                return Err(FetchError::Response(
                    "duplicate node in GraphQL pagination".into(),
                ));
            }
            nodes.push(node);
        }
        if nodes.len() > total {
            return Err(FetchError::Response(
                "GraphQL connection pagination incomplete".into(),
            ));
        }
        if !page.page_info.has_next_page {
            if nodes.len() != total {
                return Err(FetchError::Response(
                    "GraphQL connection pagination incomplete".into(),
                ));
            }
            if let Some((stage, message)) = progress {
                crate::progress::report(stage, message, Some((nodes.len(), total)));
            }
            return Ok(nodes);
        }
        if let Some((stage, message)) = progress {
            crate::progress::report(stage, message, Some((nodes.len(), total)));
        }
        let cursor = page
            .page_info
            .end_cursor
            .filter(|cursor| !cursor.is_empty())
            .ok_or_else(|| FetchError::Response("GraphQL pagination cursor missing".into()))?;
        if !cursors.insert(cursor.clone()) {
            return Err(FetchError::Response(
                "GraphQL pagination repeated a cursor".into(),
            ));
        }
        variables["cursor"] = json!(cursor);
        let data = response(query, variables.clone())?;
        page = serde_json::from_value(
            data.pointer(pointer)
                .filter(|value| !value.is_null())
                .cloned()
                .ok_or_else(|| FetchError::Response("GraphQL connection missing".into()))?,
        )?;
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DatabaseId {
    Number(u64),
    Text(String),
}
impl DatabaseId {
    fn number(&self) -> Result<u64, FetchError> {
        match self {
            Self::Number(id) => Ok(*id),
            Self::Text(id) => id
                .parse()
                .map_err(|_| FetchError::Response("invalid fullDatabaseId".into())),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Actor {
    login: String,
    database_id: Option<u64>,
    user: Option<Box<Actor>>,
}
impl Actor {
    fn user(self, identities: &mut Identities) -> Result<User, FetchError> {
        let id = self
            .database_id
            .or_else(|| self.user.as_ref().and_then(|user| user.database_id))
            .ok_or_else(|| FetchError::Response("actor numeric identity missing".into()))?;
        Ok(identities.user(RestUser {
            id,
            login: self.login,
        }))
    }
}
#[derive(Deserialize)]
struct Reaction {
    id: String,
    content: String,
    user: Option<Actor>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    path: String,
    change_type: String,
    viewer_viewed_state: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    id: String,
    full_database_id: DatabaseId,
    body: String,
    author: Option<Actor>,
    created_at: String,
    reactions: Connection<Reaction>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewNode {
    id: String,
    full_database_id: DatabaseId,
    body: String,
    state: String,
    submitted_at: Option<String>,
    author: Option<Actor>,
    reactions: Connection<Reaction>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reference {
    full_database_id: DatabaseId,
}
#[derive(Deserialize)]
struct Commit {
    oid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InlineComment {
    #[serde(flatten)]
    comment: Comment,
    reply_to: Option<Reference>,
    pull_request_review: Option<Reference>,
    commit: Option<Commit>,
    original_commit: Option<Commit>,
    path: String,
    line: Option<NonZeroU32>,
    start_line: Option<NonZeroU32>,
    original_line: Option<NonZeroU32>,
    original_start_line: Option<NonZeroU32>,
    subject_type: Option<String>,
    diff_hunk: Option<String>,
    outdated: bool,
    state: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    diff_side: RestSide,
    start_diff_side: Option<RestSide>,
    comments: Connection<InlineComment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    id: String,
    full_database_id: DatabaseId,
    title: String,
    body: String,
    author: Option<Actor>,
    state: String,
    base_ref_oid: String,
    head_ref_oid: String,
    head_ref_name: String,
    #[serde(default)]
    base_ref_name: Option<String>,
    head_repository: Option<HeadRepository>,
    changed_files: usize,
    files: Connection<File>,
    comments: Connection<Comment>,
    reviews: Connection<ReviewNode>,
    review_threads: Connection<Thread>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    viewer: Actor,
    repository: Option<Repository>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    pull_request: Option<Pull>,
}

#[derive(Deserialize)]
struct HeadRepository {
    url: String,
}

fn pull_page(field: &str, fragment: &str) -> String {
    format!(
        "query Review{fragment}($owner:String!,$name:String!,$number:Int!,$cursor:String){{repository(owner:$owner,name:$name){{pullRequest(number:$number){{{field}(first:50,after:$cursor){{...{fragment}}}}}}}}}"
    )
}
fn reactions(
    node: &str,
    page: Connection<Reaction>,
    comment: &CommentId,
    identities: &mut Identities,
    values: &mut Vec<CommentReaction>,
) -> Result<(), FetchError> {
    if page.page_info.has_next_page {
        crate::progress::report("fetch_reactions", "Fetching reactions", None);
    }
    let query = "query CommentReactions($id:ID!,$cursor:String){node(id:$id){... on Reactable{reactions(first:100,after:$cursor){...ReactionPage}}}}";
    for reaction in pages(
        page,
        query,
        json!({"id":node}),
        "/node/reactions",
        |reaction| reaction.id.clone(),
        None,
    )? {
        let value = match reaction.content.as_str() {
            "THUMBS_UP" => "👍",
            "THUMBS_DOWN" => "👎",
            "LAUGH" => "😄",
            "HOORAY" => "🎉",
            "CONFUSED" => "😕",
            "HEART" => "❤️",
            "ROCKET" => "🚀",
            "EYES" => "👀",
            other => return Err(FetchError::Response(format!("unknown reaction: {other}"))),
        };
        values.push(CommentReaction {
            comment: comment.clone(),
            author: reaction
                .user
                .map(|user| user.user(identities).map(|user| user.id))
                .transpose()?,
            reaction: value.into(),
        });
    }
    Ok(())
}

pub(super) fn fetch(address: &Address) -> Result<FetchedReview, FetchError> {
    crate::progress::report("fetch_pr", "Fetching PR", None);
    let variables = json!({"owner":address.owner,"name":address.repo,"number":address.number});
    let query = "query ReviewSnapshot($owner:String!,$name:String!,$number:Int!){viewer{login databaseId} repository(owner:$owner,name:$name){pullRequest(number:$number){id fullDatabaseId title body author{...ActorIdentity} state baseRefOid headRefOid headRefName baseRefName headRepository{url} changedFiles files(first:100){...FilePage} comments(first:100){...IssueCommentPage} reviews(first:50){...ReviewPage} reviewThreads(first:50){...ThreadPage}}}}";
    let snapshot: Snapshot = serde_json::from_value(response(query, variables.clone())?)?;
    let pull = snapshot
        .repository
        .and_then(|repo| repo.pull_request)
        .ok_or_else(|| FetchError::Response("GraphQL did not return the pull request".into()))?;
    if pull.changed_files > 3000 {
        return Err(FetchError::Response(
            "GitHub's files endpoint is limited to 3000 files".into(),
        ));
    }
    let revision = DiffRevision {
        base: pull.base_ref_oid,
        head: pull.head_ref_oid,
    };
    let endpoint = format!(
        "repos/{}/{}/pulls/{}",
        address.owner, address.repo, address.number
    );
    let files = pages(
        pull.files,
        &pull_page("files", "FilePage"),
        variables.clone(),
        "/repository/pullRequest/files",
        |file| file.path.clone(),
        Some(("fetch_files", "Files")),
    )?;
    crate::progress::report("fetch_file_stats", "Fetching file stats", None);
    let rest_files: Vec<RestFile> = list(&format!("{endpoint}/files?per_page=100"))?;
    let mut viewed = files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    if rest_files.len() != pull.changed_files || viewed.len() != pull.changed_files {
        return Err(FetchError::Response(
            "changed file counts or viewed-file paths disagree".into(),
        ));
    }
    let files = rest_files
        .into_iter()
        .map(|file| {
            let viewed = viewed.remove(&file.filename).ok_or_else(|| {
                FetchError::Response("changed file counts or viewed-file paths disagree".into())
            })?;
            let change = match file.status.as_str() {
                "added" => FileChange::Added,
                "modified" => FileChange::Modified,
                "removed" => FileChange::Deleted,
                "renamed" => FileChange::Renamed,
                "copied" => FileChange::Copied,
                "changed" => FileChange::TypeChanged,
                "unchanged" => FileChange::Unchanged,
                other => {
                    return Err(FetchError::Response(format!(
                        "unknown file change: {other}"
                    )));
                }
            };
            let graphql_change = match viewed.change_type.as_str() {
                "ADDED" => FileChange::Added,
                "MODIFIED" => FileChange::Modified,
                "DELETED" => FileChange::Deleted,
                "RENAMED" => FileChange::Renamed,
                "COPIED" => FileChange::Copied,
                "CHANGED" => FileChange::TypeChanged,
                "UNCHANGED" => FileChange::Unchanged,
                other => {
                    return Err(FetchError::Response(format!(
                        "unknown GraphQL file change: {other}"
                    )));
                }
            };
            if change != graphql_change {
                return Err(FetchError::Response("file change types disagree".into()));
            }
            let is_viewed = match viewed.viewer_viewed_state.as_str() {
                "VIEWED" => true,
                "UNVIEWED" | "DISMISSED" => false,
                other => {
                    return Err(FetchError::Response(format!(
                        "unknown viewed state: {other}"
                    )));
                }
            };
            Ok(ChangedFile {
                path: file.filename,
                previous_path: file.previous_filename,
                change,
                stats: Some(file.stats),
                viewed: is_viewed,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::progress::report(
        "fetch_file_stats",
        "File stats",
        Some((files.len(), pull.changed_files)),
    );
    let issue_comments = pages(
        pull.comments,
        &pull_page("comments", "IssueCommentPage"),
        variables.clone(),
        "/repository/pullRequest/comments",
        |comment| comment.id.clone(),
        Some(("fetch_comments", "PR comments")),
    )?;
    let threads = pages(
        pull.review_threads,
        &pull_page("reviewThreads", "ThreadPage"),
        variables.clone(),
        "/repository/pullRequest/reviewThreads",
        |thread| thread.id.clone(),
        Some(("fetch_threads", "Threads")),
    )?;
    let reviews = pages(
        pull.reviews,
        &pull_page("reviews", "ReviewPage"),
        variables,
        "/repository/pullRequest/reviews",
        |review| review.id.clone(),
        Some(("fetch_reviews", "Reviews")),
    )?;
    let mut identities = Identities::default();
    let reviewer = snapshot.viewer.user(&mut identities)?;
    let mut comments = Vec::new();
    let mut comment_reactions = Vec::new();
    for comment in issue_comments {
        let id = CommentId(identities.id("issue_comment", comment.full_database_id.number()?));
        identities.mapping("issue_comment_node", &id.0, &comment.id);
        identities.mapping(&format!("created_at:{}", comment.created_at), &id.0, &id.0);
        reactions(
            &comment.id,
            comment.reactions,
            &id,
            &mut identities,
            &mut comment_reactions,
        )?;
        comments.push(ReadComment {
            content: CommentContent {
                id,
                body: comment.body,
            },
            author: comment
                .author
                .map(|author| author.user(&mut identities))
                .transpose()?,
            target: CommentTarget::PullRequest,
            draft: false,
        });
    }
    for thread in threads {
        if thread.comments.page_info.has_next_page {
            crate::progress::report("fetch_thread_comments", "Fetching thread comments", None);
        }
        let query = "query ThreadComments($id:ID!,$cursor:String){node(id:$id){... on PullRequestReviewThread{comments(first:100,after:$cursor){...ReviewCommentPage}}}}";
        for inline in pages(
            thread.comments,
            query,
            json!({"id":thread.id}),
            "/node/comments",
            |comment| comment.comment.id.clone(),
            None,
        )? {
            let draft = match inline.state.as_str() {
                "PENDING" => true,
                "SUBMITTED" => false,
                other => {
                    return Err(FetchError::Response(format!(
                        "unknown inline comment state: {other}"
                    )));
                }
            };
            let comment = inline.comment;
            let numeric = comment.full_database_id.number()?;
            let id = CommentId(identities.id("review_comment", numeric));
            identities.mapping("review_comment_node", &id.0, &comment.id);
            if inline.reply_to.is_none() {
                identities.mapping("review_thread", &id.0, &thread.id);
            }
            identities.mapping(&format!("created_at:{}", comment.created_at), &id.0, &id.0);
            if let Some(review) = inline.pull_request_review {
                identities.mapping(
                    &format!("review_member:{}", review.full_database_id.number()?),
                    &id.0,
                    &numeric.to_string(),
                );
            }
            reactions(
                &comment.id,
                comment.reactions,
                &id,
                &mut identities,
                &mut comment_reactions,
            )?;
            let target = if let Some(parent) = inline.reply_to {
                CommentTarget::Reply(CommentId(
                    identities.id("review_comment", parent.full_database_id.number()?),
                ))
            } else {
                // GraphQL commit objects can disappear after history changes;
                // REST retains their SHAs. Fetch only the affected root.
                #[derive(Deserialize)]
                struct RestAnchor {
                    commit_id: String,
                    original_commit_id: String,
                    start_side: Option<RestSide>,
                }
                let supplement: Option<RestAnchor> = if inline.original_commit.is_none()
                    || inline.commit.is_none()
                    || ((inline.original_start_line.is_some() || inline.start_line.is_some())
                        && thread.start_diff_side.is_none())
                {
                    Some(api(
                        &format!(
                            "repos/{}/{}/pulls/comments/{numeric}",
                            address.owner, address.repo
                        ),
                        &["--method", "GET"],
                    )?)
                } else {
                    None
                };
                let side = Some(thread.diff_side);
                let start_side = thread
                    .start_diff_side
                    .or_else(|| supplement.as_ref().and_then(|anchor| anchor.start_side));
                let original_commit = inline
                    .original_commit
                    .map(|commit| commit.oid)
                    .or_else(|| {
                        supplement
                            .as_ref()
                            .map(|anchor| anchor.original_commit_id.clone())
                    })
                    .ok_or_else(|| {
                        FetchError::Response("original comment commit missing".into())
                    })?;
                let commit = inline
                    .commit
                    .map(|commit| commit.oid)
                    .or_else(|| supplement.as_ref().map(|anchor| anchor.commit_id.clone()))
                    .ok_or_else(|| FetchError::Response("current comment commit missing".into()))?;
                let original_range = read_range(
                    inline.original_line,
                    inline.original_start_line,
                    side,
                    start_side,
                );
                let current_range = read_range(inline.line, inline.start_line, side, start_side);
                let subject = match inline.subject_type.as_deref() {
                    Some("LINE") => Some(CommentSubject::Line),
                    Some("FILE") => Some(CommentSubject::File),
                    None => None,
                    Some(other) => {
                        return Err(FetchError::Response(format!(
                            "unknown comment subject: {other}"
                        )));
                    }
                };
                if (inline.original_line.is_some() && original_range.is_none())
                    || (inline.line.is_some() && current_range.is_none())
                {
                    return Err(FetchError::Response(
                        "comment diff side metadata missing".into(),
                    ));
                }
                CommentTarget::Diff(ReadDiffLocation {
                    original: ReadCommentAnchor {
                        revision: ReadCommentRevision::Commit {
                            head: original_commit,
                        },
                        position: CommentPosition {
                            path: inline.path.clone(),
                            range: original_range,
                        },
                    },
                    current: (current_range.is_some() || subject == Some(CommentSubject::File))
                        .then_some(ReadCommentAnchor {
                            revision: ReadCommentRevision::Commit { head: commit },
                            position: CommentPosition {
                                path: inline.path,
                                range: current_range,
                            },
                        }),
                    subject,
                    diff_hunk: inline.diff_hunk,
                    outdated: Some(inline.outdated),
                })
            };
            comments.push(ReadComment {
                content: CommentContent {
                    id,
                    body: comment.body,
                },
                author: comment
                    .author
                    .map(|author| author.user(&mut identities))
                    .transpose()?,
                target,
                draft,
            });
        }
    }
    let reviews = reviews
        .into_iter()
        .map(|review| {
            let numeric = review.full_database_id.number()?;
            let id = identities.id("review", numeric);
            identities.mapping("review_resource_node", &id, &review.id);
            let author = review
                .author
                .map(|author| author.user(&mut identities))
                .transpose()?;
            if review.state != "PENDING" && !review.body.trim().is_empty() {
                let comment = CommentId(identities.id("review_summary", numeric));
                if let Some(created) = review.submitted_at {
                    identities.mapping(&format!("created_at:{created}"), &comment.0, &comment.0);
                }
                identities.mapping("review_node", &comment.0, &review.id);
                reactions(
                    &review.id,
                    review.reactions,
                    &comment,
                    &mut identities,
                    &mut comment_reactions,
                )?;
                comments.push(ReadComment {
                    content: CommentContent {
                        id: comment,
                        body: review.body.clone(),
                    },
                    author: author.clone(),
                    target: CommentTarget::PullRequest,
                    draft: false,
                });
            }
            let body = Some(review.body);
            let state = match review.state.as_str() {
                "APPROVED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Approve,
                    body,
                }),
                "CHANGES_REQUESTED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::RequestChanges,
                    body,
                }),
                "COMMENTED" => ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Comment,
                    body,
                }),
                "DISMISSED" => ReadReviewState::Inactive {
                    state: InactiveReviewState::Dismissed,
                    body,
                },
                "PENDING" => ReadReviewState::Inactive {
                    state: InactiveReviewState::Pending,
                    body,
                },
                other => {
                    return Err(FetchError::Response(format!(
                        "unknown review state: {other}"
                    )));
                }
            };
            Ok(ReadReview {
                id: Some(ReviewResourceId(id)),
                author,
                state,
            })
        })
        .collect::<Result<Vec<_>, FetchError>>()?;
    crate::progress::report("fetch_verify", "Verifying PR revision", None);
    let latest: RestPullRequest = api(&endpoint, &["--method", "GET"])?;
    if latest.base.sha != revision.base
        || latest.head.sha != revision.head
        || latest.changed_files != pull.changed_files
    {
        return Err(FetchError::Response(
            "PR revisions changed during fetch; fetch again".into(),
        ));
    }
    let status = match pull.state.as_str() {
        "OPEN" => PullRequestStatus::Open,
        "CLOSED" => PullRequestStatus::Closed,
        "MERGED" => PullRequestStatus::Merged,
        other => return Err(FetchError::Response(format!("unknown PR state: {other}"))),
    };
    let pull_id = PullRequestId(identities.id("pull_request", pull.full_database_id.number()?));
    identities.mapping("pull_request_node", &pull_id.0, &pull.id);
    let source = pull.head_repository.map(|repository| BranchSource {
        repository: repository.url,
        branch: pull.head_ref_name,
    });
    let branches = source
        .clone()
        .zip(pull.base_ref_name)
        .map(|(source, branch)| BranchPair {
            source,
            sink: BranchSource {
                repository: format!("https://github.com/{}/{}", address.owner, address.repo),
                branch,
            },
        });
    let fetched = FetchedReview {
        reviewer,
        read: ReadState {
            pull_request: PullRequest {
                id: pull_id,
                title: pull.title,
                body: pull.body,
                author: pull
                    .author
                    .map(|author| author.user(&mut identities))
                    .transpose()?,
                status,
                source,
                revision,
            },
            files,
            comments,
            reactions: comment_reactions,
            reviews,
        },
        branches,
        backend: ReviewBackend {
            source: ReviewSource {
                backend: "github".into(),
                locator: format!(
                    "https://github.com/{}/{}/pull/{}",
                    address.owner, address.repo, address.number
                ),
            },
            mappings: identities.0.into_values().collect(),
        },
    };
    crate::progress::report("fetch_pr", "PR fetched", Some((1, 1)));
    Ok(fetched)
}
