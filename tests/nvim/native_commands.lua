local api = vim.api
local repository, fixture = vim.fn.getcwd(), vim.env.BUF_REVIEW_NATIVE_EXIT_FIXTURE or vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id)
  return { comment = { origin = 'read', comment = { id = id, body = id, author = { id = 'me', name = 'me' }, target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision, range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } } } } }, fetched = true, done = false, pending_delete = false, conflict = false, summary = false }
end
local initial = {
  id = 'native-session', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'native-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = { id = 'PR-1', title = 'Native commands', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'src/file.rs', viewed = false } }, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function read(name) return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/' .. name), '\n'), { luanil = { object = true, array = true } }) end
local function records(name, op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    local value = vim.json.decode(line)
    if not op or value.op == op then result[#result + 1] = value end
  end
  return result
end
local function requests(op) return records('requests.jsonl', op) end
local function state() return read('state.json') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(40)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(50)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local resets = 0
local function reset(ids, options)
  resets = resets + 1
  local snapshot = #requests('select') == 0 and vim.deepcopy(initial) or state()
  local title = 'Native commands · reset ' .. resets
  snapshot.review.read.pull_request.title = title
  if snapshot.pr then snapshot.pr.title = title end
  snapshot.generation = snapshot.generation + 1
  snapshot.comments, snapshot.todo, snapshot.done, snapshot.pending, snapshot.deleted_comments = {}, {}, {}, {}, {}
  snapshot.wip = nil
  snapshot.files = { { path = 'src/file.rs', viewed = false } }
  for _, id in ipairs(ids) do
    local item = comment(id)
    if options and options.drafts then item.comment.origin, item.fetched, item.destination = 'draft', false, 'standalone'
    else snapshot.todo[#snapshot.todo + 1] = id end
    snapshot.comments[#snapshot.comments + 1] = item
  end
  write('state.json', snapshot)
  write('routes.json', { entry = { route = 'batch', batch = snapshot.id, fresh_review = snapshot.review.id } })
  vim.cmd('Review')
  wait(function()
    return kind() == 'home' and api.nvim_buf_get_lines(0, 1, 2, false)[1] == '  ' .. title and text():find(ids[1], 1, true)
  end, 'reset did not load snapshot')
  vim.cmd('normal! zR')
end
local function batch_since(before, count)
  wait(function() return #requests('actions') > before end, 'native command did not flush local actions')
  local batches = requests('actions')
  assert(#batches == before + 1, 'native command spawned per-row writes: ' .. vim.inspect(batches))
  if count then assert(#batches[#batches].actions == count, 'wrong batched action count: ' .. vim.inspect(batches[#batches])) end
  return batches[#batches]
end
local function unique_comments(batch)
  local ids, seen = {}, {}
  for _, action in ipairs(batch.actions) do
    for _, id in ipairs(action.comments or { action.comment }) do
      if id and not seen[id] then ids[#ids + 1], seen[id] = id, true end
    end
  end
  return ids
end
local function no_stale()
  for _, result in ipairs(records('action_results.jsonl')) do
    assert(not (result.error and result.error:find('workspace changed', 1, true)), 'native actions reused stale generation')
  end
end

if vim.env.BUF_REVIEW_NATIVE_EXIT then
  reset({ 'Exit-alpha' })
  vim.fn.writefile({ '0.15' }, fixture .. '/delay_actions')
  row('Exit-alpha')
  input('s:qa!<CR>')
  error('Native editor exit did not finish')
end

-- Native :global resolves its own matches; redraw cannot change later targets.
reset({ 'Pick-alpha', 'Skip-beta', 'Pick-gamma' })
vim.fn.writefile({ '0.08' }, fixture .. '/delay_actions')
local before = #requests('actions')
vim.cmd('global/Pick-/normal s')
local sent = batch_since(before, 2)
wait(function() return #state().done == 2 end, 'global lost comments')
assert(sent.actions[1].comment == 'Pick-alpha' and sent.actions[2].comment == 'Pick-gamma' and not state().comments[2].done, 'global retargeted reordered rows')

-- :v and ranged :normal use native line selection, including structural rows.
reset({ 'Take-alpha', 'Keep-beta', 'Take-gamma' })
local first, last = row('Take-alpha'), row('Take-gamma')
before = #requests('actions')
vim.cmd(('%d,%dv/Keep-\\|@/normal s'):format(first, last))
sent = batch_since(before, 2)
wait(function() return #state().done == 2 end, 'vglobal lost comments')
assert(sent.actions[1].comment == 'Take-alpha' and sent.actions[2].comment == 'Take-gamma' and not state().comments[2].done, 'vglobal ignored inverse selection')
reset({ 'Range-alpha', 'Range-beta', 'Range-gamma' })
first, last = row('Range-alpha') - 1, row('Range-gamma')
before = #requests('actions')
vim.cmd(('%d,%dnormal s'):format(first, last))
sent = batch_since(before)
wait(function() return #state().done == 3 end, 'range lost comments')
assert(vim.deep_equal(unique_comments(sent), { 'Range-alpha', 'Range-beta', 'Range-gamma' }), 'range lost/reordered selected objects')

-- Native macro repetitions navigate the unchanged two-line comment objects.
reset({ 'Macro-alpha', 'Macro-beta', 'Macro-gamma' })
row('Macro-alpha')
vim.fn.setreg('q', 's2j')
before = #requests('actions')
vim.cmd('normal 3@q')
sent = batch_since(before, 3)
wait(function() return #state().done == 3 end, 'macro lost repeated actions')
assert(sent.actions[1].comment == 'Macro-alpha' and sent.actions[2].comment == 'Macro-beta' and sent.actions[3].comment == 'Macro-gamma', 'macro movement saw rebuilt rows')

-- Leaving Visual mode must not execute queued macro keys before action capture.
reset({ 'Visual-alpha', 'Visual-beta', 'Visual-gamma' })
api.nvim_win_set_cursor(0, { row('Visual-alpha') - 1, 0 })
before = #requests('actions')
input('Vjs2js')
sent = batch_since(before, 2)
wait(function() return #state().done == 2 end, 'visual mark consumed later macro input')
assert(vim.deep_equal(unique_comments(sent), { 'Visual-alpha', 'Visual-beta' }) and not state().comments[3].done, 'visual mark captured after later motion')
api.nvim_win_set_cursor(0, { row('Visual-alpha') - 1, 0 })
before = #requests('actions')
input('Vju2ju')
sent = batch_since(before, 2)
wait(function() return #state().done == 0 end, 'visual undo consumed later macro input')
assert(vim.deep_equal(unique_comments(sent), { 'Visual-alpha', 'Visual-beta' }), 'visual undo captured after later motion')

-- Semantic undo executes after earlier queued intent, never reads stale Lua flags.
reset({ 'Intent-alpha', 'Intent-beta' })
row('Intent-alpha')
before = #requests('actions')
vim.cmd('normal su')
sent = batch_since(before, 2)
wait(function() return state().generation >= sent.generation + 2 end, 'mark then undo did not finish')
assert(sent.actions[1].action == 'reaction' and sent.actions[2].action == 'undo' and vim.deep_equal(sent.actions[2].comments, { 'Intent-alpha' }) and not state().comments[1].done, 'undo misclassified earlier mark intent')
reset({ 'Delete-alpha', 'Delete-beta' })
row('Delete-alpha')
before = #requests('actions')
vim.cmd('normal ddu')
sent = batch_since(before, 2)
wait(function() return notices[#notices]:find('immutable', 1, true) end, 'published deletion was not rejected')
assert(sent.actions[1].action == 'delete_comments' and sent.actions[2].action == 'undo'
  and not state().comments[1].pending_delete and #state().comments == 2,
  'immutable comment deletion changed saved state')
reset({ 'Undo-twice' })
local twice = state()
twice.comments[1].done, twice.todo, twice.done = true, {}, { 'Undo-twice' }
write('state.json', twice)
vim.cmd('Review')
wait(function() return text():find('Done (1)', 1, true) end, 'undo fixture did not load Done state')
row('Undo-twice')
before = #requests('actions')
vim.cmd('normal uu')
sent = batch_since(before, 2)
wait(function() return state().generation >= sent.generation + 2 end, 'repeated undo did not finish')
assert(sent.actions[1].action == 'undo' and sent.actions[2].action == 'undo' and not state().comments[1].done, 'undo intents coalesced across contextual state change')

-- Later actions validate against restored state, rather than the stale reader.
for _, keys in ipairs({ 'us', 'udd' }) do
  reset({ 'Restore-intent' })
  local deleted = state()
  deleted.comments[1].pending_delete = true
  deleted.pending = { { id = 'restore-intent-delete', change = { kind = 'delete_comment', comment = 'Restore-intent' } } }
  write('state.json', deleted)
  vim.cmd('Review')
  wait(function() return text():find('pending deletion', 1, true) end, 'pending deletion did not load')
  row('Restore-intent')
  before = #requests('actions')
  vim.cmd('normal ' .. keys)
  sent = batch_since(before, 2)
  wait(function() return not state().comments[1].pending_delete end, 'restore followed by action did not finish')
  assert(sent.actions[1].action == 'undo', 'restore was resolved outside Rust')
  if keys == 'us' then assert(state().comments[1].done and not state().comments[1].pending_delete, 'mark used stale deletion flag')
  else assert(not state().comments[1].pending_delete and #state().comments == 1, 'immutable restored comment was deleted') end
end

-- A queued cancellation removes a local child before the later parent delete.
reset({ 'Parent-intent', 'Child-intent' }, { drafts = true })
local dependency = state()
dependency.comments[2].comment.comment.target = { kind = 'reply', target = 'Parent-intent' }
write('state.json', dependency)
vim.cmd('Review')
wait(function() return kind() == 'home' and text():find('Child-intent', 1, true) end, 'dependency fixture missing')
local parent_line = row('Parent-intent')
row('Child-intent')
before = #requests('actions')
vim.cmd('normal u')
api.nvim_win_set_cursor(0, { parent_line, 0 })
vim.cmd('normal dd')
sent = batch_since(before, 2)
wait(function() return #state().comments == 0 end, 'parent delete read stale local child')
assert(sent.actions[1].action == 'undo' and sent.actions[2].action == 'delete_comments', 'dependent actions reordered')

-- Motions/counts/dot remain native; object identity deduplicates repeated targets.
reset({ 'Motion-alpha', 'Motion-beta', 'Motion-gamma' }, { drafts = true })
api.nvim_win_set_cursor(0, { row('Motion-alpha') - 1, 0 })
before = #requests('actions')
vim.cmd('normal 2dd2j.')
sent = batch_since(before, 2)
wait(function() return #state().comments == 1 end, 'count/dot lost delete')
assert(vim.deep_equal(unique_comments(sent), { 'Motion-alpha', 'Motion-beta' }) and state().comments[1].comment.comment.id == 'Motion-gamma', 'dot/count changed selected objects')
reset({ 'Duplicate-alpha', 'Duplicate-beta' }, { drafts = true })
before = #requests('actions')
vim.cmd('global/Duplicate-/normal dd')
sent = batch_since(before, 2)
wait(function() return #state().comments == 0 end, 'global dd did not complete')
assert(vim.deep_equal(unique_comments(sent), { 'Duplicate-alpha', 'Duplicate-beta' }), 'global delete retargeted')
reset({ 'Header-alpha', 'Header-beta' }, { drafts = true })
first, last = row('Header-alpha') - 1, row('Header-beta')
before = #requests('actions')
vim.cmd(('%d,%dnormal x'):format(first, last))
sent = batch_since(before)
wait(function() return #state().comments == 0 end, 'ranged x did not complete')
assert(vim.deep_equal(unique_comments(sent), { 'Header-alpha', 'Header-beta' }), 'ranged x lost/reordered selected objects')

-- Mixed ranged actions ignore headings and apply each comment/file once.
reset({ 'Mixed-alpha', 'Mixed-beta' })
local before_structural_notices = #notices
before = #requests('actions')
vim.cmd('1,$normal s')
sent = batch_since(before)
wait(function() return #state().done == 2 and state().files[1].viewed end, 'mixed range missed file/comment')
before = #requests('actions')
vim.cmd('1,$normal u')
sent = batch_since(before)
wait(function() return #state().done == 0 and not state().files[1].viewed end, 'mixed undo missed objects')
for index = before_structural_notices + 1, #notices do
  assert(not notices[index]:find('Select a fetched comment or compared file', 1, true), 'native range emitted selection errors for structural rows')
end

-- Backend partial errors commit prefix only; no subsequent queued action runs.
reset({ 'Failure-alpha', 'Failure-beta', 'Failure-gamma' })
vim.fn.writefile({ 'Failure-beta' }, fixture .. '/fail_reaction_id')
before = #requests('actions')
vim.cmd('global/Failure-/normal s')
batch_since(before, 3)
wait(function() return notices[#notices]:find('Fixture reaction conflict', 1, true) end, 'failure diagnostic missing')
local result = records('action_results.jsonl')
assert(result[#result].applied == 1 and result[#result].failed_index == 1 and state().comments[1].done and not state().comments[2].done and not state().comments[3].done, 'queued suffix survived failure')
vim.fn.delete(fixture .. '/fail_reaction_id')

-- Failure also cancels input queued while an earlier write process is running.
reset({ 'Running-alpha', 'Running-beta' })
vim.fn.writefile({ 'Running-alpha' }, fixture .. '/fail_reaction_id')
vim.fn.writefile({ '0.3' }, fixture .. '/delay_actions')
before = #requests('actions')
local before_results = #records('action_results.jsonl')
row('Running-alpha'); input('s')
wait(function() return #requests('actions') > before end, 'first process did not start')
row('Running-beta'); input('s')
wait(function() return #records('action_results.jsonl') > before_results end, 'running action did not fail')
ReviewTest.wait(150)
assert(#requests('actions') == before + 1 and not state().comments[1].done and not state().comments[2].done, 'queued next process ran after earlier failure')
vim.fn.delete(fixture .. '/fail_reaction_id')
vim.fn.writefile({ '0.08' }, fixture .. '/delay_actions')

-- Discussion bodies/headers map back to one object; dividers carry no action.
reset({ 'Thread-root' })
local threaded = state()
threaded.comments[1].comment.comment.body = 'Thread-match first\n\nThread-match second'
write('state.json', threaded)
vim.cmd('Review')
wait(function() return kind() == 'home' and text():find('Thread-match first', 1, true) end, 'thread snapshot missing')
row('Thread-match first'); require('buf_review').enter()
wait(function() return kind() == 'thread' end, 'discussion did not open')
before = #requests('actions')
vim.cmd('global/Thread-match/normal s')
sent = batch_since(before)
wait(function() return state().comments[1].done end, 'discussion global did not mark object')
assert(sent.actions[1].comment == 'Thread-root', 'discussion selected wrong comment')
before = #requests('actions')
vim.cmd('1,$normal u')
batch_since(before)
wait(function() return not state().comments[1].done end, 'discussion range undo lost object')

-- Immutable diff rows all refer to one file; native ranges deduplicate it.
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
  if marker and marker.kind == 'diff' and marker.side == 'head' then api.nvim_set_current_win(win); break end
end
assert(kind() == 'diff', 'diff pane missing')
before = #requests('actions')
vim.cmd('1,$normal s')
sent = batch_since(before)
wait(function() return state().files[1].viewed end, 'diff range did not mark file')
assert(sent.actions[1].action == 'viewed' and sent.actions[1].path == 'src/file.rs', 'diff range marked a background comment')
before = #requests('actions')
vim.cmd('global/one\\|changed/normal u')
batch_since(before)
wait(function() return not state().files[1].viewed end, 'diff global undo lost file')

-- Async refresh callbacks may arrive within a native command; reader stays fixed.
reset({ 'Refresh-alpha', 'Refresh-beta', 'Refresh-gamma' })
vim.fn.writefile({ '0.02' }, fixture .. '/delay_refresh')
local stable, pauses = text(), 0
vim.keymap.set('n', 'T', function()
  pauses = pauses + 1
  local entry_state = vim.fn.state()
  vim.wait(120)
  assert(text() == stable, 'callback rebuilt rows inside native command: ' .. entry_state .. ' -> ' .. vim.fn.state() .. '\nBEFORE\n' .. stable .. '\nAFTER\n' .. text())
end, { buffer = true })
vim.cmd('Review refresh')
before = #requests('actions')
vim.cmd('global/Refresh-/normal sT')
wait(function() return #state().done == 3 end, 'refresh overtook queued local actions')
local refreshed, refreshed_ids = requests('actions'), {}
for index = before + 1, #refreshed do
  for _, id in ipairs(unique_comments(refreshed[index])) do refreshed_ids[#refreshed_ids + 1] = id end
end
assert(vim.deep_equal(refreshed_ids, { 'Refresh-alpha', 'Refresh-beta', 'Refresh-gamma' }), 'queued refresh changed local action order')
assert(pauses == 3, 'native pause mapping did not run for each matched row')
vim.keymap.del('n', 'T', { buffer = true })
vim.fn.delete(fixture .. '/delay_refresh')

-- Switching sessions flushes captured old-session writes before selecting new state.
reset({ 'Session-alpha', 'Session-beta' })
local old = state()
local other = vim.deepcopy(initial)
other.id, other.locator, other.review.read.pull_request.id = 'other-session', 'https://github.com/acme/repo/pull/2', 'PR-2'
other.comments, other.todo = { comment('Session-other') }, { 'Session-other' }
write('views.json', { [old.id] = old, [other.id] = other })
write('routes.json', { [other.locator] = { route = 'batch', batch = other.id, fresh_review = other.review.id } })
write('pull_requests.json', { { number = 2, title = 'Switch target', url = other.locator, author = 'me', assignees = {} } })
row('Session-alpha')
before = #requests('actions')
input('s:Review prs<CR>')
wait(function() return kind() == 'prs' end, 'PR browser did not open after pending write')
row('Switch target'); require('buf_review').enter()
wait(function() return kind() == 'home' and text():find('Session-other', 1, true) end, 'session switch failed')
sent = batch_since(before, 1)
local saved = read('views.json')
assert(sent.workspace == old.id and saved[old.id].comments[1].done and not saved[other.id].comments[1].done, 'queued write retargeted new session')

-- :normal! bypasses plugin mappings; immutable reader text remains intact.
row('Session-other')
before = #requests('actions')
vim.cmd('silent! normal! s')
assert(#requests('actions') == before and not state().comments[1].done, 'normal! unexpectedly invoked plugin action')
no_stale()

-- Real editor exit flushes captured local writes, without publishing anything.
local exit_fixture = vim.fn.tempname()
local exit_command = ('lua assert(loadfile(%q))(%q)'):format(repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/native_commands.lua')
local exited = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c', exit_command }, {
  env = { BUF_REVIEW_NATIVE_EXIT = '1', BUF_REVIEW_NATIVE_EXIT_FIXTURE = exit_fixture }, text = true,
}):wait()
assert(exited.code == 0, 'queued exit child failed: ' .. exited.stderr)
local exit_state = vim.json.decode(table.concat(vim.fn.readfile(exit_fixture .. '/state.json'), '\n'))
assert(exit_state.comments[1].done and exit_state.generation == 2, 'VimLeavePre lost queued write')
local exit_batches = 0
for _, line in ipairs(vim.fn.readfile(exit_fixture .. '/requests.jsonl')) do
  local request = vim.json.decode(line)
  assert(request.op ~= 'publish' and request.op ~= 'submit', 'editor exit published local work')
  if request.op == 'actions' then exit_batches = exit_batches + 1 end
end
assert(exit_batches == 1, 'editor exit duplicated local writes')
vim.fn.delete(exit_fixture, 'rf')
print('Neovim native command composition passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
