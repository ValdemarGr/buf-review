use std::{fs, path::Path};

use buf_review::{
    model::*,
    store::{IdMapping, OperationResult, ReviewBackend, ReviewSource, Store},
    workspace::{Action, Destination, Workspace},
};
use rusqlite::{Connection, types::ValueRef};
use serde_json::json;
use tempfile::tempdir;

fn user() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn baseline(padding: usize) -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Storage regression".into(),
            body: String::new(),
            author: Some(user()),
            status: PullRequestStatus::Open,
            source: None,
            revision: DiffRevision {
                base: "base".into(),
                head: "head".into(),
            },
        },
        files: vec![ChangedFile {
            path: "file.rs".into(),
            previous_path: None,
            change: FileChange::Modified,
            stats: None,
            viewed: false,
        }],
        comments: [
            (
                "remote",
                format!("REMOTE_STORAGE_SENTINEL {}", "r".repeat(padding)),
            ),
            ("deleted", "Removed by next remote snapshot".into()),
        ]
        .into_iter()
        .map(|(id, body)| ReadComment {
            content: CommentContent {
                id: CommentId(id.into()),
                body,
            },
            author: Some(user()),
            target: CommentTarget::PullRequest,
        })
        .collect(),
        reactions: Vec::new(),
        reviews: Vec::new(),
    }
}

fn backend(read: &ReadState) -> ReviewBackend {
    ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/acme/repo/pull/1".into(),
        },
        mappings: [("user", "reviewer", "7"), ("pull_request", "pull", "1")]
            .into_iter()
            .map(|(resource, core_id, remote_id)| IdMapping {
                resource: resource.into(),
                core_id: core_id.into(),
                remote_id: remote_id.into(),
            })
            .chain(read.comments.iter().map(|comment| {
                IdMapping {
                    resource: "issue_comment".into(),
                    core_id: comment.content.id.0.clone(),
                    remote_id: if comment.content.id.0 == "remote" {
                        "21"
                    } else {
                        "22"
                    }
                    .into(),
                }
            }))
            .collect(),
    }
}

fn apply(store: &mut Store, workspace: &mut Workspace, action: Action) {
    *workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, action)
        .unwrap();
}

fn save(body: String) -> Action {
    Action::Save {
        comment: None,
        target: Some(CommentTarget::PullRequest),
        body,
        destination: Some(Destination::Standalone),
    }
}

// Measure retained payload, independent of SQLite page allocation, WAL mode,
// and free pages awaiting vacuum. Views are deliberately excluded.
fn stored_text(project: &Path) -> Vec<String> {
    let connection = Connection::open(project.join(".review/state.sqlite3")).unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut values = Vec::new();
    for table in tables {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                if let ValueRef::Text(value) = row.get_ref(column).unwrap() {
                    values.push(String::from_utf8(value.to_vec()).unwrap());
                }
            }
        }
    }
    values
}

