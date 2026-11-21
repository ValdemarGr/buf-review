-- Invoked by the Rust integration harness with an isolated Git/gh fixture.
local api = vim.api
vim.opt.runtimepath:append(assert(vim.env.BUF_REVIEW_RUNTIME))
local executable = assert(vim.env.BUF_REVIEW_PR)
require('buf_review').setup({ executable = executable })
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
  local result = vim.system({ executable, 'workspace' }, { stdin = vim.json.encode({ op = 'active' }), text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.json.decode(result.stdout, { luanil = { object = true, array = true } }).workspace
end
local function home()
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'cached home missing')
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
local external_edit = vim.system({ executable, 'workspace' }, {
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
local changed = vim.system({ executable, 'workspace' }, {
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

-- Real backend blank writes: no object, local removal and same-ID restoration.
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
-- Fetched comments and confirmed receipts are immutable through UI and RPC.
for _, identity in ipairs({ 'own-root', assert(vim.env.BUF_REVIEW_RECEIPT_COMMENT) }) do
  home()
  local snapshot, published = active()
  for _, row in ipairs(snapshot.comments) do if row.comment.comment.id == identity then published = row end end
  assert(published and not published.destination, 'published fixture missing or still mutable')
  if published.fetched then
    for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
      if text:find(published.comment.comment.body, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
    end
    require('buf_review').enter()
    wait(function() return kind() == 'thread' end, 'published comment thread missing')
    for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
      if text == published.comment.comment.body then api.nvim_win_set_cursor(0, { line, 0 }); break end
    end
    input('<CR>')
    assert(kind() == 'thread' and vim.bo.readonly and not require('buf_review.editor').get(), 'owned published Enter opened editor')
    vim.cmd('Review edit'); vim.cmd('Review delete')
    assert(kind() == 'thread' and notices[#notices]:find('immutable', 1, true), 'explicit published mutation opened composer or hid validation')
    local before_operator = #notices
    input('dd')
    wait(function() return #notices > before_operator and notices[#notices]:find('immutable', 1, true) end, 'published delete operator bypassed core immutability')
  end
  for _, action in ipairs({
    { action = 'save', comment = identity, body = 'Attempted published edit' },
    { action = 'save', comment = identity, body = ' \t ' },
    { action = 'delete', comment = identity },
    { action = 'move', comments = { identity } },
  }) do
    local current = active()
    local result = vim.system({ executable, 'workspace' }, {
      stdin = vim.json.encode({ op = 'actions', workspace = current.id, generation = current.generation, actions = { action } }), text = true,
    }):wait()
    assert(result.code == 0, result.stderr)
    local response = vim.json.decode(result.stdout)
    assert(response.error and response.error:find('immutable', 1, true) and response.applied == 0,
      'published action accepted edit/deletion/move: ' .. vim.inspect(response))
    assert(active().generation == current.generation, 'rejected published action changed generation')
  end
  local unchanged = false
  for _, row in ipairs(active().comments) do
    if row.comment.comment.id == identity then unchanged = vim.deep_equal(row, published) end
  end
  assert(unchanged, 'published validation changed identity, body, anchor, destination or deletion state')
  if not published.fetched then
    local current = active()
    local result = vim.system({ executable, 'workspace' }, {
      stdin = vim.json.encode({ op = 'actions', workspace = current.id, generation = current.generation,
        actions = { { action = 'undo', comments = { identity } } } }), text = true,
    }):wait()
    assert(result.code == 0 and vim.json.decode(result.stdout).error, 'undo cancelled confirmed unfetched creation')
    assert(active().generation == current.generation, 'rejected unfetched undo changed generation')
    assert(not vim.tbl_contains(active().todo, identity) and not vim.tbl_contains(active().done, identity), 'confirmed receipt entered checklist')
  end
end

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
