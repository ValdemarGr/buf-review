use std::num::NonZeroU32;

use buf_review::core::{CoreError, prepare, project};
use buf_review::model::*;
use buf_review::store::{Store, StoreError};
use tempfile::tempdir;

fn user() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn baseline() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull-request".into()),
            title: "Improve review state".into(),
            body: "Keep draft history".into(),
            author: Some(user()),
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "base-revision".into(),
                head: "head-revision".into(),
            },
        },
        files: vec![ChangedFile {
            path: "src/model.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
            viewed: false,
        }],
        comments: vec![ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("fetched-comment".into()),
                body: "Original text".into(),
            },
            author: Some(user()),
            target: CommentTarget::PullRequest,
        }],
        reactions: vec![CommentReaction {
            comment: CommentId("fetched-comment".into()),
            author: Some(user().id),
            reaction: "👍".into(),
        }],
        reviews: vec![ReadReview {
            id: Some(ReviewResourceId("fetched-review".into())),
            author: Some(user()),
            state: ReadReviewState::Submitted(ReviewConclusion {
                outcome: ReviewOutcome::Comment,
                body: Some("Earlier review".into()),
            }),
        }],
    }
}

fn draft(id: &str, target: CommentTarget, body: &str) -> Event {
    Event::CreateComment {
        comment: DraftComment {
            content: CommentContent {
                id: CommentId(id.into()),
                body: body.into(),
            },
            author: user(),
            target,
        },
    }
}

#[test]
fn reopening_preserves_baseline_and_uncollapsed_ordered_history() {
    let project = tempdir().unwrap();
    let id = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let read = baseline();
    let location = DiffLocation {
        revision: read.pull_request.revision.clone(),
        position: CommentPosition {
            path: "src/model.rs".into(),
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
    };
    let events = vec![
        Event::EditComment {
            comment: CommentId("fetched-comment".into()),
            body: "Edited fetched comment".into(),
        },
        draft("draft", CommentTarget::Diff(location), "Initial draft"),
        Event::EditComment {
            comment: CommentId("draft".into()),
            body: "Revised draft".into(),
        },
        draft(
            "reply",
            CommentTarget::Reply(CommentId("draft".into())),
            "Reply to unsent draft",
        ),
        Event::SetCommentReaction {
            comment: CommentId("draft".into()),
            reaction: "❤️".into(),
            present: true,
        },
        Event::SetCommentReaction {
            comment: CommentId("draft".into()),
            reaction: "❤️".into(),
            present: false,
        },
        Event::DeleteComment {
            comment: CommentId("draft".into()),
        },
        draft("recreated", CommentTarget::PullRequest, "Revised draft"),
        Event::SetFileViewed {
            path: "src/model.rs".into(),
            viewed: true,
            commit: None,
        },
        Event::SetFileViewed {
            path: "src/model.rs".into(),
            viewed: false,
            commit: None,
        },
        Event::DismissReview {
            review: ReviewResourceId("fetched-review".into()),
            message: Some("Superseded".into()),
        },
        Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::RequestChanges,
                body: Some("Needs revision".into()),
            },
        },
        Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    ];

    {
        let mut store = Store::open(project.path()).unwrap();
        store.create_review(&id, &user(), &read).unwrap();
        store.create_batch(&batch, &id).unwrap();
        for event in &events {
            store.append_event(&batch, event).unwrap();
        }
    }

    assert!(project.path().join(".review/state.sqlite3").is_file());
    let mut reopened = Store::open(project.path()).unwrap();
    assert_eq!(
        reopened.get_review(&id).unwrap(),
        Some(Review {
            id: id.clone(),
            reviewer: user(),
            read,
        })
    );
    assert_eq!(
        reopened.get_batch(&batch).unwrap(),
        Some(Batch {
            revision: buf_review::model::BatchRevision {
                number: 0,
                namespace: Some(batch.0.clone())
            },
            id: batch,
            review: id,
            write: WriteState { events },
        })
    );
}

