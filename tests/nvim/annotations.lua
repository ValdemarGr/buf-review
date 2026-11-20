local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/state.json') end
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
local function row(value)
  for line, text in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if text == value or vim.trim(text) == value or text:sub(-#value - 2) == '  ' .. value then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. value)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, side, first, last, done)
  return {
    comment = { origin = 'read', comment = { id = id, author = { id = 'alice', name = 'alice' }, body = 'Feedback ' .. id,
      target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision,
        range = { start = { side = side, line = first }, ['end'] = { side = side, line = last or first } } } } } },
    fetched = true, done = done or false, conflict = false, summary = false,
  }
end
local root = comment('root', 'head', 1, 2)
local reply = comment('reply', 'head', 1, 2, true)
reply.comment.comment.target = { kind = 'reply', target = 'root' }
local draft = comment('draft', 'head', 1, 2, true)
draft.comment.origin, draft.fetched, draft.destination = 'draft', false, 'standalone'
local draft_only = comment('draft-only', 'head', 2, 2)
draft_only.comment.origin, draft_only.fetched, draft_only.destination = 'draft', false, 'standalone'
local receipt = comment('receipt', 'head', 2, 2, true)
receipt.comment.origin, receipt.fetched = 'draft', false
local historical = comment('historical', 'head', 1, 2)
historical.comment.comment.target.target.revision = { base = 'old-base', head = 'old-head' }
local far = comment('far', 'head', 7, 9)
local missing = comment('missing', 'head', 1)
missing.comment.comment.target.target.range = nil
local wide = comment('wide', 'head', 1)
wide.comment.comment.target = { kind = 'pull_request' }
local other = comment('other', 'head', 1)
other.comment.comment.target.target.path = 'src/other.rs'
local view = {
  id = 'annotations', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'annotations-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Annotations', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { root, reply, comment('single', 'head', 3), comment('base', 'base', 2, 3, true), draft, draft_only, receipt, historical, far, missing, wide, other },
  files = { { path = 'src/file.rs', viewed = false }, { path = 'src/other.rs', viewed = false } },
  todo = { 'root', 'single', 'historical', 'far', 'missing', 'wide', 'other' }, done = { 'reply', 'base' }, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write(view)
vim.fn.writefile({ vim.json.encode({ entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } }) }, fixture .. '/routes.json')
local function diff(side)
  local current_source = false
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.comment_role == 'comment' and meta.revision.base == revision.head then current_source = true end
  end
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' then
      local matches = current_source and (side == 'head' and meta.comment_role == 'comment' or side == 'base' and meta.comment_role == 'original_base')
        or not current_source and meta.side == side
      if matches then return buf, win end
    end
  end
  error('Missing diff side ' .. side)
