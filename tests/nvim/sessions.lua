local api = vim.api
local repository = vim.fn.getcwd()
local fixture = vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function requests(op)
  local result = {}
  local lines = vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}
  vim.list_extend(lines, vim.fn.filereadable(fixture .. '/action_attempts.jsonl') == 1 and vim.fn.readfile(fixture .. '/action_attempts.jsonl') or {})
  for _, line in ipairs(lines) do
    local request = vim.json.decode(line)
    if request.op == op then result[#result + 1] = request end
  end
  return result
end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(40)
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local function command(value) vim.cmd('Review ' .. value) end
local function enter() require('buf_review').enter() end
local function counts(values)
  return vim.tbl_extend('force', { comments = 0, edits = 0, deletions = 0, reactions = 0, files = 0, conclusions = 0, total = 0 }, values or {})
end
local function stats(status)
  return { status = status, unfinished = status == 'draft', pending = counts({ comments = 2, edits = 1, files = 1, total = 4 }), published = counts({ comments = 1, total = 1 }), conflicts = 0 }
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local first = {
  id = 'batch-first', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'snapshot-first', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'First PR', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'src/file.rs', viewed = false } }, todo = {}, done = {}, pending = {}, conflicts = {}, new_commits = false,
  stats = stats('draft'), publications = {},
}
local publication = { id = 'publication-1', generation = 3, destination = 'review', status = 'submitted', planned = counts({ comments = 1, conclusions = 1, total = 2 }), published = counts({ comments = 1, conclusions = 1, total = 2 }) }
first.publications = { publication }
local second = vim.deepcopy(first)
second.id, second.publications = 'batch-second', {}
local other = vim.deepcopy(second)
other.id, other.locator, other.stats = 'batch-other', 'https://github.com/acme/repo/pull/42', stats('published')
other.review.read.pull_request.id, other.review.read.pull_request.title = 'PR-42', 'Other PR'
local views = { [first.id] = first, [second.id] = second, [other.id] = other }
write('state.json', first)
write('views.json', views)
local pulls = {
  { number = 1, title = 'First PR', url = first.locator, author = 'alice', assignees = {} },
  { number = 42, title = 'Other PR', url = other.locator, author = 'bob', assignees = {} },
}
local routes = { entry = { route = 'prs', pull_requests = pulls }, [first.locator] = { route = 'batch', batch = first.id }, [other.locator] = { route = 'batch', batch = other.id } }
write('routes.json', routes)
local groups = {
  { pr = { id = 'PR-1', title = 'First PR', number = 1, url = first.locator }, sessions = {
    { id = first.id, reviewer = first.review.reviewer, active = true, stats = first.stats },
    { id = second.id, reviewer = first.review.reviewer, active = false, stats = second.stats },
  } },
  { pr = { id = 'PR-42', title = 'Other PR', number = 42, url = other.locator }, sessions = {
    { id = other.id, reviewer = other.review.reviewer, active = false, stats = other.stats },
  } },
}
write('sessions.json', { groups = groups })
local history_body = 'Published review comment\n\n```rust\nkeep_exact();\n```'
write('history.json', { summary = publication, operations = {
  { operation = { id = 'create-1', change = { kind = 'create_comment', comment = { id = 'comment-1', body = history_body, author = first.review.reviewer, target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision } } } } }, published = true, response = { id = 17, url = first.locator .. '#discussion_r17' } },
} })

