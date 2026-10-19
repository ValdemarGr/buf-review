local api = vim.api
local repository = vim.fn.getcwd()
local scenario = vim.env.BUF_REVIEW_POST_CASE
if not scenario then
  for _, name in ipairs({ 'standalone', 'conclusion', 'partial', 'refresh_error', 'inflight', 'conclusion_partial', 'conclusion_refresh_error', 'interrupt' }) do
    local runner, test = repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/post_publish.lua'
    local result = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c',
      ('lua assert(loadfile(%q))(%q)'):format(runner, test) }, { env = { BUF_REVIEW_POST_CASE = name }, text = true }):wait()
    assert(result.code == 0, name .. ': ' .. result.stderr)
  end
  print('Neovim blocking post-publication refresh passed')
  vim.cmd('qa!')
  return
end

local fixture = vim.fn.tempname()
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
local function records(name)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    result[#result + 1] = vim.json.decode(line, { luanil = { object = true, array = true } })
  end
  return result
end
local function requests(op)
  return vim.tbl_filter(function(value) return value.op == op end, records('requests.jsonl'))
end
local function state() return read('state.json') end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(value) vim.cmd('Review ' .. value) end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
end
local function notice(needle)
  for _, value in ipairs(notices) do if value:find(needle, 1, true) then return true end end
  return false
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, body, fetched, destination)
  return {
    comment = { origin = fetched and 'read' or 'draft', comment = { id = id, body = body,
      author = { id = fetched and 'alice' or 'me', name = fetched and 'alice' or 'me' }, target = { kind = 'pull_request' } } },
    fetched = fetched, destination = destination, done = false, pending_delete = false, conflict = false, summary = false,
  }
