local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function command(value) vim.cmd('Review ' .. value) end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. vim.v.errmsg)
  ReviewTest.wait(30)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if vim.trim(value) == needle or value:sub(-#needle - 2) == '  ' .. needle then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local view = {
  id = 'diff-lifecycle', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'diff-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Diff lifecycle', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = {}, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function path(index) return ('src/file%02d.rs'):format(index) end
for index = 1, 12 do
  local id = 'comment-' .. index
  view.files[index] = { path = path(index), viewed = false }
  view.comments[index] = {
    comment = { origin = 'read', comment = { id = id, author = { id = 'alice', name = 'alice' }, body = 'Feedback ' .. index,
      target = { kind = 'diff', target = { path = path(index), revision = revision, range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } } } } } },
    fetched = true, done = false, conflict = false, summary = false,
  }
  view.todo[index] = id
end
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
local main_windows
local function pair(expected_path, expected_kind)
  local result, windows = {}, {}
  local comment_panes = expected_kind == 'comment'
  for _, win in ipairs((expected_kind == 'context' or comment_panes) and api.nvim_tabpage_list_wins(0) or main_windows or api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == (expected_kind == 'context' and 'context' or 'diff') and (not comment_panes or meta.comment_role) then
      assert(not expected_path or meta.path == expected_path, 'managed pair retained wrong file')
      assert(vim.wo[win].diff, 'native diff disabled: ' .. api.nvim_buf_get_name(buf))
      result[#result + 1], windows[#windows + 1] = buf, win
      if comment_panes then
        assert(meta.comment_role == 'original_base' or meta.comment_role == 'comment', 'unchanged selected side was not collapsed')
        assert(meta.revision.base == (meta.comment_role == 'comment' and revision.head or revision.base)
          and meta.revision.head == revision.head, 'comment source does not preserve original/selected revisions')
      elseif expected_kind ~= 'context' then assert(vim.deep_equal(meta.revision, revision), 'file navigation changed pin') end
    end
  end
  assert(#result == 2, 'expected native diff pair: ' .. vim.inspect(result))
  if not main_windows and not expected_kind then main_windows = windows end
  assert(vim.deep_equal(vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')).comparison, revision), 'source navigation changed comparison pin')
  assert(not vim.v.errmsg:find('E96', 1, true), vim.v.errmsg)
  return result
end
local function owned_count()
  local count = 0
  for _, buf in ipairs(api.nvim_list_bufs()) do
    local meta = vim.b[buf].buf_review
    if meta and (meta.kind == 'diff' or meta.kind == 'context') then count = count + 1 end
  end
  return count
end
local function open_file(index)
  wait(function() return kind() == 'diff' and vim.b.buf_review.path == path(index) end, 'file did not open ' .. index)
  return pair(path(index))
end

command(''); wait(function() return kind() == 'home' end, 'home missing')
row(path(1)); require('buf_review').enter()
local previous = open_file(1)
-- More than eight files use fresh buffers without retaining hidden diff state.
for index = 2, 12 do
  command('next file')
  local current = open_file(index)
  assert(not vim.deep_equal(current, previous), 'file navigation reused previous comparison buffers')
  for _, buf in ipairs(previous) do assert(not api.nvim_buf_is_valid(buf), 'retired comparison buffer was not deleted') end
  assert(owned_count() == 2, 'file navigation allocated additional diff buffers')
  previous = current
end

-- A visible extra reader and an ordinary user diff survive managed-pair replacement.
local preserved_extra = api.nvim_get_current_buf()
local extra_metadata = vim.deepcopy(vim.b[preserved_extra].buf_review)
local extra_contents = api.nvim_buf_get_lines(preserved_extra, 0, -1, false)
vim.cmd('split')
local extra_window = api.nvim_get_current_win()
vim.cmd('split')
local ordinary_window = api.nvim_get_current_win()
local ordinary = api.nvim_create_buf(true, false)
api.nvim_win_set_buf(ordinary_window, ordinary)
api.nvim_buf_set_lines(ordinary, 0, -1, false, { 'ordinary user buffer' })
vim.cmd('diffthis')
api.nvim_set_current_win(extra_window)
command('previous file'); previous = open_file(11)
local preserved_other = api.nvim_get_current_buf()
local other_metadata = vim.deepcopy(vim.b[preserved_other].buf_review)
local other_contents = api.nvim_buf_get_lines(preserved_other, 0, -1, false)
local review_tab = api.nvim_get_current_tabpage()
vim.cmd('tabnew')
local other_tab = api.nvim_get_current_tabpage()
api.nvim_win_set_buf(0, preserved_other)
vim.cmd('diffthis')
api.nvim_set_current_tabpage(review_tab)
for index = 1, 10 do
  local destination = index % 2 == 1 and 12 or 11
  command(index % 2 == 1 and 'next file' or 'previous file')
  local current = open_file(destination)
  assert(api.nvim_buf_is_valid(preserved_extra) and api.nvim_buf_is_valid(preserved_other), 'visible owned diff was wiped')
  assert(vim.deep_equal(vim.b[preserved_extra].buf_review, extra_metadata) and vim.deep_equal(api.nvim_buf_get_lines(preserved_extra, 0, -1, false), extra_contents), 'same-tab snapshot was overwritten')
  assert(vim.deep_equal(vim.b[preserved_other].buf_review, other_metadata) and vim.deep_equal(api.nvim_buf_get_lines(preserved_other, 0, -1, false), other_contents), 'other-tab snapshot was overwritten')
  assert(vim.wo[ordinary_window].diff and api.nvim_buf_is_valid(ordinary), 'ordinary user diff was altered')
  assert(owned_count() <= 4, 'hidden buffers accumulated beside retained visible diffs')
  previous = current
end

-- Twelve successive comments change files without a home/reset break too.
for index = 1, 12 do
  command('next comment')
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'comment-' .. index end, 'comment navigation stalled ' .. index)
  local current = pair(path(index), 'comment')
  assert(owned_count() <= 4, 'comment navigation retained hidden code buffers')
  previous = current
end

-- Original-context pairs follow the same lifecycle; main review/other-tab readers stay visible.
local main_pair = previous
local context_previous
for index = 1, 12 do
  local contents = 'Context cycle ' .. index
  write('contexts.json', { ['comment-12'] = { available = true, anchor = view.comments[12].comment.comment.target.target,
    base_content = 'Original source', head_content = contents } })
  command('context')
  wait(function() return kind() == 'context' and api.nvim_get_current_line() == contents
    and #api.nvim_tabpage_list_wins(0) == 2 end, 'original context did not open')
  local current = pair(nil, 'context')
  if context_previous then
    assert(not vim.deep_equal(current, context_previous), 'context refresh reused previous source buffers')
    for _, buf in ipairs(context_previous) do assert(not api.nvim_buf_is_valid(buf), 'retired context buffer survived replacement') end
  end
  for _, buf in ipairs(main_pair) do assert(api.nvim_buf_is_valid(buf), 'context replacement wiped visible review code') end
  assert(api.nvim_buf_is_valid(preserved_other) and api.nvim_buf_is_valid(preserved_extra), 'context cleanup wiped another visible reader')
  assert(api.nvim_buf_is_valid(ordinary) and vim.wo[ordinary_window].diff, 'context cleanup altered user diff')
  assert(owned_count() <= 6, 'context replacement retained hidden diff buffers')
  context_previous = current
end
vim.cmd('tabclose')
api.nvim_set_current_tabpage(review_tab)
command('previous comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'comment-11' end, 'return from original context failed')
pair(path(11), 'comment')
assert(api.nvim_buf_is_valid(ordinary), 'ordinary user buffer was deleted')
api.nvim_win_close(extra_window, true)
api.nvim_set_current_tabpage(other_tab); vim.cmd('tabclose')
api.nvim_set_current_tabpage(review_tab)
command('previous comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'comment-10' end, 'navigation after retained readers closed failed')
pair(path(10), 'comment')
wait(function() return not api.nvim_buf_is_valid(preserved_extra) and not api.nvim_buf_is_valid(preserved_other) end, 'closed source snapshots were not retired')
assert(owned_count() <= 6, 'closed snapshots allocated more reader slots')
assert(api.nvim_buf_is_valid(ordinary) and vim.wo[ordinary_window].diff, 'ordinary user diff was not preserved')
api.nvim_win_close(ordinary_window, true)
command('previous comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'comment-9' end, 'navigation with hidden user buffer failed')
pair(path(9), 'comment')
assert(api.nvim_buf_is_valid(ordinary) and api.nvim_buf_get_lines(ordinary, 0, -1, false)[1] == 'ordinary user buffer',
  'hidden ordinary user buffer was deleted or changed')
print('Neovim diff lifecycle workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
