use buf_review::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store, StoreError},
};
use rusqlite::Connection;
use tempfile::tempdir;

fn reviewer() -> User {
    User {
        id: UserId("local-reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn snapshot() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("local-pr".into()),
            title: "Fetched PR".into(),
            body: "Snapshot without provider identities".into(),
            author: None,
            status: PullRequestStatus::Open,
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
            locator: "owner/repository#42".into(),
        },
        mappings: vec![
            IdMapping {
                resource: "pull_request".into(),
                core_id: "local-pr".into(),
                remote_id: "remote-pr".into(),
            },
            IdMapping {
                resource: "user".into(),
                core_id: "local-reviewer".into(),
                remote_id: "remote-user".into(),
            },
        ],
    }
}

#[test]
fn imported_snapshot_and_provider_mappings_survive_reopening_separately() {
    let project = tempdir().unwrap();
    let id = ReviewId("imported".into());
    let other = ReviewId("another-import".into());
    let read = snapshot();
    let metadata = backend();
    {
        let mut store = Store::open(project.path()).unwrap();
        store
            .import_review(&id, &reviewer(), &read, &metadata)
            .unwrap();
        // Provider identities are scoped to the local review, not the database.
        store
            .import_review(&other, &reviewer(), &read, &metadata)
            .unwrap();
    }

    let mut reopened = Store::open(project.path()).unwrap();
    for review_id in [id, other] {
        assert_eq!(
            reopened.get_review(&review_id).unwrap(),
            Some(Review {
                id: review_id.clone(),
                reviewer: reviewer(),
                read: read.clone(),
            })
        );
        assert_eq!(
            reopened.review_backend(&review_id).unwrap(),
            Some(metadata.clone())
        );
        assert!(reopened.batch_ids(&review_id).unwrap().is_empty());
    }
}

#[test]
fn duplicate_core_or_remote_mapping_rolls_back_entire_import() {
    let project = tempdir().unwrap();
    let existing_id = ReviewId("existing".into());
    let batch_id = BatchId("existing-batch".into());
    let event = Event::SetFileViewed {
        path: "src/lib.rs".into(),
        viewed: true,
    };
    let mut store = Store::open(project.path()).unwrap();
    store
        .import_review(&existing_id, &reviewer(), &snapshot(), &backend())
        .unwrap();
    store.create_batch(&batch_id, &existing_id).unwrap();
    store.append_event(&batch_id, &event).unwrap();
    let original_review = store.get_review(&existing_id).unwrap();
    let original_batch = store.get_batch(&batch_id).unwrap();
    let original_backend = store.review_backend(&existing_id).unwrap();
    let connection = Connection::open(project.path().join(".review/state.sqlite3")).unwrap();

    for duplicate_core in [true, false] {
        let id = ReviewId(
            if duplicate_core {
                "duplicate-core"
            } else {
                "duplicate-remote"
            }
            .into(),
        );
        let mut metadata = backend();
        let mut duplicate = metadata.mappings[0].clone();
        if duplicate_core {
            duplicate.remote_id = "different-remote-pr".into();
        } else {
            duplicate.core_id = "different-local-pr".into();
        }
        metadata.mappings.push(duplicate);

        assert!(matches!(
            store.import_review(&id, &reviewer(), &snapshot(), &metadata),
            Err(StoreError::Sqlite(_))
        ));
        assert_eq!(store.get_review(&id).unwrap(), None);
        assert_eq!(store.review_backend(&id).unwrap(), None);
        let leftover_rows: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM reviews WHERE review_id = ?1)
                      + (SELECT COUNT(*) FROM read_states WHERE review_id = ?1)
                      + (SELECT COUNT(*) FROM review_sources WHERE review_id = ?1)
                      + (SELECT COUNT(*) FROM id_mappings WHERE review_id = ?1)",
                [&id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftover_rows, 0);
        assert_eq!(store.get_review(&existing_id).unwrap(), original_review);
        assert_eq!(store.get_batch(&batch_id).unwrap(), original_batch);
        assert_eq!(
            store.review_backend(&existing_id).unwrap(),
            original_backend
        );
    }
}

#[test]
fn locally_created_review_has_no_provider_metadata() {
    let project = tempdir().unwrap();
    let id = ReviewId("local".into());
    let mut store = Store::open(project.path()).unwrap();
    store.create_review(&id, &reviewer(), &snapshot()).unwrap();
    assert_eq!(store.review_backend(&id).unwrap(), None);
    assert_eq!(
        store.review_backend(&ReviewId("missing".into())).unwrap(),
        None
    );
    drop(store);

    let mut reopened = Store::open(project.path()).unwrap();
    assert_eq!(reopened.review_backend(&id).unwrap(), None);
    assert_eq!(reopened.get_review(&id).unwrap().unwrap().read, snapshot());
}
