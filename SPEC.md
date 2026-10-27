# Neovim review integration specification

This is the design for the initial Neovim integration. It preserves our agreed
UX decisions and settles the remaining details with the defaults below.
It specifies intended behavior, not features already implemented. See
[README.md](README.md) for the current Rust CLI and core.

## Scope and interaction model

Use Fugitive's small action vocabulary and Oil's ordinary-buffer approach:
normal motions, search, registers, folds, splits and command completion.
The default plugin keys are contextual `<CR>`, diff/reader append `a`,
progress/reversal `s`/`u`, home `cc`, and
Vim deletion operators on comment objects in home/thread lists/discussions.
Every other action has a named command; progress and navigation also expose
`<Plug>` mappings for user composition.

The integration works with existing PRs and local PR intents in the current repository,
using GitHub through the Rust adapter. It supports reviewing and fixing feedback
in the same workspace. Source editing, file search, commits and pushes use the
user's ordinary editor and Git workflow. Local PR intents can create draft PRs
during publication. The plugin does not introduce a source-file editing workflow. Explicit PR checkout is available;
browsing never switches branches automatically.

Full submitted-review browsing, historical checklists and review dismissal UI
are outside the initial integration. Opening an old comment's original code
context is included. Existing core capabilities remain available through the CLI.

