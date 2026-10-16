use std::num::NonZeroU32;

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
            revision: DiffRevision {
                base: "base-revision".into(),
                head: "head-revision".into(),
            },
        },
        files: vec![ChangedFile {
            path: "src/model.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            viewed: false,
        }],
        comments: vec![ReadComment {
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
        },
        Event::SetFileViewed {
            path: "src/model.rs".into(),
            viewed: false,
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
