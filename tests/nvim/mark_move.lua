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
local function state() return read('state.json') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function command(value) vim.cmd('Review ' .. value) end
local function row(needle, exact)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if exact and (vim.trim(value) == needle or value:sub(-#needle - 2) == '  ' .. needle) or not exact and value:find(needle, 1, true) then
      api.nvim_win_set_cursor(0, { line, 0 }); return line
    end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, line, done)
  return {
    comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id, author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = 'src/c.rs', revision = revision, range = { start = { side = 'head', line = line }, ['end'] = { side = 'head', line = line } } } } } },
    fetched = true, done = done or false, pending_delete = false, conflict = false, summary = false,
  }
end
local initial = {
  id = 'mark-move-batch', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'mark-move-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Mark and move fixture', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = {}, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function populated()
  local value = vim.deepcopy(initial)
  value.comments = { comment('a', 1), comment('b', 1, true), comment('c', 2), comment('d', 2, true), comment('e', 3) }
  value.todo, value.done = { 'a', 'c', 'e' }, { 'b', 'd' }
  for index, letter in ipairs({ 'a', 'b', 'c', 'd', 'e' }) do
    value.files[#value.files + 1] = { path = 'src/' .. letter .. '.rs', viewed = index == 2 or index == 4 }
  end
  return value
end
write('state.json', populated())
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
vim.fn.writefile({ '0.7' }, fixture .. '/delay_fetch_objects')
require('buf_review').mark_move('comment', 1)
assert(#requests('actions') == 0 and notices[#notices]:find('No active review workspace; use :Review first', 1, true), 'missing normal no-workspace error')
command('')
wait(function() return kind() == 'home' and #requests('fetch_objects') == 1 end, 'initial home missing')
for _, key in ipairs({ ']F', '[F', ']R', '[R' }) do
  local mapping = vim.fn.maparg(key, 'n', false, true)
  assert(mapping.buffer == 1 and (mapping.callback or (mapping.rhs or '') ~= ''), 'missing buffer-local ' .. key)
end
for _, key in ipairs({ ']c', '[c' }) do assert(vim.fn.maparg(key, 'n') == '', 'native diff navigation overridden') end
for _, name in ipairs({ 'ViewedNext', 'ViewedPrevious', 'DoneNext', 'DonePrevious' }) do
  assert(vim.fn.maparg('<Plug>(Review' .. name .. ')', 'n') ~= '', 'missing mark-and-move Plug: ' .. name)
end
local function actions_before() return #requests('actions') end
local function marked(before, kind_name, identity)
  wait(function() return #records('action_results.jsonl') > before end, 'mark action did not complete')
  local sent = requests('actions')
  assert(#sent == before + 1 and #sent[#sent].actions == 1, 'mark-and-move must use one single-action RPC')
  local action = sent[#sent].actions[1]
  assert(action.action == kind_name and (action.path or action.comment) == identity, 'wrong mark target: ' .. vim.inspect(action))
  assert(kind_name == 'viewed' and action.viewed == true or kind_name == 'reaction' and action.present == true, 'mark toggled progress off')
end
local function reset(transform)
  local value = populated()
  if transform then transform(value) end
  value.generation = state().generation
  local todos = 0
  for _, item in ipairs(value.comments) do if item.fetched and not item.done then todos = todos + 1 end end
  write('state.json', value)
  local browser, selected, refreshed = kind() == 'prs', #requests('select'), #records('refresh_complete.jsonl')
  command('')
  wait(function()
    return kind() == 'home' and text():find('Todo (' .. todos .. ')', 1, true)
      and (browser and #requests('select') > selected or #records('refresh_complete.jsonl') > refreshed)
  end, 'reset did not refresh home')
end
local function file(path) return kind() == 'diff' and vim.b.buf_review.path == path end
local function thread(id) return kind() == 'thread' and vim.b.buf_review.object == id end

-- Without visible active code, home rows navigate without marking files.
row('src/c.rs', true)
local before = actions_before()
input(']F')
assert(#requests('actions') == before and not state().files[3].viewed, 'home file navigation marked an unopened file')
wait(function() return file('src/e.rs') end, 'next file did not skip viewed files')
vim.fn.delete(fixture .. '/delay_fetch_objects')
before = actions_before(); input('[F'); marked(before, 'viewed', 'src/e.rs')
wait(function() return file('src/c.rs') end, 'previous file lost stable file order')
before = actions_before(); input('[F'); marked(before, 'viewed', 'src/c.rs')
wait(function() return file('src/a.rs') end, 'previous file did not skip viewed files')
local code = #requests('code')
before = actions_before(); input('[F'); marked(before, 'viewed', 'src/a.rs')
assert(file('src/a.rs') and #requests('code') == code, 'first file wrapped or reopened at boundary')
before = actions_before(); input(']F')
assert(#requests('actions') == before and file('src/a.rs') and #requests('code') == code,
  'already-viewed file sent redundant mark or wrapped with no unfinished files')

local function open(id)
  for _, item in ipairs(state().comments) do
    if item.comment.comment.id == id and item.destination then
      row('src/c.rs', true); require('buf_review').enter()
      wait(function() return file('src/c.rs') end, 'draft code missing')
      api.nvim_win_set_cursor(0, { item.comment.comment.target.target.range.start.line, 0 })
      input('<CR>')
    end
  end
  row('Feedback ' .. id)
  require('buf_review').enter()
  wait(function() return thread(id) end, 'discussion missing for ' .. id)
  return api.nvim_get_current_buf(), api.nvim_get_current_win()
end
local function focus_diff(side)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' and meta.side == side then
      api.nvim_set_current_win(win)
      api.nvim_win_set_cursor(win, { 1, 0 }) -- Different from the open comment's code line.
      return win
    end
  end
  error('Missing ' .. side .. ' diff')
end
local function no_mark(before, message)
  assert(#requests('actions') == before, message or 'unexpected background comment mutation')
end

local function open_file_at(path)
  row(path, true); require('buf_review').enter()
  wait(function() return file(path) end, 'missing file ' .. path)
  return api.nvim_get_current_buf(), api.nvim_get_current_win()
end
local function file_focus(path, side)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' and meta.path == path and meta.side == (side or 'head') then
      api.nvim_set_current_win(win); return win
    end
  end
  error('Missing visible file ' .. path)
end
local function two_files()
  reset(); local old_code = open_file_at('src/c.rs')
  vim.cmd('split')
  local old_window = api.nvim_get_current_win() -- Keep reused code visible across the next open.
  command(''); wait(function() return kind() == 'home' end, 'home missing before second file')
  local _, file_window = open_file_at('src/a.rs')
  command('log'); wait(function() return kind() == 'log' end, 'file-navigation log missing')
  local log_window = api.nvim_get_current_win()
  api.nvim_set_current_win(old_window)
  api.nvim_set_current_win(log_window)
  return old_window, file_window, log_window, old_code
end

-- Home rows are navigation context even when the selected file is already viewed.
reset(); row('src/b.rs', true)
before = actions_before(); input(']F')
wait(function() return file('src/c.rs') end, 'home file navigation lost stable order or included viewed file')
no_mark(before, 'home file row marked without visible active code')

-- Both diff sides identify one file, and comment focus retains that active file.
for _, side in ipairs({ 'base', 'head', 'comment' }) do
  reset(); open('c')
  if side ~= 'comment' then file_focus('src/c.rs', side) end
  before = actions_before(); input(']F'); marked(before, 'viewed', 'src/c.rs')
  wait(function() return file('src/e.rs') end, side .. ' focus did not mark current file and advance')
end
-- Closing one side keeps the same logical file visible through its other side.
reset(); open('c')
local only_side
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
  if meta and meta.kind == 'diff' and meta.side == 'head' then only_side = win end
end
api.nvim_win_close(only_side, true)
before = actions_before(); input(']F'); marked(before, 'viewed', 'src/c.rs')
wait(function() return file('src/e.rs') end, 'remaining diff side did not keep active file markable')

-- Losing the last visible side while saving preserves the mark but cancels the jump.
reset(); open('c')
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
  if meta and meta.kind == 'diff' then
    if meta.side == 'base' then api.nvim_win_set_buf(win, api.nvim_create_buf(false, true))
    else only_side = win end
  end
end
vim.fn.writefile({ '0.25' }, fixture .. '/delay_actions')
before, code = actions_before(), #requests('code')
input(']F')
wait(function() return #requests('actions') == before + 1 end, 'last-side delayed action missing')
api.nvim_win_close(only_side, true)
wait(function() return #records('action_results.jsonl') > before end, 'last-side delayed action did not finish')
ReviewTest.wait(80)
assert(state().files[3].viewed and #requests('code') == code and thread('c'), 'hidden active file caused delayed jump')
vim.fn.delete(fixture .. '/delay_actions')

reset(function(value) value.files[3].viewed = true end)
open_file_at('src/c.rs')
before = actions_before(); input(']F')
wait(function() return file('src/e.rs') end, 'already-viewed file did not advance')
no_mark(before, 'already-viewed file sent redundant mutation')

-- Explicit diff focus chooses among visible files; unrelated readers retain that choice.
for _, path in ipairs({ 'src/a.rs', 'src/c.rs' }) do
  local old_window, file_window, log_window = two_files()
  api.nvim_set_current_win(path == 'src/a.rs' and file_window or old_window)
  api.nvim_set_current_win(log_window)
  before = actions_before(); input(']F'); marked(before, 'viewed', path)
  wait(function() return file(path == 'src/a.rs' and 'src/c.rs' or 'src/e.rs') end, 'non-code focus lost active file')
  assert(not state().files[path == 'src/a.rs' and 3 or 1].viewed, 'inactive visible file was marked')
  if api.nvim_win_is_valid(old_window) then api.nvim_win_close(old_window, true) end
end

-- Hidden/other-tab active files never fall back to another visible file for marking.
for _, mode in ipairs({ 'hidden', 'other-tab' }) do
  local old_window, _, log_window, old_code = two_files()
  local original_tab, extra_tab = api.nvim_get_current_tabpage()
  if mode == 'other-tab' then
    vim.cmd('tabnew'); extra_tab = api.nvim_get_current_tabpage()
    api.nvim_win_set_buf(0, old_code)
    api.nvim_set_current_tabpage(original_tab)
    api.nvim_set_current_win(log_window)
  end
  api.nvim_win_close(old_window, true)
  before, code = actions_before(), #requests('code')
  input(']F')
  wait(function() return #requests('code') > code and kind() == 'diff' end, mode .. ' active file prevented navigation')
  no_mark(before, mode .. ' active file caused marking of inactive visible code')
  if extra_tab then api.nvim_set_current_tabpage(extra_tab); vim.cmd('tabclose'); api.nvim_set_current_tabpage(original_tab) end
end

-- Delayed file marks cannot override cursor, navigation, comparison, or active-file changes.
for _, mode in ipairs({ 'cursor', 'browser', 'comparison', 'active' }) do
  local old_window, file_window, log_window
  if mode == 'active' then old_window, file_window, log_window = two_files()
  else reset(); open_file_at('src/c.rs') end
  local origin = api.nvim_get_current_win()
  vim.fn.writefile({ '0.25' }, fixture .. '/delay_actions')
  before, code = actions_before(), #requests('code')
  input(']F')
  wait(function() return #requests('actions') == before + 1 end, 'delayed file action missing')
  if mode == 'cursor' then api.nvim_win_set_cursor(0, { 3, 0 })
  elseif mode == 'browser' then command('prs'); wait(function() return kind() == 'prs' end, 'file PR browser missing')
  elseif mode == 'active' then api.nvim_set_current_win(file_window); api.nvim_set_current_win(origin)
  else
    write('latest_comparison.json', { base = string.rep('c', 40), head = string.rep('d', 40) })
    command('compare latest')
    wait(function() return state().comparison.head == string.rep('d', 40) end, 'file comparison did not change')
  end
  wait(function() return #records('action_results.jsonl') > before end, 'delayed file action did not finish')
  ReviewTest.wait(80)
  assert(#requests('code') == code + (mode == 'comparison' and 1 or 0), mode .. ' did not cancel file jump: expected ' .. (code + (mode == 'comparison' and 1 or 0)) .. ', got ' .. #requests('code') .. '\n' .. vim.inspect(vim.list_slice(requests('code'), code + 1)))
  if mode == 'browser' then assert(kind() == 'prs', 'delayed file mark replaced browser') end
  vim.fn.delete(fixture .. '/delay_actions'); vim.fn.delete(fixture .. '/latest_comparison.json')
  if old_window and api.nvim_win_is_valid(old_window) then api.nvim_win_close(old_window, true) end
end

local function cross_file(value)
  value.comments[1].comment.comment.target.target.path = 'src/a.rs'
  local historical = value.comments[3].comment.comment.target.target
  historical.path = 'src/e.rs'
  historical.revision = { base = 'old-base', head = 'old-head' }
  historical.range.start.line, historical.range['end'].line = 3, 3
end
local function diff_pair(path, head_line, historical)
  local sides = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' then
      assert(meta.path == path and vim.deep_equal(meta.revision, historical and { base = 'old-head', head = revision.head } or revision), 'comment navigation changed path/comparison incorrectly')
      local expected = meta.side == 'base' and { 'one', historical and 'historical head' or 'two', 'three' }
        or { 'one', historical and 'latest PR head' or 'changed', 'three' }
      assert(vim.deep_equal(api.nvim_buf_get_lines(buf, 0, -1, false), expected), 'comment navigation did not load pinned file content')
      sides[meta.side] = win
    end
  end
  assert(sides.base and sides.head, 'comment navigation did not open both diff sides')
  if head_line then assert(api.nvim_win_get_cursor(sides[historical and 'base' or 'head'])[1] == head_line, 'comment navigation jumped to wrong reference line') end
  return sides
end

-- Forward/backward checklist navigation follows each root's file even for historical anchors.
reset(cross_file); open('a'); diff_pair('src/a.rs', 1)
before = actions_before(); input(']R'); marked(before, 'reaction', 'a')
wait(function() return thread('c') end, 'next checklist comment did not open historical discussion')
diff_pair('src/e.rs', 3, true)
assert(text():find('Historical location', 1, true), 'historical comment lost its location label')
assert(requests('historical_code')[#requests('historical_code')].comment == 'c', 'historical discussion did not request original comparison')
reset(cross_file); open('e'); diff_pair('src/c.rs', 3)
before = actions_before(); input('[R'); marked(before, 'reaction', 'e')
wait(function() return thread('c') end, 'previous checklist comment did not open historical discussion')
diff_pair('src/e.rs', 3, true)

-- Enter and ordinary Todo navigation share the same file-following behavior.
reset(cross_file); open('c'); diff_pair('src/e.rs', 3, true)
command('next todo'); wait(function() return thread('e') end, 'ordinary next did not open next comment')
diff_pair('src/c.rs', 3)
command('previous todo'); wait(function() return thread('c') end, 'ordinary previous did not open historical comment')
diff_pair('src/e.rs', 3, true)

-- Replies follow the root file and use its current anchor, not their own nonexistent range.
reset(function(value)
  value.comments[1].comment.comment.target.target.path = 'src/a.rs'
  value.comments[1].comment.comment.target.target.range.start.line = 2
  value.comments[1].comment.comment.target.target.range['end'].line = 2
  value.comments[3].comment.comment.target = { kind = 'reply', target = 'a' }
end)
open('a'); diff_pair('src/a.rs', 2)
code = #requests('code')
before = actions_before(); input(']R'); marked(before, 'reaction', 'a')
wait(function() return #requests('code') > code and thread('a') and api.nvim_win_get_cursor(0)[1] > 5 end, 'reply navigation lost its root discussion')
diff_pair('src/a.rs', 2)
before = actions_before(); input(']R'); marked(before, 'reaction', 'c')
wait(function() return thread('e') end, 'reply did not retain selected comment identity')
diff_pair('src/c.rs', 3)

-- Historical replies inherit root comparison and its verified line numbers.
reset(function(value)
  local root = value.comments[1].comment.comment.target.target
  root.path, root.revision = 'src/a.rs', { base = 'old-base', head = 'old-head' }
  root.range.start.line, root.range['end'].line = 3, 3
  value.comments[3].comment.comment.target = { kind = 'reply', target = 'a' }
end)
row('Feedback c'); require('buf_review').enter()
wait(function() return thread('a') end, 'historical reply did not open root discussion')
diff_pair('src/a.rs', 3, true)
assert(text():find('Historical location', 1, true), 'historical reply lost location label')

-- Historical paths absent from current files still open original code; PR-wide/current missing paths preserve existing diff.
for _, mode in ipairs({ 'missing-historical', 'missing-current', 'wide' }) do
  reset(function(value)
    cross_file(value)
    local target = value.comments[3].comment.comment.target
    if mode ~= 'wide' then
      target.target.path = 'removed.rs'
      if mode == 'missing-current' then target.target.revision = revision end
    else value.comments[3].comment.comment.target = { kind = 'pull_request' } end
  end)
  open('a'); diff_pair('src/a.rs', 1)
  code = #requests('code')
  command('next todo'); wait(function() return thread('c') end, mode .. ' comment did not open')
  assert(#requests('code') == code, mode .. ' comment requested unavailable or unrelated code')
  if mode == 'missing-historical' then diff_pair('removed.rs', 3, true) else diff_pair('src/a.rs', 1) end
end

-- Home rows supply global navigation context without marking an unopened comment.
reset(); row('Feedback c')
before = actions_before(); input(']R')
wait(function() return thread('e') end, 'home navigation did not open next Todo')
no_mark(before, 'home row was marked without an open discussion')
before = actions_before(); input('[R'); marked(before, 'reaction', 'e')
wait(function() return thread('c') end, 'previous Todo followed reordered Done section')
before = actions_before(); input('[R'); marked(before, 'reaction', 'c')
wait(function() return thread('a') end, 'previous Todo skipped first item')
code = #requests('code')
before = actions_before(); input('[R'); marked(before, 'reaction', 'a')
assert(thread('a') and #requests('code') == code, 'first Todo wrapped at boundary')
reset(); row('src/c.rs', true); require('buf_review').enter()
wait(function() return file('src/c.rs') end, 'code view missing')
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
assert(kind() == 'threads', 'same-line thread list missing')
row('Feedback c')
before = actions_before(); input(']R')
wait(function() return thread('c') end, 'thread-list entry skipped its unfinished comment')
no_mark(before, 'thread-list row was marked without an open discussion')

-- A location can contain several roots and unfinished replies to Done roots.
-- Entry opens one unfinished comment without marking it; later presses traverse
-- each individual Todo globally, even when code anchors and root readers repeat.
local function multiple_at_location(value)
  local done_root = comment('b', 2, true)
  local first_reply, second_reply = comment('b-reply', 2), comment('c-reply', 2)
  first_reply.comment.comment.target = { kind = 'reply', target = 'b' }
  second_reply.comment.comment.target = { kind = 'reply', target = 'c' }
  value.comments = { comment('a', 1), done_root, first_reply, comment('c', 2), second_reply, comment('d', 2), comment('e', 3) }
  value.todo, value.done = { 'a', 'b-reply', 'c', 'c-reply', 'd', 'e' }, { 'b' }
end
local function selected_comment(id)
  local root_id = ({ ['b-reply'] = 'b', ['c-reply'] = 'c' })[id] or id
  if not thread(root_id) then return false end
  local heading = api.nvim_win_get_cursor(0)[1]
  return api.nvim_buf_get_lines(0, heading + 1, heading + 2, false)[1] == 'Feedback ' .. id
end
for _, direction in ipairs({ 1, -1 }) do
  reset(multiple_at_location)
  open_file_at('src/c.rs')
  api.nvim_win_set_cursor(0, { 2, 0 })
  before = actions_before(); input(direction > 0 and ']R' or '[R')
  wait(function() return selected_comment(direction > 0 and 'b-reply' or 'd') end, 'direct code entry required opening the location picker first')
  no_mark(before, 'direct code entry marked a comment before opening its conversation')
end
for _, mode in ipairs({ 'code', 'picker' }) do
  for _, direction in ipairs({ 1, -1 }) do
    reset(multiple_at_location)
    -- Seed deliberately wrong global selection, then replace that conversation
    -- with the current location's picker. Entry must use the location instead.
    open(direction > 0 and 'e' or 'a')
    local head_window = focus_diff('head')
    api.nvim_win_set_cursor(head_window, { 2, 0 })
    input('<CR>')
    assert(kind() == 'threads', 'multi-root location picker missing')
    row('Feedback c')
    if mode == 'code' then api.nvim_set_current_win(head_window) end
    local key = direction > 0 and ']R' or '[R'
    local order = direction > 0 and { 'b-reply', 'c', 'c-reply', 'd', 'e' } or { 'd', 'c-reply', 'c', 'b-reply', 'a' }
    before = actions_before(); input(key)
    wait(function() return selected_comment(order[1]) end, mode .. ' ' .. key .. ' did not enter unfinished comment at current location')
    no_mark(before, mode .. ' entry marked a comment before opening its conversation')
    for index, identity in ipairs(order) do
      before = actions_before(); input(key); marked(before, 'reaction', identity)
      local next_identity = order[index + 1] or identity
      wait(function() return selected_comment(next_identity) end, mode .. ' ' .. key .. ' skipped/repeated Todo after ' .. identity)
    end
    local saved = state()
    for _, item in ipairs(saved.comments) do
      local identity = item.comment.comment.id
      assert(item.done == (identity ~= (direction > 0 and 'a' or 'e')), mode .. ' traversal marked wrong comment ' .. identity)
    end
  end
end

-- When every comment at the location is Done, both entry surfaces retain the
-- existing global Todo fallback rather than marking or reopening local work.
-- The last reverse traversal marked a, so its retained continuation leads to e.
for _, mode in ipairs({ 'code', 'picker' }) do
  reset(function(value)
    multiple_at_location(value)
    for index = 2, #value.comments - 1 do value.comments[index].done = true end
    value.todo, value.done = { 'a', 'e' }, { 'b', 'b-reply', 'c', 'c-reply', 'd' }
  end)
  open('a')
  local head_window = focus_diff('head')
  api.nvim_win_set_cursor(head_window, { 2, 0 })
  input('<CR>')
  row('Feedback c')
  if mode == 'code' then api.nvim_set_current_win(head_window) end
  before = actions_before(); input(']R')
  wait(function() return selected_comment('e') end, mode .. ' all-Done location did not preserve global Todo continuation')
  no_mark(before, mode .. ' all-Done location marked a background or completed comment')
end

-- Either code pane marks the one visible discussion, independent of its cursor line.
for _, side in ipairs({ 'head', 'base' }) do
  reset(); open('c'); focus_diff(side)
  before = actions_before(); input(']R'); marked(before, 'reaction', 'c')
  wait(function() return thread('e') end, side .. ' focus did not open next diff and discussion')
  assert(vim.b.buf_review.object == 'e', 'wrong destination discussion')
end

-- Empty discussion lines retain the explicitly opened comment identity.
reset(); open('c'); api.nvim_win_set_cursor(0, { 1, 0 })
before = actions_before(); input(']R'); marked(before, 'reaction', 'c')
wait(function() return thread('e') end, 'blank discussion line lost opened comment')

-- Within a conversation, the selected reply is the visible comment to mark.
reset(function(value)
  local reply = comment('reply', 2)
  reply.comment.comment.target = { kind = 'reply', target = 'c' }
  table.insert(value.comments, 4, reply)
end)
open('c'); row('Feedback reply')
focus_diff('head')
before = actions_before(); input(']R'); marked(before, 'reaction', 'reply')
wait(function() return thread('e') end, 'visible reply selection did not advance')
assert(not state().comments[3].done, 'reply selection marked root instead')

-- Closed/hidden discussions and discussions on other tabs are not mark targets.
reset(); local _, discussion_win = open('c')
api.nvim_win_close(discussion_win, false)
focus_diff('head')
before = actions_before(); input(']R')
wait(function() return thread('a') end, 'closed discussion did not enter unfinished comment at current code location')
no_mark(before)
reset(); local other_reader = open('c')
local review_tab = api.nvim_get_current_tabpage()
vim.cmd('tabnew')
local other_tab = api.nvim_get_current_tabpage()
api.nvim_win_set_buf(0, other_reader)
api.nvim_set_current_tabpage(review_tab)
command(''); wait(function() return kind() == 'home' end, 'home missing with other-tab discussion')
row('Feedback c')
before = actions_before(); input(']R')
wait(function() return thread('e') end, 'other-tab discussion prevented navigation')
no_mark(before)
api.nvim_set_current_tabpage(other_tab); vim.cmd('tabclose')
api.nvim_set_current_tabpage(review_tab)

-- Displaying the same reader twice still identifies one comment.
reset(); local duplicate = open('c')
vim.cmd('split'); api.nvim_win_set_buf(0, duplicate)
local extra = api.nvim_get_current_win()
focus_diff('head')
before = actions_before(); input(']R'); marked(before, 'reaction', 'c')
wait(function() return thread('e') end, 'duplicate reader windows were treated as ambiguous')
if api.nvim_win_is_valid(extra) then api.nvim_win_close(extra, true) end

local function two_readers()
  reset(); local first_reader = open('a')
  vim.cmd('split'); local first_window = api.nvim_get_current_win()
  command(''); wait(function() return kind() == 'home' end, 'home missing before second discussion')
  local _, second_window = open('c')
  api.nvim_set_current_win(first_window)
  return first_reader, first_window, second_window
end

-- Explicit discussion focus chooses among several visible readers; code retains that choice.
for _, selected in ipairs({ 'a', 'c', 'redraw' }) do
  local _, first_window, second_window = two_readers()
  api.nvim_set_current_win(selected == 'c' and second_window or first_window)
  focus_diff('head')
  if selected == 'redraw' then
    local refreshed = #records('refresh_complete.jsonl')
    command('refresh')
    wait(function() return #records('refresh_complete.jsonl') > refreshed end, 'background redraw missing')
  end
  local identity = selected == 'c' and 'c' or 'a'
  before = actions_before(); input(']R'); marked(before, 'reaction', identity)
  wait(function() return thread(identity == 'a' and 'c' or 'e') end, 'code focus lost active discussion')
  assert(not state().comments[identity == 'a' and 3 or 1].done, 'inactive visible discussion was marked')
  if api.nvim_win_is_valid(first_window) then api.nvim_win_close(first_window, true) end
end

-- Hiding the active reader cannot fall back to a different visible discussion.
local _, first_window = two_readers()
focus_diff('head')
api.nvim_win_close(first_window, true)
before, code = actions_before(), #requests('code')
input(']R')
wait(function() return #requests('code') > code and kind() == 'thread' end, 'hidden active reader prevented navigation')
no_mark(before, 'inactive visible reader replaced hidden active reader as mark target')

-- An active reader on another tab cannot mark an inactive reader on this tab.
reset(); local first_reader = open('a')
review_tab = api.nvim_get_current_tabpage()
vim.cmd('tabnew'); other_tab = api.nvim_get_current_tabpage()
api.nvim_win_set_buf(0, first_reader)
api.nvim_set_current_tabpage(review_tab)
command(''); wait(function() return kind() == 'home' end, 'home missing before other-tab active reader')
open('c'); focus_diff('head')
api.nvim_set_current_tabpage(other_tab)
api.nvim_set_current_tabpage(review_tab)
before, code = actions_before(), #requests('code')
input(']R')
wait(function() return #requests('code') > code and kind() == 'thread' end, 'other-tab active reader prevented navigation')
no_mark(before, 'other-tab active reader caused a local mark')
api.nvim_set_current_tabpage(other_tab); vim.cmd('tabclose')
api.nvim_set_current_tabpage(review_tab)

local current_buf, notice_count

-- Already Done, local drafts, and pending deletions advance without attempting a mark.
for _, case in ipairs({ 'done', 'draft', 'deleted' }) do
  reset(function(value)
    local item = value.comments[3]
    if case == 'done' then item.done = true
    elseif case == 'draft' then item.fetched = false; item.comment.origin = 'draft'; item.destination = 'standalone'
    else item.pending_delete = true end
  end)
  open('c'); focus_diff('head')
  before = actions_before(); input(']R')
  wait(function() return thread('e') end, case .. ' comment did not advance')
  no_mark(before, case .. ' comment attempted an invalid mark')
end

-- Failed file/comment marks report errors and still advance without changing progress.
reset(); open('c')
vim.fn.writefile({ '' }, fixture .. '/fail_viewed')
notice_count = #notices
before = actions_before(); input(']F'); marked(before, 'viewed', 'src/c.rs')
wait(function() return file('src/e.rs') end, 'failed file mark prevented navigation')
assert(not state().files[3].viewed, 'failed file mark changed saved progress')
local found_viewed_failure = false
for at = notice_count + 1, #notices do if notices[at]:find('Fixture viewed conflict', 1, true) then found_viewed_failure = true end end
assert(found_viewed_failure, 'failed file mark error was hidden')
vim.fn.delete(fixture .. '/fail_viewed')
reset(); open('c'); focus_diff('head')
vim.fn.writefile({ '' }, fixture .. '/fail_reaction')
notice_count = #notices
before = actions_before(); input(']R'); marked(before, 'reaction', 'c')
wait(function() return thread('e') end, 'failed comment mark prevented navigation')
assert(not state().comments[3].done, 'failed comment mark changed saved progress')
local found_failure = false
for at = notice_count + 1, #notices do if notices[at]:find('Fixture reaction conflict', 1, true) then found_failure = true end end
assert(found_failure, 'failed comment mark error was hidden')
vim.fn.delete(fixture .. '/fail_reaction')

-- A delayed mark cannot override deliberate cursor movement, navigation, or pin changes.
for _, mode in ipairs({ 'cursor', 'browser', 'comparison', 'close' }) do
  reset(); local _, open_window = open('c'); focus_diff('head')
  vim.fn.writefile({ '0.25' }, fixture .. '/delay_actions')
  code = #requests('code')
  before = actions_before(); input(']R')
  wait(function() return #requests('actions') == before + 1 end, 'delayed action missing')
  if mode == 'cursor' then api.nvim_win_set_cursor(0, { 3, 0 })
  elseif mode == 'close' then api.nvim_win_close(open_window, false)
  elseif mode == 'browser' then command('prs'); wait(function() return kind() == 'prs' end, 'PR browser missing')
  else
    write('latest_comparison.json', { base = string.rep('c', 40), head = string.rep('d', 40) })
    command('compare latest')
    wait(function() return state().comparison.head == string.rep('d', 40) end, 'comparison did not change')
  end
  wait(function() return #records('action_results.jsonl') > before end, 'delayed action never completed')
  ReviewTest.wait(80)
  local expected_code = code + (mode == 'comparison' and 1 or 0) -- Pin change reloads its own code.
  assert(#requests('code') == expected_code and not thread('e'), mode .. ' did not cancel delayed movement')
  if mode == 'browser' then assert(kind() == 'prs', 'delayed mark replaced PR browser') end
  vim.fn.delete(fixture .. '/delay_actions')
  vim.fn.delete(fixture .. '/latest_comparison.json')
end

-- Choosing another discussion during a delayed mark cancels its automatic jump.
local _, active_window, other_window = two_readers()
local origin_window = focus_diff('head')
vim.fn.writefile({ '0.25' }, fixture .. '/delay_actions')
before, code = actions_before(), #requests('code')
input(']R')
wait(function() return #requests('actions') == before + 1 end, 'delayed active-comment action missing')
api.nvim_set_current_win(other_window)
api.nvim_set_current_win(origin_window)
wait(function() return #records('action_results.jsonl') > before end, 'delayed active-comment action did not finish')
ReviewTest.wait(80)
assert(state().comments[1].done and #requests('code') == code and api.nvim_get_current_win() == origin_window,
  'changed active comment did not cancel delayed automatic navigation')
vim.fn.delete(fixture .. '/delay_actions')
if api.nvim_win_is_valid(active_window) then api.nvim_win_close(active_window, true) end

-- File/comment keys exist throughout Review, while unsaved composers block navigation.
reset(); row('Feedback c'); command('reply')
assert(kind() == 'composer', 'reply composer missing')
for _, key in ipairs({ ']R', '[R', ']F', '[F' }) do
  local mapping = vim.fn.maparg(key, 'n', false, true)
  assert(mapping.buffer == 1 and (mapping.callback or (mapping.rhs or '') ~= ''), 'navigation missing in composer: ' .. key)
end
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved reply' })
current_buf, code, before = api.nvim_get_current_buf(), #requests('code'), actions_before()
for _, key in ipairs({ ']R', '[R', ']F', '[F' }) do
  input(key)
  assert(api.nvim_get_current_buf() == current_buf and api.nvim_get_current_line() == 'Unsaved reply'
    and vim.bo.modified and #requests('code') == code, key .. ' discarded unsaved composer')
  no_mark(before)
end
input(':q!<CR>')

-- Active readers retain their bound batch even when another session shows the same IDs.
reset(); local old_reader = open('c')
file_focus('src/c.rs')
local old_code = api.nvim_get_current_buf()
vim.cmd('split')
local preserved_code_window = api.nvim_get_current_win()
local other = populated()
other.id, other.review.id = 'mark-move-other-batch', 'mark-move-other-snapshot'
local views = read('views.json')
views[other.id] = other
write('views.json', views)
write('routes.json', {
  entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id },
  [initial.locator] = { route = 'batch', batch = other.id, fresh_review = other.review.id },
})
for _, case in ipairs({ { key = ']F', buffer = old_code, action = 'viewed', id = 'src/c.rs' },
  { key = ']R', buffer = old_reader, action = 'reaction', id = 'c' } }) do
  command('prs'); wait(function() return kind() == 'prs' end, 'cross-batch PR browser missing')
  row('#1 Fixture review'); require('buf_review').enter()
  wait(function() return kind() == 'home' and vim.b.buf_review.workspace == other.id end, 'second batch missing')
  row('src/c.rs', true); require('buf_review').enter()
  wait(function() return file('src/c.rs') end, 'second batch code missing')
  local second_code = api.nvim_get_current_win()
  local target_window = second_code
  if case.key == ']F' then
    command('log'); wait(function() return kind() == 'log' end, 'second batch log missing')
    target_window = api.nvim_get_current_win()
    api.nvim_set_current_win(second_code)
  end
  vim.cmd('split'); api.nvim_win_set_buf(0, case.buffer)
  local retained_window = api.nvim_get_current_win()
  api.nvim_set_current_win(target_window)
  if case.key == ']F' then
    api.nvim_set_current_win(second_code); input('a')
    assert(kind() == 'composer', 'cross-batch composer missing')
    api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved text in focused batch' })
    local composer_buffer, composer_window = api.nvim_get_current_buf(), api.nvim_get_current_win()
    api.nvim_set_current_win(retained_window); api.nvim_set_current_win(composer_window)
    before = actions_before(); input(']F')
    no_mark(before, 'bound-session target bypassed focused session unsaved composer')
    assert(api.nvim_get_current_buf() == composer_buffer and vim.bo.modified,
      'bound-session file navigation discarded focused session composer')
    input(':q!<CR>')
    command('log'); wait(function() return kind() == 'log' end, 'cross-batch log return missing')
    target_window = api.nvim_get_current_win()
    api.nvim_set_current_win(retained_window); api.nvim_set_current_win(target_window)
  end
  before = actions_before(); input(case.key); marked(before, case.action, case.id)
  assert(requests('actions')[before + 1].workspace == initial.id, 'active target marked focused pane batch instead')
  local saved = read('views.json')[initial.id]
  if case.key == ']F' then
    assert(not state().files[3].viewed and saved.files[3].viewed, 'same-path file progress saved in wrong batch')
    wait(function() return file('src/e.rs') and vim.b.buf_review.workspace == initial.id end, 'active file navigation lost its batch')
  else
    assert(not state().comments[3].done and saved.comments[3].done, 'same-ID comment progress saved in wrong batch')
    wait(function() return thread('e') and vim.b.buf_review.workspace == initial.id end, 'active reader navigation lost its batch')
  end
  if api.nvim_win_is_valid(retained_window) then api.nvim_win_close(retained_window, true) end
end

if api.nvim_win_is_valid(preserved_code_window) then api.nvim_win_close(preserved_code_window, true) end
vim.cmd('new')
for _, key in ipairs({ ']F', '[F', ']R', '[R' }) do assert(vim.fn.maparg(key, 'n') == '', 'mark-and-move leaked into source') end

print('Neovim mark-and-move workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
