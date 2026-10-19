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
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n'), { luanil = { object = true, array = true } }) end
local function records(name, op)
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(fixture .. '/' .. name) == 1 and vim.fn.readfile(fixture .. '/' .. name) or {}) do
    local value = vim.json.decode(line)
    if not op or value.op == op then result[#result + 1] = value end
  end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function command(value) vim.cmd('Review ' .. value) end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return line end
  end
  error('Missing row ' .. needle)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local function comment(id, local_draft)
  return { comment = { origin = local_draft and 'draft' or 'read', comment = { id = id, body = 'Feedback ' .. id,
    author = { id = 'me', name = 'me' }, target = { kind = 'diff', target = { path = 'src/file.rs', revision = revision,
      range = { start = { side = 'head', line = 2 }, ['end'] = { side = 'head', line = 2 } } } } } },
    fetched = not local_draft, draft = local_draft, destination = local_draft and 'standalone' or nil,
    done = false, pending_delete = false, conflict = false, summary = false }
end
local pending = comment('pending-imported'); pending.draft = true
local reply = comment('legacy-reply', true); reply.comment.comment.target = { kind = 'reply', target = 'legacy-root' }
local initial = {
  id = 'review-queue', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'review-queue-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', title = 'Review queue', revision = revision,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = { comment('published'), pending, comment('legacy-root', true), reply, comment('current', true), comment('range', true) },
  files = { { path = 'src/file.rs', viewed = false } }, todo = { 'published' }, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
local function home()
  command(''); wait(function() return kind() == 'home' and text():find('Feedback pending-imported', 1, true) end, 'Review queue home missing')
end
home()
local pending_line = row('Feedback pending-imported')
assert(api.nvim_buf_get_lines(0, pending_line - 2, pending_line - 1, false)[1]:find('draft', 1, true), 'Imported pending comment lacks draft label')
assert(vim.deep_equal(state().todo, { 'published' }), 'Imported pending draft entered checklist')

-- Published and imported pending comments remain readers, even when owned.
for _, id in ipairs({ 'published', 'pending-imported' }) do
  home(); row('Feedback ' .. id); require('buf_review').enter()
  wait(function() return kind() == 'thread' end, 'Read-only comment did not open discussion')
  row('Feedback ' .. id)
  local before = #records('requests.jsonl', 'actions')
  input('<CR>'); command('edit'); command('delete')
  assert(kind() == 'thread' and not require('buf_review.editor').get() and vim.bo.readonly,
    'Read-only comment entered mutable composer')
  assert(#records('requests.jsonl', 'actions') == before and notices[#notices]:find('immutable', 1, true),
    'Explicit published mutation reached writer')
  input('dd')
  wait(function() return #records('action_results.jsonl') == before + 1 end, 'Published operator validation did not complete')
  assert(records('action_results.jsonl')[before + 1].error:find('immutable', 1, true), 'Published delete operator escaped core validation')
  for _, item in ipairs(state().comments) do
    if item.comment.comment.id == id then assert(not item.pending_delete and item.comment.comment.body == 'Feedback ' .. id, 'Published comment mutated') end
  end
end

-- Current, explicit range and Visual range move exactly their selected drafts.
home(); row('Feedback current'); command('move')
wait(function() return state().comments[5].destination == 'review' end, 'Current draft did not move')
local first, last = row('Feedback range') - 1, row('Feedback range')
vim.cmd(('%d,%dReview move'):format(first, last))
wait(function() return state().comments[6].destination == 'review' end, 'Ranged draft did not move')
api.nvim_win_set_cursor(0, { row('Feedback legacy-root') - 1, 0 })
input('V3j:Review move<CR>')
wait(function() return state().comments[3].destination == 'review' and state().comments[4].destination == 'review' end,
  'Visual root/reply selection did not move together')
local moves = records('action_attempts.jsonl', 'move')
assert(#moves == 3 and vim.deep_equal(moves[1].comments, { 'current' }) and vim.deep_equal(moves[2].comments, { 'range' })
  and vim.deep_equal(moves[3].comments, { 'legacy-root', 'legacy-reply' }), 'Move duplicated rows or used hidden selection')
for index = 3, 6 do
  assert(state().comments[index].comment.comment.id == initial.comments[index].comment.comment.id
    and vim.deep_equal(state().comments[index].comment.comment, initial.comments[index].comment.comment), 'Move changed identity, body or anchor')
end

-- A normal new inline comment enters the Review queue without publication.
home(); row('Unviewed files'); api.nvim_win_set_cursor(0, { api.nvim_win_get_cursor(0)[1] + 1, 0 }); input('<CR>')
wait(function() return kind() == 'diff' end, 'Inline source missing')
input('a'); assert(kind() == 'composer', 'Inline composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'New queued inline feedback' }); input(':wq<CR>')
assert(state().comments[#state().comments].destination == 'review' and state().comments[#state().comments].draft,
  'New inline comment did not enter Review queue')

-- Conclusion closes only enqueue locally; publish alone executes the queue.
vim.ui.select = function(_, _, callback) callback('comment') end
command('conclude'); wait(function() return kind() == 'composer' end, 'Conclusion composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Queued review summary' }); input(':wq<CR>')
assert(kind() ~= 'composer' and state().wip.conclusion.body == 'Queued review summary', 'Conclusion write-and-close lost queued summary')
assert(#records('action_attempts.jsonl', 'publish_review') == 1 and #records('requests.jsonl', 'publish') == 0
  and #records('requests.jsonl', 'submit') == 0, 'Local move/save/conclude performed provider writes')
-- Canceling the picker preserves queued intent; Draft explicitly removes it.
local queued, local_actions = state(), #records('requests.jsonl', 'actions')
vim.ui.select = function(items, _, callback)
  assert(vim.tbl_contains(items, 'draft'), 'Conclusion picker omitted Draft')
  callback(nil)
end
command('conclude')
assert(vim.deep_equal(state(), queued) and #records('requests.jsonl', 'actions') == local_actions
  and kind() ~= 'composer', 'Canceling conclusion picker changed queued intent')
vim.ui.select = function(_, _, callback) callback('draft') end
command('conclude')
wait(function() return state().wip and not state().wip.conclusion end, 'Draft did not remove queued conclusion')
local canceled = records('action_attempts.jsonl', 'publish_review')
assert(#canceled == 2 and canceled[2].conclusion == vim.NIL and kind() ~= 'composer'
  and #records('requests.jsonl', 'publish') == 0, 'Draft opened composer, lost null action or published remotely')
assert(not vim.tbl_contains(state().pending, function(operation) return operation.change.kind == 'set_review_conclusion' end, { predicate = true }),
  'Draft retained queued conclusion marker')
command('publish')
assert(#records('requests.jsonl', 'publish') == 1 and not (state().wip and state().wip.conclusion), 'Draft publication invented a review conclusion')
local sent = state()
local attempted = vim.system({ vim.g.buf_review_executable, 'workspace' }, {
  cwd = repository, stdin = vim.json.encode({ op = 'actions', workspace = sent.id, generation = sent.generation,
    actions = { { action = 'save', comment = 'legacy-root', body = 'Attempted sent edit' } } }), text = true,
}):wait()
local refused = vim.json.decode(attempted.stdout)
assert(attempted.code == 0 and refused.error:find('immutable', 1, true)
  and state().comments[3].comment.comment.body == sent.comments[3].comment.comment.body,
  'Draft publication left sent receipts editable')

-- A later explicit conclusion can still be queued and published normally.
vim.ui.select = function(_, _, callback) callback('comment') end
command('conclude'); wait(function() return kind() == 'composer' end, 'Requeued conclusion composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Final queued summary' }); input(':wq<CR>')
assert(#records('requests.jsonl', 'publish') == 1 and state().wip.conclusion.body == 'Final queued summary', 'Requeued conclusion published on close')
command('publish')
assert(#records('requests.jsonl', 'publish') == 2 and #records('requests.jsonl', 'submit') == 0
  and not state().wip and #state().pending == 0, 'Publish did not execute queued conclusion and changes')
for _, item in ipairs(state().comments) do assert(not item.destination, 'Published queue retained unsent creation') end
print('Neovim explicit review queue workflows passed')
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
