//! Readable, deterministic projections of the immutable workspace journal.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{core::Operation, sessions::Counts};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub generation: u64,
    pub summary: String,
    pub details: Vec<String>,
    pub level: Level,
    pub raw: Vec<String>,
}

#[derive(Deserialize)]
struct Record {
    generation: u64,
    action: Value,
}

fn single_line(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result = String::new();
    for character in value.chars().take(160) {
        if character.is_control() {
            result.extend(character.escape_default());
        } else {
            result.push(character);
        }
    }
    if value.chars().count() > 160 {
        result.push('…');
    }
    result
}

fn title(value: &str) -> String {
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
        .replace('_', " ")
}

fn location(value: &Value) -> String {
    let path = value["path"].as_str().unwrap_or_default();
    let start = &value["range"]["start"];
    let end = &value["range"]["end"];
    match (start["line"].as_u64(), end["line"].as_u64()) {
        (Some(first), Some(last)) => {
            let side = start["side"].as_str().unwrap_or_default();
            if first == last && start["side"] == end["side"] {
                format!("{path}:{side}:{first}")
            } else {
                format!(
                    "{path}:{side}:{first}–{}:{last}",
                    end["side"].as_str().unwrap_or_default()
                )
            }
        }
        _ => path.to_owned(),
    }
}

fn target(value: &Value) -> String {
    match value["kind"].as_str().unwrap_or_default() {
        "pull_request" => "PR".into(),
        "diff" => location(&value["target"]),
        "reply" => format!("reply to {}", value["target"].as_str().unwrap_or_default()),
        _ => String::new(),
    }
}

fn comment(verb: &str, value: &Value) -> String {
    let anchor = target(&value["target"]);
    let id = value["comment"].as_str().unwrap_or_default();
    let anchor = if anchor.is_empty() { id } else { &anchor };
    let body = single_line(value["body"].as_str().unwrap_or_default());
    let suffix = [anchor, &body]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if suffix.is_empty() {
        verb.into()
    } else {
        format!("{verb} · {suffix}")
    }
}

fn counts(value: &Value) -> String {
    let Ok(operations) = serde_json::from_value::<Vec<Operation>>(value.clone()) else {
        return format!("{} operations", value.as_array().map_or(0, Vec::len));
    };
    let counts: Counts = operations.iter().collect();
    let labels = [
        (counts.comments, "comment", "comments"),
        (counts.edits, "edit", "edits"),
        (counts.deletions, "deletion", "deletions"),
        (counts.reactions, "reaction", "reactions"),
        (counts.files, "file", "files"),
        (counts.conclusions, "conclusion", "conclusions"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, singular, plural)| {
        format!("{count} {}", if count == 1 { singular } else { plural })
    })
    .collect::<Vec<_>>();
    if labels.is_empty() {
        "0 operations".into()
    } else {
        labels.join(", ")
    }
}

fn operation(value: &Value) -> String {
    match value["kind"].as_str().unwrap_or_default() {
        "create_comment" => comment("Created comment", &value["comment"]),
        "edit_comment" => comment("Edited comment", value),
        "delete_comment" => comment("Deleted comment", value),
        "set_file_viewed" => format!(
            "Marked file {} · {}",
            if value["viewed"] == true {
                "viewed"
            } else {
                "unviewed"
            },
            value["path"].as_str().unwrap_or_default()
        ),
        "set_comment_reaction" => format!(
            "{} reaction {} · {}",
            if value["present"] == true {
                "Added"
            } else {
                "Removed"
            },
            value["reaction"].as_str().unwrap_or_default(),
            value["comment"].as_str().unwrap_or_default()
        ),
        "set_review_conclusion" => format!(
            "Set review conclusion · {}",
            title(value["conclusion"]["outcome"].as_str().unwrap_or_default())
        ),
        "dismiss_review" => format!(
            "Dismissed review · {}",
            value["review"].as_str().unwrap_or_default()
        ),
        kind => title(kind),
    }
}

// Scalar strings are split before reaching Neovim's line API. JSON in `raw`
// retains their exact escapes and every field, including unknown provider data.
fn fields(value: &Value, indent: usize, lines: &mut Vec<String>) {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if value.is_null() {
                    continue;
                }
                match value {
                    Value::Object(_) | Value::Array(_) => {
                        lines.push(format!("{prefix}{}:", escaped_line(&title(key))));
                        fields(value, indent + 2, lines);
                    }
                    _ => {
                        let value = value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string());
                        let mut parts = value.split('\n');
                        lines.push(format!(
                            "{prefix}{}: {}",
                            escaped_line(&title(key)),
                            escaped_line(parts.next().unwrap_or_default())
                        ));
                        lines.extend(parts.map(|line| format!("{prefix}  {}", escaped_line(line))));
                    }
                }
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                lines.push(format!("{prefix}{}:", index + 1));
                fields(value, indent + 2, lines);
            }
        }
        _ => lines.push(format!("{prefix}{}", escaped_line(&value.to_string()))),
    }
}