#[test]
fn older_batches_remain_editable_after_newer_batches_are_created() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let first = BatchId("first".into());
    let second = BatchId("second".into());
    let first_event = draft("first-draft", CommentTarget::PullRequest, "First");
    let second_event = draft("second-draft", CommentTarget::PullRequest, "Second");
    let edit = Event::EditComment {
        comment: CommentId("first-draft".into()),
        body: "Revised first".into(),
    };
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&review, &user(), &baseline()).unwrap();
    store.create_batch(&first, &review).unwrap();
    store.append_event(&first, &first_event).unwrap();
    store.create_batch(&second, &review).unwrap();
    store.append_event(&second, &second_event).unwrap();
    store.append_event(&first, &edit).unwrap();

    assert_eq!(
        store.get_batch(&first).unwrap().unwrap().write.events,
        vec![first_event, edit]
    );
    assert_eq!(
        store.get_batch(&second).unwrap().unwrap().write.events,
        vec![second_event]
    );
    assert_eq!(store.batch_ids(&review).unwrap(), vec![first, second]);
}

#[test]
fn reviews_of_same_pull_request_keep_independent_snapshots_and_batches() {
    let project = tempdir().unwrap();
    let first = ReviewId("first".into());
    let second = ReviewId("second".into());
    let first_batch = BatchId("first-batch".into());
    let second_batch = BatchId("second-batch".into());
    let first_read = baseline();
    let mut second_read = first_read.clone();
    second_read.pull_request.revision.head = "new-head-revision".into();
    let event = draft("draft", CommentTarget::PullRequest, "Only first review");
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&first, &user(), &first_read).unwrap();
    store.create_review(&second, &user(), &second_read).unwrap();
    store.create_batch(&first_batch, &first).unwrap();
    store.create_batch(&second_batch, &second).unwrap();
    store.append_event(&first_batch, &event).unwrap();

    assert_eq!(store.get_review(&first).unwrap().unwrap().read, first_read);
    assert_eq!(
        store.get_review(&second).unwrap().unwrap().read,
        second_read
    );
    assert_eq!(
        store.get_batch(&first_batch).unwrap().unwrap().write.events,
        vec![event]
    );
    assert!(
        store
            .get_batch(&second_batch)
            .unwrap()
            .unwrap()
            .write
            .events
            .is_empty()
    );
    assert_eq!(store.batch_ids(&first).unwrap(), vec![first_batch]);
    assert_eq!(store.batch_ids(&second).unwrap(), vec![second_batch]);
    assert_eq!(store.review_ids().unwrap(), vec![first, second]);
}

