use std::num::NonZeroU32;

use buf_review::core::{Change, CoreError, ProjectedComment, prepare, project};
use buf_review::model::*;

#[test]
fn cached_pull_request_without_branch_source_remains_readable() {
    let cached = serde_json::to_value(review().read.pull_request).unwrap();
    assert!(cached.get("source").is_none());
    let mut pull: PullRequest = serde_json::from_value(cached.clone()).unwrap();
    assert_eq!(pull.source, None);
    assert_eq!(serde_json::to_value(&pull).unwrap(), cached);
    pull.source = Some(BranchSource {
        repository: "https://github.com/contributor/repo".into(),
        branch: "feature/review".into(),
    });
    assert_eq!(
        serde_json::from_value::<PullRequest>(serde_json::to_value(&pull).unwrap()).unwrap(),
        pull
    );
}

#[test]
fn changed_file_without_cached_stats_remains_unknown_and_zero_counts_round_trip() {
    let cached = serde_json::json!({
        "path": "src/lib.rs", "previous_path": null, "change": "modified", "viewed": false
    });
    let mut file: ChangedFile = serde_json::from_value(cached.clone()).unwrap();
    assert_eq!(file.stats, None);
    assert_eq!(serde_json::to_value(&file).unwrap(), cached);
    file.stats = Some(FileStats {
        additions: 0,
        deletions: 0,
    });
    let serialized = serde_json::to_value(&file).unwrap();
    assert_eq!(
        serialized["stats"],
        serde_json::json!({"additions": 0, "deletions": 0})
    );
    assert_eq!(
        serde_json::from_value::<ChangedFile>(serialized).unwrap(),
        file
    );
}

fn user() -> User {
    User {
        id: UserId("me".into()),
        name: "Me".into(),
    }
}
fn id(value: &str) -> CommentId {
    CommentId(value.into())
}
fn location() -> DiffLocation {
    DiffLocation {
        revision: DiffRevision {
            base: "base".into(),
            head: "head".into(),
        },
        position: CommentPosition {
            path: "src/lib.rs".into(),
            range: Some(DiffRange {
                start: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(3).unwrap(),
                },
                end: DiffLine {
                    side: DiffSide::Head,
                    line: NonZeroU32::new(5).unwrap(),
                },
            }),
        },
    }
}
fn review() -> Review {
    Review {
        id: ReviewId("review".into()),
        reviewer: user(),
        read: ReadState {
            pull_request: PullRequest {
                id: PullRequestId("pr".into()),
                title: "PR".into(),
                body: String::new(),
                author: None,
                status: PullRequestStatus::Open,
                source: None,
                revision: location().revision,
            },
            files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                previous_path: None,
                change: FileChange::Modified,
                stats: None,
                viewed: false,
            }],
            comments: vec![
                ReadComment {
                    content: CommentContent {
                        id: id("general"),
                        body: "general body".into(),
                    },
                    author: None,
                    target: CommentTarget::PullRequest,
                },
                ReadComment {
                    content: CommentContent {
                        id: id("diff"),
                        body: "diff body".into(),
                    },
                    author: None,
                    target: CommentTarget::Diff(ReadDiffLocation {
                        original: ReadCommentAnchor {
                            revision: ReadCommentRevision::Commit {
                                head: "oldhead".into(),
                            },
                            position: CommentPosition {
                                path: "old/path.rs".into(),
                                range: None,
                            },
                        },
                        current: None,
                        subject: None,
                        diff_hunk: Some("old hunk".into()),
                        outdated: None,
                    }),
                },
            ],
            reactions: vec![CommentReaction {
                comment: id("general"),
                author: Some(user().id),
                reaction: "👍".into(),
            }],
            reviews: vec![ReadReview {
                id: Some(ReviewResourceId("remote-review".into())),
                author: Some(user()),
                state: ReadReviewState::Submitted(ReviewConclusion {
                    outcome: ReviewOutcome::Approve,
                    body: None,
                }),
            }],
        },
    }
}
fn batch(events: Vec<Event>) -> Batch {
    Batch {
        revision: Default::default(),
        id: BatchId("batch".into()),
        review: ReviewId("review".into()),
        write: WriteState { events },
    }
}
fn create(value: &str, target: CommentTarget) -> Event {
    Event::CreateComment {
        comment: DraftComment {
            content: CommentContent {
                id: id(value),
                body: "draft body".into(),
            },
            author: user(),
            target,
        },
    }
}
fn edit(value: &str, body: &str) -> Event {
    Event::EditComment {
        comment: id(value),
        body: body.into(),
    }
}
fn delete(value: &str) -> Event {
    Event::DeleteComment { comment: id(value) }
}
fn reaction(value: &str, present: bool) -> Event {
    Event::SetCommentReaction {
        comment: id(value),
        reaction: "👍".into(),
        present,
    }
}
fn viewed(value: bool) -> Event {
    Event::SetFileViewed {
        path: "src/lib.rs".into(),
        viewed: value,
    }
}
fn conclusion(outcome: ReviewOutcome) -> Event {
    Event::SetReviewConclusion {
        conclusion: ReviewConclusion {
            outcome,
            body: None,
        },
    }
}