api.nvim_buf_set_name(0, repository .. '/session-navigation-source.rs')
api.nvim_buf_set_lines(0, 0, -1, false, { 'source stays exact' })
local source, source_tab = api.nvim_get_current_buf(), api.nvim_get_current_tabpage()
command('')
wait(function() return kind() == 'prs' end, 'source zero/many branch route did not show remote PRs')
assert(#requests('entry') == 1 and #requests('select') == 0, 'source entry reused cached selection')
row('#1 First PR')
vim.fn.writefile({ '0.2' }, fixture .. '/delay_fetch_objects')
enter()
wait(function() return kind() == 'loading' and #requests('select') == 1 end, 'first selected session skipped initial wait')
command('prs')
assert(#requests('list') == 0, 'duplicate navigation started while first session opened')
wait(function() return kind() == 'home' end, 'selected session did not open')
vim.fn.delete(fixture .. '/delay_fetch_objects')
local first_home, review_tab = api.nvim_get_current_buf(), api.nvim_get_current_tabpage()
assert(review_tab == source_tab and #api.nvim_list_tabpages() == 1, 'Review did not use the invoking tab')
assert(vim.b.buf_review.workspace == first.id and text():find('Session: batch-first', 1, true), 'home lost canonical batch identity')
assert(text():find('Publication: draft', 1, true) and text():find('Pending: 2 comments, 1 edit, 1 file', 1, true), 'home omitted backend statistics')
row('Review · submitted')
enter()
wait(function() return kind() == 'history' end, 'publication details did not open')
assert(vim.bo.readonly and not vim.bo.modifiable and vim.bo.filetype == 'markdown')
assert(text():find(history_body, 1, true) and text():find('published', 1, true) and text():find('discussion_r17', 1, true), 'history lost body/result details')
local before_entry = #requests('entry')
command('')
wait(function() return kind() == 'home' end, 'bound history did not return home')
assert(#requests('entry') == before_entry, 'bound :Review rerouted source branch')

vim.fn.writefile({ '0.2' }, fixture .. '/delay_code')
row('src/file.rs'); enter()
wait(function() return #requests('code') == 1 end, 'delayed code request missing')
command('sessions')
wait(function() return kind() == 'sessions' end, 'browser missing during code read')
ReviewTest.wait(250)
assert(kind() == 'sessions' and #api.nvim_tabpage_list_wins(0) == 1,
  'late code response replaced session browser: ' .. vim.inspect({ kind = kind(), windows = api.nvim_tabpage_list_wins(0), notices = notices }))
vim.fn.delete(fixture .. '/delay_code')
local resumed = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n'))
resumed.pr.title = 'First PR resumed after code read'
local resumed_views = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/views.json'), '\n'))
resumed_views[first.id] = resumed
write('state.json', resumed)
write('views.json', resumed_views)
vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
row('batch-first'); enter()
wait(function()
  return kind() == 'home' and vim.b.buf_review.workspace == first.id and api.nvim_buf_get_lines(0, 1, 2, false)[1] == '  ' .. resumed.pr.title
end, 'first resume after code read missing')
local resumed_fetches = #requests('fetch_refresh')
ReviewTest.wait(350)
assert(#requests('fetch_refresh') == resumed_fetches, 'resumed cached session fetched automatically')
vim.fn.delete(fixture .. '/delay_refresh')

-- A bound pull may finish after selection changes; it must preserve new layout.
vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
local before_refresh, switched = #requests('fetch_refresh'), false
vim.defer_fn(function()
  command('sessions')
  wait(function() return kind() == 'sessions' end, 'all sessions browser missing during pull')
  assert(row('#1 First PR') < row('#42 Other PR'), 'browser changed backend group order')
  assert(text():find('batch-first', 1, true) and text():find('batch-second', 1, true) and text():find('batch-other', 1, true), 'browser omitted sessions')
  row('batch-second'); enter()
  wait(function() return kind() == 'home' and vim.b.buf_review.workspace == second.id end, 'second session did not open during pull')
  switched = true
end, 60)
command('pull')
assert(switched and #requests('fetch_refresh') > before_refresh, 'pull failed to exercise concurrent session navigation')
vim.fn.delete(fixture .. '/delay_refresh')
assert(api.nvim_get_current_tabpage() == review_tab and #api.nvim_list_tabpages() == 1, 'session switching created another review tab')
assert(kind() == 'home' and vim.b.buf_review.workspace == second.id, 'late pull selected its old session')
assert(api.nvim_buf_get_lines(first_home, 0, -1, false)[6] == 'Session: batch-first', 'old home was retargeted')

-- A source invocation always resolves the branch again, despite selected state.
vim.cmd('tabnew')
api.nvim_set_current_buf(source)
source_tab = api.nvim_get_current_tabpage()
routes.entry = { route = 'batch', batch = first.id }
write('routes.json', routes)
command('')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == first.id end, 'source branch route did not resume returned batch')
assert(#requests('entry') == before_entry + 1 and api.nvim_get_current_tabpage() == source_tab
  and #api.nvim_list_tabpages() == 2, 'source entry skipped resolution or left the invoking tab')
assert(vim.bo[source].modified and api.nvim_buf_get_lines(source, 0, -1, false)[1] == 'source stays exact')

-- Multiple unfinished sessions use the same browser with a PR filter.
routes[first.locator] = { route = 'sessions', sessions = { pr = 'PR-1', groups = { groups[1] } } }
write('routes.json', routes)
command('prs')
wait(function() return kind() == 'prs' end, 'explicit PR picker missing')
row('#1 Fixture review')
local before_select = #requests('select')
enter()
wait(function() return kind() == 'sessions' end, 'ambiguous PR did not show filtered sessions')
assert(text():find('batch-first', 1, true) and not text():find('batch-other', 1, true) and #requests('select') == before_select, 'Lua selected an arbitrary unfinished session')
local before_browser_refresh = #requests('sessions')
command('pull')
wait(function() return #requests('sessions') > before_browser_refresh end, 'session browser refresh did not reread local state')
assert(requests('sessions')[#requests('sessions')].pr == 'PR-1', 'session refresh dropped PR filter')
command('sessions')
wait(function() return kind() == 'sessions' and text():find('batch-other', 1, true) end, 'all browser retained old PR filter')
assert(requests('sessions')[#requests('sessions')].pr == nil, 'all browser sent a PR filter')

-- Explicit selections serialize; a second Enter/new cannot race DB selection.
vim.fn.writefile({ '0.2' }, fixture .. '/delay_select')
row('batch-second')
before_select = #requests('select')
enter()
wait(function() return #requests('select') == before_select + 1 end, 'delayed select missing')
row('batch-first')
enter()
command('sessions new')
assert(#requests('select') == before_select + 1 and #requests('new') == 0, 'navigation allowed concurrent selection/new')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == second.id end, 'serialized selection did not finish')
vim.fn.delete(fixture .. '/delay_select')

-- A late UI read cannot replace a newer browser, even within the same session.
command('sessions')
wait(function() return kind() == 'sessions' end, 'sessions browser missing')
row('batch-first'); enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == first.id end, 'first resume missing')
vim.fn.writefile({ '0.3' }, fixture .. '/delay_history')
row('Review · submitted'); enter()
wait(function() return #requests('history') == 2 end, 'delayed history missing')
command('sessions')
wait(function() return kind() == 'sessions' end, 'sessions missing during history read')
ReviewTest.wait(350)
assert(kind() == 'sessions', 'late history replaced a newer browser in the same session')
row('batch-first'); enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == first.id end, 'first resume before late-session history missing')
row('Review · submitted'); enter()
wait(function() return #requests('history') == 3 end, 'second delayed history missing')
command('sessions')
wait(function() return kind() == 'sessions' end, 'sessions missing during second history read')
row('batch-second'); enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == second.id end, 'switch during history missing')
ReviewTest.wait(350)
assert(kind() == 'home' and vim.b.buf_review.workspace == second.id and #api.nvim_tabpage_list_wins(0) == 1, 'late history stole selected layout')
vim.fn.delete(fixture .. '/delay_history')

command('prs')
wait(function() return kind() == 'prs' end, 'PR picker missing before cross-PR new')
row('#42 Other review'); enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == other.id end, 'other PR did not select')
command('prs')
wait(function() return kind() == 'prs' end, 'PR picker missing before filtered new')
row('#1 Fixture review'); enter()
wait(function() return kind() == 'sessions' end, 'filtered sessions missing before new')
api.nvim_win_set_cursor(0, { 1, 0 })
command('sessions new')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace:find('new-batch-', 1, true) end, 'new session did not select distinct batch')
assert(requests('new')[1].pr == 'PR-1' and text():find(first.locator, 1, true), 'filtered header new fell back to previous PR')
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment standalone')
api.nvim_buf_set_lines(0, 0, -1, false, { 'unsaved session draft' })
local composer = api.nvim_get_current_buf()
before_select = #requests('select')
local before_sessions = #requests('sessions')
command('sessions')
assert(api.nvim_get_current_buf() == composer and vim.bo.modified and api.nvim_get_current_line() == 'unsaved session draft', 'session navigation discarded modified composer')
assert(#requests('sessions') == before_sessions and #requests('select') == before_select, 'dirty composer allowed session navigation')
api.nvim_feedkeys(api.nvim_replace_termcodes(':q!<CR>', true, false, true), 'xt', false)
ReviewTest.wait(60)

vim.fn.writefile({ '0.2' }, fixture .. '/delay_list')
local before_list = #requests('list')
command('prs')
wait(function() return #requests('list') > before_list end, 'delayed PR list missing')
command('comment standalone')
api.nvim_buf_set_lines(0, 0, -1, false, { 'typed while navigation waited' })
composer = api.nvim_get_current_buf()
ReviewTest.wait(250)
assert(api.nvim_get_current_buf() == composer and vim.bo.modified and api.nvim_get_current_line() == 'typed while navigation waited', 'late navigation discarded a newly modified composer')
vim.fn.delete(fixture .. '/delay_list')
api.nvim_feedkeys(api.nvim_replace_termcodes(':q!<CR>', true, false, true), 'xt', false)
ReviewTest.wait(60)

command('prs')
wait(function() return kind() == 'prs' end, 'PR picker missing before explicit remote new')
row('#42 Other review')
command('sessions new')
wait(function() return kind() == 'home' and text():find(other.locator, 1, true) end, 'new session from selected remote PR missing')
assert(requests('new')[2].url == other.locator and requests('new')[2].workspace == nil, 'remote new used previous session context')

command('prs assignee=@me state=closed')
wait(function() return kind() == 'prs' and text():find('state=closed', 1, true) end, 'filtered PR picker missing')
routes.entry = { route = 'prs', pull_requests = pulls }
write('routes.json', routes)
vim.cmd('tabnew')
api.nvim_set_current_buf(source)
command('')
wait(function() return kind() == 'prs' and text():find('state=open assignee=all', 1, true) end, 'source branch fallback inherited stale PR filters')

print('Neovim sessions/routing workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
