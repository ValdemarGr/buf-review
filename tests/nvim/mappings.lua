local api = vim.api
local repository, mode = vim.fn.getcwd(), vim.env.BUF_REVIEW_MAPPING_CASE
if not mode then
  for _, name in ipairs({ 'actions', 'preservation' }) do
    local result = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c',
      ('lua assert(loadfile(%q))(%q)'):format(repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/mappings.lua') },
      { env = { BUF_REVIEW_MAPPING_CASE = name }, text = true }):wait()
    assert(result.code == 0, name .. ': ' .. result.stderr)
  end
  print('Neovim default mapping workflows passed')
  vim.cmd('qa!')
  return
end

vim.g.mapleader = ' '
local fixture, hits = vim.fn.tempname(), {}
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
if mode == 'preservation' then
  for _, key in ipairs({ ' rs', ']r', '[f', '[R', ']c' }) do
    vim.keymap.set('n', key, function() hits[key] = (hits[key] or 0) + 1 end)
  end
  api.nvim_create_autocmd('FileType', { pattern = 'review', callback = function(event)
    for _, key in ipairs({ ']f', ']F' }) do
      vim.keymap.set('n', key, function() hits[key] = (hits[key] or 0) + 1 end, { buffer = event.buf })
    end
  end })
end
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')) end
local function records(name)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    result[#result + 1] = vim.json.decode(line)
  end
  return result
end
local function requests(op) return vim.tbl_filter(function(value) return value.op == op end, records('requests.jsonl')) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
end
local function input(keys) api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false) end
local function command(value) vim.cmd('Review ' .. value) end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, path, done)
  return { comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id,
    author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = path, revision = revision,
      range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } } } } } },
    fetched = true, done = done or false, pending_delete = false, conflict = false, summary = false }
end
local initial = {
  id = 'mappings', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'mapping-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Mappings', revision = revision,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('a', 'src/a.rs'), comment('b', 'src/a.rs', true), comment('c', 'src/c.rs') },
  files = { { path = 'src/a.rs', viewed = false }, { path = 'src/b.rs', viewed = true }, { path = 'src/c.rs', viewed = false } },
  todo = { 'a', 'c' }, done = { 'b' }, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
local origin = api.nvim_get_current_buf()
if mode == 'actions' then input(' rs') else command('') end
wait(function() return kind() == 'home' and text():find('Code: ready', 1, true) end, 'Review leader did not open review')
local home_epoch = 0
local function home()
  if kind() == 'home' then return end
  home_epoch = home_epoch + 1
  local refreshed = state()
  local title = 'Mappings refresh ' .. home_epoch
  refreshed.pr.title = title
  write('state.json', refreshed)
  command('')
  wait(function() return kind() == 'home' and text():find(title, 1, true) end, 'Overview refresh was not rendered')
end
local function open(id)
  home()
  row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == id end, 'Comment missing ' .. id)
end
local function code(side)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' and (meta.comment_role == 'comment' and not side
      or not meta.comment_role and meta.side == (side or 'head')) then api.nvim_set_current_win(win); return end
  end
  error('Missing code pane')
