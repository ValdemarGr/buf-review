local api = vim.api
local repository = vim.fn.getcwd()
local fixture = vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local initial = {
  id = 'append-session', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'append-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Append review', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'src/file.rs', viewed = false } }, todo = {}, done = {}, pending = {}, conflicts = {}, new_commits = false,
}
local function write(value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/state.json') end
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n'), { luanil = { object = true, array = true } }) end
local function requests(op)
  local result = {}
  local lines = vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}
  vim.list_extend(lines, vim.fn.filereadable(fixture .. '/action_attempts.jsonl') == 1 and vim.fn.readfile(fixture .. '/action_attempts.jsonl') or {})
  for _, line in ipairs(lines) do
    local value = vim.json.decode(line)
    if value.op == op then result[#result + 1] = value end
  end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(60)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local function reader_end()
  local lines = api.nvim_buf_get_lines(0, 0, -1, false)
  assert(lines[#lines] == '', 'reader trailing spacing missing')
  local last_content = #lines
  while last_content > 0 and lines[last_content] == '' do last_content = last_content - 1 end
  assert(lines[last_content] ~= '-----------------------------', 'reader has trailing append divider')
  assert(vim.bo.readonly and not vim.bo.modifiable, 'append made reader text editable')
  return #lines
end
local function location(line, empty)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
    if marker and marker.kind == 'diff' and marker.side == 'head' then api.nvim_set_current_win(win); break end
  end
  assert(kind() == 'diff', 'head comparison missing')
  api.nvim_win_set_cursor(0, { line, 0 })
  local origin, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
  input('<CR>')
  if empty then
    assert(kind() == 'diff' and api.nvim_get_current_buf() == origin and api.nvim_get_current_win() == window, 'empty diff Enter opened UI or composer')
  else
    assert(kind() == 'threads', 'diff Enter skipped existing location list')
    reader_end()
  end
end
local function refreshed(snapshot)
  local notices_before = #notices
  write(snapshot)
  vim.cmd('Review refresh')
  wait(function() return #notices > notices_before and notices[#notices]:find('workspace changed in another session', 1, true) end, 'bound refresh did not apply external state')
end
local function draft(body)
  for _, item in ipairs(state().comments) do if item.comment.comment.body == body then return item end end
  error('Missing saved comment: ' .. body)
end
local function append(body, reader, window)
  local comparison = kind() == 'diff'
  if not comparison then api.nvim_win_set_cursor(0, { reader_end(), 0 }) end
  input('a' .. body .. '<Esc>')
  assert(kind() == 'composer', 'a did not open composer: ' .. vim.inspect(notices))
  if comparison then
    assert(api.nvim_get_current_win() ~= window and api.nvim_win_get_buf(window) == reader, 'diff append replaced visible comparison pane')
  else assert(api.nvim_get_current_win() == window, 'reader append opened another pane') end
  assert(text() == body and vim.bo.filetype == 'markdown', 'a did not start inserting plain Markdown body')
  local header_text = api.nvim_eval_statusline(vim.wo.winbar, { winid = api.nvim_get_current_win(), use_winbar = true, maxwidth = 10000 }).str
  assert(header_text:find('src/file.rs', 1, true)
    and header_text:find(require('buf_review.editor').get().target.kind == 'reply' and 'Reply:' or 'Comment:', 1, true),
    'composer winbar lost target context')
  for _, namespace in pairs(api.nvim_get_namespaces()) do
    for _, mark in ipairs(api.nvim_buf_get_extmarks(0, namespace, 0, -1, { details = true })) do
      assert(not mark[4].virt_lines and not mark[4].virt_text, 'composer retained virtual context')
    end
  end
  assert(#header_text > 0 and not header_text:find('{', 1, true) and not header_text:find('target =', 1, true) and not header_text:find(revision.head, 1, true), 'comment header exposed target representation')
  input(':wq<CR>')
  wait(function() return api.nvim_get_current_buf() == reader end, 'wq did not return to original reader')
  assert(api.nvim_get_current_win() == window, 'wq closed append reader pane')
  local returned_label = api.nvim_eval_statusline(vim.wo.winbar, { winid = window, use_winbar = true, maxwidth = 10000 }).str
  assert(not returned_label:find('Comment:', 1, true) and not returned_label:find('Reply:', 1, true), 'composer winbar leaked into returned reader')
  if not comparison then reader_end() end
end

write(initial)
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'append home missing')
row('src/file.rs')
input('<CR>')
wait(function() return kind() == 'diff' end, 'comparison missing')
location(1, true)
local list, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
assert(#state().comments == 0, 'empty location entry created a comment')
append('First root on line one', list, window)
local first = draft('First root on line one')
assert(first.comment.comment.target.target.range.start.line == 1 and first.destination == 'review', 'append root lost captured line/destination')
assert(kind() == 'diff', 'new diff root did not return comparison')
location(1)
assert(kind() == 'threads' and text():find('First root on line one', 1, true), 'one-root location bypassed list')
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
  if marker and marker.kind == 'diff' and marker.side == 'head' then api.nvim_set_current_win(win); break end
end
list, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
append('Second root on same line', list, window)
assert(draft('Second root on same line').comment.comment.target.kind == 'diff' and #state().comments == 2 and kind() == 'diff', 'commented diff append edited or replied instead of creating root')
location(1)
assert(kind() == 'threads' and text():find('First root on line one', 1, true) and text():find('Second root on same line', 1, true), 'multiple-root location bypassed or failed to refresh list')
list, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
row('First root on line one')
for _, operation in ipairs({ 'reply', 'edit' }) do
  vim.cmd('Review ' .. operation)
  assert(kind() == 'composer', 'list explicit ' .. operation .. ' did not open composer')
  input(':q<CR>')
  assert(api.nvim_get_current_buf() == list and api.nvim_get_current_win() == window, 'list explicit ' .. operation .. ' did not return originating reader')
end
local before = #requests('save')
input('a<Esc>:wq<CR>')
wait(function() return api.nvim_get_current_buf() == list end, 'blank append did not return reader')
assert(#requests('save') == before and #state().comments == 2, 'blank append created saved object')

-- Saving repeatedly edits the same draft; the final close returns to the reader.
input('aSaved once<Esc>')
vim.cmd('write')
local identity = require('buf_review.editor').get().comment
assert(identity, 'first append write did not capture comment identity')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Saved twice' })
vim.cmd('write')
assert(require('buf_review.editor').get().comment == identity and #state().comments == 3, 'repeated append write duplicated creation')

-- Context follows every composer view without becoming editable Markdown.
local composing, composing_window = api.nvim_get_current_buf(), api.nvim_get_current_win()
vim.cmd('split')
local duplicate_window = api.nvim_get_current_win()
require('buf_review.editor').get().status('Saved 100%\nlocally')
for _, win in ipairs({ composing_window, duplicate_window }) do
  local label = api.nvim_eval_statusline(vim.wo[win].winbar, { winid = win, use_winbar = true, maxwidth = 10000 }).str
  assert(label:find('Saved 100%', 1, true) and label:find('locally', 1, true)
    and not label:find('\n', 1, true) and not label:find('\r', 1, true), 'composer status was interpreted as winbar formatting or multiple lines')
end
api.nvim_win_set_buf(duplicate_window, list)
local reader_label = api.nvim_eval_statusline(vim.wo[duplicate_window].winbar, { winid = duplicate_window, use_winbar = true, maxwidth = 10000 }).str
assert(not reader_label:find('Saved 100', 1, true), 'composer status leaked into another buffer')
api.nvim_win_set_buf(duplicate_window, composing)
local reopened_label = api.nvim_eval_statusline(vim.wo[duplicate_window].winbar, { winid = duplicate_window, use_winbar = true, maxwidth = 10000 }).str
assert(reopened_label:find('Saved 100%', 1, true) and text() == 'Saved twice' and not vim.bo.modified,
  'reentering composer lost context or changed saved Markdown')
api.nvim_win_close(duplicate_window, false)
api.nvim_set_current_win(composing_window)
input(':wq<CR>')
wait(function() return api.nvim_get_current_buf() == list and text():find('Saved twice', 1, true) end, 'repeated write did not refresh returned reader')

row('First root on line one'); input('<CR>')
assert(kind() == 'thread', 'location Enter did not open conversation')
local conversation = api.nvim_get_current_buf()
assert(not vim.tbl_contains(api.nvim_buf_get_lines(0, 0, -1, false), '-----------------------------'), 'single-comment conversation has divider')
append('Reply to myself', conversation, window)
assert(draft('Reply to myself').comment.comment.target.target == first.comment.comment.id, 'conversation append edited self instead of replying to root')
local dividers = {}
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value == '-----------------------------' then dividers[#dividers + 1] = line end
end
assert(#dividers == 1 and row('First root on line one') < dividers[1] and dividers[1] < row('Reply to myself'), 'conversation divider is not between its two comments')
row('Reply to myself')
input('<CR>')
assert(kind() == 'composer' and require('buf_review.editor').get().comment == draft('Reply to myself').comment.comment.id, 'conversation Enter stopped editing owned comment')
input(':q!<CR>')
assert(api.nvim_get_current_buf() == conversation and api.nvim_get_current_win() == window, 'owned Enter edit did not return conversation')

-- Confirmed, unfetched roots remain replyable published resources.
location(1); row('First root on line one'); input('<CR>')
local published = state()
published.generation = published.generation + 1
published.comments[1].destination = nil
refreshed(published)
conversation, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
append('Reply before parent refetch', conversation, window)
assert(draft('Reply before parent refetch').comment.comment.target.target == first.comment.comment.id and #state().comments == 5, 'append misclassified published unfetched root')

-- Refresh while composing must return the latest conversation, including remote replies.
location(1); row('First root on line one'); input('<CR>')
conversation, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
input('aOwn reply while remote updates<Esc>')
local changed = state()
changed.generation = changed.generation + 1
local remote = vim.deepcopy(changed.comments[1])
remote.comment.origin, remote.fetched, remote.destination = 'read', true, nil
remote.comment.comment.id, remote.comment.comment.body = 'remote-reply', 'Remote reply arrived while composing'
remote.comment.comment.author = { id = 'alice', name = 'alice' }
remote.comment.comment.target = { kind = 'reply', target = first.comment.comment.id }
changed.comments[#changed.comments + 1] = remote
refreshed(changed)
input(':wq<CR>')
wait(function() return api.nvim_get_current_buf() == conversation end, 'reply append did not restore conversation')
assert(text():find('Remote reply arrived while composing', 1, true) and text():find('Own reply while remote updates', 1, true), 'append return restored stale reader contents')
row('Remote reply arrived while composing')
append('Reply from another comment row', conversation, window)
assert(draft('Reply from another comment row').comment.comment.target.target == first.comment.comment.id, 'append replied to selected child instead of conversation root')

-- A fetched other-author root uses the same append reply workflow.
changed = state()
changed.generation = changed.generation + 1
local other_root = vim.deepcopy(changed.comments[1])
other_root.comment.origin, other_root.fetched, other_root.destination = 'read', true, nil
other_root.comment.comment.id, other_root.comment.comment.body = 'other-root', 'Another reviewer root'
other_root.comment.comment.author = { id = 'alice', name = 'alice' }
other_root.comment.comment.target.target.range = { start = { side = 'head', line = 3 }, ['end'] = { side = 'head', line = 3 } }
changed.comments[#changed.comments + 1] = other_root
refreshed(changed)
location(3); row('Another reviewer root'); input('<CR>')
conversation, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
append('Append reply to other reviewer', conversation, window)
assert(draft('Append reply to other reviewer').comment.comment.target.target == 'other-root', 'append edited or misdirected other-author root')
row('Another reviewer root')
local notices_before_enter, saves_before_enter = #notices, #requests('save')
input('<CR>')
assert(api.nvim_get_current_buf() == conversation and #notices == notices_before_enter and #requests('save') == saves_before_enter and vim.bo.readonly, 'other-author Enter performed comment action')
vim.cmd('Review reply')
assert(kind() == 'composer' and require('buf_review.editor').get().target.target == 'other-root', 'explicit reply did not target conversation root')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Explicit reply returns to conversation' })
input(':wq<CR>')
assert(api.nvim_get_current_buf() == conversation and api.nvim_get_current_win() == window and text():find('Explicit reply returns to conversation', 1, true), 'explicit reply did not return refreshed conversation')
row('Append reply to other reviewer')
vim.cmd('Review edit')
assert(kind() == 'composer', 'explicit edit did not open owned reply')
input(':q<CR>')
assert(api.nvim_get_current_buf() == conversation and api.nvim_get_current_win() == window, 'explicit edit q closed conversation')

-- Active WIP roots and replies keep their existing review destination.
changed = state()
changed.generation = changed.generation + 1
changed.wip = { id = 'append-wip', active = true }
refreshed(changed)
location(2, true)
list, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
append('Review root through append', list, window)
local review_root = draft('Review root through append')
assert(review_root.destination == 'review', 'append ignored active review destination')
location(2)
row('Review root through append'); input('<CR>')
conversation, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
changed = state()
changed.generation = changed.generation + 1
changed.wip.active = false
refreshed(changed)
append('Reply inherits unpublished review destination', conversation, window)
assert(draft('Reply inherits unpublished review destination').destination == 'review', 'append reply changed unpublished parent destination')

-- Failure retains text and dirty composer; session changes remain blocked.
location(2)
list, window = api.nvim_get_current_buf(), api.nvim_get_current_win()
input('aRetained append after failure<Esc>')
local composer = api.nvim_get_current_buf()
vim.fn.writefile({ '' }, fixture .. '/race_save')
local saved, failure = pcall(vim.cmd, 'write')
assert(not saved and failure:find('changed again during save', 1, true) and api.nvim_get_current_buf() == composer and vim.bo.modified and text() == 'Retained append after failure', 'failed append save lost text/composer')
vim.fn.delete(fixture .. '/race_save')
local sessions_before = #requests('sessions')
vim.cmd('Review sessions')
assert(api.nvim_get_current_buf() == composer and #requests('sessions') == sessions_before, 'session navigation discarded dirty append')
input(':q!<CR>')
wait(function() return api.nvim_get_current_buf() == list end, 'cancel did not restore append reader')

-- A stale pinned list refuses append; a composer opened earlier refuses changed target.
input('aPinned composer text<Esc>')
changed = state()
changed.generation = changed.generation + 1
changed.comparison.head = string.rep('c', 40)
refreshed(changed)
saved, failure = pcall(vim.cmd, 'write')
assert(not saved and failure:find('target or saved content changed', 1, true) and vim.bo.modified and text() == 'Pinned composer text', 'comparison change silently retargeted append save')
input(':q!<CR>')
wait(function() return api.nvim_get_current_buf() == list end, 'pin refusal did not retain reader')
before = #requests('save')
input('a')
assert(kind() == 'threads' and #requests('save') == before, 'stale location append silently adopted current comparison')

-- Binary code cannot enter a line list and bypass the existing anchor guard.
vim.cmd('Review')
wait(function() return kind() == 'home' end, 'binary fixture home missing')
local client = require('buf_review.client')
local request = client.request
client.request = function(root, payload, callback)
  if payload.op ~= 'code' then return request(root, payload, callback) end
  return request(root, payload, function(response, failure)
    client.request = request
    if response and response.code then response.code.binary = true end
    callback(response, failure)
  end)
end
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:match('%s%ssrc/file%.rs$') then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
input('<CR>')
wait(function() return kind() == 'diff' and client.request == request end, 'binary code fixture missing')
input('<CR>')
assert(kind() == 'diff' and notices[#notices]:find('Binary files', 1, true), 'binary Enter exposed append line anchoring')

print('Neovim append workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
