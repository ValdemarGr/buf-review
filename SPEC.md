# Neovim review plugin specification

This document records the agreed plugin behavior and the implementation choices
that still need confirmation. It describes the intended plugin, not features
already implemented. The Rust CLI and core are implemented; their current
interface is documented in [README.md](README.md).

## Interaction model

The plugin uses Fugitive as its interaction model: ordinary Neovim buffers,
splits, motions, commands and contextual mappings. The file list uses Fugitive
style viewed/unviewed sections and stage/unstage keys.

There is one unified system for reviewing code and implementing feedback.
Authorship, PR status and whether a review has a submitted conclusion do not
select separate reviewing/fixing modes. Users can review their own code.

The plugin exposes a `:Review` command with subcommands, and individual commands
and `<Plug>` mappings for progress and navigation. Users compose these primitives
through their own mappings and macros.

## Core and backend boundary

- The Rust CLI owns domain operations and persistence. The Lua plugin presents
  buffers and invokes the CLI.
- The core model remains backend agnostic. GitHub identities and provider data
  types are mapped by the GitHub adapter, which can use authenticated `gh`.
- Fetched read snapshots and draft write events remain separate in both their
  representations and SQLite tables.
- A `ReviewId` identifies a fetched snapshot. A `BatchId` links an editable event
  log to that snapshot; write operations target the batch ID.
- Multiple batches can belong to one review. Batches remain editable and
  resubmittable after publication.
- Staging retains ordered events. Net changes are reduced at commit time;
  inspection of the original event log remains possible.
- Successful operations and returned provider metadata are recorded separately.
  Subsequent commits account for those results and reuse resource mappings.
- A backend consumes core-processed commit operations rather than interpreting
  the raw event log.

The existing domain supports comments, replies, comment edits/deletes, viewed
files, reactions, review conclusions and dismissal with a reason. Replies are
limited to diff threads; PR-wide comments have no replies.

## Active selection and browsing

The plugin keeps an explicitly activated working selection identifying the
project, PR, local review snapshot and writable batch. Commands available from
other buffers can return to or navigate that active selection.

Browsing and activation are separate:

- Opening a PR shows its code reviews and the user's local review progress.
- Selecting a historical review opens its conclusion and associated comments.
- Selecting the current working review for activation sets the active selection.
- Looking at historical objects on the same PR leaves the active selection
  intact. Browsing history does not replace or destroy the working batch.
- A global "open viewed" action returns to the active selection's file list and
  pending comments, independently of the object currently being inspected.

Historical snapshots and event logs remain immutable. Existing and draft
comments authored by the current user have an edit action. That action stages
a write event rather than rewriting a fetched snapshot. Target-batch selection
for edits initiated from historical views needs confirmation below.

## PR browser and worklist

The PR browser is a normal list buffer. `<CR>` on a PR opens the list of code
reviews and local progress for that PR. `<CR>` on a historical review opens its
conclusion and comments; explicit activation selects the working context.

A worklist query supports filtering PRs by assignee. The backend executes the
query and returns normalized results for the same PR browsing flow. Assignee is
a query filter, not a variable that selects the user's workflow.

Repository scope, query defaults and the precise query interface remain open
choices below.

## File list

The active file list uses grouped progress sections:

```text
Unviewed
  src/foo.rs   2 unfinished comments

Viewed
  src/bar.rs
```

The groups reflect the fetched viewed state plus staged changes. Rows can show
unfinished comment counts independently of viewed status: a viewed file can
still contain unfinished comments.

File-list mappings follow Fugitive's interaction style:

| Mapping | Action |
| --- | --- |
| `s` | Mark the selected file viewed |
| `u` | Mark the selected file unviewed |
| `<CR>` | Open the selected file in the review diff |

These actions stage review events. Publishing remains an explicit commit.
Marking progress and navigating are separate operations.

## Diff buffers

The plugin assumes the correct branch is checked out. The review diff has:

- A read-only left buffer using a `review://` URI for the base revision.
- A normal, editable filesystem buffer on the right.
- Neovim's native diff display and hunk navigation.

Once the filesystem buffer is edited, the right side displays working-tree
content. Stored comment anchors remain tied to the reviewed commit comparison.
Displayed markers can move with edits, but their movement must not silently
change the stored anchor. New comment placement requires a mapping back to the
reviewed commit; newly added local code has no corresponding line there.

Comment counts appear as virtual text on the relevant lines of both sides, for
example `// 3 comments`.

Mappings are scoped to the review context. In particular, using the same file
buffer in an ordinary window preserves its usual editing behavior.

## Contextual `<CR>`

Normal-mode `<CR>` in a PR diff invokes a contextual review action. Insert-mode
`<CR>` retains normal newline behavior.

The primary action on a diff line is:

- If comments cover the line, open the relevant comment list.
- If there are no comments, start a new comment draft.

An explicit creation action remains available when a line already has comments.

The general contextual dispatcher executes a single relevant action immediately
and opens a picker when several actions apply. With no relevant actions it does
nothing. Editing one's own comment is one of the available contextual actions.

