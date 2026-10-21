use std::{fs, path::Path};

use buf_review::{model::*, store::Store};
use rusqlite::Connection;
use tempfile::tempdir;

struct LegacyReview {
    review: Review,
    events: Vec<Event>,
    committed: bool,
}

fn legacy_reviews() -> Vec<LegacyReview> {
    [false, true]
        .into_iter()
        .map(|committed| {
            let name = if committed { "committed" } else { "pending" };
            let reviewer = User {
                id: UserId(format!("{name}-reviewer")),
                name: name.into(),
            };
            let comment = DraftComment {
                content: CommentContent {
                    id: CommentId(format!("{name}-comment")),
                    body: format!("{name} comment"),
                },
                author: reviewer.clone(),
                target: CommentTarget::PullRequest,
            };
            LegacyReview {
                review: Review {
                    id: ReviewId(name.into()),
                    reviewer: reviewer.clone(),
                    read: ReadState {
                        pull_request: PullRequest {
                            id: PullRequestId("same-pull-request".into()),
                            title: format!("{name} snapshot"),
                            body: "Original snapshot".into(),
                            author: Some(reviewer),
                            status: PullRequestStatus::Open,
                            source: None,
                            revision: DiffRevision {
                                base: "base".into(),
                                head: format!("{name}-head"),
                            },
                        },
                        files: vec![],
                        comments: vec![],
                        reactions: vec![],
                        reviews: vec![],
                    },
                },
                events: vec![
                    Event::CreateComment {
                        comment: comment.clone(),
                    },
                    Event::EditComment {
                        comment: comment.content.id.clone(),
                        body: "Revised draft".into(),
                    },
                    Event::DeleteComment {
                        comment: comment.content.id,
                    },
                ],
                committed,
            }
        })
        .collect()
}

fn seed_legacy(project: &Path, reviews: &[LegacyReview]) -> Connection {
    let directory = project.join(".review");
    fs::create_dir_all(&directory).unwrap();
    let mut connection = Connection::open(directory.join("state.sqlite3")).unwrap();
    connection
        .execute_batch(include_str!("fixtures/schema_v1.sql"))
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for legacy in reviews {
        transaction
            .execute(
                "INSERT INTO reviews (review_id, reviewer, committed) VALUES (?1, ?2, ?3)",
                (
                    &legacy.review.id.0,
                    serde_json::to_string_pretty(&legacy.review.reviewer).unwrap(),
                    legacy.committed,
                ),
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO read_states (review_id, snapshot) VALUES (?1, ?2)",
                (
                    &legacy.review.id.0,
                    serde_json::to_string_pretty(&legacy.review.read).unwrap(),
                ),
            )
            .unwrap();
        for (index, event) in legacy.events.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO write_events (review_id, sequence, event) VALUES (?1, ?2, ?3)",
                    (
                        &legacy.review.id.0,
                        i64::try_from(index + 1).unwrap(),
                        serde_json::to_string_pretty(event).unwrap(),
                    ),
                )
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    connection
}