fn escaped_line(line: &str) -> String {
    line.chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

/// Derive display data solely from recorded actions, never mutable current state.
/// The journal remains ascending in storage; only this presentation is reversed.
pub fn project(records: Vec<Value>) -> Result<Vec<Entry>, serde_json::Error> {
    let mut entries = Vec::with_capacity(records.len());
    let mut publication = None::<String>;
    for raw in records {
        let record: Record = serde_json::from_value(raw.clone())?;
        let action = &record.action;
        let mut level = Level::Info;
        let summary = match action["action"].as_str().unwrap_or_default() {
            "open" => "Opened session".into(),
            "refresh" => "Refreshed PR".into(),
            "provider_update" => "Updated provider state".into(),
            "start" => "Started review".into(),
            "leave" => "Left review".into(),
            "save"
                if action["body"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                    && action["comment"].is_null() =>
            {
                "Skipped empty comment".into()
            }
            "save" => comment(
                if action["body"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    "Deleted comment"
                } else {
                    "Saved comment"
                },
                action,
            ),
            "delete" => comment("Deleted comment", action),
            "delete_comments" => format!(
                "Deleted comments · {}",
                action["comments"]
                    .as_array()
                    .map(|comments| comments
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            ),
            "restore" => comment("Undid local comment changes", action),
            "undo" => format!(
                "Undid comment changes · {}",
                action["comments"]
                    .as_array()
                    .map(|comments| comments
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            ),
            "viewed" => format!(
                "Marked file {} · {}",
                if action["viewed"] == true {
                    "viewed"
                } else {
                    "unviewed"
                },
                action["path"].as_str().unwrap_or_default()
            ),
            "reaction" => format!(
                "Marked comment {} · {}",
                if action["present"] == true {
                    "Done"
                } else {
                    "Todo"
                },
                action["comment"].as_str().unwrap_or_default()
            ),
            "summary" => format!(
                "Saved review conclusion · {}",
                title(action["conclusion"]["outcome"].as_str().unwrap_or_default())
            ),
            "compare_latest" => "Updated comparison to latest PR".into(),
            "relocate" => format!(
                "Relocated comment · {} · {}",
                action["comment"].as_str().unwrap_or_default(),
                location(&action["location"])
            ),
            "resolve" => format!(
                "Resolved conflict · {} · {}",
                action["comment"].as_str().unwrap_or_default(),
                title(action["resolution"].as_str().unwrap_or_default())
            ),
            "begin_publication" => {
                let label = format!(
                    "{} · {}",
                    action["destination"].as_str().unwrap_or_default(),
                    counts(&action["operations"])
                );
                publication = Some(label.clone());
                format!("Publishing {label}")
            }
            "resume_publication" => {
                // Begin actions do not record generated attempt IDs. Do not
                // attribute resumed attempts to a possibly different batch.
                publication = None;
                format!(
                    "Resumed publication · {}",
                    action["attempt_id"].as_str().unwrap_or_default()
                )
            }
            "suspend_publication" => {
                publication = None;
                level = Level::Error;
                "Suspended publication · outcome uncertain".into()
            }
            "record_result" => {
                level = Level::Success;
                format!(
                    "Published · {}",
                    operation(&action["result"]["operation"]["change"])
                )
            }
            "set_attempt_provider" => {
                let inflight = action["provider"]["inflight"].as_str().unwrap_or_default();
                if inflight.is_empty() {
                    "Recorded publication progress".into()
                } else {
                    format!("Publishing · {inflight}")
                }
            }
            "finish_publication" => {
                let outcome = if action["uncertain"] == true {
                    level = Level::Error;
                    "Publication outcome uncertain"
                } else if action["success"] == true {
                    level = Level::Success;
                    "Published"
                } else {
                    level = Level::Error;
                    "Publication failed"
                };
                match publication.take() {
                    Some(label) => format!("{outcome} {label}"),
                    None => outcome.into(),
                }
            }
            "refresh_error" => {
                level = Level::Error;
                format!(
                    "Refresh failed · {}",
                    action["error"].as_str().unwrap_or_default()
                )
            }
            "reject_anchor" => {
                level = Level::Error;
                format!(
                    "Rejected comment anchor · {} · {}",
                    action["comment"].as_str().unwrap_or_default(),
                    action["message"].as_str().unwrap_or_default()
                )
            }
            unknown => {
                let label = title(unknown);
                if label.is_empty() {
                    "Unknown action".into()
                } else {
                    label
                }
            }
        };
        let mut details = Vec::new();
        fields(action, 0, &mut details);
        entries.push(Entry {
            generation: record.generation,
            summary: single_line(&summary),
            details,
            level,
            raw: serde_json::to_string_pretty(&raw)?
                .lines()
                .map(str::to_owned)
                .collect(),
        });
    }
    entries.reverse();
    Ok(entries)
}