`<CR>` in a comment list opens the selected comment and its local event log.
The comment body is editable through the edit action when its author is the
current user; this applies equally to fetched comments and drafts.

## Visual comments and suggestions

Visual-mode `<CR>` in the review diff opens a new comment/suggestion creation
buffer anchored to the selection. It creates a new draft even where existing
comments overlap the selection.

The creation flow supports both review prose and suggested replacement code.
The editor retains the selected file, side and range as its anchor context.
Suggestion formatting and the supported visual selection modes still need
confirmation below.

## Comment editor lifecycle

Comment creation, suggestion creation and editing use a Fugitive-inspired
editor lifecycle:

- `:w` saves the editor draft only.
- Closing the editor accepts its saved text and stages the corresponding
  create/edit operation.
- `:wq` saves the text, closes the editor and stages the operation.
- `:q!` discards unsaved changes; the last saved text remains the accepted text.
- The local event log is read-only and remains inspectable.
- Publishing is an explicit batch commit rather than an effect of closing the
  comment editor.

This lifecycle applies to the comment editor. Ordinary source files retain
their native write/quit behavior; the file list changes progress through its
actions.

## Comment implementation progress

For GitHub, the current user's thumbs-up reaction means a comment has been
implemented. Only that user's reaction counts toward their progress.

Progress is derived from fetched reactions plus staged reaction changes. Marking
implemented stages a thumbs-up addition; unmarking stages its removal. Existing
reaction operations and storage provide this behavior, without a separate done
field or completion event.

Completion is tracked for each individual comment, including replies. A thread
is unfinished whenever any comment or reply in it lacks the current user's
thumbs-up:

| State | Work remains? |
| --- | --- |
| Comment has thumbs-up; no replies | No |
| Comment has thumbs-up; reply has no thumbs-up | Yes |
| Comment and every reply have thumbs-up | No |
| Another reply without thumbs-up arrives | Yes |

A parent keeps its thumbs-up when a new reply arrives. A reply does not
implicitly mark the parent or the new reply implemented. Navigation to the next
unfinished comment targets the unmarked comment or reply.

This is the plugin's implementation-progress convention. It uses GitHub
reactions rather than GitHub's native thread-resolution operation.

## Composable progress and navigation

Expose these actions independently as commands and mappings:

| Progress action | Effect |
| --- | --- |
| Mark comment implemented | Stage the current user's thumbs-up |
| Unmark comment implemented | Stage removal of that thumbs-up |
| Mark file viewed | Stage viewed status for the current file |
| Mark file unviewed | Stage unviewed status for the current file |

File viewed/unviewed actions must also be usable from within the file's PR diff
buffers. They are not limited to selecting a row in the file list. Marking a
file or comment does not deliberately advance navigation. When the line has
several unfinished comments, a picker selects the comment to mark.

Navigation actions are independent:

- Next and previous unfinished comment.
- Next and previous comment, including implemented comments.
- Next and previous file.
- Native diff motions for navigating hunks.

Comment navigation works globally within the active PR, including across its
files. Navigation preserves the active PR/batch and changes no progress state.
Users can compose marking and navigation into a single mapping or macro.

## Required CLI and read-model additions

The existing CLI supplies snapshots, batches, staging, projection, commit plans,
local diffs and publication. The requested browsing flow additionally needs:

- A remote PR listing/worklist query interface.
- The association between fetched diff comments and submitted review resources,
  using core identities.
- The reviewed commit on fetched review resources, needed to identify the
  version associated with a historical review.

These additions must preserve separate read/write representations and existing
stored snapshots. Historical anchors can be incomplete; the plugin must not
invent missing revisions or line information.

GitHub's viewed-file state is current state for the authenticated user on the
PR. It is not a historical checklist attached to each submitted review.
Historical progress can only be shown where it was retained locally.

## Open decisions

The following are not settled requirements:

1. Whether the active selection survives Neovim restarts, and whether it is
   persisted in the project's SQLite database. Persisting there was proposed
   but has not been approved.
2. Whether a worklist is restricted to the current repository or can query
   across repositories. Current-repository scope was proposed but has not been
   approved.
3. The worklist query interface and defaults. Open PRs assigned to the current
   user were proposed as a default.
4. The suggestion editor's initial content and how users choose or insert a
   suggestion. GitHub supports suggestion blocks in Markdown comment bodies;
   the plugin's exact editor flow and backend representation are not settled.
5. Handling of characterwise, linewise and blockwise visual selections. The
   current write model represents inclusive line ranges, without columns.
6. How working-tree positions are mapped to reviewed-commit anchors, and how
   outdated or unavailable locations are presented during navigation.
7. Navigation ordering, boundary/wrapping behavior and exact keys beyond the
   agreed `<CR>` and file-list `s`/`u` mappings.
8. Editor acceptance/cancellation for empty new drafts and unchanged edits,
   including an explicit abort path if needed.
9. Target-batch selection for edits initiated from historical comment views,
   including cases where the resource is absent from the active snapshot.

Plugin implementation still requires a reviewed plan and user approval.