Require Neovim 0.10 or newer, the matching `pr` executable, Git and authenticated
`gh` on PATH. Loading the plugin defines `:Review`; no setup call or required
Lua dependency is needed. Report a missing executable/authentication with the
failing dependency identified. Use standard [Neovim process APIs](https://raw.githubusercontent.com/neovim/neovim/v0.10.0/runtime/doc/news.txt)
with argument arrays, not interpolated shell commands.

## Workspace and entry

A session is a batch with one canonical stable `BatchId`, scoped to repository,
provider PR and authenticated reviewer. It contains a pinned comparison,
fetched state, pending changes and at most one unsubmitted WIP review. Refresh
and publication preserve its identity. Several sessions may belong to one PR.
The WIP may be active or left; activity controls new inline comment defaults,
not whether the session is unfinished. Rust owns lifecycle and routing rules.

From ordinary source buffers, `:Review` resolves open PRs for the current branch:
exactly one routes to that PR; zero or multiple matches show the remote PR list.
Inside a bound review buffer, `:Review` returns to that session's home.
`:Review prs` always shows the remote PR picker. Selecting a PR creates a new
session when none are unfinished, resumes the only unfinished session, or opens
the saved sessions browser filtered to that PR when several are unfinished.

`:Review sessions` uses the same browser without a PR filter, grouped by PR with
the current PR first. Rows show publication status, reviewer, pending comments
and other changes, published counts and conflicts. Enter explicitly resumes the
selected stable session ID. `:Review sessions new` creates another session for
the PR under the browser cursor, selected remote PR, or bound session's PR.
`start` continues to control WIP activity within one session.

The first home load in a Neovim session shows a loading view until remote
comments, reactions and file progress have been fetched. Review actions wait
while Neovim remains responsive. PR selection also waits for Git preparation.
Later remote refreshes retain cached data
and run in the background. Fetch failures preserve cached work with a visible
error. Explicit navigation is serialized; background responses update only
their original session and never select it or retarget another session's UI.

The PR list initially shows open PRs across all authors and assignees in the
current repository. Rows show number, title, author and assignees, sorted by
most recently updated, then PR number. `/` searches visible rows. The optional
`assignee=LOGIN`, `assignee=@me`, or `assignee=all` argument filters the remote
query. The optional `state=open|closed|merged|all` argument defaults to open;
this also makes saved closed or merged PRs reachable through the list. No
assignee argument means all assignees. The header shows the applied filters.
`:Review refresh` in the PR list refreshes that query and retains its filters;
in the sessions browser it rereads local state and preserves its PR filter;
elsewhere it refreshes the bound session. Listing failure preserves the
previous rows and shows the error.

`:Review` opens in the current window and tab, without creating or locating a
dedicated Review tab. Each repository has one active view; invoking `:Review`
from another tab moves that view there. Unrelated user splits and unsaved source
buffers are preserved. Home, diff and discussion views reuse the active tab;
original code context uses a separate, explicitly opened tab. Normal Vim
window/tab commands remain available.

Review buffers carry their own repository, PR, session and object identities.
Commands from them target that session. Plain `:Review` from source follows
current-branch routing; other bound actions retain their own workspace identity.
Missing context produces a
clear message, never a guessed PR, comment or line. Switching the active PR
never retargets an already-open composer.

Persist selected stable batch ID, pinned comparison, WIP contents and activity,
saved summary/outcome, draft destinations and pending changes in the existing
`.review/state.sqlite3` at the repository working-tree root. Keep state separate
for authenticated reviewers; publishing always verifies the saved reviewer
against the current account. Unsaved buffer text has ordinary Neovim modified
buffer behavior and is not promised to survive editor termination.

## PR objects and local entry

A canonical PR object has a stable local `PullRequestId`, a metadata revision,
title/body and a source: `LocalIntent { branches }` or
`Remote { locator, branches }`. Each branch identifies repository and branch
name; the pair survives binding to a remote PR. Migrated remote records may
lack branch metadata until fetched. Reviews and batches retain their existing
identities and reference this PR object. Rust/SQLite own object persistence,
session routing, metadata and publication; Neovim owns the command and composer.

`:Review pr BASE` selects the current source branch and the named target branch.
Read GitHub for an open PR matching both repositories and branches. Reuse it
immediately when present; otherwise create or reuse an unbound local intent
before starting a review. Apply the ordinary create/resume/choose session rules.
Plain `:Review` routing remains unchanged. Detached HEAD, ambiguous push targets
or missing committed refs produce an actionable error rather than a guessed pair.

Local snapshots compare committed branch changes against the selected base's
merge base. The index and working tree never enter the diff. Refresh reads the
saved source branch, even after another branch has been checked out. As with
remote reviews, refresh preserves comparison pins; compare-latest advances them
explicitly. Local Git preparation needs no provider review metadata.

Local home identifies its intent and creation-pending state. Enter on the title
opens an ordinary Markdown composer: first line title, remaining text body.
The initial description is empty. `:w` and `:wq` save locally; neither creates a
remote PR. Metadata edits use the canonical object's revision to reject stale
cross-session saves, preserving unsaved composer text. Existing remote PR
descriptions remain unchanged. Local sessions are grouped by their PR object
with source/target labels, and have no remote checkout action.

Before publishing dependent comments or progress, resolve the intent under the
existing publication lock. Recheck the exact open branch pair: reuse its PR or
create a draft PR with the saved title/body. Require a nonblank creation title
and already-pushed reviewed commits; report an ordinary Git/Fugitive push
instruction rather than pushing. Persist frozen creation input before the
request and remote binding before publishing dependent operations. PR-only
publication also resolves the intent. The ordinary standalone/WIP destination
rules continue to apply afterward.

Keep logical PR, review, batch and comment IDs plus comparison pins unchanged.
Creation/binding survives comment failures and restart. Reconcile unknown
creation results by exact-pair lookup; an inconclusive result cannot blindly
repeat creation. A create race reuses the winner. If the remote PR is already
bound to a different logical PR object, block with a clear error and retain both
objects and their saved sessions/drafts; no automatic identity merge.

## Explicit checkout

Home shows local branch, relationship to the latest fetched PR head, and staged,
modified and untracked file counts. Refresh this status asynchronously on home
entry, editor focus and remote refresh. Enter on its checkout row or
`:Review checkout` requests a switch; PR visits never prompt or mutate source.

Lua owns local Git orchestration and editor guards. Rust exposes optional remote
PR source repository/branch metadata in its existing snapshot; SQLite stores no
checkout or stash lifecycle state. Git remains authoritative for branches and stashes.

Block switching while any loaded source buffer in the repository has unsaved
text, including hidden buffers, or Git reports conflicts or an unfinished
merge/rebase/cherry-pick/revert. Clean checkouts switch immediately. Dirty disk
state offers `Cancel` first, `Stash & switch`, and `Carry changes & switch` through
`vim.ui.select`. Recheck guards after choosing. Carry uses Git's ordinary refusal
to overwrite conflicting edits. Never force, reset or rebase local commits.

Stash preserves staged, unstaged and untracked files in an ordinary descriptive
named Git stash, excluding ignored files and reserved `.review/` state. Retain
and report the stash even if checkout fails. Restore through ordinary Git or
Fugitive; no managed return command or stash registry. Reload only unmodified
source buffers after Git completes, preserving their editor options and any
new unsaved text. Switching leaves review sessions and comparison pins intact.
Worktree management and automatic checkout are outside this pass.

## Review home

The header shows the PR, reviewer, compared revisions, freshness, and whether
a WIP review is active. Session publication status, pending/published counts
and a Publication history section show saved plans and their results. Enter
on a history row opens operation bodies, confirmed results and errors read-only.
A left WIP remains visibly resumable. New commits and
publication/conflict errors appear here without changing the comparison.

| Section, in order | Contents |
| --- | --- |
| Todo | Fetched remote comments/replies without the current user's thumbs-up |
| Unpublished changes | Saved drafts, edits/deletes, conclusion and pending progress |
| Unviewed files | Compared files not marked viewed |
| Viewed files | Compared files marked viewed |
| Done | Fetched remote comments/replies with the current user's thumbs-up |

The compact labeled header abbreviates compared hashes. Theme highlight groups
distinguish headings, counts, paths, authors and statuses. Sections use native
folds without visible markers and show counts, including zero. Each comment
shows path, location, author and status above an indented body preview. Both
lines select the same object and remain searchable with normal Vim motions.
Unpublished comments show Review or Standalone destination. Conflict, pending
deletion and rejected-anchor labels are explicit. `<CR>` on a saved draft opens
its editor; on a conflict it opens resolution; on a file it opens the review
diff. A pending progress row opens its associated comment or file.

Viewed/unviewed file rows show `M +12 -3  path`, with `A` for added and `D`
for deleted files. Renames, copies and type changes use `M`. Color the status,
added count and removed count separately using theme highlight groups. Rust
retains additions/deletions from the existing GitHub file fetch with each file
in the pinned comparison. Refresh must not substitute counts from a newer PR
head while the comparison is pinned. Missing cached counts display `?` rather
than zero. Row actions retain the original file path as their object identity.

Todo is ordered by file path, thread location, then conversation order.
Use the current anchor only when it matches the pinned comparison; otherwise
use the recorded original anchor. At one path, order base before head, then
starting line, ending line and thread creation order. Stable provider identity
breaks remaining ties. Replies follow their root's location and retain
conversation creation order. Comments without code locations appear last, in
creation order with identity as the tie-breaker. File rows use path order within
their viewed/unviewed sections. Folding does not alter logical navigation order.

Local comment creation order comes from the persisted event sequence, including
after refresh and restart. Publication preserves that sequence with parents
before replies, while edits and cancelled drafts remain collapsed. UUIDs identify
comments and operations; they do not determine comment chronology.

## Immutable code and discussion

The diff uses immutable `review://` buffers for exact base and head revisions,
Neovim's native diff display, and native hunk motions. Review buffers stay
read-only in every window; ordinary source buffers are separate. Show comment
counts on relevant lines on either side. Local source edits do not change
compared code or saved comment anchors.

Current-comparison Base/Head diff winbars show `[viewed]` or `[unviewed]` beside
the file path, preserving their revision/path labels. Derive this marker from
projected local progress: both panes update after `s`/`u` without publishing.
Historical winbars retain their existing labels.

Opening a discussion uses one reusable split below both code panes:

```text
Base revision       | Reviewed head
--------------------+--------------------
Thread list / thread / Markdown composer
```

Before any file is open, use home above the discussion/conclusion composer.
Summary-only reviews never require choosing an arbitrary file. Returning home
or opening a file reuses this review layout without accumulating windows.
The latest local navigation request wins: older loads cannot replace a newer
file, comment or home choice. After successfully opening a file, close the
managed discussion if anchored to another file. Keep same-file and PR-wide
discussions, and any additional user-opened windows.

The thread list, thread and composer reuse this bottom window. Opening another
object never silently saves or discards a modified composer. Refuse replacement
with a normal modified-buffer message until the user saves or explicitly
abandons it. Apply the same protection to PR switches and comparison changes.
Read-only views can be replaced freely. Saved comments remain available after
closing or replacing their editor.

Thread lists contain styled entries without usage banners. Thread discussions
use themed location, author and status headers, original Markdown bodies, and
dividers only between comments, never after the last comment. Spacing and
dividers carry no comment action. Both readers retain final blank spacing,
without a trailing append divider. `a` works anywhere in either reader,
including the end. Usage belongs in the help page.

When no comments remain, close the managed thread selector or discussion and
return focus to its origin. Pending-deletion comments remain visible for undo
and do not make the reader empty.

### Contextual Enter

| Context | `<CR>` |
| --- | --- |
| PR list row | Activate that PR |
| File row | Open its immutable review diff |
| Todo/Done comment row | Open selected discussion and matching code location |
| Diff line with no threads | No action |
| Diff line with one or several threads | Open the location's thread list; Enter selects an existing thread |
| Thread list entry | Open that thread |
| Visual selection in diff | No plugin mapping; visual `a` starts a range comment |
| Own comment in a thread | Edit that comment |
| Another user's or unknown author's comment or reply | Remain read-only; use `a` to reply to the root |

Thread lists identify author, body preview and range, using the same location
and conversation ordering as home. Enter on a heading or empty area does
nothing. Unknown authorship never grants editing permission; open it read-only
and allow explicit reply where the target supports replies. Editing/deletion
requires verified ownership and provider permission.

On a diff line, normal `a` starts a new root comment at that captured code
location, even when threads already exist. In a location's thread list, `a`
starts a new root at its captured location. In a conversation, `a` replies to
the root thread, including when its author is the current user. It does not depend on the row
under the cursor. Native `a` in composers and source files is unchanged.
Existing explicit `:Review comment` and `:Review reply` remain available.

GitHub diff replies belong to the root thread. Replying with `a` from any reply
still targets that thread; do not pretend the provider supports nested replies.
PR-wide discussion comments and review summaries have no diff-thread reply
action in this version. Their bodies remain readable/editable when permitted;
unsupported reply attempts explain that a standalone PR-wide comment can be
created instead.
The [GitHub comment API](https://docs.github.com/en/rest/pulls/comments#create-a-reply-for-a-review-comment)
requires root-thread targets for replies.

`:Review comment` starts another comment even on a line with existing threads.
Characterwise and linewise visual selections anchor inclusive whole touched
lines. Reject blockwise selections with a clear message. A selection stays on
one diff side; never infer columns or a cross-pane range. File-level comments
can be created explicitly from a file row, including files without text hunks.

## Comment editing, suggestions and deletion

All comment composers are ordinary Markdown buffers, with no protected prefix
or special editable footer. Their read-only virtual context identifies target,
revision, side/range and publication destination; metadata is
not editable prose. A destination chosen for a draft stays fixed. Changing WIP
activity does not move existing drafts.

| Operation | Result |
| --- | --- |
| First `:w` on new nonblank text | Save one local draft with its identity, target and destination |
| Later `:w` | Save body edit to the same identity |
| Save unchanged body | No new body event |
| Save blank new draft | Create nothing |
| Save blank saved pending comment | Delete the local draft; no provider creation remains |
| Edit/delete a successfully sent or fetched comment | Reject; published content is immutable |
| Undo or type nonblank text, then `:w`, while deletion is unsent | Restore the same comment identity and save its body |
| `:wq` | Save locally, then return to the originating reader or context if save succeeds |
| `:q` | Ordinary modified-buffer rules; no publication |
| `:q!` | Discard unsaved edits; earlier saves remain |

All ordinary comment composers, including explicit edit/reply commands, return
to their originating reader or context after `:wq`. Reused reader panes restore
their immutable list/thread. From home or diff, the ordinary discussion composer
keeps that context visible and closes back to it.
Direct diff comments restore the originating pane and both diff panes' saved
cursor and scroll positions when the composer closes.

A successful save persists body and associated draft metadata atomically.
Whitespace-only text counts as blank. Blank saves use the same ownership,
dependency, conflict and publication restrictions as explicit deletion.
Successfully sent comments and conclusions are immutable, including pending
provider drafts and creations confirmed before refetch. Corrections use new
comments or replies. An unsent conclusion retains its blank-body validation.
Repeated blank writes after canceling an unsent comment add no events.
Errors keep the buffer modified and visible. No comment-editor close action
publishes. Saved event history is append-only and inspectable through
`:Review log`; editing changes the projection, not prior event records.
The log viewer is a read-only timeline with newest generations first. Rust
prepares readable action summaries and full details; Neovim renders native folds
and highlights errors. Enter toggles the event or nested recorded-payload fold;
ordinary fold commands work. Preserve every journal entry and its complete
original JSON, including provider bookkeeping and unknown actions. Use existing
generation IDs for order, without inferring timestamps or changing stored history.

Reader actions share one ordered queue per session. Capture stable object IDs
without rebuilding the reader while Vim executes a command, mapping, operator
or macro. Rust applies queued action lists sequentially, including contextual
undo and grouped deletion against current state; ownership/dependency preflight
covers each selection, and execution failures retain the successful prefix.
Defer reader rebuilds through native state()/SafeState boundaries. Reads,
composer capture/save and session switching flush prior local writes; native
BufWriteCmd completes the current local write before returning. Complete native
append/type/save/return sequences preserve the original reader and anchors.
Flush pending local writes on editor exit without initiating publication.
Native motions, counts, visual selections, macros, global/vglobal and ranged
normal use the same path. normal! retains native mapping bypass. Provider
publication is explicit and waits for its writes and subsequent PR refresh.

`:Review suggest` inserts a fenced Markdown `suggestion` block at the composer
cursor, pre-filled with the whole selected lines from the immutable revision.
It is available for line/range-anchored root comments with available source.
The user edits the proposed replacement and can add prose in the same buffer.
Replies, PR-wide comments and unavailable source receive a clear unsupported
context message. Suggestions are ordinary comment body text, with the same
save and publication lifecycle; no separate suggestion editor or domain subtype.

`:Review delete` cancels the selected unsent local comment. Sent or fetched
comments cannot be edited or deleted. Reject deleting a parent with unpublished
replies; list those dependents without silently cascading deletion. Unsent
create/edit/delete changes coalesce within the publication boundary.

In home, thread lists and discussions, `dd`, `d{motion}`, `D`, `x`, and
visual `d`/`x` delete every touched comment once. Native Vim motions and visual
selection determine the touched rows; headings, files, dividers and spacing
have no comment action. Already pending deletions are inert. Preflight ownership,
summary restrictions and unpublished dependents for the whole selection before
staging anything. Selected replies are deleted before selected parents; never
include an unselected reply implicitly. Requests run sequentially and stop on
the first backend failure. Cursor position is retained, clamping at buffer end.
These mappings do not affect composers or diff/source buffers.
Empty normal-mode motions are no-ops: `d0`/`dh` at column zero, `d^` at the
first nonblank character, and `D`/`d$` on an empty body line do not stage deletion.
Visual selections that include a newline and linewise deletion still act on
that comment. Thread lists and discussions have a blank top margin so Vim's
clamping of an empty range at the start of a buffer cannot select a comment.

Normal and visual `u` restore pending-deletion comments in these same views,
cancel new unpublished comments or replies, and otherwise retain their
Todo/unviewed behavior. Each selected object is handled once. Canceling drafts
uses the same ownership and unpublished-reply dependency checks as deletion;
selected draft replies are canceled before selected parents. Restoration keeps
the publication restrictions of `:Review restore`. Explicit `:Review todo` and
`:Review unviewed` retain their progress-only behavior.

`:Review restore` remains available for legacy unsent deletion intent; it does
not recreate a remotely deleted comment or change successfully sent content.

## Publication membership and conclusion

All new inline roots, file comments and replies join the publication's review.
No separate local review identity is needed: the publication identifies the
attempt, while receipts retain GitHub's provider review identity. Immutable
fetched snapshot `ReviewId` values are a separate concept.

Legacy unsent standalone inline comments are detected by their destination.
`:Review move` explicitly moves the current comment or a range/visual selection
into the review, preserving identities, text and anchors. Reject sent content
and selections missing dependent unpublished parents or replies. Never move
comments implicitly. New replies to unpublished legacy parents retain their
parent's destination until that selection is explicitly moved.

`:Review start` and `:Review leave` remain compatibility commands; activity does
not change new inline membership. Legacy PR-wide issue comments retain their
saved target. Review-wide prose uses the conclusion composer.

Home `cc` and `:Review conclude` select Comment, Approve or Request changes and
open the saved Markdown summary. Draft removes the unsent conclusion
(`publish_review` with `conclusion: null`), keeping prior local history. The
picker changes no provider state. Saving
queues the unique optional `PublishReview` event (`publish_review` in JSON),
containing outcome and summary. Later unsent saves replace its projected value;
local history remains append-only. The conclusion is ordinary unpublished intent.

Every composer is local: `:w` saves, `:wq` saves and returns, `:x`/`ZZ` use normal
write-and-close behavior, `:q!` discards only unsaved edits. Closing, unloading,
changing windows or leaving Neovim never publishes. Comment and Request changes
require nonblank summary; Approve may omit it. Summary-only reviews work.

## Publication boundaries and retries

Only `:Review publish` / JSON `op: publish` may write to the provider. The former
`submit` operation is rejected. Publishing never asks for a conclusion. Missing
`PublishReview` means draft: send comments in a pending GitHub review. If present,
conclude after all preceding saved comments and ordered operations succeed.

A publication uses at most one GitHub review. Freeze its ordered surviving
operations, with the conclusion terminal. Coalesce only unsent changes; preserve
dependencies and the order of remaining operations. Validate known constraints
and account identity before the first write. A blocked conflict/anchor prevents
publication; never filter it out or bypass it with unrelated work.

Persist a marker before each write and its confirmed result/provider identities
before advancing. Successfully sent content is immutable immediately, even in
an unsubmitted provider review. A failed publication retains its ID, ordered
plan and confirmed prefix; retry sends only unapplied operations. Confirmed
failures permit editing unsent intent within that scope. Unknown results freeze
the affected attempt until reconciliation, blocking later publication writes.
No blind duplicate creation and no skip-ahead after errors.

Adjacent compatible line-root creations use native bulk create-review. A final
conclusion can be included when it immediately follows that group and ends the
queue. Other operations split the group; existing pending reviews, file roots,
replies and progress use supported sequential writes. Arbitrary GraphQL aliases
are not atomic and can continue after individual errors, so they cannot replace
strict stop-on-error sequencing.

Successful draft publication closes that publication while retaining the pending
provider review identity for a later publication to extend or conclude. A
completed publication and its results never reopen; new work gets a new ID.
A provider-side review from another tool is not silently adopted or deleted.
Legacy identities, events, drafts, receipts and recovery data survive migration.

Neovim waits for publication and a fresh subsequent fetch. Drain earlier
background refreshes before writing; an earlier snapshot cannot satisfy this
post-write refresh. Refresh after partial success without retrying writes.
Report refresh failures separately, preserving all receipts and original errors.
Ctrl-C interrupts waiting without canceling an already-started write; keep the
publication guard until its result and refresh complete.

## Individual comment progress and navigation

Checklist membership comes only from fetched remote comments across the PR,
independent of author, submitted-review selection or review membership. Include
inline roots/replies, file comments, PR-wide discussion comments and nonblank
review-summary bodies. An outcome without comment text is not a checklist item.
GitHub [review summaries are comment resources with reactions](https://docs.github.com/en/graphql/reference/pulls#pullrequestreview);
normalizing them is required backend work, not existing core behavior.

Only the current user's thumbs-up counts: absent means Todo, present means Done.
Other users' reactions and provider thread-resolution status do not determine
this checklist. Staged reaction intent updates the display immediately. Local
drafts are excluded. A publication receipt alone never adds membership; the
user's own published comments/replies enter the checklist once fetched too.
Fetched PENDING roots and replies retain stable identities and a `draft` flag,
shown as `[draft]` in readers. They are read-only and excluded from Todo/Done
until submission. Refreshing submitted state updates the flag without duplicate
comments or new local intent. Provider-prepared local copies remain drafts for
this rule, even if refresh sees them after a failed submission. They enter the
checklist only after confirmed submission and a fetch; a private preparation
step must not turn them into Todo or duplicate their local identities.

Completion is per fetched item. A thread is complete only when every fetched
member is Done. A new fetched reply without the user's thumbs-up adds work;
it does not remove the parent's mark. File viewed state is independent.
Pending remote deletion remains labeled until absence is fetched; it does not
silently become Done. Deleted remote items disappear after refresh unless a
local operation needs them retained as a conflict.

Inline diff markers aggregate the fetched comments and replies anchored at each
marker line: `[ ]` when none are Done, `[-]` when some are Done, `[x]` when all
are Done. Use the projected own-thumbs-up state, including staged intent, without
additional provider reads. Nonfetched local comments, including publication
receipts, have a separate neutral count and never affect completion. Draft-only
markers have no completion badge. Default theme links are `BufReviewTodo` to
`DiagnosticInfo`, `BufReviewPartial` to `DiagnosticWarn`, and `BufReviewDone` to
`DiagnosticOk`.

Highlight the active comment's exact referenced line range while its pane is
visible in the current tab. Moving into code retains it; selecting another
comment replaces it. Replies use their root's anchor. Match the bound session,
path, displayed comparison and diff side. Historical views map verified original
ranges to their left pane; never guess current lines or reconstructed base lines.
Skip absent, out-of-bounds, binary and file-only anchors. Hiding/closing the pane
or changing tabs clears the highlight. Combine range extmarks for trailing space
with window-local matches over the same lines so native diff backgrounds do not leave text
unhighlighted. Remove only the plugin's matches during cleanup. Keep inline
markers and native diff state intact. `BufReviewCommentRange` defaults to
`Visual` and is user-overridable.

| Read-only selected object | `s` | `u` |
| --- | --- | --- |
| File row or immutable diff buffer | Mark viewed | Mark unviewed |
| Fetched comment in list/thread | Add own thumbs-up; Done | Remove own thumbs-up; Todo |
| Pending-deletion comment | Unavailable | Restore unsent deletion |
| New unpublished comment or reply | Unavailable | Cancel creation |

Visual progress acts once per intersected object, ignores headings, and never
marks an entire thread through its header. Reject progress on local drafts.
In editable comments and ordinary source buffers, keep native bindings.
Named viewed/unviewed actions also work from within the file's review diff.

Marking preserves cursor row/column and scroll position. When an item changes
section, the cursor stays in place rather than following it; clamp only at the
buffer end. Navigation uses the backend's stable sequence and the current object
identified by the focused buffer and cursor. Home file/comment rows supply the
corresponding current object. Never recover context from a previously selected
object, last path, active pane or saved continuation. Plain `s` creates no
continuation; subsequent navigation uses the object then under the cursor.

Next searches strictly after current, then wraps through the prefix before it;
previous searches strictly before current, then wraps through the suffix after
it. Each step skips excluded objects and never returns current on wrap. Normal
counts repeat this search from each result, loading only the final destination;
a complete cycle may return to the original object. With no current
object, start at the first/last eligible object. With no eligible destination,
give a notice without moving. File navigation excludes viewed files; comment
and Todo navigation exclude Done comments. Use projected progress, including
staged unpublished viewed flags and own thumbs-up changes. Changing progress
does not reorder the stable sequence. Navigation never changes progress.

`:Review next|previous todo`, `:Review next|previous comment` and
`:Review next|previous file` use this filtered wrapping search. Review-local
`]f`/`[f` open the next/previous unviewed file; `]r`/`[r` open the next/previous
Todo comment and its code. Include each root and reply independently, including
unfinished replies to Done roots. From code without a current comment, `]r`
opens the first eligible comment at the current diff line; `[r` opens the last.
Without a matching location, use the first/last global Todo. Subsequent presses
in a discussion search from the focused comment in global order, including
across threads on the same line. Other visible panes never supply current
objects. Unsaved composers block navigation. A newer local navigation request
cancels an older load even while its view is still loading.

Keep marking separate: `]f`, inspect, `s`, `]f` for files; `]r`, inspect, `s`,
`]r` for comments. Contextual `s` stages viewed/Done through the existing
action-list API, preserving cursor and scroll position without navigation.
Historical code cannot mark a current file viewed; mark the comment Done from
its discussion instead. Native `[c`/`]c` remain diff-hunk navigation.

Reuse stable numbered reader buffers by review and reader kind/side. Navigation
replaces the contents and resets target metadata, highlights and filetype together.
Do not retarget a reader displayed outside its managed pane, including other tabs;
use another slot instead. Such copies retain their original target and continue
normal refreshes for that target. Composers keep private buffers, and their return
readers cannot be reassigned until the composer closes.
Unregister outgoing managed diff panes before replacing or closing them. Hidden
reader slots must not accumulate native diff membership, and unrelated user diffs
remain untouched. Register only the new displayed pair.
Use native `keepjumps` for internal buffer switches and pane creation, including
composer returns and original-context tabs. Preserve existing jump history;
ordinary user motions retain their native jump behavior.

## Refresh, conflicts and comparison updates

`:Review refresh` fetches a new immutable snapshot of remote comments, reactions
and metadata. It does not change the pinned comparison. Match resources by typed
provider identities, including receipt mappings for previously local drafts;
there must be only one logical item after publication and refetch.

Reconcile saved intent against the previously observed and newly fetched
versions. Preserve nonconflicting changes; if the remote already matches local
intent, no duplicate write is needed. A body edited differently on both sides,
a remote body change under a local deletion, or a remote deletion under a local
edit/reply is a conflict. Progress retains the user's explicitly staged desired
state over fetched reactions/viewed flags.
Refresh does not overwrite modified editor text; after saving, check its captured
base version against the latest remote state before publication.

A conflict row opens a read-only Markdown conflict view in the existing
discussion window, with Local, Remote and Previously observed sections. It
preserves the code panes and does not add discussion windows. The header
identifies the affected object and choices. `:Review resolve local` retains local
intent against the displayed remote version; `remote` discards that conflicting
local intent; `edit` opens the local Markdown composer and saving resolves with
its resulting body. Choices are local until publication. Preserve original
snapshot/event history and recheck if the remote changes again.

For a deletion conflict, local confirms the deletion against the displayed
version and remote cancels it; edit is unavailable because this is not a body
edit operation.

If the remote target was deleted, keep local text readable but reject both local
and edit resolution against a nonexistent object. Use remote to abandon the
stale operation, or explicitly create a new comment with the retained text and
a valid target.
Do not resurrect remote content automatically. A dependent reply cannot publish
until its missing/blocked parent is addressed.

`:Review compare latest` explicitly pins the latest fetched base/head pair,
updates immutable diff buffers and shows old/new revision IDs. It fetches remote
metadata if needed, but never changes local source, branch or worktree. Missing
local commits for the selected comparison are fetched from the PR repository,
using the exact pinned base/head IDs. PR selection prepares that history, latest
fetched PR head and original comment commits before displaying review objects;
existing objects skip the network. Later refreshes prepare new hashes while local staging stays
available. Preserve the index, refs and FETCH_HEAD;
never check out the PR during object preparation or substitute newer revisions when a pin is unavailable.
Show fetch progress or an actionable failure, and retry on opening code or an
explicit refresh. Saved drafts retain exact original anchors. They remain
publishable if the provider accepts them, even when historical. Flag rejected
anchors and require explicit relocation or deletion before submission.

To relocate an unpublished root, select the desired cursor line or visual range
in the current diff, then run `:Review relocate [draft-id]`. If omitted, choose
from that PR's unpublished inline/file roots with `vim.ui.select`. Show old and
proposed anchors in its composer; `:w` explicitly saves the new anchor under the
same logical draft identity. Unsaved relocation is discarded with `:q!` just
like unsaved text. Dependent replies retain their parent relationship. Published
comments cannot relocate. No refresh, rename or comparison update guesses a
replacement anchor.

## Original code context

Opening a current comment follows its file in both pinned diff panes. Opening
an outdated/unmatched comment automatically replaces those same panes with the
referenced historical file on the left and its latest fetched PR-head version on
the right, retaining the thread below. Resolve renames between those versions;
if the current file is absent, show an empty right pane labelled absent.
Show Historical / PR head and both commit SHAs in the winbars. Original head-side
comments use their original head file; original base-side comments use their
recorded base file and path. When the original base is unknown, use the merge-base
of the target and original head, explicitly labelled Reconstructed. Only
jump/highlight verified original ranges, mapped to the left pane. Never guess
reconstructed base-side lines. Latest PR head is independent of the session pin
and local checkout; changes during loading must not display a stale pair.
Unavailable history displays its recorded hunk in a read-only reference pane above
the thread.
Current-comment/file navigation restores the pinned diff. These views do not
change the workspace's pinned comparison. Historical code supports reading and
navigation; replies/Done stay in the thread. New inline roots and viewed-file
marks require current code. PR-wide comments open discussion without changing
code. Apply these rules to Enter and comment navigation, including `]r`/`[r`.

`:Review context` opens the selected comment's recorded original context in a
separate reusable read-only context tab. With a known revision pair and objects,
show its original diff; with only a known commit, show the original side's file
when identifiable. Otherwise show any recorded diff hunk with the discussion
and an unavailable-location label. Never invent a base revision, line or outdated
flag. Missing context does not prevent reading the comment. An explicit context
action never changes the workspace's pinned comparison; `:Review` returns to
its home and ordinary `:tabclose` closes the auxiliary view.

Git owns file contents. On PR selection, prepare pinned base/head history and
latest fetched PR head before showing review objects. Batch-check original thread
commits locally, then fetch remaining hashes together; a commit outside current
branch history may already
exist locally. Later refreshes prepare new hashes without replacing the current
view. Missing historical objects report explicit warnings and keep hunk fallback;
missing current pins report an error. Preserve the worktree, index, branch, refs
and FETCH_HEAD. SQLite keeps snapshots/anchors/receipts, not duplicate Git blobs.
Use confirmed publication receipts and stable provider identities to recover
our own comments' exact original comparisons across imports and sessions.
Locally pruned historical objects are refetched when preparing the PR if possible.

## Commands and mappings

Tab-complete subcommands, argument names/values and applicable saved draft IDs.
Use `vim.ui.select` for bounded choices. Local help describes object-dependent
actions. Unsupported contexts fail with a specific explanation and no mutation.
Commands target the object under cursor or visual range; `comment` from the
home header is the explicit PR-wide creation context.

| Command after `:Review` | Action |
| --- | --- |
| No subcommand | Bound session home; from source route current branch to its PR or PR list |
| `pr BASE` | Create/select PR object for current branch to BASE; reuse remote PR or defer draft creation until Publish |
| `checkout` | Explicit PR checkout, with unsaved-buffer guards and dirty-work choices |
| `sessions` / `sessions new` | Browse saved sessions grouped by PR / create a session for selected or current PR |
| `prs [assignee=LOGIN\|@me\|all] [state=open\|closed\|merged\|all]` | Choose PR in current repository; defaults open/all assignees |
| `start` / `leave` | Activate or leave saved WIP |
| `conclude` | Outcome picker and review summary; home `cc` |
| `publish` | Send the ordered saved queue; missing conclusion leaves a draft review |
| `refresh` | Refresh remote PR query, reread local session browser, or refresh bound session; never repin comparison |
| `comment [review\|standalone]` | New comment at current line, range, file row or home header |
| `reply [review\|standalone]` | Reply to selected inline comment, including own comment |
| `edit` / `delete` | Edit or cancel the selected unsent local comment |
| `restore` | Cancel selected fetched comment's unsent staged deletion |
| `suggest` | Insert suggestion in eligible Markdown composer |
| `done` / `todo` | Stage own thumbs-up addition/removal for selected fetched comment |
| `viewed` / `unviewed` | Stage viewed state for selected file or current diff |
| `next [todo\|comment\|file]` / `previous [todo\|comment\|file]` | Navigate; Todo is default |
| `compare latest` | Explicitly update pinned comparison |
| `context` | Selected comment's original code context |
| `move` | Move selected unsent legacy inline comments into the review |
| `relocate [draft-id]` | Propose selected current diff location for an unpublished root |
| `resolve local\|remote\|edit` | Resolve selected conflict locally |
| `log` | Read-only event/result history for current workspace lineage |

The `<Plug>` mappings are `<Plug>(ReviewDone)`, `<Plug>(ReviewTodo)`,
`<Plug>(ReviewViewed)`, `<Plug>(ReviewUnviewed)`, `<Plug>(ReviewNextTodo)`,
`<Plug>(ReviewPreviousTodo)`, `<Plug>(ReviewNextComment)`,
`<Plug>(ReviewPreviousComment)`, `<Plug>(ReviewNextFile)` and
`<Plug>(ReviewPreviousFile)`. They invoke the corresponding
named action. Default normal-mode mappings are buffer-local `<CR>`, `s`, `u`,
diff/reader `a`, and home-only `cc`, plus `d`, `dd`, `D`, `x` for deletion in
home/thread lists/discussions; visual `d`/`x` applies there too. Visual
`a` in review diffs creates a range comment and replaces the native
`a{textobject}` prefix there. Visual `<CR>` has no plugin mapping; visual `s`/`u`
are limited to read-only review object lists/threads.
Review-local `]f`/`[f` navigate unviewed files; `]r`/`[r` navigate Todo comments.
These motions never write progress and respect staged unpublished marks.
Native `]c`/`[c` remain diff-hunk navigation. Review-local `]f`/`[f` replace their
native `gf` aliases; `gf` itself remains available.

Global normal-mode defaults are `<leader>rs` (`:Review`), `<leader>rr`
(`:Review refresh`), `<leader>rp` (`:Review publish`), `<leader>rcc`
(`:Review conclude`), and `<leader>rcs` (`:Review suggest` in an eligible composer).
Use `mapleader` as configured before plugin loading. Existing user mappings
take precedence over these and the Review-local navigation defaults. Source
buffers retain native editing/navigation; Insert-mode Enter remains newline.

## End-to-end walkthroughs

### Review a PR

1. Open `:Review`, choose a PR with Enter if needed, then `:Review start`.
2. Open an unviewed file with Enter. Inspect immutable code with native motions.
3. Press `a` on a code line for a new thread. Enter opens existing threads as a
   list, even for a single thread; Enter again reads it. No threads means no action.
   Visual `a` instead starts a range comment. Write Markdown, optionally
   `:Review suggest`, then `:wq` to save locally and return to the originating context.
   Mark inspected files viewed with `s`; `]f` opens the next unviewed file.
4. Return with `:Review`; inspect unpublished comments. Use `:Review move` on
   selected legacy standalone inline drafts when they should join the review.
5. Optionally `cc`, choose outcome, write summary, `:wq`. This queues a local
   conclusion event; it performs no provider writes.
6. `:Review publish` sends the saved queue. Missing conclusion leaves a draft.
   A queued conclusion submits after preceding operations. Failed publication
   retains its ID and confirmed results; retry sends only remaining operations.

A summary-only review can begin at `cc`. New inline drafts join the review;
existing legacy destinations never change implicitly.

### Fix feedback

1. Open the PR's Todo list. No submitted-review selection is required.
2. Enter on an item opens discussion and matching code. For outdated comments,
   inspect discussion and use `:Review context` if original code is needed.
3. Edit source using ordinary navigation such as Telescope; review buffers keep
   their compared revisions.
4. Press `a` in the conversation to reply, including to one's own thread;
   `:wq` saves locally and returns to the conversation.
5. Mark the individual fetched item with `s`. Cursor stays in place;
   `]r` opens the next Todo from the item now under the cursor. `u` reopens a
   Done item.
6. `:Review publish` sends the ordered saved queue. A queued conclusion ends
   its review; otherwise comments remain drafts.
   Commit/push source using the existing Git workflow.
7. Refresh fetches new replies as Todo when unmarked, including one's own newly
   fetched replies. Resolve remote conflicts explicitly; no silent overwrite.

## Backend and persistence contract

Lua presents buffers and invokes asynchronous Rust CLI operations through
structured arguments/results. It does not implement GitHub requests, read SQLite
directly, or infer business rules by parsing display text. Use Neovim APIs and
standard Lua facilities; no required Telescope, popup or custom UI framework.
The Rust core remains provider agnostic; GitHub identities stay in the adapter.

A local `ReviewId` identifies an immutable fetched snapshot. A `BatchId` is the
canonical session identity and remains stable across snapshots and publication.
Internal workspace aliases do not become separate user-facing session IDs.
Compose pinned revisions, one optional WIP identity/activity, per-draft
destination, saved conclusion, conflict state and publication references with
that batch. Preserve immutable snapshots, append-only events and receipts.
The protocol supplies session routing, grouped summaries, counts and publication
details; Lua only presents those projections.
Refresh selects an authoritative remote snapshot and an atomically persisted
workspace generation against it. Reconcile outstanding intent into its working
projection while preserving draft identities, destinations, original anchors and operation
provenance. Retain superseded snapshot/log history. Only the active generation
contributes pending writes; carrying history forward must not replay already
confirmed effects. Provider mappings connect a published local identity with
its fetched identity so refetch cannot duplicate a comment or lose progress.
All saves and conflict resolutions verify the workspace generation to prevent
overwriting a concurrent editor session's changes.

Store fetched payloads once. Current workspaces reference the selected review,
batch revision/event prefix and receipts; generation history records changed
fields instead of copying full workspaces. Hydration preserves exact historical
generations, including explicit null provider fields and publication recovery.
Unchanged fetches reuse an existing snapshot when reviewer, remote payload and
provider identities all match. Preserve explicitly imported review IDs while
sharing equal payloads. Migrate old checkpoints transactionally and compact the
database after migration; retain drafts, receipts, action logs and referenced
historical evidence.

Remote refresh has two phases. `fetch_refresh` imports an immutable snapshot
and returns a `prepared_refresh` token without mutating the session. The client
queues `apply_refresh` with that token alongside local writes; only this short
commit reconciles the session. Local actions remain available during network
fetches. The commit reconciles against the latest saved generation while
checking the captured baseline, publication receipts, pinned comparison and
active publication attempt. User actions retain strict generation checks.
`apply_refresh` with `compare_latest: true` explicitly updates the comparison
through the same guarded path.

Local selection operations use one ordered action-list request and return one
final projection. Preserve per-action generations, event ordering and history;
a failed action retains the successful prefix and reports its index. The client
applies that projection once, preserving the existing cursor/navigation rules.
Routine projections query publication lifecycle fields without decoding every
historical workspace snapshot. Hidden readers become stale and rebuild before
display or interaction; only visible readers redraw immediately.

A newly fetched routing snapshot suppresses the duplicate initial refresh only
when the backend confirms that exact snapshot is selected. Cached entry still
waits for the first home refresh of the Neovim session; later refreshes remain in
the background.
Fetch ordinary PR connections together, paginate overflow completely and retain
supplemental reads required for file metadata and revision consistency.

Required additions, beyond the current CLI:

- Normalized PR listing/state/assignee filtering and workspace projections,
  including reviewer, freshness, ordered rows, anchors, conflicts and pending
  destinations. Paginate remote queries completely, including comments, replies,
  review summaries and reactions; commit a refreshed snapshot only after a
  complete successful fetch, never a truncated or partially failed response.
- Repository workspace/WIP metadata in the existing SQLite store; transactional
  save, refresh reconciliation, conflict resolution, draft relocation and
  append-only supersession of obsolete pending intent, including cancellation of
  unsent deletions.
- Destination-aware preparation and execution: eligible standalone selection,
  actual grouped provider review roots/replies, saved-summary submission, and
  progress after confirmed review submission. All publication uses this workflow;
  local mutations accept ordered action lists, including single-item lists.
- Typed identities, provider thread/review associations and chronological sort
  metadata; normalize nonblank remote review summaries into checklist comment
  resources with reactions and permitted body edits. Submitted-summary deletion
  remains unavailable.
- Recorded pending-review preparation and uncertain-attempt reconciliation.
  Existing successful-operation receipts cover confirmed results, not unknown
  network outcomes. Account verification and publication locking remain required.
- Exact historical-context retrieval from available objects/hunks, body/version
  checks, and adapter validation of anchors, ownership and provider restrictions.

These are implementation requirements. The present integration must not be
claimed complete until the behavior below is verified. Detailed Rust/Lua change
plans and schema migrations are reviewed before implementation; this document
does not authorize implementation or live provider writes.

## Delivery and acceptance

Implement in dependency order: workspace/persistence and reconciliation;
destination-aware publication; Neovim home/diff/discussion/editors; then complete
both walkthroughs against a fixture provider and a real headless Neovim session.
Parallel backend/UI work is suitable after their structured contract is agreed.

The integration is complete only when these observable cases pass:

1. PR entry, filtering, switching and restart restore repository/reviewer-scoped
   saved state; cached display remains usable after a failed refresh. List
   refresh retains state/assignee filters; closed saved PRs remain reachable.
2. Home section order/counts, deterministic Todo order, folds and search agree
   with underlying rows, including unlocated comments and replies under Done roots.
3. Immutable base/head context, zero/one/multiple-thread Enter and supported visual
   ranges behave as specified; ordinary source editing and mappings are unaffected.
4. One discussion split is reused. Modified buffers survive attempted navigation,
   PR switches and refresh. Save failures keep text; no ordinary comment close
   publishes. No-op/blank/save-again/delete rules preserve identity and history.
5. New inline comments share the publication review; legacy standalone selections
   move only through explicit `move`. Sent content is immutable; unsent
   cancellation, membership changes and dependency checks preserve identities.
6. Suggestions remain Markdown body text with captured source. File/PR-wide
   contexts, unknown authors, unsupported replies and unavailable source report
   their actual capabilities without fabricated anchors or permissions.
7. `cc`/conclude queues one local conclusion event; Draft cancels its unsent
   value without changing history. All save/close paths remain
   local; only Publish writes. Draft and summary-only publications both work.
8. Native batching, ordered stop-on-error, confirmed prefix receipts, unchanged
   failed publication IDs, partial retries and uncertain reconciliation prevent
   duplicates and skip-ahead writes. Completed publications remain immutable.
9. Checklist membership is fetched-only and individual, includes own fetched
   replies and nonblank summaries, and counts only own thumbs-up. New replies,
   reaction overlays, pending deletions and file viewed independence behave as
   specified; receipts and private WIP preparation alone never create checklist
   members.
10. Marking preserves cursor/viewport without following moved entries. All
    next/previous searches use focused cursor context and stable backend order,
    wrap without returning current, and stay put without a destination. File
    searches skip viewed; comment/Todo searches skip Done, including staged
    unpublished progress. Commands and `<Plug>` mappings agree. Navigation never
    marks progress; contextual `s` stages progress without navigation. Other
    visible panes and remembered selections never provide navigation context.
    Native lowercase diff motions remain unchanged.
11. Refresh preserves saved/unsaved local work, reconciles published identities,
    and shows edit/delete conflicts. Keep local/use remote/edit affects only
    explicit intent; blocked publication prevents skip-ahead writes.
    Changed-body deletion conflicts and missing-target resolution never discard
    new remote work or recreate absent resources implicitly.
12. New commits do not repin code. Explicit comparison updates preserve old
    anchors; rejected drafts require explicit relocation/deletion. Relocation
    preserves draft/reply identity and never moves published comments.
13. Historical comments compare their referenced historical file on the left with
    latest fetched PR head on the right in the managed diff panes. Renames and
    absent current files are handled. Reconstructed context is labelled and only
    verified ranges are highlighted on the left.
    Missing history keeps its hunk/thread readable. Explicit context may still
    open an auxiliary tab. Editing a comment targets current resource
    identity and uses current conflict checks, not a historical snapshot.
14. Existing snapshots, batch logs, receipts and provider metadata survive storage
    migration. Reviewer mismatch blocks writes; concurrent sessions cannot publish
    overlapping plans or overwrite a newer workspace generation.
