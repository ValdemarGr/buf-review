local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:append(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function requests(operation)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}) do
    local request = vim.json.decode(line)
    if not operation or request.op == operation then result[#result + 1] = request end
  end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function lines() return api.nvim_buf_get_lines(0, 0, -1, false) end
local function text() return table.concat(lines(), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(25)
end
local function command(value) vim.cmd('Review ' .. value) end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(25)
end
local function row(needle)
  for at, line in ipairs(lines()) do
    if line:find(needle, 1, true) then api.nvim_win_set_cursor(0, { at, 0 }); return at end
  end
  error('Missing row ' .. needle .. '\n' .. text())
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local view = {
  id = 'log-session', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'log-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Log fixture', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = {}, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
local recorded = {
  generation = 9,
  action = { action = 'finish_publication', success = false, error = 'gh failed\nSecond diagnostic', uncertain = false },
}
local raw = {
  '{', '  "generation": 9,', '  "action": {', '    "action": "finish_publication",', '    "success": false,',
  '    "error": "gh failed\\nSecond diagnostic",', '    "uncertain": false', '  }', '}',
}
local entries = {
  { generation = 9, summary = 'Publication failed', details = { 'Standalone · 2 operations', 'gh failed\nSecond diagnostic' }, level = 'error', raw = raw },
  { generation = 8, summary = 'Published standalone · 2 comments', details = { 'Published successfully' }, level = 'success', raw = { '{"generation":8,"action":{"action":"finish_publication","success":true}}' } },
  { generation = 3, summary = 'Saved comment · src/file.rs:24', details = { 'A comment body', '', 'Second paragraph' }, level = 'info', raw = { '{"generation":3,"action":{"action":"save","body":"A comment body\\n\\nSecond paragraph"}}' } },
}
write('log.json', entries)
command(''); wait(function() return kind() == 'home' end, 'home missing')
local home = api.nvim_get_current_buf()
command('log'); wait(function() return kind() == 'log' end, 'log missing')
assert(vim.bo.readonly and not vim.bo.modifiable and vim.bo.buftype == 'nofile', 'log must be a read-only scratch buffer')
assert(vim.wo.foldtext == '', 'closed entries must retain error and publication-success highlights')
assert(lines()[1]:find('Review log', 1, true) and lines()[1]:find(view.id, 1, true), 'log title lacks session identity')
assert(not text():find('{{{', 1, true), 'fold markers leaked into rendered log')
local rows = {}
for at, line in ipairs(lines()) do if line:match('^#%d+%s') then rows[#rows + 1] = at end end
assert(#rows == #entries, 'timeline lost journal entries')
for index, at in ipairs(rows) do
  assert(lines()[at]:find('#' .. entries[index].generation, 1, true) and lines()[at]:find(entries[index].summary, 1, true), 'timeline changed supplied generation order or summary')
  assert(vim.fn.foldclosed(at) == at and vim.fn.foldlevel(at) == 1, 'journal entry is not initially folded at level one')
end
api.nvim_win_set_cursor(0, { rows[1], 0 }); input('<CR>')
assert(vim.fn.foldclosed(rows[1]) == -1, 'Enter did not expand journal entry')
local detail = row('gh failed')
assert(vim.fn.foldclosed(detail) == -1 and text():find('Second diagnostic', 1, true), 'multiline diagnostic missing or hidden after expansion')
local payload = row('Recorded payload')
assert(vim.fn.foldlevel(payload) == 2 and vim.fn.foldclosed(payload) == payload, 'raw payload must remain folded inside expanded journal entry')
input('<CR>')
assert(vim.fn.foldclosed(payload) == -1, 'Enter did not expand recorded payload')
local displayed = {}
local all = lines()
for offset = 1, #raw do displayed[#displayed + 1] = vim.trim(all[payload + offset]) end
assert(vim.deep_equal(vim.json.decode(table.concat(displayed, '\n')), recorded), 'rendered raw payload lost recorded event fields')
input('<CR>')
assert(vim.fn.foldclosed(payload) == payload, 'Enter did not collapse recorded payload')
api.nvim_win_set_cursor(0, { rows[1], 0 }); input('<CR>')
assert(vim.fn.foldclosed(rows[1]) == rows[1], 'Enter did not collapse journal entry')
input('zo'); assert(vim.fn.foldclosed(rows[1]) == -1, 'native zo cannot expand journal entry')
input('zc'); assert(vim.fn.foldclosed(rows[1]) == rows[1], 'native zc cannot collapse journal entry')
local groups = {}
for _, namespace in pairs(api.nvim_get_namespaces()) do
  for _, mark in ipairs(api.nvim_buf_get_extmarks(0, namespace, 0, -1, { details = true })) do
    if mark[4].hl_group then groups[mark[4].hl_group] = true end
  end
end
assert(groups.BufReviewTitle and groups.BufReviewError and groups.BufReviewDone and groups.BufReviewSection and groups.BufReviewCount and groups.BufReviewLabel, 'log lacks title, event or payload highlights')
command(''); wait(function() return kind() == 'home' and api.nvim_get_current_buf() == home end, 'Review did not return to home')

-- Pending logs cannot replace a newer home selection or modified composer.
vim.fn.writefile({ '0.3' }, fixture .. '/delay_log')
local before = #requests('log')
command('log'); wait(function() return #requests('log') == before + 1 end, 'delayed home log request missing')
command(''); ReviewTest.wait(400)
assert(kind() == 'home' and api.nvim_get_current_buf() == home, 'old log callback replaced newer home navigation')
before = #requests('log')
command('log'); wait(function() return #requests('log') == before + 1 end, 'delayed composer log request missing')
api.nvim_win_set_cursor(0, { 1, 0 }); command('comment standalone')
assert(kind() == 'composer', 'unsaved composer setup missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved body while log loads' })
local composer = api.nvim_get_current_buf()
ReviewTest.wait(400)
assert(api.nvim_get_current_buf() == composer and kind() == 'composer' and vim.bo.modified and text() == 'Unsaved body while log loads', 'old log callback discarded unsaved composer')
input(':q!<CR>')
vim.fn.delete(fixture .. '/delay_log')
command(''); wait(function() return kind() == 'home' end, 'home missing after composer close')

-- A delayed response from an old session cannot open over the selected session.
local other = vim.deepcopy(view)
other.id, other.locator = 'other-log-session', 'https://github.com/acme/repo/pull/42'
write('views.json', { [view.id] = view, [other.id] = other })
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id },
  [other.locator] = { route = 'batch', batch = other.id, fresh_review = other.review.id } })
vim.fn.writefile({ '0.3' }, fixture .. '/delay_log')
before = #requests('log')
command('log'); wait(function() return #requests('log') == before + 1 end, 'delayed old-session log request missing')
command('prs'); wait(function() return kind() == 'prs' end, 'PR browser missing')
row('#42'); input('<CR>')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == other.id end, 'second session missing')
ReviewTest.wait(400)
assert(kind() == 'home' and vim.b.buf_review.workspace == other.id, 'old log callback replaced selected session')
vim.fn.delete(fixture .. '/delay_log')

write('log.json', {})
command('log'); wait(function() return kind() == 'log' and text():find('0 events', 1, true) end, 'empty log missing')
assert(vim.bo.readonly and not vim.bo.modifiable and text():find('Review log', 1, true), 'empty log lost viewer layout')
for _, request in ipairs(requests()) do
  assert(vim.tbl_contains({ 'entry', 'select', 'fetch_objects', 'get', 'log', 'list', 'route' }, request.op), 'log viewer caused unexpected action: ' .. request.op)
end
print('Neovim review log viewer passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
