use buf_review::{
    core::ProjectedComment,
    model::*,
    store::{IdMapping, OperationResult, ReviewBackend, ReviewSource, Store},
    workspace::*,
};
use std::num::NonZeroU32;
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
            id: PullRequestId("pr".into()),
            title: "Example".into(),
            body: "".into(),
            author: None,
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
        comments: vec![ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("remote".into()),
                body: "original".into(),
            },
            author: Some(user()),
            target: CommentTarget::Diff(ReadDiffLocation {
                original: ReadCommentAnchor {
                    revision: ReadCommentRevision::Commit {
                        head: "head".into(),
                    },
                    position: position(2),
                },
                current: None,
                subject: Some(CommentSubject::Line),
                diff_hunk: None,
                outdated: Some(false),
            }),
        }],
        reactions: vec![],
        reviews: vec![],
    }
}
fn position(line: u32) -> CommentPosition {
    CommentPosition {
        path: "file.rs".into(),
        range: Some(DiffRange {
            start: DiffLine {
                side: DiffSide::Head,
                line: NonZeroU32::new(line).unwrap(),
            },
            end: DiffLine {
                side: DiffSide::Head,
                line: NonZeroU32::new(line).unwrap(),
            },
        }),
    }
}
fn location() -> DiffLocation {
    DiffLocation {
        revision: baseline().pull_request.revision,
        position: position(3),
    }
}
fn create(store: &mut Store, id: &str, read: ReadState) -> ReviewId {
    let id = ReviewId(id.into());
    store.create_review(&id, &user(), &read).unwrap();
    id
}
fn save(target: CommentTarget, destination: Option<Destination>) -> Action {
    Action::Save {
        comment: None,
        target: Some(target),
        body: "draft".into(),
        destination,
    }
}
fn apply(store: &mut Store, w: &mut Workspace, action: Action) {
    *w = store.mutate_workspace(&w.id, w.generation, action).unwrap();
}

#[test]
fn same_location_threads_keep_chronology_through_receipts_refresh_and_restart() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let root = |id: &str| ReadComment {
        draft: false,
        content: CommentContent {
            id: CommentId(id.into()),
            body: id.into(),
        },
        author: Some(user()),
        target: CommentTarget::Diff(ReadDiffLocation {
            original: ReadCommentAnchor {
                revision: ReadCommentRevision::Commit {
                    head: "head".into(),
                },
                position: position(3),
            },
            current: None,
            subject: Some(CommentSubject::Line),
            diff_hunk: None,
            outdated: Some(false),
        }),
    };
    let reply = |id: &str, parent: &str| ReadComment {
        draft: false,
        target: CommentTarget::Reply(CommentId(parent.into())),
        ..root(id)
    };
    let fetched_ids = ["z-oldest", "y-old-reply", "a-newer-root", "b-newer-reply"];
    let fetched = vec![
        root(fetched_ids[0]),
        reply(fetched_ids[1], fetched_ids[0]),
        root(fetched_ids[2]),
        reply(fetched_ids[3], fetched_ids[0]),
    ];
    let metadata = |core_id: &str, remote: usize, day: usize| {
        vec![
            IdMapping {
                resource: "review_comment".into(),
                core_id: core_id.into(),
                remote_id: remote.to_string(),
            },
            IdMapping {
                resource: format!("created_at:2026-01-{day:02}T00:00:00Z"),
                core_id: core_id.into(),
                remote_id: remote.to_string(),
            },
        ]
    };
    let mut read = baseline();
    read.comments = fetched.iter().rev().cloned().collect();
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: fetched_ids
            .iter()
            .enumerate()
            .flat_map(|(i, id)| metadata(id, 100 + i, i + 1))
            .collect(),
    };
    let snapshot = ReviewId("ordering-before".into());
    store
        .import_review(&snapshot, &user(), &read, &backend)
        .unwrap();
    let mut w = store.open_workspace(&snapshot).unwrap();
    let creates = [
        (
            "z-local-reply",
            CommentTarget::Reply(CommentId(fetched_ids[0].into())),
        ),
        ("y-local-root", CommentTarget::Diff(location())),
        (
            "x-local-reply",
            CommentTarget::Reply(CommentId("y-local-root".into())),
        ),
        ("w-local-root", CommentTarget::Diff(location())),
        (
            "v-local-reply",
            CommentTarget::Reply(CommentId("y-local-root".into())),
        ),
        (
            "u-local-reply",
            CommentTarget::Reply(CommentId(fetched_ids[0].into())),
        ),
        (
            "t-local-reply",
            CommentTarget::Reply(CommentId("w-local-root".into())),
        ),
    ];
    w = store
        .update_workspace(&w.id, w.generation, |w| {
            for (id, target) in &creates {
                let id = CommentId((*id).into());
                w.batch
                    .write
                    .events
                    .push(RecordedEvent::new(Event::CreateComment {
                        comment: DraftComment {
                            content: CommentContent {
                                id: id.clone(),
                                body: id.0.clone(),
                            },
                            author: user(),
                            target: target.clone(),
                        },
                    }));
                w.destinations.insert(id, Destination::Standalone);
            }
            Ok(())
        })
        .unwrap();
    let expected = [
        "z-oldest",
        "y-old-reply",
        "b-newer-reply",
        "z-local-reply",
        "u-local-reply",
        "a-newer-root",
        "y-local-root",
        "x-local-reply",
        "v-local-reply",
        "w-local-root",
        "t-local-reply",
    ];
    let ids = |w: &Workspace| {
        w.view()
            .unwrap()
            .comments
            .iter()
            .map(|c| c.comment.content().id.0.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&w), expected);
    assert_eq!(w.view().unwrap().todo.len(), fetched.len());
    assert!(w.publication_plan().is_err());
    apply(
        &mut store,
        &mut w,
        Action::MoveToReview {
            comments: creates
                .iter()
                .map(|(id, _)| CommentId((*id).into()))
                .collect(),
        },
    );
    let plan = w.publication_plan().unwrap();
    assert_eq!(
        plan.operations
            .iter()
            .map(|op| match &op.change {
                Event::CreateComment { comment } => comment.content.id.0.as_str(),
                other => panic!("unexpected operation: {other:?}"),
            })
            .collect::<Vec<_>>(),
        creates.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations.clone(),
        },
    );
    for (i, operation) in plan.operations.iter().enumerate().take(3) {
        apply(
            &mut store,
            &mut w,
            Action::RecordResult {
                result: OperationResult {
                    operation: operation.clone(),
                    mappings: vec![metadata(creates[i].0, 300 + i, 5 + i)[0].clone()],
                    response: serde_json::json!({"id":300+i}),
                },
            },
        );
    }
    assert_eq!(w.pending().unwrap().operations, plan.operations[3..]);
    assert_eq!(ids(&w), expected);
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    drop(store);
    let mut store = Store::open(project.path()).unwrap();
    w = store.workspace(&w.id).unwrap().unwrap();
    assert_eq!(w.attempt.as_ref().unwrap().operations, plan.operations);
    assert_eq!(ids(&w), expected);
    for (i, operation) in plan.operations.iter().enumerate().skip(3) {
        apply(
            &mut store,
            &mut w,
            Action::RecordResult {
                result: OperationResult {
                    operation: operation.clone(),
                    mappings: vec![metadata(creates[i].0, 300 + i, 5 + i)[0].clone()],
                    response: serde_json::json!({"id":300+i}),
                },
            },
        );
    }
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert_eq!(ids(&w), expected);
    let mut latest = baseline();
    latest.comments = fetched;
    let mut latest_backend = backend;
    for (i, (id, target)) in creates.iter().enumerate() {
        let alias = format!("fetched-{id}");
        let target = match target {
            CommentTarget::Reply(parent) if creates.iter().any(|(id, _)| *id == parent.0) => {
                CommentTarget::Reply(CommentId(format!("fetched-{}", parent.0)))
            }
            CommentTarget::Reply(parent) => CommentTarget::Reply(parent.clone()),
            CommentTarget::Diff(_) => root(&alias).target,
            CommentTarget::PullRequest => CommentTarget::PullRequest,
        };
        latest.comments.push(ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId(alias.clone()),
                body: (*id).into(),
            },
            author: Some(user()),
            target,
        });
        latest_backend
            .mappings
            .extend(metadata(&alias, 300 + i, 5 + i));
    }
    latest.comments.reverse();
    latest_backend.mappings.reverse();
    let next = ReviewId("ordering-after".into());
    store
        .import_review(&next, &user(), &latest, &latest_backend)
        .unwrap();
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert_eq!(ids(&w), expected);
    assert!(w.view().unwrap().comments.iter().all(|c| c.fetched));
    assert_eq!(w.view().unwrap().todo.len(), expected.len());
    assert_eq!(store.get_review(&w.review.id).unwrap().unwrap(), w.review);
    assert!(
        matches!(w.pending().unwrap().operations.as_slice(), [op] if matches!(op.change, Event::SetFileViewed { viewed:true, .. }))
    );
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let restarted = store.workspace(&w.id).unwrap().unwrap();
    assert_eq!(ids(&restarted), expected);
    assert_eq!(restarted.results, w.results);
    assert_eq!(restarted.pending().unwrap(), w.pending().unwrap());
}

