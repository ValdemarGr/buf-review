# buf-review (temporary live test)

An editor-independent review tool with a Rust domain model, SQLite persistence,
local Git diffs and GitHub reading/writing through the `gh` CLI. The Neovim
frontend is a separate next step.

## Fetch

With `gh` installed and authenticated, run from your project directory:

```sh
cargo run --bin pr -- fetch https://github.com/OWNER/REPO/pull/NUMBER
```

Or install with `cargo install --path .` and run `pr fetch <PR URL>`. The command
accepts github.com PR URLs and prints the new local `ReviewId` on stdout. It saves
the fetched snapshot in the current directory's `.review/state.sqlite3` without
creating a batch. Repeated fetches create independent immutable snapshots.

The adapter reads PR metadata, revisions, changed files and the authenticated
user's viewed flags, PR-wide comments, diff comments and replies, individual
comment reactions and reviews. All collections are paginated through `gh api`.
Opaque core IDs are stable across fetches; provider resource IDs and the PR URL
are saved separately from read snapshots and draft events.

Fetching finishes before storage begins. The snapshot and provider mappings are
then inserted in one transaction. A failed request or invalid response leaves no
partial snapshot. Incomplete file collections, differing revision pairs during
fetch and PRs beyond GitHub's 3000-file REST limit are rejected. Remote reads are
not a server-side transaction, so other review activity can change during fetch.

## CLI workflow

Commands use the current directory's `.review/state.sqlite3`. Review and batch
selection is explicit; opening a batch does not change any global active state.
Install with `cargo install --path .`, or replace `pr` below with
`cargo run --bin pr --`.

```sh
review=$(pr fetch https://github.com/OWNER/REPO/pull/NUMBER)
batch=$(pr batch new "$review")
pr show "$review"
pr batch open "$batch"
pr diff "$review"
```

Append one JSON event at a time through stdin. The following marks a file viewed:

```sh
pr stage "$batch" <<'JSON'
{"kind":"set_file_viewed","path":"src/lib.rs","viewed":true}
JSON
pr state "$batch"
pr plan "$batch"
pr commit "$batch"
```

| Command | Output |
| --- | --- |
| `pr fetch <url>` | New review ID, plain text |
| `pr reviews` | JSON array of review IDs |
| `pr show <review-id>` | JSON review, including reviewer and fetched snapshot |
| `pr batch new <review-id>` | New batch ID, plain text |
| `pr batch list <review-id>` | JSON array of batch IDs |
| `pr batch open <batch-id>` | JSON batch and its complete ordered event log |
| `pr stage <batch-id>` | Retained event JSON; input is one Event JSON on stdin |
| `pr state <batch-id>` | JSON projection of snapshot and the complete event log |
| `pr plan <batch-id>` | JSON pending net operations, accounting for saved results |
| `pr diff <review-id> [--base <commit>] [--head <commit>] [--path <path>]` | Local Git diff |
| `pr commit <batch-id>` | JSON array of all successful operation results for the batch |

Errors go to stderr with a nonzero exit status. `stage` deserializes and retains
events without cancelling or replacing earlier events. `state`, `plan` and
`commit` validate their interpretation; invalid references or unsupported
placements produce explicit errors.

Diffs use two local commit objects, defaulting to the fetched base/head. Both
overrides are independent. The command does not fetch missing commits or change
the working tree. Paths are literal, and external diff/text conversion commands
are disabled.

### Events

`src/model.rs` defines the wire format. For example, create a PR-wide draft with
a new logical ID and the `reviewer` object returned by `pr show`:

```json
{
  "kind": "create_comment",
  "comment": {
    "id": "YOUR-NEW-LOGICAL-UUID",
    "body": "Review comment",
    "author": {"id": "REVIEWER-CORE-ID", "name": "reviewer"},
    "target": {"kind": "pull_request"}
  }
}
```

The same ID targets subsequent operations:

