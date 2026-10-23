local api = vim.api
local repository = vim.fn.getcwd()
local function run(name, env)
  local runner = repository .. '/tests/nvim/run.lua'
  local path = repository .. '/tests/nvim/' .. name .. '.lua'
  local command = ('lua assert(loadfile(%q))(%q)'):format(runner, path)
  return vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c', command }, { env = env, text = true }):wait()
end
local fixture = vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local messages = {}
vim.notify = function(message) messages[#messages + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function read_comment(id, author, line, draft)
  return { comment = { origin = draft and 'draft' or 'read', comment = { id = id, body = 'Feedback ' .. id, author = { id = author, name = author }, target = { kind = 'diff', target = { revision = revision, path = 'src/file.rs', range = { start = { side = 'head', line = line }, ['end'] = { side = 'head', line = line } } } } } }, fetched = not draft, draft = draft or false, destination = draft and 'review' or nil, done = false, conflict = false, summary = false }
end
local initial = {
  id = 'workspace-1', generation = 0,
  locator = 'https://github.com/acme/repo/pull/1',
  review = { id = 'snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Fixture review', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comparison = revision, comments = { read_comment('first', 'alice', 1), read_comment('second', 'me', 2) },
  files = { { path = 'src/file.rs', viewed = false } }, todo = { 'first', 'second' }, done = {}, pending = {}, conflicts = {}, new_commits = false,
  last_error = 'Cached gh failed\nSecond cached diagnostic',
}
initial.comments[1].anchor_error = 'Anchor rejected\nExact location unavailable'
initial.comments[1].comment.comment.body = table.concat({ 'Feedback first', 'Preserve this body line', '', '- [ ] Keep Markdown list', '', '```rust', 'let values = [1, 2, 3];', 'let total: i32 = values.iter().sum();', 'assert_eq!(total, 6);', '```', '', '> Keep quoted feedback', '', 'Final discussion paragraph' }, '\n')
local function write(path, data) vim.fn.writefile({ vim.json.encode(data) }, fixture .. '/' .. path) end
write('state.json', initial)
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
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), (reason or 'timed out') .. '\n' .. vim.inspect(messages) .. '\n' .. table.concat(vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}, '\n'))
  ReviewTest.wait(30)
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(text) vim.cmd('Review ' .. text) end
local function input(text)
  api.nvim_feedkeys(api.nvim_replace_termcodes(text, true, false, true), 'xt', false)
  ReviewTest.wait(60)
end
local function edit(text) api.nvim_buf_set_lines(0, 0, -1, false, { text }) end
local function focus_diff(side)
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
    if marker and marker.kind == 'diff' and marker.side == side then api.nvim_set_current_win(win); return end
  end
  error('Missing diff side: ' .. side)