#[test]
fn batch_identity_is_global_and_requires_an_existing_review() {
    let project = tempdir().unwrap();
    let first = ReviewId("first".into());
    let second = ReviewId("second".into());
    let missing = ReviewId("missing".into());
    let batch = BatchId("shared-batch-id".into());
    let orphan = BatchId("orphan".into());
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&first, &user(), &baseline()).unwrap();
    store.create_review(&second, &user(), &baseline()).unwrap();
    store.create_batch(&batch, &first).unwrap();

    assert!(matches!(
        store.create_batch(&batch, &second),
        Err(StoreError::Sqlite(_))
    ));
    assert!(matches!(
        store.create_batch(&orphan, &missing),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(store.get_batch(&batch).unwrap().unwrap().review, first);
    assert_eq!(store.get_batch(&orphan).unwrap(), None);
    assert!(store.batch_ids(&second).unwrap().is_empty());
    assert!(store.batch_ids(&missing).unwrap().is_empty());
}

#[test]
fn unknown_reviews_and_batches_are_absent_and_cannot_accept_events() {
    let project = tempdir().unwrap();
    let review = ReviewId("missing-review".into());
    let batch = BatchId("missing-batch".into());
    let mut store = Store::open(project.path()).unwrap();

    assert_eq!(store.get_review(&review).unwrap(), None);
    assert_eq!(store.get_batch(&batch).unwrap(), None);
    assert!(matches!(
        store.append_event(&batch, &draft("draft", CommentTarget::PullRequest, "Text")),
        Err(StoreError::BatchNotFound(found)) if found == batch
    ));
    assert!(store.review_ids().unwrap().is_empty());
    assert!(store.batch_ids(&review).unwrap().is_empty());
}

#[test]
fn duplicate_creation_preserves_original_snapshot_and_batch_history() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let event = draft("draft", CommentTarget::PullRequest, "Existing draft");
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&review, &user(), &baseline()).unwrap();
    store.create_batch(&batch, &review).unwrap();
    store.append_event(&batch, &event).unwrap();
    let original_review = store.get_review(&review).unwrap();
    let original_batch = store.get_batch(&batch).unwrap();
    let mut replacement = baseline();
    replacement.pull_request.title = "Replacement".into();
    let mut other_user = user();
    other_user.id = UserId("other-reviewer".into());

    assert!(matches!(
        store.create_review(&review, &other_user, &replacement),
        Err(StoreError::Sqlite(_))
    ));
    assert!(matches!(
        store.create_batch(&batch, &review),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(store.get_review(&review).unwrap(), original_review);
    assert_eq!(store.get_batch(&batch).unwrap(), original_batch);
}

#[test]
fn concurrent_connections_append_without_overwriting_each_others_events() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&review, &user(), &baseline()).unwrap();
    store.create_batch(&batch, &review).unwrap();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|writer| {
            let mut connection = Store::open(project.path()).unwrap();
            let batch = batch.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                for index in 0..24 {
                    let event = draft(
                        &format!("{writer}-{index}"),
                        CommentTarget::PullRequest,
                        &format!("Writer {writer}, event {index}"),
                    );
                    connection.append_event(&batch, &event).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }

    let events = store.get_batch(&batch).unwrap().unwrap().write.events;
    assert_eq!(events.len(), 48);
    for writer in 0..2 {
        let actual: Vec<_> = events
            .iter()
            .filter(|event| match event {
                Event::CreateComment { comment } => {
                    comment.content.id.0.starts_with(&format!("{writer}-"))
                }
                _ => false,
            })
            .cloned()
            .collect();
        let expected: Vec<_> = (0..24)
            .map(|index| {
                draft(
                    &format!("{writer}-{index}"),
                    CommentTarget::PullRequest,
                    &format!("Writer {writer}, event {index}"),
                )
            })
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn invalid_events_are_rejected_without_poisoning_the_log() {
    let directory = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(directory.path()).unwrap();
    store.create_review(&review, &user(), &baseline()).unwrap();
    store.create_batch(&batch, &review).unwrap();
    let created = draft("draft", CommentTarget::PullRequest, "Draft");
    let deleted = Event::DeleteComment {
        comment: CommentId("draft".into()),
    };
    store.append_event(&batch, &created).unwrap();
    store.append_event(&batch, &deleted).unwrap();
    let original = store.get_batch(&batch).unwrap().unwrap();
    let invalid = [
        Event::EditComment {
            comment: CommentId("missing".into()),
            body: "Edit".into(),
        },
        Event::DeleteComment {
            comment: CommentId("missing".into()),
        },
        Event::SetCommentReaction {
            comment: CommentId("missing".into()),
            reaction: "👍".into(),
            present: true,
        },
        Event::SetCommentReaction {
            comment: CommentId("fetched-comment".into()),
            reaction: String::new(),
            present: true,
        },
        Event::SetFileViewed {
            path: "missing.rs".into(),
            viewed: true,
            commit: None,
        },
        Event::DismissReview {
            review: ReviewResourceId("missing".into()),
            message: None,
        },
        draft("draft", CommentTarget::PullRequest, "Reused identity"),
        draft("fetched-comment", CommentTarget::PullRequest, "Duplicate"),
        draft("", CommentTarget::PullRequest, "Empty identity"),
        draft(
            "reply",
            CommentTarget::Reply(CommentId("missing".into())),
            "Missing parent",
        ),
        draft(
            "reply",
            CommentTarget::Reply(CommentId("fetched-comment".into())),
            "PR-wide parent",
        ),
        draft(
            "diff",
            CommentTarget::Diff(DiffLocation {
                revision: DiffRevision {
                    base: String::new(),
                    head: "head".into(),
                },
                position: CommentPosition {
                    path: "src/model.rs".into(),
                    range: None,
                },
            }),
            "Invalid anchor",
        ),
    ];
    for event in invalid {
        assert!(
            matches!(store.append_event(&batch, &event), Err(StoreError::Core(_))),
            "event should be rejected: {event:?}"
        );
        assert_eq!(store.get_batch(&batch).unwrap(), Some(original.clone()));
    }

    let edit = Event::EditComment {
        comment: CommentId("fetched-comment".into()),
        body: "Still editable".into(),
    };
    store.append_event(&batch, &edit).unwrap();
    let retained = store.get_batch(&batch).unwrap().unwrap();
    assert_eq!(retained.write.events, vec![created, deleted, edit]);
    let snapshot = store.get_review(&review).unwrap().unwrap();
    assert!(project(&snapshot, &retained).is_ok());
    assert_eq!(
        prepare(&snapshot, &retained, &[]).unwrap().operations.len(),
        1
    );
}

#[test]
fn staging_allows_parent_first_deletion_until_replies_are_deleted() {
    let directory = tempdir().unwrap();
    let review_id = ReviewId("review".into());
    let batch_id = BatchId("batch".into());
    let read = baseline();
    let mut store = Store::open(directory.path()).unwrap();
    store.create_review(&review_id, &user(), &read).unwrap();
    store.create_batch(&batch_id, &review_id).unwrap();
    let parent = draft(
        "parent",
        CommentTarget::Diff(DiffLocation {
            revision: read.pull_request.revision,
            position: CommentPosition {
                path: "src/model.rs".into(),
                range: None,
            },
        }),
        "Parent",
    );
    let reply = draft(
        "reply",
        CommentTarget::Reply(CommentId("parent".into())),
        "Reply",
    );
    for event in [
        parent,
        reply,
        Event::DeleteComment {
            comment: CommentId("parent".into()),
        },
    ] {
        store.append_event(&batch_id, &event).unwrap();
    }
    let snapshot = store.get_review(&review_id).unwrap().unwrap();
    let dangling = store.get_batch(&batch_id).unwrap().unwrap();
    assert!(matches!(
        project(&snapshot, &dangling),
        Err(CoreError::MissingComment(_))
    ));
    assert!(prepare(&snapshot, &dangling, &[]).is_err());

    store
        .append_event(
            &batch_id,
            &Event::DeleteComment {
                comment: CommentId("reply".into()),
            },
        )
        .unwrap();
    let complete = store.get_batch(&batch_id).unwrap().unwrap();
    assert_eq!(complete.write.events.len(), 4);
    assert!(project(&snapshot, &complete).is_ok());
    assert!(
        prepare(&snapshot, &complete, &[])
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn concurrent_conflicting_events_are_validated_against_the_latest_log() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&review, &user(), &baseline()).unwrap();
    store.create_batch(&batch, &review).unwrap();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let mut connection = Store::open(project.path()).unwrap();
            let batch = batch.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                connection.append_event(
                    &batch,
                    &draft("shared", CommentTarget::PullRequest, "Shared identity"),
                )
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(StoreError::Core(CoreError::DuplicateComment(_)))
            ))
            .count(),
        1
    );
    assert_eq!(
        store.get_batch(&batch).unwrap().unwrap().write.events.len(),
        1
    );
}

