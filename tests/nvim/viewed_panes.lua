local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:append(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
vim.o.columns = 200
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n')) end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') .. '\n' end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function command(value) vim.cmd('Review ' .. value) end
local function row(path)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:sub(-#path - 2) == '  ' .. path then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing file ' .. path .. '\n' .. text())
end
local comparison = { base = string.rep('a', 40), head = string.rep('b', 40) }
local viewed = string.rep('c', 40)
local base, middle, head = 'one\nbase\nthree\nfour\n', 'one\nviewed\nthree\nfour\n', 'one\nhead\nthree\nfour\n'
local view = {
  id = 'viewed-panes', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = comparison,
  review = { id = 'viewed-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Viewed panes', revision = comparison }, files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = {}, files = {}, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local cases = {
  { path = 'three.txt', before = base, source = middle, after = head, count = 3 },
  { path = 'plain.txt', before = base, after = head, count = 2 },
  { path = 'left-empty.txt', before = middle, source = middle, after = head, count = 2 },
  { path = 'right-empty.txt', before = base, source = middle, after = middle, count = 2 },
  { path = 'absent-base.txt', before = '', source = '', after = head, base_missing = true, count = 3 },
  { path = 'viewed-at-base.txt', before = base, source = base, after = head, commit = comparison.base, count = 2 },
}
local codes = {}
for _, case in ipairs(cases) do
  view.files[#view.files + 1] = { path = case.path, viewed = false }
  local code = { path = case.path, revision = comparison, base_content = case.before, head_content = case.after,
    base_missing = case.base_missing or false, head_missing = false, binary = false }
  if case.source then code.last_viewed = { commit = case.commit or viewed, path = case.path, content = case.source, missing = false } end
  codes[case.path] = code
end
local local_path = fixture .. '/three.txt'
vim.fn.writefile(vim.split(head:sub(1, -2), '\n'), local_path)
codes['three.txt'].local_path = local_path
codes['three.txt'].local_dirty = false
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
write('code_contexts.json', codes)
local function panes()
  local result = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    if vim.b[buf].buf_review and vim.b[buf].buf_review.kind == 'diff' or api.nvim_buf_get_name(buf) == local_path then
      result[#result + 1] = { window = win, buffer = buf, marker = vim.b[buf].buf_review }
    end
  end
  table.sort(result, function(left, right) return api.nvim_win_get_position(left.window)[2] < api.nvim_win_get_position(right.window)[2] end)
  return result
end
local function open(case)
  command(''); wait(function() return kind() == 'home' end, 'overview missing')
  row(case.path); require('buf_review').enter()
  wait(function() return kind() == 'diff' and vim.b.buf_review.path == case.path end, 'file missing ' .. case.path)
  local result = panes()
  assert(#result == case.count, case.path .. ': wrong pane count ' .. #result)
  return result
end
local initial_tab
local current = open(cases[1])
initial_tab = api.nvim_get_current_tabpage()
assert(text(current[1].buffer) == base and text(current[2].buffer) == middle and text(current[3].buffer) == head,
  'target/viewed/head contents overwritten or reordered')
assert(current[1].buffer ~= current[2].buffer, 'target and viewed shared scratch buffer')
assert(vim.wo[current[2].window].winbar:find('Last viewed ' .. viewed, 1, true), 'last viewed label missing')
assert(current[2].marker.historical and current[2].marker.revision.base == viewed, 'viewed reference uses wrong commit')
assert(not current[1].marker.historical and not current[3].marker.historical, 'current comparison became historical')
assert(read('state.json').generation == 0, 'opening file changed saved review state')
for _, pane in ipairs(current) do assert(vim.wo[pane.window].diff, 'native diff missing') end
local namespace = api.nvim_get_namespaces()['buf-review-comment-diff']
local function colors(buf)
  local result = {}
  for _, mark in ipairs(api.nvim_buf_get_extmarks(buf, namespace, 0, -1, { details = true })) do
    result[mark[4].hl_group] = true
  end
  return result
end
assert(colors(current[2].buffer).BufReviewCommentAdded and colors(current[2].buffer).BufReviewCommentRemoved,
  'viewed middle lost base additions or later removals')
assert(not next(colors(current[1].buffer)) and not next(colors(current[3].buffer)), 'viewed overlays leaked to comparison panes')

api.nvim_set_current_win(current[2].window)
local ok, err = pcall(require('buf_review').new_comment)
assert(not ok and err:find('reference only', 1, true), 'old viewed lines allowed new current-head comment')
input('s')
assert(notices[#notices]:find('Historical code cannot mark', 1, true), 'old viewed pane marked current file')
assert(read('state.json').generation == 0, 'reference action mutated state')
api.nvim_set_current_win(current[3].window)
for _, action in ipairs({ { key = 's', viewed = true }, { key = 'u', viewed = false } }) do
  input(action.key)
  wait(function() return read('state.json').files[1].viewed == action.viewed end, 'current HEAD progress failed')
  for _, index in ipairs({ 1, 3 }) do
    assert(vim.wo[current[index].window].winbar:find(action.viewed and '[viewed]' or '[unviewed]', 1, true), 'current pane progress label stale')
  end
end

-- Ordinary navigation must discard the historical middle and its overlays.
input(']f')
wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'plain.txt' end, 'next file did not use ordinary navigation')
assert(#panes() == 2, 'next file retained old viewed pane')
for _, pane in ipairs(panes()) do
  assert(not next(colors(pane.buffer)), 'old middle overlays survived next-file navigation')
  assert(not vim.wo[pane.window].winhighlight:find('BufReviewCommentUnchanged', 1, true), 'old middle disabled native colors')
end
for _, case in ipairs({ cases[3], cases[4], cases[5] }) do open(case) end
assert(api.nvim_get_current_tabpage() == initial_tab, 'file navigation created another review tab')

-- A contracted viewed pane at the current base still supports base-side actions.
local at_base = open(cases[6])
api.nvim_set_current_win(at_base[1].window)
input('s')
wait(function() return read('state.json').files[6].viewed end, 'viewed current base could not mark file')
require('buf_review').new_comment()
assert(kind() == 'composer', 'viewed current base could not anchor a comment')
local target = require('buf_review.editor').get().target.target
assert(vim.deep_equal(target.revision, comparison) and target.range.start.side == 'base', 'viewed current base anchored wrong revision or side')
input(':q!<CR>')

-- Explicit o preserves target and viewed while replacing only right with native editing.
current = open(cases[1])
api.nvim_set_current_win(current[3].window)
input('o')
local editable = panes()
assert(#editable == 3 and text(editable[1].buffer) == base and text(editable[2].buffer) == middle, 'o lost target/viewed panes')
assert(api.nvim_buf_get_name(editable[3].buffer) == local_path and not editable[3].marker, 'local right pane became review scratch')
api.nvim_set_current_win(editable[3].window)
api.nvim_buf_set_lines(0, 0, -1, false, { 'one', 'viewed', 'three', 'four' })
wait(function() return not colors(editable[2].buffer).BufReviewCommentRemoved end, 'native editing did not update viewed removals')
assert(vim.bo[editable[3].buffer].modified, 'native edits were not retained')
current = open(cases[1])
assert(#current == 3 and current[3].buffer == editable[3].buffer and not current[3].marker, 'unsaved native edits did not select working-tree right pane')
assert(text(current[2].buffer) == middle, 'unsaved right pane changed historical middle')

api.nvim_buf_set_lines(editable[3].buffer, 0, -1, false, vim.split(head:sub(1, -2), '\n'))
vim.bo[editable[3].buffer].modified = false
codes['three.txt'].local_dirty = true
write('code_contexts.json', codes)
current = open(cases[1])
assert(not current[3].marker and current[3].buffer == editable[3].buffer, 'dirty working tree did not use native right pane')
print('Neovim target/viewed/head panes passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