end
local conclusion = scenario:find('conclusion', 1, true) == 1
local partial = scenario == 'partial' or scenario == 'conclusion_partial'
local refresh_error = scenario == 'refresh_error' or scenario == 'conclusion_refresh_error'
local destination = conclusion and 'review' or 'standalone'
local draft = comment('delivered', 'Saved comment', false, destination)
local remaining = comment('remaining', 'Still pending', false, 'standalone')
local function operation(row) return { id = 'op-' .. row.comment.comment.id, change = { kind = 'create_comment', comment = row.comment.comment } } end
local initial = {
  id = 'post-publish', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'post-publish-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Post-publication refresh', revision = revision,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { draft }, files = {}, todo = {}, done = {}, pending = { operation(draft) },
  conflicts = {}, publications = {}, new_commits = false,
}
if partial then
  initial.comments[#initial.comments + 1] = remaining
  initial.pending[#initial.pending + 1] = operation(remaining)
end
local receipt = {
  id = 'receipt-' .. scenario, generation = 1, destination = destination,
  status = partial and 'failed' or (conclusion and 'submitted' or 'published'),
  planned = { comments = partial and 2 or 1, total = partial and 2 or 1 },
  published = { comments = 1, total = 1 },
}
local delivered = vim.deepcopy(draft)
delivered.destination = nil
local fetched = vim.deepcopy(delivered)
fetched.comment.origin, fetched.fetched = 'read', true
local local_comments, remote_comments = { delivered }, { fetched, comment('remote-only', 'Fetched after publication', true) }
local pending = {}
if partial then
  local_comments[#local_comments + 1], remote_comments[#remote_comments + 1] = remaining, remaining
  pending[1] = operation(remaining)
end
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
write('post_publish.json', {
  tag = scenario, operation = 'publish',
  ['local'] = { comments = local_comments, pending = pending, publications = { receipt }, wip = vim.NIL },
  remote = { comments = remote_comments, pending = pending },
  error = partial and 'Fixture publication failed after first receipt' or nil,
})
command('')
wait(function() return kind() == 'home' and text():find('Code: ready', 1, true) end, 'Initial review did not finish preparing')
assert(#requests('fetch_refresh') == 0, 'Fresh entry unexpectedly started a remote refresh')
if scenario == 'inflight' then
  vim.fn.writefile({ '0.20' }, fixture .. '/delay_refresh')
  command('refresh')
  wait(function() return #records('post_publish_events.jsonl') == 1 end, 'Background snapshot did not start')
  assert(not records('post_publish_events.jsonl')[1].publication, 'Background snapshot already contains later publication')
elseif refresh_error then
  vim.fn.writefile({ '' }, fixture .. '/fail_refresh')
elseif scenario == 'interrupt' then
  vim.fn.writefile({ '0.30' }, fixture .. '/delay_publish')
end

local composer
if conclusion then
  vim.ui.select = function(_, _, callback) callback('comment') end
  command('conclude')
  wait(function() return kind() == 'composer' end, 'Conclusion composer missing')
  composer = api.nvim_get_current_buf()
  api.nvim_buf_set_lines(composer, 0, -1, false, { 'Ready to submit' })
  api.nvim_feedkeys(api.nvim_replace_termcodes(':wq<CR>', true, false, true), 'xt', false)
  assert(not api.nvim_buf_is_valid(composer) and #requests('publish') == 0 and #requests('submit') == 0,
    'Conclusion write-and-close published instead of queuing locally')
end
-- No wait here: publish itself must finish publication and fresh-state redraw.
if scenario == 'interrupt' then
  local function interrupt()
    if #requests('publish') == 0 then vim.defer_fn(interrupt, 5); return end
    api.nvim_input('\003')
  end
  vim.defer_fn(interrupt, 0)
end
command('publish')
if scenario == 'interrupt' then
  assert(notice('Publication continues in the background; wait for it to finish before retrying'), 'Native Ctrl-C did not interrupt publication wait')
  assert(not state().publications[1], 'Interrupted wait did not return before publication finished')
  command('publish')
  assert(notice('Publication is still running; wait before retrying'), 'Interrupted publication allowed duplicate writes')
  assert(#requests('publish') == 1, 'Duplicate publication reached provider')
  wait(function() return text():find('Fetched after publication', 1, true) end, 'Interrupted publication did not finish refreshing in background')
end

local final = state()
assert(final.id == initial.id and vim.deep_equal(final.comparison, revision), 'Post-publication refresh changed session or comparison pin')
assert(final.publications[1] and final.publications[1].id == receipt.id, 'Publication receipt missing after command returned')
assert(#requests('fetch_refresh') >= 1, 'Publication returned before fetching remote state')
assert(#requests('apply_refresh') >= 1, 'Publication returned before applying fetched state')
assert(#requests('publish') == 1 and #requests('submit') == 0, 'Publication was repeated or used the removed submit command')
local events, publication_index, fresh_index = records('post_publish_events.jsonl')
for index, event in ipairs(events) do
  if event.phase == 'published' then publication_index = index end
  if event.phase == 'fetch' and event.publication == scenario then fresh_index = index end
end
assert(publication_index and fresh_index and fresh_index > publication_index, 'Post-publication fetch did not observe completed writes')
if scenario == 'inflight' then
  assert(events[1].phase == 'fetch' and not events[1].publication and #requests('fetch_refresh') == 2,
    'Pre-publication in-flight snapshot counted as post-publication refresh')
end
if refresh_error then
  assert(notice('Publication finished; refresh failed:') and notice('Fixture refresh failed'), 'Refresh failure was not reported separately')
  assert(not notice('Fixture publication failed'), 'Successful publication reported as failed')
  assert(#final.pending == 0 and not final.comments[1].destination, 'Refresh failure lost delivered receipt state')
  assert(text():find('refresh failed', 1, true), 'Review did not redraw refresh failure before returning')
else
  assert(kind() == 'home' and text():find('Fetched after publication', 1, true), 'Command returned before fresh remote state was rendered')
  assert(final.comments[1].fetched, 'Published comment remains an unfetched local receipt')
  if partial then
    assert(notice('Fixture publication failed after first receipt'), 'Partial publication error disappeared during refresh')
    assert(#final.pending == 1 and final.pending[1].id == 'op-remaining', 'Refresh lost pending operation after partial publication')
    assert(text():find('Still pending', 1, true), 'Pending comment disappeared after partial publication')
    if conclusion then assert(not final.wip and final.publications[1].published.comments == 1, 'Submitted review outcome lost after partial progress failure') end
  else
    assert(#final.pending == 0 and (#notices == 0 or scenario == 'interrupt'), 'Successful publication retained pending state or reported an error: ' .. vim.inspect(notices))
  end
end
if composer then
  assert(not api.nvim_buf_is_valid(composer) and not final.wip and kind() == 'home', 'Queued conclusion did not publish with fresh-state redraw')
end
print('Post-publication ' .. scenario .. ' passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
