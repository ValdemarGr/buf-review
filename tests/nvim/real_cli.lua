-- Invoked by the Rust integration harness with an isolated Git/gh fixture.
local api = vim.api
vim.opt.runtimepath:append(assert(vim.env.BUF_REVIEW_RUNTIME))
vim.g.buf_review_executable = assert(vim.env.BUF_REVIEW_PR)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function wait(predicate, description)
  assert(ReviewTest.wait(10000, predicate, 10), description .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(50)
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(50)
end
local function active()
  local result = vim.system({ vim.g.buf_review_executable, 'workspace' }, { stdin = vim.json.encode({ op = 'active' }), text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.json.decode(result.stdout, { luanil = { object = true, array = true } }).workspace
end
local function home()
  local notices_before = #notices
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'cached home missing')
  wait(function() return #notices > notices_before and notices[#notices]:find('fixture offline', 1, true) and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Error:', 1, true) end, 'offline refresh did not retain cached home')
end

vim.cmd('Review sessions')
wait(function() return kind() == 'sessions' end, 'local sessions browser missing')
local original = active().id
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find(original, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'home' end, 'cached session selection missing')
home()
assert(vim.bo.readonly and not vim.bo.modifiable)
vim.cmd('Review start')
wait(function() return active().wip and active().wip.active end, 'WIP did not start')
local selected
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('file.rs', 1, true) and text:find('Todo', 1, true) then selected = line; break end
end
assert(selected, 'root Todo row missing')
api.nvim_win_set_cursor(0, { selected, 0 })
require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'inline discussion did not open')
assert(#api.nvim_tabpage_list_wins(0) == 3, 'diff/discussion layout differs')
vim.cmd('Review reply')
assert(kind() == 'composer')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Saved review reply from Neovim' })
vim.cmd('Review leave')
wait(function() return not active().wip.active end, 'own activity change did not finish while composing')
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'local reply save did not close')
local saved = active()
local draft
for _, row in ipairs(saved.comments) do
  if row.comment.comment.body == 'Saved review reply from Neovim' then draft = row end
end
assert(draft and draft.destination == 'review' and not draft.fetched, 'saved review reply lost identity/destination')
home()
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Saved review reply from Neovim', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
assert(kind() == 'composer')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved local edit must survive external change' })
local concurrent_edit = active()
local external_edit = vim.system({ vim.g.buf_review_executable, 'workspace' }, {
  stdin = vim.json.encode({ op = 'actions', workspace = concurrent_edit.id, generation = concurrent_edit.generation, actions = { { action = 'save', comment = draft.comment.comment.id, body = 'Externally changed review reply' } } }), text = true,
}):wait()
assert(external_edit.code == 0, external_edit.stderr)
local unsafe, refusal = pcall(vim.cmd, 'write')
assert(not unsafe and refusal:find('saved content changed', 1, true) and vim.bo.modified, 'changed saved body was blindly overwritten')
assert(api.nvim_get_current_line() == 'Unsaved local edit must survive external change')
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == draft.comment.comment.id then assert(row.comment.comment.body == 'Externally changed review reply') end
end
input(':q!<CR>')
home()
vim.cmd('Review leave')
wait(function() return active().wip and not active().wip.active end, 'WIP did not leave')
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('file.rs', 1, true) and text:find('Todo', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
local before_progress = vim.fn.winsaveview()
vim.cmd('Review done')
wait(function() return #active().done == 1 end, 'individual progress was not saved')
local after_progress = vim.fn.winsaveview()
assert(after_progress.lnum == before_progress.lnum and after_progress.col == before_progress.col and after_progress.topline == before_progress.topline and after_progress.leftcol == before_progress.leftcol, 'progress moved cursor or viewport to Done')
vim.ui.select = function(_, _, callback) callback('comment') end
vim.cmd('Review conclude')
wait(function() return kind() == 'composer' end, 'conclusion composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Saved summary, cancelled submission' })
local concurrent = active()
local changed = vim.system({ vim.g.buf_review_executable, 'workspace' }, {
  stdin = vim.json.encode({ op = 'actions', workspace = concurrent.id, generation = concurrent.generation, actions = { { action = 'viewed', path = 'file.rs', viewed = true } } }), text = true,
}):wait()
assert(changed.code == 0, changed.stderr)
local wrote, failure = pcall(vim.cmd, 'write')
assert(wrote and not vim.bo.modified, 'real stale save did not safely retry: ' .. tostring(failure))
input(':q<CR>')
wait(function() return kind() ~= 'composer' end, 'local q did not close saved summary')
assert(active().wip.conclusion.body == 'Saved summary, cancelled submission')

-- Contextual u cancels saved creation in both destinations through the real backend.
for _, destination in ipairs({ 'standalone', 'review' }) do
  home()
  api.nvim_win_set_cursor(0, { 1, 0 })
  if destination == 'review' then
    for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
      if text:match('%s%sfile%.rs$') then api.nvim_win_set_cursor(0, { line, 0 }); break end
    end
  end
  vim.cmd('Review comment ' .. destination)
  assert(kind() == 'composer', 'real ' .. destination .. ' cancellation composer missing: ' .. vim.inspect(notices))
  local body = 'Cancel real ' .. destination .. ' creation with u'
  api.nvim_buf_set_lines(0, 0, -1, false, { body })
  input(':wq<CR>')
  wait(function() return kind() ~= 'composer' end, 'cancellation test composer did not close')
  home()
  local cancelled_id
  for _, row in ipairs(active().comments) do
    if row.comment.comment.body == body then
      assert(row.destination == destination and not row.fetched, 'saved cancellation draft lost destination')
      cancelled_id = row.comment.comment.id
    end
  end
  assert(cancelled_id, 'real cancellation draft missing')
  for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if text:find(body, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
  end
  input(destination == 'review' and 'Vu' or 'u')
  wait(function()
    for _, row in ipairs(active().comments) do if row.comment.comment.id == cancelled_id then return false end end
    return true
  end, 'contextual u did not cancel real ' .. destination .. ' creation')
end

-- Real backend blank writes: no object, local removal, then fetched staged deletion.
home()
api.nvim_win_set_cursor(0, { 1, 0 })
vim.cmd('Review comment standalone')
api.nvim_buf_set_lines(0, 0, -1, false, { ' \t ' })
local before_blank = active()
vim.cmd('write')
assert(not vim.bo.modified and active().generation == before_blank.generation, 'new blank comment changed real workspace')
input(':q<CR>')
for _, close_after in ipairs({ false, true }) do
  home()
  api.nvim_win_set_cursor(0, { 1, 0 })
  vim.cmd('Review comment standalone')
  api.nvim_buf_set_lines(0, 0, -1, false, { 'Real draft removed by blank ' .. tostring(close_after) })
  vim.cmd('write')
  local identity = require('buf_review.editor').get().comment
  assert(identity)
  input('cc <Tab> <Esc>')
  input(close_after and ':wq<CR>' or ':w<CR>')
  for _, row in ipairs(active().comments) do assert(row.comment.comment.id ~= identity, 'blank draft remained in real workspace') end
  if close_after then
    wait(function() return kind() ~= 'composer' end, 'real blank draft :wq did not close')
  else
    assert(kind() == 'composer' and not vim.bo.modified)
    local generation = active().generation
    vim.cmd('write')
    assert(active().generation == generation, 'repeated blank write changed real workspace')
    input('u')
    assert(api.nvim_get_current_line() == 'Real draft removed by blank false', 'real draft undo did not recover original body')
    vim.cmd('write')
    local restored = false
    for _, row in ipairs(active().comments) do
      if row.comment.comment.id == identity then restored = row.comment.comment.body == 'Real draft removed by blank false' end
    end
    assert(restored and require('buf_review.editor').get().comment == identity, 'undo write failed to restore same real draft')
    api.nvim_buf_set_lines(0, 0, -1, false, { 'Typed real draft after deletion' })
    vim.cmd('write')
    assert(require('buf_review.editor').get().comment == identity)
    api.nvim_buf_set_lines(0, 0, -1, false, { ' ' })
    input(':wq<CR>')
    wait(function() return kind() ~= 'composer' end, 'restored real draft blank :wq did not close')
  end
end
home()
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Owned fetched feedback', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'owned fetched comment thread missing')
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text == 'Owned fetched feedback' then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
input('dd')
local dd_staged = false
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == 'own-root' then dd_staged = row.pending_delete end
end
assert(dd_staged, 'real thread-body dd failed to stage deletion')
wait(function() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('pending deletion', 1, true) end, 'real dd response was not rendered')
api.nvim_win_set_cursor(0, { 2, 0 })
input('u')
wait(function()
  for _, row in ipairs(active().comments) do if row.comment.comment.id == 'own-root' then return not row.pending_delete end end
end, 'real dd deletion did not restore')
vim.cmd('Review edit')
api.nvim_buf_set_lines(0, 0, -1, false, { ' \t ' })
local before_external_delete = active()
local external_before_delete = vim.system({ vim.g.buf_review_executable, 'workspace' }, {
  stdin = vim.json.encode({ op = 'actions', workspace = before_external_delete.id, generation = before_external_delete.generation, actions = { { action = 'save', comment = 'own-root', body = 'Externally updated owned feedback' } } }), text = true,
}):wait()
assert(external_before_delete.code == 0, external_before_delete.stderr)
local blank_saved, blank_error = pcall(vim.cmd, 'write')
assert(not blank_saved and blank_error:find('saved content changed', 1, true) and vim.bo.modified and api.nvim_get_current_line() == ' \t ', 'stale blank save deleted changed comment or lost whitespace')
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == 'own-root' then assert(not row.pending_delete and row.comment.comment.body == 'Externally updated owned feedback') end
end
input(':q!<CR>')
home()
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Externally updated owned feedback', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'updated owned fetched comment thread missing')
vim.cmd('Review edit')
input('cc <Tab> <Esc>')
vim.cmd('write')
assert(not vim.bo.modified)
local staged = false
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == 'own-root' then staged = row.pending_delete and row.comment.comment.body == 'Owned fetched feedback' end
end
assert(staged, 'real fetched blank write failed to stage deletion with original body')
local deletion_generation = active().generation
vim.cmd('write')
assert(active().generation == deletion_generation, 'repeat fetched blank write changed real workspace')
input('u')
assert(api.nvim_get_current_line() == 'Externally updated owned feedback', 'real fetched undo did not recover original body')
vim.cmd('write')
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == 'own-root' then assert(not row.pending_delete and row.comment.comment.body == 'Externally updated owned feedback') end
end
api.nvim_buf_set_lines(0, 0, -1, false, { 'Typed real fetched body after deletion' })
vim.cmd('write')
assert(require('buf_review.editor').get().comment == 'own-root')
api.nvim_buf_set_lines(0, 0, -1, false, { ' ' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'restored fetched blank :wq did not close')
home()
local deletion_visible = false
local in_pending = false
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Unpublished changes', 1, true) then in_pending = true end
  if text:find('Unviewed files', 1, true) then in_pending = false end
  if in_pending and text:find('pending deletion', 1, true) then
    deletion_visible = api.nvim_buf_get_lines(0, line, line + 1, false)[1]:find('Owned fetched feedback', 1, true) ~= nil
    break
  end
end
assert(deletion_visible, 'real fetched staged deletion missing from Unpublished changes')

-- Confirmed-but-unfetched comments are published resources, including while deleted.
local receipt_id = assert(vim.env.BUF_REVIEW_RECEIPT_COMMENT)
local before_receipt_edit = active()
local receipt_edit = vim.system({ vim.g.buf_review_executable, 'workspace' }, {
  stdin = vim.json.encode({ op = 'actions', workspace = before_receipt_edit.id, generation = before_receipt_edit.generation, actions = { { action = 'save', comment = receipt_id, body = 'Confirmed receipt ready for deletion' } } }), text = true,
}):wait()
assert(receipt_edit.code == 0, receipt_edit.stderr)
home()
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Confirmed receipt ready for deletion', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'confirmed receipt did not open discussion')
vim.cmd('Review edit')
input('cc <Tab> <Esc>')
vim.cmd('write')
local receipt_deleted = false
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == receipt_id then receipt_deleted = row.pending_delete and not row.fetched end
end
assert(receipt_deleted, 'confirmed unfetched deletion disappeared from workspace View')
input('u')
assert(api.nvim_get_current_line() == 'Confirmed receipt ready for deletion')
vim.cmd('write')
assert(require('buf_review.editor').get().comment == receipt_id)
for _, row in ipairs(active().comments) do
  if row.comment.comment.id == receipt_id then assert(not row.pending_delete and row.comment.comment.body == 'Confirmed receipt ready for deletion') end
end
api.nvim_buf_set_lines(0, 0, -1, false, { ' ' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'confirmed receipt blank :wq did not close')
home()
local receipt_visible = false
in_pending = false
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Unpublished changes', 1, true) then in_pending = true end
  if text:find('Unviewed files', 1, true) then in_pending = false end
  if in_pending and text:find('pending deletion', 1, true) then
    local preview = api.nvim_buf_get_lines(0, line, line + 1, false)[1] or ''
    if preview:find('Confirmed', 1, true) then receipt_visible = true end
  end
end
assert(receipt_visible, 'confirmed receipt deletion missing from Unpublished changes')
assert(not vim.tbl_contains(active().todo, receipt_id) and not vim.tbl_contains(active().done, receipt_id), 'confirmed unfetched deletion entered fetched-only checklist')
for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if text:find('Confirmed comment before refetch', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
input('u')
wait(function()
  for _, row in ipairs(active().comments) do if row.comment.comment.id == receipt_id then return not row.pending_delete and not row.fetched end end
end, 'u did not restore confirmed unfetched comment')
local before_unfetched_reverse = active()
local notices_before_reverse = #notices
input('u')
wait(function() return #notices > notices_before_reverse and notices[#notices]:find('only for fetched comments', 1, true) end, 'unfetched reverse did not report backend validation')
local after_unfetched_reverse = active()
assert(after_unfetched_reverse.generation == before_unfetched_reverse.generation, 'u cancelled confirmed unfetched creation')
local confirmed_present = false
for _, row in ipairs(after_unfetched_reverse.comments) do if row.comment.comment.id == receipt_id then confirmed_present = true end end
assert(confirmed_present, 'u removed published receipt from real workspace')
input('dd')
wait(function()
  for _, row in ipairs(active().comments) do if row.comment.comment.id == receipt_id then return row.pending_delete end end
end, 'restored confirmed unfetched comment could not be deleted again')

-- Exercise direct diff append against the actual Rust/SQLite protocol.
home()
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:match('%s%sfile%.rs$') then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
input('<CR>')
wait(function() return kind() == 'diff' end, 'real append comparison missing')
local append_reader, append_window = api.nvim_get_current_buf(), api.nvim_get_current_win()
input('aReal diff append root<Esc>:wq<CR>')
wait(function() return api.nvim_get_current_buf() == append_reader end, 'real append did not return originating comparison')
assert(api.nvim_get_current_win() == append_window, 'real append did not return originating comparison window')
local append_id
for _, row in ipairs(active().comments) do
  if row.comment.comment.body == 'Real diff append root' then
    append_id = row.comment.comment.id
    assert(row.comment.comment.target.kind == 'diff' and row.comment.comment.target.target.range.start.line == 1, 'real append lost inline target')
  end
end
assert(append_id, 'real append did not save local root')
input('<CR>')
assert(kind() == 'threads', 'real diff Enter bypassed existing location list')
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:find('Real diff append root', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
input('u')
wait(function()
  for _, row in ipairs(active().comments) do if row.comment.comment.id == append_id then return false end end
  return true
end, 'real appended creation could not be cancelled')

vim.cmd('tabnew')
assert(vim.fn.maparg('<CR>', 'n') == '' and vim.fn.maparg('s', 'n') == '' and vim.fn.maparg('cc', 'n') == '')
assert(vim.fn.maparg('dd', 'n') == '', 'comment dd leaked to ordinary buffer')
print('Real CLI Neovim workflow passed')
vim.cmd('qa!')
