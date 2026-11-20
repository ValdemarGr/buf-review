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
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n')) end
local function records(name, op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    local value = vim.json.decode(line)
    if not op or value.op == op then result[#result + 1] = value end
  end
  return result
end
local function requests(op) return records('requests.jsonl', op) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
local function command(value) vim.cmd('Review ' .. value) end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(25)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(25)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle .. '\n' .. text())
end
local current = { base = string.rep('a', 40), head = string.rep('b', 40) }
local old = { base = 'old-base', head = 'old-head' }
local latest = current.head
local followup = { base = old.head, head = latest }
local function comment(id, revision, side, path)
  return {
    comment = { origin = 'read', comment = { id = id, body = 'Feedback ' .. id, author = { id = 'alice', name = 'alice' }, target = {
      kind = 'diff', target = { path = path or 'src/file.rs', revision = revision,
        range = { start = { side = side or 'head', line = 2 }, ['end'] = { side = side or 'head', line = 3 } } },
    } } }, fetched = true, done = false, conflict = false, summary = false,
  }
end
local reply = comment('reply', old)
reply.comment.comment.target = { kind = 'reply', target = 'recorded' }
local view = {
  id = 'historical-batch', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = current,
  review = { id = 'historical-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Historical references', revision = { base = current.base, head = latest } }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('recorded', old), reply, comment('current', current), comment('removed', old, 'head', 'removed.rs'), comment('reconstructed-base', { head = 'old-head' }, 'base'), comment('reconstructed-head', { head = 'old-head' }), comment('missing', old), comment('recorded-base', old, 'base'), comment('renamed', old, 'head', 'src/old.rs') },
  files = { { path = 'src/file.rs', viewed = false }, { path = 'src/next.rs', viewed = false } },
  todo = { 'recorded', 'reply', 'current', 'removed', 'reconstructed-base', 'reconstructed-head', 'missing', 'recorded-base', 'renamed' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function response(id, provenance, location, source, path)
  return { anchor = view.comments[id].comment.comment.target.target, diff_hunk = '@@ -2 +2 @@\n-old\n+historical', code = {
    path = path or 'src/file.rs', base_path = view.comments[id].comment.comment.target.target.path, revision = { base = source or old.head, head = latest },
    base_content = 'one\nhistorical ' .. (id == 5 and 'reconstructed base' or 'head') .. '\nthree\n', head_content = 'one\nlatest PR head\nthree\n',
    binary = false, historical = true, provenance = provenance, location = location and vim.deepcopy(location) or vim.NIL,
  } }
end
local head_location = { path = 'src/file.rs', revision = followup, range = { start = { side = 'base', line = 2 }, ['end'] = { side = 'base', line = 3 } } }
local removed = response(4, 'recorded', head_location)
removed.code.path, removed.code.base_path, removed.code.location.path = 'removed.rs', 'removed.rs', 'removed.rs'
removed.code.head_content, removed.code.head_missing = '', true
write('historical_contexts.json', {
  ['reconstructed-base'] = response(5, 'reconstructed', nil, 'reconstructed-base'),
  ['reconstructed-head'] = response(6, 'recorded', head_location),
  renamed = response(9, 'recorded', head_location, nil, 'src/file.rs'),
  removed = removed,
  missing = { code = vim.NIL, anchor = view.comments[7].comment.comment.target.target, diff_hunk = '@@ -2 +2 @@\n-old\n+historical', message = 'Original commits unavailable; recorded hunk retained' },
})
write('state.json', view)
write('views.json', { [view.id] = view })
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
vim.fn.writefile({ '0.25' }, fixture .. '/delay_fetch_objects')
command('')
wait(function() return #requests('fetch_objects') == 1 end, 'initial Git preparation missing')
assert(#records('fetch_objects_complete.jsonl') == 0, 'fixture delay did not exercise preparation')
assert(not text():find('Todo (', 1, true), 'PR became usable before required objects were prepared')
wait(function() return kind() == 'home' and text():find('Todo (9)', 1, true) end, 'home missing after Git preparation')
assert(#requests('cache_contexts') == 0, 'obsolete SQLite context cache job scheduled')
vim.fn.delete(fixture .. '/delay_fetch_objects')
local function open(id)
  command(''); wait(function() return kind() == 'home' end, 'home missing')
  row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' and vim.b.buf_review.object == (id == 'reply' and 'recorded' or id) end, 'thread missing ' .. id)
end
local original_tab = api.nvim_get_current_tabpage()
local function pair(path, revision, historical, reconstructed, source_path, head_missing)
  local result = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' and meta.comment_role ~= 'original_base' then
      assert(meta.path == path and vim.deep_equal(meta.revision, revision), 'wrong diff pair: ' .. vim.inspect(meta))
      assert(meta.source_path == (meta.side == 'base' and (source_path or path) or path), 'wrong physical source path')
      assert((meta.historical == true) == historical, 'historical metadata missing or leaked to current code')
      assert(vim.wo[win].diff, 'reference side lost native diff mode')
      if historical then
        local label = meta.side == 'base' and 'Comment' or 'HEAD'
        assert(vim.wo[win].winbar:find(label, 1, true), 'reference side is not visibly labeled ' .. label)
        assert(vim.wo[win].winbar:find(revision[meta.side], 1, true), 'reference side revision is not visible')
        assert(vim.wo[win].winbar:find(meta.side == 'base' and (source_path or path) or path, 1, true), 'reference source path is not visible')
        if reconstructed and meta.side == 'base' then assert(vim.wo[win].winbar:lower():find('reconstructed', 1, true), 'reconstructed source looks exact') end
        if head_missing and meta.side == 'head' then
          assert(vim.wo[win].winbar:find('absent', 1, true), 'absent current file is not visibly labeled')
          assert(text(buf) == '', 'absent current file invented replacement content')
        end
      end
      result[meta.side] = { window = win, buffer = buf }
    end
  end
  assert(result.base and result.head, 'missing managed diff pair')
  assert(api.nvim_get_current_tabpage() == original_tab, 'comment opened a separate historical tab')
  assert(vim.deep_equal(read('state.json').comparison, current), 'historical display mutated pinned comparison')
  return result
end
local function ranges(side, expected)
  local ns = api.nvim_get_namespaces()['buf-review-comment-range']
  wait(function()
    local found = {}
    for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
      local buf = api.nvim_win_get_buf(win)
      local meta = vim.b[buf].buf_review
      if meta and meta.kind == 'diff' then
        for _, mark in ipairs(api.nvim_buf_get_extmarks(buf, ns, 0, -1, { details = true })) do
          assert(meta.side == side, 'historical range highlighted unverified side')
          local last = mark[4].end_row or mark[2]
          if last > mark[2] and (mark[4].end_col or 0) == 0 then last = last - 1 end
          for line = mark[2], last do found[line + 1] = true end
        end
      end
    end
    local rows = {}; for line in pairs(found) do rows[#rows + 1] = line end; table.sort(rows)
    return vim.deep_equal(rows, expected)
  end, 'wrong historical highlighted range')
end
open('recorded')
local sides = pair('src/file.rs', followup, true)
assert(text(sides.base.buffer):find('historical head', 1, true) and text(sides.head.buffer):find('changed', 1, true), 'follow-up buffers do not compare referenced source with latest PR head')
assert(api.nvim_win_get_cursor(sides.base.window)[1] == 2, 'recorded historical range did not position diff cursor')
ranges('base', { 2, 3 })
open('reply'); pair('src/file.rs', followup, true); ranges('base', { 2, 3 })
local before = #requests('actions')
require('buf_review').progress(true)
wait(function() return #records('action_results.jsonl') > before end, 'historical reply mark missing')
assert(requests('actions')[before + 1].actions[1].comment == 'reply', 'historical reply lost individual mark identity')

-- Historical references permit replies, but cannot create a root against the current pin.
api.nvim_set_current_win(pair('src/file.rs', followup, true).base.window)
local previous_notices = #notices
input('a')
assert(kind() == 'diff' and #notices > previous_notices, 'historical code opened current-pin comment composer')
previous_notices = #notices
input('Vja')
assert(kind() == 'diff' and #notices > previous_notices, 'historical visual selection bypassed comparison guard')
input('<Esc>')
for _, key in ipairs({ 's', 'u' }) do
  before, previous_notices = #requests('actions'), #notices
  input(key)
  assert(#requests('actions') == before and #notices > previous_notices, 'historical code staged current file progress via ' .. key)
end

-- File checklist remains pinned: a historical code pane never marks a current file viewed.
before = #requests('actions')
input(']f')
wait(function() return kind() == 'diff' and not vim.b.buf_review.historical end, 'file navigation did not restore pinned code')
assert(#requests('actions') == before, 'historical code incorrectly marked current file viewed')
pair('src/next.rs', current, false)
open('current'); ranges('base', { 2, 3 })
open('recorded-base'); sides = pair('src/file.rs', { base = old.base, head = latest }, true); ranges('base', { 2, 3 })
assert(text(sides.base.buffer):find('historical base', 1, true), 'deleted-line comment showed historical Head instead of recorded Base')
open('renamed'); pair('src/file.rs', followup, true, false, 'src/old.rs'); ranges('base', { 2, 3 })
open('removed'); pair('removed.rs', followup, true, false, nil, true); ranges('base', { 2, 3 })
local current_reads = #requests('code')
command('')
wait(function() return kind() == 'home' end, 'overview retained removed historical file')
assert(#requests('code') == current_reads, 'overview requested removed path in current comparison')
assert(vim.deep_equal(read('state.json').comparison, current), 'overview changed unchanged HEAD')
open('removed'); pair('removed.rs', followup, true, false, nil, true)
open('reconstructed-base'); sides = pair('src/file.rs', { base = 'reconstructed-base', head = latest }, true, true); ranges(nil, {})
assert(api.nvim_win_get_cursor(sides.base.window)[1] == 1, 'reconstructed Base guessed deleted-line position')
open('reconstructed-head'); sides = pair('src/file.rs', followup, true); ranges('base', { 2, 3 })
assert(api.nvim_win_get_cursor(sides.base.window)[1] == 2, 'original Head verified range did not position cursor')
open('missing')
local hunk_visible = false
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local buf = api.nvim_win_get_buf(win)
  local meta = vim.b[buf].buf_review
  if text(buf):find('@@ -2 +2 @@', 1, true) then hunk_visible = true end
  assert(not meta or meta.kind ~= 'diff', 'unavailable history retained misleading current/old diff')
end
assert(hunk_visible, 'missing history did not show retained hunk alongside thread')
assert(kind() == 'thread', 'fallback hid historical discussion')

-- Historical reads obey latest selection, modified-composer guards, and native diff cleanup.
vim.fn.writefile({ '0.25' }, fixture .. '/delay_historical_code')
command(''); wait(function() return kind() == 'home' end, 'home before race missing')
local historical_reads = #requests('historical_code')
row('Feedback recorded'); require('buf_review').enter()
wait(function() return #requests('historical_code') > historical_reads end, 'delayed historical request missing')
command('')
wait(function() return kind() == 'home' end, 'newer home choice missing')
ReviewTest.wait(400)
assert(kind() == 'home', 'stale historical callback replaced newer home')
historical_reads = #requests('historical_code')
row('Feedback recorded'); require('buf_review').enter()
wait(function() return #requests('historical_code') > historical_reads end, 'second delayed historical request missing')
row('Feedback current'); require('buf_review').enter()
wait(function() return kind() == 'thread' and vim.b.buf_review.object == 'current' end, 'newer current comment did not open')
ReviewTest.wait(400)
assert(kind() == 'thread' and vim.b.buf_review.object == 'current', 'stale historical callback replaced newer current comment')
ranges('base', { 2, 3 })

-- A refresh may discover a newer PR head without moving the session's comparison pin.
command(''); wait(function() return kind() == 'home' end, 'home before head refresh missing')
historical_reads = #requests('historical_code')
row('Feedback recorded'); require('buf_review').enter()
wait(function() return #requests('historical_code') > historical_reads end, 'historical request before head refresh missing')
local refreshed = read('state.json')
refreshed.review.read.pull_request.revision.head = string.rep('d', 40)
write('state.json', refreshed)
command('pull')
wait(function() return read('state.json').generation > refreshed.generation end, 'PR-head refresh missing')
wait(function() return #requests('historical_code') == historical_reads + 1 and kind() == 'thread' and vim.b.buf_review.object == 'recorded' end,
  'historical response changed selected comparison after PR-head refresh')
pair('src/file.rs', followup, true); ranges('base', { 2, 3 })
vim.fn.delete(fixture .. '/delay_historical_code')
for _ = 1, 10 do open('recorded'); open('current') end
local owned, hidden = 0, 0
for _, buf in ipairs(api.nvim_list_bufs()) do
  local meta = vim.b[buf].buf_review
  if meta and meta.kind == 'diff' then
    owned = owned + 1
    if #vim.fn.win_findbuf(buf) == 0 then
      hidden = hidden + 1
      assert(meta.comment_role == 'selected_head', 'hidden non-selected pane retained')
    end
  end
end
assert(owned == 3 and hidden == 1, 'historical/current switching accumulated diff buffers: ' .. owned .. '/' .. hidden)
input('a')
wait(function() return kind() == 'composer' end, 'reply composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved reply' })
local composer, reads = api.nvim_get_current_buf(), #requests('historical_code')
command('next comment')
ReviewTest.wait(80)
assert(api.nvim_get_current_buf() == composer and vim.bo.modified and #requests('historical_code') == reads, 'navigation overwrote unsaved reply')
assert(#requests('cache_contexts') == 0, 'historical reads scheduled obsolete byte-cache jobs')
print('Neovim historical diff panes passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