#[test]
fn local_history_does_not_repeat_remote_snapshots_events_or_receipts() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let review = ReviewId("large".into());
    let mut read = baseline(128 * 1024);
    read.files.extend((0..512).map(|index| ChangedFile {
        path: format!("src/large_pull_request/module_{index}/file.rs"),
        previous_path: None,
        change: FileChange::Modified,
        stats: None,
        viewed: false,
    }));
    store
        .import_review(&review, &user(), &read, &backend(&read))
        .unwrap();
    let mut workspace = store.create_batch_workspace(&review).unwrap();
    let before: usize = stored_text(project.path()).iter().map(String::len).sum();
    for index in 0..32 {
        apply(
            &mut store,
            &mut workspace,
            save(format!(
                "DRAFT_STORAGE_SENTINEL_{index}:{}",
                "d".repeat(4096)
            )),
        );
    }
    let plan = workspace.prepare(Destination::Standalone).unwrap();
    assert_eq!(plan.operations.len(), 32);
    apply(
        &mut store,
        &mut workspace,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations.clone(),
        },
    );
    for (index, operation) in plan.operations.into_iter().enumerate() {
        apply(
            &mut store,
            &mut workspace,
            Action::RecordResult {
                result: OperationResult {
                    operation,
                    mappings: Vec::new(),
                    response: json!({"body":format!("RECEIPT_STORAGE_SENTINEL_{index}:{}", "p".repeat(4096))}),
                },
            },
        );
    }
    apply(
        &mut store,
        &mut workspace,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    let published = stored_text(project.path());
    let published_bytes: usize = published.iter().map(String::len).sum();
    assert!(
        published_bytes - before < 6 * (64 * 4096) + 256 * 1024,
        "local history retained {} bytes for 256 KiB of new comment/receipt bodies",
        published_bytes - before
    );
    for index in 0..32 {
        for (kind, limit) in [("DRAFT", 8), ("RECEIPT", 4)] {
            let marker = format!("{kind}_STORAGE_SENTINEL_{index}:");
            let copies: usize = published
                .iter()
                .map(|text| text.matches(&marker).count())
                .sum();
            assert!(
                (1..=limit).contains(&copies),
                "{marker} retained {copies} copies"
            );
        }
    }
    let remote_copies: usize = published
        .iter()
        .map(|text| text.matches("REMOTE_STORAGE_SENTINEL").count())
        .sum();
    assert!(
        remote_copies <= 2,
        "remote comment repeated {remote_copies} times"
    );
    let actions = (0..32)
        .map(|index| Action::Viewed {
            path: "file.rs".into(),
            viewed: index % 2 == 0,
        })
        .collect();
    let result = store
        .mutate_workspace_actions(&workspace.id, workspace.generation, actions)
        .unwrap();
    assert!(result.failure.is_none());
    assert_eq!(result.applied, 32);
    let after = stored_text(project.path());
    let after_bytes: usize = after.iter().map(String::len).sum();
    assert!(
        after_bytes - published_bytes < 128 * 1024,
        "32 viewed actions retained {} bytes",
        after_bytes - published_bytes
    );
    assert_eq!(
        after
            .iter()
            .map(|text| text.matches("REMOTE_STORAGE_SENTINEL").count())
            .sum::<usize>(),
        remote_copies
    );
    drop(store);
    let mut reopened = Store::open(project.path()).unwrap();
    assert_eq!(
        reopened.workspace(&workspace.id).unwrap().unwrap(),
        result.workspace
    );
    assert_eq!(
        reopened
            .operation_results(&workspace.batch.id)
            .unwrap()
            .len(),
        32
    );
}

#[test]
fn identical_imports_share_payload_without_losing_per_snapshot_provider_identities() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let read = baseline(128 * 1024);
    let before: usize = stored_text(project.path()).iter().map(String::len).sum();
    let mut expected = Vec::new();
    for index in 0..16 {
        let review = ReviewId(format!("snapshot-{index}"));
        let mut metadata = backend(&read);
        metadata.mappings.push(IdMapping {
            resource: "issue_comment_node".into(),
            core_id: "remote".into(),
            remote_id: format!("NODE_{index}"),
        });
        store
            .import_review(&review, &user(), &read, &metadata)
            .unwrap();
        expected.push((review, metadata));
    }
    let retained = stored_text(project.path());
    let bytes: usize = retained.iter().map(String::len).sum();
    assert!(
        bytes - before < 128 * 1024 + 128 * 1024,
        "16 identical 128 KiB snapshots retained {} bytes",
        bytes - before
    );
    assert_eq!(
        retained
            .iter()
            .map(|text| text.matches("REMOTE_STORAGE_SENTINEL").count())
            .sum::<usize>(),
        1
    );
    drop(store);
    let mut reopened = Store::open(project.path()).unwrap();
    for (id, mut metadata) in expected {
        assert_eq!(reopened.get_review(&id).unwrap().unwrap().read, read);
        metadata
            .mappings
            .sort_by(|a, b| (&a.resource, &a.core_id).cmp(&(&b.resource, &b.core_id)));
        assert_eq!(reopened.review_backend(&id).unwrap().unwrap(), metadata);
    }
}