end
local function home_row(pattern)
  for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do if text:find(pattern, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end end
  error('Missing row: ' .. pattern)
end
local function local_discussion(body, line)
  for row, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:match('%s%ssrc/file%.rs$') then api.nvim_win_set_cursor(0, { row, 0 }); break end
  end
  require('buf_review').enter()
  wait(function() return kind() == 'diff' end)
  api.nvim_win_set_cursor(0, { line, 0 })
  require('buf_review').enter()
  home_row(body); require('buf_review').enter()
  assert(kind() == 'thread', 'local discussion did not open')
end

local function object_text()
  local cursor = api.nvim_win_get_cursor(0)[1]
  local line = api.nvim_get_current_line()
  if line:match('^    ') and cursor > 1 then
    return api.nvim_buf_get_lines(0, cursor - 2, cursor, false)[1] .. '\n' .. line
  end
  local following = api.nvim_buf_get_lines(0, cursor, cursor + 1, false)[1]
  return line .. (following and following:match('^    ') and ('\n' .. following) or '')
end

local function stationary_view(before, reason)
  local after = vim.fn.winsaveview()
  local count = api.nvim_buf_line_count(0)
  local expected_line = math.min(before.lnum, count)
  local expected_column = math.min(before.col, math.max(0, #(api.nvim_buf_get_lines(0, expected_line - 1, expected_line, false)[1] or '') - 1))
  assert(after.lnum == expected_line and after.col == expected_column, reason .. ': cursor moved ' .. vim.inspect(before) .. ' -> ' .. vim.inspect(after))
  assert(after.topline == math.min(before.topline, count) and after.leftcol == before.leftcol and (after.skipcol or 0) == (before.skipcol or 0), reason .. ': viewport moved ' .. vim.inspect(before) .. ' -> ' .. vim.inspect(after))
end

local function discussion_body_preserved()
  local lines = api.nvim_buf_get_lines(0, 0, -1, false)
  local body = vim.split(initial.comments[1].comment.comment.body, '\n', { plain = true })
  local first_body, occurrences = nil, 0
  for line, text in ipairs(lines) do
    if text == body[1] then first_body = line; occurrences = occurrences + 1 end
  end
  assert(occurrences == 1, 'discussion body preview was duplicated into metadata')
  for offset, text in ipairs(body) do assert(lines[first_body + offset - 1] == text, 'discussion changed Markdown body line ' .. offset) end
  assert(lines[first_body - 1] == '' and lines[first_body - 2]:find('@alice', 1, true) and lines[first_body - 2]:find('src/file.rs', 1, true), 'discussion lacks separate styled metadata and body')
  assert(vim.bo.filetype == 'markdown' and vim.bo.readonly and not vim.bo.modifiable, 'discussion lost native read-only Markdown')
  local discussion_namespace = api.nvim_get_namespaces()['buf-review-discussion']
  assert(discussion_namespace, 'discussion highlight namespace missing')
  local groups = {}
  for _, mark in ipairs(api.nvim_buf_get_extmarks(0, discussion_namespace, 0, -1, { details = true })) do if mark[4].hl_group then groups[mark[4].hl_group] = true end end
  assert(groups.BufReviewPath and groups.BufReviewAuthor and groups.BufReviewError, 'discussion metadata lacks semantic highlights')
  return first_body
end

local function loading_blocks_actions(reason)
  wait(function() return kind() == 'loading' end, reason .. ': loading buffer missing')
  assert(vim.bo.readonly and not vim.bo.modifiable, reason .. ': loading buffer is writable')
  local visible = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n')
  assert(visible:find('Loading remote review', 1, true), reason .. ': no loading status')
  assert(not visible:find('Feedback first', 1, true), reason .. ': stale home remained visible')
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local marker = vim.b[buf].buf_review
    assert(not (marker and marker.kind == 'home'), reason .. ': another stale home window remained visible')
  end
  local guarded = { 'start', 'save', 'reaction', 'viewed', 'publish', 'submit', 'summary', 'code' }
  local before = {}
  for _, operation in ipairs(guarded) do before[operation] = #requests(operation) end
  local picker = vim.ui.select
  local opened_picker = false
  vim.ui.select = function() opened_picker = true end
  for _, action in ipairs({ 'start', 'comment', 'conclude', 'publish', 'done', 'viewed' }) do
    command(action)
    assert(kind() == 'loading', reason .. ': ' .. action .. ' replaced loading view')
  end
  require('buf_review').enter()
  require('buf_review').progress(true)
  assert(kind() == 'loading' and not opened_picker, reason .. ': loading allowed object actions')
  vim.ui.select = picker
  local responsive = false
  vim.defer_fn(function() responsive = true end, 30)
  assert(ReviewTest.wait(80, function() return responsive end, 5), reason .. ': remote fetch blocked Neovim event loop')
  assert(kind() == 'loading', reason .. ': fixture fetch ended before loading checks')
  for _, operation in ipairs(guarded) do assert(#requests(operation) == before[operation], reason .. ': loading sent ' .. operation) end
end

local function refresh_completions()
  local path = fixture .. '/refresh_complete.jsonl'
  return vim.fn.filereadable(path) == 1 and #vim.fn.readfile(path) or 0
end

local function background_home(reason, completed_before)
  wait(function() return kind() == 'home' end, reason .. ': saved home missing')
  assert(refresh_completions() == completed_before, reason .. ': home waited for remote completion')
  assert(table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback first', 1, true), reason .. ': saved content hidden')
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
    assert(not (marker and marker.kind == 'loading'), reason .. ': background fetch installed blocking view')
  end
  local responsive = false
  vim.defer_fn(function() responsive = true end, 30)
  assert(ReviewTest.wait(80, function() return responsive end, 5), reason .. ': background request blocked event loop')
  assert(kind() == 'home' and refresh_completions() == completed_before, reason .. ': remote finished before background checks')
end

if vim.env.BUF_REVIEW_FIRST_FAILURE == '1' then
  vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
  vim.fn.writefile({ '' }, fixture .. '/fail_refresh')
  command('')
  loading_blocks_actions('failed first load')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Fixture refresh failed', 1, true) end)
  local completed = refresh_completions()
  local cached = vim.deepcopy(state().comments)
  vim.fn.delete(fixture .. '/fail_refresh')
  command('')
  background_home('entry after failed first fetch', completed)
  local before_start = #requests('start')
  command('start')
  wait(function() return #requests('start') == before_start + 1 and state().wip and state().wip.active end)
  assert(refresh_completions() == completed, 'failure consumed first wait only after a later fetch finished')
  wait(function() return refresh_completions() > completed end)
  assert(vim.deep_equal(state().comments, cached), 'failed initial fetch or later background action lost cached comments')
  print('Failed first fetch unlocks later background refresh')
  vim.fn.delete(fixture, 'rf')
  vim.cmd('qa!')
end

vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')

command('')
loading_blocks_actions('initial entry')
wait(function() return kind() == 'home' and #requests('fetch_refresh') == 1 and state().generation == 1 end)
vim.fn.delete(fixture .. '/delay_refresh')
assert(vim.bo.readonly and not vim.bo.modifiable)
assert(api.nvim_buf_get_lines(0, 0, 1, false)[1]:find(initial.locator, 1, true))
local home_lines = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n')
assert(home_lines:find('Todo %(2%)') and home_lines:find('Unpublished changes %(0%)'))
assert(home_lines:find('Pull request:', 1, true) and home_lines:find('Reviewer:', 1, true) and home_lines:find('Git:', 1, true) and home_lines:find('HEAD', 1, true) and home_lines:find('Freshness:', 1, true), 'home header lost labeled context')
assert(home_lines:find(revision.base:sub(1, 10), 1, true) and home_lines:find(revision.head:sub(1, 10), 1, true), 'home comparison omitted short hashes')
assert(not home_lines:find(revision.base, 1, true) and not home_lines:find(revision.head, 1, true), 'home comparison kept full hashes')
assert(not home_lines:find('{{{1', 1, true), 'home exposed fold markers')
assert(vim.wo.foldmethod == 'expr', 'home sections do not use native expression folds')
local todo_heading = home_row('Todo (2)')
assert(vim.fn.foldlevel(todo_heading) == 1 and vim.fn.foldlevel(todo_heading + 1) == 1, 'Todo heading/items do not share their native fold')
vim.cmd('normal! zc')
assert(vim.fn.foldclosed(todo_heading) == todo_heading and vim.fn.foldclosedend(todo_heading) >= todo_heading + 4, 'Todo fold excludes two-line comment rows')
vim.cmd('normal! zo')
assert(vim.fn.foldclosed(todo_heading) == -1, 'native fold could not reopen Todo')
local home_namespace = api.nvim_get_namespaces()['buf-review-home']
assert(home_namespace, 'home highlight namespace missing')
local highlight_groups = {}
for _, mark in ipairs(api.nvim_buf_get_extmarks(0, home_namespace, 0, -1, { details = true })) do
  if mark[4].hl_group then highlight_groups[mark[4].hl_group] = true end
end
for _, group in ipairs({ 'BufReviewTitle', 'BufReviewLabel', 'BufReviewSection', 'BufReviewPath', 'BufReviewAuthor', 'BufReviewPreview', 'BufReviewState', 'BufReviewError' }) do
  assert(highlight_groups[group], 'missing home semantic highlight: ' .. group)
end
local body_line = home_row('Feedback first')
assert(api.nvim_get_current_line():match('^    '), 'comment preview is not its own indented line')
api.nvim_win_set_cursor(0, { body_line - 1, 0 })
assert(api.nvim_get_current_line():find('src/file.rs', 1, true) and api.nvim_get_current_line():find('@alice', 1, true), 'metadata missing above comment preview')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
assert(vim.b.buf_review.object == 'first', 'metadata Enter targeted another comment')
local first_discussion_body = discussion_body_preserved()
api.nvim_win_set_cursor(0, { first_discussion_body + 6, 3 })
vim.cmd('normal! zt')
local body_view = vim.fn.winsaveview()
require('buf_review').progress(true)
wait(function() return #state().done == 1 end)
stationary_view(body_view, 'discussion body s')
discussion_body_preserved()
require('buf_review').progress(false)
wait(function() return #state().done == 0 end)
stationary_view(body_view, 'discussion body u')
discussion_body_preserved()
local discussion_refresh_generation = state().generation
command('refresh')
wait(function() return state().generation > discussion_refresh_generation end)
stationary_view(body_view, 'discussion body background refresh')

command('')
wait(function() return kind() == 'home' end)
body_line = home_row('Feedback first')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
assert(vim.b.buf_review.object == 'first', 'preview Enter targeted another comment')
command('')
wait(function() return kind() == 'home' end)

-- Both visual lines target the same item while s/u leave physical position fixed.
body_line = home_row('Feedback first')
api.nvim_win_set_cursor(0, { body_line, 2 })
vim.cmd('normal! zt')
local before_progress = vim.fn.winsaveview()
require('buf_review').progress(true)
wait(function() return #state().done == 1 end)
stationary_view(before_progress, 'home preview s')
assert(requests('reaction')[#requests('reaction')].comment == 'first', 'preview progress targeted another comment')
local progress_refresh = refresh_completions()
command('refresh')
wait(function() return refresh_completions() > progress_refresh end)
local current_position, before_next = vim.fn.winsaveview(), #messages
command('next')
assert(kind() == 'home' and #messages > before_next and messages[#messages]:find('No other todo items', 1, true),
  'next used remembered progress instead of the current cursor item')
stationary_view(current_position, 'no other Todo after refresh')
home_row('Feedback first')
command('next')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'second' end, 'next did not advance after highlighted Done item')
command('')
wait(function() return kind() == 'home' end)

body_line = home_row('Feedback first')
api.nvim_win_set_cursor(0, { body_line - 1, 0 })
before_progress = vim.fn.winsaveview()
require('buf_review').progress(false)
wait(function() return #state().done == 0 end)
stationary_view(before_progress, 'home metadata u')
assert(requests('reaction')[#requests('reaction')].comment == 'first', 'metadata progress targeted another comment')

-- Visual progress deduplicates metadata/body and ignores headings, without moving.
local before_bulk = #requests('reaction')
home_row('Todo (2)')
input('V4j')
before_progress = vim.fn.winsaveview()
input('s')
wait(function() return #state().done == 2 end)
stationary_view(before_progress, 'visual Todo s')
assert(#requests('reaction') == before_bulk + 2, 'visual progress repeated objects or acted on heading')
before_bulk = #requests('reaction')
home_row('Done (2)')
input('V4j')
before_progress = vim.fn.winsaveview()
input('u')
wait(function() return #state().done == 0 end)
stationary_view(before_progress, 'visual Done u')
assert(#requests('reaction') == before_bulk + 2, 'visual unmark repeated objects or acted on heading')

-- Later fetches leave home usable and preserve native fold/cursor choices.
todo_heading = home_row('Todo (2)')
vim.cmd('normal! zc')
local folded_file = home_row('Unviewed files') + 1
api.nvim_win_set_cursor(0, { folded_file, 2 })
local before_refresh_cursor = api.nvim_win_get_cursor(0)
local completed_before = refresh_completions()
vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
command('refresh')
background_home('later manual refresh', completed_before)
assert(vim.fn.foldclosed(todo_heading) == todo_heading and vim.deep_equal(api.nvim_win_get_cursor(0), before_refresh_cursor), 'background fetch displaced home folds or cursor while pending')
wait(function() return refresh_completions() > completed_before end)
assert(vim.fn.foldclosed(todo_heading) == todo_heading, 'background refresh discarded closed Todo fold')
assert(vim.deep_equal(api.nvim_win_get_cursor(0), before_refresh_cursor) and api.nvim_get_current_line():find('src/file.rs', 1, true), 'background refresh moved selected file: ' .. vim.inspect(before_refresh_cursor) .. ' -> ' .. vim.inspect(api.nvim_win_get_cursor(0)) .. ' ' .. api.nvim_get_current_line())
completed_before = refresh_completions()
local before_background_viewed = #requests('viewed')
command('refresh')
background_home('actionable background home', completed_before)
before_progress = vim.fn.winsaveview()
require('buf_review').progress(true)
wait(function() return #requests('viewed') == before_background_viewed + 1 and state().files[1].viewed end)
stationary_view(before_progress, 'home file s during background refresh')
assert(refresh_completions() == completed_before and kind() == 'home', 'progress waited for background refresh')
local after_mark_view = vim.fn.winsaveview()
wait(function() return refresh_completions() > completed_before end)
assert(state().files[1].viewed, 'stale background completion overwrote accepted progress')
local after_background = vim.fn.winsaveview()
assert(after_background.lnum == after_mark_view.lnum and after_background.col == after_mark_view.col, 'later refresh jumped to previously marked file')
local moved_file = home_row('Viewed files') + 1
api.nvim_win_set_cursor(0, { moved_file, 0 })
before_progress = vim.fn.winsaveview()
require('buf_review').progress(false)
wait(function() return not state().files[1].viewed end)
stationary_view(before_progress, 'home file u')
vim.fn.delete(fixture .. '/delay_refresh')
todo_heading = home_row('Todo (2)')
vim.cmd('normal! zo')
assert(home_lines:find('Cached gh failed\nSecond cached diagnostic', 1, true), 'cached multiline error was truncated or merged')
local anchor_line = home_row('Anchor rejected')
assert(api.nvim_buf_get_lines(0, anchor_line - 1, anchor_line, false)[1]:find('Exact location unavailable', 1, true), 'anchor diagnostic did not stay within its object row')
command('start')
wait(function() return state().wip and state().wip.active end)
home_row('src/file.rs') -- Todo row opens both code and discussion.
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
assert(#api.nvim_tabpage_list_wins(0) == 3)
assert(vim.b.buf_review.object == 'first', 'multiline diagnostics shifted comment Enter target')
assert(state().comments[1].comment.comment.body == initial.comments[1].comment.comment.body, 'display formatting changed comment body')
command('reply')
assert(kind() == 'composer')
edit('Review reply')
input(':wq<CR>')
wait(function() return #requests('save') == 1 and kind() ~= 'composer' end, 'comment :wq did not save and close')
assert(#requests('submit') == 0 and #requests('publish') == 0)
assert(requests('save')[1].destination == 'review')
command('')
wait(function() return kind() == 'home' end)
home_row('Feedback first')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
discussion_body_preserved()
assert(#vim.tbl_filter(function(line) return line == '-----------------------------' end, api.nvim_buf_get_lines(0, 0, -1, false)) == 1,
  'root/reply discussion has a trailing divider instead of one inter-comment separator')
local divider = home_row('---')
assert(api.nvim_get_current_line() == '-----------------------------', 'discussion divider changed Markdown separator')
local before_divider_save = #requests('save')
require('buf_review').enter()
assert(kind() == 'thread' and #requests('save') == before_divider_save, 'discussion divider opened comment action')
local divider_highlight = false
for _, mark in ipairs(api.nvim_buf_get_extmarks(0, api.nvim_get_namespaces()['buf-review-discussion'], 0, -1, { details = true })) do
  if mark[2] == divider - 1 and mark[4].hl_group == 'BufReviewLabel' then divider_highlight = true end
end
assert(divider_highlight, 'discussion divider has no semantic highlight')
local before_divider_delete, before_divider_notice = #requests('delete'), #messages
input('dd')
assert(#requests('delete') == before_divider_delete and #messages == before_divider_notice, 'discussion divider dd fell back to root comment')
home_row('Review reply')
local removed_reply
for _, row in ipairs(state().comments) do if row.comment.comment.body == 'Review reply' then removed_reply = row.comment.comment.id end end
input('dd')
wait(function() return #requests('delete') == before_divider_delete + 1 end)
for _, row in ipairs(state().comments) do assert(row.comment.comment.id ~= removed_reply, 'thread body dd did not cancel local draft') end
assert(not table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Review reply', 1, true), 'cancelled local reply remained in discussion')
assert(not vim.tbl_contains(api.nvim_buf_get_lines(0, 0, -1, false), '-----------------------------'), 'single-comment discussion retained a divider')

command('')
wait(function() return kind() == 'home' end)
command('leave')
wait(function() return not state().wip.active end)
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Standalone PR discussion')
input(':w<CR>')
local saved = requests('save')[2]
assert(saved.destination == 'standalone' and saved.target.kind == 'pull_request')
edit('Unsaved edits')
command('')
assert(kind() == 'composer' and vim.bo.modified, 'navigation replaced modified composer')
input(':q!<CR>')
wait(function() return kind() ~= 'composer' end)
command('')
wait(function() return kind() == 'home' end)

-- Conclusion writes queue a local publication intent; only publish sends it.
vim.ui.select = function(_, _, callback) callback('comment') end
command('conclude')
wait(function() return kind() == 'composer' end)
edit('Summary')
input(':w<CR>')
assert(#requests('submit') == 0)
input(':q<CR>')
assert(#requests('submit') == 0 and #requests('publish') == 0 and state().wip.conclusion.body == 'Summary' and kind() ~= 'composer', 'saved conclusion q published or lost local summary')
command('conclude')
wait(function() return kind() == 'composer' end)
input('ZZ')
assert(#requests('submit') == 0, 'unchanged reopened ZZ submitted')
command('conclude')
wait(function() return kind() == 'composer' end)
local summary_buf = api.nvim_get_current_buf()
api.nvim_buf_delete(summary_buf, { force = true })
assert(#requests('submit') == 0, 'buffer deletion submitted')
command('')
wait(function() return kind() == 'home' end)
command('conclude')
wait(function() return kind() == 'composer' end)
edit('Summary ready')
input(':w<CR>')
vim.fn.writefile({ '' }, fixture .. '/fail_submit')
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end)
assert(#requests('submit') == 0 and #requests('publish') == 0 and state().wip.conclusion.body == 'Summary ready', 'conclusion close published instead of queuing')
command('publish')
wait(function() return #requests('publish') == 1 end)
assert(state().wip and state().wip.conclusion.body == 'Summary ready', 'failed publication lost queued conclusion')
vim.fn.delete(fixture .. '/fail_submit')
command('publish')
wait(function() return #requests('publish') == 2 and not state().wip end)

-- Fix-feedback workflow: per-item progress, cursor-based navigation, ordinary source untouched.
command('')
wait(function() return kind() == 'home' end)
home_row('Feedback first')
before_progress = vim.fn.winsaveview()
require('buf_review').progress(true)
wait(function() return #state().done == 1 end)
stationary_view(before_progress, 'fix-feedback s')
home_row('Feedback first')
command('next')
wait(function() return kind() == 'thread' end)
assert(vim.b.buf_review.object == 'second', 'Todo navigation skipped item after current')
command('')
wait(function() return kind() == 'home' end)
local second_preview = home_row('Feedback second')
api.nvim_win_set_cursor(0, { second_preview - 1, 0 })
ReviewTest.wait(30)
local before_boundary = vim.fn.winsaveview()
local before_boundary_notice = #messages
command('next')
assert(kind() == 'home' and #messages > before_boundary_notice and messages[#messages]:find('No other todo items', 1, true), 'next used previous marked item instead of current cursor')
stationary_view(before_boundary, 'no other Todo after manual motion')
require('buf_review').enter()
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'second' end)

local immutable_saves = #requests('save')
command('edit')
assert(kind() == 'thread' and #requests('save') == immutable_saves and messages[#messages]:find('immutable', 1, true), 'own fetched comment became editable')
command('publish')
wait(function() return #requests('publish') == 3 end)
assert(#requests('submit') == 0)

-- Suggestions capture immutable selected source and remain Markdown text.
command('')
wait(function() return kind() == 'home' end)
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' end)
assert(vim.bo.readonly and not vim.bo.modifiable and vim.wo.diff)
api.nvim_win_set_cursor(0, { 3, 1 })
local diff_progress_view = vim.fn.winsaveview()
command('viewed')
wait(function() return state().files[1].viewed end)
stationary_view(diff_progress_view, 'diff viewed')
command('unviewed')
wait(function() return not state().files[1].viewed end)
stationary_view(diff_progress_view, 'diff unviewed')

api.nvim_win_set_cursor(0, { 3, 0 })
command('comment')
assert(kind() == 'composer')
command('suggest')
local suggested = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n')
assert(suggested:find('```suggestion\nthree\n```', 1, true))
input(':q!<CR>')

-- Visual a creates roots with inclusive touched-line anchors; Enter stays unmapped.
focus_diff('head')
local visual_append = vim.fn.maparg('a', 'x', false, true)
assert(visual_append.buffer == 1 and visual_append.callback and vim.fn.maparg('<CR>', 'x') == '', 'diff visual creation mappings inconsistent')
input('ggvjla')
assert(kind() == 'composer')
edit('Range draft')
input(':wq<CR>')
local range_draft = requests('save')[#requests('save')]
assert(range_draft.target.target.range.start.line == 1 and range_draft.target.target.range['end'].line == 2
  and range_draft.target.target.range.start.side == 'head' and range_draft.target.target.range['end'].side == 'head')
local draft_id
for _, row in ipairs(state().comments) do if row.comment.comment.body == 'Range draft' then draft_id = row.comment.comment.id end end
assert(draft_id)
focus_diff('head')
api.nvim_win_set_cursor(0, { 1, 0 })
require('buf_review').enter()
assert(kind() == 'threads', 'multiple threads did not open chooser')
local chooser = api.nvim_buf_get_lines(0, 0, -1, false)
assert(#chooser == 6 and chooser[1] == '' and chooser[2]:find('src/file.rs', 1, true) and chooser[2]:find('@alice', 1, true) and chooser[3]:match('^    '), 'thread chooser lost safe margin or styled object rows')
assert(api.nvim_win_get_cursor(0)[1] == 2, 'chooser did not start on first actionable row')
assert(vim.fn.maparg('X', 'n') == '' and vim.fn.maparg('X', 'x') == '', 'chooser replaced native visual X')
local before_chooser_delete = #requests('delete')
local before_chooser_notice = #messages
for _, line in ipairs({ 1, 2 }) do
  for _, keys in ipairs(line == 1 and { 'dd', 'd0', 'dh', 'D', 'x', '<CR>' } or { 'd0', 'dh', 'd^' }) do
    api.nvim_win_set_cursor(0, { line, 0 })
    if keys == 'd^' then api.nvim_win_set_cursor(0, { line, #(api.nvim_get_current_line():match('^%s*')) }) end
    input(keys)
    assert(kind() == 'threads' and #requests('delete') == before_chooser_delete and #messages == before_chooser_notice, 'chooser margin/zero-length motion invoked comment action: ' .. keys)
  end
end
for _, line in ipairs({ 2, 3 }) do
  api.nvim_win_set_cursor(0, { line, 0 })
  input('dd')
  assert(#requests('delete') == before_chooser_delete and messages[#messages]:find('verified ownership', 1, true), 'chooser dd bypassed comment ownership')
end
api.nvim_win_set_cursor(0, { 2, 0 })
require('buf_review').enter()
assert(kind() == 'thread' and vim.b.buf_review.object == 'first', 'chooser metadata opened wrong thread')
focus_diff('head')
api.nvim_win_set_cursor(0, { 1, 0 })
require('buf_review').enter()
assert(kind() == 'threads')
api.nvim_win_set_cursor(0, { 3, 0 })
require('buf_review').enter()
assert(kind() == 'thread' and vim.b.buf_review.object == 'first', 'chooser preview opened wrong thread')
focus_diff('head')
for _, key in ipairs({ 'd', 'dd', 'D', 'x', 'X' }) do assert(vim.fn.maparg(key, 'n') == '', 'review diff replaced native ' .. key) end
api.nvim_win_set_cursor(0, { 2, 0 })
require('buf_review').enter()
assert(kind() == 'threads', 'line-two thread chooser missing')
for _, line in ipairs({ 2, 3 }) do
  api.nvim_win_set_cursor(0, { line, 0 })
  local before_immutable_delete = #requests('delete')
  input('dd')
  assert(#requests('delete') == before_immutable_delete and messages[#messages]:find('immutable', 1, true), 'chooser deleted fetched comment')
  local restored, reversed = #requests('restore'), #requests('reaction')
  input('u')
  wait(function() return #requests('reaction') == reversed + 1 end)
  assert(#requests('restore') == restored and requests('reaction')[reversed + 1].present == false, 'nonpending chooser u did not reverse progress')
end
command('')
wait(function() return kind() == 'home' end)
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' end)
input('gg<C-v>ja')
assert(kind() == 'diff', 'blockwise selection created composer')
input('<Esc>')
focus_diff('base')
assert(vim.fn.maparg('<CR>', 'x') == '' and vim.fn.maparg('a', 'x', false, true).buffer == 1, 'base diff visual mappings inconsistent')
input('ggVja')
assert(kind() == 'composer')
local line_range = require('buf_review.editor').get().target.target.range
assert(line_range.start.line == 1 and line_range['end'].line == 2
  and line_range.start.side == 'base' and line_range['end'].side == 'base', 'visual line comment lost base range')
input(':q!<CR>')
focus_diff('head')

-- Relocation + body use one atomic Save; cancellation sends nothing.
api.nvim_win_set_cursor(0, { 3, 0 })
local before_relocate = #requests('save')
command('relocate ' .. draft_id)
assert(kind() == 'composer', vim.inspect(messages))
input(':q!<CR>')
assert(#requests('save') == before_relocate and #requests('relocate') == 0)
command('relocate ' .. draft_id)
command('suggest')
assert(table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('```suggestion\nthree\n```', 1, true), 'unsaved relocation suggestion used original lines')
edit('Relocated body')
input(':wq<CR>')
assert(#requests('save') == before_relocate + 1 and #requests('relocate') == 0)
local relocated = requests('save')[#requests('save')]
assert(relocated.comment == draft_id and relocated.target.target.range.start.line == 3)

-- Current GitHub anchors may name only head commit. They still match exactly.
local next_state = state()
local first = next_state.comments[1].comment.comment
first.target.target.current = vim.deepcopy(first.target.target)
first.target.target.current.revision = { head = revision.head }
first.target.target.revision = { base = 'old-base', head = 'old-head' }
write('state.json', next_state)
command('refresh')
wait(function() return #requests('fetch_refresh') > 0 and state().generation > next_state.generation end)
command('')
wait(function() return kind() == 'home' end)
home_row('Feedback first')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
assert(not api.nvim_buf_get_lines(0, 0, 1, false)[1]:find('Historical', 1, true))

-- Auxiliary original-context command preserves the review tab and pinned comparison.
next_state = state()
first = next_state.comments[1].comment.comment
first.target.target.current = nil
write('state.json', next_state)
command('refresh')
wait(function() return state().generation > next_state.generation end)
local review_tab = api.nvim_get_current_tabpage()
local pinned = {}
for _, win in ipairs(api.nvim_tabpage_list_wins(review_tab)) do
  local buf = api.nvim_win_get_buf(win)
  local marker = vim.b[buf].buf_review
  if marker.kind == 'diff' then pinned[buf] = vim.deepcopy(marker.revision) end
end
command('context')
wait(function() return kind() == 'context' end)
assert(api.nvim_get_current_tabpage() ~= review_tab)
assert(table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('@@ -1 +1 @@', 1, true))
for buf, pair in pairs(pinned) do assert(vim.deep_equal(vim.b[buf].buf_review.revision, pair)) end
vim.fn.writefile({ '' }, fixture .. '/context_pair')
command('context')
wait(function() return kind() == 'context' and #api.nvim_tabpage_list_wins(0) == 2 end)
assert(api.nvim_win_get_cursor(0)[1] == 2 and vim.wo.winbar:find('old-head', 1, true), 'original context lost exact recorded line/revision')
for buf, pair in pairs(pinned) do assert(vim.deep_equal(vim.b[buf].buf_review.revision, pair)) end
local context_return = state()
context_return.pr.title = 'Context return refreshed'
write('state.json', context_return)
command('')
wait(function() return kind() == 'home' and api.nvim_buf_get_lines(0, 1, 2, false)[1] == '  ' .. context_return.pr.title end)

-- Unknown authors stay read-only; PR-wide comments cannot pretend to be threads.
next_state = state()
local unknown = read_comment('unknown', 'other', 2)
unknown.comment.comment.author = nil
local wide = read_comment('wide', 'alice', 2)
wide.comment.comment.target = { kind = 'pull_request' }
next_state.comments[#next_state.comments + 1] = unknown
next_state.comments[#next_state.comments + 1] = wide
write('state.json', next_state)
command('refresh')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback unknown', 1, true) end)
home_row('Feedback unknown')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
local unknown_notices = #messages
require('buf_review').enter()
assert(kind() == 'thread' and #messages == unknown_notices, 'unknown-author Enter performed comment action')
command('')
wait(function() return kind() == 'home' end)
home_row('Feedback wide')
require('buf_review').enter()
assert(kind() == 'thread')
command('reply')
assert(kind() == 'thread')

-- Empty Approve summary is valid and remains queued until explicit publication.
command('')
wait(function() return kind() == 'home' end)
vim.ui.select = function(_, _, callback) callback('approve') end
command('conclude')
wait(function() return kind() == 'composer' end)
edit('')
local before_summary_submit = #requests('submit')
input(':wq<CR>')
wait(function() return kind() ~= 'composer' and state().wip and state().wip.conclusion.outcome == 'approve' end)
assert(#requests('submit') == before_summary_submit, 'Approve close submitted immediately')
command('publish')
wait(function() return not state().wip end)

-- Real editor shutdown and last-window :q never complete saved conclusions.
for _, operation in ipairs({ 'qa', 'wqa', 'last' }) do
  local before_shutdown = #requests('submit')
  local process = run('shutdown', { BUF_REVIEW_SHUTDOWN = operation })
  assert(process.code == 0, process.stderr)
  assert(#requests('submit') == before_shutdown, operation .. ' submitted during editor shutdown')
end

vim.fn.writefile({ '' }, fixture .. '/fail_refresh')
vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
local cached_comments = vim.deepcopy(state().comments)
completed_before = refresh_completions()
command('')
background_home('later cached entry', completed_before)
wait(function() return refresh_completions() > completed_before end)
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('workspace changed in another session', 1, true) end)
completed_before = refresh_completions()
command('refresh')
background_home('later failed manual refresh', completed_before)
wait(function() return refresh_completions() > completed_before end)
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Fixture refresh failed', 1, true) end)
vim.fn.delete(fixture .. '/delay_refresh')
local failed_home = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n')
assert(vim.deep_equal(state().comments, cached_comments), 'failed fetch changed cached comments or saved drafts')
assert(failed_home:find('Freshness:', 1, true) and failed_home:lower():find('cached', 1, true), 'failed fetch did not identify cached fallback')
assert(failed_home:find('Fixture refresh failed\nProvider diagnostic\n  Final diagnostic', 1, true), 'refresh diagnostic lines were lost')
assert(failed_home:find('Feedback first', 1, true))
home_row('Feedback first')
require('buf_review').enter()
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'first' end, 'refresh diagnostic shifted selected comment identity')
assert(state().comments[1].comment.comment.body == initial.comments[1].comment.comment.body)

-- Failed list refresh retains multiline diagnostics, cached rows, filters and IDs.
command('prs assignee=@me state=closed')
wait(function() return kind() == 'prs' end)
local source_body = { 'ordinary source', 'keep exact text' }
vim.cmd('tabnew')
local source_buffer = api.nvim_get_current_buf()
api.nvim_buf_set_lines(source_buffer, 0, -1, false, source_body)
command('prs assignee=@me state=closed')
wait(function() return kind() == 'prs' end)
local second_pr = home_row('#42 Other review')
assert(api.nvim_buf_get_lines(0, second_pr - 1, second_pr, false)[1]:find('with details', 1, true), 'multiline PR title split its object row')
vim.fn.writefile({ 'Listing failed\r', 'Provider list diagnostic', '  Final list diagnostic' }, fixture .. '/fail_list')
local before_failed_list = #requests('list')
command('refresh')
wait(function() return #requests('list') > before_failed_list and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Provider list diagnostic', 1, true) end)
local failed_list = table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n')
assert(failed_list:find('Listing failed\nProvider list diagnostic\n  Final list diagnostic', 1, true), 'list diagnostic lines were lost')
assert(failed_list:find('#1 Fixture review', 1, true) and failed_list:find('#42 Other review with details', 1, true), 'failed list refresh discarded cached PR rows')
assert(api.nvim_get_current_line():find('#42', 1, true), 'failed list refresh lost selected PR')
local last_list = requests('list')[#requests('list')]
assert(last_list.assignee == '@me' and last_list.state == 'closed', 'failed list refresh lost filters')
local before_open = #requests('route')
vim.fn.writefile({ '0.3' }, fixture .. '/delay_refresh')
vim.fn.writefile({ '' }, fixture .. '/switch_workspace')
completed_before = refresh_completions()
require('buf_review').enter()
wait(function() return #requests('route') > before_open and kind() == 'home' and vim.b.buf_review.workspace == 'workspace-42' end)
background_home('PR switch after first session fetch', completed_before)
vim.fn.delete(fixture .. '/switch_workspace')
assert(vim.b.buf_review.workspace == 'workspace-42', 'PR selection retained previous workspace identity')
local selected_leave = #requests('leave')
command('leave')
wait(function() return #requests('leave') == selected_leave + 1 end)
assert(requests('leave')[#requests('leave')].workspace == 'workspace-42', 'selected workspace remained locked or action targeted old PR')
assert(refresh_completions() == completed_before, 'PR switch action waited for remote refresh')
wait(function() return refresh_completions() > completed_before end)
vim.fn.delete(fixture .. '/delay_refresh')
assert(requests('route')[#requests('route')].url == 'https://github.com/acme/repo/pull/42', 'diagnostic lines shifted cached PR Enter target')
assert(vim.deep_equal(api.nvim_buf_get_lines(source_buffer, 0, -1, false), source_body) and vim.bo[source_buffer].modified, 'review navigation changed ordinary source text')
assert(state().comments[1].comment.comment.body == initial.comments[1].comment.comment.body)

-- Background refresh and unrelated own actions must not strand a composer.
vim.fn.delete(fixture .. '/fail_refresh')
vim.fn.writefile({ '0.2' }, fixture .. '/delay_refresh')
local before_background = state().generation
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Typed while background refresh ran')
local retained_composer = api.nvim_get_current_buf()
command('refresh')
assert(api.nvim_get_current_buf() == retained_composer and kind() == 'composer' and vim.bo.modified and api.nvim_get_current_line() == 'Typed while background refresh ran', 'refresh replaced existing modified composer')
wait(function() return state().generation > before_background end)
vim.fn.delete(fixture .. '/delay_refresh')
local before_background_save = #requests('save')
input(':wq<CR>')
wait(function() return kind() ~= 'composer' and #requests('save') == before_background_save + 1 end)
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Typed before leaving WIP')
local before_leave = state().generation
command('leave')
wait(function() return state().generation > before_leave end)
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end)

-- Real CLI-style nonzero stale errors safely retry once after a bound cache read.
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Safe after another session changed unrelated state')
local external = state()
external.generation = external.generation + 1
write('state.json', external)
local before_get = #requests('get')
local saved, stale = pcall(vim.cmd, 'write')
assert(saved and not vim.bo.modified and #requests('get') == before_get + 1, stale)
assert(api.nvim_get_current_line() == 'Safe after another session changed unrelated state')
input(':q<CR>')
wait(function() return kind() ~= 'composer' end)

-- A second concurrent generation change never causes an unbounded retry loop.
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Retained while publication journals keep advancing')
vim.fn.writefile({ '' }, fixture .. '/race_save')
local before_race_save, before_race_get = #requests('save'), #requests('get')
saved, stale = pcall(vim.cmd, 'write')
assert(not saved and stale:find('changed again during save', 1, true) and vim.bo.modified)
assert(#requests('save') == before_race_save + 2 and #requests('get') == before_race_get + 1)
vim.fn.delete(fixture .. '/race_save')
input(':q!<CR>')

-- A saved local body change must continue refusing the retained composer edit.
external = state()
external.generation = external.generation + 1
external.comments[2] = read_comment('stale-local', 'me', 2, true)
external.comments[2].comment.comment.body = 'Saved local body'
external.todo = vim.tbl_filter(function(id) return id ~= 'second' end, external.todo)
external.done = vim.tbl_filter(function(id) return id ~= 'second' end, external.done)
write('state.json', external)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Saved local body', 1, true) end)
home_row('Saved local body')
require('buf_review').enter()
assert(kind() == 'composer', 'unsent local comment did not open editor')
edit('Unsaved edit must not overwrite external changes')
external = state()
external.comments[2].comment.comment.body = 'External changed body'
external.generation = external.generation + 1
write('state.json', external)
local before_unsafe = #requests('save')
before_get = #requests('get')
saved, stale = pcall(vim.cmd, 'write')
assert(not saved and stale:find('saved content changed', 1, true) and vim.bo.modified)
assert(#requests('get') == before_get + 1 and #requests('save') == before_unsafe + 1 and api.nvim_get_current_line() == 'Unsaved edit must not overwrite external changes')
assert(state().comments[2].comment.comment.body == 'External changed body')
input(':q!<CR>')

-- Normal close affects only its initiating view of an ordinary composer.
command('')
wait(function() return kind() == 'home' end)
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit('Shared composer stays open elsewhere')
input(':w<CR>')
local shared = api.nvim_get_current_buf()
vim.cmd('split')
local windows = #api.nvim_tabpage_list_wins(0)
input(':q<CR>')
assert(api.nvim_buf_is_valid(shared) and #api.nvim_tabpage_list_wins(0) == windows - 1 and #vim.fn.win_findbuf(shared) == 1)
api.nvim_set_current_win(vim.fn.win_findbuf(shared)[1])
vim.cmd('split')
windows = #api.nvim_tabpage_list_wins(0)
edit('Shared composer updated')
input(':wq<CR>')
assert(api.nvim_buf_is_valid(shared) and #api.nvim_tabpage_list_wins(0) == windows - 1 and #vim.fn.win_findbuf(shared) == 1)
api.nvim_set_current_win(vim.fn.win_findbuf(shared)[1])
input(':q<CR>')

-- A queued conclusion can remain visible in another split without publishing.
vim.ui.select = function(_, _, callback) callback('approve') end
command('conclude')
wait(function() return kind() == 'composer' end)
edit('Shared conclusion')
input(':w<CR>')
shared = api.nvim_get_current_buf()
vim.cmd('split')
windows = #api.nvim_tabpage_list_wins(0)
local before_shared_submit = #requests('submit')
input(':wq<CR>')
wait(function() return state().wip and state().wip.conclusion.body == 'Shared conclusion' end)
assert(api.nvim_buf_is_valid(shared) and #api.nvim_tabpage_list_wins(0) == windows - 1 and #vim.fn.win_findbuf(shared) == 1)
api.nvim_set_current_win(vim.fn.win_findbuf(shared)[1])
input(':q<CR>')
assert(#requests('submit') == before_shared_submit)

-- A confirmed published root can retain draft origin until its first refetch.
local receipt_baseline = state()
receipt_baseline.pr.title = 'Published root receipt fixture'
write('state.json', receipt_baseline)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find(receipt_baseline.pr.title, 1, true) end)
local receipt_view = state()
receipt_view.comments[#receipt_view.comments + 1] = {
  comment = { origin = 'draft', comment = {
    id = 'receipt-root', body = 'Published root before first refetch', author = initial.review.reviewer,
    target = { kind = 'diff', target = { revision = revision, path = 'src/file.rs', range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } },
  } },
  fetched = false, done = false, conflict = false, summary = false,
}
write('state.json', receipt_view)
command('refresh')
wait(function() return state().generation > receipt_view.generation end)
assert(not table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Published root before first refetch', 1, true), 'confirmed receipt appeared as an unpublished draft')
local function open_receipt()
  command('')
  wait(function() return kind() == 'home' end)
  home_row('Unviewed files')
  api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
  require('buf_review').enter()
  wait(function() return kind() == 'diff' end)
  api.nvim_win_set_cursor(0, { 1, 0 })
  require('buf_review').enter()
  wait(function() return kind() == 'threads' end)
  home_row('Published root before first refetch')
  require('buf_review').enter()
  wait(function() return kind() == 'thread' end)
  home_row('Published root before first refetch')
end
open_receipt()
command('reply')
assert(kind() == 'composer', 'published receipt root rejected a new reply')
edit('Review reply before parent refetch')
input(':wq<CR>')
local receipt_reply = requests('save')[#requests('save')]
assert(receipt_reply.destination == 'review' and receipt_reply.target.target == 'receipt-root')
open_receipt()
command('reply review')
assert(kind() == 'composer', 'published receipt root rejected explicit Review reply')
input(':q!<CR>')
for _, id in ipairs(state().todo) do assert(id ~= 'receipt-root', 'publication receipt entered checklist before refetch') end

-- Confirmed receipt comments remain immutable during an active review.
open_receipt()
command('start')
wait(function() return state().wip and state().wip.active end)
local before_receipt_saves = #requests('save')
command('edit')
assert(kind() == 'thread' and messages[#messages]:find('immutable', 1, true) and #requests('save') == before_receipt_saves,
  'published receipt became editable before refetch')
command('')
wait(function() return kind() == 'home' end)
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' end)
assert(not vim.tbl_contains(vim.fn.getcompletion('Review relocate ', 'cmdline'), 'receipt-root'), 'published receipt offered for relocation completion')
command('relocate receipt-root')
assert(kind() == 'diff' and messages[#messages]:find('unpublished', 1, true), 'published receipt opened relocation composer')
local select_before = vim.ui.select
vim.ui.select = function(items, _, callback)
  for _, row in ipairs(items) do assert(row.comment.comment.id ~= 'receipt-root', 'published receipt offered by relocation picker') end
  callback(nil)
end
command('relocate')
vim.ui.select = select_before
local retained_marker = false
for _, mark in ipairs(api.nvim_buf_get_extmarks(0, api.nvim_get_namespaces()['buf-review'], 0, -1, { details = true })) do
  if mark[2] == 0 and mark[4].virt_text then retained_marker = true end
end
assert(retained_marker, 'published receipt disappeared from diff annotations')
command('')
wait(function() return kind() == 'home' end)

-- Cursor motions select the local draft to cancel, never a remembered comment.
home_row('Feedback first')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
command('')
wait(function() return kind() == 'home' end)
home_row('External changed body')
local before_cursor_delete = #requests('delete')
input('dd')
wait(function()
  for _, row in ipairs(state().comments) do if row.comment.comment.id == 'stale-local' then return false end end
  return true
end)
assert(#requests('delete') == before_cursor_delete + 1 and requests('delete')[#requests('delete')].comment == 'stale-local',
  'cursor cancellation targeted previously opened comment')
assert(vim.fn.maparg('X', 'n') == '' and vim.fn.maparg('X', 'x') == '', 'home replaced native visual X')
local before_unbound_delete, before_unbound_notice = #requests('delete'), #messages
local before_unbound_restore = #requests('restore')
for _, line in ipairs({ 1, home_row('Todo (') - 1, home_row('Unviewed files') + 1 }) do
  api.nvim_win_set_cursor(0, { line, 0 })
  input('dd')
end
assert(#requests('delete') == before_unbound_delete and #requests('restore') == before_unbound_restore and #messages == before_unbound_notice,
  'home header/gap/file deletion targeted remembered comment')
home_row('Feedback first')
input('dd')
assert(#requests('delete') == before_unbound_delete and messages[#messages]:find('verified ownership', 1, true), 'home dd bypassed ownership')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
home_row('Feedback first')
assert(vim.fn.maparg('X', 'n') == '' and vim.fn.maparg('X', 'x') == '', 'discussion replaced native visual X')
local body_line = api.nvim_win_get_cursor(0)[1]
api.nvim_win_set_cursor(0, { body_line - 1, 0 })
before_unbound_notice = #messages
input('dd')
assert(#requests('delete') == before_unbound_delete and #messages == before_unbound_notice, 'discussion blank dd targeted root')
api.nvim_win_set_cursor(0, { body_line, 0 })
input('dd')
assert(#requests('delete') == before_unbound_delete, 'fetched discussion deletion changed saved state')
local before_gap_restore, before_gap_reaction = #requests('restore'), #requests('reaction')
for _, line in ipairs({ 1, body_line - 1 }) do
  api.nvim_win_set_cursor(0, { line, 0 })
  input('u')
end
assert(#requests('restore') == before_gap_restore and #requests('reaction') == before_gap_reaction, 'thread margin/gap u targeted root fallback')
api.nvim_win_set_cursor(0, { body_line, 0 })
input('u')
wait(function() return #requests('reaction') == before_gap_reaction + 1 end)
assert(requests('reaction')[before_gap_reaction + 1].comment == 'first' and requests('reaction')[before_gap_reaction + 1].present == false,
  'nonpending thread u did not reverse current comment progress')

-- Closing returns to the originating code pane; empty sides cannot anchor lines.
command('')
wait(function() return kind() == 'home' end)
vim.fn.writefile({ '' }, fixture .. '/empty_base')
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' end)
focus_diff('head')
local origin = api.nvim_get_current_win()
command('comment')
for _, key in ipairs({ 'd', 'dd', 'D', 'x', 'X' }) do assert(vim.fn.maparg(key, 'n') == '', 'composer replaced native ' .. key) end
assert(vim.fn.maparg('a', 'n') == '' and vim.fn.maparg('a', 'x') == '', 'composer replaced native append/text-object prefix')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Native deletion', 'Keep composer text' })
local before_native_delete = #requests('delete')
input('dd')
assert(api.nvim_get_current_line() == 'Keep composer text' and #requests('delete') == before_native_delete, 'composer dd deleted review object')
edit('Return to originating head pane')
input(':wq<CR>')
assert(api.nvim_get_current_win() == origin and vim.b.buf_review.side == 'head', 'composer close lost originating code pane')
focus_diff('base')
local before_empty = #requests('save')
require('buf_review').enter()
assert(kind() == 'diff' and messages[#messages]:find('no source lines', 1, true), 'empty base accepted line comment')
input('ggVa')
assert(kind() == 'diff', 'empty base accepted range comment')
input('<Esc>')
command('relocate ' .. draft_id)
assert(kind() == 'diff' and #requests('save') == before_empty, 'empty base accepted relocation')
command('')
wait(function() return kind() == 'home' end)
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
command('comment')
edit('File comment remains available for empty sides')
input(':wq<CR>')
local saved_file = requests('save')[#requests('save')]
assert(saved_file.target.target.range == nil)
local file_draft
for _, row in ipairs(state().comments) do if row.comment.comment.body == 'File comment remains available for empty sides' then file_draft = row.comment.comment.id end end
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' end)
api.nvim_win_set_cursor(0, { 3, 0 })
command('relocate ' .. file_draft)
command('suggest')
assert(table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('```suggestion\nthree\n```', 1, true), 'file-root relocation rejected proposed line suggestion')
input(':q!<CR>')
command('')
wait(function() return kind() == 'home' end)
vim.fn.delete(fixture .. '/empty_base')
vim.fn.writefile({ '' }, fixture .. '/empty_head')
home_row('Unviewed files')
api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 })
require('buf_review').enter()
wait(function() return kind() == 'diff' and api.nvim_buf_line_count(0) == 1 and api.nvim_buf_get_lines(0, 0, 1, false)[1] == '' end)
require('buf_review').enter()
assert(kind() == 'diff' and messages[#messages]:find('no source lines', 1, true), 'empty head accepted line comment')

-- Removed comparison commands cannot reload code or alter existing panes.
local compared_panes = {}
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local buf = api.nvim_win_get_buf(win)
  compared_panes[win] = { buffer = buf, revision = vim.b[buf].buf_review.revision }
end
local pinned_before = vim.deepcopy(state().comparison)
local before_refresh, before_code, before_notice = #requests('apply_refresh'), #requests('code'), #messages
command('compare latest')
assert(#messages > before_notice and messages[#messages]:find('Unknown Review command', 1, true), 'removed comparison command accepted')
assert(#requests('apply_refresh') == before_refresh and #requests('code') == before_code and vim.deep_equal(state().comparison, pinned_before), 'removed comparison command reloaded or reanchored code')
for win, pane in pairs(compared_panes) do
  assert(api.nvim_win_is_valid(win) and api.nvim_win_get_buf(win) == pane.buffer and vim.deep_equal(vim.b[pane.buffer].buf_review.revision, pane.revision), 'removed comparison command replaced an existing pane')
end
vim.cmd('tabnew')
assert(vim.fn.maparg('<CR>', 'n') == '' and vim.fn.maparg('s', 'n') == '' and vim.fn.maparg('cc', 'n') == '')
assert(vim.fn.maparg('wq', 'c', true) == '', 'composer abbreviation leaked')
assert(vim.fn.maparg('a', 'n') == '' and vim.fn.maparg('a', 'x') == '', 'review append mappings leaked to source')
for _, key in ipairs({ 'd', 'dd', 'D', 'x', 'X' }) do assert(vim.fn.maparg(key, 'n') == '', 'comment mapping leaked to source: ' .. key) end
api.nvim_buf_set_lines(0, 0, -1, false, { 'Native source deletion', 'Keep source text' })
local before_source_delete = #requests('delete')
input('dd')
assert(api.nvim_get_current_line() == 'Keep source text' and #requests('delete') == before_source_delete, 'ordinary source dd deleted review object')
vim.bo.modified = false

-- Blank ordinary comments use deletion semantics; conclusion rules above stay separate.
local blank_state = state()
blank_state.generation = blank_state.generation + 1
blank_state.comments = { read_comment('blank-owned', 'me', 1) }
blank_state.pending, blank_state.conflicts, blank_state.done = {}, {}, {}
blank_state.todo, blank_state.wip = { 'blank-owned' }, nil
write('state.json', blank_state)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback blank-owned', 1, true) end)
local before_blank_publish, before_blank_submit = #requests('publish'), #requests('submit')
api.nvim_win_set_cursor(0, { 1, 0 })
command('comment')
edit(' \t ')
vim.cmd('write')
assert(not vim.bo.modified and #state().comments == 1, 'new whitespace comment created an object')
input(':q<CR>')
wait(function() return kind() ~= 'composer' end)

for _, close_after in ipairs({ false, true }) do
  command('')
  wait(function() return kind() == 'home' end)
  api.nvim_win_set_cursor(0, { 1, 0 })
  command('comment')
  edit('Draft to remove ' .. tostring(close_after))
  vim.cmd('write')
  local identity = require('buf_review.editor').get().comment
  assert(identity, 'saved draft identity missing')
  input('cc <Tab> <Esc>')
  input(close_after and ':wq<CR>' or ':w<CR>')
  assert(not vim.tbl_contains(vim.tbl_map(function(row) return row.comment.comment.id end, state().comments), identity), 'blank draft write did not remove draft')
  if close_after then
    wait(function() return kind() ~= 'composer' end, 'blank draft :wq did not close')
  else
    assert(kind() == 'composer' and not vim.bo.modified, 'blank draft :w lost editor or retained modified flag')
    local before_repeat = #requests('save')
    vim.cmd('write')
    assert(#requests('save') == before_repeat, 'repeat blank write resent removed identity')
    input('u')
    assert(api.nvim_get_current_line() == 'Draft to remove false', 'undo did not recover saved draft body')
    vim.cmd('write')
    assert(require('buf_review.editor').get().comment == identity and #state().comments == 2, 'undo write recreated draft with different identity')
    edit('Typed draft after deletion')
    vim.cmd('write')
    assert(state().comments[2].comment.comment.id == identity and state().comments[2].comment.comment.body == 'Typed draft after deletion', 'typing after blank deletion lost draft identity/body')
    edit(' ')
    input(':wq<CR>')
    wait(function() return kind() ~= 'composer' end)
  end
end

command('')
wait(function() return kind() == 'home' end)
home_row('Feedback blank-owned')
require('buf_review').enter()
wait(function() return kind() == 'thread' end)
local immutable_before = vim.deepcopy(state().comments[1])
local immutable_save_count, immutable_delete_count = #requests('save'), #requests('delete')
command('edit')
assert(kind() == 'thread' and messages[#messages]:find('immutable', 1, true), 'fetched comment opened an editable composer')
input('<CR>')
assert(kind() == 'thread' and vim.bo.readonly and not vim.bo.modifiable, 'fetched Enter opened an editor')
command('delete')
assert(messages[#messages]:find('immutable', 1, true), 'fetched deletion did not explain immutability')
assert(#requests('save') == immutable_save_count and #requests('delete') == immutable_delete_count
  and vim.deep_equal(state().comments[1], immutable_before), 'immutable comment changed through edit/delete')
assert(#requests('publish') == before_blank_publish and #requests('submit') == before_blank_submit, 'local comment actions published remote changes')

-- Delete restrictions must fail without discarding the typed whitespace.
local restricted = state()
local parent = { comment = { origin = 'draft', comment = { id = 'blank-parent', body = 'Parent with local reply', author = { id = 'me', name = 'me' }, target = { kind = 'pull_request' } } }, fetched = false, done = false, destination = 'standalone', conflict = false, summary = false }
local child = vim.deepcopy(parent)
child.comment.comment.id, child.comment.comment.body = 'blank-child', 'Local child'
child.comment.comment.target = { kind = 'reply', target = 'blank-parent' }
restricted.comments = { parent, child }
restricted.generation = restricted.generation + 1
restricted.todo, restricted.pending = {}, {}
write('state.json', restricted)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Parent with local reply', 1, true) end)
home_row('Parent with local reply')
require('buf_review').enter()
assert(kind() == 'composer')
edit(' \t ')
local removed, delete_error = pcall(vim.cmd, 'write')
assert(not removed and delete_error:find('local replies', 1, true) and vim.bo.modified and api.nvim_get_current_line() == ' \t ', 'failed blank deletion discarded text or modified state')
assert(#state().comments == 2 and state().comments[1].comment.comment.body == 'Parent with local reply', 'failed deletion changed saved objects')
input(':q!<CR>')

-- Delete operators use intersected object rows once, ignoring surrounding structure.
for _, case in ipairs({
  { keys = 'dl', column = 0, expected = { 'operator-a' } },
  { keys = 'd$', expected = { 'operator-a' } },
  { keys = 'dj', expected = { 'operator-a' } },
  { keys = 'd2j', expected = { 'operator-a', 'operator-b' } },
  { keys = 'D', expected = { 'operator-a' } },
  { keys = 'x', expected = { 'operator-a' } },
  { keys = 'vjx', expected = { 'operator-a' } },
  { keys = 'V2jd', expected = { 'operator-a', 'operator-b' } },
  { keys = 'V4jx', expected = { 'operator-a', 'operator-b', 'operator-c' } },
  { keys = '<C-v>2jd', expected = { 'operator-a', 'operator-b' } },
  { keys = '<C-v>3jx', expected = { 'operator-a', 'operator-b' } },
  { keys = 'v2j0d', selection = 'exclusive', expected = { 'operator-a' } },
  { keys = 'v2j0d', selection = 'inclusive', expected = { 'operator-a', 'operator-b' } },
  { keys = 'dG', header = true, duplicate = true, expected = { 'operator-a', 'operator-b', 'operator-c' } },
}) do
  local operated = state()
  operated.generation = operated.generation + 1
  operated.comments = { read_comment('operator-a', 'me', 1, true), read_comment('operator-b', 'me', 2, true), read_comment('operator-c', 'me', 3, true) }
  operated.pending, operated.conflicts, operated.done = {}, {}, {}
  if case.duplicate then operated.pending = { { id = 'duplicate-a', change = { kind = 'create_comment', comment = operated.comments[1].comment.comment } } } end
  operated.todo, operated.wip = {}, nil
  write('state.json', operated)
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback operator-a', 1, true) end)
  local preview = home_row('Feedback operator-a')
  api.nvim_win_set_cursor(0, { case.header and 1 or preview - 1, case.column or 4 })
  vim.cmd('normal! zv')
  local before = #requests('delete')
  local saved_selection = vim.o.selection
  vim.o.selection = case.selection or 'inclusive'
  input(case.keys)
  vim.o.selection = saved_selection
  wait(function() return #requests('delete') >= before + #case.expected end, case.keys .. ' did not delete selected objects')
  local sent = requests('delete')
  assert(#sent == before + #case.expected, case.keys .. ' duplicated or expanded selected objects')
  for offset, identity in ipairs(case.expected) do
    assert(sent[before + offset].comment == identity, case.keys .. ' targeted wrong object: ' .. vim.inspect(sent))
  end
  for _, row in ipairs(state().comments) do
    assert(not vim.tbl_contains(case.expected, row.comment.comment.id), case.keys .. ' retained selected draft')
  end
  assert(#state().comments == 3 - #case.expected, case.keys .. ' changed unselected object count')
end

-- Preflight the whole selection; explicitly selected children delete before parents.
for _, forbidden in ipairs({ 'foreign', 'summary', 'unselected-child', 'selected-child' }) do
  local operated = state()
  operated.generation = operated.generation + 1
  operated.pending, operated.conflicts, operated.done = {}, {}, {}
  if forbidden == 'foreign' or forbidden == 'summary' then
    operated.comments = { read_comment('operator-owned', 'me', 1), read_comment('operator-forbidden', forbidden == 'foreign' and 'alice' or 'me', 2) }
    operated.comments[2].summary = forbidden == 'summary'
    operated.todo = { 'operator-owned', 'operator-forbidden' }
  else
    operated.comments, operated.todo = { vim.deepcopy(parent), vim.deepcopy(child) }, {}
    operated.comments[1].comment.comment.id = 'a-selected-parent'
    operated.comments[2].comment.comment.id = 'z-selected-child'
    operated.comments[2].comment.comment.target.target = 'a-selected-parent'
  end
  write('state.json', operated)
  command('')
  local wanted = (forbidden == 'foreign' or forbidden == 'summary') and 'Feedback operator-owned' or 'Parent with local reply'
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find(wanted, 1, true) end)
  local preview = home_row(wanted)
  api.nvim_win_set_cursor(0, { preview - 1, 4 })
  local before = #requests('delete')
  input(forbidden == 'selected-child' and 'V3jd' or (forbidden == 'unselected-child' and 'dd' or 'V2jd'))
  if forbidden == 'selected-child' then
    wait(function() return #requests('delete') >= before + 2 end)
    local sent = requests('delete')
    assert(#sent == before + 2 and sent[before + 1].comment == 'z-selected-child' and sent[before + 2].comment == 'a-selected-parent', 'selected parent/reply deletion did not run child first exactly once')
    assert(#state().comments == 0, 'explicitly selected local thread was not cancelled')
  else
    assert(#requests('delete') == before, forbidden .. ' selection partially deleted allowed objects')
    assert(#state().comments == 2, forbidden .. ' selection changed saved state')
  end
end

local failed_delete = state()
failed_delete.generation = failed_delete.generation + 1
failed_delete.comments = { read_comment('failure-a', 'me', 1, true), read_comment('failure-b', 'me', 2, true), read_comment('failure-c', 'me', 3, true) }
failed_delete.todo, failed_delete.pending = {}, {}
write('state.json', failed_delete)
vim.fn.writefile({ 'failure-b' }, fixture .. '/fail_delete')
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback failure-a', 1, true) end)
api.nvim_win_set_cursor(0, { home_row('Feedback failure-a') - 1, 0 })
local before_failed_delete = #requests('delete')
input('V4jd')
wait(function() return #requests('delete') >= before_failed_delete + 2 and messages[#messages]:find('Fixture deletion conflict', 1, true) end)
assert(#requests('delete') == before_failed_delete + 2, 'range deletion continued after backend failure')
assert(#state().comments == 2 and state().comments[1].comment.comment.id == 'failure-b' and state().comments[2].comment.comment.id == 'failure-c', 'failed range deletion changed later objects')
vim.fn.delete(fixture .. '/fail_delete')

-- Cancelling the last local root closes its managed selector or discussion.
local cancelled_roots = state()
local local_root = vim.deepcopy(parent)
local_root.comment.comment.id, local_root.comment.comment.body = 'local-root-a', 'Local root A'
local_root.comment.comment.target = { kind = 'diff', target = { revision = revision, path = 'src/file.rs', range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } }
local other_root = vim.deepcopy(local_root)
other_root.comment.comment.id, other_root.comment.comment.body = 'local-root-b', 'Local root B'
vim.fn.delete(fixture .. '/empty_head')
for _, reader in ipairs({ 'selector', 'thread' }) do
  cancelled_roots.generation = state().generation + 1
  cancelled_roots.comments = reader == 'selector' and { local_root, other_root } or { other_root }
  cancelled_roots.todo, cancelled_roots.done, cancelled_roots.pending = {}, {}, {}
  cancelled_roots.files[1].viewed = false
  write('state.json', cancelled_roots)
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Local root B', 1, true) end)
  api.nvim_win_set_cursor(0, { home_row('Unviewed files') + 1, 0 })
  require('buf_review').enter()
  wait(function() return kind() == 'diff' end)
  local source_window, source_buffer = api.nvim_get_current_win(), api.nvim_get_current_buf()
  api.nvim_win_set_cursor(0, { 1, 0 })
  require('buf_review').enter()
  assert(kind() == 'threads')
  if reader == 'selector' then
    local picker = api.nvim_buf_get_lines(0, 0, -1, false)
    assert(#picker == 6 and not vim.tbl_contains(picker, '-----------------------------'), 'two-root selector has a trailing divider')
    api.nvim_win_set_cursor(0, { 3, 0 }); input('dd')
    wait(function() return #state().comments == 1 end)
    local remaining_picker = api.nvim_buf_get_lines(0, 0, -1, false)
    assert(kind() == 'threads' and #remaining_picker == 4 and remaining_picker[1] == ''
      and remaining_picker[3]:find('Local root B', 1, true) and remaining_picker[4] == '',
      'single-root selector retained deleted root or divider footer')
  else
    home_row('Local root B'); require('buf_review').enter()
    assert(kind() == 'thread' and not vim.tbl_contains(api.nvim_buf_get_lines(0, 0, -1, false), '-----------------------------'),
      'single-root discussion retained a divider')
  end
  local discussion_window = api.nvim_get_current_win()
  home_row('Local root B'); input('dd')
  wait(function() return #state().comments == 0 and kind() == 'diff' end)
  assert(not api.nvim_win_is_valid(discussion_window) and api.nvim_get_current_win() == source_window
    and api.nvim_get_current_buf() == source_buffer, 'empty ' .. reader .. ' did not close back to source code')
end

-- Empty body newlines are objects only for linewise or explicit visual deletion.
for _, case in ipairs({ { keys = 'D', deleted = false }, { keys = 'd$', deleted = false }, { keys = 'dd', deleted = true }, { keys = 'vd', deleted = true }, { keys = 'vx', deleted = true } }) do
  local empty_body = state()
  empty_body.generation = empty_body.generation + 1
  empty_body.comments = { read_comment('empty-body', 'me', 1, true) }
  empty_body.comments[1].comment.comment.body = 'Body before empty line\n\nBody after empty line'
  empty_body.todo, empty_body.pending, empty_body.done = {}, {}, {}
  write('state.json', empty_body)
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Body before empty line', 1, true) end)
  local_discussion('Body before empty line', 1)
  assert(api.nvim_buf_get_lines(0, 0, 1, false)[1] == '', 'discussion lacks safe top margin')
  local before, notices = #requests('delete'), #messages
  for _, line in ipairs({ 1, 2 }) do
    for _, keys in ipairs(line == 1 and { 'dd', 'D', 'x', '<CR>' } or { 'd0', 'dh', 'd^' }) do
      api.nvim_win_set_cursor(0, { line, 0 })
      if keys == 'd^' then api.nvim_win_set_cursor(0, { line, #(api.nvim_get_current_line():match('^%s*')) }) end
      input(keys)
      assert(kind() == 'thread' and #requests('delete') == before and #messages == notices, 'thread margin/zero-length motion invoked action: ' .. keys)
    end
  end
  local before_body = home_row('Body before empty line')
  api.nvim_win_set_cursor(0, { before_body + 1, 0 })
  assert(api.nvim_get_current_line() == '', 'empty body line missing')
  input(case.keys)
  if case.deleted then wait(function() return #requests('delete') == before + 1 end) end
  assert(#requests('delete') == before + (case.deleted and 1 or 0) and #state().comments == (case.deleted and 0 or 1), case.keys .. ' misclassified empty body newline')
end

-- Cancelling a new normal operator must not replace the last completed visual redo.
local visual_repeat = state()
visual_repeat.generation = visual_repeat.generation + 1
visual_repeat.comments = { read_comment('visual-root', 'me', 1), read_comment('visual-first', 'me', 1, true), read_comment('visual-reply', 'me', 1, true) }
visual_repeat.comments[1].comment.comment.body = 'Visual root source'
visual_repeat.comments[2].comment.comment.target = { kind = 'reply', target = 'visual-root' }
visual_repeat.comments[2].comment.comment.body = 'First visual reply'
visual_repeat.comments[3].comment.comment.target = { kind = 'reply', target = 'visual-root' }
visual_repeat.comments[3].comment.comment.body = 'Visual reply before blank\n\nVisual reply after blank'
visual_repeat.todo, visual_repeat.pending, visual_repeat.done = { 'visual-root' }, {}, {}
write('state.json', visual_repeat)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Visual root source', 1, true) end)
home_row('Visual root source')
require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'visual-repeat discussion did not open')
home_row('First visual reply')
local before_visual_repeat = #requests('delete')
input('vd')
wait(function() return #requests('delete') == before_visual_repeat + 1 end)
input('d<Esc>')
api.nvim_win_set_cursor(0, { home_row('Visual reply before blank') + 1, 0 })
assert(api.nvim_get_current_line() == '')
input('.')
wait(function() return #requests('delete') == before_visual_repeat + 2 end, 'cancelled normal operator erased visual empty-newline redo')
assert(requests('delete')[before_visual_repeat + 2].comment == 'visual-reply', 'visual dot targeted stale root')

-- Native operator redo follows the current object and never consumes text registers.
local repeat_view = state()
repeat_view.generation = repeat_view.generation + 1
repeat_view.comments = { read_comment('repeat-a', 'me', 1, true), read_comment('repeat-b', 'me', 2, true) }
repeat_view.todo, repeat_view.pending, repeat_view.done = {}, {}, {}
write('state.json', repeat_view)
command('')
wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Feedback repeat-a', 1, true) end)
vim.fn.setreg('a', 'Named register stays')
vim.fn.setreg('1', 'Delete register stays')
vim.fn.setreg('-', 'Small register stays')
vim.fn.setreg('"', 'Unnamed register stays')
local registers = {}
for _, register in ipairs({ 'a', '1', '-', '"' }) do registers[register] = { vim.fn.getreg(register), vim.fn.getregtype(register) } end
local before_repeat = #requests('delete')
api.nvim_win_set_cursor(0, { home_row('Feedback repeat-a') - 1, 0 })
vim.cmd('normal! zv')
input('"add')
wait(function() return #requests('delete') == before_repeat + 1 end)
api.nvim_win_set_cursor(0, { home_row('Feedback repeat-b') - 1, 0 })
vim.cmd('normal! zv')
input('.')
wait(function() return #requests('delete') == before_repeat + 2 end)
local repeated = requests('delete')
assert(repeated[before_repeat + 1].comment == 'repeat-a' and repeated[before_repeat + 2].comment == 'repeat-b', 'dot repeated stale object instead of current row')
for register, saved in pairs(registers) do assert(vim.fn.getreg(register) == saved[1] and vim.fn.getregtype(register) == saved[2], 'review deletion changed register ' .. register) end

local before_todo_generation, before_todo_notices

-- Contextual u cancels creation in either destination; explicit Todo stays progress-only.
do
  for _, destination in ipairs({ 'standalone', 'review' }) do
    local cancelled = state()
    cancelled.generation = cancelled.generation + 1
    local draft = vim.deepcopy(parent)
    draft.comment.comment.id, draft.comment.comment.body = 'reverse-new-' .. destination, 'Cancel new ' .. destination
    draft.destination = destination
    if destination == 'review' then draft.comment.comment.target = read_comment('anchor', 'me', 1).comment.comment.target end
    cancelled.comments, cancelled.todo, cancelled.done, cancelled.pending, cancelled.files = { draft }, {}, {}, {}, {}
    cancelled.wip = destination == 'review' and { active = true } or nil
    write('state.json', cancelled)
    command('')
    wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Cancel new ' .. destination, 1, true) end)
    home_row('Cancel new ' .. destination)
    local before_cancel, before_progress = #requests('delete'), #requests('reaction')
    before_todo_generation, before_todo_notices = state().generation, #messages
    command('todo')
    wait(function() return #messages > before_todo_notices end, 'explicit Todo draft validation missing')
    assert(#requests('delete') == before_cancel and #requests('reaction') == before_progress + 1 and state().generation == before_todo_generation and #state().comments == 1, 'explicit Todo cancelled new comment')
    before_progress = #requests('reaction')
    if destination == 'standalone' then
      local home_window = api.nvim_get_current_win()
      require('buf_review').enter()
      assert(kind() == 'composer')
      edit('Unsaved comment must survive contextual cancellation')
      local composer_window = api.nvim_get_current_win()
      api.nvim_set_current_win(home_window)
      home_row('Cancel new ' .. destination)
      input('u')
      assert(#requests('delete') == before_cancel and #state().comments == 1 and messages[#messages]:find('unsaved text', 1, true), 'u cancelled draft while composer had unsaved text')
      api.nvim_set_current_win(composer_window)
      assert(vim.bo.modified and api.nvim_get_current_line() == 'Unsaved comment must survive contextual cancellation', 'failed cancellation discarded composer text')
      input(':q!<CR>')
      api.nvim_set_current_win(home_window)
      home_row('Cancel new ' .. destination)
    end
    local position = vim.fn.winsaveview()
    input('u')
    wait(function() return #state().comments == 0 end)
    stationary_view(position, 'new ' .. destination .. ' cancellation')
    assert(#requests('delete') == before_cancel + 1 and requests('delete')[before_cancel + 1].comment == draft.comment.comment.id and #requests('reaction') == before_progress, 'u did not cancel only selected creation')
  end

  -- Mixed visual reversal deduplicates every object and includes new comments.
  local mixed = state()
  mixed.generation = mixed.generation + 1
  local draft = vim.deepcopy(parent)
  draft.comment.comment.id, draft.comment.comment.body = 'mixed-new', 'Cancel mixed draft'
  mixed.comments = { draft, read_comment('mixed-done', 'alice', 2) }
  mixed.comments[2].done = true
  mixed.todo, mixed.done = {}, { 'mixed-done' }
  mixed.pending = {}
  mixed.files = { { path = 'src/mixed.rs', viewed = true } }
  mixed.wip = nil
  write('state.json', mixed)
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Cancel mixed draft', 1, true) end)
  local before_cancel, before_restore, before_progress, before_viewed = #requests('delete'), #requests('restore'), #requests('reaction'), #requests('viewed')
  input('ggVGu')
  wait(function() return #requests('delete') == before_cancel + 1 and #requests('restore') == before_restore and #requests('reaction') == before_progress + 1 and #requests('viewed') == before_viewed + 1 and #state().comments == 1 and not state().comments[1].done and not state().files[1].viewed end)
  assert(requests('delete')[before_cancel + 1].comment == 'mixed-new' and requests('reaction')[before_progress + 1].comment == 'mixed-done', 'mixed visual u expanded, duplicated, or misclassified selected objects')

  -- Preflight the whole selection; selected local children cancel before parents.
  for _, case in ipairs({ 'unselected-child', 'selected-child', 'foreign' }) do
    local threaded = state()
    threaded.generation = threaded.generation + 1
    threaded.comments = { vim.deepcopy(parent), vim.deepcopy(child) }
    threaded.comments[1].comment.comment.id, threaded.comments[1].comment.comment.body = 'cancel-parent', 'Cancel parent'
    threaded.comments[2].comment.comment.id, threaded.comments[2].comment.comment.body = 'cancel-child', 'Cancel child'
    threaded.comments[2].comment.comment.target = { kind = 'reply', target = 'cancel-parent' }
    threaded.comments[1].comment.comment.target = read_comment('anchor', 'me', 1).comment.comment.target
    threaded.comments[1].destination, threaded.comments[2].destination = 'review', 'review'
    if case == 'foreign' then threaded.comments[2].comment.comment.author.id = 'alice' end
    threaded.todo, threaded.done, threaded.pending, threaded.files = {}, {}, {}, {}
    threaded.wip = { active = true }
    write('state.json', threaded)
    command('')
    wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Cancel parent', 1, true) end)
    home_row('Cancel parent')
    before_cancel, before_restore, before_progress = #requests('delete'), #requests('restore'), #requests('reaction')
    input(case == 'unselected-child' and 'u' or 'ggVGu')
    if case == 'selected-child' then
      wait(function() return #state().comments == 0 end)
      local sent = requests('delete')
      assert(#sent == before_cancel + 2 and sent[before_cancel + 1].comment == 'cancel-child' and sent[before_cancel + 2].comment == 'cancel-parent', 'u did not cancel selected child before parent exactly once')
    else
      assert(#requests('delete') == before_cancel and #requests('restore') == before_restore and #requests('reaction') == before_progress and #state().comments == 2, case .. ' cancellation wrote before preflight completed')
      assert(messages[#messages]:find(case == 'foreign' and 'verified ownership' or 'unpublished replies', 1, true), case .. ' cancellation lacked actionable error')
    end
  end

  -- A failed cancellation stops the queue before later comments.
  local failed = state()
  failed.generation = failed.generation + 1
  failed.comments, failed.wip = {}, nil
  for _, id in ipairs({ 'cancel-failure-a', 'cancel-failure-b', 'cancel-failure-c' }) do
    local row = vim.deepcopy(parent)
    row.comment.comment.id, row.comment.comment.body = id, id
    failed.comments[#failed.comments + 1] = row
  end
  write('state.json', failed)
  vim.fn.writefile({ 'cancel-failure-b' }, fixture .. '/fail_delete')
  command('')
  wait(function() return kind() == 'home' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('cancel-failure-a', 1, true) end)
  before_cancel = #requests('delete')
  input('ggVGu')
  wait(function() return #requests('delete') == before_cancel + 2 and messages[#messages]:find('Fixture deletion conflict', 1, true) end)
  assert(#state().comments == 2 and state().comments[1].comment.comment.id == 'cancel-failure-b' and state().comments[2].comment.comment.id == 'cancel-failure-c', 'u continued cancellation after backend failure')
  vim.fn.delete(fixture .. '/fail_delete')
end

local first_failure = run('workflows', { BUF_REVIEW_FIRST_FAILURE = '1' })
assert(first_failure.code == 0, first_failure.stderr)
for _, name in ipairs({ 'sessions', 'review_tab', 'append', 'performance', 'object_fetch', 'commit_comparison', 'historical_panes', 'comment_panes', 'local_panes', 'mark_move', 'diff_lifecycle', 'annotations', 'navigation', 'log', 'native_commands', 'composer_native', 'append_native', 'action_queue', 'overview_stats', 'diff_scroll', 'checkout_git', 'checkout_ui', 'local_pr', 'local_pr_cli', 'post_publish', 'mappings', 'jumplist', 'reader_reuse', 'review_queue' }) do
  local child = run(name)
  assert(child.code == 0, name .. ': ' .. child.stderr)
end
print('Neovim review/fix workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
