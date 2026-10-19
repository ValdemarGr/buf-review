use buf_review::{
    model::*,
    store::{IdMapping, ReviewBackend, ReviewSource, Store},
    workspace::{Action, Destination, Workspace},
};
use tempfile::TempDir;

fn branches() -> BranchPair {
    BranchPair {
        source: BranchSource {
            repository: "https://github.com/author/project".into(),
            branch: "feature".into(),
        },
        sink: BranchSource {
            repository: "https://github.com/team/project".into(),
            branch: "master".into(),
        },
    }
}

fn reviewer() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn read(id: &PullRequestId, status: PullRequestStatus) -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: id.clone(),
            title: "Snapshot title".into(),
            body: "Snapshot body".into(),
            author: Some(reviewer()),
            status,
            source: Some(branches().source),
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

fn backend(locator: &str, core: &PullRequestId) -> ReviewBackend {
    ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: locator.into(),
        },
        mappings: vec![IdMapping {
            resource: "pull_request".into(),
            core_id: core.0.clone(),
            remote_id: "123".into(),
        }],
    }
}

fn local_session(store: &mut Store, object: &PullRequestObject) -> Workspace {
    let id = ReviewId(uuid::Uuid::new_v4().to_string());
    store
        .create_review(
            &id,
            &reviewer(),
            &read(&object.id, PullRequestStatus::Local),
        )
        .unwrap();
    store.create_batch_workspace(&id).unwrap()
}

fn save(body: &str) -> Action {
    Action::Save {
        comment: None,
        target: Some(CommentTarget::PullRequest),
        body: body.into(),
        destination: Some(Destination::Standalone),
    }
}

#[test]
fn intent_exists_before_reviews_and_pair_reuses_its_identity() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store.create_or_use_intent(&branches(), "", "").unwrap();
    assert!(store.review_ids().unwrap().is_empty());
    assert_eq!(
        store.pull_request(&object.id).unwrap(),
        Some(object.clone())
    );
    assert_eq!(
        store
            .create_or_use_intent(&branches(), "Ignored", "Ignored")
            .unwrap(),
        object
    );
    let mut same = branches();
    same.source.repository = same.source.repository.to_ascii_uppercase();
    assert_eq!(
        store.create_or_use_intent(&same, "", "").unwrap().id,
        object.id
    );
    same.source.branch = "Feature".into();
    assert_ne!(
        store.create_or_use_intent(&same, "", "").unwrap().id,
        object.id
    );
}

#[test]
fn description_is_shared_and_cas_does_not_mutate_comparison_or_baseline() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store.create_or_use_intent(&branches(), "", "").unwrap();
    let first = local_session(&mut store, &object);
    let second = local_session(&mut store, &object);
    let first = store
        .mutate_workspace(&first.id, first.generation, save("Draft comment"))
        .unwrap();
    let saved = store
        .mutate_workspace(
            &first.id,
            first.generation,
            Action::PrDetails {
                revision: 0,
                title: "New PR".into(),
                body: "Description".into(),
            },
        )
        .unwrap();
    assert_eq!(saved.generation, first.generation + 1);
    assert_eq!(saved.review, first.review);
    assert_eq!(saved.comparison, first.comparison);
    assert_eq!(saved.batch.write, first.batch.write);
    let shared = store.workspace_view(&second.id).unwrap().pr.unwrap();
    assert_eq!(shared.title, "New PR");
    assert_eq!(shared.revision, 1);
    let error = store
        .mutate_workspace(
            &second.id,
            second.generation,
            Action::PrDetails {
                revision: 0,
                title: "Stale overwrite".into(),
                body: "".into(),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("PR details changed"));
    assert_eq!(
        store.workspace(&second.id).unwrap().unwrap().generation,
        second.generation
    );
    assert_eq!(store.pull_request(&object.id).unwrap().unwrap(), shared);
    assert_eq!(
        store.workspace_log(&first.id).unwrap().last().unwrap()["action"]["action"],
        "pr_details"
    );
}

#[test]
fn description_failure_commits_only_successful_action_prefix() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store.create_or_use_intent(&branches(), "", "").unwrap();
    let workspace = local_session(&mut store, &object);
    let result = store
        .mutate_workspace_actions(
            &workspace.id,
            workspace.generation,
            vec![
                Action::PrDetails {
                    revision: 0,
                    title: "Title".into(),
                    body: "".into(),
                },
                save("Kept prefix"),
                Action::PrDetails {
                    revision: 0,
                    title: "Lost suffix".into(),
                    body: "".into(),
                },
                save("Must not be saved"),
            ],
        )
        .unwrap();
    assert_eq!(result.applied, 2);
    assert_eq!(result.failure.unwrap().0, 2);
    assert_eq!(result.workspace.generation, 2);
    assert_eq!(result.workspace.pending().unwrap().operations.len(), 1);
    assert_eq!(
        store.pull_request(&object.id).unwrap().unwrap().title,
        "Title"
    );
}