#[test]
fn migration_preserves_authoritative_refresh_local_intent_and_every_historical_generation() {
    let source = tempdir().unwrap();
    let mut store = Store::open(source.path()).unwrap();
    let old = ReviewId("old".into());
    let read = baseline(1024);
    store
        .import_review(&old, &user(), &read, &backend(&read))
        .unwrap();
    let mut workspace = store.create_batch_workspace(&old).unwrap();
    store.select_batch(&workspace.batch.id).unwrap();
    apply(
        &mut store,
        &mut workspace,
        save("Published before refresh".into()),
    );
    let published = workspace.saved_comment.clone().unwrap();
    let plan = workspace.prepare(Destination::Standalone).unwrap();
    apply(
        &mut store,
        &mut workspace,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations.clone(),
        },
    );
    apply(
        &mut store,
        &mut workspace,
        Action::RecordResult {
            result: OperationResult {
                operation: plan.operations[0].clone(),
                mappings: vec![IdMapping {
                    resource: "issue_comment".into(),
                    core_id: published.0.clone(),
                    remote_id: "31".into(),
                }],
                response: json!({"id":31,"optional":null,"nested":{"dot.key":null,"quote\"key":null}}),
            },
        },
    );
    apply(
        &mut store,
        &mut workspace,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    apply(
        &mut store,
        &mut workspace,
        Action::Save {
            comment: Some(CommentId("remote".into())),
            target: None,
            body: "Local edit against old remote text".into(),
            destination: None,
        },
    );
    apply(
        &mut store,
        &mut workspace,
        save("Uncertain new draft".into()),
    );
    let draft = workspace.saved_comment.clone().unwrap();
    let plan = workspace.prepare(Destination::Standalone).unwrap();
    apply(
        &mut store,
        &mut workspace,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
    let provider_nulls = json!({"inflight":null,"nested":{"dot.key":null,"quote\"key":null,"remove":true},"absent_to_null":null});
    apply(
        &mut store,
        &mut workspace,
        Action::SetAttemptProvider {
            provider: provider_nulls.clone(),
        },
    );
    let null_generation = workspace.generation;
    let provider_missing =
        json!({"nested":{"dot.key":"changed","quote\"key":null},"null_to_absent":null});
    apply(
        &mut store,
        &mut workspace,
        Action::SetAttemptProvider {
            provider: provider_missing.clone(),
        },
    );
    let missing_generation = workspace.generation;
    apply(
        &mut store,
        &mut workspace,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("Provider response lost".into()),
        },
    );
    let suspended = workspace.suspended_attempts.clone();
    let mut fresh = read.clone();
    fresh.pull_request.revision.head = "new-head".into();
    fresh.files.push(ChangedFile {
        path: "fresh.rs".into(),
        previous_path: None,
        change: FileChange::Added,
        stats: None,
        viewed: false,
    });
    fresh
        .comments
        .retain(|comment| comment.content.id.0 != "deleted");
    fresh.comments[0].content.body = "Authoritative remote edit".into();
    let fresh_id = ReviewId("fresh".into());
    store
        .import_review(&fresh_id, &user(), &fresh, &backend(&fresh))
        .unwrap();
    workspace = store
        .refresh_workspace(&workspace.id, workspace.generation, &fresh_id)
        .unwrap();
    assert_eq!(workspace.review.read, fresh);
    assert_eq!(workspace.comparison, read.pull_request.revision);
    assert_eq!(workspace.comparison_files, read.files);
    assert_eq!(workspace.suspended_attempts, suspended);
    assert!(
        workspace
            .conflicts
            .contains_key(&CommentId("remote".into()))
    );
    let view = workspace.view().unwrap();
    assert!(
        view.comments
            .iter()
            .all(|row| row.comment.content().id.0 != "deleted")
    );
    assert!(
        view.comments
            .iter()
            .any(|row| row.comment.content().id == draft)
    );
    assert_eq!(workspace.results.len(), 1);
    let history = store.batch_history(&workspace.batch.id).unwrap();
    let actions = store.workspace_log(&workspace.id).unwrap();
    let publications =
        serde_json::to_value(store.publication_history(&workspace).unwrap()).unwrap();
    let expected_view = serde_json::to_value(store.workspace_view(&workspace.id).unwrap()).unwrap();

    // Seed an actual pre-compaction database: full Workspace JSON in both the
    // current row and every generation, with independent baseline/event/result
    // tables. No normalized-storage tables are copied into this fixture.
    let legacy = tempdir().unwrap();
    fs::create_dir(legacy.path().join(".review")).unwrap();
    let connection = Connection::open(legacy.path().join(".review/state.sqlite3")).unwrap();
    connection.execute_batch(r#"
        CREATE TABLE pull_requests(pull_request_id TEXT PRIMARY KEY NOT NULL,object TEXT NOT NULL);
        CREATE TABLE reviews(review_id TEXT PRIMARY KEY NOT NULL,reviewer TEXT NOT NULL);
        CREATE TABLE read_states(review_id TEXT PRIMARY KEY NOT NULL,snapshot TEXT NOT NULL);
        CREATE TABLE batches(batch_id TEXT PRIMARY KEY NOT NULL,review_id TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE batch_revisions(batch_id TEXT NOT NULL,revision INTEGER NOT NULL,review_id TEXT NOT NULL,namespace TEXT NOT NULL,PRIMARY KEY(batch_id,revision));
        CREATE TABLE write_events(batch_id TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 0,sequence INTEGER NOT NULL,event TEXT NOT NULL,PRIMARY KEY(batch_id,revision,sequence));
        CREATE TABLE review_sources(review_id TEXT PRIMARY KEY NOT NULL,backend TEXT NOT NULL,locator TEXT NOT NULL);
        CREATE TABLE id_mappings(review_id TEXT NOT NULL,resource TEXT NOT NULL,core_id TEXT NOT NULL,remote_id TEXT NOT NULL,PRIMARY KEY(review_id,resource,core_id),UNIQUE(review_id,resource,remote_id));
        CREATE TABLE operation_results(batch_id TEXT NOT NULL,operation_id TEXT NOT NULL,sequence INTEGER NOT NULL,operation TEXT NOT NULL,mappings TEXT NOT NULL,response TEXT NOT NULL,PRIMARY KEY(batch_id,operation_id),UNIQUE(batch_id,sequence));
        CREATE TABLE workspaces(workspace_id TEXT PRIMARY KEY NOT NULL,reviewer_id TEXT NOT NULL,pull_request_id TEXT NOT NULL,locator TEXT,generation INTEGER NOT NULL,state TEXT NOT NULL);
        CREATE TABLE workspace_history(workspace_id TEXT NOT NULL,generation INTEGER NOT NULL,state TEXT NOT NULL,action TEXT NOT NULL,PRIMARY KEY(workspace_id,generation));
        CREATE TABLE workspace_active(reviewer_id TEXT PRIMARY KEY NOT NULL,workspace_id TEXT NOT NULL,selected INTEGER NOT NULL);
    "#).unwrap();
    let pull = store
        .pull_request(&workspace.review.read.pull_request.id)
        .unwrap()
        .unwrap();
    connection
        .execute(
            "INSERT INTO pull_requests VALUES (?1,?2)",
            (&pull.id.0, serde_json::to_string(&pull).unwrap()),
        )
        .unwrap();
    for id in store.review_ids().unwrap() {
        let review = store.get_review(&id).unwrap().unwrap();
        connection
            .execute(
                "INSERT INTO reviews VALUES (?1,?2)",
                (&id.0, serde_json::to_string(&review.reviewer).unwrap()),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO read_states VALUES (?1,?2)",
                (&id.0, serde_json::to_string(&review.read).unwrap()),
            )
            .unwrap();
        let metadata = store.review_backend(&id).unwrap().unwrap();
        connection
            .execute(
                "INSERT INTO review_sources VALUES (?1,?2,?3)",
                (&id.0, &metadata.source.backend, &metadata.source.locator),
            )
            .unwrap();
        for mapping in metadata.mappings {
            connection
                .execute(
                    "INSERT INTO id_mappings VALUES (?1,?2,?3,?4)",
                    (&id.0, mapping.resource, mapping.core_id, mapping.remote_id),
                )
                .unwrap();
        }
    }
    connection
        .execute(
            "INSERT INTO batches VALUES (?1,?2,?3)",
            (
                &workspace.batch.id.0,
                &workspace.batch.review.0,
                workspace.batch.revision.number as i64,
            ),
        )
        .unwrap();
    for revision in 0..=workspace.batch.revision.number {
        let batch = store
            .batch_revision(&workspace.batch.id, revision)
            .unwrap()
            .unwrap();
        connection
            .execute(
                "INSERT INTO batch_revisions VALUES (?1,?2,?3,?4)",
                (
                    &batch.id.0,
                    revision as i64,
                    &batch.review.0,
                    batch.revision.namespace.as_deref().unwrap_or(&batch.id.0),
                ),
            )
            .unwrap();
        for (index, event) in batch.write.events.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO write_events VALUES (?1,?2,?3,?4)",
                    (
                        &batch.id.0,
                        revision as i64,
                        (index + 1) as i64,
                        serde_json::to_string(event).unwrap(),
                    ),
                )
                .unwrap();
        }
    }
    for (index, result) in workspace.results.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO operation_results VALUES (?1,?2,?3,?4,?5,?6)",
                (
                    &workspace.batch.id.0,
                    &result.operation.id,
                    (index + 1) as i64,
                    serde_json::to_string(&result.operation).unwrap(),
                    serde_json::to_string(&result.mappings).unwrap(),
                    serde_json::to_string(&result.response).unwrap(),
                ),
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO workspaces VALUES (?1,?2,?3,?4,?5,?6)",
            (
                &workspace.id,
                &workspace.review.reviewer.id.0,
                &workspace.review.read.pull_request.id.0,
                &workspace.locator,
                workspace.generation as i64,
                serde_json::to_string(&workspace).unwrap(),
            ),
        )
        .unwrap();
    for entry in &history {
        connection
            .execute(
                "INSERT INTO workspace_history VALUES (?1,?2,?3,?4)",
                (
                    &workspace.id,
                    entry.generation as i64,
                    serde_json::to_string(&entry.state).unwrap(),
                    serde_json::to_string(&entry.action).unwrap(),
                ),
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO workspace_active VALUES (?1,?2,1)",
            (&workspace.review.reviewer.id.0, &workspace.id),
        )
        .unwrap();
    drop(connection);
    drop(store);

    for _ in 0..2 {
        let mut reopened = Store::open(legacy.path()).unwrap();
        assert_eq!(
            reopened.workspace(&workspace.id).unwrap().unwrap(),
            workspace
        );
        assert_eq!(
            reopened.last_active_workspace().unwrap().unwrap(),
            workspace
        );
        assert_eq!(reopened.workspace_log(&workspace.id).unwrap(), actions);
        assert_eq!(
            serde_json::to_value(reopened.publication_history(&workspace).unwrap()).unwrap(),
            publications
        );
        assert_eq!(
            serde_json::to_value(reopened.workspace_view(&workspace.id).unwrap()).unwrap(),
            expected_view
        );
        let migrated = reopened.batch_history(&workspace.batch.id).unwrap();
        assert_eq!(migrated.len(), history.len());
        for (expected, actual) in history.iter().zip(migrated) {
            assert_eq!(actual.generation, expected.generation);
            assert_eq!(actual.action, expected.action);
            assert_eq!(
                actual.state, expected.state,
                "generation {}",
                expected.generation
            );
            assert_eq!(
                reopened
                    .workspace_at_generation(&workspace.id, expected.generation)
                    .unwrap()
                    .unwrap(),
                expected.state
            );
        }
        let explicit_null = reopened
            .workspace_at_generation(&workspace.id, null_generation)
            .unwrap()
            .unwrap();
        assert_eq!(explicit_null.attempt.unwrap().provider, provider_nulls);
        let missing = reopened
            .workspace_at_generation(&workspace.id, missing_generation)
            .unwrap()
            .unwrap();
        assert_eq!(missing.attempt.unwrap().provider, provider_missing);
        assert_eq!(
            reopened.operation_results(&workspace.batch.id).unwrap(),
            workspace.results
        );
    }
}

