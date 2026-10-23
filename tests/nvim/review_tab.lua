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
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function view(id, number)
  return {
    id = id, generation = 0, locator = 'https://github.com/acme/repo/pull/' .. number, comparison = revision,
    review = { id = 'snapshot-' .. id, reviewer = { id = 'me', name = 'me' }, read = {
      pull_request = { id = 'PR-' .. number, title = 'PR ' .. number, revision = revision },
      files = {}, comments = {}, reactions = {}, reviews = {},
    } },
    comments = {}, files = { { path = 'file.rs', viewed = false } }, pending = {}, todo = {}, done = {},
    conflicts = {}, publications = {}, new_commits = false,
  }
end
local first, second = view('tab-first', 1), view('tab-second', 2)
write('state.json', first)
write('views.json', { [first.id] = first, [second.id] = second })
write('routes.json', {
  entry = { route = 'batch', batch = first.id, fresh_review = first.review.id },
  [second.locator] = { route = 'batch', batch = second.id, fresh_review = second.review.id },
})
write('pull_requests.json', { { number = 2, title = 'PR 2', url = second.locator, author = 'alice', assignees = {} } })
local edit_path = fixture .. '/editable.rs'
vim.fn.writefile({ 'head' }, edit_path)
write('code_contexts.json', { ['file.rs'] = { path = 'file.rs', revision = revision, base_content = 'base\n', head_content = 'head\n',
  local_path = edit_path, local_dirty = false, binary = false } })
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function command(value) vim.cmd('Review ' .. (value or '')) end
local function row(needle)
  for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if text:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local function assert_tab(page, count)
  assert(api.nvim_get_current_tabpage() == page and #api.nvim_list_tabpages() == count, 'Review created or selected another tab')
  assert(vim.t[page].buf_review.repository == repository, 'Review tab lost repository marker')
end

local source_tab, source = api.nvim_get_current_tabpage(), api.nvim_get_current_buf()
api.nvim_buf_set_name(source, repository .. '/review-tab-unsaved-source.rs')
api.nvim_buf_set_lines(source, 0, -1, false, { 'unsaved source' })
command()
wait(function() return kind() == 'home' end, 'initial Review missing')
local review = api.nvim_get_current_tabpage()
assert(review ~= source_tab, 'Review reused ordinary source tab')
assert_tab(review, 2)

-- The tab marker survives replacing the only Review buffer with native editing.
local native = api.nvim_create_buf(true, false)
api.nvim_buf_set_name(native, fixture .. '/ordinary-outside-git.txt')
api.nvim_buf_set_lines(native, 0, -1, false, { 'keep native unsaved text' })
api.nvim_set_current_buf(native)
assert(vim.b.buf_review == nil, 'native replacement inherited Review metadata')
command()
wait(function() return kind() == 'home' end, 'Review did not recover from native replacement')
assert_tab(review, 2)
assert(api.nvim_buf_is_loaded(native) and vim.bo[native].modified
  and api.nvim_buf_get_lines(native, 0, -1, false)[1] == 'keep native unsaved text', 'Review discarded native edits')

-- Replacing a pane does not make other user windows owned by Review.
vim.cmd('vsplit')
local extra = api.nvim_get_current_win()
api.nvim_win_set_buf(extra, native)
command()
wait(function() return kind() == 'home' end, 'Review did not return beside native window')
assert_tab(review, 2)
assert(api.nvim_win_is_valid(extra) and api.nvim_win_get_buf(extra) == native, 'Review closed another native window')
api.nvim_set_current_win(extra)
vim.cmd('only')
command()
wait(function() return kind() == 'home' end, 'Review did not recover after only')
assert_tab(review, 2)

-- Wiping all bound readers cannot remove the tab's identity.
api.nvim_set_current_buf(native)
for _, buf in ipairs(api.nvim_list_bufs()) do
  if vim.b[buf].buf_review then api.nvim_buf_delete(buf, { force = true }) end
end
assert(vim.b.buf_review == nil, 'wipe left a bound Review reader')
command()
wait(function() return kind() == 'home' end, 'Review did not recover after readers wiped')
assert_tab(review, 2)

api.nvim_set_current_tabpage(source_tab)
command()
wait(function() return kind() == 'home' end, 'Review did not focus marked tab from ordinary tab')
assert_tab(review, 2)
assert(api.nvim_buf_get_lines(source, 0, -1, false)[1] == 'unsaved source' and vim.bo[source].modified,
  'Review modified ordinary source tab')

-- PR and session navigation share the same marked tab.
command('prs')
wait(function() return kind() == 'prs' end, 'PR browser missing')
row('#2 PR 2'); require('buf_review').enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == second.id end, 'second PR missing')
assert_tab(review, 2)
command('sessions new')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace ~= second.id end, 'new session missing')
local session = vim.b.buf_review.workspace
assert_tab(review, 2)
api.nvim_set_current_buf(native)
command()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == session end, 'tab marker lost current session')
assert_tab(review, 2)

row('file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'diff missing')
vim.cmd('only')
command()
wait(function() return kind() == 'home' end, 'Review did not recover from surviving right diff pane')
assert_tab(review, 2)

row('file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'diff before local editing missing')
api.nvim_feedkeys('o', 'xt', false)
wait(function() return api.nvim_buf_get_name(0) == edit_path end, 'o did not open local buffer')
local edit_buffer = api.nvim_get_current_buf()
api.nvim_buf_set_lines(edit_buffer, 0, -1, false, { 'keep local edit' })
assert(vim.b.buf_review == nil, 'local editing acquired Review metadata')
assert_tab(review, 2)
vim.cmd('only')
command()
wait(function() return kind() == 'home' end, 'Review did not recover from only native editing pane')
assert_tab(review, 2)
assert(api.nvim_buf_is_loaded(edit_buffer) and vim.bo[edit_buffer].modified
  and api.nvim_buf_get_lines(edit_buffer, 0, -1, false)[1] == 'keep local edit', 'tab recovery discarded local edit')

row('file.rs'); require('buf_review').enter()
wait(function() return api.nvim_get_current_buf() == edit_buffer end, 'local editing pane did not reopen')
local editing_window = api.nvim_get_current_win()
vim.cmd('vsplit')
local ordinary_window = api.nvim_get_current_win()
api.nvim_win_set_buf(ordinary_window, native)
for _, win in ipairs(api.nvim_tabpage_list_wins(review)) do
  if vim.b[api.nvim_win_get_buf(win)].buf_review then api.nvim_win_close(win, true) end
end
api.nvim_set_current_win(editing_window)
command()
wait(function() return kind() == 'home' end, 'Review did not reuse surviving owned pane')
assert_tab(review, 2)
assert(api.nvim_win_is_valid(ordinary_window) and api.nvim_win_get_buf(ordinary_window) == native,
  'recovering a surviving pane replaced another user window')
api.nvim_win_close(ordinary_window, true)

-- Closed marked tabs are recreated once; old native buffers remain intact.
vim.cmd('tabclose')
assert(api.nvim_get_current_tabpage() == source_tab, 'closing Review changed ordinary tab')
command()
wait(function() return kind() == 'home' end, 'closed Review tab was not recreated')
local recreated = api.nvim_get_current_tabpage()
assert(recreated ~= review, 'closed tab handle was reused')
assert_tab(recreated, 2)
command()
wait(function() return kind() == 'home' end, 'repeated Review missing')
assert_tab(recreated, 2)
assert(api.nvim_buf_is_loaded(native) and api.nvim_buf_get_lines(native, 0, -1, false)[1] == 'keep native unsaved text',
  'Review recreation deleted native buffer')

print('Neovim marked Review tab reuse passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
