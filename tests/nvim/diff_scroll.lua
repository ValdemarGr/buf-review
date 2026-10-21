local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
vim.o.lines, vim.o.columns = 60, 180
vim.opt.diffopt = { 'internal', 'filler', 'closeoff', 'context:9999' }
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local old = { base = 'historical-base', head = 'historical-head' }
local base, head = {}, {}
for line = 1, 260 do
  base[#base + 1] = ('shared source line %03d'):format(line)
  if line == 20 then
    for added = 1, 13 do head[#head + 1] = 'inserted head line ' .. added end
  end
  if line < 110 or line > 118 then head[#head + 1] = base[#base] end
end
local base_text, head_text = table.concat(base, '\n') .. '\n', table.concat(head, '\n') .. '\n'
local function head_line(line) return line + (line < 20 and 0 or (line < 110 and 13 or 4)) end
local function comment(id, side, line, comparison)
  return { comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id, author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = 'src/long.rs', revision = comparison or revision, range = { start = { side = side, line = line }, ['end'] = { side = side, line = line } } } } } }, fetched = true, done = false, pending_delete = false, conflict = false, summary = false }
end
local view = {
  id = 'diff-scroll', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'scroll-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Diff scroll', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('first-head', 'head', head_line(80)), comment('later-base', 'base', 180), comment('historical', 'head', 140, old) },
  files = { { path = 'src/long.rs', viewed = false } }, todo = { 'first-head', 'later-base', 'historical' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
write('code_contexts.json', { ['src/long.rs'] = { path = 'src/long.rs', revision = revision, base_content = base_text, head_content = head_text, binary = false } })
local historical_revision = { base = old.head, head = revision.head }
local historical_contexts = { historical = {
  anchor = view.comments[3].comment.comment.target.target, diff_hunk = '@@ -140 +140 @@\n-old\n+new',
  code = { path = 'src/long.rs', base_path = 'src/long.rs', revision = historical_revision, base_content = base_text, head_content = head_text, binary = false,
    historical = true, provenance = 'recorded', location = { path = 'src/long.rs', revision = historical_revision, range = { start = { side = 'base', line = 140 }, ['end'] = { side = 'base', line = 140 } } } },
} }
for index = 1, 2 do
  local row = view.comments[index].comment.comment
  local anchor = row.target.target
  local side = anchor.range.start.side
  local source = { base = revision[side], head = revision.head }
  local location = vim.deepcopy(anchor)
  location.revision = source
  location.range.start.side, location.range['end'].side = 'base', 'base'
  historical_contexts[row.id] = { anchor = anchor, code = {
    path = anchor.path, base_path = anchor.path, revision = source,
    base_content = side == 'base' and base_text or head_text, head_content = head_text,
    historical = true, provenance = 'recorded', binary = false, location = location,
    original_base = { commit = revision.base, path = anchor.path, content = base_text, missing = false, provenance = 'recorded' },
  } }
end
write('historical_contexts.json', historical_contexts)
write('contexts.json', { historical = { available = true, anchor = view.comments[3].comment.comment.target.target,
  base_content = head_text, head_content = base_text } })
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function pair(context)
  local result = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
    if marker and marker.kind == (context and 'context' or 'diff') then
      local side = context and vim.wo[win].winbar:match('^Original (%a+) ') or marker.side
      if not context and marker.comment_role == 'original_base' then side = 'base'
      elseif not context and marker.comment_role == 'comment' and marker.revision.base == revision.head then side = 'head' end
      if side then result[side] = win end
    end
  end
  assert(result.base and result.head, 'Missing comparison panes')
  return result
end
local function assert_alignment(panes, left_line, right_line, reason, expected_kind, anchor_side)
  local focused = api.nvim_get_current_win()
  vim.cmd('redraw!')
  local positions = {}
  for side, line in pairs({ base = left_line, head = right_line }) do
    local win = panes[side]
    positions[side] = vim.fn.screenpos(win, line, 1)
  end
  for side, line in pairs({ base = left_line, head = right_line }) do
    local win = panes[side]
    local top = api.nvim_win_get_position(win)[1] + 1 + (vim.wo[win].winbar ~= '' and 1 or 0)
    assert(positions[side].row >= top and positions[side].row < top + api.nvim_win_get_height(win), reason .. ': ' .. side .. ' anchor not visible without scroll\n' .. vim.inspect(positions))
    local source, rendered = api.nvim_buf_get_lines(api.nvim_win_get_buf(win), line - 1, line, false)[1], {}
    for offset = 0, #source - 1 do rendered[#rendered + 1] = vim.fn.screenstring(positions[side].row, positions[side].col + offset) end
    assert(table.concat(rendered) == source, reason .. ': ' .. side .. ' anchor hidden by a fold or filler row')
  end
  assert(positions.base.row == positions.head.row, reason .. ': corresponding source lines misaligned without scroll\n' .. vim.inspect(positions))
  assert(vim.deep_equal(api.nvim_win_get_cursor(panes[anchor_side]), { anchor_side == 'base' and left_line or right_line, 0 }), reason .. ': selected anchor cursor moved from line start')
  assert(api.nvim_get_current_win() == focused and kind() == expected_kind, reason .. ': synchronization stole focus')
end
local function aligned(id, left_line, right_line, reason, anchor_side)
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == id end, 'Missing selected thread ' .. id)
  assert_alignment(pair(), left_line, right_line, reason, 'thread', anchor_side or 'head')