```json
{"kind":"edit_comment","comment":"YOUR-NEW-LOGICAL-UUID","body":"Revised text"}
{"kind":"delete_comment","comment":"YOUR-NEW-LOGICAL-UUID"}
{"kind":"set_comment_reaction","comment":"COMMENT-CORE-ID","reaction":"👍","present":true}
{"kind":"set_review_conclusion","conclusion":{"outcome":"approve","body":null}}
{"kind":"dismiss_review","review":"REVIEW-RESOURCE-CORE-ID","message":"Superseded"}
```

These are separate event examples; submit each as a separate `stage` invocation.
Use `present:false` to remove a reaction and `viewed:false` to mark a file
unviewed. Conclusions accept `comment`, `approve` or `request_changes`.

A diff draft's target contains the complete local comparison and a one-based,
inclusive range on one side. A single line has equal endpoints; `range:null`
targets the file:

```json
{
  "kind": "diff",
  "target": {
    "revision": {"base": "BASE-SHA", "head": "HEAD-SHA"},
    "path": "src/lib.rs",
    "range": {
      "start": {"side": "head", "line": 3},
      "end": {"side": "head", "line": 5}
    }
  }
}
```

Use `side:"base"` for the old side. Replies use
`{"kind":"reply","target":"PARENT-COMMENT-CORE-ID"}` and require a diff
thread. The parent may be fetched, a draft, or another reply in that thread.
PR-wide comments have no replies in the core specification. Recreating a
deleted draft requires a new ID.

## Commit and retry

The core projects the entire event log into intended state. At commit time it
compares that state with the fetched snapshot plus successful operation results.
The resulting `CommitState` contains only net operations. A create followed by a
delete produces no server write; toggles back to their baseline cancel too.
The original events remain inspectable.

After each successful server operation, its processed payload, returned IDs and
response metadata are saved in one SQLite transaction before the next operation.
Returned IDs are also installed in the separate provider mapping table. New
comments retain their logical IDs, so later edits, replies, reactions and deletes
can target the published resources.

A failure stops the commit. Run `pr plan <batch-id>` to inspect remaining changes,
then repeat `pr commit <batch-id>` to resume. Recorded effects are accounted for,
so a successful creation is not repeated. Append new events to any old batch and
commit it again. A later delete removes an already published draft even though
its original create/delete pair would have cancelled before publication. A
successful conclusion is not repeated unless the intended conclusion changes.

GitHub publishing validates known constraints for the whole plan before writing.
It supports PR-wide and diff comments, native diff replies, edits/deletes,
viewed/unviewed files, all eight GitHub comment reactions, conclusions and
dismissal with a reason. GitHub requires nonempty bodies for comment-only and
request-changes conclusions; the core keeps optional text for all outcomes.
Draft diff anchors retain their base/head pair, but GitHub accepts a reviewed
head, path and location rather than an arbitrary comparison base. The adapter
does not substitute a different anchor; GitHub validates whether the location
exists in its diff. Permissions are also enforced by GitHub.

Commit processes hold an OS lock on the existing SQLite file while preparing,
publishing and recording results. This serializes commits within the project;
the OS releases the lock when the process exits. It does not require a lock file
or prevent other commands from staging events for the next commit.

Remote writes and SQLite commits cannot form a single transaction. If a remote
write succeeds but its local result cannot be saved, the CLI reports that
explicitly; repeating the operation can repeat the remote write. A lost server
response has the same uncertainty.

## Model

Each local `ReviewId` owns a fetched `ReadState`. A globally unique `BatchId`
identifies an editable `WriteState` and links it to one review. Multiple batches
can share a review's fetched baseline. A `PullRequestId` identifies the PR;
multiple local reviews can concern the same PR.
All resource identities are local domain identities. Backend adapters will map
provider IDs and data types into this model rather than exposing them to the
core. The caller assigns resource identities; the CLI generates review/batch IDs.