#[test]
fn durable_creation_marker_freezes_description_until_reconciled() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Title", "Body")
        .unwrap();
    let workspace = local_session(&mut store, &object);
    let resolution = PullRequestResolution {
        branches: branches(),
        title: object.title.clone(),
        body: object.body.clone(),
        draft: true,
    };
    let marked = store
        .mark_pr_resolution(&object.id, object.revision, &resolution)
        .unwrap();
    assert_eq!(marked.revision, 1);
    drop(store);
    let mut store = Store::open(project.path()).unwrap();
    assert_eq!(store.pr_resolution(&object.id).unwrap(), Some(resolution));
    let error = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::PrDetails {
                revision: marked.revision,
                title: "Changed".into(),
                body: "".into(),
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("in progress or uncertain"));
    assert!(
        store
            .clear_pr_resolution(&object.id, object.revision)
            .is_err()
    );
    let cleared = store
        .clear_pr_resolution(&object.id, marked.revision)
        .unwrap();
    assert_eq!(cleared.revision, 2);
    assert!(store.pr_resolution(&object.id).unwrap().is_none());
    store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::PrDetails {
                revision: cleared.revision,
                title: "Changed".into(),
                body: "".into(),
            },
        )
        .unwrap();
}

#[test]
fn bind_preserves_ids_pins_and_drafts_for_every_referencing_session() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Title", "Body")
        .unwrap();
    let first = local_session(&mut store, &object);
    let first = store
        .mutate_workspace(&first.id, first.generation, save("Retain me"))
        .unwrap();
    let second = local_session(&mut store, &object);
    let remote_id = PullRequestId("remote-deterministic-id".into());
    let backend = backend("https://github.com/team/project/pull/7", &remote_id);
    let bound = store
        .bind_pull_request(&object.id, object.revision, &backend, None, None)
        .unwrap();
    assert_eq!(bound.id, object.id);
    assert_eq!(
        bound.source,
        PullRequestSource::Remote {
            locator: backend.source.locator.clone(),
            branches: Some(branches())
        }
    );
    for before in [&first, &second] {
        let loaded = store.workspace(&before.id).unwrap().unwrap();
        assert_eq!(loaded.id, before.id);
        assert_eq!(loaded.generation, before.generation);
        assert_eq!(loaded.review, before.review);
        assert_eq!(loaded.batch, before.batch);
        assert_eq!(loaded.comparison, before.comparison);
        assert_eq!(
            loaded.locator.as_deref(),
            Some(backend.source.locator.as_str())
        );
        assert_eq!(
            store.workspace_view(&loaded.id).unwrap().pr,
            Some(bound.clone())
        );
        let mappings = store
            .review_backend(&loaded.review.id)
            .unwrap()
            .unwrap()
            .mappings;
        assert!(
            mappings
                .iter()
                .any(|mapping| mapping.resource == "pull_request"
                    && mapping.core_id == object.id.0
                    && mapping.remote_id == "123")
        );
    }
    let refreshed = ReviewId("remote-snapshot".into());
    store
        .import_review_with_branches(
            &refreshed,
            &reviewer(),
            &read(&remote_id, PullRequestStatus::Open),
            &backend,
            Some(&branches()),
        )
        .unwrap();
    assert_eq!(
        store
            .get_review(&refreshed)
            .unwrap()
            .unwrap()
            .read
            .pull_request
            .id,
        object.id
    );
    let latest = store
        .refresh_workspace(&first.id, first.generation, &refreshed)
        .unwrap();
    assert_eq!(
        latest
            .pending()
            .unwrap()
            .operations
            .iter()
            .map(|operation| &operation.change)
            .collect::<Vec<_>>(),
        first
            .pending()
            .unwrap()
            .operations
            .iter()
            .map(|operation| &operation.change)
            .collect::<Vec<_>>()
    );
    assert_eq!(latest.comparison, first.comparison);
    assert_eq!(latest.batch.id, first.batch.id);
}

