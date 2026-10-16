PRAGMA foreign_keys = ON;

BEGIN;

CREATE TABLE IF NOT EXISTS reviews (
    review_id TEXT PRIMARY KEY NOT NULL,
    reviewer TEXT NOT NULL CHECK (json_valid(reviewer)),
    committed INTEGER NOT NULL DEFAULT 0 CHECK (committed IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS read_states (
    review_id TEXT PRIMARY KEY NOT NULL REFERENCES reviews(review_id),
    snapshot TEXT NOT NULL CHECK (json_valid(snapshot))
) STRICT;

CREATE TABLE IF NOT EXISTS write_events (
    review_id TEXT NOT NULL REFERENCES reviews(review_id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event TEXT NOT NULL CHECK (json_valid(event)),
    PRIMARY KEY (review_id, sequence)
) STRICT;

COMMIT;