The read state contains PR metadata, revision pair, changed files, comments,
reactions and review state, including pending and dismissed reviews. Read
comments can have unavailable authors and incomplete historical anchors. The
adapter preserves original and current locations, when available, plus the
diff hunk and line/file subject. It does not invent a historical base revision
or an outdated flag absent from the REST response.

`ReadComment` and `DraftComment` compose common content and target kinds but have
separate representations. Draft anchors retain the exact local base/head pair;
a missing draft range targets the whole file. Read anchors can contain only the
reviewed commit, and a missing read range can mean unavailable location data.
This does not weaken the write-side requirements. Legacy snapshots containing
complete comparison anchors remain readable without rewriting stored payloads.

The write state is an ordered event log. Events can create, edit and delete
comments, mark files viewed or unviewed, set a review conclusion, and add or remove
comment reactions, or dismiss a review with a reason. Replies and reactions refer
to `CommentId`, whether the target was fetched or is still a draft. Replies are
limited to diff threads; PR-wide comments have no replies in this specification.
Editing either kind uses `EditComment`.
Reaction values are text or emoji, not provider-specific enums.

Staging preserves every event. Adding then deleting a comment, or adding then
removing a reaction, leaves both events inspectable. Recreating a deleted comment
uses a new identity. There is no undo operation. The core interprets events
and collapse net changes into commit state. A backend adapter consumes that
post-processed state and writes it to the server; it does not interpret the raw
event history. Provider mapping and server communication belong to the adapter.
The GitHub adapter uses `gh` for API access and authentication. Dismissal is a
standard core operation; no backend capability framework has been added.

The CLI selects an existing batch ID or creates a new batch. Batches have no
completed status: users can append to an old batch and submit it again. Submission
does not exhaust the ID or make the log immutable.

## Storage

`Store::open(project_root)` creates or opens `.review/state.sqlite3`. SQLite keeps
review metadata, fetched snapshots, batches and individual sequenced events in
separate tables. The `batches` table links `batch_id` to `review_id`; event rows
refer only to `batch_id`. Snapshots and event payloads use Serde JSON; SQLite owns
transactions and event ordering. `src/schema.sql` defines the current schema.

`review_sources` and `id_mappings` hold backend metadata separately.
`Store::import_review` saves a snapshot and its source/mappings atomically;
`Store::review_backend` retrieves that metadata. Read snapshots and write events
stay in their own tables.

`operation_results` stores successful processed writes separately, keyed by
batch and deterministic operation ID and ordered by execution sequence. A row
means the operation completed; its JSON payload, mappings and raw provider
response preserve the relevant metadata. Success does not complete the batch.
Logical resource IDs in event JSON can join `id_mappings` to resolve returned
server IDs, or join the processed operation JSON in `operation_results` to
inspect completion metadata. A duplicate result cannot overwrite a prior
successful result.

The store creates and reads reviews and batches, lists their IDs and appends
events using only `BatchId`. No storage operation replaces a baseline or collapses an
event log. Batch IDs are unique across all reviews, and SQLite enforces the
review/batch relationship. Event sequences are independent for each batch.

Opening a database from the previous review-owned-log schema migrates it in one
transaction. Each original review gets a batch whose ID equals its review ID.
Snapshots, reviewers and ordered events are preserved. The original event table
is retained as `legacy_write_events`; the old `committed` column is retained but
no longer controls editing. Reopening the migrated store does not migrate again.

To recover the original schema for an older binary, stop database users and,
within one SQLite transaction, rename `write_events` to `batch_write_events`,
then rename `legacy_write_events` to `write_events`. Original snapshots and logs
are available to the old reader; newer batch data remains preserved in the
batch tables but cannot be represented by the old API. Use a copy of the database
for this recovery so the current store remains available.

## Checks

Run `cargo test`, `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.
Rust 1.89 or newer is required. The existing Nix development shell supplies the
Rust tools. GitHub integration tests use a fixture `gh` executable; they do not
write to a live PR.
