local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
-- The first inspection enables highlight-state tracking and resets attribute IDs.
api.nvim__inspect_cell(1, 0, 0)
vim.o.lines, vim.o.columns = 60, 240
vim.o.termguicolors = true
vim.opt.diffopt = { 'internal', 'filler', 'closeoff', 'context:9999' }
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
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
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle .. '\n' .. text())
end
local selected = { base = string.rep('a', 40), head = string.rep('b', 40) }
local original = { base = 'original-base', head = 'comment-head' }
local latest = string.rep('c', 40)
local lines = {}
for line = 1, 90 do lines[line] = ('shared source line %03d'):format(line) end
local function contents(changes)
  local result = vim.deepcopy(lines)
  for line, value in pairs(changes or {}) do result[line] = value end
  return table.concat(result, '\n') .. '\n'
end
local base = contents({ [10] = 'original line', [20] = 'original second change' })
local middle = contents({ [10] = 'comment line', [20] = 'comment second change' })
local head = contents({ [10] = 'selected line', [20] = 'comment second change', [30] = 'selected later change' })
local cases = {
  { id = 'three', before = base, source = middle, after = head, roles = { 'original_base', 'comment', 'selected_head' } },
  { id = 'left-empty', before = middle, source = middle, after = head, roles = { 'comment', 'selected_head' } },
  { id = 'right-empty', before = base, source = middle, after = middle, roles = { 'original_base', 'comment' } },
  { id = 'both-empty', before = middle, source = middle, after = middle, roles = { 'comment' } },
  { id = 'outer-equal', before = base, source = middle, after = base, roles = { 'original_base', 'comment', 'selected_head' } },
  { id = 'mapped', before = base, source = middle, after = head, roles = { 'original_base', 'comment', 'selected_head' } },
  { id = 'reconstructed', before = base, source = middle, after = head, roles = { 'original_base', 'comment', 'selected_head' } },
}
local view = {
  id = 'comment-panes', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = selected,
  review = { id = 'comment-panes-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Comment source panes', revision = { base = selected.base, head = latest },
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'src/file.rs', viewed = false } }, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = true,
}
local responses = {}
for _, case in ipairs(cases) do
  local anchor = { path = 'src/file.rs', revision = original,
    range = { start = { side = 'head', line = 10 }, ['end'] = { side = 'head', line = 11 } } }
  local target = vim.deepcopy(anchor)
  if case.id == 'mapped' then target.current = { path = 'src/file.rs', revision = selected,
    range = { start = { side = 'head', line = 50 }, ['end'] = { side = 'head', line = 51 } } } end
  view.comments[#view.comments + 1] = {
    comment = { origin = 'read', comment = { id = case.id, body = 'Feedback ' .. case.id,
      author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = target } } },
    fetched = true, done = false, pending_delete = false, conflict = false, summary = false,
  }
  view.todo[#view.todo + 1] = case.id
  local revision = { base = original.head, head = selected.head }
  responses[case.id] = { anchor = anchor, diff_hunk = '@@ -10 +10 @@\n-original\n+comment', code = {
    path = 'src/file.rs', base_path = 'src/file.rs', revision = revision,
    base_content = case.source, head_content = case.after, binary = false, historical = true, provenance = 'recorded',
    location = { path = 'src/file.rs', revision = revision,
      range = { start = { side = 'base', line = 10 }, ['end'] = { side = 'base', line = 11 } } },
    original_base = { commit = original.base, path = 'src/file.rs', content = case.before, missing = false,
      provenance = case.id == 'reconstructed' and 'reconstructed' or 'recorded' },
  } }
end
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
write('historical_contexts.json', responses)
local original_tab
local function panes(case)
  local result, order = {}, {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' then
      assert(meta.comment_role, 'comment pane has no source role: ' .. vim.inspect(meta))
      assert(not result[meta.comment_role], 'duplicate comment source role')
      result[meta.comment_role] = { window = win, buffer = buf, metadata = meta }
      order[#order + 1] = { role = meta.comment_role, column = api.nvim_win_get_position(win)[2] }
    end
  end
  table.sort(order, function(left, right) return left.column < right.column end)
  local actual = {}; for _, item in ipairs(order) do actual[#actual + 1] = item.role end
  assert(vim.deep_equal(actual, case.roles), case.id .. ': wrong source order ' .. vim.inspect(actual))
  assert(api.nvim_get_current_tabpage() == original_tab, 'comment navigation created a tab')
  for role, source in pairs({ original_base = case.before, comment = case.source, selected_head = case.after }) do
    if result[role] then assert(text(result[role].buffer) .. '\n' == source, case.id .. ': wrong bytes for ' .. role) end
  end
  assert(result.comment.metadata.revision.base == original.head, 'comment pane lost exact original commit')
  assert(result.comment.metadata.source_path == 'src/file.rs', 'comment pane lost original path')
  if result.selected_head then
    assert(result.selected_head.metadata.revision.head == selected.head, 'selected pane used latest fetched PR head')
  end
  if case.id == 'reconstructed' then
    assert(vim.wo[result.original_base.window].winbar:lower():find('reconstructed', 1, true), 'reconstructed original base looks exact')
    assert(not vim.wo[result.comment.window].winbar:lower():find('reconstructed', 1, true), 'exact comment source mislabeled reconstructed')
  end
  return result
end
local function open(case)
  command(''); wait(function() return kind() == 'home' end, 'overview missing')
  row('Feedback ' .. case.id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == case.id end, 'discussion missing ' .. case.id)
  local result = panes(case)
  assert(api.nvim_win_get_cursor(result.comment.window)[1] == 10, 'comment cursor used remapped/current range')
  local namespace = api.nvim_get_namespaces()['buf-review-comment-range']
  wait(function()
    local found = {}
    for _, pane in pairs(result) do
      for _, mark in ipairs(api.nvim_buf_get_extmarks(pane.buffer, namespace, 0, -1, { details = true })) do
        assert(pane == result.comment, 'comment range highlighted a comparison side')
        local last = mark[4].end_row or mark[2]
        if last > mark[2] and (mark[4].end_col or 0) == 0 then last = last - 1 end
        for line = mark[2], last do found[line + 1] = true end
      end
    end
    return found[10] and found[11] and not found[50]
  end, 'original comment range missing ' .. case.id)
  return result
end

command(''); wait(function() return kind() == 'home' end, 'initial overview missing')
original_tab = api.nvim_get_current_tabpage()
for _, case in ipairs(cases) do open(case) end
local current = open(cases[1])
local namespace = api.nvim_get_namespaces()['buf-review-comment-diff']
assert(namespace, 'middle diff namespace missing')
local colors = {}
for _, mark in ipairs(api.nvim_buf_get_extmarks(current.comment.buffer, namespace, 0, -1, { details = true })) do
  local line, details = mark[2] + 1, mark[4]
  colors[line] = colors[line] or {}
  colors[line][details.hl_group or details.line_hl_group] = details.priority
end
assert(colors[10] and colors[10].BufReviewCommentAdded and colors[10].BufReviewCommentRemoved, 'overlapping middle changes missing')
assert(colors[10].BufReviewCommentRemoved > colors[10].BufReviewCommentAdded, 'red does not override green')
assert(colors[20] and colors[20].BufReviewCommentAdded and not colors[20].BufReviewCommentRemoved, 'green-only middle change missing')
assert(colors[30] and colors[30].BufReviewCommentRemoved and not colors[30].BufReviewCommentAdded, 'red-only middle change missing')
for role, pane in pairs(current) do
  if role ~= 'comment' then
    assert(#api.nvim_buf_get_extmarks(pane.buffer, namespace, 0, -1, {}) == 0, 'middle color overlays leaked onto side pane')
  end
end

-- Compare rendered cells with known controls; range highlighting must not mask changes.
local control = api.nvim_create_buf(false, true)
api.nvim_buf_set_lines(control, 0, -1, false, { 'green control', 'red control' })
local control_namespace = api.nvim_create_namespace('buf-review-comment-color-control')
local control_window = api.nvim_open_win(control, false, { split = 'right', win = current.selected_head.window })
vim.wo[control_window].diff = false
vim.wo[control_window].winhighlight = ''
vim.wo[control_window].winbar = ''
api.nvim_set_hl(0, 'DiffAdd', { fg = '#ffffff', bg = '#005500', ctermfg = 15, ctermbg = 22 })
api.nvim_set_hl(0, 'DiffDelete', { fg = '#ffffff', bg = '#550000', ctermfg = 15, ctermbg = 52 })
api.nvim_set_hl(0, 'BufReviewColorControlGreen', { fg = '#ffffff', bg = '#005500', ctermfg = 15, ctermbg = 22 })
api.nvim_set_hl(0, 'BufReviewColorControlRed', { fg = '#ffffff', bg = '#550000', ctermfg = 15, ctermbg = 52 })
for index, group in ipairs({ 'BufReviewColorControlGreen', 'BufReviewColorControlRed' }) do
  api.nvim_buf_set_extmark(control, control_namespace, index - 1, 0, { end_row = index, end_col = 0, hl_group = group, hl_eol = true })
end
vim.bo[current.comment.buffer].syntax = ''
api.nvim_win_call(current.comment.window, function() vim.cmd('keepjumps normal! 10Gzt') end)
vim.cmd('redraw!')
ReviewTest.wait(30)
vim.cmd('redraw!')
local function cell(win, line)
  local position = vim.fn.screenpos(win, line, 1)
  assert(position.row > 0 and position.col > 0, 'colored source line not visible: ' .. line)
  return api.nvim__inspect_cell(1, position.row - 1, position.col - 1)
end
assert(cell(control_window, 1)[2].background == 0x005500, 'green render control did not use configured color')
assert(cell(control_window, 2)[2].background == 0x550000, 'red render control did not use configured color')
assert(cell(current.comment.window, 20)[2].background == 0x005500, 'green middle line masked by another highlight')
assert(cell(current.comment.window, 30)[2].background == 0x550000, 'red middle line masked by another highlight')
assert(cell(current.comment.window, 10)[2].background == 0x550000, 'selected comment range masks red precedence')
api.nvim_win_close(control_window, true)
api.nvim_buf_delete(control, { force = true })

-- Saving a reply preserves the discussion and all three source views.
local saved = {}
for role, pane in pairs(current) do
  api.nvim_win_call(pane.window, function()
    vim.fn.winrestview({ lnum = 55, col = 3, topline = 40, leftcol = 0 })
    saved[role] = vim.fn.winsaveview()
  end)
end
local discussion, discussion_view = api.nvim_get_current_win(), vim.fn.winsaveview()
command('reply')
assert(kind() == 'composer', 'reply composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Reply preserves source panes' })
input(':wq<CR>')
wait(function() return kind() == 'thread' end, 'reply did not restore discussion')
assert(api.nvim_get_current_win() == discussion, 'reply returned to a code pane')
assert(vim.deep_equal(vim.fn.winsaveview(), discussion_view), 'reply changed discussion position')
for role, pane in pairs(current) do
  assert(api.nvim_win_is_valid(pane.window), 'reply closed ' .. role)
  assert(vim.deep_equal(api.nvim_win_call(pane.window, vim.fn.winsaveview), saved[role]), 'reply changed ' .. role .. ' scroll/cursor')
end

for _ = 1, 3 do for _, case in ipairs(cases) do open(case) end end
local owned, hidden = 0, 0
for _, buf in ipairs(api.nvim_list_bufs()) do
  local meta = vim.b[buf].buf_review
  if meta and meta.kind == 'diff' then
    owned = owned + 1
    if #vim.fn.win_findbuf(buf) == 0 then hidden = hidden + 1 end
  end
end
assert(owned == 3 and hidden == 0, 'comment navigation accumulated source buffers')

local function ordinary_file()
  command(''); wait(function() return kind() == 'home' end, 'overview missing before ordinary file')
  row('Unviewed files')
  api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
  require('buf_review').enter()
  wait(function() return kind() == 'diff' and not vim.b.buf_review.comment_role end, 'ordinary file comparison missing')
  local count = 0
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' then
      count = count + 1
      assert(not meta.comment_role, 'ordinary file retained comment role')
      assert(not vim.wo[win].winhighlight:find('BufReviewCommentUnchanged', 1, true), 'ordinary diff inherited disabled native colors')
    end
  end
  assert(count == 2, 'ordinary file retained comment comparison pane')
end
open(cases[4]); ordinary_file()
current = open(cases[1])
api.nvim_set_current_win(current.original_base.window)
vim.cmd('only')
ordinary_file()
print('Neovim original/comment/selected source panes passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
