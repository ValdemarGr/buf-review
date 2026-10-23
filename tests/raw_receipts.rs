use buf_review::{model::*, store::Store, workspace::Workspace};
use serde_json::json;
use tempfile::tempdir;

fn user() -> User {
    User {
        id: UserId("reviewer".into()),
        name: "Reviewer".into(),
    }
}

fn read() -> ReadState {
    ReadState {
        pull_request: PullRequest {
            id: PullRequestId("pull".into()),
            title: "Raw receipt validation".into(),
            body: String::new(),
            author: Some(user()),
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

fn draft(id: &str, body: &str) -> Event {
    Event::CreateComment {
        comment: DraftComment {
            content: CommentContent {
                id: CommentId(id.into()),
                body: body.into(),
            },
            author: user(),
            target: CommentTarget::PullRequest,
        },
    }
}

fn refresh(store: &mut Store, workspace: Workspace, name: &str, read: &ReadState) -> Workspace {
    let id = ReviewId(name.into());
    store.create_review(&id, &user(), read).unwrap();
    store
        .refresh_workspace(&workspace.id, workspace.generation, &id)
        .unwrap()
}

#[test]
fn raw_append_consumes_confirmed_creation_and_folded_edits_across_fetches() {
    for state in [
        "before_fetch",
        "fetched_before_receipt",
        "fetched_after_receipt",
        "deleted",
    ] {
        let project = tempdir().unwrap();
        let mut store = Store::open(project.path()).unwrap();
        let baseline = read();
        let review = ReviewId("initial".into());
        store.create_review(&review, &user(), &baseline).unwrap();
        let mut workspace = store.open_workspace(&review).unwrap();
        store
            .append_event(&workspace.batch.id, &draft("original", "First draft"))
            .unwrap();
        store
            .append_event(
                &workspace.batch.id,
                &Event::EditComment {
                    comment: CommentId("original".into()),
                    body: "Published body".into(),
                },
            )
            .unwrap();
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        let operation = workspace.publication_plan().unwrap().operations.remove(0);
        assert!(
            matches!(&operation.change, Event::CreateComment { comment } if comment.content.body == "Published body")
        );

        let mut fetched = baseline.clone();
        fetched.comments.push(ReadComment {
            draft: false,
            content: CommentContent {
                id: CommentId("original".into()),
                body: "Published body".into(),
            },
            author: Some(user()),
            target: CommentTarget::PullRequest,
        });
        if state == "fetched_before_receipt" {
            workspace = refresh(&mut store, workspace, "fetched", &fetched);
        }
        store
            .record_operation_result(
                &workspace.batch.id,
                &operation,
                &[],
                &json!({"confirmed":true}),
            )
            .unwrap();
        workspace = store.workspace(&workspace.id).unwrap().unwrap();
        if matches!(state, "fetched_after_receipt" | "deleted") {
            workspace = refresh(&mut store, workspace, "fetched", &fetched);
        }
        if state == "deleted" {
            workspace = refresh(&mut store, workspace, "deleted", &baseline);
        }
        let receipts = workspace.results.clone();
        store
            .append_event(&workspace.batch.id, &draft("unrelated", "New local draft"))
            .unwrap();
        let current = store.workspace(&workspace.id).unwrap().unwrap();
        assert_eq!(current.results, receipts);
        let (projected, pending) = current.get_unpublished().unwrap();
        assert_eq!(pending.operations.len(), 1, "{state}");
        assert!(
            matches!(&pending.operations[0].change, Event::CreateComment { comment } if comment.content.id.0 == "unrelated")
        );
        assert_eq!(
            projected
                .comments
                .iter()
                .any(|comment| comment.content().id.0 == "original"),
            state != "deleted",
            "{state}"
        );
        assert!(
            store
                .append_event(&workspace.batch.id, &draft("original", "Reused identity"))
                .is_err()
        );
    }
}
