local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
local git = vim.system({ 'git', 'init', '--quiet', fixture }, { text = true }):wait()
assert(git.code == 0, git.stderr)
vim.o.shadafile = 'NONE'
local files = {}
for _, name in ipairs({ 'first', 'second', 'current' }) do
  local path, lines = fixture .. '/' .. name .. '.txt', {}
  for line = 1, 40 do lines[line] = name .. ' line ' .. line end
  vim.fn.writefile(lines, path)
  files[#files + 1] = path
end
vim.cmd.edit(files[1])
vim.cmd('clearjumps')
api.nvim_win_set_cursor(0, { 3, 0 })
vim.cmd.edit(files[2]); api.nvim_win_set_cursor(0, { 7, 0 })
vim.cmd.edit(files[3]); api.nvim_win_set_cursor(0, { 11, 0 })
local history = vim.fn.getjumplist()
assert(#history[1] == 2 and history[1][1].lnum == 3 and history[1][2].lnum == 7, 'User jump history was not seeded')

vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
end
local function command(value) vim.cmd('Review ' .. value) end
local function input(keys) api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false) end
local function row(needle, exact)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if exact and (vim.trim(value) == needle or value:sub(-#needle - 2) == '  ' .. needle)
      or not exact and value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle .. '\n' .. text())
end
local function unchanged(reason)
  for _, win in ipairs(api.nvim_list_wins()) do
    local actual = api.nvim_win_call(win, function() return vim.fn.getjumplist() end)
    assert(vim.deep_equal(actual, history), reason .. ' changed jump history in window ' .. win .. '\nExpected: '
      .. vim.inspect(history) .. '\nActual: ' .. vim.inspect(actual))
  end
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local old = { base = 'old-base', head = 'old-head' }
local function comment(id, path, pair)
  return { comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id,
    author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = { path = path, revision = pair or revision,
      range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } } } } } },
    fetched = true, done = false, conflict = false, summary = false }
end
local view = {
  id = 'jumplist', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'jumplist-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Jump history', revision = revision,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('a', 'src/a.rs'), comment('b', 'src/a.rs'), comment('c', 'src/c.rs'), comment('historical', 'src/a.rs', old), comment('missing', 'src/a.rs', old) },
  files = { { path = 'src/a.rs', viewed = false }, { path = 'src/c.rs', viewed = false } },
  todo = { 'a', 'b', 'c', 'historical', 'missing' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local route = { route = 'batch', batch = view.id, fresh_review = view.review.id }
write('state.json', view)
write('routes.json', { entry = route, [view.locator] = route })
write('historical_contexts.json', { missing = { code = vim.NIL, diff_hunk = '@@ -1 +1 @@\n-before\n+after', message = 'Recorded hunk only' } })
write('contexts.json', { historical = { available = true, anchor = view.comments[4].comment.comment.target.target,
  base_content = 'one\nbefore\nthree', head_content = 'one\nafter\nthree' } })
command('')
wait(function() return kind() == 'home' and text():find('Code: ready', 1, true) end, 'Initial overview missing')
unchanged('Opening Review')
local home_epoch = 0
local function home()
  if kind() == 'home' then return end
  home_epoch = home_epoch + 1
  local refreshed = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n'))
  local title = 'Jump history refresh ' .. home_epoch
  refreshed.pr.title = title
  write('state.json', refreshed)
  command(''); wait(function() return kind() == 'home' and text():find(title, 1, true) end, 'Overview refresh missing')
end
local function open(id)
  home(); row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == id end, 'Thread missing ' .. id)
end

-- Browsers and code readers reuse windows without becoming user jump targets.
command('prs'); wait(function() return kind() == 'prs' end, 'PR browser missing'); unchanged('PR browser')
row('#1 Fixture review'); require('buf_review').enter()
wait(function() return kind() == 'home' end, 'Selected review missing'); unchanged('Browser selection')
row('src/a.rs', true); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'First file missing'); unchanged('Opening diff')
command('next file')
wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'src/c.rs' end, 'Next file missing'); unchanged('Next file')
command('previous file')
wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'src/a.rs' end, 'Previous file missing'); unchanged('Previous file')
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
wait(function() return kind() == 'threads' end, 'Same-line thread list missing'); unchanged('Thread list')
row('Feedback a'); require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'Selected thread missing'); unchanged('Thread reader')

-- Composer replacement and native return preserve the history of that window.
row('Feedback a'); command('reply')
assert(kind() == 'composer', 'Reply composer missing'); unchanged('Opening composer')
input(':q!<CR>')
assert(kind() == 'thread', 'Composer did not return to thread'); unchanged('Closing composer')
command('next comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'b' end, 'Next comment missing'); unchanged('Next comment')
command('log'); wait(function() return kind() == 'log' end, 'Log missing'); unchanged('Log reader')
home(); unchanged('Returning home')

-- Historical source pairs, retained hunks and context tabs obey the same rule.
open('historical'); unchanged('Historical pair')
command('context')
wait(function() return kind() == 'context' end, 'Original source context missing'); unchanged('Context source pair')
local context_tab = api.nvim_get_current_tabpage()
write('contexts.json', { historical = { available = true, anchor = view.comments[4].comment.comment.target.target,
  base_content = 'Refreshed original source', head_content = 'Refreshed context head' } })
command('context')
wait(function() return kind() == 'context' and api.nvim_get_current_tabpage() == context_tab and text() == 'Refreshed context head' end, 'Context reuse missing')
unchanged('Reusing context pair')
home(); open('missing'); unchanged('Historical hunk fallback')
command('context')
wait(function() return kind() == 'context' and text():find('Recorded hunk only', 1, true) end, 'Recorded context missing')
unchanged('Recorded context fallback')

-- The existing native destination remains useful after the entire Review flow.
input('<C-o>')
assert(api.nvim_buf_get_name(0) == files[2] and api.nvim_win_get_cursor(0)[1] == 7, 'Ctrl-O did not return to prior user jump')
vim.cmd('normal! G')
input('<C-o>')
assert(api.nvim_buf_get_name(0) == files[2] and api.nvim_win_get_cursor(0)[1] == 7, 'Native user jumps stopped recording after Review navigation')
print('Neovim review jumplist preservation passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