#[test]
fn remote_import_adopts_unbound_intent_and_canonicalizes_provider_identity() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Local title", "")
        .unwrap();
    let local = local_session(&mut store, &object);
    let remote_id = PullRequestId("provider-id".into());
    let remote = backend("https://github.com/team/project/pull/7", &remote_id);
    let id = ReviewId("fresh".into());
    store
        .import_review_with_branches(
            &id,
            &reviewer(),
            &read(&remote_id, PullRequestStatus::Open),
            &remote,
            Some(&branches()),
        )
        .unwrap();
    let imported = store.get_review(&id).unwrap().unwrap();
    assert_eq!(imported.read.pull_request.id, object.id);
    let object = store.pull_request(&object.id).unwrap().unwrap();
    assert_eq!(object.title, "Snapshot title");
    assert_eq!(store.workspace_view(&local.id).unwrap().pr, Some(object));
    assert_eq!(
        store.review_backend(&id).unwrap().unwrap().mappings[0].core_id,
        imported.read.pull_request.id.0
    );
    assert!(store.pull_request(&remote_id).unwrap().is_none());
}

#[test]
fn canonical_collision_preserves_both_objects_marker_and_local_drafts() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let remote_id = PullRequestId("cached-pr".into());
    let remote = backend("https://github.com/team/project/pull/7", &remote_id);
    store
        .import_review_with_branches(
            &ReviewId("closed-pr".into()),
            &reviewer(),
            &read(&remote_id, PullRequestStatus::Closed),
            &remote,
            Some(&branches()),
        )
        .unwrap();
    let intent = store
        .create_or_use_intent(&branches(), "New PR", "Body")
        .unwrap();
    let workspace = local_session(&mut store, &intent);
    let workspace = store
        .mutate_workspace(&workspace.id, workspace.generation, save("Keep draft"))
        .unwrap();
    let marker = PullRequestResolution {
        branches: branches(),
        title: intent.title.clone(),
        body: intent.body.clone(),
        draft: true,
    };
    let intent = store
        .mark_pr_resolution(&intent.id, intent.revision, &marker)
        .unwrap();
    let error = store
        .bind_pull_request(
            &intent.id,
            intent.revision,
            &remote,
            Some(&branches()),
            None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("another local PR object"));
    assert_eq!(
        store.pull_request(&intent.id).unwrap(),
        Some(intent.clone())
    );
    assert!(store.pull_request(&remote_id).unwrap().is_some());
    assert_eq!(store.pr_resolution(&intent.id).unwrap(), Some(marker));
    assert_eq!(store.workspace(&workspace.id).unwrap().unwrap(), workspace);
    assert!(
        store
            .review_backend(&workspace.review.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn binding_rejects_wrong_pair_and_cannot_redirect_an_existing_resource() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Title", "Body")
        .unwrap();
    let workspace = local_session(&mut store, &object);
    let remote = backend("https://github.com/team/project/pull/7", &object.id);
    let mut wrong = branches();
    wrong.sink.branch = "another-base".into();
    assert!(
        store
            .bind_pull_request(&object.id, object.revision, &remote, Some(&wrong), None)
            .is_err()
    );
    assert_eq!(
        store.pull_request(&object.id).unwrap(),
        Some(object.clone())
    );
    assert!(
        store
            .review_backend(&workspace.review.id)
            .unwrap()
            .is_none()
    );
    let bound = store
        .bind_pull_request(
            &object.id,
            object.revision,
            &remote,
            Some(&branches()),
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .cached_workspace(&remote.source.locator)
            .unwrap()
            .unwrap()
            .id,
        workspace.id
    );
    let other = backend("https://github.com/team/project/pull/8", &object.id);
    assert!(
        store
            .bind_pull_request(&object.id, bound.revision, &other, Some(&branches()), None)
            .is_err()
    );
    assert_eq!(store.pull_request(&object.id).unwrap(), Some(bound.clone()));
    let error = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            Action::PrDetails {
                revision: bound.revision,
                title: "Must not edit GitHub title".into(),
                body: "".into(),
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("remote PR details cannot be edited locally")
    );
    assert_eq!(store.pull_request(&object.id).unwrap(), Some(bound));
}

#[test]
fn failed_import_rolls_back_canonical_metadata_and_binding() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Local", "")
        .unwrap();
    let local = local_session(&mut store, &object);
    let remote_id = PullRequestId("remote".into());
    let remote = backend("https://github.com/team/project/pull/7", &remote_id);
    assert!(
        store
            .import_review_with_branches(
                &local.review.id,
                &reviewer(),
                &read(&remote_id, PullRequestStatus::Open),
                &remote,
                Some(&branches())
            )
            .is_err()
    );
    assert_eq!(store.pull_request(&object.id).unwrap(), Some(object));
    assert!(store.review_backend(&local.review.id).unwrap().is_none());
    assert_eq!(store.workspace(&local.id).unwrap().unwrap(), local);
}

