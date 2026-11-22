local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local cases = {
  { path = 'src/with spaces.rs', change = 'modified', stats = { additions = 12, deletions = 3 }, viewed = false, label = 'M +12 -3' },
  { path = 'src/added.rs', change = 'added', stats = { additions = 4, deletions = 0 }, viewed = false, label = 'A +4 -0' },
  { path = 'src/deleted.rs', change = 'deleted', stats = { additions = 0, deletions = 7 }, viewed = true, label = 'D +0 -7' },
  { path = 'src/new name.rs', previous_path = 'src/old name.rs', change = 'renamed', stats = { additions = 2, deletions = 2 }, viewed = true, label = 'M +2 -2' },
  { path = 'src/zero.rs', change = 'unchanged', stats = { additions = 0, deletions = 0 }, viewed = false, label = 'M +0 -0' },
  { path = 'src/unknown.rs', change = 'modified', viewed = false, label = 'M +? -?' },
  { path = 'src/copied.rs', change = 'copied', stats = { additions = 1, deletions = 1 }, viewed = true, label = 'M +1 -1' },
  { path = 'src/type.rs', change = 'type_changed', stats = { additions = 3, deletions = 4 }, viewed = false, label = 'M +3 -4' },
}
local files = vim.deepcopy(cases)
for _, item in ipairs(files) do item.label = nil end
local initial = {
  id = 'overview-stats', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'overview-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'File overview', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = files, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')) end
local function requests(op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}) do
    local value = vim.json.decode(line)
    if value.op == op then result[#result + 1] = value end
  end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(40)
end
local function file_row(path)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:sub(-#path - 2) == '  ' .. path then api.nvim_win_set_cursor(0, { line, 0 }); return line, value end
  end
  error('Missing file row: ' .. path .. '\n' .. text())
end
local function assert_rows()
  local home_ns = api.nvim_get_namespaces()['buf-review-home']
  for _, item in ipairs(cases) do
    local line, value = file_row(item.path)
    assert(value == '  ' .. item.label .. '  ' .. item.path, 'wrong change/counts/path for ' .. item.path .. ': ' .. value)
    local status, added, removed = item.label:match('^(%a) (%S+) (%S+)$')
    local spans = {}
    for _, mark in ipairs(api.nvim_buf_get_extmarks(0, home_ns, { line - 1, 0 }, { line - 1, -1 }, { details = true })) do
      spans[#spans + 1] = { group = mark[4].hl_group, start = mark[3], finish = mark[4].end_col }
    end
    local function span(group, start, finish)
      for _, mark in ipairs(spans) do
        if mark.group == group and mark.start == start and (not finish or mark.finish == finish) then return end
      end
      error('Missing file highlight ' .. group .. ' for ' .. item.path .. ': ' .. vim.inspect(spans))
    end
    span(status == 'A' and 'BufReviewAdded' or (status == 'D' and 'BufReviewDeleted' or 'BufReviewModified'), 2, 3)
    span('BufReviewAdded', 4, 4 + #added)
    span('BufReviewDeleted', 5 + #added, 5 + #added + #removed)
    span('BufReviewPath', 2 + #item.label + 2)
    local section
    for previous = line - 1, 1, -1 do
      local candidate = api.nvim_buf_get_lines(0, previous - 1, previous, false)[1]
      if candidate:match('^%S') then section = candidate; break end
    end
    local viewed
    for _, file in ipairs(state().files) do if file.path == item.path then viewed = file.viewed end end
    assert(section:find(viewed and 'Viewed files' or 'Unviewed files', 1, true), 'file moved into wrong progress section')
  end
end

vim.cmd('Review')
wait(function() return kind() == 'home' end, 'overview missing')
assert_rows()
for group, link in pairs({ BufReviewAdded = 'Added', BufReviewDeleted = 'Removed', BufReviewModified = 'Changed' }) do
  assert(api.nvim_get_hl(0, { name = group, link = true }).link == link, 'file highlight does not inherit native theme: ' .. group)
end

-- Counts are presentation only: actions keep exact paths, including whitespace.
file_row('src/with spaces.rs')
local before = #requests('actions')
input('s')
wait(function() return state().files[1].viewed end, 'styled row did not mark file viewed')
local sent = requests('actions')
assert(#sent == before + 1 and sent[#sent].actions[1].path == 'src/with spaces.rs', 'viewed action parsed or altered displayed path')
assert_rows()
file_row('src/with spaces.rs')
input('u')
wait(function() return not state().files[1].viewed end, 'styled row did not clear viewed')
assert_rows()

-- Enter on modified/added/deleted/renamed rows keeps ordinary file navigation.
for _, path in ipairs({ 'src/with spaces.rs', 'src/added.rs', 'src/deleted.rs', 'src/new name.rs' }) do
  file_row(path)
  before = #requests('code')
  input('<CR>')
  wait(function() return kind() == 'diff' and vim.b.buf_review.path == path end, 'styled row lost file navigation: ' .. path)
  sent = requests('code')
  assert(#sent == before + 1 and sent[#sent].path == path, 'styled file row changed code request path')
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'overview did not reopen')
  assert_rows()
end
assert(not text():find('src/old name.rs', 1, true), 'previous rename path replaced navigable current path')
print('Neovim overview file stats passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