fn assert_original_data(connection: &Connection, legacy: &LegacyReview) {
    let metadata = connection
        .query_row(
            "SELECT reviewer, committed, snapshot
             FROM reviews JOIN read_states USING (review_id) WHERE review_id = ?1",
            [&legacy.review.id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        metadata,
        (
            serde_json::to_string_pretty(&legacy.review.reviewer).unwrap(),
            legacy.committed,
            serde_json::to_string_pretty(&legacy.review.read).unwrap(),
        )
    );
    let mut statement = connection
        .prepare(
            "SELECT sequence, event FROM legacy_write_events
             WHERE review_id = ?1 ORDER BY sequence",
        )
        .unwrap();
    let events = statement
        .query_map([&legacy.review.id.0], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        events,
        legacy
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| (
                i64::try_from(index + 1).unwrap(),
                serde_json::to_string_pretty(event).unwrap(),
            ))
            .collect::<Vec<_>>()
    );
    for (sequence, event) in events {
        let migrated = connection
            .query_row(
                "SELECT event FROM write_events WHERE batch_id = ?1 AND sequence = ?2",
                (&legacy.review.id.0, sequence),
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(migrated, event);
    }
}

#[test]
fn migration_preserves_snapshots_and_logs_and_both_batches_remain_editable() {
    let project = tempdir().unwrap();
    let reviews = legacy_reviews();
    let connection = seed_legacy(project.path(), &reviews);
    let mut store = Store::open(project.path()).unwrap();

    for legacy in &reviews {
        let batch_id = BatchId(legacy.review.id.0.clone());
        assert_eq!(
            store.get_review(&legacy.review.id).unwrap(),
            Some(legacy.review.clone())
        );
        assert_eq!(
            store.batch_ids(&legacy.review.id).unwrap(),
            vec![batch_id.clone()]
        );
        assert_eq!(
            store.get_batch(&batch_id).unwrap(),
            Some(Batch {
                revision: buf_review::model::BatchRevision {
                    number: 0,
                    namespace: Some(batch_id.0.clone())
                },
                id: batch_id.clone(),
                review: legacy.review.id.clone(),
                write: WriteState {
                    events: legacy.events.clone(),
                },
            })
        );
        assert_original_data(&connection, legacy);

        let event = Event::SetReviewConclusion {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        };
        store.append_event(&batch_id, &event).unwrap();
        let mut expected = legacy.events.clone();
        expected.push(event);
        assert_eq!(
            store.get_batch(&batch_id).unwrap().unwrap().write.events,
            expected
        );
        assert_original_data(&connection, legacy);
    }
    let mut statement = connection.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(statement.query([]).unwrap().next().unwrap().is_none());
}

#[test]
fn reopening_preserves_new_batches_and_does_not_repeat_legacy_migration() {
    let project = tempdir().unwrap();
    let reviews = legacy_reviews();
    let connection = seed_legacy(project.path(), &reviews);
    let new_id = BatchId("second-batch".into());
    let event = Event::SetReviewConclusion {
        conclusion: ReviewConclusion {
            outcome: ReviewOutcome::Approve,
            body: None,
        },
    };
    {
        let mut store = Store::open(project.path()).unwrap();
        store.create_batch(&new_id, &reviews[1].review.id).unwrap();
        store.append_event(&new_id, &event).unwrap();
    }

    let mut reopened = Store::open(project.path()).unwrap();
    assert_eq!(
        reopened.get_batch(&new_id).unwrap(),
        Some(Batch {
            revision: buf_review::model::BatchRevision {
                number: 0,
                namespace: Some(new_id.0.clone())
            },
            id: new_id.clone(),
            review: reviews[1].review.id.clone(),
            write: WriteState {
                events: vec![event],
            },
        })
    );
    let mut batches = reopened.batch_ids(&reviews[1].review.id).unwrap();
    batches.sort();
    let mut expected = vec![BatchId(reviews[1].review.id.0.clone()), new_id];
    expected.sort();
    assert_eq!(batches, expected);
    for legacy in &reviews {
        assert_original_data(&connection, legacy);
    }
}

#[test]
fn failed_migration_leaves_original_database_intact() {
    let project = tempdir().unwrap();
    let reviews = legacy_reviews();
    let connection = seed_legacy(project.path(), &reviews);
    connection
        .execute_batch("CREATE TABLE legacy_write_events (marker TEXT) STRICT;")
        .unwrap();

    assert!(Store::open(project.path()).is_err());
    assert!(
        connection
            .column_exists(None, "write_events", "review_id")
            .unwrap()
    );
    assert!(
        !connection
            .column_exists(None, "write_events", "batch_id")
            .unwrap()
    );
    let has_batches: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'batches')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_batches);
    connection
        .execute_batch("DROP TABLE legacy_write_events;")
        .unwrap();

    let mut store = Store::open(project.path()).unwrap();
    for legacy in &reviews {
        assert_original_data(&connection, legacy);
        assert_eq!(
            store
                .get_batch(&BatchId(legacy.review.id.0.clone()))
                .unwrap()
                .unwrap()
                .write
                .events,
            legacy.events
        );
    }
}

