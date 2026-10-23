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
local function code_requests()
  local result = {}
  for _, request in ipairs(requests()) do
    if request.op == 'code' or request.op == 'historical_code' then result[#result + 1] = request end
  end
  return result
end
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
    fetched = true, done = done or false, conflict = false, summary = false,
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
command('')
wait(function() return kind() == 'home' and #requests('fetch_objects') == 1 end, 'initial home missing')
assert(require('buf_review').mark_move == nil, 'combined mark-and-move API remains')
local function navigation_maps()
  for _, key in ipairs({ ']r', '[r', ']f', '[f' }) do
    local mapping = vim.fn.maparg(key, 'n', false, true)
    assert(mapping.buffer == 1 and (mapping.callback or (mapping.rhs or '') ~= ''), 'missing buffer-local ' .. key)
  end
  for _, key in ipairs({ ']F', '[F', ']R', '[R', ']c', '[c' }) do
    assert(vim.fn.maparg(key, 'n') == '', 'unexpected navigation mapping ' .. key)
  end
  for _, name in ipairs({ 'ViewedNext', 'ViewedPrevious', 'DoneNext', 'DonePrevious' }) do
    assert(vim.fn.maparg('<Plug>(Review' .. name .. ')', 'n') == '', 'combined Plug API remains: ' .. name)
  end
end
navigation_maps()
local function actions_before() return #requests('actions') end
local function marked(before, kind_name, identity)
  wait(function() return #records('action_results.jsonl') > before end, 'stage action did not complete')
  local sent = requests('actions')
  assert(#sent == before + 1 and #sent[#sent].actions == 1, 'staging must use one single-action RPC')
  local action = sent[#sent].actions[1]
  assert(action.action == kind_name and (action.path or action.comment) == identity, 'wrong mark target: ' .. vim.inspect(action))
  assert(kind_name == 'viewed' and action.viewed == true or kind_name == 'reaction' and action.present == true, 'staging toggled progress off')
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
    local displayed = meta and meta.side
    if meta and meta.comment_role == 'original_base' then displayed = 'base'
    elseif meta and meta.comment_role == 'comment' and meta.revision.base == revision.head then displayed = 'head' end
    if meta and meta.kind == 'diff' and meta.path == path and displayed == (side or 'head') then
      api.nvim_set_current_win(win); return win
    end
  end
  error('Missing visible file ' .. path)
end
-- Counts repeat selection in memory, loading only the final destination.
for _, marked_items in ipairs({ false, true }) do
  for _, target in ipairs({ 'file', 'comment' }) do
    for _, direction in ipairs({ 1, -1 }) do
      reset(function(value)
        for index, item in ipairs(value.comments) do
          item.comment.comment.target.target.path = value.files[index].path
          if not marked_items then item.done = false end
        end
        if not marked_items then
          value.todo, value.done = { 'a', 'b', 'c', 'd', 'e' }, {}
          for _, item in ipairs(value.files) do item.viewed = false end
        end
      end)
      local start = direction > 0 and 'a' or 'e'
      if target == 'file' then open_file_at('src/' .. start .. '.rs') else open(start) end
      local before, code, historical = actions_before(), #code_requests(), #requests('historical_code')
      local saved = state()
      local key = tostring(marked_items and 7 or 4) .. (direction > 0 and ']' or '[') .. (target == 'file' and 'f' or 'r')
      local destination = marked_items and 'c' or (direction > 0 and 'e' or 'a')
      input(key)
      wait(function() return target == 'file' and file('src/' .. destination .. '.rs') or target == 'comment' and thread(destination) end,
        key .. ' ignored count, marked exclusion, or cyclic order')
      local loads = code_requests()
      assert(#loads == code + 1 and (target == 'file' and loads[#loads].path == 'src/' .. destination .. '.rs'
        or target == 'comment' and loads[#loads].comment == destination)
        and #requests('historical_code') == historical + (target == 'comment' and 1 or 0), key .. ' loaded intermediate destinations')
      no_mark(before, key .. ' staged progress')
      assert(vim.deep_equal(saved.files, state().files) and vim.deep_equal(saved.comments, state().comments), key .. ' mutated progress')
    end
  end
end

-- From code, entering the local comment is step one; later steps are global.
for _, direction in ipairs({ 1, -1 }) do
  reset(); open_file_at('src/c.rs'); api.nvim_win_set_cursor(0, { 2, 0 })
  local before, code = actions_before(), #code_requests()
  input(direction > 0 and '2]r' or '2[r')
  wait(function() return thread(direction > 0 and 'e' or 'a') end, 'count repeated code-location entry instead of advancing globally')
  assert(#code_requests() == code + 1, 'counted code entry loaded intermediate discussion')
  no_mark(before)
end

-- With no current object, a sole eligible destination opens once for any count.
for _, target in ipairs({ 'file', 'comment' }) do
  for _, direction in ipairs({ 1, -1 }) do
    reset(function(value)
      for _, item in ipairs(value.comments) do item.done = item.comment.comment.id ~= 'c' end
      value.todo, value.done = { 'c' }, { 'a', 'b', 'd', 'e' }
      for _, item in ipairs(value.files) do item.viewed = item.path ~= 'src/c.rs' end
    end)
    command('log'); wait(function() return kind() == 'log' end, 'counted navigation log missing')
    local before, code = actions_before(), #code_requests()
    input('9' .. (direction > 0 and ']' or '[') .. (target == 'file' and 'f' or 'r'))
    wait(function() return target == 'file' and file('src/c.rs') or target == 'comment' and thread('c') end,
      'remaining count discarded sole destination after finding it')
    assert(#code_requests() == code + 1, 'count reopened sole destination')
    no_mark(before)
  end
end

-- Every navigation key skips marked items without changing progress.
for _, target in ipairs({ 'comment', 'file' }) do
  for _, focused in ipairs({ false, true }) do
    for _, direction in ipairs({ 1, -1 }) do
      reset()
      if focused then
        if target == 'comment' then open('c') else open_file_at('src/c.rs') end
      else row(target == 'comment' and 'Feedback c' or 'src/c.rs', target == 'file') end
      navigation_maps()
      local before = actions_before()
      input((direction > 0 and ']' or '[') .. (target == 'comment' and 'r' or 'f'))
      local destination = direction > 0 and 'e' or 'a'
      wait(function() return target == 'comment' and thread(destination) or target == 'file' and file('src/' .. destination .. '.rs') end,
        'navigation lost highlighted item, backend order, or marked exclusion')
      no_mark(before, 'navigation staged progress')
      assert(not state().comments[3].done and not state().files[3].viewed, 'navigation changed current item progress')
    end
  end
end

-- Tail/head exhaustion wraps; current is never its own destination.
for _, target in ipairs({ 'comment', 'file' }) do
  for _, direction in ipairs({ 1, -1 }) do
    reset()
    local start, destination = direction > 0 and 'e' or 'a', direction > 0 and 'a' or 'e'
    if target == 'comment' then open(start) else open_file_at('src/' .. start .. '.rs') end
    local before = actions_before()
    input((direction > 0 and ']' or '[') .. (target == 'comment' and 'r' or 'f'))
    wait(function() return target == 'comment' and thread(destination) or target == 'file' and file('src/' .. destination .. '.rs') end,
      'navigation failed to wrap')
    no_mark(before)
  end
end
for _, target in ipairs({ 'comment', 'file' }) do
  reset(function(value)
    for _, item in ipairs(value.comments) do item.done = true end
    value.todo, value.done = {}, { 'a', 'b', 'c', 'd', 'e' }
    for _, item in ipairs(value.files) do item.viewed = true end
  end)
  if target == 'comment' then open('c') else open_file_at('src/c.rs') end
  local before, code = actions_before(), #code_requests()
  for _, direction in ipairs({ ']', '[' }) do
    input('9' .. direction .. (target == 'comment' and 'r' or 'f'))
    assert((target == 'comment' and thread('c') or target == 'file' and file('src/c.rs')) and #code_requests() == code,
      'no eligible destination reopened or moved current item')
    no_mark(before, 'already marked item sent redundant mutation')
  end
end

-- A sole eligible current row stays highlighted without reopening or marking it.
for _, target in ipairs({ 'comment', 'file' }) do
  reset(function(value)
    for _, item in ipairs(value.comments) do item.done = item.comment.comment.id ~= 'c' end
    value.todo, value.done = { 'c' }, { 'a', 'b', 'd', 'e' }
    for _, item in ipairs(value.files) do item.viewed = item.path ~= 'src/c.rs' end
  end)
  row(target == 'comment' and 'Feedback c' or 'src/c.rs', target == 'file')
  local before, code = actions_before(), #code_requests()
  for _, direction in ipairs({ ']', '[' }) do
    input('9' .. direction .. (target == 'comment' and 'r' or 'f'))
    assert(kind() == 'home' and #code_requests() == code, 'navigation returned current row as destination')
    no_mark(before, 'home row was marked')
  end
end

-- No highlighted object means first/last eligible, regardless of earlier readers.
for _, target in ipairs({ 'comment', 'file' }) do
  for _, direction in ipairs({ 1, -1 }) do
    reset(); open('c')
    command('log'); wait(function() return kind() == 'log' end, 'log missing')
    local before = actions_before()
    input((direction > 0 and ']' or '[') .. (target == 'comment' and 'r' or 'f'))
    local destination = direction > 0 and 'a' or 'e'
    wait(function() return target == 'comment' and thread(destination) or target == 'file' and file('src/' .. destination .. '.rs') end,
      'unrelated reader reused remembered navigation context')
    no_mark(before, 'unrelated reader marked visible background pane')
  end
end
reset(); row('src/c.rs', true)
local before = actions_before(); input(']f')
wait(function() return file('src/e.rs') end, 'initial file move failed')
command(''); wait(function() return kind() == 'home' end, 'home missing after file move')
api.nvim_win_set_cursor(0, { 1, 0 })
input(']f')
wait(function() return file('src/a.rs') end, ']f, :Review, ]f retained previous file')
no_mark(before, 'home navigation marked remembered visible file')

-- Projected local viewed progress wins over the unchanged remote read snapshot.
reset(function(value)
  value.review.read.files = vim.deepcopy(value.files)
  value.review.read.files[3].viewed = false
  value.files[3].viewed = true
  value.pending = { { id = 'local-viewed-c', change = { kind = 'set_file_viewed', path = 'src/c.rs', viewed = true } } }
end)
row('src/a.rs', true)
before = actions_before(); input(']f')
wait(function() return file('src/e.rs') end, ']f included locally staged viewed file from remote snapshot')
input('[f')
wait(function() return file('src/a.rs') end, '[f included locally staged viewed file')
assert(not state().review.read.files[3].viewed and state().files[3].viewed and state().pending[1].change.viewed,
  'navigation altered projected or pending progress')
no_mark(before)

local function selected_comment(id)
  local root_id = ({ ['b-reply'] = 'b', ['c-reply'] = 'c' })[id] or id
  if not thread(root_id) then return false end
  local heading = api.nvim_win_get_cursor(0)[1]
  return api.nvim_buf_get_lines(0, heading + 1, heading + 2, false)[1] == 'Feedback ' .. id
end

local history_actions = actions_before()
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
      local role = meta.comment_role
      local source = role == 'original_base' and (historical and 'old-base' or revision.base)
        or (historical and 'old-head' or revision.head)
      assert(meta.path == path and vim.deep_equal(meta.revision, { base = source, head = revision.head }), 'comment navigation changed path/comparison incorrectly')
      local expected = { 'one', role == 'original_base' and (historical and 'historical base' or 'two')
        or (role == 'comment' and historical and 'historical head' or 'changed'), 'three' }
      assert(vim.deep_equal(api.nvim_buf_get_lines(buf, 0, -1, false), expected), 'comment navigation did not load pinned file content')
      local side = role == 'original_base' and (historical and 'original_base' or 'base')
        or (role == 'comment' and (historical and 'base' or 'head') or 'head')
      sides[side] = win
    end
  end
  assert(sides.base and sides.head, 'comment navigation did not open both diff sides')
  if head_line then assert(api.nvim_win_get_cursor(sides[historical and 'base' or 'head'])[1] == head_line, 'comment navigation jumped to wrong reference line') end
  return sides
end

-- Forward/backward checklist navigation follows each root's file even for historical anchors.
reset(cross_file); open('a'); diff_pair('src/a.rs', 1)
before = actions_before(); input(']r')
wait(function() return thread('c') end, 'next checklist comment did not open historical discussion')
diff_pair('src/e.rs', 3, true)
assert(text():find('Historical location', 1, true), 'historical comment lost its location label')
assert(requests('historical_code')[#requests('historical_code')].comment == 'c', 'historical discussion did not request original comparison')
reset(cross_file); open('e'); diff_pair('src/c.rs', 3)
before = actions_before(); input('[r')
wait(function() return thread('c') end, 'previous checklist comment did not open historical discussion')
diff_pair('src/e.rs', 3, true)

-- Enter and ordinary Todo navigation share the same file-following behavior.
reset(cross_file); open('c'); diff_pair('src/e.rs', 3, true)
command('next todo'); wait(function() return thread('e') end, 'ordinary next did not open next comment')
diff_pair('src/c.rs', 3)
command('previous todo'); wait(function() return thread('c') end, 'ordinary previous did not open historical comment')
diff_pair('src/e.rs', 3, true)

-- Replies follow the root file and original anchor, not their own nonexistent range.
reset(function(value)
  value.comments[1].comment.comment.target.target.path = 'src/a.rs'
  value.comments[1].comment.comment.target.target.range.start.line = 2
  value.comments[1].comment.comment.target.target.range['end'].line = 2
  value.comments[3].comment.comment.target = { kind = 'reply', target = 'a' }
end)
open('a'); diff_pair('src/a.rs', 2)
local code = #code_requests()
before = actions_before(); input(']r')
wait(function() return #code_requests() > code and thread('a') and api.nvim_win_get_cursor(0)[1] > 5 end, 'reply navigation lost its root discussion')
diff_pair('src/a.rs', 2)
before = actions_before(); input(']r')
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

-- Original paths absent from current files still open original code; PR-wide comments preserve existing diff.
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
  code = #code_requests()
  command('next todo'); wait(function() return thread('c') end, mode .. ' comment did not open')
  assert(#code_requests() == code + (mode == 'wide' and 0 or 1), mode .. ' comment requested unavailable or unrelated code')
  if mode == 'missing-historical' then diff_pair('removed.rs', 3, true)
  elseif mode == 'missing-current' then diff_pair('removed.rs', 3)
  else diff_pair('src/a.rs', 1) end
end

no_mark(history_actions, 'historical/reply navigation staged progress')

-- A picker row supplies current identity; code enters local unfinished work.
reset(); open_file_at('src/c.rs')
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
assert(kind() == 'threads', 'same-line thread list missing')
navigation_maps(); row('Feedback c')
before = actions_before(); input(']r')
wait(function() return thread('e') end, 'picker reopened highlighted comment')
no_mark(before)
for _, side in ipairs({ 'base', 'head' }) do
  reset(function(value)
    for index = 3, 4 do
      local range = value.comments[index].comment.comment.target.target.range
      range.start.side, range['end'].side = side, side
    end
  end)
  open('e'); file_focus('src/c.rs', side); api.nvim_win_set_cursor(0, { 2, 0 })
  before = actions_before(); input(']r')
  wait(function() return thread('c') end, side .. ' code ignored local unfinished comment')
  input(']r')
  wait(function() return thread('e') end, 'focused comment did not advance')
  no_mark(before, 'comment navigation marked visible or focused discussion')
end

local function multiple_at_location(value)
  local done_root = comment('b', 2, true)
  local first_reply, second_reply = comment('b-reply', 2), comment('c-reply', 2)
  first_reply.comment.comment.target = { kind = 'reply', target = 'b' }
  second_reply.comment.comment.target = { kind = 'reply', target = 'c' }
  value.comments = { comment('a', 1), done_root, first_reply, comment('c', 2), second_reply, comment('d', 2), comment('e', 3) }
  value.todo, value.done = { 'a', 'b-reply', 'c', 'c-reply', 'd', 'e' }, { 'b' }
end
for _, mode in ipairs({ 'code', 'picker' }) do
  for _, direction in ipairs({ 1, -1 }) do
    reset(multiple_at_location); open(direction > 0 and 'e' or 'a')
    local head_window = file_focus('src/c.rs')
    api.nvim_win_set_cursor(head_window, { 2, 0 }); input('<CR>'); row('Feedback c')
    if mode == 'code' then api.nvim_set_current_win(head_window) end
    local key = direction > 0 and ']r' or '[r'
    local order = mode == 'code'
      and (direction > 0 and { 'b-reply', 'c', 'c-reply', 'd', 'e', 'a' } or { 'd', 'c-reply', 'c', 'b-reply', 'a', 'e' })
      or (direction > 0 and { 'c-reply', 'd', 'e', 'a', 'b-reply', 'c' } or { 'b-reply', 'a', 'e', 'd', 'c-reply', 'c' })
    before = actions_before()
    for _, id in ipairs(order) do
      input(key)
      wait(function() return selected_comment(id) end, mode .. ' ' .. key .. ' skipped/repeated ' .. id)
    end
    input(key)
    wait(function() return selected_comment(order[1]) end, mode .. ' traversal failed to wrap')
    no_mark(before, 'same-line traversal staged progress')
  end
end

-- Unmapped thread headers use buffer root, independent of previously focused reply.
for _, previous in ipairs({ 'c', 'c-reply' }) do
  reset(function(value)
    local reply = comment('c-reply', 2)
    reply.comment.comment.target = { kind = 'reply', target = 'c' }
    table.insert(value.comments, 4, reply)
  end)
  open('c'); row('Feedback ' .. previous)
  api.nvim_exec_autocmds('CursorMoved', { buffer = api.nvim_get_current_buf() })
  api.nvim_win_set_cursor(0, { 1, 0 })
  api.nvim_exec_autocmds('CursorMoved', { buffer = api.nvim_get_current_buf() })
  before = actions_before(); input(']r')
  wait(function() return selected_comment('c-reply') end, 'blank header reused previous reply identity')
  no_mark(before)
end

-- Staging and navigation are separate: s preserves focused buffer/cursor.
for _, target in ipairs({ 'comment', 'base', 'head' }) do
  reset(); open('c')
  if target ~= 'comment' then file_focus('src/c.rs', target); api.nvim_win_set_cursor(0, { 2, 0 }) end
  local buffer, window, cursor, code = api.nvim_get_current_buf(), api.nvim_get_current_win(), api.nvim_win_get_cursor(0), #code_requests()
  before = actions_before(); input('s')
  marked(before, target == 'comment' and 'reaction' or 'viewed', target == 'comment' and 'c' or 'src/c.rs')
  assert(api.nvim_get_current_buf() == buffer and api.nvim_get_current_win() == window
    and vim.deep_equal(api.nvim_win_get_cursor(0), cursor) and #code_requests() == code, 's navigated or moved cursor')
  assert(target == 'comment' and state().comments[3].done and not state().files[3].viewed
    or target ~= 'comment' and state().files[3].viewed and not state().comments[3].done,
    's staged background pane instead of focused context')
  local staged = actions_before(); input(target == 'comment' and ']r' or ']f')
  wait(function() return target == 'comment' and thread('e') or target ~= 'comment' and file('src/e.rs') end, 'next failed after staging')
  no_mark(staged, 'navigation sent second progress write')
end
reset(function(value)
  local reply = comment('c-reply', 2)
  reply.comment.comment.target = { kind = 'reply', target = 'c' }
  table.insert(value.comments, 4, reply)
end)
open('c'); row('Feedback c-reply')
local cursor = api.nvim_win_get_cursor(0)
before = actions_before(); input('s'); marked(before, 'reaction', 'c-reply')
assert(thread('c') and vim.deep_equal(api.nvim_win_get_cursor(0), cursor)
  and not state().comments[3].done and state().comments[4].done, 's staged root or moved from focused reply')

-- Unrelated focused readers cannot stage visible background files/comments.
reset(); open('c'); command('log'); wait(function() return kind() == 'log' end, 'log missing')
navigation_maps()
before = actions_before(); input('s')
assert(kind() == 'log', 's navigated from unrelated reader')
no_mark(before, 's staged remembered background context')

-- Immediate navigation settles an in-flight s write, then reads updated progress.
for _, target in ipairs({ 'comment', 'file' }) do
  reset()
  if target == 'comment' then open('c') else open_file_at('src/c.rs') end
  vim.fn.writefile({ '0.15' }, fixture .. '/delay_actions')
  before = actions_before(); input(target == 'comment' and 's]r' or 's]f')
  wait(function() return target == 'comment' and thread('e') or target == 'file' and file('src/e.rs') end, 'navigation lost pending stage writer')
  marked(before, target == 'comment' and 'reaction' or 'viewed', target == 'comment' and 'c' or 'src/c.rs')
  assert(target == 'comment' and state().comments[3].done or target == 'file' and state().files[3].viewed,
    'navigation lost staged progress')
  vim.fn.delete(fixture .. '/delay_actions')
end

-- Unsaved composers block navigation without staging or discarding text.
reset(); row('Feedback c'); command('reply')
assert(kind() == 'composer', 'reply composer missing')
navigation_maps()
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved reply' })
local current_buf, code = api.nvim_get_current_buf(), #code_requests()
before = actions_before()
for _, key in ipairs({ ']r', '[r', ']f', '[f' }) do
  input(key)
  assert(api.nvim_get_current_buf() == current_buf and api.nvim_get_current_line() == 'Unsaved reply'
    and vim.bo.modified and #code_requests() == code, key .. ' discarded unsaved composer')
  no_mark(before)
end
input(':q!<CR>')
vim.cmd('new')
for _, key in ipairs({ ']r', '[r', ']f', '[f', ']F', '[F', ']R', '[R' }) do
  assert(vim.fn.maparg(key, 'n') == '', 'review navigation leaked into source')
end

print('Neovim navigation and staging workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