#[test]
fn existing_provider_state_backfills_without_changing_session_or_history() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let pull = PullRequestId("original-logical-id".into());
    let snapshot = ReviewId("original-snapshot".into());
    store
        .import_review(
            &snapshot,
            &reviewer(),
            &read(&pull, PullRequestStatus::Open),
            &backend("https://github.com/team/project/pull/7", &pull),
        )
        .unwrap();
    let workspace = store.create_batch_workspace(&snapshot).unwrap();
    let workspace = store
        .mutate_workspace(
            &workspace.id,
            workspace.generation,
            save("Historical draft"),
        )
        .unwrap();
    let log = store.workspace_log(&workspace.id).unwrap();
    drop(store);
    let old = rusqlite::Connection::open(project.path().join(".review/state.sqlite3")).unwrap();
    old.execute_batch("DROP TABLE pr_resolutions; DROP TABLE pull_requests;")
        .unwrap();
    drop(old);
    let store = Store::open(project.path()).unwrap();
    assert_eq!(store.workspace(&workspace.id).unwrap().unwrap(), workspace);
    assert_eq!(store.workspace_log(&workspace.id).unwrap(), log);
    let object = store.pull_request(&pull).unwrap().unwrap();
    assert_eq!(object.id, pull);
    assert_eq!(object.revision, 0);
    assert_eq!(object.title, "Snapshot title");
    assert!(matches!(
        object.source,
        PullRequestSource::Remote { branches: None, .. }
    ));
}

#[test]
fn bound_pr_can_complete_each_saved_reviewers_mapping_without_changing_identity() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Title", "Body")
        .unwrap();
    let first = local_session(&mut store, &object);
    let second_id = ReviewId("second-reviewer".into());
    let second_user = User {
        id: UserId("second-user".into()),
        name: "Other reviewer".into(),
    };
    store
        .create_review(
            &second_id,
            &second_user,
            &read(&object.id, PullRequestStatus::Local),
        )
        .unwrap();
    let second = store.create_batch_workspace(&second_id).unwrap();
    assert!(
        store
            .map_review_reviewer(&second_id, &second_user.id, "22")
            .is_err()
    );
    let mut remote = backend("https://github.com/team/project/pull/7", &object.id);
    remote.mappings.push(IdMapping {
        resource: "user".into(),
        core_id: reviewer().id.0,
        remote_id: "11".into(),
    });
    let bound = store
        .bind_pull_request(&object.id, object.revision, &remote, None, None)
        .unwrap();
    let before = store.workspace(&second.id).unwrap().unwrap();
    assert!(
        store
            .map_review_reviewer(&second_id, &reviewer().id, "11")
            .is_err()
    );
    store
        .map_review_reviewer(&second_id, &second_user.id, "22")
        .unwrap();
    store
        .map_review_reviewer(&second_id, &second_user.id, "22")
        .unwrap();
    assert!(
        store
            .map_review_reviewer(&second_id, &second_user.id, "23")
            .is_err()
    );
    let mapped = store.review_backend(&second_id).unwrap().unwrap();
    assert!(
        mapped
            .mappings
            .iter()
            .any(|mapping| mapping.resource == "user"
                && mapping.core_id == second_user.id.0
                && mapping.remote_id == "22")
    );
    assert!(
        !store
            .review_backend(&first.review.id)
            .unwrap()
            .unwrap()
            .mappings
            .iter()
            .any(|mapping| mapping.core_id == second_user.id.0)
    );
    assert_eq!(store.workspace(&second.id).unwrap().unwrap(), before);
    assert_eq!(store.pull_request(&object.id).unwrap(), Some(bound));
}

#[test]
fn baseline_created_after_binding_gets_provider_metadata_and_actual_remote_description() {
    let project = TempDir::new().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let object = store
        .create_or_use_intent(&branches(), "Local title", "Local body")
        .unwrap();
    local_session(&mut store, &object);
    let remote = backend("https://github.com/team/project/pull/7", &object.id);
    let bound = store
        .bind_pull_request(
            &object.id,
            object.revision,
            &remote,
            Some(&branches()),
            Some(("Existing GitHub title", "Existing GitHub body")),
        )
        .unwrap();
    let late = local_session(&mut store, &object);
    assert_eq!(
        late.review.read.pull_request.status,
        PullRequestStatus::Local
    );
    assert_eq!(late.review.read.pull_request.title, "Snapshot title");
    assert_eq!(
        late.locator.as_deref(),
        Some(remote.source.locator.as_str())
    );
    assert_eq!(store.review_backend(&late.review.id).unwrap(), Some(remote));
    let actual = store.workspace_view(&late.id).unwrap().pr.unwrap();
    assert_eq!(actual, bound);
    assert_eq!(actual.title, "Existing GitHub title");
    assert_eq!(actual.body, "Existing GitHub body");
}
