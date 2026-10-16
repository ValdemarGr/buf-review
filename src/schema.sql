CREATE TABLE IF NOT EXISTS reviews (
    review_id TEXT PRIMARY KEY NOT NULL,
    reviewer TEXT NOT NULL CHECK (json_valid(reviewer))
) STRICT;

CREATE TABLE IF NOT EXISTS read_states (
    review_id TEXT PRIMARY KEY NOT NULL REFERENCES reviews(review_id),
    snapshot TEXT NOT NULL CHECK (json_valid(snapshot))
) STRICT;

CREATE TABLE IF NOT EXISTS batches (
    batch_id TEXT PRIMARY KEY NOT NULL,
    review_id TEXT NOT NULL REFERENCES reviews(review_id)
) STRICT;

CREATE TABLE IF NOT EXISTS write_events (
    batch_id TEXT NOT NULL REFERENCES batches(batch_id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event TEXT NOT NULL CHECK (json_valid(event)),
    PRIMARY KEY (batch_id, sequence)
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