#[test]
fn refresh_preserves_history_but_drops_confirmed_comment_missing_from_complete_snapshot() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![],
    };
    let before = ReviewId("mixed-before".into());
    store
        .import_review(&before, &user(), &baseline(), &backend)
        .unwrap();
    let mut w = store.open_workspace(&before).unwrap();
    apply(&mut store, &mut w, Action::Start);
    let expected = ["z-review-first", "a-standalone-later"];
    w = store
        .update_workspace(&w.id, w.generation, |w| {
            for (id, destination) in [
                (expected[0], Destination::Review),
                (expected[1], Destination::Standalone),
            ] {
                let id = CommentId(id.into());
                w.batch
                    .write
                    .events
                    .push(RecordedEvent::new(Event::CreateComment {
                        comment: DraftComment {
                            content: CommentContent {
                                id: id.clone(),
                                body: id.0.clone(),
                            },
                            author: user(),
                            target: CommentTarget::Diff(location()),
                        },
                    }));
                w.destinations.insert(id, destination);
            }
            Ok(())
        })
        .unwrap();
    assert!(w.publication_plan().is_err());
    let plan = w.pending().unwrap();
    assert_eq!(plan.operations.len(), 2);
    // Preserve a receipt produced by the former destination-selective runner.
    store
        .record_operation_result(
            &w.batch.id,
            &plan.operations[1],
            &[IdMapping {
                resource: "review_comment".into(),
                core_id: expected[1].into(),
                remote_id: "300".into(),
            }],
            &serde_json::json!({"id":300}),
        )
        .unwrap();
    w = store.workspace(&w.id).unwrap().unwrap();
    let after = ReviewId("mixed-after".into());
    store
        .import_review(&after, &user(), &baseline(), &backend)
        .unwrap();
    w = store
        .refresh_workspace(&w.id, w.generation, &after)
        .unwrap();
    let creations = |w: &Workspace| {
        w.batch
            .write
            .events
            .iter()
            .filter_map(|event| match &event.event {
                Event::CreateComment { comment } => Some(comment.content.id.0.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(creations(&w), expected);
    assert_eq!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .map(|c| c.comment.content().id.0.as_str())
            .collect::<Vec<_>>(),
        ["remote", expected[0]]
    );
    assert!(
        matches!(w.pending().unwrap().operations.as_slice(), [op] if matches!(&op.change, Event::CreateComment {comment} if comment.content.id.0==expected[0]))
    );
    assert!(w.wip.as_ref().unwrap().active);
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let restarted = store.workspace(&w.id).unwrap().unwrap();
    assert_eq!(creations(&restarted), expected);
    assert_eq!(restarted, w);
}

#[test]
fn restart_generations_and_noop_saves_preserve_local_identity() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let comment = w.saved_comment.clone().unwrap();
    let generation = w.generation;
    let count = w.batch.write.events.len();
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(comment.clone()),
            target: None,
            body: "draft".into(),
            destination: None,
        },
    );
    assert_eq!(w.batch.write.events.len(), count);
    assert_eq!(w.generation, generation);
    assert!(store.mutate_workspace(&w.id, 0, Action::Start).is_err());
    drop(store);
    let store = Store::open(project.path()).unwrap();
    let saved = store.last_active_workspace().unwrap().unwrap();
    assert_eq!(saved, w);
    assert_eq!(store.workspace_view(&w.id).unwrap().comments.len(), 2);
}

#[test]
fn blank_new_comment_is_noop_and_blank_saved_draft_cancels_without_relocation() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    let before = w.clone();
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: None,
            target: None,
            body: " \n\t".into(),
            destination: None,
        },
    );
    assert_eq!(w, before);
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let comment = w.saved_comment.clone().unwrap();
    let mut invalid = location();
    invalid.position.path = "missing".into();
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(comment.clone()),
            target: Some(CommentTarget::Diff(invalid)),
            body: " \n\t".into(),
            destination: None,
        },
    );
    assert!(w.saved_comment.is_none());
    assert!(w.pending().unwrap().operations.is_empty());
    assert!(
        !w.view()
            .unwrap()
            .comments
            .iter()
            .any(|row| row.comment.content().id == comment)
    );
    assert!(
        matches!(&w.batch.write.events.iter().map(|record| record.event.clone()).collect::<Vec<_>>()[..],[Event::CreateComment{comment:draft},Event::DeleteComment{comment:deleted}] if draft.content.id==comment && draft.target==CommentTarget::Diff(location()) && *deleted==comment)
    );
    let deleted = w.clone();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(comment),
                    target: None,
                    body: String::new(),
                    destination: None
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), deleted);
    drop(store);
    assert_eq!(
        Store::open(project.path())
            .unwrap()
            .workspace(&w.id)
            .unwrap()
            .unwrap(),
        deleted
    );
}

#[test]
fn blank_fetched_comment_is_immutable_even_without_a_stale_generation() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    let stale = w.generation;
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let before = w.clone();
    let blank = Action::Save {
        comment: Some(CommentId("remote".into())),
        target: None,
        body: " \t\n".into(),
        destination: None,
    };
    assert!(matches!(
        store.mutate_workspace(&w.id, stale, blank.clone()),
        Err(buf_review::store::StoreError::Workspace(
            WorkspaceError::StaleGeneration { .. }
        ))
    ));
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
    assert!(store.mutate_workspace(&w.id, w.generation, blank).is_err());
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
}

#[test]
fn blank_save_reuses_delete_permissions_dependencies_conflicts_and_freezes() {
    let base = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    let remote = CommentId("remote".into());
    let mut cases = vec![];
    for author in [
        None,
        Some(User {
            id: UserId("other".into()),
            name: "Other".into(),
        }),
    ] {
        let mut w = base.clone();
        w.review.read.comments[0].author = author;
        cases.push((w, remote.clone()));
    }
    let mut summary = base.clone();
    summary.summary_comments.insert(remote.clone());
    cases.push((summary, remote.clone()));
    let mut conflict = base.clone();
    let previous = conflict.review.read.comments[0].clone();
    conflict.conflicts.insert(
        remote.clone(),
        Conflict {
            comment: remote.clone(),
            previous: previous.clone(),
            remote: Some(previous),
            kind: ConflictKind::Changed,
        },
    );
    cases.push((conflict, remote.clone()));
    let mut dependent = base.clone();
    dependent
        .apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let parent = dependent.saved_comment.clone().unwrap();
    dependent
        .apply(save(CommentTarget::Reply(parent.clone()), None))
        .unwrap();
    cases.push((dependent, parent));
    let mut frozen = base.clone();
    frozen
        .apply(save(
            CommentTarget::Diff(location()),
            Some(Destination::Review),
        ))
        .unwrap();
    let draft = frozen.saved_comment.clone().unwrap();
    frozen
        .apply(Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        })
        .unwrap();
    let operations = frozen.publication_plan().unwrap().operations;
    frozen
        .apply(Action::BeginPublication {
            destination: Destination::Review,
            operations,
        })
        .unwrap();
    cases.push((frozen, draft));
    for (before, comment) in cases {
        let mut blank = before.clone();
        let mut explicit = before.clone();
        let deletion = explicit
            .apply(Action::Delete {
                comment: comment.clone(),
            })
            .unwrap_err()
            .to_string();
        let save = blank
            .apply(Action::Save {
                comment: Some(comment),
                target: None,
                body: "\n \t".into(),
                destination: None,
            })
            .unwrap_err()
            .to_string();
        assert_eq!(save, deletion);
        assert_eq!(blank, before);
        assert_eq!(explicit, before);
    }
}

#[test]
fn nonblank_save_restores_only_unsent_local_deletion_atomically() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let snapshot = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&snapshot).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let comment = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Delete {
            comment: comment.clone(),
        },
    );
    let deleted = w.clone();
    let deleted_batch = store.get_batch(&w.batch.id).unwrap().unwrap();
    let mut invalid = location();
    invalid.position.path = "absent".into();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(comment.clone()),
                    target: Some(CommentTarget::Diff(invalid)),
                    body: "must not persist".into(),
                    destination: None,
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), deleted);
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(comment.clone()),
            target: None,
            body: "restored text".into(),
            destination: None,
        },
    );
    assert_eq!(w.saved_comment, Some(comment.clone()));
    let view = w.view().unwrap();
    let row = view
        .comments
        .iter()
        .find(|row| row.comment.content().id == comment)
        .unwrap();
    assert_eq!(row.comment.content().body, "restored text");
    assert_eq!(row.destination, Some(Destination::Review));
    assert!(
        matches!(&w.pending().unwrap().operations[..], [operation] if matches!(&operation.change,Event::CreateComment { comment:draft } if draft.content.id==comment && draft.content.body=="restored text"))
    );
    assert_eq!(
        store
            .batch_revision(&deleted_batch.id, deleted_batch.revision.number)
            .unwrap()
            .unwrap(),
        deleted_batch
    );
    drop(store);
    assert_eq!(
        Store::open(project.path())
            .unwrap()
            .workspace(&w.id)
            .unwrap()
            .unwrap(),
        w
    );
}

#[test]
fn restore_canceled_local_draft_preserves_original_body_and_destination() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let snapshot = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&snapshot).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let comment = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Delete {
            comment: comment.clone(),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Restore {
            comment: comment.clone(),
        },
    );
    let view = w.view().unwrap();
    let row = view
        .comments
        .iter()
        .find(|row| row.comment.content().id == comment)
        .unwrap();
    assert_eq!(row.comment.content().body, "draft");
    assert_eq!(row.destination, Some(Destination::Review));
    assert_eq!(w.pending().unwrap().operations.len(), 1);
}

#[test]
fn destination_defaults_and_parent_dependencies_do_not_change_with_activity() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let root = w.saved_comment.clone().unwrap();
    assert!(!w.wip.as_ref().unwrap().active);
    apply(&mut store, &mut w, Action::Start);
    apply(&mut store, &mut w, Action::Leave);
    let before = w.clone();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                save(
                    CommentTarget::Reply(root.clone()),
                    Some(Destination::Standalone)
                )
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Reply(root.clone()), None),
    );
    let reply = w.saved_comment.clone().unwrap();
    assert_eq!(w.destinations[&reply], Destination::Review);
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Delete {
                    comment: root.clone()
                }
            )
            .is_err()
    );
    apply(&mut store, &mut w, Action::Delete { comment: reply });
    apply(&mut store, &mut w, Action::Delete { comment: root });
    assert!(w.pending().unwrap().operations.is_empty());
}