#[test]
fn unchanged_automatic_fetch_reuses_review_id_but_new_metadata_or_remote_state_does_not() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let read = baseline(1024);
    let metadata = backend(&read);
    let first = store
        .cache_import_review(&user(), &read, &metadata, None)
        .unwrap();
    for _ in 0..8 {
        assert_eq!(
            store
                .cache_import_review(&user(), &read, &metadata, None)
                .unwrap(),
            first
        );
    }
    let mut changed_metadata = metadata.clone();
    changed_metadata.mappings.push(IdMapping {
        resource: "issue_comment_node".into(),
        core_id: "remote".into(),
        remote_id: "NODE_REMOTE".into(),
    });
    let richer = store
        .cache_import_review(&user(), &read, &changed_metadata, None)
        .unwrap();
    assert_ne!(richer, first);
    assert_eq!(
        store
            .cache_import_review(&user(), &read, &changed_metadata, None)
            .unwrap(),
        richer
    );
    let mut changed = read.clone();
    changed.comments.remove(1);
    changed.comments[0].content.body = "Remote edit on unchanged head commit".into();
    let edited = store
        .cache_import_review(&user(), &changed, &backend(&changed), None)
        .unwrap();
    assert_ne!(edited, first);
    assert_ne!(edited, richer);
    assert_eq!(store.get_review(&first).unwrap().unwrap().read, read);
    assert_eq!(store.get_review(&edited).unwrap().unwrap().read, changed);
    let local = store.cache_review(&user(), &read).unwrap();
    assert_eq!(store.cache_review(&user(), &read).unwrap(), local);
}