end
local function marker(buf, line)
  local ns = api.nvim_get_namespaces()['buf-review']
  local parts = {}
  for _, mark in ipairs(api.nvim_buf_get_extmarks(buf, ns, { line - 1, 0 }, { line - 1, -1 }, { details = true })) do
    for _, span in ipairs(mark[4].virt_text or {}) do parts[#parts + 1] = span[1] end
  end
  return table.concat(parts)
end
local function range_rows(buf)
  local ns = api.nvim_get_namespaces()['buf-review-comment-range']
  assert(ns, 'missing comment-range namespace')
  local rows = {}
  for _, mark in ipairs(api.nvim_buf_get_extmarks(buf, ns, 0, -1, { details = true })) do
    local details, first = mark[4], mark[2]
    assert(details.hl_group == 'BufReviewCommentRange' or details.line_hl_group == 'BufReviewCommentRange', 'range uses wrong highlight group')
    local last = details.end_row or first
    if last > first and (details.end_col or 0) == 0 then last = last - 1 end
    for line = first, last do rows[line + 1] = true end
  end
  local result = {}
  for line in pairs(rows) do result[#result + 1] = line end
  table.sort(result)
  return result
end
local function match_rows(win)
  local rows = {}
  for _, match in ipairs(vim.fn.getmatches(win)) do
    if match.group == 'BufReviewCommentRange' then
      assert(match.priority == 200, 'range match priority differs from range background')
      for key, position in pairs(match) do
        if key:match('^pos%d+$') then rows[position[1]] = true end
      end
    end
  end
  local result = {}
  for line in pairs(rows) do result[#result + 1] = line end
  table.sort(result)
  return result
end
local user_match, user_window
local function preserve_user_match()
  if not user_window or not api.nvim_win_is_valid(user_window) then return end
  for _, match in ipairs(vim.fn.getmatches(user_window)) do if match.id == user_match and match.group == 'Search' then return end end
  error('range cleanup removed unrelated user match')
end
local function ranges(head, base)
  local head_buf, base_buf = diff('head'), diff('base')
  local ok, err = pcall(wait, function()
    if not vim.deep_equal(range_rows(head_buf), head) or not vim.deep_equal(range_rows(base_buf), base or {}) then return false end
    for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
      local buf = api.nvim_win_get_buf(win)
      local expected = buf == head_buf and head or (buf == base_buf and (base or {}) or {})
      local meta = vim.b[buf].buf_review
      if meta and meta.comment_role == 'comment' then expected = {} end
      if not vim.deep_equal(match_rows(win), expected) then return false end
    end
    return true
  end, 'wrong highlighted range: expected ' .. vim.inspect(head) .. ' / ' .. vim.inspect(base or {}))
  if not ok then
    local windows = {}
    for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
      windows[#windows + 1] = { marker = vim.b[api.nvim_win_get_buf(win)].buf_review, matches = match_rows(win) }
    end
    error(err .. '\nActual head/base ranges: ' .. vim.inspect({ range_rows(head_buf), range_rows(base_buf) }) .. '\nVisible: ' .. vim.inspect(windows))
  end
  preserve_user_match()
end
local function open(id, expected_root)
  command(''); wait(function() return kind() == 'home' end, 'home missing')
  row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == (expected_root or id) end, 'discussion missing ' .. id)
end

command(''); wait(function() return kind() == 'home' end, 'initial home missing')
row('src/file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' end, 'initial file missing')
local head, base = diff('head'), diff('base')
assert(marker(head, 1):find('2 comments [-]', 1, true) and marker(head, 1):find('1 local comment', 1, true), 'mixed remote/reply/draft status wrong')
assert(marker(head, 2):find('2 local comments', 1, true) and not marker(head, 2):find('%[[ x%-]%]'), 'drafts/receipts received checklist status')
assert(marker(head, 3):find('1 comment [ ]', 1, true), 'unhandled remote comment marker wrong')
assert(marker(base, 2):find('1 comment [x]', 1, true), 'Done base marker wrong')
ranges({})

-- File winbars show projected progress immediately in every visible code pane.
local function file_winbars(path, viewed)
  local sides = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local meta = vim.b[api.nvim_win_get_buf(win)].buf_review
    if meta and meta.kind == 'diff' then
      local expected = ('%s %s  %s [%s]'):format(meta.side == 'base' and 'Base' or 'Head',
        revision[meta.side], path, viewed and 'viewed' or 'unviewed')
      if meta.path ~= path or vim.wo[win].winbar ~= expected then return false end
      sides[meta.side] = true
    end
  end
  return sides.base and sides.head
end
wait(function() return file_winbars('src/file.rs', false) end, 'initial diff winbars lack unviewed status or pin/path labels')
local function file_progress(key, viewed)
  local buffer, window, cursor = api.nvim_get_current_buf(), api.nvim_get_current_win(), api.nvim_win_get_cursor(0)
  local head_tick, base_tick = api.nvim_buf_get_changedtick(head), api.nvim_buf_get_changedtick(base)
  local actions, code = #requests('actions'), #requests('code')
  local refreshes, objects = #requests('fetch_refresh'), #requests('fetch_objects')
  local publications, submissions = #requests('publish'), #requests('submit')
  input(key)
  wait(function() return #requests('actions') == actions + 1 and state().files[1].viewed == viewed
    and file_winbars('src/file.rs', viewed) end, key .. ' did not update both projected file winbars')
  assert(api.nvim_get_current_buf() == buffer and api.nvim_get_current_win() == window
    and vim.deep_equal(api.nvim_win_get_cursor(0), cursor), 'file progress moved focused code/cursor')
  assert(api.nvim_buf_get_changedtick(head) == head_tick and api.nvim_buf_get_changedtick(base) == base_tick
    and #requests('code') == code, 'file progress reloaded immutable code')
  assert(#requests('fetch_refresh') == refreshes and #requests('fetch_objects') == objects
    and #requests('publish') == publications and #requests('submit') == submissions,
    'file status required fetch or publication')
end
for _, side in ipairs({ 'base', 'head' }) do
  local _, win = diff(side)
  api.nvim_set_current_win(win); api.nvim_win_set_cursor(0, { 2, 0 })
  local duplicate
  if side == 'head' then vim.cmd('split'); duplicate = api.nvim_get_current_win() end
  file_progress('s', true)
  file_progress('u', false)
  if duplicate then api.nvim_win_close(duplicate, true) end
end
local _, viewed_window = diff('head')
api.nvim_set_current_win(viewed_window)
file_progress('s', true)
input(']f')
wait(function() return kind() == 'diff' and file_winbars('src/other.rs', false) end,
  'opening another file retained viewed status from reused diff panes')
command(''); wait(function() return kind() == 'home' end, 'home missing after file status navigation')
row('src/file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' and file_winbars('src/file.rs', true) end, 'reopened file lost projected viewed status')
head, base = diff('head'), diff('base')
file_progress('u', false)

-- Progress uses the current local projection immediately; replies participate, drafts do not.
open('root'); ranges({ 1, 2 })
head, user_window = diff('head')
user_match = api.nvim_win_call(user_window, function() return vim.fn.matchadd('Search', 'one') end)
local refreshes, code_reads = #requests('fetch_refresh'), #requests('code')
local function progress(keys, expected)
  local before = #requests('actions')
  input(keys)
  wait(function() return #requests('actions') == before + 1 and marker(head, 1):find(expected, 1, true) end, 'local progress marker did not update')
  assert(#requests('fetch_refresh') == refreshes and #requests('code') == code_reads, 'annotation update fetched extra state')
end
progress('s', '2 comments [x]')
progress('u', '2 comments [-]')
row('Feedback reply'); progress('u', '2 comments [ ]')
progress('s', '2 comments [-]')
row('Feedback root'); progress('s', '2 comments [x]')
local changed = state()
changed.comments[1].done, changed.comments[2].done = false, false
write(changed)
command('pull')
wait(function() return marker(head, 1):find('2 comments [ ]', 1, true) end, 'remote refresh did not recompute markers')
ranges({ 1, 2 })
changed = state()
local moved = vim.deepcopy(changed.comments[1].comment.comment.target.target)
moved.range.start.line, moved.range['end'].line = 2, 3
changed.comments[1].comment.comment.target.target.current = moved
write(changed); command('pull')
wait(function() return state().generation > changed.generation end, 'current anchor did not refresh')
ranges({ 1, 2 })
changed = state()
changed.comments[1].comment.comment.target.target.current = nil
write(changed); command('pull')
wait(function() return state().generation > changed.generation end, 'original anchor did not refresh')
ranges({ 1, 2 })

-- Reply selection follows the root span and remains highlighted when code has focus.
row('Feedback reply')
local _, head_window = diff('head')
api.nvim_set_current_win(head_window)
ranges({ 1, 2 })

-- Each visible window gets its own match even when both display the same buffer.
vim.cmd('split')
local duplicate_window = api.nvim_get_current_win()
ranges({ 1, 2 })
vim.v.errmsg = ''
api.nvim_win_call(duplicate_window, function() vim.fn.clearmatches() end)
local before_cleared_refresh = state().generation
command('pull')
wait(function() return state().generation > before_cleared_refresh end, 'refresh after external match cleanup did not finish')
ranges({ 1, 2 })
assert(not vim.v.errmsg:find('E803', 1, true), 'external clearmatches caused stale match deletion error')
api.nvim_win_close(duplicate_window, true)
ranges({ 1, 2 })

-- Reusing a diff window for ordinary source clears only the owned range match.
local source = api.nvim_create_buf(true, false)
api.nvim_buf_set_lines(source, 0, -1, false, { 'ordinary source' })
vim.bo[source].modified = false
api.nvim_win_set_buf(user_window, source)
wait(function() return #match_rows(user_window) == 0 end, 'source window retained comment range match')
preserve_user_match()
open('base'); ranges({}, { 2, 3 })
open('single'); ranges({ 3 })

-- Hidden/closed active discussions clear the range without marking another background reader.
local discussion_window = api.nvim_get_current_win()
local _, code_window = diff('head')
api.nvim_set_current_win(code_window)
api.nvim_win_set_buf(discussion_window, api.nvim_create_buf(false, true))
ranges({})
open('root')
discussion_window = api.nvim_get_current_win()
_, code_window = diff('head')
api.nvim_set_current_win(code_window)
api.nvim_win_close(discussion_window, true)
ranges({})

-- Current-tab visibility controls the buffer decorations too.
open('root'); ranges({ 1, 2 })
local review_tab = api.nvim_get_current_tabpage()
head, base = diff('head'), diff('base')
vim.cmd('tabnew')
wait(function() return #range_rows(head) == 0 and #range_rows(base) == 0 end, 'other-tab discussion range remained active')
for _, win in ipairs(api.nvim_tabpage_list_wins(review_tab)) do assert(#match_rows(win) == 0, 'other-tab range match remained active') end
vim.cmd('tabclose'); api.nvim_set_current_tabpage(review_tab)
ranges({ 1, 2 })

-- Among visible readers only explicit active selection determines the highlighted side/span.
local root_reader = api.nvim_get_current_buf()
local root_source = api.nvim_get_current_win()
vim.cmd('split')
local root_window = api.nvim_get_current_win()
api.nvim_set_current_win(root_source)
open('base')
local base_window = api.nvim_get_current_win()
assert(api.nvim_win_get_buf(root_window) == root_reader, 'Opening another thread repurposed retained reader')
api.nvim_set_current_win(root_window)
ranges({ 1, 2 })
api.nvim_set_current_win(base_window); ranges({}, { 2, 3 })
api.nvim_set_current_win(root_window)
_, code_window = diff('head')
api.nvim_set_current_win(code_window); ranges({ 1, 2 })
api.nvim_win_set_buf(root_window, api.nvim_create_buf(false, true))
ranges({})
api.nvim_set_current_win(base_window); ranges({}, { 2, 3 })
api.nvim_win_close(root_window, true)

-- Saving a new comment activates its assigned ID without requiring another cursor move.
open('root')
head, head_window = diff('head')
api.nvim_set_current_win(head_window)
input('a')
assert(kind() == 'composer', 'new comment composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'New saved draft' })
local before_actions = #requests('actions')
refreshes, code_reads = #requests('fetch_refresh'), #requests('code')
input(':w<CR>')
wait(function() return kind() == 'composer' and vim.b.buf_review.object ~= nil end, 'saved comment ID missing')
ranges({ 1 })
assert(marker(head, 1):find('2 local comments', 1, true), 'saved draft marker did not remain neutral')
assert(#requests('actions') == before_actions + 1 and #requests('fetch_refresh') == refreshes and #requests('code') == code_reads,
  'saved draft annotation required additional RPCs')
input(':q!<CR>'); ranges({})

-- Historical ranges stay on the referenced source, never its newer PR-head counterpart.
open('historical'); ranges({}, { 1, 2 })
head, base = diff('head'), diff('base')
assert(marker(base, 1):find('1 comment [ ]', 1, true), 'historical reference lost its marker')
assert(marker(head, 1):find('2 comments', 1, true), 'selected-head markers lost current anchors: ' .. marker(head, 1))
changed = state()
changed.comments[9].comment.comment.target.target.range.start.line = 2
changed.comments[9].comment.comment.target.target.range['end'].line = 7
write(changed); command('pull')
wait(function() return state().generation > changed.generation end, 'out-of-bounds fixture did not refresh')
open('far'); ranges({})
open('missing'); ranges({})
command('next comment')
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'wide' end, 'PR-wide discussion missing')
ranges({})
open('root'); root_reader = api.nvim_get_current_buf()
root_source = api.nvim_get_current_win()
vim.cmd('split'); root_window = api.nvim_get_current_win()
api.nvim_set_current_win(root_source)
open('other')
assert(api.nvim_win_get_buf(root_window) == root_reader, 'Opening another file repurposed retained reader')
_, code_window = diff('head')
api.nvim_set_current_win(root_window)
api.nvim_set_current_win(code_window)
ranges({})
api.nvim_win_close(root_window, true)

-- Identical paths and pins in another local batch do not inherit the active range.
local another = state()
another.id, another.generation = 'annotations-other-batch', 0
local views = vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/views.json'), '\n'))
views[another.id] = another
vim.fn.writefile({ vim.json.encode(views) }, fixture .. '/views.json')
vim.fn.writefile({ vim.json.encode({ entry = { route = 'batch', batch = view.id, fresh_review = view.review.id },
  [view.locator] = { route = 'batch', batch = another.id, fresh_review = another.review.id } }) }, fixture .. '/routes.json')
command('prs'); wait(function() return kind() == 'prs' end, 'other-batch PR browser missing')
for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
  if value:find('#1 Fixture review', 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); break end
end
require('buf_review').enter()
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == another.id end, 'other annotation batch missing')
row('src/file.rs'); require('buf_review').enter()
wait(function() return kind() == 'diff' and vim.b.buf_review.workspace == another.id end, 'other batch code missing')
code_window = api.nvim_get_current_win()
vim.cmd('split'); api.nvim_win_set_buf(0, root_reader)
root_window = api.nvim_get_current_win()
api.nvim_set_current_win(code_window)
ranges({})
api.nvim_win_close(root_window, true)

-- Exact-current comment source panes retain local file progress indicators.
open('root')
local current_buffer, current_window = diff('head')
api.nvim_set_current_win(current_window)
local initial_viewed = state().files[1].viewed
require('buf_review').progress(not initial_viewed, false, 'file')
wait(function()
  return state().files[1].viewed ~= initial_viewed
    and vim.wo[current_window].winbar:find(initial_viewed and '[unviewed]' or '[viewed]', 1, true)
end, 'current comment source winbar did not update viewed status')
assert(api.nvim_win_get_buf(current_window) == current_buffer, 'viewed toggle replaced comment source')
require('buf_review').progress(initial_viewed, false, 'file')
wait(function()
  return state().files[1].viewed == initial_viewed
    and vim.wo[current_window].winbar:find(initial_viewed and '[viewed]' or '[unviewed]', 1, true)
end, 'current comment source winbar did not restore viewed status')

-- Base-side draft paths use the current logical name after a rename, while
-- immutable source bytes still belong to the old physical path.
local base_anchor
for _, value in ipairs(state().comments) do
  if value.comment.comment.id == 'base' then base_anchor = vim.deepcopy(value.comment.comment.target.target) end
end
vim.fn.writefile({ vim.json.encode({ base = { anchor = base_anchor, code = {
  path = 'src/file.rs', base_path = 'src/old.rs', revision = revision,
  base_content = 'one\ntwo\nthree\n', head_content = 'one\nchanged\nthree\n',
  historical = true, provenance = 'recorded', binary = false, location = base_anchor,
} } }) }, fixture .. '/historical_contexts.json')
open('base')
local _, renamed_window = diff('base')
api.nvim_set_current_win(renamed_window)
assert(vim.b.buf_review.source_path == 'src/old.rs', 'rename fixture lost physical base path')
api.nvim_win_set_cursor(0, { 2, 0 })
input('<CR>')
assert(kind() == 'threads', 'renamed base source lost existing root')
input('aRenamed file draft<Esc>')
assert(kind() == 'composer', 'renamed base source could not append')
input(':wq<CR>')
wait(function()
  return kind() == 'threads' and table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n'):find('Renamed file draft', 1, true)
end, 'renamed base draft disappeared from retained source list')
local renamed_draft
for _, value in ipairs(state().comments) do
  if value.comment.comment.body == 'Renamed file draft' then renamed_draft = value.comment.comment.target.target end
end
assert(renamed_draft.path == 'src/file.rs' and renamed_draft.range.start.side == 'base'
  and vim.deep_equal(renamed_draft.revision, revision), 'renamed base draft changed logical anchor')

print('Neovim comment annotation workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