#[test]
fn checklist_only_fetched_individual_reactions_and_file_progress_independent() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let mut read = baseline();
    read.reactions.push(CommentReaction {
        comment: CommentId("remote".into()),
        author: Some(UserId("other".into())),
        reaction: "👍".into(),
    });
    let id = create(&mut store, "snapshot", read);
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Reply(CommentId("remote".into())), None),
    );
    let draft = w.saved_comment.clone().unwrap();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Reaction {
                    comment: draft,
                    present: true
                }
            )
            .is_err()
    );
    assert_eq!(w.view().unwrap().todo, vec![CommentId("remote".into())]);
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    assert_eq!(w.view().unwrap().todo.len(), 1);
    apply(
        &mut store,
        &mut w,
        Action::Reaction {
            comment: CommentId("remote".into()),
            present: true,
        },
    );
    let view = w.view().unwrap();
    assert!(view.todo.is_empty());
    assert_eq!(view.done.len(), 1);
    assert!(view.files[0].viewed);
    apply(
        &mut store,
        &mut w,
        Action::Reaction {
            comment: CommentId("remote".into()),
            present: false,
        },
    );
    assert_eq!(w.view().unwrap().todo.len(), 1);
}

#[test]
fn contextual_undo_resolves_current_state_and_never_revives_canceled_drafts() {
    let mut workspace = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    let remote = CommentId("remote".into());
    workspace
        .apply(Action::Reaction {
            comment: remote.clone(),
            present: true,
        })
        .unwrap();
    workspace
        .apply(Action::Undo {
            comments: vec![remote.clone()],
        })
        .unwrap();
    assert_eq!(workspace.view().unwrap().todo, vec![remote]);

    workspace
        .apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let draft = workspace.saved_comment.clone().unwrap();
    workspace
        .apply(Action::Undo {
            comments: vec![draft.clone()],
        })
        .unwrap();
    let canceled = workspace.clone();
    workspace
        .apply(Action::Undo {
            comments: vec![draft.clone()],
        })
        .unwrap();
    workspace.apply(Action::Delete { comment: draft }).unwrap();
    assert_eq!(workspace, canceled);
    assert!(
        workspace
            .view()
            .unwrap()
            .comments
            .iter()
            .all(|row| row.fetched)
    );
    let missing = workspace.apply(Action::Undo {
        comments: vec![CommentId("unknown".into())],
    });
    assert!(missing.is_err());
    assert_eq!(workspace, canceled);
}

#[test]
fn contextual_undo_deletes_selected_drafts_child_first_and_preflights_dependencies() {
    let mut workspace = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    workspace
        .apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let root = workspace.saved_comment.clone().unwrap();
    workspace
        .apply(save(CommentTarget::Reply(root.clone()), None))
        .unwrap();
    let child = workspace.saved_comment.clone().unwrap();
    let before = workspace.clone();
    assert!(
        workspace
            .apply(Action::Undo {
                comments: vec![root.clone()]
            })
            .is_err()
    );
    assert_eq!(workspace, before);
    workspace
        .apply(Action::DeleteComments {
            comments: vec![root, child.clone(), child],
        })
        .unwrap();
    assert!(workspace.pending().unwrap().operations.is_empty());
    let deleted = workspace
        .batch
        .write
        .events
        .iter()
        .filter_map(|event| match &event.event {
            Event::DeleteComment { comment } => Some(comment),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(deleted.len(), 2);
    assert_eq!(*deleted[0], before.saved_comment.unwrap());
}

#[test]
fn grouped_deletions_resolve_prior_queued_undo_and_preflight_the_whole_selection() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let mut read = baseline();
    let mut foreign = read.comments[0].clone();
    foreign.content.id = CommentId("foreign".into());
    foreign.author = Some(User {
        id: UserId("other".into()),
        name: "Other".into(),
    });
    read.comments.push(foreign);
    let id = create(&mut store, "snapshot", read);
    let mut workspace = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut workspace,
        save(CommentTarget::Diff(location()), None),
    );
    let root = workspace.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut workspace,
        save(CommentTarget::Reply(root.clone()), None),
    );
    let child = workspace.saved_comment.clone().unwrap();
    for selection in [
        vec![root.clone()],
        vec![child.clone(), CommentId("foreign".into())],
        vec![child.clone(), CommentId("missing".into())],
    ] {
        let failed = store
            .mutate_workspace_actions(
                &workspace.id,
                workspace.generation,
                vec![Action::DeleteComments {
                    comments: selection,
                }],
            )
            .unwrap();
        assert_eq!(failed.applied, 0);
        assert!(failed.failure.is_some());
        assert_eq!(failed.workspace, workspace);
    }
    let canceled = store
        .mutate_workspace_actions(
            &workspace.id,
            workspace.generation,
            vec![
                Action::Undo {
                    comments: vec![child],
                },
                Action::DeleteComments {
                    comments: vec![root.clone(), root.clone()],
                },
                Action::Delete {
                    comment: CommentId("remote".into()),
                },
                Action::Undo {
                    comments: vec![CommentId("remote".into())],
                },
                Action::DeleteComments {
                    comments: vec![CommentId("remote".into())],
                },
            ],
        )
        .unwrap();
    assert_eq!(canceled.applied, 2);
    assert_eq!(canceled.failure.as_ref().map(|(index, _)| *index), Some(2));
    let rows = canceled.workspace.view().unwrap().comments;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        store.workspace_log(&workspace.id).unwrap().last().unwrap()["action"],
        serde_json::json!({"action":"delete_comments","comments":[root]})
    );
}

#[test]
fn contextual_undo_preserves_all_members_of_a_frozen_review() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "snapshot", baseline());
    let mut workspace = store.open_workspace(&id).unwrap();
    let mut comments = Vec::new();
    for destination in [
        Destination::Standalone,
        Destination::Review,
        Destination::Standalone,
    ] {
        apply(
            &mut store,
            &mut workspace,
            save(CommentTarget::Diff(location()), Some(destination)),
        );
        comments.push(workspace.saved_comment.clone().unwrap());
    }
    apply(
        &mut store,
        &mut workspace,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    let operations = workspace.publication_plan().unwrap().operations;
    apply(
        &mut store,
        &mut workspace,
        Action::BeginPublication {
            destination: Destination::Review,
            operations,
        },
    );
    let generation = workspace.generation;
    let result = store
        .mutate_workspace_actions(
            &workspace.id,
            generation,
            vec![Action::Undo {
                comments: comments.clone(),
            }],
        )
        .unwrap();
    assert_eq!(result.applied, 0);
    assert_eq!(result.failure.as_ref().map(|(index, _)| *index), Some(0));
    assert_eq!(result.workspace.generation, generation);
    let remaining = result
        .workspace
        .view()
        .unwrap()
        .comments
        .into_iter()
        .map(|row| row.comment.content().id.clone())
        .collect::<Vec<_>>();
    assert!(remaining.contains(&comments[0]));
    assert!(remaining.contains(&comments[1]));
    assert!(remaining.contains(&comments[2]));
    assert_eq!(result.workspace, workspace);
    let persisted = store.workspace(&workspace.id).unwrap().unwrap();
    assert_eq!(persisted, result.workspace);
    assert!(persisted.results.is_empty());
    let canceled = store
        .mutate_workspace_actions(
            &workspace.id,
            persisted.generation,
            vec![Action::DeleteComments {
                comments: vec![comments[2].clone(), comments[1].clone()],
            }],
        )
        .unwrap();
    assert_eq!(canceled.applied, 0);
    assert_eq!(canceled.workspace.generation, persisted.generation);
    assert!(canceled.failure.is_some());
    assert_eq!(canceled.workspace, persisted);
}

#[test]
fn refresh_exposes_latest_branch_source_without_repinning_the_comparison() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let mut original = baseline();
    original.pull_request.source = Some(BranchSource {
        repository: "https://github.com/original/repo".into(),
        branch: "old-feature".into(),
    });
    let id = create(&mut store, "old", original);
    let mut workspace = store.open_workspace(&id).unwrap();
    let mut latest = baseline();
    let source = BranchSource {
        repository: "https://github.com/contributor/repo".into(),
        branch: "feature/review".into(),
    };
    latest.pull_request.source = Some(source.clone());
    latest.pull_request.revision.head = "next".into();
    let next = create(&mut store, "new", latest);
    workspace = store
        .refresh_workspace(&workspace.id, workspace.generation, &next)
        .unwrap();
    let view = workspace.view().unwrap();
    assert_eq!(view.comparison.head, "head");
    assert_eq!(view.review.read.pull_request.source, Some(source.clone()));
    let reopened = store.workspace(&workspace.id).unwrap().unwrap();
    assert_eq!(
        reopened.view().unwrap().review.read.pull_request.source,
        Some(source)
    );
    assert_eq!(
        store
            .get_review(&id)
            .unwrap()
            .unwrap()
            .read
            .pull_request
            .source
            .unwrap()
            .branch,
        "old-feature"
    );
}

