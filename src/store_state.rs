//! Storage references and SQLite-applied changes keep generations independent
//! of fetched snapshot size. Public domain values remain fully hydrated.
use super::*;
use crate::workspace::Workspace;
use serde_json::json;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Delta {
    Set { path: String, value: Value },
    Remove { path: String },
}

// Object members change independently; arrays are native replacement values.
// SQLite applies paths, including explicit JSON nulls, without merge-patch's
// ambiguity between removing a member and assigning null.
fn changes(before: &Value, after: &Value, path: &str, output: &mut Vec<Delta>) {
    if before == after {
        return;
    }
    if let (Value::Object(before), Value::Object(after)) = (before, after) {
        for key in before.keys().filter(|key| !after.contains_key(*key)) {
            output.push(Delta::Remove {
                path: format!("{path}.{}", serde_json::to_string(key).unwrap()),
            });
        }
        for (key, value) in after {
            let child = format!("{path}.{}", serde_json::to_string(key).unwrap());
            match before.get(key) {
                Some(previous) => changes(previous, value, &child, output),
                None => output.push(Delta::Set {
                    path: child,
                    value: value.clone(),
                }),
            }
        }
    } else {
        output.push(Delta::Set {
            path: path.into(),
            value: after.clone(),
        });
    }
}

pub(super) struct HistoryRow {
    pub generation: u64,
    pub state: Value,
    pub action: Value,
}

impl Store {
    pub(super) fn store_value(
        connection: &Connection,
        value: &Value,
    ) -> Result<String, StoreError> {
        Self::store_json(connection, &serde_json::to_string(value)?)
    }

