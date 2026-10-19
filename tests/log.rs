use buf_review::log::{Level, project};
use serde_json::{Value, json};

fn record(generation: u64, action: Value) -> Value {
    json!({"generation": generation, "action": action})
}

fn create(id: &str) -> Value {
    json!({"id":id,"change":{"kind":"create_comment","comment":{
        "id":id,"author":{"id":"reviewer","name":"Reviewer"},"body":"Check this",
        "target":{"kind":"pull_request"}
    }}})
}

#[test]
fn newest_first_readable_entries_preserve_complete_journal_records() {
    let original = vec![
        record(0, json!({"action":"open"})),
        record(
            4,
            json!({"action":"save","body":"First line\n\nSecond line","destination":"standalone",
            "target":{"kind":"diff","target":{"revision":{"base":"old","head":"new"},
                "path":"src/main.rs","range":{"start":{"line":24,"side":"head"},
                "end":{"line":26,"side":"head"}}}}}),
        ),
        record(
            9,
            json!({"action":"future_operation","extra":{"not_known":true},"future\nkey":"kept\u{0000}"}),
        ),
    ];
    let entries = project(original.clone()).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.generation)
            .collect::<Vec<_>>(),
        vec![9, 4, 0]
    );
    assert_eq!(entries[0].summary, "Future operation");
    assert!(entries[1].summary.contains("src/main.rs:head:24–head:26"));
    assert!(entries[1].summary.contains("First line Second line"));
    assert!(
        entries[1]
            .details
            .iter()
            .any(|line| line == "Body: First line")
    );
    assert!(
        entries[1]
            .details
            .iter()
            .any(|line| line == "  Second line")
    );
    for (entry, original) in entries.iter().zip(original.iter().rev()) {
        assert_eq!(
            serde_json::from_str::<Value>(&entry.raw.join("\n")).unwrap(),
            *original
        );
        assert!(!entry.summary.chars().any(char::is_control));
        assert!(
            entry
                .details
                .iter()
                .chain(&entry.raw)
                .all(|line| !line.chars().any(char::is_control))
        );
    }
}

#[test]
fn publication_counts_outcomes_and_provider_bookkeeping_are_distinct() {
    let operations = json!([create("one"),create("two"),{
        "id":"reaction","change":{"kind":"set_comment_reaction","comment":"remote","reaction":"👍","present":true}
    }]);
    for (success, uncertain, expected, level) in [
        (true, false, "Published", Level::Success),
        (false, false, "Publication failed", Level::Error),
        (false, true, "Publication outcome uncertain", Level::Error),
    ] {
        let entries = project(vec![
            record(1,json!({"action":"begin_publication","destination":"standalone","operations":operations})),
            record(2,json!({"action":"set_attempt_provider","provider":{"inflight":"operation one","prepared":[],"submitted":false}})),
            record(3,json!({"action":"record_result","result":{"operation":create("one"),"mappings":[],"response":{"id":42}}})),
            record(4,json!({"action":"finish_publication","success":success,"uncertain":uncertain,"error":"Server failed\nTry later"})),
        ]).unwrap();
        assert_eq!(
            entries[0].summary,
            format!("{expected} standalone · 2 comments, 1 reaction")
        );
        assert_eq!(entries[0].level, level);
        assert!(
            entries[0]
                .details
                .iter()
                .any(|line| line == "Error: Server failed")
        );
        assert!(entries[0].details.iter().any(|line| line == "  Try later"));
        assert_eq!(entries[1].level, Level::Success);
        assert!(
            entries[1]
                .summary
                .contains("Created comment · PR · Check this")
        );
        assert_eq!(entries[2].summary, "Publishing · operation one");
    }
}

#[test]
fn resumed_attempt_does_not_inherit_unrelated_publication_counts() {
    let entries = project(vec![
        record(1,json!({"action":"begin_publication","destination":"review","operations":[create("one")]})),
        record(2,json!({"action":"suspend_publication"})),
        record(3,json!({"action":"begin_publication","destination":"standalone","operations":[]})),
        record(4,json!({"action":"finish_publication","success":true,"uncertain":false})),
        record(5,json!({"action":"resume_publication","attempt_id":"earlier-review"})),
        record(6,json!({"action":"finish_publication","success":true,"uncertain":false})),
    ]).unwrap();
    assert_eq!(entries[0].summary, "Published");
    assert_eq!(entries[1].summary, "Resumed publication · earlier-review");
    assert_eq!(
        entries[4].summary,
        "Suspended publication · outcome uncertain"
    );
    assert_eq!(entries[4].level, Level::Error);
}

#[test]
fn append_only_history_does_not_rewrite_existing_summaries() {
    let mut records = vec![
        record(0, json!({"action":"open"})),
        record(1, json!({"action":"delete","comment":"remote"})),
        record(
            2,
            json!({"action":"reaction","comment":"other","present":true}),
        ),
    ];
    let before = project(records.clone()).unwrap();
    records.push(record(3, json!({"action":"refresh"})));
    records.push(record(
        4,
        json!({"action":"save","comment":"remote","body":"Different now"}),
    ));
    let after = project(records).unwrap();
    assert_eq!(before, after[2..]);
    assert_eq!(before[0].summary, "Marked comment Done · other");
    assert_eq!(before[1].summary, "Deleted comment · remote");
}

#[test]
fn action_catalog_has_readable_labels_and_error_levels() {
    for (action, expected) in [
        (json!({"action":"refresh"}), "Refreshed PR"),
        (
            json!({"action":"provider_update"}),
            "Updated provider state",
        ),
        (json!({"action":"start"}), "Started review"),
        (json!({"action":"leave"}), "Left review"),
        (
            json!({"action":"delete_comments","comments":["first","second"]}),
            "Deleted comments · first, second",
        ),
        (
            json!({"action":"undo","comments":["first","second"]}),
            "Undid comment changes · first, second",
        ),
        (
            json!({"action":"restore","comment":"id"}),
            "Undid local comment changes · id",
        ),
        (
            json!({"action":"viewed","path":"src.rs","viewed":true}),
            "Marked file viewed · src.rs",
        ),
        (
            json!({"action":"reaction","comment":"id","present":false}),
            "Marked comment Todo · id",
        ),
        (
            json!({"action":"summary","conclusion":{"outcome":"request_changes","body":"Please fix"}}),
            "Saved review conclusion · Request changes",
        ),
        (
            json!({"action":"compare_latest"}),
            "Updated comparison to latest PR",
        ),
        (
            json!({"action":"relocate","comment":"id","location":{"path":"new.rs"}}),
            "Relocated comment · id · new.rs",
        ),
        (
            json!({"action":"resolve","comment":"id","resolution":"remote"}),
            "Resolved conflict · id · Remote",
        ),
        (
            json!({"action":"save","comment":"id","body":"  "}),
            "Deleted comment · id",
        ),
        (json!({"action":"save","body":""}), "Skipped empty comment"),
    ] {
        assert_eq!(
            project(vec![record(1, action)]).unwrap()[0].summary,
            expected
        );
    }
    for action in [
        json!({"action":"refresh_error","error":"Fetch failed\nOffline"}),
        json!({"action":"reject_anchor","comment":"id","message":"Bad anchor\nTry another"}),
    ] {
        let entries = project(vec![record(1, action)]).unwrap();
        assert_eq!(entries[0].level, Level::Error);
        assert!(
            entries[0]
                .details
                .iter()
                .any(|line| line == "  Offline" || line == "  Try another")
        );
    }
}
