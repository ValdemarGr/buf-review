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
    local value = vim.json.decode(line)
    if not op or value.op == op then result[#result + 1] = value end
  end
  return result
end
local function requests(op) return records('requests.jsonl', op) end
local function completions() return #records('fetch_objects_complete.jsonl') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(value) vim.cmd('Review ' .. value) end
local function enter() require('buf_review').enter() end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. vim.inspect(records('requests.jsonl')))
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if (needle == 'src/file.rs' and value:sub(-#needle - 2) == '  ' .. needle)
      or (needle ~= 'src/file.rs' and value:find(needle, 1, true)) then
      api.nvim_win_set_cursor(0, { line, 0 }); return
    end
  end
  error('Missing row: ' .. needle)
end
local function home(batch)
  return kind() == 'home' and vim.b.buf_review.workspace == batch.id
end
local function make_view(number)
  local revision = { base = string.rep('a', 40), head = string.rep(tostring(number), 40) }
  local comment = {
    comment = { origin = 'read', comment = { id = 'comment-' .. number, body = 'Feedback ' .. number, author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision, range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } } } },
    fetched = true, done = false, conflict = false, summary = false,
  }
  return {
    id = 'objects-batch-' .. number, generation = 0, locator = 'https://github.com/acme/repo/pull/' .. number, comparison = revision,
    review = { id = 'snapshot-' .. number, reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-' .. number, title = 'PR ' .. number, revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
    comments = { comment }, files = { { path = 'src/file.rs', viewed = false } }, todo = { 'comment-' .. number }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
  }
end
local first, second, third, fourth = make_view(1), make_view(2), make_view(3), make_view(4)
local views, routes, pulls = {}, {}, {}
for _, view in ipairs({ first, second, third, fourth }) do
  views[view.id] = view
  routes[view.locator] = { route = 'batch', batch = view.id, fresh_review = view.review.id }
  pulls[#pulls + 1] = { number = #pulls + 1, title = view.review.read.pull_request.title, url = view.locator, author = 'alice', assignees = {} }
end
routes.entry = routes[first.locator]
write('state.json', first)
write('views.json', views)
write('routes.json', routes)
write('pull_requests.json', pulls)
vim.fn.writefile({ '0.8' }, fixture .. '/delay_fetch_objects')
local function select(view)
  command('prs')
  wait(function() return kind() == 'prs' end, 'PR browser missing')
  row('#' .. view.locator:match('/(%d+)$') .. ' ')
  enter()
end

-- Opening a PR waits for Git preparation while Neovim's event loop stays responsive.
command('')
wait(function() return #requests('fetch_objects') == 1 end, 'selection did not prepare pinned objects')
assert(completions() == 0 and kind() == 'loading', 'review became usable before required objects were prepared')
local responsive = false
vim.defer_fn(function() responsive = true end, 20)
assert(ReviewTest.wait(100, function() return responsive end, 5), 'Git object fetch blocked event loop')
assert(#requests('code') == 0 and #requests('actions') == 0, 'loading started code or mutation')
wait(function() return home(first) and completions() == 1 end, 'prepared review home missing')
row('Feedback 1')
require('buf_review').progress(true)
wait(function() return read('state.json').generation == 1 end, 'local staging failed after object fetch')
row('src/file.rs'); enter()
wait(function() return kind() == 'diff' end, 'code did not open after object preparation')
assert(#requests('code') == 1 and #requests('fetch_objects') == 1 and read('state.json').generation == 1, 'prepared code fetched again or changed generation')
local fetch = requests('fetch_objects')[1]
assert(fetch.workspace == first.id and fetch.generation == nil, 'fetch was sent as generation-bound mutation')

-- Retained bound panes can switch PRs while the current PR waits for preparation.
vim.cmd('split')
local retained_window = api.nvim_get_current_win()
select(second)
wait(function() return kind() == 'loading' and #requests('fetch_objects') == 2 end, 'second PR preparation missing')
api.nvim_set_current_win(retained_window)
select(third)
wait(function() return home(third) and #requests('fetch_objects') == 3 end, 'switch during object fetch blocked navigation')
wait(function() return completions() == 3 end, 'background preparations did not complete')
ReviewTest.wait(60)
assert(home(third) and #requests('code') == 1, 'old preparation opened code or retargeted current PR')
assert(api.nvim_win_is_valid(retained_window), 'PR switch closed user-owned bound pane')
api.nvim_win_close(retained_window, true)
assert(#api.nvim_tabpage_list_wins(0) == 1, 'managed PR panes accumulated during preparation')

-- Changing checkout while a read waits discards obsolete code callbacks.
local latest = { base = string.rep('c', 40), head = string.rep('d', 40) }
local checked_out = read('state.json')
checked_out.comparison = latest
write('state.json', checked_out)
command('')
wait(function() return home(third) and #requests('fetch_objects') == 4 end, 'new comparison did not prepare new pins')
row('src/file.rs'); enter()
checked_out = read('state.json')
checked_out.comparison = { base = string.rep('e', 40), head = string.rep('f', 40) }
write('state.json', checked_out)
command('')
wait(function() return home(third) and #requests('fetch_objects') == 5 end, 'second comparison reused old preparation')
wait(function() return completions() == 5 end, 'updated comparison preparations did not complete')
ReviewTest.wait(60)
assert(home(third) and #requests('code') == 1, 'obsolete comparison callback opened deferred code')
assert(vim.deep_equal(records('fetch_objects_complete.jsonl')[5].response.revision, read('state.json').comparison), 'fetch did not prepare exact latest comparison')
row('src/file.rs'); enter()
wait(function() return kind() == 'diff' and #requests('code') == 2 end, 'prepared latest comparison code did not open')
assert(#requests('fetch_objects') == 5, 'ready comparison fetched again when opening code')
vim.fn.delete(fixture .. '/delay_fetch_objects')

-- Network failures preserve local work; opening code and refresh can retry.
vim.fn.writefile({ 'Fetch pinned commit failed: remote unavailable' }, fixture .. '/fail_fetch_objects')
select(fourth)
wait(function() return home(fourth) and completions() == 6 and #notices > 0 and notices[#notices]:find('remote unavailable', 1, true) end, 'fetch failure lacked useful notification or hid home')
row('Feedback 4'); require('buf_review').progress(true)
wait(function() return read('state.json').generation == 1 and read('state.json').comments[1].done end, 'failed object fetch disabled staging')
row('src/file.rs'); enter()
wait(function() return completions() == 7 end, 'opening code did not retry failed fetch')
ReviewTest.wait(50)
assert(home(fourth) and #requests('code') == 2, 'failed retry opened code')
vim.fn.delete(fixture .. '/fail_fetch_objects')
command('pull')
wait(function() return completions() == 8 and home(fourth) end, 'explicit refresh did not retry failed preparation')
row('src/file.rs'); enter()
wait(function() return kind() == 'diff' and #requests('code') == 3 end, 'code did not recover after successful retry')
assert(#requests('fetch_objects') == 8, 'successful retry did not mark comparison ready')

-- Explicit new sessions prepare their own bound comparison without mutation.
command('sessions new')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace:find('new-batch-', 1, true) and #requests('fetch_objects') == 9 end, 'new session omitted object preparation')
wait(function() return completions() == 9 end, 'new session object preparation did not finish')
assert(read('state.json').generation == 0, 'new session preparation wrote workspace history')

print('Neovim pinned-object preparation workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