end

vim.cmd('Review')
wait(function() return kind() == 'home' end, 'Missing review home')

-- Direct diff composers restore both code views after their split is removed.
row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 }); input('<CR>')
wait(function() return kind() == 'diff' end, 'Missing direct comment source')
for _, origin_side in ipairs({ 'base', 'head' }) do
  for _, close in ipairs({ 'save', 'discard' }) do
    local panes = pair()
    for _, side in ipairs({ 'base', 'head' }) do
      api.nvim_win_call(panes[side], function()
        vim.fn.winrestview({ lnum = side == 'base' and 195 or head_line(195), col = 4,
          topline = side == 'base' and 150 or head_line(150), leftcol = 0 })
      end)
    end
    api.nvim_set_current_win(panes[origin_side]); vim.cmd('redraw!')
    input('Vj'); vim.cmd('redraw!')
    local views = {}
    for _, side in ipairs({ 'base', 'head' }) do views[side] = api.nvim_win_call(panes[side], vim.fn.winsaveview) end
    input('a')
    assert(kind() == 'composer', 'Visual direct comment did not open composer')
    if close == 'save' then
      api.nvim_buf_set_lines(0, 0, -1, false, { 'Direct ' .. origin_side .. ' range comment' })
      input(':wq<CR>')
    else input(':q!<CR>') end
    wait(function() return kind() == 'diff' end, 'Direct composer did not return to code')
    vim.cmd('redraw!')
    assert(api.nvim_get_current_win() == panes[origin_side], origin_side .. ' ' .. close .. ' returned to wrong code pane')
    for _, side in ipairs({ 'base', 'head' }) do
      local restored = api.nvim_win_call(panes[side], vim.fn.winsaveview)
      assert(vim.deep_equal(restored, views[side]), origin_side .. ' ' .. close .. ' changed ' .. side .. ' code view: '
        .. vim.inspect(views[side]) .. ' -> ' .. vim.inspect(restored))
    end
  end
end
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'Missing home after direct composers')
row('Feedback first-head')
input('<CR>')
aligned('first-head', 80, head_line(80), 'first discussion split')
vim.cmd('Review next comment')
aligned('later-base', 180, head_line(180), 'next base-side comment, reused discussion', 'base')
vim.cmd('Review previous comment')
aligned('first-head', 80, head_line(80), 'previous head-side comment')
input('s')
input(']r')
aligned('later-base', 180, head_line(180), 'stage then next comment', 'base')
vim.cmd('Review next comment')
aligned('historical', 140, head_line(140), 'historical referenced source', 'base')

-- Default diff folds must reveal the actual line, not merely the folded range.
vim.opt.diffopt = { 'internal', 'filler', 'closeoff', 'context:6' }
vim.cmd('Review previous comment')
aligned('later-base', 180, head_line(180), 'default-fold base-side comment', 'base')
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'Missing home before historical first split')
row('Feedback historical'); input('<CR>')
aligned('historical', 140, head_line(140), 'historical first discussion split with folds', 'base')

-- The explicit original-context pair uses the same post-layout reveal contract.
local original_tab = api.nvim_get_current_tabpage()
vim.cmd('Review context')
wait(function() return api.nvim_get_current_tabpage() ~= original_tab and kind() == 'context' end, 'Missing original context pair')
local panes = pair(true)
assert(api.nvim_get_current_win() == panes.head, 'Original context did not preserve head focus')
assert_alignment(panes, head_line(140), 140, 'original context pair', 'context', 'head')
print('Neovim diff anchor scroll alignment passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
