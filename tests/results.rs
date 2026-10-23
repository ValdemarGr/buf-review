use buf_review::{
    core::{Change, Operation},
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store, StoreError},
};
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;

fn reviewer() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn snapshot() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pr".into()),
            title: "Review".into(),
            body: String::new(),
            author: None,
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![],
        comments: vec![],
        reactions: vec![],
        reviews: vec![],
    }
}

fn backend() -> ReviewBackend {
    ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/owner/repo/pull/1".into(),
        },
        mappings: vec![IdMapping {
            resource: "pull_request".into(),
            core_id: "pr".into(),
            remote_id: "1".into(),
        }],
    }
}

fn operation(id: &str, comment: &str) -> Operation {
    Operation {
        id: id.into(),
        change: Change::CreateComment {
            comment: DraftComment {
                content: CommentContent {
                    id: CommentId(comment.into()),
                    body: "Draft".into(),
                },
                author: reviewer(),
                target: CommentTarget::PullRequest,
            },
        },
    }
}

#[test]
fn results_and_identities_survive_reopening_without_changing_snapshot_or_log() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let first = BatchId("first".into());
    let second = BatchId("second".into());
    let operation;
    let mapping = IdMapping {
        resource: "issue_comment".into(),
        core_id: "draft".into(),
        remote_id: "42".into(),
    };
    let event = self::operation("unused", "draft").change;
    let response = json!({"id": 42, "html_url": "https://github.com/comment/42"});
    {
        let mut store = Store::open(project.path()).unwrap();
        store
            .import_review(&review, &reviewer(), &snapshot(), &backend())
            .unwrap();
        store.create_batch(&first, &review).unwrap();
        store.create_batch(&second, &review).unwrap();
        store.append_event(&first, &event).unwrap();
        operation = store
            .workspace(&first.0)
            .unwrap()
            .unwrap()
            .publication_plan()
            .unwrap()
            .operations
            .remove(0);
        store
            .record_operation_result(
                &first,
                &operation,
                std::slice::from_ref(&mapping),
                &response,
            )
            .unwrap();
    }
    let mut store = Store::open(project.path()).unwrap();
    let results = store.operation_results(&first).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].operation, operation);
    assert_eq!(
        results[0].mappings.as_slice(),
        std::slice::from_ref(&mapping)
    );
    assert_eq!(results[0].response, response);
    assert!(store.operation_results(&second).unwrap().is_empty());
    assert_eq!(store.get_review(&review).unwrap().unwrap().read, snapshot());
    assert_eq!(
        store
            .get_batch(&first)
            .unwrap()
            .unwrap()
            .write
            .events
            .into_iter()
            .map(|record| record.event)
            .collect::<Vec<_>>(),
        [event]
    );
    assert!(
        store
            .review_backend(&review)
            .unwrap()
            .unwrap()
            .mappings
            .contains(&mapping)
    );
    // A logical comment identity in the original event joins its server result.
    let database = Connection::open(project.path().join(".review/state.sqlite3")).unwrap();
    let remote: String = database
        .query_row(
            "SELECT remote_id FROM write_events
             JOIN batches USING (batch_id)
             LEFT JOIN id_mappings
             ON id_mappings.review_id = batches.review_id
             AND id_mappings.core_id = json_extract(write_events.event, '$.comment.id')
             AND id_mappings.resource = 'issue_comment'
             WHERE batch_id = ?1",
            [&first.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remote, "42");
    let completed_remote: i64 = database
        .query_row(
            "SELECT json_extract(operation_results.response, '$.id')
             FROM write_events
             LEFT JOIN operation_results
             ON operation_results.batch_id = write_events.batch_id
             AND json_extract(operation_results.operation, '$.change.comment.id')
                 = json_extract(write_events.event, '$.comment.id')
             WHERE write_events.batch_id = ?1",
            [&first.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completed_remote, 42);
    // Saving success does not freeze a batch.
    store
        .append_event(&first, &self::operation("later", "later-draft").change)
        .unwrap();
    assert_eq!(
        store.get_batch(&first).unwrap().unwrap().write.events.len(),
        2
    );
}

#[test]
fn failed_mapping_or_duplicate_result_rolls_back_all_local_success_data() {
    let project = tempdir().unwrap();
    let review = ReviewId("review".into());
    let batch = BatchId("batch".into());
    let mut store = Store::open(project.path()).unwrap();
    store
        .import_review(&review, &reviewer(), &snapshot(), &backend())
        .unwrap();
    store.create_batch(&batch, &review).unwrap();
    let first = operation("first", "first-draft");
    let first_mapping = IdMapping {
        resource: "issue_comment".into(),
        core_id: "first-draft".into(),
        remote_id: "42".into(),
    };
    store
        .record_operation_result(
            &batch,
            &first,
            std::slice::from_ref(&first_mapping),
            &json!({"id":42}),
        )
        .unwrap();
    let conflict = IdMapping {
        core_id: "second-draft".into(),
        ..first_mapping.clone()
    };
    let second = operation("second", "second-draft");
    assert!(matches!(
        store.record_operation_result(&batch, &second, &[conflict], &json!({"id":42})),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(store.operation_results(&batch).unwrap().len(), 1);
    assert_eq!(
        store
            .review_backend(&review)
            .unwrap()
            .unwrap()
            .mappings
            .len(),
        2
    );
    let replacement = IdMapping {
        remote_id: "100".into(),
        ..first_mapping.clone()
    };
    assert!(matches!(
        store.record_operation_result(&batch, &first, &[replacement], &json!({"id":100})),
        Err(StoreError::Sqlite(_))
    ));
    assert!(
        store
            .review_backend(&review)
            .unwrap()
            .unwrap()
            .mappings
            .contains(&first_mapping)
    );
    // The failed second result did not consume a sequence or operation ID.
    let mapping = IdMapping {
        resource: "issue_comment".into(),
        core_id: "second-draft".into(),
        remote_id: "43".into(),
    };
    store
        .record_operation_result(&batch, &second, &[mapping], &json!({"id":43}))
        .unwrap();
    let results = store.operation_results(&batch).unwrap();
    assert_eq!(
        results.iter().map(|r| &r.operation.id).collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn missing_batch_cannot_read_or_record_results() {
    let project = tempdir().unwrap();
    let batch = BatchId("missing".into());
    let mut store = Store::open(project.path()).unwrap();
    assert!(matches!(
        store.operation_results(&batch),
        Err(StoreError::BatchNotFound(id)) if id == batch
    ));
    assert!(matches!(
        store.record_operation_result(&batch, &operation("operation", "draft"), &[], &json!(null)),
        Err(StoreError::BatchNotFound(id)) if id == batch
    ));
}

#[test]
fn dismissal_reason_is_optional() {
    let event: Event =
        serde_json::from_value(json!({"kind":"dismiss_review","review":"review"})).unwrap();
    assert_eq!(
        event,
        Event::DismissReview {
            review: ReviewResourceId("review".into()),
            message: None
        }
    );
}

#[test]
fn publication_result_groups_are_atomic_and_preserve_newer_local_saves() {
    use buf_review::{
        store::OperationResult,
        workspace::{Action, Destination},
    };
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("group-review".into());
    let batch = BatchId("group-batch".into());
    store
        .import_review(&review, &reviewer(), &snapshot(), &backend())
        .unwrap();
    store.create_batch(&batch, &review).unwrap();
    for id in ["first", "second"] {
        store
            .append_event(&batch, &operation(id, id).change)
            .unwrap();
    }
    let workspace = store.workspace(&batch.0).unwrap().unwrap();
    let operations = workspace.publication_plan().unwrap().operations;
    let workspace = store
        .mutate_workspace(
            &batch.0,
            workspace.generation,
            Action::BeginPublication {
                destination: Destination::Standalone,
                operations: operations.clone(),
            },
        )
        .unwrap();
    let attempt = workspace.attempt.as_ref().unwrap().id.clone();
    let workspace = store
        .mutate_publication(
            &batch.0,
            &attempt,
            Action::SetAttemptProvider {
                provider: json!({"inflight":"operation batch","creating":operations}),
            },
        )
        .unwrap();
    // A local save can finish while the provider request is in flight.
    let before = store
        .mutate_workspace(
            &batch.0,
            workspace.generation,
            Action::Save {
                comment: None,
                target: Some(CommentTarget::PullRequest),
                body: "Saved during network request".into(),
                destination: None,
            },
        )
        .unwrap();
    let receipt = |operation: Operation| {
        let Change::CreateComment { comment } = &operation.change else {
            unreachable!()
        };
        let mapping = IdMapping {
            resource: "issue_comment".into(),
            core_id: comment.content.id.0.clone(),
            remote_id: format!("remote-{}", comment.content.id.0),
        };
        Action::RecordResult {
            result: OperationResult {
                operation,
                mappings: vec![mapping],
                response: json!({"confirmed":true}),
            },
        }
    };
    let mut invalid = operations[1].clone();
    invalid.id = "not-in-frozen-plan".into();
    assert!(
        store
            .mutate_publication_group(
                &batch.0,
                &attempt,
                vec![
                    Action::SetAttemptProvider {
                        provider: json!({"inflight":null})
                    },
                    receipt(operations[0].clone()),
                    receipt(invalid),
                ]
            )
            .is_err()
    );
    assert_eq!(store.workspace(&batch.0).unwrap().unwrap(), before);
    assert!(store.operation_results(&batch).unwrap().is_empty());

    let after = store
        .mutate_publication_group(
            &batch.0,
            &attempt,
            vec![
                Action::SetAttemptProvider {
                    provider: json!({"inflight":null}),
                },
                receipt(operations[0].clone()),
                receipt(operations[1].clone()),
            ],
        )
        .unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(after.results.len(), 2);
    assert_eq!(after.batch.write.events, before.batch.write.events);
    let pending = after.publication_plan().unwrap().operations;
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].change, Change::CreateComment { comment }
        if comment.content.body == "Saved during network request")
    );
    assert!(after.attempt.as_ref().unwrap().provider["inflight"].is_null());
    let mut conflicting = after.results[0].clone();
    conflicting.mappings[0].remote_id = "different-provider-comment".into();
    assert!(
        store
            .mutate_publication_group(
                &batch.0,
                &attempt,
                vec![Action::RecordResult {
                    result: conflicting
                }]
            )
            .is_err()
    );
    assert_eq!(store.workspace(&batch.0).unwrap().unwrap(), after);
    drop(store);
    let mut store = Store::open(project.path()).unwrap();
    assert_eq!(store.operation_results(&batch).unwrap().len(), 2);
    assert_eq!(
        store
            .workspace(&batch.0)
            .unwrap()
            .unwrap()
            .publication_plan()
            .unwrap()
            .operations,
        pending
    );
}
