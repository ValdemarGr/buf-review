-- Exercise command composition through the real plugin, not editor descriptors.
local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local initial = {
  id = 'append-native-session', generation = 0,
  locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'append-native-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Native append review', revision = revision },
    files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = {}, files = { { path = 'src/file.rs', viewed = false } },
  todo = {}, done = {}, pending = {}, conflicts = {}, new_commits = false,
}
local remote = {
  comment = { origin = 'read', comment = {
    id = 'remote-root', body = 'Remote root', author = { id = 'me', name = 'me' },
    target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision,
      range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } },
    } },
  } },
  fetched = true, done = false, conflict = false, summary = false,
}
local remote_child = vim.deepcopy(remote)
remote_child.comment.comment.id, remote_child.comment.comment.body = 'remote-child', 'Remote child'
remote_child.comment.comment.author = { id = 'alice', name = 'alice' }
remote_child.comment.comment.target = { kind = 'reply', target = 'remote-root' }
initial.comments, initial.todo = { remote, remote_child }, { 'remote-root', 'remote-child' }
vim.fn.writefile({ vim.json.encode(initial) }, fixture .. '/state.json')
local function state()
  return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n'), { luanil = { object = true, array = true } })
end
local function requests(op)
  local result = {}
  for _, filename in ipairs({ 'requests.jsonl', 'action_attempts.jsonl' }) do
    for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. filename) == 1 and vim.fn.readfile(fixture .. '/' .. filename) or {}) do
      local value = vim.json.decode(line)
      if value.op == op then result[#result + 1] = value end
    end
  end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(40)
end
local function keys(value) return api.nvim_replace_termcodes(value, true, false, true) end
local function normal(value) vim.cmd.normal({ keys(value), bang = false }) end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local function matches(body)
  local result = {}
  for _, item in ipairs(state().comments) do
    if item.comment.comment.body == body then result[#result + 1] = item.comment.comment end
  end
  return result
end

vim.cmd('Review')
wait(function() return kind() == 'home' end, 'home missing')
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:match('%s%ssrc/file%.rs$') then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
normal('<CR>')
wait(function() return kind() == 'diff' end, 'comparison missing')
local diff, diff_win = api.nvim_get_current_buf(), api.nvim_get_current_win()
local source = api.nvim_buf_get_lines(diff, 0, -1, false)
assert(vim.b.buf_review.side == 'head' and vim.bo[diff].readonly and not vim.bo[diff].modifiable, 'fixture did not open immutable head')

-- Vim selects lines 1 and 3. Every append must restore its diff before the next
-- match, even though saving pumps callbacks and comments change that same view.
vim.cmd([[global/^one$\|^three$/normal ]] .. keys('aNative root<Esc>:wq<CR>'))
assert(api.nvim_get_current_buf() == diff and api.nvim_get_current_win() == diff_win, 'global append failed to restore original diff before returning: ' .. vim.inspect(notices) .. '\n' .. text())
local roots = matches('Native root')
assert(#roots == 2, 'global append lost matched source lines: ' .. vim.inspect(notices))
for index, line in ipairs({ 1, 3 }) do
  local target = roots[index].target
  assert(target.kind == 'diff' and target.target.path == 'src/file.rs' and vim.deep_equal(target.target.revision, revision), 'global append changed target identity')
  assert(target.target.range.start.line == line and target.target.range['end'].line == line and target.target.range.start.side == 'head', 'global append changed matched anchor/order')
end
wait(function() return api.nvim_get_current_buf() == diff end, 'global refresh displaced diff')

-- A location with existing roots remains a list. Native append adds a distinct
-- root on that exact line, returning to the list within the same command stream.
api.nvim_win_set_cursor(diff_win, { 1, 0 })
normal('<CR>')
assert(kind() == 'threads', 'commented line did not open root list')
local list, discussion = api.nvim_get_current_buf(), api.nvim_get_current_win()
normal('aSame-line root<Esc>:wq<CR>')
assert(api.nvim_get_current_buf() == list and api.nvim_get_current_win() == discussion, 'list append did not return synchronously')
local same = matches('Same-line root')
assert(#same == 1 and same[1].target.kind == 'diff' and same[1].target.target.range.start.line == 1, 'list append replied or changed code anchor')
wait(function() return text():find('Same-line root', 1, true) end, 'list was not rebuilt after native append')

-- Repeating a native macro replies twice to the conversation root, never edits
-- the owned root or targets a previous child as its thread is rebuilt.
row('Native root')
normal('<CR>')
assert(kind() == 'thread', 'root list did not open conversation')
local conversation = api.nvim_get_current_buf()
vim.fn.setreg('q', keys('aMacro reply<Esc>:wq<CR>'))
normal('2@q')
assert(api.nvim_get_current_buf() == conversation and api.nvim_get_current_win() == discussion, 'reply macro ran ahead of composer close')
local replies = matches('Macro reply')
assert(#replies == 2 and replies[1].target.kind == 'reply' and replies[1].target.target == roots[1].id and vim.deep_equal(replies[1].target, replies[2].target), 'reply macro changed thread or edited existing comment')
assert(#matches('Native root') == 2, 'reply macro overwrote existing root')
wait(function() return text():find('Macro reply', 1, true) end, 'reply macro did not rebuild conversation')

-- A global over the conversation body uses the same append pipeline and leaves
-- all existing comment identities unchanged.
local ids = {}
for _, item in ipairs(state().comments) do ids[#ids + 1] = item.comment.comment.id end
vim.cmd('global/^Macro reply$/normal ' .. keys('aGlobal reply<Esc>:wq<CR>'))
local global_replies = matches('Global reply')
assert(#global_replies == 2, 'conversation global lost matching comment bodies')
for _, item in ipairs(global_replies) do assert(item.target.kind == 'reply' and item.target.target == roots[1].id, 'conversation global replied to child') end
for index, id in ipairs(ids) do assert(state().comments[index].comment.comment.id == id, 'conversation global changed prior identity') end
assert(api.nvim_get_current_buf() == conversation, 'conversation global did not return reader')

-- Local save failure leaves the composer dirty. Later commands in the same
-- native sequence must see that guard before attempting session navigation.
vim.fn.writefile({ '' }, fixture .. '/race_save')
local sessions_before, saves_before = #requests('sessions'), #state().comments
normal('aRetained native reply<Esc>:wq<CR>:Review sessions<CR>')
assert(kind() == 'composer' and vim.bo.modified and text() == 'Retained native reply', 'failed native write lost composer text')
assert(#requests('sessions') == sessions_before and #state().comments == saves_before, 'failed native write allowed navigation or duplicate creation')
local retained = api.nvim_get_current_buf()
assert(require('buf_review.editor').get(retained) and not require('buf_review.editor').get(retained).saving, 'failed save stranded composer saving state')
vim.fn.delete(fixture .. '/race_save')
normal(':q!<CR>')
assert(api.nvim_get_current_buf() == conversation, 'failed composer cancel lost return reader')

-- Composer target validation happens after the preceding native action. Undoing
-- a new root then appending must not create an orphan reply or enter Insert mode.
api.nvim_set_current_win(diff_win)
api.nvim_win_set_cursor(diff_win, { 3, 0 })
normal('<CR>')
row('Native root')
normal('<CR>')
local comments_before, writes_before = #state().comments, #requests('save')
normal('ua')
assert(kind() == 'thread' and not require('buf_review.editor').get() and vim.fn.mode() == 'n', 'append opened composer or inserted after earlier undo removed root')
assert(#state().comments == comments_before - 1 and #requests('save') == writes_before and not vim.bo.modified, 'undo-append created orphan reply or changed reader text')
assert(notices[#notices]:find('Comment no longer present', 1, true), 'undo-append did not report missing target')

-- Native deletion cannot mutate a published root; later replies remain available.
api.nvim_set_current_win(diff_win)
api.nvim_win_set_cursor(diff_win, { 2, 0 })
normal('<CR>'); row('Remote root'); normal('<CR>'); row('Remote root')
local before_published = #state().comments
normal('dd:Review edit<CR>')
assert(kind() == 'thread' and not require('buf_review.editor').get(),
  'published deletion/edit opened composer or staged deletion')
assert(#state().comments == before_published and notices[#notices]:find('immutable', 1, true), 'published mutation changed saved state or hid validation')
row('Remote child'); normal(':Review reply<CR>')
assert(kind() == 'composer' and require('buf_review.editor').get().target.target == 'remote-root', 'published child selection lost root reply capability')
normal(':q!<CR>')
row('Remote root'); normal('aReply without changing published root<Esc>:wq<CR>')
assert(kind() == 'thread' and state().comments[1].comment.comment.body == 'Remote root',
  'published-root append mutated its parent')
assert(#matches('Reply without changing published root') == 1, 'immutable parent prevented new reply')

assert(vim.deep_equal(api.nvim_buf_get_lines(diff, 0, -1, false), source) and not vim.bo[diff].modified, 'native comment operations mutated immutable source')
assert(#requests('publish') == 0 and #requests('submit') == 0, 'native local composition published remotely')
print('Real-plugin native append composition passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