#[test]
fn canonical_batches_are_shared_by_cli_and_workspace_and_resolution_never_selects() {
    use buf_review::store::BatchRoute;
    use buf_review::workspace::{Action, Destination};
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("review".into());
    store.create_review(&review, &user(), &baseline()).unwrap();
    let cli = BatchId("cli".into());
    store.create_batch(&cli, &review).unwrap();
    store
        .append_event(&cli, &draft("draft", CommentTarget::PullRequest, "first"))
        .unwrap();
    let workspace = store.workspace("cli").unwrap().unwrap();
    assert_eq!(workspace.id, cli.0);
    assert_eq!(
        workspace.destinations[&CommentId("draft".into())],
        Destination::Standalone
    );
    assert_eq!(workspace.pending().unwrap().operations.len(), 1);
    assert!(
        matches!(store.resolve_workspace(&review).unwrap(), BatchRoute::Open(w) if w.id==cli.0)
    );
    assert!(store.active_workspace(&user().id).unwrap().is_none());
    store.select_batch(&cli).unwrap();
    let second = store.create_batch_workspace(&review).unwrap();
    assert!(
        matches!(store.resolve_workspace(&review).unwrap(), BatchRoute::Choose(w) if w.len()==2)
    );
    assert_eq!(
        store.active_workspace(&user().id).unwrap().unwrap().id,
        cli.0
    );
    let edited = store
        .mutate_workspace(
            "cli",
            workspace.generation,
            Action::Save {
                comment: Some(CommentId("draft".into())),
                target: None,
                body: "edited".into(),
                destination: None,
            },
        )
        .unwrap();
    assert_eq!(store.get_batch(&cli).unwrap().unwrap(), edited.batch);
    assert_eq!(store.batch_workspaces().unwrap().len(), 2);
    drop(store);
    let store = Store::open(project.path()).unwrap();
    assert_eq!(
        store
            .workspace("cli")
            .unwrap()
            .unwrap()
            .pending()
            .unwrap()
            .operations,
        edited.pending().unwrap().operations
    );
    assert_eq!(
        store.workspace(&second.id).unwrap().unwrap().batch.id.0,
        second.id
    );
}