#[test]
fn file_stats_follow_the_pinned_comparison_across_refresh_and_reopen() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let mut original = baseline();
    let pinned_stats = FileStats {
        additions: 12,
        deletions: 3,
    };
    original.files[0].stats = Some(pinned_stats.clone());
    let id = create(&mut store, "old", original);
    let mut workspace = store.open_workspace(&id).unwrap();
    let mut latest = baseline();
    let latest_stats = FileStats {
        additions: 99,
        deletions: 40,
    };
    latest.pull_request.revision.head = "next".into();
    latest.files[0].stats = Some(latest_stats.clone());
    latest.files[0].viewed = true;
    let next = create(&mut store, "new", latest);
    workspace = store
        .refresh_workspace(&workspace.id, workspace.generation, &next)
        .unwrap();
    assert_eq!(
        workspace.view().unwrap().files[0].stats,
        Some(pinned_stats.clone())
    );
    assert!(workspace.view().unwrap().files[0].viewed);
    workspace = store.workspace(&workspace.id).unwrap().unwrap();
    assert_eq!(workspace.view().unwrap().files[0].stats, Some(pinned_stats));
    apply(&mut store, &mut workspace, Action::CompareLatest);
    assert_eq!(workspace.view().unwrap().files[0].stats, Some(latest_stats));
}

#[test]
fn publication_selection_freezes_wip_and_receipts_do_not_create_todo() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let review_draft = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        save(
            CommentTarget::Diff(location()),
            Some(Destination::Standalone),
        ),
    );
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    assert_eq!(plan.operations.len(), 4);
    assert!(matches!(
        plan.operations.last().unwrap().change,
        Event::SetReviewConclusion { .. }
    ));
    assert_eq!(w.publication_plan().unwrap().operations.len(), 4);
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations.clone(),
        },
    );
    assert!(
        store
            .mutate_workspace(&w.id, w.generation, Action::Leave)
            .is_err()
    );
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                save(CommentTarget::Diff(location()), Some(Destination::Review))
            )
            .is_err()
    );
    for op in plan.operations {
        apply(
            &mut store,
            &mut w,
            Action::RecordResult {
                result: OperationResult {
                    operation: op,
                    mappings: vec![],
                    response: serde_json::Value::Null,
                },
            },
        );
    }
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert!(w.wip.is_none());
    assert_eq!(w.view().unwrap().todo.len(), 1);
    assert!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .any(|c| matches!(&c.comment,ProjectedComment::Draft(d) if d.content.id==review_draft))
    );
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    assert!(
        w.publication_plan()
            .unwrap()
            .operations
            .iter()
            .any(|o| matches!(o.change, Event::SetReviewConclusion { .. }))
    );
}

#[test]
fn provider_id_mapping_merges_new_fetched_identity_with_local_receipt() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![],
    };
    let id = ReviewId("old".into());
    store
        .import_review(&id, &user(), &baseline(), &backend)
        .unwrap();
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let local = w.saved_comment.clone().unwrap();
    let plan = w.publication_plan().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations.clone(),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: plan.operations[0].clone(),
                mappings: vec![IdMapping {
                    resource: "review_comment".into(),
                    core_id: local.0.clone(),
                    remote_id: "99".into(),
                }],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    let mut fresh = baseline();
    let mut fetched = fresh.comments[0].clone();
    fetched.content = CommentContent {
        id: CommentId("new-fetched-id".into()),
        body: "draft".into(),
    };
    fresh.comments.push(fetched);
    let fresh_backend = ReviewBackend {
        mappings: vec![IdMapping {
            resource: "review_comment".into(),
            core_id: "new-fetched-id".into(),
            remote_id: "99".into(),
        }],
        ..backend
    };
    let next = ReviewId("fresh".into());
    store
        .import_review(&next, &user(), &fresh, &fresh_backend)
        .unwrap();
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert_eq!(w.view().unwrap().comments.len(), 2);
    assert!(w.view().unwrap().todo.contains(&local));
    assert!(w.pending().unwrap().operations.is_empty());
    assert_eq!(store.get_review(&w.review.id).unwrap().unwrap(), w.review);
    let persisted = store.get_batch(&w.batch.id).unwrap().unwrap();
    assert!(
        buf_review::core::project(
            &store.get_review(&persisted.review).unwrap().unwrap(),
            &persisted
        )
        .is_ok()
    );
    assert_eq!(
        store.get_review(&next).unwrap().unwrap().read.comments[1]
            .content
            .id
            .0,
        "new-fetched-id"
    );
}

#[test]
fn confirmed_failure_imports_pending_comments_and_preserves_preparation_receipts() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![],
    };
    let id = ReviewId("old".into());
    store
        .import_review(&id, &user(), &baseline(), &backend)
        .unwrap();
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let local = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    let plan = w.publication_plan().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations.clone(),
        },
    );
    let prepared = OperationResult {
        operation: plan.operations[0].clone(),
        mappings: vec![IdMapping {
            resource: "review_comment".into(),
            core_id: local.0.clone(),
            remote_id: "99".into(),
        }],
        response: serde_json::Value::Null,
    };
    let prepared_operation = prepared.operation.clone();
    let prepared_receipt = prepared.clone();
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"pending":{"id":12,"node_id":"node","head":"head"},"prepared":[prepared],"submitted":false}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: false,
            error: Some("rejected".into()),
        },
    );
    assert!(w.attempt.is_none());
    assert!(w.preparation.get("pending").is_some());
    let mut fresh = baseline();
    let mut mirror = fresh.comments[0].clone();
    mirror.content = CommentContent {
        id: CommentId("mirror".into()),
        body: "draft".into(),
    };
    mirror.draft = true;
    fresh.comments.push(mirror);
    let next = ReviewId("fresh".into());
    let backend = ReviewBackend {
        mappings: vec![
            IdMapping {
                resource: "review_comment".into(),
                core_id: "mirror".into(),
                remote_id: "99".into(),
            },
            IdMapping {
                resource: "review_member:12".into(),
                core_id: "mirror".into(),
                remote_id: "99".into(),
            },
        ],
        ..backend
    };
    store
        .import_review(&next, &user(), &fresh, &backend)
        .unwrap();
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert_eq!(w.view().unwrap().todo.len(), 1);
    assert_eq!(w.view().unwrap().comments.len(), 2);
    assert!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .any(|c| c.fetched && c.draft && c.comment.content().id == local)
    );
    // A suspended unknown submission uses the same private-membership rule.
    let attempt_id = w.suspended_attempts[0].id.clone();
    apply(&mut store, &mut w, Action::ResumePublication { attempt_id });
    assert!(
        w.attempt
            .as_ref()
            .unwrap()
            .operations
            .contains(&prepared_operation)
    );
    // Reconciliation records prepared evidence after the same comment was fetched.
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: prepared_receipt,
        },
    );
    assert!(w.has_result(&prepared_operation.id));
    assert!(w.pending().is_ok());
    let before = w.clone();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(local.clone()),
                    target: None,
                    body: "must remain immutable".into(),
                    destination: None,
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("submission response lost".into()),
        },
    );
    let frozen = w.suspended_attempts[0].clone();
    let latest = ReviewId("fresh-while-uncertain".into());
    store
        .import_review(&latest, &user(), &fresh, &backend)
        .unwrap();
    w = store
        .refresh_workspace(&w.id, w.generation, &latest)
        .unwrap();
    assert_eq!(w.suspended_attempts[0], frozen);
    assert_eq!(w.view().unwrap().todo.len(), 1);
    assert_eq!(w.view().unwrap().comments.len(), 2);
    assert!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .any(|row| row.fetched && row.draft && row.comment.content().id == local)
    );
}

#[test]
fn complete_fetch_absorbs_receipt_and_drops_confirmed_comment_missing_remotely() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let local = w.saved_comment.clone().unwrap();
    let plan = w.publication_plan().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations.clone(),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: plan.operations[0].clone(),
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    let next = create(&mut store, "fresh", baseline());
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert!(
        !w.view()
            .unwrap()
            .comments
            .iter()
            .any(|c| c.comment.content().id == local)
    );
    assert_eq!(w.view().unwrap().todo.len(), 1);
    assert!(w.pending().unwrap().operations.is_empty());
}

#[test]
fn publication_lock_is_repository_wide_and_failed_start_does_not_persist() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let first = create(&mut store, "first", baseline());
    let mut one = store.open_workspace(&first).unwrap();
    apply(&mut store, &mut one, save(CommentTarget::PullRequest, None));
    let plan = one.publication_plan().unwrap();
    apply(
        &mut store,
        &mut one,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
    let mut second = baseline();
    second.pull_request.id = PullRequestId("second-pr".into());
    let second = create(&mut store, "second", second);
    let mut two = store.open_workspace(&second).unwrap();
    apply(&mut store, &mut two, save(CommentTarget::PullRequest, None));
    let plan = two.publication_plan().unwrap();
    assert!(
        store
            .mutate_workspace(
                &two.id,
                two.generation,
                Action::BeginPublication {
                    destination: Destination::Standalone,
                    operations: plan.operations
                }
            )
            .is_err()
    );
    assert!(store.workspace(&two.id).unwrap().unwrap().attempt.is_none());
    apply(
        &mut store,
        &mut one,
        Action::FinishPublication {
            success: false,
            uncertain: false,
            error: Some("confirmed failure".into()),
        },
    );
    let plan = two.publication_plan().unwrap();
    apply(
        &mut store,
        &mut two,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
}

#[test]
fn rejected_anchor_blocks_its_review_and_dependents_until_explicit_relocation() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let root = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Reply(root.clone()), None),
    );
    apply(&mut store, &mut w, save(CommentTarget::PullRequest, None));
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RejectAnchor {
            comment: root.clone(),
            message: "line no longer reviewable".into(),
        },
    );
    assert!(w.publication_plan().is_err());
    assert_eq!(w.pending().unwrap().operations.len(), 4);
    assert!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .any(|c| c.comment.content().id == root && c.anchor_error.is_some())
    );
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(root.clone()),
            target: None,
            body: "body edited".into(),
            destination: None,
        },
    );
    assert!(w.publication_plan().is_err());
    let mut moved = location();
    moved.position = position(4);
    apply(
        &mut store,
        &mut w,
        Action::Relocate {
            comment: root,
            location: moved,
        },
    );
    assert!(w.publication_plan().is_ok());
}