end
local function file(path) return kind() == 'diff' and vim.b.buf_review.path == path end
if mode == 'preservation' then
  local before = state().generation
  for _, key in ipairs({ ' rs', ']r', '[f', '[R', ']c', ']f', ']F' }) do
    input(key)
    assert(hits[key] == 1, 'Review replaced existing exact global or buffer mapping: ' .. key)
  end
  assert(kind() == 'home' and state().generation == before and #requests('actions') == 0, 'Preserved user mapping invoked review actions')
  vim.cmd('new')
  for _, key in ipairs({ ']R', ']F', '[F', '[r' }) do
    assert(vim.fn.maparg(key, 'n') == '', 'Review navigation leaked into ordinary buffer: ' .. key)
  end
  input('[R')
  assert(hits['[R'] == 2, 'Ordinary source lost preserved global user mapping')
else
  for _, key in ipairs({ ']C', '[C', ']c', '[c', ']R', '[R', ']F', '[F',
    '<Plug>(ReviewViewedNext)', '<Plug>(ReviewViewedPrevious)', '<Plug>(ReviewDoneNext)', '<Plug>(ReviewDonePrevious)' }) do
    assert(vim.fn.maparg(key, 'n') == '', 'Unexpected mapping ' .. key)
  end
  assert(require('buf_review').mark_move == nil, 'Removed combined navigation API remains available')
  for _, key in ipairs({ ']r', '[r', ']f', '[f' }) do
    assert(vim.fn.maparg(key, 'v') == '' and vim.fn.maparg(key, 'i') == '', 'Navigation mapped outside Normal mode: ' .. key)
  end
  open('a')
  local before = state().generation
  for _, step in ipairs({ { ']r', 'c' }, { ']r', 'a' }, { '[r', 'c' }, { '[r', 'a' } }) do
    input(step[1])
    wait(function() return kind() == 'thread' and vim.b.buf_review.object == step[2] end, 'Pure comment navigation skipped ' .. step[2])
  end
  code()
  for _, step in ipairs({ { ']f', 'src/c.rs' }, { ']f', 'src/a.rs' }, { '[f', 'src/c.rs' }, { '[f', 'src/a.rs' } }) do
    input(step[1]); wait(function() return file(step[2]) end, 'Pure file navigation skipped ' .. step[2])
  end
  assert(state().generation == before and #requests('actions') == 0, 'Pure bracket navigation wrote progress')
  assert(not state().comments[1].done and state().comments[2].done and not state().files[1].viewed, 'Pure navigation changed progress')
  api.nvim_win_set_cursor(0, { 1, 0 }); input(']c')
  assert(api.nvim_win_get_cursor(0)[1] == 2, 'Native diff hunk navigation stopped working')
  input('[c')
  assert(#requests('actions') == 0, 'Native hunk navigation invoked review actions')

  open('a')
  local current_buffer, current_view = api.nvim_get_current_buf(), vim.fn.winsaveview()
  input('s')
  wait(function() return state().comments[1].done and #requests('actions') == 1 end, 'Comment staging did not save progress')
  assert(api.nvim_get_current_buf() == current_buffer and vim.deep_equal(vim.fn.winsaveview(), current_view), 'Comment staging moved cursor or viewport')
  input(']r')
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'c' end, 'Comment navigation did not skip staged and Done comments')
  assert(#requests('actions') == 1, 'Comment navigation staged progress')
  input('s')
  wait(function() return state().comments[3].done and #requests('actions') == 2 end, 'Second comment staging did not save progress')
  input('[r')
  assert(kind() == 'thread' and vim.b.buf_review.object == 'c' and #requests('actions') == 2, 'Navigation moved or wrote progress with no remaining Todo')
  home()
  row('src/a.rs'); require('buf_review').enter(); wait(function() return file('src/a.rs') end, 'First file missing')
  current_buffer, current_view = api.nvim_get_current_buf(), vim.fn.winsaveview()
  input('s')
  wait(function() return state().files[1].viewed and #requests('actions') == 3 end, 'File staging did not save progress')
  assert(api.nvim_get_current_buf() == current_buffer and vim.deep_equal(vim.fn.winsaveview(), current_view), 'File staging moved cursor or viewport')
  input(']f')
  wait(function() return file('src/c.rs') end, 'File navigation did not skip staged and viewed files')
  assert(#requests('actions') == 3, 'File navigation staged progress')
  input('s')
  wait(function() return state().files[3].viewed and #requests('actions') == 4 end, 'Second file staging did not save progress')
  input('[f')
  assert(file('src/c.rs') and #requests('actions') == 4, 'Navigation moved or wrote progress with no unviewed files')

  -- Leaders remain available from ordinary buffers and route existing commands.
  home()
  local home = api.nvim_get_current_buf()
  api.nvim_set_current_buf(origin)
  before = #requests('fetch_refresh'); input(' rr')
  wait(function() return #requests('fetch_refresh') == before + 1 and #records('refresh_complete.jsonl') == before + 1 end, 'Refresh leader did not fetch')
  input(' rp')
  assert(#requests('publish') == 1 and #requests('fetch_refresh') == before + 2, 'Publish leader did not publish and refresh')
  vim.ui.select = function(_, _, callback) callback('comment') end
  input(' rcc')
  wait(function() return kind() == 'composer' and require('buf_review.editor').get().kind == 'conclusion' end, 'Conclusion leader did not open composer')
  input(':q!<CR>')
  api.nvim_set_current_buf(home)
  row('src/a.rs'); require('buf_review').enter(); wait(function() return file('src/a.rs') end, 'Suggestion source missing')
  code(); api.nvim_win_set_cursor(0, { 2, 0 }); input('a')
  assert(kind() == 'composer', 'Inline composer missing')
  input(' rcs')
  assert(text():find('```suggestion\nchanged\n```', 1, true), 'Suggestion leader did not use current composer anchor')
  input(':q!<CR>')

  -- Real input must not wait for a longer global Visual mapping to time out.
  code()
  local timeoutlen, previous_af = vim.o.timeoutlen, vim.fn.maparg('af', 'x', false, true)
  vim.o.timeoutlen = 2000
  vim.keymap.set('x', 'af', function() hits.af = (hits.af or 0) + 1 end)
  local before_visual = #records('requests.jsonl')
  local started = vim.uv.hrtime()
  api.nvim_input('ggVja')
  local opened = ReviewTest.wait(500, function() return kind() == 'composer' end, 10)
  assert(opened and (vim.uv.hrtime() - started) / 1e6 < 500, 'Visual a waited for longer global mapping timeout')
  local editor = require('buf_review.editor').get()
  local target = editor.target
  assert(target.kind == 'diff' and target.target.range.start.line == 1 and target.target.range['end'].line == 2
    and target.target.range.start.side == 'head' and target.target.range['end'].side == 'head', 'Immediate Visual a lost selected range')
  assert(not hits.af and #records('requests.jsonl') == before_visual, 'Visual composer invoked longer mapping or backend request')
  vim.o.timeoutlen = timeoutlen
  vim.keymap.del('x', 'af')
  if next(previous_af) then vim.fn.mapset('x', false, previous_af) end
  input(':q!<CR>')
end
print('Mapping ' .. mode .. ' passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