#[test]
fn refresh_keeps_batch_identity_and_historical_receipts_out_of_delivered_delta() {
    use buf_review::store::IdMapping;
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let old = ReviewId("old".into());
    store.create_review(&old, &user(), &baseline()).unwrap();
    let batch = BatchId("stable".into());
    store.create_batch(&batch, &old).unwrap();
    store
        .append_event(&batch, &draft("draft", CommentTarget::PullRequest, "body"))
        .unwrap();
    let workspace = store.workspace(&batch.0).unwrap().unwrap();
    let operation = workspace.pending().unwrap().operations[0].clone();
    store
        .record_operation_result(
            &batch,
            &operation,
            &[] as &[IdMapping],
            &serde_json::json!({"id":1}),
        )
        .unwrap();
    let delivered = store.workspace(&batch.0).unwrap().unwrap();
    let mut fresh = baseline();
    fresh.comments.push(ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId("draft".into()),
            body: "body".into(),
        },
        author: Some(user()),
        target: CommentTarget::PullRequest,
    });
    let new = ReviewId("new".into());
    store.create_review(&new, &user(), &fresh).unwrap();
    let refreshed = store
        .refresh_workspace(&batch.0, delivered.generation, &new)
        .unwrap();
    assert_eq!(refreshed.batch.id, batch);
    assert_eq!(refreshed.id, batch.0);
    assert!(refreshed.completed.is_empty());
    assert_eq!(refreshed.results.len(), 1);
    assert!(refreshed.pending().unwrap().operations.is_empty());
    assert_eq!(
        store.batch_revision(&batch, 0).unwrap().unwrap(),
        workspace.batch
    );
    assert_eq!(
        store.operation_results(&batch).unwrap()[0].operation.id,
        operation.id
    );
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let reopened = store.workspace(&batch.0).unwrap().unwrap();
    assert!(reopened.pending().unwrap().operations.is_empty());
    assert!(reopened.completed.is_empty());
    assert_eq!(reopened.results.len(), 1);
    assert!(!reopened.is_unfinished().unwrap());
}