    fn store_json(connection: &Connection, value: &str) -> Result<String, StoreError> {
        let parsed: Value = serde_json::from_str(value)?;
        let canonical = serde_json::to_vec(&parsed)?;
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &canonical).to_string();
        connection.execute(
            "INSERT OR IGNORE INTO stored_values(value_id,value) VALUES (?1,?2)",
            (&id, value),
        )?;
        let existing: String = connection.query_row(
            "SELECT value FROM stored_values WHERE value_id=?1",
            [&id],
            |row| row.get(0),
        )?;
        if serde_json::from_str::<Value>(&existing)? != parsed {
            return Err(crate::workspace::WorkspaceError::Invalid(
                "immutable JSON value identity collision".into(),
            )
            .into());
        }
        Ok(id)
    }

    fn stored_value(connection: &Connection, id: &str) -> Result<Value, StoreError> {
        let value: String = connection.query_row(
            "SELECT value FROM stored_values WHERE value_id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    }

    pub(super) fn compact_workspace(
        connection: &Connection,
        workspace: &Workspace,
    ) -> Result<Value, StoreError> {
        let mut state = serde_json::to_value(workspace)?;
        state.as_object_mut().unwrap().remove("review");
        state["review_id"] = json!(workspace.review.id);
        state["batch"].as_object_mut().unwrap().remove("write");
        state["event_count"] = json!(workspace.batch.write.events.len());
        state.as_object_mut().unwrap().remove("results");
        state["result_count"] = json!(workspace.results.len());
        state["comparison_files"] = json!(Self::store_value(
            connection,
            &serde_json::to_value(&workspace.comparison_files)?,
        )?);
        Ok(state)
    }

    pub(super) fn hydrate_workspace(
        connection: &Connection,
        mut state: Value,
    ) -> Result<Workspace, StoreError> {
        let review = Self::read_review(
            connection,
            &serde_json::from_value(state["review_id"].clone())?,
        )?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        state["review"] = serde_json::to_value(review)?;
        let batch: BatchId = serde_json::from_value(state["batch"]["id"].clone())?;
        let revision: BatchRevision = serde_json::from_value(state["batch"]["revision"].clone())?;
        let mut saved = Self::read_revision(connection, &batch, revision.number)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let count = state["event_count"]
            .as_u64()
            .ok_or(rusqlite::Error::InvalidQuery)? as usize;
        if saved.write.events.len() < count {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        saved.write.events.truncate(count);
        state["batch"]["write"] = serde_json::to_value(saved.write)?;
        let results = Self::read_results(connection, &batch)?;
        let count = state["result_count"]
            .as_u64()
            .ok_or(rusqlite::Error::InvalidQuery)? as usize;
        if results.len() < count {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        state["results"] = serde_json::to_value(&results[..count])?;
        state["comparison_files"] = Self::stored_value(
            connection,
            state["comparison_files"]
                .as_str()
                .ok_or(rusqlite::Error::InvalidQuery)?,
        )?;
        Ok(serde_json::from_value(state)?)
    }

    pub(super) fn state_delta(before: &Value, after: &Value) -> Result<String, StoreError> {
        let mut delta = Vec::new();
        changes(before, after, "$", &mut delta);
        Ok(serde_json::to_string(&delta)?)
    }

    fn apply_delta(
        connection: &Connection,
        state: &str,
        delta: &str,
    ) -> Result<String, StoreError> {
        let mut state = state.to_owned();
        for change in serde_json::from_str::<Vec<Delta>>(delta)? {
            state = match change {
                Delta::Set { path, value } => connection.query_row(
                    "SELECT json_set(?1,?2,json(?3))",
                    (&state, path, serde_json::to_string(&value)?),
                    |row| row.get(0),
                )?,
                Delta::Remove { path } => {
                    connection.query_row("SELECT json_remove(?1,?2)", (&state, path), |row| {
                        row.get(0)
                    })?
                }
            };
        }
        Ok(state)
    }

    fn result_value(
        connection: &Connection,
        batch: &str,
        id: &str,
    ) -> Result<Option<Value>, StoreError> {
        let result = connection.query_row(
            "SELECT operation,mappings,response FROM operation_results WHERE batch_id=?1 AND operation_id=?2",
            (batch, id),
            |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?)),
        ).optional()?;
        result
            .map(|(operation, mappings, response)| {
                Ok(json!({
                    "operation":serde_json::from_str::<Value>(&operation)?,
                    "mappings":serde_json::from_str::<Value>(&mappings)?,
                    "response":serde_json::from_str::<Value>(&response)?,
                }))
            })
            .transpose()
    }

    pub(super) fn compact_action(
        connection: &Connection,
        state: &Value,
        action: &Value,
    ) -> Result<Value, StoreError> {
        let mut action = action.clone();
        if action["action"] == "publication_group" {
            action["actions"] = Value::Array(
                action["actions"]
                    .as_array()
                    .ok_or(rusqlite::Error::InvalidQuery)?
                    .iter()
                    .map(|child| Self::compact_action(connection, state, child))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            return Ok(action);
        }
        match action["action"].as_str() {
            Some("set_attempt_provider") if action["provider"] == state["attempt"]["provider"] => {
                action.as_object_mut().unwrap().remove("provider");
                action["state_reference"] = json!("provider");
            }
            Some("begin_publication") if action["operations"] == state["attempt"]["operations"] => {
                action.as_object_mut().unwrap().remove("operations");
                action["state_reference"] = json!("operations");
            }
            // Result actions are a constant-size reference to their immutable receipt.
            Some("record_result") => {
                if let (Some(batch), Some(id)) = (
                    state["batch"]["id"].as_str(),
                    action["result"]["operation"]["id"].as_str(),
                ) && Self::result_value(connection, batch, id)?.as_ref() == action.get("result")
                {
                    let result = action.as_object_mut().unwrap().remove("result").unwrap();
                    action["result_reference"] = result["operation"]["id"].clone();
                }
            }
            _ => {}
        }
        Ok(action)
    }

    fn expand_action(
        connection: &Connection,
        state: &Value,
        mut action: Value,
    ) -> Result<Value, StoreError> {
        if action["action"] == "publication_group" {
            action["actions"] = Value::Array(
                action["actions"]
                    .as_array()
                    .ok_or(rusqlite::Error::InvalidQuery)?
                    .iter()
                    .cloned()
                    .map(|child| Self::expand_action(connection, state, child))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            return Ok(action);
        }
        if let Some(field) = action.as_object_mut().unwrap().remove("state_reference") {
            let field = field.as_str().ok_or(rusqlite::Error::InvalidQuery)?;
            action[field] = state["attempt"][field].clone();
        }
        if let Some(id) = action.as_object_mut().unwrap().remove("result_reference") {
            let result = Self::result_value(
                connection,
                state["batch"]["id"]
                    .as_str()
                    .ok_or(rusqlite::Error::InvalidQuery)?,
                id.as_str().ok_or(rusqlite::Error::InvalidQuery)?,
            )?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            action["result"] = result;
        }
        Ok(action)
    }

    pub(super) fn history_states(
        connection: &Connection,
        id: &str,
        through: Option<u64>,
    ) -> Result<Vec<HistoryRow>, StoreError> {
        Self::read_history_states(connection, id, through, false)
    }

    pub(super) fn publication_states(
        connection: &Connection,
        id: &str,
    ) -> Result<Vec<HistoryRow>, StoreError> {
        Self::read_history_states(connection, id, None, true)
    }

    fn read_history_states(
        connection: &Connection,
        id: &str,
        through: Option<u64>,
        publication: bool,
    ) -> Result<Vec<HistoryRow>, StoreError> {
        let rows = {
            let mut statement = connection.prepare(
                "WITH history AS (
                    SELECT generation,action,
                        CASE WHEN ?3 THEN (SELECT json_group_array(json(value)) FROM json_each(state)
                            WHERE json_extract(value,'$.path') LIKE '$.\"attempt\"%'
                               OR json_extract(value,'$.path') LIKE '$.\"suspended_attempts\"%'
                               OR json_extract(value,'$.path') LIKE '$.\"last_error\"%'
                               OR json_extract(value,'$.path') LIKE '$.\"batch\"%')
                        ELSE state END AS state
                    FROM workspace_history
                    WHERE workspace_id=?1 AND (?2 IS NULL OR generation<=?2)
                 ) SELECT generation,state,action FROM history
                 WHERE NOT ?3 OR state!='[]' OR json_extract(action,'$.action') IN
                    ('open','refresh','provider_update','begin_publication','record_result',
                     'finish_publication','suspend_publication','resume_publication','publication_group')
                 ORDER BY generation",
            )?;
            statement
                .query_map(
                    (id, through.map(|generation| generation as i64), publication),
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)? as u64,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut state = "{}".to_owned();
        rows.into_iter()
            .map(|(generation, delta, action)| {
                state = Self::apply_delta(connection, &state, &delta)?;
                let value = serde_json::from_str(&state)?;
                let action: Value = serde_json::from_str(&action)?;
                // Provider deltas affect the next lifecycle entry; no need to
                // expand their redundant action payload for publication summaries.
                let action = if publication && action["action"] == "set_attempt_provider" {
                    action
                } else {
                    Self::expand_action(connection, &value, action)?
                };
                Ok(HistoryRow {
                    generation,
                    action,
                    state: value,
                })
            })
            .collect()
    }
}