#[test]
fn uncertain_review_blocks_skip_ahead_publication_until_reconciliation() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let root = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    let plan = w.publication_plan().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    let uncertain_id = w.attempt.as_ref().unwrap().id.clone();
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("lost response".into()),
        },
    );
    assert!(w.attempt.is_none());
    assert_eq!(w.suspended_attempts.len(), 1);
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(root),
                    target: None,
                    body: "changed frozen".into(),
                    destination: None
                }
            )
            .is_err()
    );
    assert!(
        store
            .mutate_workspace(&w.id, w.generation, Action::Leave)
            .is_err()
    );
    apply(&mut store, &mut w, save(CommentTarget::PullRequest, None));
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let before = w.clone();
    assert!(w.publication_plan().is_err());
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::BeginPublication {
                    destination: Destination::Standalone,
                    operations: w.pending().unwrap().operations,
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
    assert_eq!(w.suspended_attempts[0].id, uncertain_id);
    apply(
        &mut store,
        &mut w,
        Action::ResumePublication {
            attempt_id: uncertain_id,
        },
    );
    assert!(w.attempt.is_some());
    assert!(w.suspended_attempts.is_empty());
}

#[test]
fn uncertain_progress_preserves_new_intent_but_blocks_same_target_until_reconciled() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("unknown viewed result".into()),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: false,
        },
    );
    assert!(!w.view().unwrap().files[0].viewed);
    assert!(w.publication_plan().unwrap().operations.is_empty());
    assert_eq!(w.suspended_attempts.len(), 1);
}

#[test]
fn save_relocation_is_atomic_and_preserves_body_identity_and_reply_targets() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let root = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Reply(root.clone()), None),
    );
    let reply = w.saved_comment.clone().unwrap();
    let mut moved = location();
    moved.position = position(8);
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(root.clone()),
            target: Some(CommentTarget::Diff(moved.clone())),
            body: "relocated text".into(),
            destination: None,
        },
    );
    let view = w.view().unwrap();
    assert!(view.comments.iter().any(|c|matches!(&c.comment,ProjectedComment::Draft(d) if d.content.id==root&&d.content.body=="relocated text"&&d.target==CommentTarget::Diff(moved.clone()))));
    assert!(view.comments.iter().any(|c|matches!(&c.comment,ProjectedComment::Draft(d) if d.content.id==reply&&d.target==CommentTarget::Reply(root.clone()))));
    let before = w.clone();
    let mut invalid = moved;
    invalid.position.path = "absent".into();
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(root),
                    target: Some(CommentTarget::Diff(invalid)),
                    body: "must not persist".into(),
                    destination: None
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace(&w.id).unwrap().unwrap(), before);
}

#[test]
fn process_publication_lock_releases_with_its_file_handle() {
    let project = tempdir().unwrap();
    let first = Store::open(project.path()).unwrap();
    let second = Store::open(project.path()).unwrap();
    let lock = first.publication_lock().unwrap();
    assert!(second.publication_lock().is_err());
    drop(lock);
    assert!(second.publication_lock().is_ok());
}

#[test]
fn uncertain_comment_retains_the_whole_ordered_attempt_and_blocks_later_progress() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    let create_id = plan.operations[0].id.clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"inflight":format!("create comment {create_id}")}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("lost root response".into()),
        },
    );
    assert!(
        w.suspended_attempts[0]
            .operations
            .iter()
            .any(|op| matches!(op.change, Event::SetFileViewed { .. }))
    );
    assert_eq!(w.suspended_attempts[0].operations.len(), 3);
    assert!(w.publication_plan().is_err());
    assert_eq!(w.pending().unwrap().operations.len(), 3);
}

#[test]
fn reconciling_old_submitted_progress_preserves_new_wip_summary() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    let conclusion = plan
        .operations
        .iter()
        .find(|op| matches!(op.change, Event::SetReviewConclusion { .. }))
        .unwrap()
        .clone();
    let progress = plan
        .operations
        .iter()
        .find(|op| matches!(op.change, Event::SetFileViewed { .. }))
        .unwrap()
        .clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: conclusion.clone(),
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"inflight":format!("operation {}",progress.id),"submitted":true}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("lost progress response".into()),
        },
    );
    let suspended = w.suspended_attempts[0].id.clone();
    assert!(w.wip.is_none());
    apply(&mut store, &mut w, Action::Start);
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Comment,
                body: Some("new summary".into()),
            },
        },
    );
    let fresh_id = w.wip.as_ref().unwrap().id.clone();
    apply(
        &mut store,
        &mut w,
        Action::ResumePublication {
            attempt_id: suspended,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: conclusion,
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: progress,
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert_eq!(w.wip.as_ref().unwrap().id, fresh_id);
    assert!(w.publication_plan().unwrap().operations.iter().any(|op|matches!(&op.change,Event::SetReviewConclusion{conclusion} if conclusion.body.as_deref()==Some("new summary"))));
}

#[test]
fn summary_permissions_and_chronological_unlocated_order_use_provider_metadata() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let mut read = baseline();
    read.comments = vec![
        ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("summary".into()),
                body: "own summary".into(),
            },
            author: Some(user()),
            target: CommentTarget::PullRequest,
        },
        ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("unknown".into()),
                body: "unknown author".into(),
            },
            author: None,
            target: CommentTarget::PullRequest,
        },
    ];
    let backend = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![
            IdMapping {
                resource: "review_summary".into(),
                core_id: "summary".into(),
                remote_id: "12".into(),
            },
            IdMapping {
                resource: "created_at:2026-01-02T00:00:00Z".into(),
                core_id: "summary".into(),
                remote_id: "12".into(),
            },
            IdMapping {
                resource: "created_at:2026-01-01T00:00:00Z".into(),
                core_id: "unknown".into(),
                remote_id: "11".into(),
            },
        ],
    };
    let id = ReviewId("snapshot".into());
    store.import_review(&id, &user(), &read, &backend).unwrap();
    let w = store.open_workspace(&id).unwrap();
    let view = store.workspace_view(&w.id).unwrap();
    assert_eq!(
        view.todo,
        vec![CommentId("unknown".into()), CommentId("summary".into())]
    );
    assert!(
        view.comments
            .iter()
            .any(|c| c.summary && c.comment.content().id.0 == "summary")
    );
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Delete {
                    comment: CommentId("summary".into())
                }
            )
            .is_err()
    );
    assert!(
        store
            .mutate_workspace(
                &w.id,
                w.generation,
                Action::Save {
                    comment: Some(CommentId("unknown".into())),
                    body: "edit".into(),
                    target: None,
                    destination: None
                }
            )
            .is_err()
    );
}

#[test]
fn refresh_preserves_uncertain_progress_and_later_opposite_intent() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Reaction {
            comment: CommentId("remote".into()),
            present: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    let viewed = plan
        .operations
        .iter()
        .find(|op| matches!(op.change, Event::SetFileViewed { .. }))
        .unwrap()
        .clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"inflight":format!("operation {}",viewed.id)}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("viewed response lost".into()),
        },
    );
    let suspended = w.suspended_attempts[0].clone();
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: false,
        },
    );
    let mut fresh = baseline();
    fresh.files[0].viewed = true;
    let next = create(&mut store, "fresh", fresh);
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert_eq!(w.suspended_attempts[0], suspended);
    assert!(!w.view().unwrap().files[0].viewed);
    assert_eq!(w.view().unwrap().done.len(), 1);
    assert!(w.last_error.is_some());
    assert!(w.publication_plan().is_err());
    apply(
        &mut store,
        &mut w,
        Action::ResumePublication {
            attempt_id: suspended.id,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: viewed,
                mappings: vec![],
                response: serde_json::json!({"reconciled":true}),
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert!(
        w.publication_plan()
            .unwrap()
            .operations
            .iter()
            .any(|op| matches!(op.change, Event::SetFileViewed { viewed: false, .. }))
    );
}

#[test]
fn suspended_refresh_keeps_durable_receipts_without_replaying_fetched_creations() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let local = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let plan = w.publication_plan().unwrap();
    let created = plan.operations[0].clone();
    let conclusion = plan.operations[1].clone();
    let progress = plan.operations[2].clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    for operation in [created.clone(), conclusion.clone()] {
        apply(
            &mut store,
            &mut w,
            Action::RecordResult {
                result: OperationResult {
                    operation,
                    mappings: vec![],
                    response: serde_json::Value::Null,
                },
            },
        );
    }
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"inflight":format!("operation {}",progress.id),"submitted":true}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("progress result lost".into()),
        },
    );
    let suspended = w.suspended_attempts[0].id.clone();
    let mut fresh = baseline();
    let mut comment = fresh.comments[0].clone();
    comment.content.id = local.clone();
    comment.content.body = "draft".into();
    fresh.comments.push(comment);
    let next = create(&mut store, "fresh", fresh);
    w = store.refresh_workspace(&w.id, w.generation, &next).unwrap();
    assert!(w.has_result(&created.id));
    assert_eq!(w.view().unwrap().todo.len(), 2);
    apply(
        &mut store,
        &mut w,
        Action::ResumePublication {
            attempt_id: suspended,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: created,
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    assert!(w.pending().is_ok());
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: progress,
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert!(w.pending().unwrap().operations.is_empty());
}

#[test]
fn publication_receipt_merges_after_unrelated_save_without_overwriting_new_work() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let id = create(&mut store, "old", baseline());
    let mut w = store.open_workspace(&id).unwrap();
    apply(&mut store, &mut w, save(CommentTarget::PullRequest, None));
    let plan = w.publication_plan().unwrap();
    let original = plan.operations[0].clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Standalone,
            operations: plan.operations,
        },
    );
    let attempt = w.attempt.as_ref().unwrap().id.clone();
    let stale = w.clone();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), Some(Destination::Review)),
    );
    let concurrent = w.saved_comment.clone().unwrap();
    assert!(w.generation > stale.generation);
    w = store
        .mutate_publication(
            &w.id,
            &attempt,
            Action::RecordResult {
                result: OperationResult {
                    operation: original.clone(),
                    mappings: vec![],
                    response: serde_json::Value::Null,
                },
            },
        )
        .unwrap();
    assert!(w.has_result(&original.id));
    assert!(
        w.view()
            .unwrap()
            .comments
            .iter()
            .any(|row| row.comment.content().id == concurrent)
    );
    assert!(
        store
            .mutate_publication(
                &w.id,
                "wrong-attempt",
                Action::RefreshError {
                    error: "must not persist".into()
                }
            )
            .is_err()
    );
    assert!(
        store
            .workspace(&w.id)
            .unwrap()
            .unwrap()
            .last_error
            .is_none()
    );
}

