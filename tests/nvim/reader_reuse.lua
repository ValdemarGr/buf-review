local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n')) end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(value) vim.cmd('Review ' .. value) end
local function input(keys) api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false) end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
end
local function row(needle, exact)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if exact and (vim.trim(value) == needle or value:sub(-#needle - 2) == '  ' .. needle)
      or not exact and value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, path, pair)
  return { comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id,
    author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = path, revision = pair or revision,
      range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } } } } } },
    fetched = true, done = false, conflict = false, summary = false }
end
local initial = {
  id = 'reader-reuse', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'reader-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Reader reuse', revision = revision,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('a', 'a.lua'), comment('b', 'b.md'), comment('historical', 'a.lua', { base = 'old-base', head = 'old-head' }) },
  files = { { path = 'a.lua', viewed = false }, { path = 'b.md', viewed = false }, { path = 'plain', viewed = false } },
  todo = { 'a', 'b', 'historical' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
write('code_contexts.json', {
  ['a.lua'] = { path = 'a.lua', revision = revision, base_content = 'one\nlocal before = true\nthree', head_content = 'one\nlocal after = true\nthree', binary = false },
  ['b.md'] = { path = 'b.md', revision = revision, base_content = 'one\nBefore docs\nthree', head_content = 'one\nAfter docs\nthree', binary = false },
  plain = { path = 'plain', revision = revision, base_content = 'one\nBefore plain\nthree', head_content = 'one\nAfter plain\nthree', binary = false },
})
command('')
wait(function() return kind() == 'home' and text():find('Code: ready', 1, true) end, 'Overview missing')
local home_buffer, home_epoch = api.nvim_get_current_buf(), 0
local function home()
  if kind() == 'home' then return end
  home_epoch = home_epoch + 1
  local value = read('state.json')
  local title = 'Reader refresh ' .. home_epoch
  value.pr.title = title; write('state.json', value)
  command(''); wait(function() return kind() == 'home' and text():find(title, 1, true) end, 'Overview refresh missing')
  assert(api.nvim_get_current_buf() == home_buffer, 'Overview reader identity changed')
end
local function open(id)
  home(); row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == id end, 'Thread missing ' .. id)
end
local function pair(path)
  local buffers, windows = {}, {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' and meta.path == path then buffers[meta.side], windows[meta.side] = buf, win end
  end
  assert(buffers.base and buffers.head, 'Missing managed diff pair for ' .. path)
  return buffers, windows
end
local function named(buf)
  assert(vim.fn.fnamemodify(api.nvim_buf_get_name(buf), ':t'):match('^review%-%d+$'), 'Reader name is not a short stable slot name')
  assert(vim.bo[buf].buftype == 'nofile' and vim.bo[buf].readonly and not vim.bo[buf].modifiable and not vim.bo[buf].buflisted, 'Reader lost scratch/read-only options')
end
local function count_readers()
  local count = 0
  for _, buf in ipairs(api.nvim_list_bufs()) do
    local meta = vim.b[buf].buf_review
    if meta and meta.kind ~= 'composer' then count = count + 1 end
  end
  return count
end

row('a.lua', true); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'Lua file missing')
local slots, windows = pair('a.lua')
local original_name = api.nvim_buf_get_name(slots.head)
local original_autocmds = #api.nvim_get_autocmds({ buffer = slots.head })
for _ = 1, 4 do
  command('next file'); wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'b.md' end, 'Markdown file missing')
  assert(vim.deep_equal(pair('b.md'), slots), 'File navigation replaced reader buffers')
  assert(vim.bo[slots.head].filetype == 'markdown' and text(slots.head):find('After docs', 1, true), 'Reused reader retained old code or filetype')
  command('next file'); wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'plain' end, 'Untyped file missing')
  assert(vim.deep_equal(pair('plain'), slots) and vim.bo[slots.head].filetype == 'review', 'Unknown file inherited previous source filetype')
  command('previous file'); wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'b.md' end, 'Previous Markdown missing')
  command('previous file'); wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'a.lua' end, 'Previous Lua missing')
  assert(vim.deep_equal(pair('a.lua'), slots) and api.nvim_buf_get_name(slots.head) == original_name, 'Reader identity or name changed')
