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
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')) end
local function requests(op)
  local values = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}) do
    local value = vim.json.decode(line)
    if value.op == op then values[#values + 1] = value end
  end
  return values
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(value) vim.cmd('Review ' .. value) end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function input(value)
  api.nvim_feedkeys(api.nvim_replace_termcodes(value, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if vim.trim(value) == needle or value:sub(-#needle - 2) == '  ' .. needle then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id)
  return { comment = { origin = 'read', comment = { id = id, author = { id = 'alice', name = 'alice' }, body = 'Feedback ' .. id,
    target = { kind = 'diff', target = { path = 'src/' .. id .. '.rs', revision = revision,
      range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } } } },
    fetched = true, done = false, conflict = false, summary = false }
end
local wide = comment('wide')
wide.comment.comment.target = { kind = 'pull_request' }
local initial = {
  id = 'navigation', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'navigation-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Navigation', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('a'), comment('c'), wide, comment('e') },
  files = { { path = 'src/a.rs', viewed = false }, { path = 'src/b.rs', viewed = false }, { path = 'src/c.rs', viewed = false }, { path = 'src/e.rs', viewed = false } },
  todo = { 'a', 'c', 'wide', 'e' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
command(''); wait(function() return kind() == 'home' end, 'initial home missing')
local home_buffer = api.nvim_get_current_buf()
local function reset()
  for _, name in ipairs({ 'delay_code', 'delay_historical_code', 'delay_actions', 'delay_context', 'delay_log' }) do vim.fn.delete(fixture .. '/' .. name) end
  local value = vim.deepcopy(initial)
  value.generation = state().generation + 1
  value.review.read.pull_request.title = "Navigation reset " .. value.generation
  write('state.json', value)
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find(value.review.read.pull_request.title, 1, true) end, 'home reset did not complete')
  home_buffer = api.nvim_get_current_buf()
end
local function open(id)
  row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == id end, 'comment missing ' .. id)
  return api.nvim_get_current_buf(), api.nvim_get_current_win()
end
local function focus_code()
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' and (meta.comment_role == 'comment' or not meta.comment_role and meta.side == 'head') then
      api.nvim_set_current_win(win); return win
    end
  end
  error('Missing head pane')
end
local function focus_thread(id)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'thread' and meta.object == id then api.nvim_set_current_win(win); return win end
  end
  error('Missing discussion ' .. id)
end
local function current_file(path) return kind() == 'diff' and vim.b.buf_review.path == path end
local function visible_thread(id)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'thread' and (not id or meta.object == id) then return true end
  end
  return false
end
local function code_since(before)
  local paths, records = {}, requests('code')
  for index = before + 1, #records do paths[#paths + 1] = records[index].path end
  return paths
end

-- Staging stays put; pure file changes retire the managed old discussion.
open('a'); focus_code()
local before_actions = #requests('actions')
input('s')
wait(function() return state().files[1].viewed end, 'file progress was not staged')
assert(current_file('src/a.rs') and visible_thread('a'), 'staging navigated away from current file')
input(']f')
wait(function() return current_file('src/b.rs') end, 'file navigation did not advance')
assert(not visible_thread(), 'file change retained managed discussion from previous file')
assert(state().files[1].viewed and not state().comments[1].done, 'file mark changed comment progress')
before_actions = #requests('actions')
input(']r')
wait(function() return kind() == 'thread' end, 'comment navigation without open discussion failed')
assert(#requests('actions') == before_actions, 'closed old discussion was marked in background')

-- Reopening the same file preserves its existing managed discussion.
reset(); local same_reader = open('a')
focus_code(); vim.cmd('split')
local file_picker = api.nvim_get_current_win()
vim.cmd('diffoff')
api.nvim_win_set_buf(file_picker, home_buffer)
row('src/a.rs'); require('buf_review').enter()
wait(function() return current_file('src/a.rs') end, 'same file did not reopen')
assert(#vim.fn.win_findbuf(same_reader) == 1, 'same-file navigation closed its discussion')
api.nvim_win_close(file_picker, true)

-- User-visible extra readers remain available when managed discussion closes.
reset(); local retained_reader = open('a')
vim.cmd('split')
local retained_window = api.nvim_get_current_win()
focus_code(); input(']f')
wait(function() return current_file('src/b.rs') end, 'file change with extra reader failed')
assert(api.nvim_win_is_valid(retained_window) and api.nvim_win_get_buf(retained_window) == retained_reader
  and #vim.fn.win_findbuf(retained_reader) == 1, 'file change removed manually retained reader')
api.nvim_win_close(retained_window, true)

-- A newer PR-wide selection wins over an older delayed code/comment response.
reset(); open('a')
vim.fn.writefile({ '0.25' }, fixture .. '/delay_historical_code')
local before_comments = #requests('historical_code')
command('next todo')
wait(function() return #requests('historical_code') == before_comments + 1 end, 'delayed comment read missing')
api.nvim_set_current_buf(home_buffer)
row('Feedback wide'); require('buf_review').enter()
assert(kind() == 'thread' and vim.b.buf_review.object == 'wide', 'new PR-wide choice missing')
ReviewTest.wait(400)
assert(kind() == 'thread' and vim.b.buf_review.object == 'wide', 'older code callback replaced newer PR-wide choice')
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
  if meta and meta.kind == 'diff' then assert(meta.path == 'src/a.rs', 'stale callback changed code after PR-wide choice') end
end
vim.fn.delete(fixture .. '/delay_historical_code')
command('next todo')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'e' end, 'new selected comment and navigation order diverged')

-- Returning home cancels delayed code and comment installation too.
reset(); open('a')
vim.fn.writefile({ '0.25' }, fixture .. '/delay_historical_code')
before_comments = #requests('historical_code')
command('next todo')
wait(function() return #requests('historical_code') == before_comments + 1 end, 'home-race delayed read missing')
command(''); wait(function() return kind() == 'home' end, 'newer home choice missing')
ReviewTest.wait(400)
assert(kind() == 'home' and #api.nvim_tabpage_list_wins(0) == 1, 'old callback reopened code after home')

-- Explicit file navigation settles pending comment progress without competing requests.
reset(); open('a')
vim.fn.writefile({ '0.12' }, fixture .. '/delay_actions')
vim.fn.writefile({ '0.35' }, fixture .. '/delay_code')
local before_code
before_actions, before_code = #requests('actions'), #requests('code')
input('s')
wait(function() return #requests('actions') == before_actions + 1 end, 'delayed comment mark missing')
focus_code()
command('next file')
wait(function() return #requests('code') == before_code + 1 end, 'new file intent missing')
wait(function() return current_file('src/b.rs') end, 'file navigation did not settle comment progress')
ReviewTest.wait(200)
assert(state().comments[1].done and vim.deep_equal(code_since(before_code), { 'src/b.rs' }), 'staging issued competing navigation')
assert(not visible_thread(), 'new file retained old discussion after delayed comment mark')

-- Explicit comment navigation also settles pending file progress.
reset(); open('a'); focus_code()
vim.fn.writefile({ '0.12' }, fixture .. '/delay_actions')
vim.fn.writefile({ '0.35' }, fixture .. '/delay_historical_code')
before_actions, before_code = #requests('actions'), #requests('code')
before_comments = #requests('historical_code')
input('s')
wait(function() return #requests('actions') == before_actions + 1 end, 'delayed file mark missing')
focus_thread('a')
command('next todo')
wait(function() return #requests('historical_code') == before_comments + 1 end, 'new comment intent missing')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'c' end, 'pending file mark overrode newer comment choice')
ReviewTest.wait(200)
assert(state().files[1].viewed and #requests('code') == before_code
  and #requests('historical_code') == before_comments + 1
  and requests('historical_code')[before_comments + 1].comment == 'c', 'file staging issued competing navigation')

-- PR-wide and log discussions are independent of the selected code file.
reset(); open('a')
command('next todo'); wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'c' end, 'global discussion setup missing')
command('next todo'); assert(kind() == 'thread' and vim.b.buf_review.object == 'wide', 'PR-wide discussion missing')
local wide_reader = api.nvim_get_current_buf()
focus_code(); input(']f')
wait(function() return current_file('src/e.rs') end, 'file navigation beside PR-wide comment failed')
assert(#vim.fn.win_findbuf(wide_reader) == 1, 'file change closed PR-wide discussion')
reset(); open('a'); focus_code()
command('log'); wait(function() return kind() == 'log' end, 'log setup missing')
local log_reader = api.nvim_get_current_buf()
focus_code()
input(']f'); wait(function() return current_file('src/b.rs') end, 'file navigation beside log failed')
assert(#vim.fn.win_findbuf(log_reader) == 1, 'file change closed independent log view')

-- An older local reader cannot replace a newer comment or reopen a context tab after home.
reset(); open('a')
vim.fn.writefile({ '0.25' }, fixture .. '/delay_log')
local before_log = #requests('log')
command('log'); wait(function() return #requests('log') == before_log + 1 end, 'delayed log request missing')
command('next todo')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'c' end, 'new comment after log request missing')
ReviewTest.wait(400)
assert(kind() == 'thread' and vim.b.buf_review.object == 'c', 'older log callback replaced newer comment')
reset(); open('a')
vim.fn.writefile({ '0.25' }, fixture .. '/delay_context')
local before_context, before_tabs = #requests('context'), #api.nvim_list_tabpages()
command('context'); wait(function() return #requests('context') == before_context + 1 end, 'delayed context request missing')
command(''); wait(function() return kind() == 'home' end, 'home after context request missing')
ReviewTest.wait(400)
assert(kind() == 'home' and #api.nvim_list_tabpages() == before_tabs, 'older context callback reopened original view after home')

-- Changed-file cleanup cannot discard a modified composer.
reset(); open('a'); focus_code(); input('a')
assert(kind() == 'composer', 'guard composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved comment' })
local composer = api.nvim_get_current_buf()
before_actions, before_code = #requests('actions'), #requests('code')
input(']f'); command('next file')
assert(api.nvim_get_current_buf() == composer and vim.bo.modified and api.nvim_get_current_line() == 'Unsaved comment'
  and #requests('actions') == before_actions and #requests('code') == before_code, 'file navigation discarded or bypassed unsaved composer')
input(':q!<CR>')

-- Staging in a retained reader stays bound to its session during explicit navigation.
reset(); local old_reader = open('a')
local another = vim.deepcopy(initial)
another.id, another.generation = 'navigation-other', 0
local views = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/views.json'), '\n'))
views[another.id] = another
write('views.json', views)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id },
  [initial.locator] = { route = 'batch', batch = another.id, fresh_review = another.review.id } })
command('prs'); wait(function() return kind() == 'prs' end, 'mixed-session PR browser missing')
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:find('#1 Fixture review', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == another.id end, 'newer session missing')
row('src/a.rs'); require('buf_review').enter()
wait(function() return current_file('src/a.rs') and vim.b.buf_review.workspace == another.id end, 'newer session code missing')
local origin_window = api.nvim_get_current_win()
vim.cmd('split'); api.nvim_win_set_buf(0, old_reader)
local extra_window = api.nvim_get_current_win()
vim.fn.writefile({ '0.12' }, fixture .. '/delay_actions')
vim.fn.writefile({ '0.35' }, fixture .. '/delay_code')
before_actions, before_code = #requests('actions'), #requests('code')
input('s')
wait(function() return #requests('actions') == before_actions + 1 end, 'old-session mark missing')
assert(requests('actions')[before_actions + 1].workspace == initial.id, 'old reader marked wrong bound session')
api.nvim_set_current_win(origin_window)
command('next file')
wait(function() return current_file('src/b.rs') and vim.b.buf_review.workspace == another.id end, 'old-bound staging replaced newer session navigation')
ReviewTest.wait(200)
views = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/views.json'), '\n'))
assert(views[initial.id].comments[1].done and not state().comments[1].done, 'mixed-session progress was lost or saved in wrong session')
assert(vim.deep_equal(code_since(before_code), { 'src/b.rs' }), 'old-bound staging issued competing code request')
assert(api.nvim_win_is_valid(extra_window) and api.nvim_win_get_buf(extra_window) == old_reader, 'newer navigation closed manually retained old reader')
api.nvim_win_close(extra_window, true)

print('Neovim latest-navigation workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