#[test]
fn submitted_review_refresh_keeps_fetched_resource_identity_separate_from_receipt_operation() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let source = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![],
    };
    let original = ReviewId("before-submission".into());
    store
        .import_review(&original, &user(), &baseline(), &source)
        .unwrap();
    let mut w = store.open_workspace(&original).unwrap();
    let conclusion = ReviewConclusion {
        outcome: ReviewOutcome::Approve,
        body: Some("review summary".into()),
    };
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: conclusion.clone(),
        },
    );
    let plan = w.publication_plan().unwrap();
    let operation = plan.operations[0].clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    let receipt_mapping = IdMapping {
        resource: "review".into(),
        core_id: operation.id.clone(),
        remote_id: "100".into(),
    };
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: operation.clone(),
                mappings: vec![receipt_mapping.clone()],
                response: serde_json::json!({"id":100,"state":"APPROVED"}),
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    let receipt_snapshot = w.review.id.clone();
    let mut fetched = baseline();
    fetched.reviews.push(ReadReview {
        id: Some(ReviewResourceId("fetched-review".into())),
        author: Some(user()),
        state: ReadReviewState::Submitted(conclusion),
    });
    let fetched_backend = ReviewBackend {
        mappings: vec![IdMapping {
            resource: "review".into(),
            core_id: "fetched-review".into(),
            remote_id: "100".into(),
        }],
        ..source
    };
    let incoming = ReviewId("after-submission".into());
    store
        .import_review(&incoming, &user(), &fetched, &fetched_backend)
        .unwrap();
    w = store
        .refresh_workspace(&w.id, w.generation, &incoming)
        .unwrap();
    assert_eq!(store.get_review(&w.review.id).unwrap().unwrap(), w.review);
    let current = store.review_backend(&w.review.id).unwrap().unwrap();
    assert!(
        current.mappings.iter().any(|m| m.resource == "review"
            && m.core_id == "fetched-review"
            && m.remote_id == "100")
    );
    assert!(!current.mappings.contains(&receipt_mapping));
    assert!(
        store
            .review_backend(&receipt_snapshot)
            .unwrap()
            .unwrap()
            .mappings
            .contains(&receipt_mapping)
    );
    assert_eq!(w.results[0].mappings, vec![receipt_mapping]);
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    let again = ReviewId("second-refresh".into());
    store
        .import_review(&again, &user(), &fetched, &fetched_backend)
        .unwrap();
    w = store
        .refresh_workspace(&w.id, w.generation, &again)
        .unwrap();
    assert_eq!(
        w.review.read.reviews[0].id,
        Some(ReviewResourceId("fetched-review".into()))
    );
    assert!(w.view().unwrap().files[0].viewed);
}

#[test]
fn uncertain_submitted_review_can_record_conclusion_receipt_after_fetch_without_mapping_collision()
{
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let source = ReviewBackend {
        source: ReviewSource {
            backend: "github".into(),
            locator: "https://github.com/a/b/pull/1".into(),
        },
        mappings: vec![],
    };
    let original = ReviewId("before-submission".into());
    store
        .import_review(&original, &user(), &baseline(), &source)
        .unwrap();
    let mut w = store.open_workspace(&original).unwrap();
    let conclusion = ReviewConclusion {
        outcome: ReviewOutcome::Approve,
        body: Some("review summary".into()),
    };
    apply(
        &mut store,
        &mut w,
        Action::Summary {
            conclusion: conclusion.clone(),
        },
    );
    let plan = w.publication_plan().unwrap();
    let operation = plan.operations[0].clone();
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: plan.operations,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: serde_json::json!({"pending":{"id":100,"node_id":"node"},"inflight":"submit","submitted":false}),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: false,
            uncertain: true,
            error: Some("submission response lost".into()),
        },
    );
    let suspended = w.suspended_attempts[0].id.clone();
    let mut fetched = baseline();
    fetched.reviews.push(ReadReview {
        id: Some(ReviewResourceId("fetched-review".into())),
        author: Some(user()),
        state: ReadReviewState::Submitted(conclusion),
    });
    let fetched_backend = ReviewBackend {
        mappings: vec![IdMapping {
            resource: "review".into(),
            core_id: "fetched-review".into(),
            remote_id: "100".into(),
        }],
        ..source
    };
    let incoming = ReviewId("after-submission".into());
    store
        .import_review(&incoming, &user(), &fetched, &fetched_backend)
        .unwrap();
    w = store
        .refresh_workspace(&w.id, w.generation, &incoming)
        .unwrap();
    apply(
        &mut store,
        &mut w,
        Action::ResumePublication {
            attempt_id: suspended,
        },
    );
    let receipt_mapping = IdMapping {
        resource: "review".into(),
        core_id: operation.id.clone(),
        remote_id: "100".into(),
    };
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: OperationResult {
                operation: operation.clone(),
                mappings: vec![receipt_mapping.clone()],
                response: serde_json::json!({"id":100,"state":"APPROVED"}),
            },
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert!(w.has_result(&operation.id));
    assert!(w.wip.is_none());
    assert!(w.pending().unwrap().operations.is_empty());
    let mappings = store
        .review_backend(&w.review.id)
        .unwrap()
        .unwrap()
        .mappings;
    assert_eq!(
        mappings
            .iter()
            .filter(|m| m.resource == "review" && m.remote_id == "100")
            .map(|m| m.core_id.as_str())
            .collect::<Vec<_>>(),
        vec!["fetched-review"]
    );
    assert_eq!(w.results[0].mappings, vec![receipt_mapping]);
    assert_eq!(store.get_review(&incoming).unwrap().unwrap().read, fetched);
}

#[test]
fn publication_plan_combines_destinations_in_event_order_without_skipping_blocked_intent() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(Action::Viewed {
        path: "file.rs".into(),
        viewed: true,
    })
    .unwrap();
    w.apply(save(CommentTarget::PullRequest, None)).unwrap();
    let standalone = w.saved_comment.clone().unwrap();
    w.apply(save(
        CommentTarget::Diff(location()),
        Some(Destination::Standalone),
    ))
    .unwrap();
    let inline = w.saved_comment.clone().unwrap();
    assert_eq!(w.destinations[&standalone], Destination::Standalone);
    assert_eq!(w.destinations[&inline], Destination::Review);
    w.apply(Action::PublishReview {
        conclusion: Some(ReviewConclusion {
            outcome: ReviewOutcome::Approve,
            body: None,
        }),
    })
    .unwrap();
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: true,
    })
    .unwrap();
    let plan = w.publication_plan().unwrap();
    assert!(matches!(
        &plan.operations[0].change,
        Event::SetFileViewed { .. }
    ));
    assert!(
        matches!(&plan.operations[1].change, Event::CreateComment { comment } if comment.content.id == standalone)
    );
    assert!(
        matches!(&plan.operations[2].change, Event::CreateComment { comment } if comment.content.id == inline)
    );
    assert!(matches!(
        &plan.operations[3].change,
        Event::SetCommentReaction { .. }
    ));
    assert!(matches!(
        &plan.operations[4].change,
        Event::SetReviewConclusion { .. }
    ));
    w.apply(Action::RejectAnchor {
        comment: inline,
        message: "invalid anchor".into(),
    })
    .unwrap();
    assert!(w.publication_plan().is_err());
}

#[test]
fn fetched_and_api_delivered_comments_are_immutable_but_progress_remains_local() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let local = w.saved_comment.clone().unwrap();
    let operations = w.publication_plan().unwrap().operations;
    w.apply(Action::BeginPublication {
        destination: Destination::Review,
        operations: operations.clone(),
    })
    .unwrap();
    w.apply(Action::RecordResult {
        result: OperationResult {
            operation: operations[0].clone(),
            mappings: vec![],
            response: serde_json::Value::Null,
        },
    })
    .unwrap();
    w.apply(Action::FinishPublication {
        success: true,
        uncertain: false,
        error: None,
    })
    .unwrap();
    for comment in [CommentId("remote".into()), local] {
        for action in [
            Action::Save {
                comment: Some(comment.clone()),
                target: None,
                body: "changed".into(),
                destination: None,
            },
            Action::Delete {
                comment: comment.clone(),
            },
            Action::Relocate {
                comment: comment.clone(),
                location: location(),
            },
        ] {
            let before = w.clone();
            assert!(w.apply(action).is_err());
            assert_eq!(w, before);
        }
    }
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: true,
    })
    .unwrap();
    assert_eq!(w.view().unwrap().done, [CommentId("remote".into())]);
}

