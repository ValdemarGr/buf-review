CREATE TABLE IF NOT EXISTS pull_requests (
    pull_request_id TEXT PRIMARY KEY NOT NULL,
    object TEXT NOT NULL CHECK (json_valid(object))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS pull_request_locators
    ON pull_requests(json_extract(object, '$.source.locator') COLLATE NOCASE)
    WHERE json_extract(object, '$.source.kind') = 'remote';

CREATE UNIQUE INDEX IF NOT EXISTS pull_request_intents
    ON pull_requests(
        json_extract(object, '$.source.branches.source.repository') COLLATE NOCASE,
        json_extract(object, '$.source.branches.source.branch'),
        json_extract(object, '$.source.branches.sink.repository') COLLATE NOCASE,
        json_extract(object, '$.source.branches.sink.branch')
    ) WHERE json_extract(object, '$.source.kind') = 'local_intent';

CREATE TABLE IF NOT EXISTS pr_resolutions (
    pull_request_id TEXT PRIMARY KEY NOT NULL REFERENCES pull_requests(pull_request_id),
    state TEXT NOT NULL CHECK (json_valid(state))
) STRICT;

CREATE TABLE IF NOT EXISTS reviews (
    review_id TEXT PRIMARY KEY NOT NULL,
    reviewer TEXT NOT NULL CHECK (json_valid(reviewer))
) STRICT;

-- Immutable JSON values are shared by identity; the value is not indexed twice.
CREATE TABLE IF NOT EXISTS stored_values (
    value_id TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL CHECK (json_valid(value))
) STRICT;

CREATE TABLE IF NOT EXISTS review_snapshots (
    review_id TEXT PRIMARY KEY NOT NULL REFERENCES reviews(review_id),
    value_id TEXT NOT NULL REFERENCES stored_values(value_id)
) STRICT;

CREATE VIEW IF NOT EXISTS read_states AS
    SELECT review_id, value AS snapshot
    FROM review_snapshots JOIN stored_values USING(value_id);

CREATE TABLE IF NOT EXISTS batches (
    batch_id TEXT PRIMARY KEY NOT NULL,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    revision INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS batch_revisions (
    batch_id TEXT NOT NULL REFERENCES batches(batch_id),
    revision INTEGER NOT NULL,
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    PRIMARY KEY(batch_id, revision)
) STRICT;

CREATE TABLE IF NOT EXISTS write_events (
    batch_id TEXT NOT NULL REFERENCES batches(batch_id),
    revision INTEGER NOT NULL DEFAULT 0,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event TEXT NOT NULL CHECK (json_valid(event)),
    PRIMARY KEY (batch_id, revision, sequence)
) STRICT;

CREATE TABLE IF NOT EXISTS review_sources (
    review_id TEXT PRIMARY KEY NOT NULL REFERENCES reviews(review_id),
    backend TEXT NOT NULL,
    locator TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS id_mappings (
    review_id TEXT NOT NULL REFERENCES review_sources(review_id),
    resource TEXT NOT NULL,
    core_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    PRIMARY KEY (review_id, resource, core_id),
    UNIQUE (review_id, resource, remote_id)
) STRICT;

CREATE TABLE IF NOT EXISTS operation_results (
    batch_id TEXT NOT NULL REFERENCES batches(batch_id),
    operation_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    operation TEXT NOT NULL CHECK (json_valid(operation)),
    mappings TEXT NOT NULL CHECK (json_valid(mappings)),
    response TEXT NOT NULL CHECK (json_valid(response)),
    PRIMARY KEY (batch_id, operation_id),
    UNIQUE (batch_id, sequence)
) STRICT;

-- Workspace generations retain superseded local intent without modifying snapshots.
CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id TEXT PRIMARY KEY NOT NULL,
    reviewer_id TEXT NOT NULL,
    pull_request_id TEXT NOT NULL,
    locator TEXT,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    state TEXT NOT NULL CHECK (json_valid(state))
) STRICT;

CREATE TABLE IF NOT EXISTS workspace_history (
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    generation INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (json_valid(state)),
    action TEXT NOT NULL CHECK (json_valid(action)),
    PRIMARY KEY (workspace_id, generation)
) STRICT;

CREATE TABLE IF NOT EXISTS workspace_active (
    reviewer_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    selected INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS workspace_publication_lock (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id)
) STRICT;
