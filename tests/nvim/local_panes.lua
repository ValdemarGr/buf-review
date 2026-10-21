local api = vim.api
local repository, project = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(project, 'p')
vim.env.BUF_REVIEW_FIXTURE = project
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
local function git(...)
  local result = vim.system(vim.list_extend({ 'git' }, { ... }), { cwd = project, text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
git('init', '--quiet')
git('config', 'user.name', 'Review fixture')
git('config', 'user.email', 'review@example.invalid')
local path = project .. '/file.txt'
vim.fn.writefile({ 'original' }, path)
git('add', 'file.txt'); git('commit', '--quiet', '-m', 'Base')
local base = git('rev-parse', 'HEAD')
vim.fn.writefile({ 'committed' }, path)
git('commit', '--quiet', '-am', 'Head')
local head = git('rev-parse', 'HEAD')
local revision = { base = base, head = head }
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, project .. '/' .. name) end
local function state() return vim.json.decode(table.concat(vim.fn.readfile(project .. '/state.json'), '\n')) end
local view = {
  id = 'local-panes', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'local-panes-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Local panes', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = { { comment = { origin = 'read', comment = { id = 'root', body = 'Existing comment', author = { id = 'alice', name = 'alice' },
    target = { kind = 'diff', target = { path = 'file.txt', revision = revision,
      range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } },
  } }, fetched = true, done = false, pending_delete = false, conflict = false, summary = false } },
  files = { { path = 'file.txt', viewed = false } }, pending = {}, todo = { 'root' }, done = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
local function code(dirty, current_head, text)
  write('code_contexts.json', { ['file.txt'] = { path = 'file.txt', revision = { base = base, head = current_head or head },
    base_content = 'original\n', head_content = text or 'committed\n', binary = false, local_path = path, local_dirty = dirty } })
end
code(false)
write('historical_contexts.json', { root = { anchor = view.comments[1].comment.comment.target.target, code = {
  path = 'file.txt', base_path = 'file.txt', revision = { base = head, head = head },
  base_content = 'committed\n', head_content = 'committed\n', binary = false, historical = true, provenance = 'recorded',
  original_base = { commit = base, path = 'file.txt', content = 'original\n', missing = false, provenance = 'recorded' },
  location = { path = 'file.txt', revision = { base = head, head = head }, range = { start = { side = 'base', line = 1 }, ['end'] = { side = 'base', line = 1 } } },
  local_path = path, local_dirty = false,
} } })
local client = require('buf_review.client')
local native_request = client.request
local hold_code, held_code = false, nil
client.request = function(root, request, callback)
  return native_request(root, request, function(response, err)
    if response and (request.op == 'code' or request.op == 'historical_code') then response.workspace = state() end
    if request.op == 'code' and hold_code then
      held_code = function() callback(response, err) end
      return
    end
    callback(response, err)
  end)
end
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd.cd(project)
vim.cmd.edit(path)
local native = api.nvim_get_current_buf()
local native_window = api.nvim_get_current_win()
vim.bo[native].filetype = 'text'
vim.bo[native].shiftwidth = 7
vim.bo[native].undolevels = 80
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. vim.inspect(vim.b.buf_review)
    .. '\n' .. table.concat(vim.fn.readfile(project .. '/requests.jsonl'), '\n'))
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function row(needle)
  for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if needle == 'file.txt' and text:sub(-#needle - 2) == '  ' .. needle
      or needle ~= 'file.txt' and text:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local function home()
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'home missing')
end
local function open_file()
  home(); row('file.txt'); require('buf_review').enter()
  wait(function() return kind() == 'diff' or api.nvim_get_current_buf() == native end, 'file panes missing')
end
local function review_panes()
  local result = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local marker = vim.b[buf].buf_review
    if marker and marker.kind == 'diff' then result[#result + 1] = { window = win, buffer = buf, marker = marker } end
  end
  return result
end
local function assert_native()
  assert(api.nvim_get_current_buf() == native and vim.b[native].buf_review == nil, 'local file treated as Review scratch')
  assert(vim.bo[native].buftype == '' and vim.bo[native].modifiable and not vim.bo[native].readonly, 'native editing options changed')
  assert(vim.bo[native].filetype == 'text' and vim.bo[native].shiftwidth == 7 and vim.bo[native].undolevels == 80, 'native configuration changed')
  for _, mapping in ipairs(api.nvim_buf_get_keymap(native, 'n')) do
    assert(not vim.tbl_contains({ 'a', 's', 'u', 'o' }, mapping.lhs), 'Review installed native editing mapping ' .. mapping.lhs)
  end
  assert(api.nvim_win_is_valid(native_window) and api.nvim_win_get_buf(native_window) == native, 'other native window changed')
end

open_file()
assert(#review_panes() == 2 and api.nvim_get_current_buf() ~= native, 'clean file did not use committed Review panes')
local right = api.nvim_get_current_win()
input('o')
assert_native()
assert(api.nvim_get_current_win() == right and #review_panes() == 1, 'o did not convert existing right pane through shared setup')
input('A edit<Esc>')
assert(api.nvim_get_current_line() == 'committed edit', 'native append failed')
input('u')
assert(api.nvim_get_current_line() == 'committed', 'native undo was replaced by Review progress')
assert_native()
input('oextra<Esc>')
assert(api.nvim_buf_line_count(native) == 2, 'native o was replaced by Review open')
home()
assert(api.nvim_buf_is_loaded(native) and vim.bo[native].modified and api.nvim_buf_line_count(native) == 2, 'closing local pane discarded native edits')
open_file()
assert_native()
assert(#review_panes() == 1, 'unsaved local changes did not select edit pane')

-- Restore file bytes explicitly as user work, then open a contracted comment.
api.nvim_buf_set_lines(native, 0, -1, false, { 'committed' })
vim.cmd.write()
home(); row('Existing comment'); require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'comment thread missing')
local source
for _, pane in ipairs(review_panes()) do if pane.marker.comment_role == 'comment' then source = pane.window end end
assert(#review_panes() == 2 and source, 'comment at HEAD did not contract to two committed panes')
api.nvim_set_current_win(source)
input('o')
assert_native()
assert(#review_panes() == 2, 'o did not add real right pane to contracted comment layout')
input('A local<Esc>')
assert(api.nvim_get_current_line() == 'committed local', 'contracted edit pane is not native')
local middle
for _, pane in ipairs(review_panes()) do if pane.marker.comment_role == 'comment' then middle = pane.buffer end end
local colors = api.nvim_get_namespaces()['buf-review-comment-diff']
local function removed()
  for _, mark in ipairs(api.nvim_buf_get_extmarks(middle, colors, 0, -1, { details = true })) do
    if mark[4].hl_group == 'BufReviewCommentRemoved' then return true end
  end
  return false
end
wait(removed, 'native edit did not update middle removal colors')
input('u')
wait(function() return not removed() end, 'native undo did not restore middle colors')
input('A local<Esc>')
home()
assert(vim.bo[native].modified and api.nvim_get_current_line() ~= 'committed local', 'home did not retain modified native buffer separately')

-- Disk changes, including staged changes, use the same Local descriptor.
api.nvim_buf_set_lines(native, 0, -1, false, { 'staged content' })
api.nvim_buf_call(native, function() vim.cmd.write() end)
git('add', 'file.txt')
code(true)
open_file(); assert_native()
local captured = review_panes()[1].buffer
git('commit', '--quiet', '-m', 'Unpushed local commit')
local newer = git('rev-parse', 'HEAD')
assert(vim.b[captured].buf_review.revision.head == head and api.nvim_get_current_buf() == native,
  'committing silently retargeted an already open diff')
local current = state()
current.comparison.head = newer
write('state.json', current)
code(false, newer, 'staged content\n')
open_file()
assert(kind() == 'diff' and vim.b.buf_review.revision.head == newer and api.nvim_get_current_buf() ~= native,
  'explicit reopen did not capture new unpushed HEAD as committed Review pane')
assert(vim.b.buf_review.revision.head ~= head, 'reopen reused previous HEAD snapshot')
assert(state().review.read.pull_request.revision.head == head, 'local reopen changed fetched remote head')
assert(vim.b[native].buf_review == nil and api.nvim_buf_is_loaded(native), 'reopening wiped or decorated native file')

-- A clean empty file remains a committed Review pane; native buffer empty-line
-- representation must never be mistaken for unsaved changes.
vim.fn.writefile({}, path)
git('commit', '--quiet', '-am', 'Empty file')
local empty_head = git('rev-parse', 'HEAD')
api.nvim_buf_set_lines(native, 0, -1, false, {})
vim.bo[native].modified = false
current = state()
current.comparison.head = empty_head
write('state.json', current)
code(false, empty_head, '')
open_file()
assert(kind() == 'diff' and vim.b.buf_review.revision.head == empty_head and api.nvim_get_current_buf() ~= native,
  'clean empty file was treated as local edits')

-- Two code reads can observe different Git HEADs without any persisted Review
-- generation changing. A late old canonical workspace must not roll HEAD back.
home()
local home_buffer = api.nvim_get_current_buf()
hold_code = true
row('file.txt'); require('buf_review').enter()
wait(function() return held_code ~= nil end, 'delayed canonical code response missing')
vim.fn.writefile({ 'newest committed' }, path)
git('commit', '--quiet', '-am', 'Newer HEAD during code read')
local newest = git('rev-parse', 'HEAD')
current = state()
current.comparison.head = newest
write('state.json', current)
code(false, newest, 'newest committed\n')
hold_code = false
row('file.txt'); require('buf_review').enter()
wait(function() return kind() == 'diff' and vim.b.buf_review.revision.head == newest end, 'new canonical HEAD did not open')
local latest_window, latest_buffer = api.nvim_get_current_win(), api.nvim_get_current_buf()
held_code()
ReviewTest.wait(50)
assert(api.nvim_get_current_win() == latest_window and api.nvim_get_current_buf() == latest_buffer
  and vim.b.buf_review.revision.head == newest, 'late old code response replaced newer panes')
assert(vim.wo[latest_window].winbar:find('[unviewed]', 1, true), 'late canonical workspace rolled HEAD back under newer pane')
api.nvim_win_set_buf(latest_window, home_buffer)
assert(table.concat(api.nvim_buf_get_lines(home_buffer, 0, -1, false), '\n'):find('HEAD ' .. newest:sub(1, 10), 1, true),
  'late canonical workspace rolled back current Review HEAD')
for _, record in ipairs(vim.fn.readfile(project .. '/requests.jsonl')) do
  local request = vim.json.decode(record)
  assert(request.op ~= 'publish' and request.op ~= 'actions', 'native editing invoked Review writes')
end

print('Neovim committed/local pane workflows passed')
vim.fn.delete(project, 'rf')
vim.cmd('qa!')