#[test]
fn projection_and_publication_preserve_interleaved_creation_chronology() {
    let mut review = review();
    review.read.comments[0].content.id = id("z-fetched-first");
    review.read.comments[1].content.id = id("a-fetched-second");
    review.read.reactions.clear();
    let created = [
        "z-root", "y-reply", "x-root", "w-reply", "v-reply", "u-nested",
    ];
    let batch = batch(vec![
        create(created[0], CommentTarget::Diff(location())),
        create(created[1], CommentTarget::Reply(id(created[0]))),
        create(created[2], CommentTarget::Diff(location())),
        create(created[3], CommentTarget::Reply(id(created[0]))),
        create(created[4], CommentTarget::Reply(id(created[2]))),
        create(created[5], CommentTarget::Reply(id(created[1]))),
        edit(created[0], "edited without changing creation order"),
    ]);
    let projected = project(&review, &batch).unwrap();
    assert_eq!(
        projected
            .comments
            .iter()
            .map(|c| c.content().id.0.as_str())
            .collect::<Vec<_>>(),
        ["z-fetched-first", "a-fetched-second"]
            .into_iter()
            .chain(created)
            .collect::<Vec<_>>()
    );
    let plan = prepare(&review, &batch, &[]).unwrap();
    assert_eq!(
        plan.operations
            .iter()
            .map(|op| match &op.change {
                Change::CreateComment { comment } => comment.content.id.0.as_str(),
                other => panic!("unexpected operation: {other:?}"),
            })
            .collect::<Vec<_>>(),
        created
    );
    // Partial delivery must retain both remaining order and operation identities.
    assert_eq!(
        prepare(&review, &batch, &plan.operations[..2])
            .unwrap()
            .operations,
        plan.operations[2..]
    );
    assert!(
        prepare(&review, &batch, &plan.operations)
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn unsent_changes_cancel_without_erasing_history() {
    let batch = batch(vec![
        create("draft", CommentTarget::PullRequest),
        edit("draft", "edited"),
        reaction("draft", true),
        delete("draft"),
        edit("general", "edited"),
        edit("general", "general body"),
        viewed(true),
        viewed(false),
        reaction("general", false),
        reaction("general", true),
    ]);
    assert!(
        prepare(&review(), &batch, &[])
            .unwrap()
            .operations
            .is_empty()
    );
    assert_eq!(batch.write.events.len(), 10);
    assert_eq!(project(&review(), &batch).unwrap().comments.len(), 2);
}

#[test]
fn create_edit_collapses_then_successful_create_becomes_edit_then_delete() {
    let mut batch = batch(vec![
        create("draft", CommentTarget::PullRequest),
        edit("draft", "first edit"),
    ]);
    let first = prepare(&review(), &batch, &[]).unwrap().operations;
    assert_eq!(first.len(), 1);
    assert!(
        matches!(&first[0].change, Change::CreateComment { comment } if comment.content.body == "first edit")
    );
    assert!(
        prepare(&review(), &batch, &first)
            .unwrap()
            .operations
            .is_empty()
    );
    batch.write.events.push(edit("draft", "second edit"));
    let second = prepare(&review(), &batch, &first).unwrap().operations;
    assert!(
        matches!(&second[0].change, Change::EditComment { comment, body } if *comment == id("draft") && body == "second edit")
    );
    assert_ne!(first[0].id, second[0].id);
    let mut receipts = first;
    receipts.extend(second);
    batch.write.events.push(delete("draft"));
    let last = prepare(&review(), &batch, &receipts).unwrap().operations;
    assert!(
        matches!(&last[0].change, Change::DeleteComment { comment } if *comment == id("draft"))
    );
    receipts.extend(last);
    assert!(
        prepare(&review(), &batch, &receipts)
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn delivered_edits_and_toggles_reverse_even_when_intent_returns_to_baseline() {
    let mut batch = batch(vec![
        edit("general", "changed"),
        viewed(true),
        reaction("general", false),
    ]);
    let sent = prepare(&review(), &batch, &[]).unwrap().operations;
    batch.write.events.extend([
        edit("general", "general body"),
        viewed(false),
        reaction("general", true),
    ]);
    assert!(
        prepare(&review(), &batch, &[])
            .unwrap()
            .operations
            .is_empty()
    );
    let reversal = prepare(&review(), &batch, &sent).unwrap().operations;
    assert_eq!(reversal.len(), 3);
    assert!(
        matches!(&reversal[0].change, Change::EditComment { body, .. } if body == "general body")
    );
    assert!(matches!(
        &reversal[1].change,
        Change::SetFileViewed { viewed: false, .. }
    ));
    assert!(matches!(
        &reversal[2].change,
        Change::SetCommentReaction { present: true, .. }
    ));
    let receipts = sent.into_iter().chain(reversal).collect::<Vec<_>>();
    assert!(
        prepare(&review(), &batch, &receipts)
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn partial_receipts_skip_only_successes_and_keep_operation_ids_stable() {
    let batch = batch(vec![
        create("draft", CommentTarget::Diff(location())),
        create("reply", CommentTarget::Reply(id("draft"))),
        reaction("reply", true),
        viewed(true),
    ]);
    let all = prepare(&review(), &batch, &[]).unwrap().operations;
    assert_eq!(all.len(), 4);
    assert!(
        matches!(&all[0].change, Change::CreateComment { comment } if comment.content.id == id("draft"))
    );
    assert!(
        matches!(&all[1].change, Change::CreateComment { comment } if comment.content.id == id("reply"))
    );
    let retry = prepare(&review(), &batch, &all[..1]).unwrap().operations;
    assert_eq!(retry, all[1..]);
    assert!(
        prepare(&review(), &batch, &all)
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn nested_draft_threads_create_parent_first_delete_child_first() {
    let mut batch = batch(vec![
        create("z-root", CommentTarget::Diff(location())),
        create("a-reply", CommentTarget::Reply(id("z-root"))),
        create("b-reply", CommentTarget::Reply(id("a-reply"))),
    ]);
    let sent = prepare(&review(), &batch, &[]).unwrap().operations;
    assert!(
        matches!(&sent[0].change, Change::CreateComment { comment } if comment.content.id == id("z-root"))
    );
    batch
        .write
        .events
        .extend([delete("z-root"), delete("a-reply"), delete("b-reply")]);
    assert!(
        prepare(&review(), &batch, &[])
            .unwrap()
            .operations
            .is_empty()
    );
    let deletes = prepare(&review(), &batch, &sent).unwrap().operations;
    assert_eq!(
        deletes
            .iter()
            .map(|operation| match &operation.change {
                Change::DeleteComment { comment } => comment.clone(),
                _ => panic!(),
            })
            .collect::<Vec<_>>(),
        vec![id("b-reply"), id("a-reply"), id("z-root")]
    );
}

#[test]
fn fetched_anchors_remain_read_only_and_can_receive_replies() {
    let read = review();
    let batch = batch(vec![
        edit("diff", "new body"),
        create("reply", CommentTarget::Reply(id("diff"))),
    ]);
    let projection = project(&read, &batch).unwrap();
    let fetched = projection.comments.iter().find(|comment| matches!(comment, ProjectedComment::Read(comment) if comment.content.id == id("diff"))).unwrap();
    let ProjectedComment::Read(fetched) = fetched else {
        panic!()
    };
    assert_eq!(fetched.target, read.read.comments[1].target);
    assert_eq!(fetched.author, None);
    assert_eq!(fetched.content.body, "new body");
    assert_eq!(prepare(&read, &batch, &[]).unwrap().operations.len(), 2);
}

#[test]
fn explicit_new_review_is_sent_even_when_matching_snapshot_then_skipped_on_retry() {
    let mut batch = batch(vec![
        conclusion(ReviewOutcome::Comment),
        conclusion(ReviewOutcome::Approve),
    ]);
    let sent = prepare(&review(), &batch, &[]).unwrap().operations;
    assert_eq!(sent.len(), 1);
    assert!(
        matches!(&sent[0].change, Change::SetReviewConclusion { conclusion } if conclusion.outcome == ReviewOutcome::Approve)
    );
    assert!(
        prepare(&review(), &batch, &sent)
            .unwrap()
            .operations
            .is_empty()
    );
    batch
        .write
        .events
        .push(conclusion(ReviewOutcome::RequestChanges));
    let next = prepare(&review(), &batch, &sent).unwrap().operations;
    assert_eq!(next.len(), 1);
    assert_ne!(sent[0].id, next[0].id);
}

#[test]
fn dismissal_preserves_read_review_body_and_retries_skip_success() {
    let batch = batch(vec![Event::DismissReview {
        review: ReviewResourceId("remote-review".into()),
        message: Some("superseded".into()),
    }]);
    let sent = prepare(&review(), &batch, &[]).unwrap().operations;
    assert!(
        matches!(&sent[0].change, Change::DismissReview { message: Some(message), .. } if message == "superseded")
    );
    assert!(
        prepare(&review(), &batch, &sent)
            .unwrap()
            .operations
            .is_empty()
    );
    assert!(matches!(
        project(&review(), &batch).unwrap().reviews[0].state,
        ReadReviewState::Inactive {
            state: InactiveReviewState::Dismissed,
            ..
        }
    ));
}

#[test]
fn unsupported_and_dangling_replies_fail_explicitly_without_cascades() {
    assert!(
        prepare(
            &review(),
            &batch(vec![create("reply", CommentTarget::Reply(id("general")))]),
            &[]
        )
        .is_err()
    );
    assert!(
        prepare(
            &review(),
            &batch(vec![create("reply", CommentTarget::Reply(id("missing")))]),
            &[]
        )
        .is_err()
    );
    assert!(
        prepare(
            &review(),
            &batch(vec![
                create("reply", CommentTarget::Reply(id("diff"))),
                delete("diff")
            ]),
            &[]
        )
        .is_err()
    );
    assert!(
        prepare(
            &review(),
            &batch(vec![
                create("reply", CommentTarget::Reply(id("diff"))),
                delete("diff"),
                delete("reply")
            ]),
            &[]
        )
        .is_ok()
    );
}

#[test]
fn invalid_write_anchors_and_identity_reuse_fail() {
    let mut bad = location();
    bad.revision.base.clear();
    assert!(
        project(
            &review(),
            &batch(vec![create("draft", CommentTarget::Diff(bad))])
        )
        .is_err()
    );
    let mut bad = location();
    bad.position.range.as_mut().unwrap().start.side = DiffSide::Base;
    assert!(
        project(
            &review(),
            &batch(vec![create("draft", CommentTarget::Diff(bad))])
        )
        .is_err()
    );
    let mut bad = location();
    bad.position.range.as_mut().unwrap().start.line = NonZeroU32::new(6).unwrap();
    assert!(
        project(
            &review(),
            &batch(vec![create("draft", CommentTarget::Diff(bad))])
        )
        .is_err()
    );
    assert!(matches!(
        project(
            &review(),
            &batch(vec![
                create("draft", CommentTarget::PullRequest),
                delete("draft"),
                create("draft", CommentTarget::PullRequest)
            ])
        ),
        Err(CoreError::DuplicateComment(_))
    ));
    assert!(matches!(
        project(&review(), &batch(vec![edit("missing", "body")])),
        Err(CoreError::MissingComment(_))
    ));
    let mut wrong = batch(vec![]);
    wrong.review = ReviewId("another".into());
    assert!(matches!(
        project(&review(), &wrong),
        Err(CoreError::ReviewMismatch(_))
    ));
}

#[test]
fn reactions_are_scoped_to_current_reviewer_and_deleted_comments_cancel() {
    let mut read = review();
    read.read.reactions.push(CommentReaction {
        comment: id("general"),
        author: Some(UserId("other".into())),
        reaction: "👍".into(),
    });
    let batch = batch(vec![
        reaction("general", false),
        create("draft", CommentTarget::PullRequest),
        reaction("draft", true),
        delete("draft"),
    ]);
    let projection = project(&read, &batch).unwrap();
    assert_eq!(projection.reactions.len(), 1);
    assert_eq!(projection.reactions[0].author, Some(UserId("other".into())));
    let changes = prepare(&read, &batch, &[]).unwrap().operations;
    assert_eq!(changes.len(), 1);
    assert!(
        matches!(&changes[0].change, Change::SetCommentReaction { comment, present: false, .. } if *comment == id("general"))
    );
}

#[test]
fn incomplete_fetched_reply_ancestry_is_preserved_without_restricting_existing_operations() {
    let mut read = review();
    read.read.comments.push(ReadComment {
        content: CommentContent {
            id: id("orphan"),
            body: "Retained reply".into(),
        },
        author: None,
        target: CommentTarget::Reply(id("deleted-remote-parent")),
    });
    let empty = batch(vec![]);
    assert!(project(&read, &empty).is_ok());
    assert!(prepare(&read, &empty, &[]).unwrap().operations.is_empty());
    let edits = prepare(&read, &batch(vec![edit("orphan", "Revised reply")]), &[])
        .unwrap()
        .operations;
    assert!(
        matches!(&edits[0].change, Change::EditComment { comment, .. } if *comment == id("orphan"))
    );
    let deletes = prepare(&read, &batch(vec![delete("orphan")]), &[])
        .unwrap()
        .operations;
    assert!(
        matches!(&deletes[0].change, Change::DeleteComment { comment } if *comment == id("orphan"))
    );
    assert!(
        prepare(
            &read,
            &batch(vec![create(
                "new-reply",
                CommentTarget::Reply(id("orphan"))
            )]),
            &[]
        )
        .is_err()
    );
}

#[test]
fn deleting_fetched_root_preserves_independent_fetched_replies() {
    let mut read = review();
    read.read.comments.push(ReadComment {
        content: CommentContent {
            id: id("remote-reply"),
            body: "Keep this reply".into(),
        },
        author: None,
        target: CommentTarget::Reply(id("diff")),
    });
    let deletion = batch(vec![delete("diff")]);
    let projected = project(&read, &deletion).unwrap();
    assert!(
        projected
            .comments
            .iter()
            .any(|comment| comment.content().id == id("remote-reply"))
    );
    let plan = prepare(&read, &deletion, &[]).unwrap();
    assert_eq!(plan.operations.len(), 1);
    assert!(
        matches!(&plan.operations[0].change,Change::DeleteComment{comment} if *comment==id("diff"))
    );
    assert!(
        prepare(&read, &deletion, &plan.operations)
            .unwrap()
            .operations
            .is_empty()
    );
    let invalid = batch(vec![
        delete("diff"),
        create("new", CommentTarget::Reply(id("remote-reply"))),
    ]);
    assert!(project(&read, &invalid).is_err());
}

#[test]
fn internal_revision_namespace_preserves_legacy_receipts_and_separates_rewritten_logs() {
    let review = review();
    let legacy = batch(vec![create("draft", CommentTarget::PullRequest)]);
    let original = prepare(&review, &legacy, &[]).unwrap().operations[0]
        .id
        .clone();
    let mut migrated = legacy.clone();
    migrated.id = BatchId("canonical-batch".into());
    migrated.revision = BatchRevision {
        number: 3,
        namespace: Some(legacy.id.0.clone()),
    };
    assert_eq!(
        prepare(&review, &migrated, &[]).unwrap().operations[0].id,
        original
    );
    migrated.revision = BatchRevision {
        number: 4,
        namespace: Some("canonical-batch:revision:4".into()),
    };
    let rewritten = prepare(&review, &migrated, &[]).unwrap().operations[0]
        .id
        .clone();
    assert_ne!(rewritten, original);
    assert_eq!(
        prepare(&review, &migrated, &[]).unwrap().operations[0].id,
        rewritten
    );
}
