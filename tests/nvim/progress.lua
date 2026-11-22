local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_progress.py' })
local client = require('buf_review.client')
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason)
end

-- Stderr chunks and EOF are independent of event boundaries. Progress arrives
-- while the process runs and the last unterminated event precedes its result.
local events, finished = {}, false
client.request(repository, { op = 'stream' }, function(response, err)
  assert(not err and response.ok and #events == 2, 'final response ran before all progress callbacks')
  finished = true
end, function(event) events[#events + 1] = event end)
wait(function() return #events == 1 end, 'split stderr event did not stream before process exit')
assert(not finished and events[1].completed == 1, 'progress was buffered until exit')
wait(function() return finished end, 'stream did not finish')
assert(events[2].completed == 2 and events[2].total == 2, 'EOF progress event lost counts')

finished, events = false, {}
client.request(repository, { op = 'failure' }, function(response, err)
  assert(not response and #events == 1 and err:find('Actual provider diagnostic', 1, true)
    and err:find('Final diagnostic without newline', 1, true) and err:find('"wrong"', 1, true),
    'stderr parser lost non-progress diagnostics')
  assert(not err:find('publish_comments', 1, true), 'progress JSON polluted provider error')
  finished = true
end, function(event) events[#events + 1] = event end)
wait(function() return finished end, 'failure did not finish')

local echoes, native_echo = {}, api.nvim_echo
api.nvim_echo = function(chunks, history, opts)
  assert(history == false, 'progress polluted message history')
  echoes[#echoes + 1] = chunks[1][1]
  return native_echo(chunks, history, opts)
end
local progress = require('buf_review.progress')
local old = progress.start('Old request')
old.update({ message = 'Preparing' })
local latest = progress.start('New request')
latest.update({ message = 'Comments', completed = 8, total = 18 })
local count, window, buffer, cursor = #echoes, api.nvim_get_current_win(), api.nvim_get_current_buf(), api.nvim_win_get_cursor(0)
old.update({ message = 'Stale update' }); old.finish()
assert(#echoes == count and echoes[count]:find('Comments 8/18', 1, true), 'old request overwrote active progress')
assert(api.nvim_get_current_win() == window and api.nvim_get_current_buf() == buffer
  and vim.deep_equal(api.nvim_win_get_cursor(0), cursor), 'progress moved editor context')
vim.cmd('tabnew')
latest.update({ message = 'Background update' }); latest.finish()
assert(#echoes == count, 'other-tab request overwrote command line')
vim.cmd('tabclose')

local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n')) end
local function records()
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}) do result[#result + 1] = vim.json.decode(line) end
  return result
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local draft = { comment = { origin = 'draft', comment = { id = 'comment', body = 'Comment', author = { id = 'me', name = 'me' }, target = { kind = 'pull_request' } } }, destination = 'standalone', fetched = false }
local initial = {
  id = 'progress', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Progress', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { draft }, files = { { path = 'src/file.rs', viewed = false } }, pending = { { id = 'operation', change = { kind = 'create_comment', comment = draft.comment.comment } } }, todo = {}, done = {}, conflicts = {}, publications = {},
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
write('post_publish.json', { tag = 'progress', operation = 'publish', ['local'] = { pending = {}, publications = { { id = 'publication', status = 'published', planned = { comments = 1, total = 1 }, published = { comments = 1, total = 1 } } } }, remote = { pending = {} } })
vim.cmd('runtime plugin/buf_review.lua')
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
vim.cmd('Review')
wait(function() return kind() == 'home' and echoes[#echoes]:find('Complete', 1, true) end, 'initial fetch/preparation progress did not finish')
local saw_fetch, saw_prepare = false, false
for _, message in ipairs(echoes) do
  saw_fetch = saw_fetch or message:find('Fetching PR', 1, true)
  saw_prepare = saw_prepare or message:find('Preparing Git objects 2/2', 1, true)
end
assert(saw_fetch and saw_prepare, 'initial review progress omitted fetch or Git preparation')

echoes = {}
local publish_complete = false
api.nvim_echo = function(chunks, history, opts)
  local message = chunks[1][1]
  if message:find('Publish · Complete', 1, true) then
    local applied = false
    for _, request in ipairs(records()) do applied = applied or request.op == 'apply_refresh' end
    assert(applied, 'publication progress completed before final refresh applied')
    publish_complete = true
  end
  echoes[#echoes + 1] = message
  return native_echo(chunks, history, opts)
end
window, buffer, cursor = api.nvim_get_current_win(), api.nvim_get_current_buf(), api.nvim_win_get_cursor(0)
api.nvim_feedkeys(api.nvim_replace_termcodes(':Review publish<CR>', true, false, true), 'xt', false)
assert(publish_complete and api.nvim_get_current_win() == window and api.nvim_get_current_buf() == buffer,
  'blocking publication lost progress completion or editor context')
local comments, refreshed
for index, message in ipairs(echoes) do
  if message:find('Comments 2/2', 1, true) then comments = index end
  if message:find('Refreshing PR state', 1, true) then refreshed = index end
end
assert(comments and refreshed and comments < refreshed and echoes[#echoes]:find('Complete', 1, true),
  'publication did not progress through comments then final refresh')

-- Pull owns the full fetch/apply/preparation wait. Opening prepared code needs
-- no second preparation or progress workflow.
echoes = {}
local changed = read('state.json')
changed.comparison.head = string.rep('c', 40)
write('state.json', changed)
api.nvim_feedkeys(api.nvim_replace_termcodes(':Review pull<CR>', true, false, true), 'xt', false)
assert(echoes[#echoes] and echoes[#echoes]:find('Pull · Complete', 1, true), 'pull returned before completion')
local prepared = false
for _, message in ipairs(echoes) do prepared = prepared or message:find('Pull · Preparing Git objects 2/2', 1, true) end
assert(prepared, 'pull completed without streamed Git preparation counts')
local preparations = 0
for _, request in ipairs(records()) do if request.op == 'fetch_objects' then preparations = preparations + 1 end end
count = #echoes
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:sub(-#'src/file.rs' - 2) == '  src/file.rs' then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'foreground prepared code did not open')
local opened_preparations = 0
for _, request in ipairs(records()) do if request.op == 'fetch_objects' then opened_preparations = opened_preparations + 1 end end
assert(opened_preparations == preparations and #echoes == count, 'prepared code started another progress workflow')

-- A deferred fetch/apply is not a successful pull. Keep current data visible,
-- report the retry instruction, and avoid preparing an unapplied snapshot.
local request, notify = client.request, vim.notify
local notices = {}
vim.notify = function(message) notices[#notices + 1] = message end
client.request = function(root, payload, callback, on_progress)
  return request(root, payload, function(response, err)
    if payload.op == 'fetch_refresh' and response and response.prepared_refresh then
      response.prepared_refresh.review = nil
      response.prepared_refresh.deferred = true
    end
    callback(response, err)
  end, on_progress)
end
echoes = {}
api.nvim_feedkeys(api.nvim_replace_termcodes(':Review pull<CR>', true, false, true), 'xt', false)
client.request, vim.notify = request, notify
assert(echoes[#echoes]:find('Pull · Finished with errors', 1, true), 'deferred pull falsely completed')
assert(notices[#notices] and notices[#notices]:find('Pull deferred; remote state was not applied. Run :Review pull to retry.', 1, true),
  'deferred pull lost retry instruction')
local deferred_preparations = 0
for _, entry in ipairs(records()) do if entry.op == 'fetch_objects' then deferred_preparations = deferred_preparations + 1 end end
assert(deferred_preparations == preparations and kind() == 'diff', 'deferred pull prepared or replaced current snapshot')

-- A second caller joins preparation already in flight. Its active progress
-- owner must receive the remaining counts without another fetch/preparation.
echoes = {}
local joined_at, joined_finished
local joined_requests = #records()
client.request = function(root, payload, callback, on_progress)
  if payload.op == 'fetch_objects' then
    vim.defer_fn(function()
      joined_at = #echoes + 1
      api.nvim_feedkeys(api.nvim_replace_termcodes(':Review pull<CR>', true, false, true), 'xt', false)
      joined_finished = true
    end, 50)
  end
  return request(root, payload, callback, on_progress)
end
api.nvim_feedkeys(api.nvim_replace_termcodes(':Review pull<CR>', true, false, true), 'xt', false)
client.request = request
assert(joined_finished, 'second pull did not finish inside shared preparation')
local joined_prepared, joined_fetches, joined_objects = false, 0, 0
for index = joined_at, #echoes do
  joined_prepared = joined_prepared or echoes[index]:find('Pull · Preparing Git objects 2/2', 1, true)
end
for index, entry in ipairs(records()) do
  if index > joined_requests then
    if entry.op == 'fetch_refresh' then joined_fetches = joined_fetches + 1 end
    if entry.op == 'fetch_objects' then joined_objects = joined_objects + 1 end
  end
end
assert(joined_fetches == 1 and joined_objects == 1, 'coalesced pull duplicated fetch/preparation')
assert(joined_prepared, 'coalesced pull lost remaining Git preparation counts')

-- Native borrowed-window context must not make a delayed browser look visible
-- while the user is actually in another tab.
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'home before delayed browser missing')
vim.fn.writefile({ '0.15' }, fixture .. '/delay_list')
vim.cmd('Review prs')
local source = api.nvim_get_current_win()
vim.cmd('tabnew')
local other_tab = api.nvim_get_current_tabpage()
count = #echoes
wait(function()
  local marker = vim.b[api.nvim_win_get_buf(source)].buf_review
  return marker and marker.kind == 'prs'
end, 'delayed browser did not finish in original window')
assert(api.nvim_get_current_tabpage() == other_tab and #echoes == count,
  'borrowed old-tab context leaked progress into visible tab')
api.nvim_echo = native_echo
print('Neovim streamed progress workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