#[test]
fn move_adopts_only_unsent_local_inline_families_atomically() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let root = w.saved_comment.clone().unwrap();
    w.apply(save(CommentTarget::Reply(root.clone()), None))
        .unwrap();
    let reply = w.saved_comment.clone().unwrap();
    // A persisted old standalone family stays unchanged until explicitly adopted.
    w.destinations.insert(root.clone(), Destination::Standalone);
    w.destinations
        .insert(reply.clone(), Destination::Standalone);
    let before = w.clone();
    assert!(w.publication_plan().is_err());
    assert!(
        w.apply(Action::MoveToReview {
            comments: vec![root.clone()]
        })
        .is_err()
    );
    assert_eq!(w, before);
    assert!(
        w.apply(Action::MoveToReview {
            comments: vec![reply.clone()]
        })
        .is_err()
    );
    assert_eq!(w, before);
    w.apply(Action::MoveToReview {
        comments: vec![root.clone(), reply.clone()],
    })
    .unwrap();
    assert_eq!(w.batch, before.batch);
    assert_eq!(w.destinations[&root], Destination::Review);
    assert_eq!(w.destinations[&reply], Destination::Review);
    assert_eq!(w.projected().unwrap(), before.projected().unwrap());
    assert!(w.publication_plan().is_ok());
    w.apply(save(CommentTarget::PullRequest, None)).unwrap();
    let general = w.saved_comment.clone().unwrap();
    let before = w.clone();
    for comment in [general, CommentId("remote".into())] {
        assert!(
            w.apply(Action::MoveToReview {
                comments: vec![comment]
            })
            .is_err()
        );
        assert_eq!(w, before);
    }
    assert!(matches!(
        serde_json::from_value::<Action>(serde_json::json!({"action":"move", "comments":[root]}))
            .unwrap(),
        Action::MoveToReview { .. }
    ));
}

#[test]
fn imported_pending_comments_remain_read_only_outside_the_checklist() {
    let mut read = baseline();
    read.comments[0].draft = true;
    let mut w = Workspace::new(Review {
        id: ReviewId("pending-snapshot".into()),
        reviewer: user(),
        read,
    });
    let view = w.view().unwrap();
    assert!(view.comments[0].fetched && view.comments[0].draft);
    assert!(view.todo.is_empty() && view.done.is_empty() && view.pending.is_empty());
    assert!(
        w.apply(Action::Reaction {
            comment: CommentId("remote".into()),
            present: true
        })
        .is_err()
    );
    assert!(
        w.apply(Action::Save {
            comment: Some(CommentId("remote".into())),
            target: None,
            body: "changed".into(),
            destination: None
        })
        .is_err()
    );
}

#[test]
fn failed_attempt_keeps_its_full_order_identity_and_successful_prefix() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(Action::Viewed {
        path: "file.rs".into(),
        viewed: true,
    })
    .unwrap();
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let draft = w.saved_comment.clone().unwrap();
    let operations = w.publication_plan().unwrap().operations;
    w.apply(Action::BeginPublication {
        destination: Destination::Review,
        operations: operations.clone(),
    })
    .unwrap();
    let attempt = w.attempt.as_ref().unwrap().id.clone();
    w.apply(Action::RecordResult {
        result: OperationResult {
            operation: operations[0].clone(),
            mappings: vec![],
            response: serde_json::Value::Null,
        },
    })
    .unwrap();
    w.apply(Action::FinishPublication {
        success: false,
        uncertain: false,
        error: Some("rejected comment".into()),
    })
    .unwrap();
    assert!(w.attempt.is_none());
    assert_eq!(w.suspended_attempts[0].id, attempt);
    assert_eq!(w.suspended_attempts[0].operations, operations);
    assert!(!w.suspended_attempts[0].uncertain);
    w.apply(Action::Save {
        comment: Some(draft),
        target: None,
        body: "fixed comment".into(),
        destination: None,
    })
    .unwrap();
    let retry = w.publication_plan().unwrap();
    assert_eq!(retry.operations.len(), 1);
    assert!(
        matches!(&retry.operations[0].change, Event::CreateComment { comment } if comment.content.body == "fixed comment")
    );
    w.apply(Action::ResumePublication {
        attempt_id: attempt.clone(),
    })
    .unwrap();
    assert_eq!(w.attempt.as_ref().unwrap().id, attempt);
    assert!(w.has_result(&operations[0].id));
}

#[test]
fn known_failure_retry_replans_only_original_unsent_targets() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(Action::Viewed {
        path: "file.rs".into(),
        viewed: true,
    })
    .unwrap();
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let draft = w.saved_comment.clone().unwrap();
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: true,
    })
    .unwrap();
    w.apply(Action::PublishReview {
        conclusion: Some(ReviewConclusion {
            outcome: ReviewOutcome::Approve,
            body: None,
        }),
    })
    .unwrap();
    let original = w.publication_plan().unwrap().operations;
    w.apply(Action::BeginPublication {
        destination: Destination::Review,
        operations: original.clone(),
    })
    .unwrap();
    let attempt_id = w.attempt.as_ref().unwrap().id.clone();
    w.apply(Action::RecordResult {
        result: OperationResult {
            operation: original[0].clone(),
            mappings: vec![],
            response: serde_json::Value::Null,
        },
    })
    .unwrap();
    w.apply(Action::FinishPublication {
        success: false,
        uncertain: false,
        error: Some("rejected".into()),
    })
    .unwrap();
    w.apply(Action::Save {
        comment: Some(draft.clone()),
        target: None,
        body: "corrected".into(),
        destination: None,
    })
    .unwrap();
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: false,
    })
    .unwrap();
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: true,
    })
    .unwrap();
    w.apply(Action::PublishReview {
        conclusion: Some(ReviewConclusion {
            outcome: ReviewOutcome::Comment,
            body: Some("corrected summary".into()),
        }),
    })
    .unwrap();
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    let unrelated = w.saved_comment.clone().unwrap();
    w.apply(Action::RejectAnchor {
        comment: unrelated.clone(),
        message: "unrelated invalid anchor".into(),
    })
    .unwrap();
    assert!(w.publication_plan().is_err());
    w.apply(Action::ResumePublication {
        attempt_id: attempt_id.clone(),
    })
    .unwrap();
    let resumed = w.attempt.as_ref().unwrap();
    assert_eq!(resumed.id, attempt_id);
    assert_eq!(resumed.operations.len(), 4);
    assert_eq!(resumed.operations[0], original[0]);
    assert!(
        matches!(&resumed.operations[1].change, Event::CreateComment { comment } if comment.content.id == draft && comment.content.body == "corrected")
    );
    assert!(matches!(
        &resumed.operations[2].change,
        Event::SetCommentReaction { present: true, .. }
    ));
    assert_ne!(resumed.operations[2].id, original[2].id);
    assert!(
        matches!(&resumed.operations[3].change, Event::SetReviewConclusion { conclusion } if conclusion.body.as_deref() == Some("corrected summary"))
    );
    assert_ne!(resumed.operations[3].id, original[3].id);
    assert!(
        !resumed
            .operations
            .iter()
            .any(|op| operation_comment(&op.change) == Some(&unrelated))
    );
}

#[test]
fn own_pending_api_comment_keeps_private_draft_status_after_success_and_restart() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let review = create(&mut store, "snapshot", baseline());
    let mut w = store.open_workspace(&review).unwrap();
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let comment = w.saved_comment.clone().unwrap();
    let operations = w.publication_plan().unwrap().operations;
    apply(
        &mut store,
        &mut w,
        Action::BeginPublication {
            destination: Destination::Review,
            operations: operations.clone(),
        },
    );
    let result = OperationResult {
        operation: operations[0].clone(),
        mappings: vec![],
        response: serde_json::json!({"id":100}),
    };
    apply(
        &mut store,
        &mut w,
        Action::RecordResult {
            result: result.clone(),
        },
    );
    let provider = serde_json::json!({"pending":{"id":99,"node_id":"pending99","head":"head"},"prepared":[result],"submitted":false});
    apply(
        &mut store,
        &mut w,
        Action::SetAttemptProvider {
            provider: provider.clone(),
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        },
    );
    assert_eq!(w.preparation, provider);
    let view = w.view().unwrap();
    let row = view
        .comments
        .iter()
        .find(|row| row.comment.content().id == comment)
        .unwrap();
    assert!(row.draft && !row.fetched && row.destination.is_none());
    assert!(!view.todo.contains(&comment) && !view.done.contains(&comment));
    assert!(
        w.apply(Action::Delete {
            comment: comment.clone()
        })
        .is_err()
    );
    drop(store);
    let store = Store::open(project.path()).unwrap();
    assert_eq!(
        serde_json::to_value(store.workspace(&w.id).unwrap().unwrap().view().unwrap()).unwrap(),
        serde_json::to_value(view).unwrap()
    );
}

