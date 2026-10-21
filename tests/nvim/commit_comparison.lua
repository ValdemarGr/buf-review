local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
local notices = {}
local history_ready, hold_objects, held_objects = false, false, nil
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n')) end
local old = { base = string.rep('a', 40), head = string.rep('b', 40) }
local latest = { base = old.base, head = string.rep('d', 40) }
local worktree = vim.deepcopy(old)
local commits = {
  { id = latest.head, subject = 'Newest change' },
  { id = string.rep('e', 40), subject = 'Earlier change' },
}
local initial = {
  id = 'commit-comparison', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = old,
  review = { id = 'commit-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Commit comparison', revision = latest },
    files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = {}, files = { { path = 'src/file.rs', viewed = true } }, todo = {}, done = {},
  pending = {}, conflicts = {}, publications = {}, new_commits = true, unpulled_commits = {},
  comparison_error = 'Git history unavailable\nMissing saved comparison commit',
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })

-- The shared fixture models transport; this boundary models Rust's new projection
-- and saved-comparison response without duplicating Git history logic in Lua.
local client = require('buf_review.client')
local native_request = client.request
local function project(view)
  view.comparison = vim.deepcopy(worktree)
  view.new_commits = view.comparison.head ~= latest.head
  view.unpulled_commits = history_ready and view.new_commits and vim.deepcopy(commits) or {}
  view.review.read.pull_request.revision = vim.deepcopy(latest)
  return view
end
client.request = function(root, request, callback)
  return native_request(root, request, function(response, err)
    if request.op == 'fetch_objects' and not err then
      history_ready = true
      response.workspace = project(read('state.json'))
      response.revision = vim.deepcopy(worktree)
      if hold_objects then held_objects = function() callback(response, err) end; return end
    end
    if response and response.workspace then
      project(response.workspace)
      if response.workspace.generation >= read('state.json').generation then write('state.json', response.workspace) end
    end
    callback(response, err)
  end)
end
vim.cmd('runtime plugin/buf_review.lua')
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(25)
end
local function requests(op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}) do
    local value = vim.json.decode(line)
    if value.op == op then result[#result + 1] = value end
  end
  return result
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end

vim.cmd('Review')
wait(function() return kind() == 'home' end, 'review home missing')
local home = text()
assert(#requests('fetch_objects') == 1, 'prepared history caused another object fetch')
assert(home:find('Error: Git history unavailable\nMissing saved comparison commit', 1, true), 'Git comparison error hidden')
assert(home:find('not at remote head', 1, true) and not home:find(':Review compare', 1, true), 'remote head mismatch suggests removed command')
local section = assert(home:find('Unpulled commits (2)', 1, true))
local newest = assert(home:find(latest.head:sub(1, 10) .. ' Newest change', 1, true))
local earlier = assert(home:find(commits[2].id:sub(1, 10) .. ' Earlier change', 1, true))
assert(section < newest and newest < earlier and earlier < home:find('Todo (0)', 1, true), 'commits not newest first above Todo')
row('Newest change')
require('buf_review').enter()
assert(kind() == 'home' and #requests('code') == 0 and #requests('actions') == 0, 'commit display triggered navigation or mutation')

-- Normal Git checkout controls the next projection without comparison events.
worktree = vim.deepcopy(latest)
vim.cmd('Review refresh')
wait(function() return not text():find('not at remote head', 1, true) end, 'refresh ignored checked-out HEAD')
assert(not text():find('Unpulled commits', 1, true), 'current HEAD retained unpulled commits')
worktree = vim.deepcopy(old)
vim.cmd('Review refresh')
wait(function() return text():find('Unpulled commits (2)', 1, true) end, 'checkout of old commit lost commit visibility')
assert(#requests('actions') == 0 and #requests('compare_saved') == 0, 'checkout wrote comparison events')

-- Open snapshots stay anchored until the user reopens their diff.
row('src/file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'diff did not open')
local panes = {}
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local buf = api.nvim_win_get_buf(win)
  local meta = vim.b[buf].buf_review
  if meta and meta.kind == 'diff' then panes[win] = buf end
end
worktree = vim.deepcopy(latest)
vim.cmd('Review refresh')
wait(function() return vim.deep_equal(read('state.json').comparison, latest) end, 'refresh did not derive HEAD')
for win, buf in pairs(panes) do
  assert(api.nvim_win_is_valid(win) and api.nvim_win_get_buf(win) == buf, 'refresh replaced open committed pane')
  assert(vim.deep_equal(vim.b[buf].buf_review.revision, old), 'refresh silently reanchored committed pane')
end
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'home missing before reopen')
row('src/file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' and vim.deep_equal(vim.b.buf_review.revision, latest) end, 'reopen did not capture new HEAD')

local generation = read('state.json').generation
vim.cmd('Review compare latest')
assert(notices[#notices]:find('Unknown Review command', 1, true), 'removed compare command still accepted')
assert(read('state.json').generation == generation, 'removed compare command mutated state')
assert(not vim.tbl_contains(vim.fn.getcompletion('Review ', 'cmdline'), 'compare'), 'removed compare command still completed')

-- An object preparation started before an accepted local action must never
-- replace the newer projection, even when comparison and review still match.
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'home missing before delayed preparation')
local fresh = read('state.json')
fresh.review.id = 'fresh-object-snapshot'
write('state.json', fresh)
hold_objects = true
vim.cmd('Review refresh')
wait(function() return held_objects ~= nil end, 'object response not held')
row('src/file.rs')
require('buf_review').progress(false, false, 'file')
wait(function() return not read('state.json').files[1].viewed and text():find('Unviewed files (1)', 1, true) end, 'local action not accepted during object preparation')
local deliver = held_objects
held_objects, hold_objects = nil, false
deliver()
ReviewTest.wait(40)
assert(text():find('Unviewed files (1)', 1, true) and text():find('Viewed files (0)', 1, true), 'stale object response replaced accepted local progress')
assert(#requests('actions') == 1, 'object preparation repeated local action')

print('Neovim commit comparison workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
