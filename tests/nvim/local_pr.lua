local api = vim.api
local repository, sandbox = vim.fn.getcwd(), vim.fn.tempname()
local project, fixture, bin = sandbox .. '/project', sandbox .. '/fixture', sandbox .. '/bin'
vim.fn.mkdir(project, 'p')
vim.fn.mkdir(fixture, 'p')
vim.fn.mkdir(bin, 'p')
local function git(...)
  local result = vim.system(vim.list_extend({ 'git' }, { ... }), { cwd = project, text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
git('init', '-b', 'master')
git('config', 'user.name', 'Review fixture')
git('config', 'user.email', 'review@example.invalid')
vim.fn.writefile({ 'one', 'two', 'three' }, project .. '/app.txt')
git('add', 'app.txt')
git('commit', '-m', 'Base')
local base = git('rev-parse', 'HEAD')
git('switch', '-c', 'feature')
vim.fn.writefile({ 'one', 'changed', 'three' }, project .. '/app.txt')
git('add', 'app.txt')
git('commit', '-m', 'Feature')
local head = git('rev-parse', 'HEAD')
git('remote', 'add', 'origin', 'https://github.com/acme/repo.git')
vim.fn.writefile({ 'unstaged work remains' }, project .. '/app.txt')
vim.fn.writefile({ 'untracked work remains' }, project .. '/untracked.txt')
local original_status = git('status', '--porcelain')
local gh_log = sandbox .. '/gh.log'
vim.fn.writefile({ '#!/bin/sh', 'echo "$@" >> "$BUF_REVIEW_GH_LOG"', 'exit 1' }, bin .. '/gh')
assert(vim.fn.setfperm(bin .. '/gh', 'rwxr-xr-x') == 1)
vim.env.BUF_REVIEW_GH_LOG = gh_log
vim.env.PATH = bin .. ':' .. vim.env.PATH
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd.cd(project)
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')) end
local function requests(op)
  local found = {}
  local lines = vim.fn.filereadable(fixture .. '/requests.jsonl') == 1 and vim.fn.readfile(fixture .. '/requests.jsonl') or {}
  vim.list_extend(lines, vim.fn.filereadable(fixture .. '/action_attempts.jsonl') == 1 and vim.fn.readfile(fixture .. '/action_attempts.jsonl') or {})
  for _, line in ipairs(lines) do
    local request = vim.json.decode(line)
    if request.op == op then found[#found + 1] = request end
  end
  return found
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(40)
end
local function home()
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'Overview missing')
end
local revision = { base = base, head = head }
local branches = {
  source = { repository = 'https://github.com/acme/repo', branch = 'feature' },
  sink = { repository = 'https://github.com/acme/repo', branch = 'master' },
}
local initial = {
  id = 'local-batch', generation = 0, comparison = revision,
  pr = { id = 'logical-pr', revision = 0, source = { kind = 'local_intent', branches = branches }, title = '', body = '' },
  review = { id = 'local-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'logical-pr', title = '', body = '', revision = revision, source = branches.source,
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'app.txt', viewed = false, change = 'modified', stats = { additions = 1, deletions = 1 } } },
  todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', initial)
local routes = { ['pr:master'] = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } }
write('routes.json', routes)

-- Branch-pair command uses Rust routing; malformed invocations never start it.
vim.cmd('Review pr')
vim.cmd('Review pr master extra')
assert(#requests('pr') == 0 and notices[#notices]:find('Use :Review pr BASE', 1, true))
vim.cmd('Review pr master')
wait(function() return kind() == 'home' end, 'Local PR did not open')
assert(requests('pr')[1].base == 'master', 'Command dropped target branch')
assert(text():find('feature → master [local]', 1, true) and text():find('(untitled)', 1, true))
assert(text():find('local committed snapshot', 1, true) and text():find('PR creation: pending', 1, true))
assert(not text():find('Checkout:', 1, true), 'Local intent exposed remote checkout')
assert(vim.fn.filereadable(gh_log) == 0, 'Local visit invoked gh')
vim.cmd('Review log')
wait(function() return kind() == 'log' end, 'Local log missing')
assert(api.nvim_buf_get_lines(0, 1, 2, false)[1] == 'feature → master [local]', 'Local log lost PR identity')
home()
local before = #notices
vim.cmd('Review checkout')
assert(#notices > before and notices[#notices]:find('Local PR intent', 1, true))

-- Native Markdown writes update only canonical PR metadata; snapshots stay pinned.
api.nvim_win_set_cursor(0, { 2, 0 })
input('<CR>')
wait(function() return kind() == 'composer' end, 'Title did not open composer')
assert(require('buf_review.editor').get().kind == 'pr' and vim.bo.filetype == 'markdown')
api.nvim_buf_set_lines(0, 0, -1, false, { 'PR title', '', 'Body paragraph', '', '- Preserve Markdown' })
vim.cmd.write()
assert(not vim.bo.modified and state().pr.title == 'PR title' and state().pr.body == 'Body paragraph\n\n- Preserve Markdown')
assert(state().review.read.pull_request.title == '' and vim.deep_equal(state().comparison, revision), 'Metadata save rewrote pinned snapshot')
local saved = requests('pr_details')
assert(#saved == 1 and saved[1].revision == 0 and #requests('publish') == 0 and #requests('submit') == 0)
api.nvim_buf_set_lines(0, 0, -1, false, { 'Updated title', 'Body without separator' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, ':wq kept metadata composer open')
assert(state().pr.title == 'Updated title' and state().pr.body == 'Body without separator')
assert(requests('pr_details')[2].revision == 1 and #requests('publish') == 0 and #requests('submit') == 0, ':wq published local PR')
home()
assert(api.nvim_buf_get_lines(0, 1, 2, false)[1] == '  Updated title', 'Overview used historical title')

-- Shared PR metadata has its own CAS; concurrent edits retain composer text.
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Unsaved competing title', '', 'Retain this text' })
local external = state()
external.pr.title, external.pr.revision, external.generation = 'Another session title', external.pr.revision + 1, external.generation + 1
write('state.json', external)
local ok = pcall(vim.cmd, 'write')
assert(not ok and vim.bo.modified and text():find('Unsaved competing title', 1, true), 'Concurrent metadata edit lost unsaved text')
assert(state().pr.title == 'Another session title', 'Stale composer overwrote metadata')
input(':q!<CR>')
home()
assert(api.nvim_buf_get_lines(0, 1, 2, false)[1] == '  Another session title')
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
api.nvim_buf_set_lines(0, 0, -1, false, { 'PR revision conflict', '', 'Still unsaved' })
external = state()
external.pr.title, external.pr.revision = 'Shared PR update', external.pr.revision + 1
write('state.json', external)
ok = pcall(vim.cmd, 'write')
assert(not ok and vim.bo.modified and text():find('Still unsaved', 1, true), 'PR CAS failure discarded composer')
assert(state().pr.title == 'Shared PR update', 'Workspace generation bypassed PR CAS')
input(':q!<CR>')
home()

-- Empty metadata remains legal locally; creation validates it at publication.
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
api.nvim_buf_set_lines(0, 0, -1, false, { '' })
vim.cmd.write()
assert(state().pr.title == '' and state().pr.body == '' and not vim.bo.modified)
api.nvim_buf_set_lines(0, 0, -1, false, { 'Ready for GitHub', '', 'Description' })
input(':wq<CR>')
home()

-- Comment composers and immutable panes work before GitHub PR creation.
row('app.txt'); input('<CR>')
wait(function() return kind() == 'diff' end, 'Local committed diff missing')
assert(text() == 'one\nchanged\nthree', 'Viewer used dirty working-file content')
api.nvim_win_set_cursor(0, { 2, 0 }); input('a')
wait(function() return kind() == 'composer' end, 'Local comment composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Local draft survives binding' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'Comment :wq did not close')
local draft = state().comments[1].comment.comment
assert(draft.body == 'Local draft survives binding' and vim.deep_equal(draft.target.target.revision, revision))

-- Existing create/resume/choose routing also serves local intents.
home()
local current = state()
local other = vim.deepcopy(current)
other.id = 'local-batch-two'
write('views.json', { [current.id] = current, [other.id] = other })
local empty = { comments = 0, edits = 0, deletions = 0, reactions = 0, files = 0, conclusions = 0, total = 0 }
local stats = { status = 'draft', unfinished = true, pending = empty, published = empty, conflicts = 0 }
local groups = { { pr = { id = current.pr.id, source = current.pr.source, title = current.pr.title }, sessions = {
  { id = current.id, reviewer = current.review.reviewer, active = true, stats = stats },
  { id = other.id, reviewer = other.review.reviewer, active = false, stats = stats },
} } }
write('sessions.json', { groups = groups })
routes['pr:master'] = { route = 'sessions', sessions = { pr = current.pr.id, groups = groups } }
write('routes.json', routes)
local selected_before = #requests('select')
vim.cmd('Review pr master')
wait(function() return kind() == 'sessions' end, 'Ambiguous local sessions skipped browser')
assert(text():find('feature → master [local]', 1, true) and #requests('select') == selected_before)
row(other.id); input('<CR>')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == other.id end, 'Local session selection missing')
vim.cmd('Review sessions')
wait(function() return kind() == 'sessions' end, 'Local session browser missing')
row(current.id); input('<CR>')
wait(function() return kind() == 'home' and vim.b.buf_review.workspace == current.id end, 'Local session resume missing')

-- Publication may bind canonical identity without replacing review/session IDs.
local remote = vim.deepcopy(state().pr)
remote.source, remote.revision = { kind = 'remote', locator = 'https://github.com/acme/repo/pull/9', branches = branches }, remote.revision + 1
write('published_pr.json', remote)
vim.cmd('Review publish')
wait(function() return text():find('https://github.com/acme/repo/pull/9', 1, true) end, 'Published binding missing')
assert(state().id == initial.id and state().pr.id == initial.pr.id and state().comments[1].comment.comment.id == draft.id, 'Remote binding replaced local identities')
assert(not text():find('PR creation: pending', 1, true) and text():find('Checkout:', 1, true))
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
assert(kind() == 'home', 'Existing remote PR title became writable')
assert(git('status', '--porcelain') == original_status and git('rev-parse', 'HEAD') == head, 'Review command mutated local Git changes')
assert(vim.fn.filereadable(gh_log) == 0, 'UI invoked provider outside Rust')

print('Neovim local PR intent workflows passed')
vim.cmd('qa!')