#[test]
fn finished_conclusion_preserves_history_and_identical_next_review_gets_a_new_operation() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    let conclusion = ReviewConclusion {
        outcome: ReviewOutcome::Approve,
        body: None,
    };
    w.apply(Action::PublishReview {
        conclusion: Some(conclusion.clone()),
    })
    .unwrap();
    w.apply(Action::Viewed {
        path: "file.rs".into(),
        viewed: true,
    })
    .unwrap();
    let original_events = w.batch.write.events.clone();
    let operations = w.publication_plan().unwrap().operations;
    let sent_conclusion = operations.last().unwrap().clone();
    w.apply(Action::BeginPublication {
        destination: Destination::Review,
        operations: operations.clone(),
    })
    .unwrap();
    for operation in operations {
        w.apply(Action::RecordResult {
            result: OperationResult {
                operation,
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        })
        .unwrap();
    }
    w.apply(Action::FinishPublication {
        success: true,
        uncertain: false,
        error: None,
    })
    .unwrap();
    assert_eq!(w.batch.write.events, original_events);
    assert!(
        w.results
            .iter()
            .any(|result| result.operation == sent_conclusion)
    );
    assert!(w.publication_plan().unwrap().operations.is_empty());
    assert!(w.wip.is_none());
    w.apply(Action::Reaction {
        comment: CommentId("remote".into()),
        present: true,
    })
    .unwrap();
    let reaction = w.publication_plan().unwrap().operations[0].clone();
    w.apply(Action::PublishReview {
        conclusion: Some(conclusion),
    })
    .unwrap();
    let next = w.publication_plan().unwrap().operations;
    assert_eq!(next.len(), 2);
    assert_eq!(next[0], reaction);
    assert_eq!(next[1].change, sent_conclusion.change);
    assert_ne!(next[1].id, sent_conclusion.id);
    assert_eq!(
        w.batch.write.events[..original_events.len()],
        original_events
    );
}

#[test]
fn refresh_preserves_interleaving_of_surviving_local_operations() {
    let project = tempdir().unwrap();
    let mut store = Store::open(project.path()).unwrap();
    let snapshot = create(&mut store, "before", baseline());
    let mut w = store.open_workspace(&snapshot).unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Viewed {
            path: "file.rs".into(),
            viewed: true,
        },
    );
    apply(
        &mut store,
        &mut w,
        save(CommentTarget::Diff(location()), None),
    );
    let inline = w.saved_comment.clone().unwrap();
    apply(
        &mut store,
        &mut w,
        Action::Reaction {
            comment: CommentId("remote".into()),
            present: true,
        },
    );
    apply(&mut store, &mut w, save(CommentTarget::PullRequest, None));
    apply(
        &mut store,
        &mut w,
        Action::Save {
            comment: Some(inline),
            target: None,
            body: "final local body".into(),
            destination: None,
        },
    );
    apply(
        &mut store,
        &mut w,
        Action::PublishReview {
            conclusion: Some(ReviewConclusion {
                outcome: ReviewOutcome::Approve,
                body: None,
            }),
        },
    );
    let expected = w
        .publication_plan()
        .unwrap()
        .operations
        .into_iter()
        .map(|op| op.change)
        .collect::<Vec<_>>();
    let fresh = create(&mut store, "after", baseline());
    w = store
        .refresh_workspace(&w.id, w.generation, &fresh)
        .unwrap();
    assert_eq!(
        w.publication_plan()
            .unwrap()
            .operations
            .into_iter()
            .map(|op| op.change)
            .collect::<Vec<_>>(),
        expected
    );
    drop(store);
    let store = Store::open(project.path()).unwrap();
    assert_eq!(
        store
            .workspace(&w.id)
            .unwrap()
            .unwrap()
            .pending()
            .unwrap()
            .operations
            .into_iter()
            .map(|op| op.change)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn publish_review_can_edit_or_cancel_unsent_conclusion_without_erasing_history() {
    let mut w = Workspace::new(Review {
        id: ReviewId("snapshot".into()),
        reviewer: user(),
        read: baseline(),
    });
    w.apply(save(CommentTarget::Diff(location()), None))
        .unwrap();
    w.apply(Action::PublishReview {
        conclusion: Some(ReviewConclusion {
            outcome: ReviewOutcome::Approve,
            body: None,
        }),
    })
    .unwrap();
    let edited = ReviewConclusion {
        outcome: ReviewOutcome::Comment,
        body: Some("edited conclusion".into()),
    };
    w.apply(Action::PublishReview {
        conclusion: Some(edited.clone()),
    })
    .unwrap();
    let old_summary = w.publication_plan().unwrap().operations.pop().unwrap();
    assert!(
        matches!(&old_summary.change,Event::SetReviewConclusion { conclusion } if conclusion==&edited)
    );
    let history = w.batch.write.events.clone();
    let cancel = serde_json::from_value::<Action>(
        serde_json::json!({"action":"publish_review","conclusion":null}),
    )
    .unwrap();
    assert!(matches!(cancel, Action::PublishReview { conclusion: None }));
    w.apply(cancel.clone()).unwrap();
    assert_eq!(w.batch.write.events, history);
    assert!(w.wip.as_ref().unwrap().conclusion.is_none());
    assert!(
        matches!(&w.publication_plan().unwrap().operations[..],[op] if matches!(op.change,Event::CreateComment { .. }))
    );
    w.apply(Action::PublishReview {
        conclusion: Some(edited),
    })
    .unwrap();
    assert_eq!(w.batch.write.events[..history.len()], history);
    let operations = w.publication_plan().unwrap().operations;
    assert_ne!(operations.last().unwrap().id, old_summary.id);
    w.apply(Action::BeginPublication {
        destination: Destination::Review,
        operations,
    })
    .unwrap();
    let frozen = w.clone();
    assert!(w.apply(cancel.clone()).is_err());
    assert_eq!(w, frozen);
    w.apply(Action::FinishPublication {
        success: false,
        uncertain: false,
        error: Some("confirmed rejection".into()),
    })
    .unwrap();
    let attempt_id = w.suspended_attempts[0].id.clone();
    w.apply(cancel.clone()).unwrap();
    w.apply(Action::ResumePublication {
        attempt_id: attempt_id.clone(),
    })
    .unwrap();
    assert_eq!(w.attempt.as_ref().unwrap().id, attempt_id);
    assert!(
        w.attempt
            .as_ref()
            .unwrap()
            .operations
            .iter()
            .all(|op| !matches!(op.change, Event::SetReviewConclusion { .. }))
    );
    w.apply(Action::FinishPublication {
        success: false,
        uncertain: true,
        error: Some("unknown response".into()),
    })
    .unwrap();
    let uncertain = w.clone();
    assert!(w.apply(cancel).is_err());
    assert_eq!(w, uncertain);
}

#[test]
fn cancelled_viewed_and_reaction_intent_stays_cancelled_after_remote_changes() {
    let mut workspace = Workspace::new(Review {
        id: ReviewId("initial".into()),
        reviewer: user(),
        read: baseline(),
    });
    for viewed in [true, false] {
        workspace
            .apply(Action::Viewed {
                path: "file.rs".into(),
                viewed,
            })
            .unwrap();
    }
    for present in [true, false] {
        workspace
            .apply(Action::Reaction {
                comment: CommentId("remote".into()),
                present,
            })
            .unwrap();
    }
    assert!(workspace.pending().unwrap().operations.is_empty());
    assert!(workspace.batch.write.events[1].cancelled);
    assert!(workspace.batch.write.events[3].cancelled);
    let records = workspace.batch.write.events.clone();
    let mut fresh = baseline();
    fresh.files[0].viewed = true;
    fresh.reactions.push(CommentReaction {
        comment: CommentId("remote".into()),
        author: Some(user().id),
        reaction: "👍".into(),
    });
    workspace
        .refresh(Review {
            id: ReviewId("fresh".into()),
            reviewer: user(),
            read: fresh,
        })
        .unwrap();
    assert_eq!(workspace.batch.write.events, records);
    assert!(workspace.pending().unwrap().operations.is_empty());
    let projected = workspace.projected().unwrap();
    assert!(projected.files[0].viewed);
    assert!(
        projected
            .reactions
            .iter()
            .any(|item| item.comment.0 == "remote")
    );
    workspace
        .apply(Action::Viewed {
            path: "file.rs".into(),
            viewed: false,
        })
        .unwrap();
    assert!(!workspace.batch.write.events.last().unwrap().cancelled);
    assert_eq!(workspace.pending().unwrap().operations.len(), 1);
}

#[test]
fn refresh_does_not_treat_edits_folded_into_a_sent_creation_as_pending_conflicts() {
    let mut workspace = Workspace::new(Review {
        id: ReviewId("initial".into()),
        reviewer: user(),
        read: baseline(),
    });
    workspace
        .apply(save(CommentTarget::PullRequest, None))
        .unwrap();
    let local = workspace.saved_comment.clone().unwrap();
    workspace
        .apply(Action::Save {
            comment: Some(local.clone()),
            target: None,
            destination: None,
            body: "edited before publication".into(),
        })
        .unwrap();
    let operations = workspace.publication_plan().unwrap().operations;
    assert_eq!(operations.len(), 1);
    workspace
        .apply(Action::BeginPublication {
            destination: Destination::Standalone,
            operations: operations.clone(),
        })
        .unwrap();
    workspace
        .apply(Action::RecordResult {
            result: OperationResult {
                operation: operations[0].clone(),
                mappings: vec![],
                response: serde_json::Value::Null,
            },
        })
        .unwrap();
    workspace
        .apply(Action::FinishPublication {
            success: true,
            uncertain: false,
            error: None,
        })
        .unwrap();
    let records = workspace.batch.write.events.clone();
    let receipts = workspace.results.clone();
    for (generation, body) in ["edited before publication", "changed remotely"]
        .into_iter()
        .enumerate()
    {
        let mut read = baseline();
        read.comments.push(ReadComment {
            content: CommentContent {
                id: local.clone(),
                body: body.into(),
            },
            author: Some(user()),
            target: CommentTarget::PullRequest,
            draft: false,
        });
        workspace
            .refresh(Review {
                id: ReviewId(format!("fresh-{generation}")),
                reviewer: user(),
                read,
            })
            .unwrap();
        assert!(workspace.conflicts.is_empty());
        assert!(workspace.pending().unwrap().operations.is_empty());
        assert_eq!(workspace.batch.write.events, records);
        assert_eq!(workspace.results, receipts);
        assert!(
            workspace
                .projected()
                .unwrap()
                .comments
                .iter()
                .any(|comment| comment.content().id == local && comment.content().body == body)
        );
    }
}