#[test]
fn history_deltas_capture_provider_binding_before_the_next_local_save() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let branches = BranchPair {
        source: BranchSource {
            repository: "https://github.com/acme/repo".into(),
            branch: "feature".into(),
        },
        sink: BranchSource {
            repository: "https://github.com/acme/repo".into(),
            branch: "main".into(),
        },
    };
    let object = store
        .create_or_use_intent(&branches, "Local PR", "")
        .unwrap();
    let mut read = baseline(0);
    read.pull_request.id = object.id.clone();
    read.pull_request.status = PullRequestStatus::Local;
    read.pull_request.source = Some(branches.source);
    let review = ReviewId("local".into());
    store.create_review(&review, &user(), &read).unwrap();
    let initial = store.create_batch_workspace(&review).unwrap();
    assert_eq!(initial.locator, None);
    let remote = backend(&read);
    store
        .bind_pull_request(&object.id, object.revision, &remote, None, None)
        .unwrap();
    let current = store
        .mutate_workspace(
            &initial.id,
            initial.generation,
            save("After PR binding".into()),
        )
        .unwrap();
    assert_eq!(
        current.locator.as_deref(),
        Some(remote.source.locator.as_str())
    );
    drop(store);

    let reopened = Store::open(project.path()).unwrap();
    assert_eq!(reopened.workspace(&current.id).unwrap().unwrap(), current);
    assert_eq!(
        reopened
            .workspace_at_generation(&current.id, current.generation)
            .unwrap()
            .unwrap(),
        current
    );
    assert_eq!(
        reopened
            .workspace_at_generation(&initial.id, initial.generation)
            .unwrap()
            .unwrap(),
        initial
    );
    let history = reopened.batch_history(&current.batch.id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].state, initial);
    assert_eq!(history[1].state, current);
}
