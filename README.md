# buf-review

An editor-independent review tool with a Rust domain model, SQLite persistence,
local Git diffs and GitHub reading/writing through the `gh` CLI. Its Neovim
frontend uses immutable code buffers, local Markdown drafts and an individual
comment checklist.

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`, builds the `pr` CLI with
the committed Cargo lockfile, checks `pr --version`, then immediately publishes a
GitHub release with generated notes. The release asset
`pr-<tag>-x86_64-unknown-linux-gnu.tar.gz` contains the `pr` executable, built on
Ubuntu 24.04 for Linux x86_64 with glibc.

Commit the workflow and desired package version in `Cargo.toml` and `Cargo.lock`
before tagging. For example:

```sh
git tag v0.1.0
git push origin v0.1.0
```

## Neovim

Install the CLI with `cargo install --path .`. Add this repository to Neovim's
runtime path through your package manager or `pack/*/start` directory. Neovim
0.10+, Git and authenticated `gh` are required; no setup call or Lua dependency
is needed. The optional `vim.g.buf_review_executable` selects a different `pr`
executable; otherwise it is found on PATH.

Run `:Review` inside a Git repository. From source, the current branch's only
open PR is selected; zero or multiple matches open the PR picker. Selecting a
PR creates a session when none are unfinished, resumes its only unfinished session,
or opens the session browser when several are unfinished. Use
`:Review prs` to choose any PR, or `:Review sessions` to browse saved sessions
grouped by PR. `:Review sessions new` creates another session for the selected
or current PR. Each session is a batch with a stable ID; refresh and publication
preserve it. `:Review` opens in the current window and tab; it never creates
or jumps to a dedicated Review tab. If that repository's review is active
elsewhere, the current tab takes over. Other user splits and unsaved source
buffers are preserved.

Use `:Review pr master` to review the current branch against `master` before a
GitHub PR exists. The command reuses an existing open PR for that exact source/
target repository and branch pair, or creates a local PR intent before opening
its review session. Existing session routing still creates, resumes or offers
unfinished batches. The review comparison follows checked-out Git `HEAD`.
The base must name an available local branch; fetch/create it first when absent.

For a local intent, Enter on the home title opens a Markdown composer: first
line is the PR title, the rest its body. `:w` and `:wq` save locally. Publishing
reuses a matching open PR or creates a draft PR, then sends the selected review
changes. Push the reviewed commits with ordinary Git/Fugitive first; the plugin
does not push them. Binding preserves logical PR, session and draft IDs plus
saved comment anchors. A PR creation result survives later comment failures/restarts.
Conflicting existing local PR identities block binding and retain both sessions.

PR browsing leaves your checkout unchanged. Home shows the local branch, its
relationship to the fetched PR head, and staged/modified/untracked file counts.
Enter on `Checkout:` or run `:Review checkout` to switch to the PR branch.
Unsaved source buffers and unfinished Git operations block switching. With disk
changes, choose Cancel, Stash & switch, or Carry changes & switch. Stashes are
ordinary named Git stashes; restore them through Git or Fugitive. Stashing includes
untracked files but excludes `.review/` and ignored files. Existing local commits
are preserved; successful checkout reloads unmodified source buffers. Review
sessions and saved comment anchors stay fixed. Reopen the diff to read the new
checkout. Checkout orchestration belongs to Lua; Rust supplies its remote PR
source metadata alongside review state.

Use `:Review start` to make new inline comments part of a WIP review. Enter on a
file opens the committed base/`HEAD` diff. When that file has local edits, the
right pane opens the ordinary editable file instead. Press `o` in a review diff
to switch its right pane to **Local**, including a clean file. Local mode stays
editable until the diff is reopened. Commit changes and reopen before creating
comments; unpushed commits are allowed locally. Enter on code opens its existing-thread
list, including a single thread, and does nothing when there are no threads.
Enter on a list entry opens its discussion. `a` on code or in the location's
thread list starts a new thread; `a` in a discussion replies to its root,
including your own thread. Enter edits an unsent local comment. Successfully sent comments, including
pending review comments, are read-only. Visual `a` starts a range comment. `:w` saves the Markdown
composer locally; `:wq` returns to its originating reader or code context.
Existing source editing and navigation remain ordinary Neovim.

Default normal-mode shortcuts use your configured leader: `<leader>rs` shows
Review, `<leader>rr` refreshes, `<leader>rp` publishes, `<leader>rcc` concludes a
review, and `<leader>rcs` inserts a suggestion in its comment composer. Set
`mapleader` before loading the plugin. Existing user mappings take precedence.

In the home buffer, `cc` or `:Review conclude` chooses Comment, Approve or
Request changes and opens the conclusion composer. Choose Draft to remove an
unsent conclusion and keep publication pending. Saving queues one
`publish_review` event; saving again replaces its unsent value. `:wq`, `:x` and
`ZZ` save locally and return. Closing any composer never writes to GitHub.

Only `:Review publish` sends saved operations. Inline comments share one review
per publication. Without a queued conclusion the review stays pending/draft;
with one, publication concludes after its comments and other ordered changes.
To move unpublished standalone inline comments into that review, select
them and run `:Review move` (current comment, Ex range or visual selection).
Sent comments cannot move. Include unpublished parents and replies together.

Publication waits for writes, then fetches fresh PR state. Refresh failure is
reported separately and keeps confirmed publication results intact.
Local events receive UUIDs when created; refresh and retries retain those IDs.
Publication coalesces local intent before excluding confirmed operations. A local
viewed or Done toggle reversed back to its known baseline stays cancelled after
refresh. New toggles after publication are independent changes.

Todo includes fetched remote comments without **your** thumbs-up, including
individual replies and nonblank review summaries. `s` marks the selected fetched
comment Done; `u` reopens it. On a file, the same keys mark viewed/unviewed.
Marking keeps cursor and scroll position; `:Review next` and `:Review previous`
navigate Todo separately. Local drafts and fetched pending review comments are excluded until submitted
and fetched. Pending comments remain visible with a `[draft]` label.
Your own fetched comments enter the same checklist.

Current-comparison Base/Head winbars show `[viewed]` or `[unviewed]` beside the
file path. Both panes update after `s`/`u` from staged local progress without
publishing, preserving their revision/path labels. Historical winbars retain
their existing labels.
Local viewed changes record their commit. Later commits touching that file make
the mark ineffective for both display and publication; its saved event remains.
Checking out the earlier commit and reopening restores the mark.

Overview file rows show `M +12 -3  path`: modified, added (`A`) or deleted (`D`),
with added/removed line counts. Renames, copies and type changes use `M`.
Status and counts use theme colors. Stats describe the checked-out comparison;
unavailable cached counts show `?`.

In Review buffers, `]f`/`[f` navigate unviewed files and `]r`/`[r` navigate Todo
comments. They skip viewed/Done objects, including locally staged progress that
has not been published, and never change progress themselves.
Navigation uses the backend's stable order and the object under the current
buffer's cursor. Each step starts strictly after/before that object, wraps, and
never returns the current object. Counts such as `4]f` take four eligible steps,
loading only the final destination. With no current object, start at the first/last
eligible object; with no destination, stay put. Home rows supply current objects;
previously opened files, discussions and other visible panes supply no context.
`:Review next`/`:Review previous` use the same filtered search for Todo;
`comment` also skips Done, and `file` skips viewed files.

Use `]f`, inspect, `s`, then `]f` to review files; use `]r`, inspect, `s`, then
`]r` for comments. `s` stages viewed/Done without navigation, keeping cursor and
scroll position. Each reply is separate, including replies to Done roots. From
code without a current comment, `]r` opens the first eligible comment at the
current line if available; `[r` opens the last. Otherwise use the first/last
global Todo. Unsaved composers block navigation. Each search uses the object
then under the cursor, with no remembered continuation. `[c`/`]c` retain native
diff navigation.

Newer navigation wins over pending loads. Opening another file closes its
predecessor's managed discussion; same-file and PR-wide discussions remain, as
do additional windows opened manually.
Internal pane changes preserve the jumplist; ordinary user motions still record
jumps normally.

Comment navigation always shows the recorded comment version, with up to three
panes: original base, commented version, and checked-out `HEAD`. Adjacent panes
contract when their file diff is empty; the commented version and its anchor stay
visible even when both surrounding panes contract. Different commit IDs alone do
not require separate panes. A comment on `HEAD` normally needs only its original
base and commented version. File navigation restores the ordinary base/`HEAD`
comparison, using the editable local file on the right when it has local edits.
The commented version shows additions relative to its original base in green and
lines removed toward checked-out `HEAD` in red. Red takes precedence when a
line was both added and later removed. Replies use their root's original anchor;
GitHub's relocated current anchor never replaces the recorded version.
Winbars identify each version, commit and path. Renames are followed; absent files
stay distinct from empty files. Unknown original base-side context is labelled
Reconstructed and its lines are never guessed. Missing history shows the recorded
hunk above the thread. Historical code remains a reference: replies and Done work
in the thread, while new inline comments and file-viewed marks require current
code.
PR-wide comments open their discussion without changing code.
Readers reuse stable numbered buffers during navigation. A manual split keeps
its existing target; navigation uses another slot when needed. Composers remain
separate, and their return readers stay reserved until the composer closes.
PR/session navigation and `o` use the active view's tab. Closing that tab does
not create a replacement; the next `:Review` uses its invoking window and tab.

Inline markers show fetched comments with `[ ]` none Done, `[-]` partly Done,
or `[x]` all Done, using your thumbs-up on each comment/reply. Local comments
are counted separately without a completion badge. Opening or focusing a
comment highlights its exact referenced range on the matching diff side while
its pane is visible. Replies use the root's range; historical anchors never
guess current lines. `BufReviewCommentRange` defaults to the theme's `Visual`.

In home/thread lists and discussions, `dd`, `d{motion}`, `D`, `x` and visual
`d`/`x` delete touched owned comments. Deletion stays local until publication;
deleting an unpublished draft cancels its creation. Headings and dividers are inert.
`u` restores a pending deletion, cancels a new unpublished comment or reply,
or marks a fetched comment Todo or a file unviewed. Visual `u` applies the same
action per object, with selected draft replies canceled before their parents.

Home shows publication status, pending/published counts and publication history.
Enter on a publication opens its saved operations, results and errors read-only.
`:Review log` opens the session's complete event timeline, newest first, with
readable action summaries and highlighted errors. Enter expands an event's
details; its exact recorded JSON is in a nested fold. Native `zo`/`zc` work too.
Generation numbers identify event order; the viewer does not change saved history.

Review actions compose with native motions, macros, `:global`, `:vglobal` and
ranged `:normal`. Reader buffers stay fixed while the command runs. Actions
capture object IDs, enter one ordered session queue and execute as Rust action
lists; readers rebuild at Neovim's safe boundary. Contextual `u` and grouped
deletion resolve against the preceding actions' results. Reads, composer saves
and session switches wait for earlier local writes; saves also complete their
own write before returning. Pending local writes finish on editor exit.
`a` supports complete insert/save/return sequences in native commands. Provider
publication remains explicit and blocks until publication and its refresh
complete; `:normal!` bypasses mappings.
Background refresh fetches an immutable snapshot, then queues its local merge
alongside writes. Local actions never wait for that network fetch.

The selected session and saved drafts persist in the repository root's
`.review/state.sqlite3`. Refresh preserves local intent, reports conflicts and
never moves saved comment anchors. PR refresh runs `git fetch --all`, then reads
the checked-out `HEAD`; fetching never pulls or switches branches. Review shows
`not at remote head` when `HEAD` differs from the fetched PR head, alongside
remote commits absent locally. Publish rejects that mismatch before API writes.
Push or update your checkout with ordinary Git, reopen and inspect, then retry.
Commands, `<Plug>` mappings, composer behavior and original-context navigation are
documented in `:help buf-review` (generate help tags with `:helptags` for a manual
runtime-path installation). [SPEC.md](SPEC.md) records the complete UX contract.

### Workspace protocol

`pr workspace` reads one JSON request from stdin and returns JSON. It discovers
the Git working-tree root, including when invoked from a subdirectory. For
example, `{"op":"active"}` reads saved selection without contacting GitHub;
`{"op":"list","state":"open","assignee":"@me"}` lists assigned PRs.
`{"op":"get","workspace":"BATCH-ID"}` reads a specific saved session
without changing selection or contacting GitHub.
`{"op":"entry"}` routes the current branch; `{"op":"route","url":"PR-URL"}`
routes a selected PR. Routing returns a batch ID, a filtered sessions browser,
or the remote PR list. `{"op":"select","batch":"BATCH-ID"}` explicitly
persists selection and returns its workspace view. `{"op":"sessions"}`
lists saved sessions; `{"op":"new","workspace":"BATCH-ID"}` creates a
new session for that PR. View IDs are canonical batch IDs. Backend projections
provide pending/published counts and publication history; Lua does not infer
session lifecycle from WIP activity.
Mutations include the workspace ID and its expected `generation`; stale saves
fail without replacing newer work. The Lua frontend uses this structured API;
provider logic and storage remain in Rust.

`{"op":"pr","base":"master"}` creates/selects the current branch's PR
object and returns the same batch/session routing. Workspace views expose its
canonical `pr`: logical `id`, metadata `revision`, `title`, `body`, and tagged
`source` (`local_intent` with `branches`, or `remote` with `locator` and optional
`branches`). Each branch has `repository` and `branch`. The immutable review
snapshot retains its own PR metadata and comparison.

Save intent metadata through the ordinary action list using
`{"action":"pr_details","revision":0,"title":"Title","body":"Body"}`.
Its revision is the PR object's revision, alongside the request's workspace
generation. Metadata and its history commit together; stale or already-remote
PR edits fail without replacing newer text. `publish` resolves local PR intent before executing the saved publication.
These CLI operations return local state with publication receipts. The Neovim
plugin explicitly chains a subsequent fetch and reconciliation.

`{"op":"log","workspace":"BATCH-ID"}` returns `log` entries newest first.
Each has `generation`, `summary`, `details` (display lines), `level`
(`info`/`success`/`error`) and `raw` (complete recorded JSON as display lines).

Local actions accept an ordered list, including a one-element list for ordinary
saves. For example:

```json
{"op":"actions","workspace":"BATCH-ID","generation":3,"actions":[{"action":"reaction","comment":"COMMENT-ID","present":true},{"action":"viewed","path":"src/main.rs","viewed":true}]}
```

The response contains one final `workspace` and an `applied` count. Each changed
action retains its own generation and history entry. A domain validation failure
commits the successful prefix and returns `error` and zero-based `failed_index`;
clients apply the returned workspace before reporting the error. Publication and
provider journal actions are excluded from this list. Malformed lists are
rejected before applying any action.
`undo` and `delete_comments` accept `comments` arrays. Rust preflights each
selection and orders draft children before parents; execution failures retain
successful children and stop the rest. Each changed child has a journal entry.

Successful state responses contain `workspace`. Read/publication failures that
also need to return updated state contain both `workspace` and `error`;
clients must apply that generation before retry. Invalid requests and stale
generations exit nonzero with an error on stderr. Snapshot and batch inspection
commands below remain available independently. Local mutation input is always an
`actions` array, including for one action; scalar mutation operations are rejected.

Routing that fetched a new snapshot can return `fresh_review`. Pass it to
`select`; the backend returns `fresh:true` only when it matches the selected
snapshot. This skips a duplicate first refresh. Cached selections still refresh;
later refreshes run in the background. Hidden readers rebuild when displayed,
so local actions redraw only visible review buffers.

Selection runs `{"op":"fetch_objects","workspace":"BATCH-ID"}` before showing
review objects. Rust prepares checked-out `HEAD`, its review base and latest fetched PR head
first, then batch-checks all original comment commits in Git's local object
database. Any remaining missing hashes fetch together. Already-local commits need
no network, including
commits outside current branch history. Later refreshes prepare newly discovered
hashes while the existing view remains usable. Unavailable historical hashes
produce explicit warnings and retain the recorded-hunk fallback; unavailable
current comparison commits produce an error. Git-object preparation never changes the current branch,
worktree, index, refs or FETCH_HEAD.

Git stores file contents; SQLite retains snapshots, anchors and publication
receipts. Confirmed receipts restore our comments' exact original comparisons
on later imports. Historical views read Git objects directly; after local pruning,
preparation attempts to fetch missing objects again. No duplicate SQLite blob
cache or background file-caching job is used.

A refresh response with `refresh_deferred:true` preserves the current workspace
when publication or a newer refresh makes its fetched snapshot obsolete. Clients
must retain the previous freshness indicator and retry explicitly. Composer
saves retry once after a cache read only when their saved target/content remain
unchanged; concurrent edits still fail without replacing text.

## Fetch

With `gh` installed and authenticated, run from your project directory:

```sh
cargo run --bin pr -- fetch https://github.com/OWNER/REPO/pull/NUMBER
```

Or install with `cargo install --path .` and run `pr fetch <PR URL>`. The command
accepts github.com PR URLs and prints the selected local `ReviewId` on stdout. It saves
the fetched snapshot in the current directory's `.review/state.sqlite3` without
creating a batch. An unchanged fetch reuses the matching immutable snapshot;
changed remote state becomes the new baseline while local intent stays separate.

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

The low-level commands below use the current directory's `.review/state.sqlite3`.
Review and batch
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

Save local changes through the same action-list API used by Neovim. A new batch
starts at generation zero:

```sh
pr workspace <<JSON
{"op":"actions","workspace":"$batch","generation":0,"actions":[{"action":"save","body":"Review comment","target":{"kind":"pull_request"}}]}
JSON
pr state "$batch"
pr plan "$batch"
```

Use the generation returned by the latest response when publishing:

```sh
pr workspace <<JSON
{"op":"publish","workspace":"$batch","generation":1}
JSON
```

`publish` is the sole provider-write operation. It sends saved operations in dependency order;
an optional `{"action":"publish_review","conclusion":{"outcome":"comment","body":"Summary"}}`
action concludes the review. Setting `conclusion` to `null` cancels the unsent
conclusion. Without that action, comments publish as a draft
review. `{"action":"move","comments":["local-id"]}` moves selected unsent
inline comments into the review.

| Command | Output |
| --- | --- |
| `pr fetch <url>` | New review ID, plain text |
| `pr reviews` | JSON array of review IDs |
| `pr show <review-id>` | JSON review, including reviewer and fetched snapshot |
| `pr batch new <review-id>` | New batch ID, plain text |
| `pr batch list <review-id>` | JSON array of batch IDs |
| `pr batch open <batch-id>` | JSON batch and its complete ordered event log |
| `pr state <batch-id>` | JSON projection at Git HEAD, including local events |
| `pr plan <batch-id>` | JSON pending operations at Git HEAD, accounting for saved results |
| `pr diff <review-id> [--base <commit>] [--head <commit>] [--path <path>]` | Local Git diff |
| `pr workspace` | Structured JSON response; reads one request from stdin |

Invalid requests go to stderr with a nonzero exit status. Local action failures
return the final saved workspace, successful-prefix count and failing index.
Fetched replies remain independent resources when their root is deleted, while
new replies require a live diff root. Delete unpublished replies before their
parent; action order is preserved. Provider constraints are checked before
publication.

Diffs use two local commit objects, defaulting to the fetched base/head. Both
overrides are independent. The command does not fetch missing commits or change
the working tree. Paths are literal, and external diff/text conversion commands
are disabled.

### Local actions

`src/workspace.rs` defines actions. Rust assigns comment identities and ownership.
For example, create a PR-wide draft:

```json
{
  "action": "save",
  "body": "Review comment",
  "target": {"kind": "pull_request"}
}
```

The saved comment ID targets later actions:

```json
[
  {"action":"save","comment":"COMMENT-ID","body":"Revised text"},
  {"action":"delete","comment":"COMMENT-ID"},
  {"action":"restore","comment":"COMMENT-ID"},
  {"action":"reaction","comment":"FETCHED-COMMENT-ID","present":true},
  {"action":"viewed","path":"src/lib.rs","viewed":true}
]
```

Put actions in the request's `actions` array. `restore` restores an unsent
deletion. Progress uses your thumbs-up on fetched comments; use
`present:false` to reopen one and `viewed:false` to mark a file unviewed. `start`
activates a WIP review; `summary` saves its conclusion, with outcome `comment`,
`approve` or `request_changes`. The action log remains available through `log`.

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
PR-wide comments have no replies in the core specification.

## Publication and retry

The core projects the entire event log into intended state. At publication it
compares that state with the fetched snapshot plus successful operation results.
The resulting `CommitState` contains only net operations. A create followed by a
delete produces no server write; toggles back to their baseline cancel too.
The original events remain inspectable.

Each confirmed operation saves its own receipt, returned identities and response,
including successes from a partially failed batch. Successfully sent content is immutable,
including pending review comments. Corrections are new comments or replies.
Unsent create/edit/delete changes coalesce; deleting an unsent comment cancels
its creation. Provider mappings preserve local identities across refreshes.

Publication follows dependencies, sending one batch per operation family at each
dependency depth. Independent comments share a request; replies wait until their
new parents have confirmed identities. Viewed flags and reactions are batched
by family, and a queued conclusion follows the other operations. One publication
uses at most one provider review: it reuses the authenticated reviewer's pending
review for that PR and head, or creates one when missing. A later publication may
extend or conclude that pending review without recreating confirmed comments.

Batch writes use GraphQL aliases in one HTTP request. GitHub executes those
fields serially, without atomicity or rollback: some may succeed while others
fail. All confirmed results are saved, then any error, blocked operation or
uncertain result stops later batches. Retry retains the publication ID and
confirmed receipts; confirmed operations never resend. New unrelated work waits
for another publication. Completed publications remain immutable.

Publishing verifies the reviewer/account and known constraints before writing.
Only unsent comments can be edited, deleted, moved or relocated. GitHub validates
exact saved head/path/range anchors; the adapter never substitutes a location.
Nonblank text is required for Comment and Request changes conclusions; Approve
may omit it. Reusing an existing pending review never deletes its earlier comments
or treats them as successful receipts for new local operations.

Publication holds `.review/publish.lock`; local saves remain available outside
the frozen scope. Remote writes and SQLite cannot form one transaction. Lost
responses remain explicitly uncertain. Unknown creates stay blocked until their
provider identities are confirmed; they never blindly resend. Unknown viewed or
reaction operations are reconciled together with one fresh fetch before retry
decides which operations remain. Later batches stay blocked while results are
unresolved.

## Model

Each local `ReviewId` owns an immutable `ReadState`, fetched or built from local
committed changes. A globally unique `BatchId`
identifies an editable `WriteState` and links it to one review. Multiple batches
can share a review's fetched baseline. A `PullRequestId` identifies the PR;
multiple local reviews can concern the same PR.
The canonical PR object exists before its reviews and can start as a local
intent. Binding it to GitHub retains its logical ID and all referencing batches.
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
Comment creation order follows the persisted event sequence through projection,
publication and refresh; UUIDs remain identities rather than ordering keys.
The GitHub adapter uses `gh` for API access and authentication. Dismissal is a
standard core operation; no backend capability framework has been added.

The CLI selects an existing batch ID or creates a new batch. Batches have no
completed status: users can append new local intent to the same batch. Each
completed publication remains immutable; later publication gets a new ID.

## Storage

`Store::open(project_root)` creates or opens `.review/state.sqlite3`. SQLite keeps
review metadata, fetched snapshots, batches and individual sequenced events in
separate tables. The `batches` table links `batch_id` to `review_id`; event rows
refer only to `batch_id`. Snapshots and event payloads use Serde JSON; SQLite owns
transactions and event ordering. `src/schema.sql` defines the current schema.

Fetched payloads are shared rather than copied into every workspace generation.
Workspace records reference their review, event prefix and successful receipts;
history stores field changes and reconstructs prior generations when needed.
An ordinary progress mark therefore records local changes without another copy
of the PR. Identical fetches reuse evidence only when reviewer and provider
identities also match. Explicitly imported review IDs remain valid.

Only the current database format is supported. Opening an incompatible database
returns an error without converting or deleting it.

`review_sources` and `id_mappings` hold backend metadata separately.
`Store::import_review` saves a snapshot and its source/mappings atomically;
`Store::review_backend` retrieves that metadata. Read snapshots and write events
stay in their own tables.

`operation_results` stores successful processed writes separately, keyed by
batch and event UUID and ordered by execution sequence. A row
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


## Checks

Run `cargo test`, `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.
Rust 1.89 or newer is required. The existing Nix development shell supplies the
Rust tools and Python for fixture processes. Neovim comes from your existing
PATH. Run the standalone UI
workflows with:

```sh
nvim --headless -u NONE -c 'lua loadfile("tests/nvim/run.lua")("tests/nvim/workflows.lua")'
```

The test driver yields to the real event loop so native command boundaries and
`SafeState` are exercised.
`cargo test` also runs a headless Neovim workflow against the real cached CLI.
GitHub integration tests use fixture `gh` executables; they do not write to a
live PR.
