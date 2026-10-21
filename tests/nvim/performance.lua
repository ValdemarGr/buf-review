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
local function read(name)
  return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n'), { luanil = { object = true, array = true } })
end
local function records(name, op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    local value = vim.json.decode(line, { luanil = { object = true, array = true } })
    if not op or value.op == op then result[#result + 1] = value end
  end
  return result
end
local function requests(op) return records('requests.jsonl', op) end
local function state() return read('state.json') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(40)
end
local function command(value) vim.cmd('Review ' .. value) end
local function row(body)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(body, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row: ' .. body .. '\n' .. text())
end
local function watch(buf)
  local changes = 0
  api.nvim_buf_attach(buf, false, { on_lines = function() changes = changes + 1 end })
  return function() return changes end
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, author)
  return { comment = { origin = 'read', comment = { id = id, body = id, author = { id = author or 'me', name = author or 'me' }, target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision, range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } } } }, fetched = true, done = false, pending_delete = false, conflict = false, summary = false }
end
local initial = {
  id = 'performance-batch', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'fresh-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Performance fixture', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('initial-root') }, files = { { path = 'src/file.rs', viewed = false } }, todo = { 'initial-root' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
local cold = vim.env.BUF_REVIEW_COLD
local route = { route = 'batch', batch = initial.id }
if cold ~= 'cached' then route.fresh_review = cold == 'mismatch' and 'another-snapshot' or initial.review.id end
write('routes.json', { entry = route })
if cold then vim.fn.writefile({ '0.2' }, fixture .. '/delay_refresh') end
command('')
if cold then
  wait(function() return kind() == 'loading' and #requests('fetch_refresh') == 1 end, 'cached/mismatched first open skipped remote wait')
  assert(not text():find('initial-root', 1, true), 'initial wait exposed cached objects')
end
wait(function() return kind() == 'home' end, 'initial home missing')
if cold then
  assert(#requests('fetch_refresh') == 1, 'cached/mismatched first open refreshed more than once')
  print('Cold snapshot proof ' .. cold .. ' passed')
  vim.fn.delete(fixture, 'rf')
  vim.cmd('qa!')
end
assert(#requests('fetch_refresh') == 0, 'authoritative fresh route was fetched twice')
assert(requests('select')[1].fresh_review == initial.review.id, 'route snapshot proof was not forwarded')
vim.fn.writefile({ '0.15' }, fixture .. '/delay_refresh')
command('')
assert(kind() == 'home', 'later cached entry blocked on refresh')
wait(function() return #requests('fetch_refresh') == 1 and state().generation > 0 end, 'fresh proof incorrectly suppressed later refresh')
vim.fn.delete(fixture .. '/delay_refresh')
local home = api.nvim_get_current_buf()
local home_changes = watch(home)
local function reset(comments)
  local snapshot = state()
  snapshot.generation = snapshot.generation + 1
  snapshot.comments, snapshot.todo, snapshot.done, snapshot.pending = comments, {}, {}, {}
  for _, item in ipairs(comments) do if item.fetched then snapshot.todo[#snapshot.todo + 1] = item.comment.comment.id end end
  write('state.json', snapshot)
  command('')
  wait(function() return kind() == 'home' and text():find(comments[1].comment.comment.body, 1, true) end, 'reset did not apply bound snapshot')
end

for _, size in ipairs({ 1, 10, 100 }) do
  local comments = {}
  for index = 1, size do comments[#comments + 1] = comment(('visual-%d-%03d'):format(size, index)) end
  reset(comments)
  local first = row(comments[1].comment.comment.body) - 1
  api.nvim_win_set_cursor(0, { first, 2 })
  input('V' .. (size * 2 - 1) .. 'j')
  local position = vim.fn.winsaveview()
  local before_requests, before_render = #requests('actions'), home_changes()
  input('s')
  wait(function() return #state().done == size and text():find('Done (' .. size .. ')', 1, true) end, 'visual progress did not finish')
  local sent = requests('actions')
  assert(#sent == before_requests + 1 and #sent[#sent].actions == size, 'visual ' .. size .. ' did not use one ordered RPC')
  for index, action in ipairs(sent[#sent].actions) do assert(action.action == 'reaction' and action.comment == comments[index].comment.comment.id, 'visual order/dedup changed') end
  assert(home_changes() == before_render + 1, 'visual operation rendered intermediate workspaces')
  local after = vim.fn.winsaveview()
  assert(after.lnum == math.min(position.lnum, api.nvim_buf_line_count(0)), 'visual batch moved physical cursor row')
end

-- Immutable hidden readers do no redraw work, then refresh before interaction.
reset({ comment('hidden-root') })
row('hidden-root'); require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'first discussion missing')
local hidden = api.nvim_get_current_buf()
local hidden_changes = watch(hidden)
vim.cmd('split')
local retained_window = api.nvim_get_current_win()
command('')
wait(function() return kind() == 'home' end, 'home missing')
row('hidden-root'); require('buf_review').enter()
wait(function() return kind() == 'thread' and api.nvim_get_current_buf() ~= hidden end, 'second discussion missing')
local visible = api.nvim_get_current_buf()
local visible_changes = watch(visible)
api.nvim_win_close(retained_window, true)
local hidden_before, home_before, visible_before = hidden_changes(), home_changes(), visible_changes()
require('buf_review').progress(true)
wait(function() return state().comments[1].done and text():find('Done', 1, true) end, 'visible discussion did not update')
assert(hidden_changes() == hidden_before and home_changes() == home_before, 'hidden reader redrew eagerly')
assert(visible_changes() == visible_before + 1, 'visible reader did not render exactly once')
local current, other = api.nvim_get_current_win()
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
  if marker and marker.kind == 'diff' and marker.side == 'base' then other = win end
end
assert(other, 'base pane missing')
api.nvim_win_set_buf(other, hidden)
assert(api.nvim_get_current_win() == current and text(hidden):find('Done', 1, true) and hidden_changes() == hidden_before + 1, 'noncurrent BufWinEnter did not refresh hidden reader')
api.nvim_set_current_win(other)
row('hidden-root'); require('buf_review').progress(false)
wait(function() return not state().comments[1].done end, 'lazy reader metadata targeted wrong comment')
command('')
wait(function() return kind() == 'home' and text():find('Todo (1)', 1, true) end, 'hidden home did not refresh on return')

-- Reader return after save renders latest roots once, with the original pin.
row('hidden-root'); require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'discussion missing before return test')
local head
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
  if marker and marker.kind == 'diff' and marker.comment_role == 'comment' then head = win end
end
assert(head, 'current commented version missing')
api.nvim_set_current_win(head)
assert(vim.b.buf_review.revision.base == revision.head, 'reader return used wrong comment version')
api.nvim_win_set_cursor(0, { 1, 0 })
input('<CR>')
assert(kind() == 'threads')
local list = api.nvim_get_current_buf()
local list_changes = watch(list)
input('aNew root after hidden save<Esc>')
local before_return = list_changes()
input(':wq<CR>')
wait(function() return api.nvim_get_current_buf() == list and text():find('New root after hidden save', 1, true) end, 'saved root missing from lazy returned list')
assert(list_changes() == before_return + 1, 'hidden return reader rendered before it was shown')
command('')
local changed_pin = state()
changed_pin.generation = changed_pin.generation + 1
changed_pin.comparison = { base = 'new-base', head = 'new-head' }
write('state.json', changed_pin)
command('refresh')
wait(function() return text():find('new-base', 1, true) end, 'comparison change did not reach home')
api.nvim_win_set_buf(0, list)
local before_pin_action = #requests('actions')
input('a')
assert(kind() == 'threads' and #requests('actions') == before_pin_action and notices[#notices]:find('Comparison changed', 1, true), 'lazy location reader guessed a new pin')
command('')

-- Rust preflights the whole selection before any mutation.
local owned_preflight, foreign_preflight = comment('owned-preflight'), comment('foreign-preflight', 'other')
for _, item in ipairs({ owned_preflight, foreign_preflight }) do
  item.comment.origin, item.fetched, item.destination = 'draft', false, 'review'
end
reset({ owned_preflight, foreign_preflight })
local first = row('owned-preflight') - 1
api.nvim_win_set_cursor(0, { first, 0 })
local before = #requests('actions')
input('V3jd')
assert(#requests('actions') == before + 1 and not state().comments[1].pending_delete and notices[#notices]:find('verified ownership', 1, true), 'batched deletion bypassed preflight')

-- Late failure commits only the successful prefix and renders final state once.
local drafts = {}
for _, id in ipairs({ 'cancel-a', 'cancel-b', 'cancel-c' }) do
  local item = comment(id)
  item.comment.origin, item.fetched, item.destination = 'draft', false, 'standalone'
  drafts[#drafts + 1] = item
end
reset(drafts)
vim.fn.writefile({ 'cancel-b' }, fixture .. '/fail_delete')
first = row('cancel-a') - 1
api.nvim_win_set_cursor(0, { first, 0 })
before = #requests('actions')
local before_prefix_render = home_changes()
input('V5ju')
wait(function() return #state().comments == 2 and notices[#notices]:find('Fixture deletion conflict', 1, true) end, 'batch failure did not stop after prefix')
local sent = requests('actions')
local result = records('action_results.jsonl')
assert(#sent == before + 1 and #sent[#sent].actions == 1 and sent[#sent].actions[1].action == 'undo'
  and vim.deep_equal(sent[#sent].actions[1].comments, { 'cancel-a', 'cancel-b', 'cancel-c' })
  and result[#result].applied == 0 and result[#result].failed_index == 0, 'semantic undo/prefix accounting changed')
assert(state().comments[1].comment.comment.id == 'cancel-b' and state().comments[2].comment.comment.id == 'cancel-c', 'failed action or suffix persisted')
assert(home_changes() == before_prefix_render + 1, 'prefix failure rendered intermediate state')

for _, mode in ipairs({ 'cached', 'mismatch' }) do
  local command = ('lua assert(loadfile(%q))(%q)'):format(repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/performance.lua')
  local child = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c', command }, { env = { BUF_REVIEW_COLD = mode }, text = true }):wait()
  assert(child.code == 0, child.stderr)
end
print('Neovim batched actions/lazy readers/freshness passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