#[test]
fn original_comment_and_submitted_review_json_remains_readable() {
    // These strings use the original model's field names and required authors.
    const USER: &str = r#"{"id":"reviewer","name":"Reviewer"}"#;
    const SNAPSHOT: &str = r#"{
        "pull_request": {
            "id":"pr", "title":"Original", "body":"Body",
            "author":{"id":"reviewer","name":"Reviewer"}, "status":"open",
            "revision":{"base":"base","head":"head"}
        },
        "files":[],
        "comments":[{
            "id":"remote-comment", "body":"Original comment",
            "author":{"id":"reviewer","name":"Reviewer"},
            "target":{"kind":"diff","target":{
                "revision":{"base":"base","head":"head"}, "path":"src/lib.rs",
                "range":{"start":{"side":"head","line":2},"end":{"side":"head","line":4}}
            }}
        }],
        "reactions":[{"comment":"remote-comment","author":"reviewer","reaction":"👍"}],
        "conclusions":[{
            "author":{"id":"reviewer","name":"Reviewer"},
            "conclusion":{"outcome":"approve","body":"Original conclusion"}
        }]
    }"#;
    const EVENT: &str = r#"{
        "kind":"create_comment",
        "comment":{
            "id":"draft", "body":"Original draft",
            "author":{"id":"reviewer","name":"Reviewer"},
            "target":{"kind":"diff","target":{
                "revision":{"base":"base","head":"head"}, "path":"src/lib.rs", "range":null
            }}
        }
    }"#;

    let project = tempdir().unwrap();
    let connection = seed_legacy(project.path(), &[]);
    connection
        .execute(
            "INSERT INTO reviews (review_id, reviewer, committed) VALUES ('legacy', ?1, 1)",
            [USER],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO read_states (review_id, snapshot) VALUES ('legacy', ?1)",
            [SNAPSHOT],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO write_events (review_id, sequence, event) VALUES ('legacy', 1, ?1)",
            [EVENT],
        )
        .unwrap();

    let mut store = Store::open(project.path()).unwrap();
    let read = store
        .get_review(&ReviewId("legacy".into()))
        .unwrap()
        .unwrap()
        .read;
    let known_user = User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    };
    assert_eq!(read.pull_request.author, Some(known_user.clone()));
    assert_eq!(read.comments[0].author, Some(known_user.clone()));
    assert_eq!(
        read.comments[0].content.id,
        CommentId("remote-comment".into())
    );
    assert_eq!(read.comments[0].content.body, "Original comment");
    let CommentTarget::Diff(location) = &read.comments[0].target else {
        panic!("original diff target was not preserved");
    };
    assert_eq!(
        location.original.revision,
        ReadCommentRevision::Comparison(DiffRevision {
            base: "base".into(),
            head: "head".into(),
        })
    );
    assert_eq!(location.original.position.path, "src/lib.rs");
    let range = location.original.position.range.as_ref().unwrap();
    assert_eq!((range.start.line.get(), range.end.line.get()), (2, 4));
    assert_eq!(range.start.side, DiffSide::Head);
    assert_eq!(range.end.side, DiffSide::Head);
    assert_eq!(location.current, None);
    assert_eq!(location.subject, None);
    assert_eq!(location.diff_hunk, None);
    assert_eq!(location.outdated, None);
    assert_eq!(read.reactions[0].author, Some(known_user.id.clone()));
    assert_eq!(
        read.reviews,
        vec![ReadReview {
            id: None,
            author: Some(known_user.clone()),
            state: ReadReviewState::Submitted(ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: Some("Original conclusion".into()),
            }),
        }]
    );
    assert_eq!(
        store
            .get_batch(&BatchId("legacy".into()))
            .unwrap()
            .unwrap()
            .write
            .events,
        vec![Event::CreateComment {
            comment: DraftComment {
                content: CommentContent {
                    id: CommentId("draft".into()),
                    body: "Original draft".into(),
                },
                author: known_user,
                target: CommentTarget::Diff(DiffLocation {
                    revision: DiffRevision {
                        base: "base".into(),
                        head: "head".into(),
                    },
                    position: CommentPosition {
                        path: "src/lib.rs".into(),
                        range: None,
                    },
                }),
            },
        }]
    );
    let raw = connection
        .query_row(
            "SELECT snapshot, event FROM read_states
             JOIN write_events ON batch_id = read_states.review_id WHERE review_id = 'legacy'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!(raw, (SNAPSHOT.into(), EVENT.into()));
}