end
named(home_buffer); named(slots.base); named(slots.head)
assert(vim.bo[slots.head].filetype == 'lua' and #api.nvim_get_autocmds({ buffer = slots.head }) == original_autocmds, 'Reader reuse accumulated handlers or retained filetype')
assert(vim.fn.maparg('a', 'x', false, true).callback, 'Reused code lost visual append')
home()
assert(vim.b.buf_review.path == nil and vim.b.buf_review.side == nil and vim.bo.filetype == 'review', 'Overview inherited source metadata')
assert(vim.fn.maparg('a', 'x') == '' and vim.fn.maparg('dd', 'n') ~= '', 'Reader role mappings leaked across views')

open('a')
local thread = api.nvim_get_current_buf()
named(thread)
command('next comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'b' end, 'Second thread missing')
assert(api.nvim_get_current_buf() == thread and text():find('Feedback b', 1, true) and not text():find('Feedback a', 1, true), 'Thread reader identity or object was stale')
open('historical')
local historical_slots = pair('a.lua')
assert(historical_slots.base == slots.base and historical_slots.head == slots.head, 'Historical pair allocated new reader buffers')
assert(vim.b[slots.base].buf_review.historical, 'Historical setup lost source identity')

-- Refreshing original context keeps a manually retained source snapshot visible.
local review_tab = api.nvim_get_current_tabpage()
local context = { available = true, anchor = initial.comments[3].comment.comment.target.target,
  base_content = 'Original base', head_content = 'Original head' }
write('contexts.json', { historical = context })
command('context')
wait(function() return kind() == 'context' and text() == 'Original head' end, 'Original context missing')
local context_head, context_window = api.nvim_get_current_buf(), api.nvim_get_current_win()
local context_metadata = vim.deepcopy(vim.b.buf_review)
vim.cmd('split'); local context_extra = api.nvim_get_current_win()
context.head_content = 'Refreshed head'; write('contexts.json', { historical = context })
command('context')
wait(function() return kind() == 'context' and text() == 'Refreshed head' end, 'Refreshed original context missing')
assert(api.nvim_win_is_valid(context_extra) and api.nvim_win_get_buf(context_extra) == context_head
  and text(context_head) == 'Original head' and vim.deep_equal(vim.b[context_head].buf_review, context_metadata), 'Context refresh closed or repurposed extra reader')
assert(api.nvim_get_current_win() == context_window and api.nvim_get_current_buf() ~= context_head, 'Context refresh did not replace only managed reader')
api.nvim_win_close(context_extra, true)
vim.cmd('tabclose'); api.nvim_set_current_tabpage(review_tab)
local _, historical_windows = pair('a.lua')
api.nvim_set_current_win(historical_windows.head)
command('next file')
wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'b.md' end, 'Current code after historical view missing')
assert(vim.deep_equal(pair('b.md'), slots), 'Historical/current switch replaced reader buffers')
for _, buf in pairs(slots) do
  local meta = vim.b[buf].buf_review
  assert(not meta.historical and not meta.provenance and meta.source_path == 'b.md' and vim.deep_equal(meta.revision, revision), 'Historical metadata leaked into current code')
end

-- Each extra split retains its source snapshot, while the managed pair advances.
local before = count_readers()
for index = 1, 12 do
  local path = index % 2 == 1 and 'a.lua' or 'b.md'
  local direction = path == 'a.lua' and 'previous file' or 'next file'
  local source, source_windows = pair(index % 2 == 1 and 'b.md' or 'a.lua')
  api.nvim_set_current_win(source_windows.head)
  local snapshot, metadata, contents = source.head, vim.deepcopy(vim.b[source.head].buf_review), text(source.head)
  vim.cmd('split'); local extra = api.nvim_get_current_win()
  command(direction)
  wait(function() return kind() == 'diff' and vim.b.buf_review.path == path end, 'Snapshot navigation missing')
  local current = pair(path)
  assert(current.head ~= snapshot and vim.deep_equal(vim.b[snapshot].buf_review, metadata) and text(snapshot) == contents, 'Navigation repurposed visible snapshot')
  api.nvim_win_close(extra, true)
  assert(api.nvim_buf_is_valid(snapshot) and text(snapshot) == contents, 'Closing snapshot wiped or unloaded its content')
  assert(count_readers() <= before + 1, 'Repeated snapshots allocated unbounded reader slots')
  assert(not vim.v.errmsg:find('E96', 1, true), 'Hidden reader slots remained in native diff membership')
end

-- A saved composer in another split reserves its hidden return reader.
open('a'); thread = api.nvim_get_current_buf()
input('aReserved reply<Esc>'); assert(kind() == 'composer', 'Reply composer missing')
local composer = api.nvim_get_current_buf()
assert(composer ~= thread, 'Composer reused read-only reader')
input(':w<CR>'); assert(not vim.bo.modified, 'Reply was not saved')
local composer_source = api.nvim_get_current_win()
vim.cmd('split'); local composer_window = api.nvim_get_current_win()
api.nvim_set_current_win(composer_source)
home(); row('Feedback b'); require('buf_review').enter()
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'b' end, 'Thread beside saved composer missing')
assert(api.nvim_get_current_buf() ~= thread and vim.b[thread].buf_review.object == 'a', 'Reader reserved by composer return was repurposed')
api.nvim_set_current_win(composer_window); input(':q<CR>')
assert(api.nvim_get_current_buf() == thread, 'Composer did not return to its reserved reader')
wait(function() return text():find('Reserved reply', 1, true) end, 'Returned conversation did not display saved reply')
assert(not api.nvim_buf_is_valid(composer), 'Closed composer became a reusable reader')
api.nvim_win_close(composer_window, true)

-- Local sessions own separate reader slots; returning resumes the original home.
home()
local current, other = read('state.json'), vim.deepcopy(initial)
other.id, other.review.id, other.locator = 'reader-other', 'other-reader-snapshot', 'https://github.com/acme/repo/pull/42'
write('views.json', { [current.id] = current, [other.id] = other })
write('routes.json', { entry = { route = 'batch', batch = current.id, fresh_review = current.review.id },
  [current.locator] = { route = 'batch', batch = current.id, fresh_review = current.review.id },
  [other.locator] = { route = 'batch', batch = other.id, fresh_review = other.review.id } })
command('prs'); wait(function() return kind() == 'prs' end, 'Session PR browser missing')
row('#42'); require('buf_review').enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == other.id end, 'Other session missing')
assert(api.nvim_get_current_buf() ~= home_buffer, 'Different sessions shared mutable reader slot')
named(api.nvim_get_current_buf())
command('prs'); wait(function() return kind() == 'prs' end, 'Return PR browser missing')
row('#1 Fixture review'); require('buf_review').enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == current.id end, 'Original session missing')
assert(api.nvim_get_current_buf() == home_buffer, 'Returning session replaced its home reader')
print('Neovim reusable reader slots passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