#[test]
fn workspace_migration_preserves_legacy_operation_namespaces_journals_and_aliases() {
    use buf_review::{
        core,
        store::OperationResult,
        workspace::{Destination, PublicationAttempt, Workspace},
    };
    let project = tempdir().unwrap();
    let review = legacy_reviews().remove(0).review;
    let directory = project.path().join(".review");
    fs::create_dir(&directory).unwrap();
    let connection = Connection::open(directory.join("state.sqlite3")).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    // The immediate predecessor schema: one workspace per PR/account, unversioned logs.
    connection.execute_batch("CREATE TABLE reviews(review_id TEXT PRIMARY KEY,reviewer TEXT NOT NULL);CREATE TABLE read_states(review_id TEXT PRIMARY KEY REFERENCES reviews(review_id),snapshot TEXT NOT NULL);CREATE TABLE batches(batch_id TEXT PRIMARY KEY,review_id TEXT NOT NULL REFERENCES reviews(review_id));CREATE TABLE write_events(batch_id TEXT REFERENCES batches(batch_id),sequence INTEGER,event TEXT,PRIMARY KEY(batch_id,sequence));CREATE TABLE workspaces(workspace_id TEXT PRIMARY KEY,reviewer_id TEXT,pull_request_id TEXT,locator TEXT,generation INTEGER,state TEXT,UNIQUE (reviewer_id, pull_request_id));").unwrap();
    connection
        .execute_batch(include_str!("../src/schema.sql"))
        .unwrap();
    connection
        .execute(
            "INSERT INTO reviews VALUES (?1,?2)",
            (
                &review.id.0,
                serde_json::to_string(&review.reviewer).unwrap(),
            ),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO read_states VALUES (?1,?2)",
            (&review.id.0, serde_json::to_string(&review.read).unwrap()),
        )
        .unwrap();
    let mut first = Workspace::new(review.clone());
    first.id = "old-workspace".into();
    first.batch.id = BatchId("old-log".into());
    first.batch.write.events = vec![Event::CreateComment {
        comment: DraftComment {
            content: CommentContent {
                id: CommentId("root".into()),
                body: "original".into(),
            },
            author: review.reviewer.clone(),
            target: CommentTarget::PullRequest,
        },
    }];
    let unknown = core::prepare(&review, &first.batch, &[])
        .unwrap()
        .operations
        .remove(0);
    let mut current = first.clone();
    current.generation = 1;
    current.batch.id = BatchId("current-log".into());
    current.batch.write.events.push(Event::EditComment {
        comment: CommentId("root".into()),
        body: "saved".into(),
    });
    current.suspended_attempts.push(PublicationAttempt {
        id: "unknown-attempt".into(),
        destination: Destination::Standalone,
        operations: vec![unknown.clone()],
        uncertain: true,
        provider: serde_json::json!({"inflight":format!("operation {}",unknown.id)}),
        wip: None,
    });
    // A receipt for a separate resource is history, not an outstanding baseline delta.
    let receipt = OperationResult {
        operation: core::Operation {
            id: "known-progress".into(),
            change: Event::SetFileViewed {
                path: "old.rs".into(),
                viewed: true,
                commit: None,
            },
        },
        mappings: vec![],
        response: serde_json::json!({"ok":true}),
    };
    current.results.push(receipt.clone());
    for ws in [&first, &current] {
        connection
            .execute(
                "INSERT INTO batches VALUES (?1,?2)",
                (&ws.batch.id.0, &review.id.0),
            )
            .unwrap();
        for (i, event) in ws.batch.write.events.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO write_events VALUES (?1,?2,?3)",
                    (
                        &ws.batch.id.0,
                        (i + 1) as i64,
                        serde_json::to_string(event).unwrap(),
                    ),
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO workspace_history VALUES (?1,?2,?3,?4)",
                (
                    &first.id,
                    ws.generation as i64,
                    serde_json::to_string(ws).unwrap(),
                    r#"{"action":"provider_update"}"#,
                ),
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO operation_results VALUES (?1,?2,1,?3,'[]',?4)",
            (
                &current.batch.id.0,
                &receipt.operation.id,
                serde_json::to_string(&receipt.operation).unwrap(),
                serde_json::to_string(&receipt.response).unwrap(),
            ),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO workspaces VALUES (?1,?2,?3,NULL,1,?4)",
            (
                &first.id,
                &review.reviewer.id.0,
                &review.read.pull_request.id.0,
                serde_json::to_string(&current).unwrap(),
            ),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO workspace_active VALUES (?1,?2,1)",
            (&review.reviewer.id.0, &first.id),
        )
        .unwrap();
    drop(connection);
    let mut store = Store::open(project.path()).unwrap();
    let migrated = store.workspace("old-workspace").unwrap().unwrap();
    assert_eq!(migrated.id, "current-log");
    assert_eq!(migrated.batch.id.0, "current-log");
    assert_eq!(migrated.suspended_attempts, current.suspended_attempts);
    assert!(migrated.completed.is_empty());
    assert_eq!(migrated.results, vec![receipt]);
    assert_eq!(
        store
            .active_workspace(&review.reviewer.id)
            .unwrap()
            .unwrap()
            .id,
        "current-log"
    );
    assert_eq!(
        store
            .get_batch(&BatchId("old-log".into()))
            .unwrap()
            .unwrap(),
        migrated.batch
    );
    let old = store
        .batch_revision(&migrated.batch.id, 0)
        .unwrap()
        .unwrap();
    assert_eq!(
        core::prepare(&review, &old, &[]).unwrap().operations[0].id,
        unknown.id
    );
    assert_eq!(store.batch_history(&migrated.batch.id).unwrap().len(), 2);
    assert_eq!(store.batch_workspaces().unwrap().len(), 1);
    let another = store.create_batch_workspace(&review.id).unwrap();
    assert_ne!(another.id, migrated.id);
    drop(store);
    let reopened = Store::open(project.path()).unwrap();
    assert_eq!(reopened.batch_workspaces().unwrap().len(), 2);
    assert_eq!(
        reopened
            .workspace("old-workspace")
            .unwrap()
            .unwrap()
            .suspended_attempts,
        migrated.suspended_attempts
    );
}
