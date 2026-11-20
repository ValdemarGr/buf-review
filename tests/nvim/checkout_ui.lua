local api = vim.api
local repository, sandbox = vim.fn.getcwd(), vim.fn.tempname()
local project, fixture, bin = sandbox .. '/project', sandbox .. '/project/.review/fixture', sandbox .. '/bin'
vim.fn.mkdir(fixture, 'p')
vim.fn.mkdir(bin, 'p')
local function system(arguments, root)
  return vim.system(arguments, { cwd = root or project, text = true }):wait()
end
local function git(...)
  local result = system(vim.list_extend({ 'git' }, { ... }))
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
local function disk(path, lines) vim.fn.writefile(lines, project .. '/' .. path) end
system({ 'git', 'init', '-b', 'main' })
git('config', 'user.name', 'Review fixture')
git('config', 'user.email', 'review@example.invalid')
disk('app.txt', { 'main source' })
disk('notes.txt', { 'ordinary notes' })
git('add', 'app.txt', 'notes.txt')
git('commit', '-m', 'Base')
local base = git('rev-parse', 'HEAD')
git('switch', '-c', 'feature')
disk('app.txt', { 'feature source' })
git('add', 'app.txt')
git('commit', '-m', 'Feature')
local head = git('rev-parse', 'HEAD')
git('remote', 'add', 'origin', 'https://github.com/acme/repo.git')
git('config', 'branch.feature.remote', 'origin')
git('config', 'branch.feature.merge', 'refs/heads/feature')
git('switch', 'main')
vim.env.GIT_ALLOW_PROTOCOL = 'file'
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.env.BUF_REVIEW_CHECKOUT_GH_LOG = sandbox .. '/gh.jsonl'
vim.env.PATH = bin .. ':' .. vim.env.PATH
vim.fn.writefile({
  '#!/usr/bin/env python3',
  'import json, os, pathlib, subprocess, sys',
  'with open(os.environ["BUF_REVIEW_CHECKOUT_GH_LOG"], "a") as stream:',
  '    stream.write(json.dumps({"args":sys.argv[1:],"cwd":os.getcwd()}) + "\\n")',
  'if pathlib.Path(os.environ["BUF_REVIEW_FIXTURE"], "fail_checkout").exists():',
  '    print("Fixture checkout failed", file=sys.stderr)',
  '    sys.exit(1)',
  'assert sys.argv[1:] == ["pr", "checkout", "https://github.com/acme/repo/pull/1"]',
  'sys.exit(subprocess.run(["git", "switch", "feature"]).returncode)',
}, bin .. '/gh')
assert(vim.fn.setfperm(bin .. '/gh', 'rwxr-xr-x') == 1)
vim.g.buf_review_executable = repository .. '/tests/nvim/fixture_pr.py'
vim.opt.runtimepath:append(repository)
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd.cd(project)
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local revision = { base = base, head = head }
local initial = {
  id = 'checkout-ui', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'checkout-snapshot', reviewer = { id = 'me', name = 'me' }, read = { pull_request = {
    id = 'PR-1', url = 'https://github.com/acme/repo/pull/1', title = 'Checkout fixture', revision = revision,
    source = { repository = 'https://github.com/acme/repo', branch = 'feature' },
  }, files = {}, comments = {}, reactions = {}, reviews = {} } },
  comments = {}, files = { { path = 'app.txt', viewed = false } }, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
local function state() return vim.json.decode(table.concat(vim.fn.readfile(fixture .. '/state.json'), '\n')) end
local function gh_calls()
  local result = {}
  for _, line in ipairs(vim.fn.filereadable(vim.env.BUF_REVIEW_CHECKOUT_GH_LOG) == 1 and vim.fn.readfile(vim.env.BUF_REVIEW_CHECKOUT_GH_LOG) or {}) do result[#result + 1] = vim.json.decode(line) end
  return result
end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function checkout_row()
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:match('^Checkout:') then api.nvim_win_set_cursor(0, { line, 0 }); return value end
  end
end
local function pinned()
  local current = state()
  assert(current.id == initial.id and current.review.id == initial.review.id and vim.deep_equal(current.comparison, revision), 'Checkout changed review session or pinned comparison')
end
local function hidden_file(path)
  local buf = vim.fn.bufadd(project .. '/' .. path)
  vim.fn.bufload(buf)
  vim.bo[buf].autoread = true
  return buf
end
local selections, choice = 0, 'Cancel'
vim.ui.select = function(items, options, callback)
  selections = selections + 1
  assert(vim.deep_equal(items, { 'Cancel', 'Stash & switch', 'Carry changes & switch' }), 'Dirty checkout lost safe default/options')
  callback(choice, vim.fn.index(items, choice) + 1)
end

-- Merely visiting the review only inspects Git, excluding reserved local state.
vim.cmd('Review')
wait(function()
  local row = kind() == 'home' and checkout_row()
  return row and row:find('main → feature', 1, true) and row:find('0 staged, 0 modified, 0 untracked', 1, true)
end, 'Checkout overview status missing')
local overview = checkout_row()
assert(overview:find('main', 1, true) and overview:find('feature', 1, true), 'Overview lost current/PR source branch')
assert(git('branch', '--show-current') == 'main' and #gh_calls() == 0 and git('stash', 'list') == '', 'Opening review mutated checkout/stash')
pinned()

-- Unsaved normal file buffers block checkout even when hidden from every window.
local notes = hidden_file('notes.txt')
api.nvim_buf_set_lines(notes, 0, -1, false, { 'unsaved hidden text' })
local before = #notices
vim.cmd('Review checkout')
wait(function() return #notices > before end, 'Hidden modified buffer did not block checkout')
assert(#gh_calls() == 0 and selections == 0 and git('branch', '--show-current') == 'main' and git('stash', 'list') == '', 'Hidden modified buffer allowed Git mutation')
assert(vim.bo[notes].modified and api.nvim_buf_get_lines(notes, 0, -1, false)[1] == 'unsaved hidden text', 'Blocked checkout discarded buffer text')
api.nvim_buf_set_lines(notes, 0, -1, false, { 'ordinary notes' })
vim.bo[notes].modified = false

-- Enter on the overview row performs clean checkout and reloads unmodified files.
local app = hidden_file('app.txt')
before = #notices
assert(api.nvim_buf_get_lines(app, 0, -1, false)[1] == 'main source')
checkout_row(); input('<CR>')
wait(function() return git('branch', '--show-current') == 'feature' and #gh_calls() == 1 and #notices > before end, 'Enter did not checkout PR source')
wait(function() return api.nvim_buf_get_lines(app, 0, -1, false)[1] == 'feature source' end, 'Checkout did not checktime hidden unmodified file')
assert(selections == 0 and kind() == 'home', 'Clean checkout prompted or left overview')
pinned()

-- Cancel preserves staged, unstaged and untracked changes without creating a stash.
git('switch', 'main')
disk('notes.txt', { 'staged notes' }); git('add', 'notes.txt'); disk('notes.txt', { 'staged notes', 'unstaged notes' })
disk('loose.txt', { 'untracked notes' })
local index, worktree = git('diff', '--cached'), git('diff')
api.nvim_exec_autocmds('FocusGained', {})
wait(function()
  local row = checkout_row()
  return row and row:find('main → feature', 1, true) and row:find('1 staged, 1 modified, 1 untracked', 1, true)
end, 'FocusGained did not refresh checkout dirty counts')
assert(#gh_calls() == 1 and git('stash', 'list') == '', 'Status refresh mutated checkout/stash')
choice, before = 'Cancel', #notices
vim.cmd('Review checkout')
wait(function() return selections == 1 end, 'Dirty checkout did not offer selection')
wait(function()
  local row = checkout_row()
  return row and row:find('1 staged, 1 modified, 1 untracked', 1, true) and not row:find('[switching]', 1, true)
end, 'Cancel did not restore dirty checkout overview')
assert(#gh_calls() == 1 and git('branch', '--show-current') == 'main' and git('stash', 'list') == '', 'Cancel mutated Git')
assert(git('diff', '--cached') == index and git('diff') == worktree and vim.fn.filereadable(project .. '/loose.txt') == 1, 'Cancel lost local changes')
pinned()

-- Stash remains an ordinary Git stash; .review fixture state stays on disk.
choice, before = 'Stash & switch', #notices
vim.cmd('Review checkout')
wait(function() return selections == 2 and #gh_calls() == 2 and git('branch', '--show-current') == 'feature' and #notices > before end, 'Stash-and-switch failed')
local stash = git('rev-parse', 'refs/stash')
local stash_files = git('stash', 'show', '--include-untracked', '--name-only', stash)
assert(stash_files:find('notes.txt', 1, true) and stash_files:find('loose.txt', 1, true) and not stash_files:find('.review', 1, true), 'Ordinary stash omitted changes or included review state')
assert(vim.fn.filereadable(fixture .. '/state.json') == 1 and git('status', '--porcelain', '--', '.', ':(top,exclude).review') == '', 'Stash switch removed local review state or left changes')
pinned()

-- Carry performs normal checkout, keeping index/worktree/untracked changes intact.
git('switch', 'main')
disk('notes.txt', { 'carried staged' }); git('add', 'notes.txt'); disk('notes.txt', { 'carried staged', 'carried unstaged' })
disk('carry.txt', { 'carried untracked' })
index, worktree = git('diff', '--cached'), git('diff')
choice, before = 'Carry changes & switch', #notices
vim.cmd('Review checkout')
wait(function() return selections == 3 and #gh_calls() == 3 and git('branch', '--show-current') == 'feature' and #notices > before end, 'Carry-and-switch failed')
assert(git('diff', '--cached') == index and git('diff') == worktree and vim.fn.filereadable(project .. '/carry.txt') == 1 and git('rev-parse', 'refs/stash') == stash, 'Carry altered local changes or created a stash')
pinned()

-- Failed checkout retains its new stash, which Git can restore manually with index.
git('switch', 'main')
vim.fn.writefile({ '' }, fixture .. '/fail_checkout')
choice, before = 'Stash & switch', #notices
vim.cmd('Review checkout')
wait(function() return selections == 4 and #gh_calls() == 4 and #notices > before end, 'Failed checkout did not report result')
local retained = git('rev-parse', 'refs/stash')
assert(notices[#notices]:find('Stash retained: ' .. retained, 1, true), 'Failed checkout omitted retained stash identity')
assert(retained ~= stash and git('branch', '--show-current') == 'main', 'Failed checkout lost stash or switched branch')
assert(git('status', '--porcelain', '--', '.', ':(top,exclude).review') == '', 'Failed checkout did not leave stashed changes recoverable')
git('stash', 'apply', '--index', retained)
assert(git('diff', '--cached') == index and git('diff') == worktree and vim.fn.filereadable(project .. '/carry.txt') == 1, 'Manual Git stash restore lost changes/index')
assert(git('rev-parse', 'refs/stash') == retained, 'Checkout silently popped retained stash')
pinned()

-- An in-progress operation blocks UI checkout before prompting or invoking gh.
vim.fn.mkdir(project .. '/.git/rebase-merge', 'p')
before = #notices
vim.cmd('Review checkout')
wait(function() return #notices > before end, 'Rebase did not block checkout')
assert(selections == 4 and #gh_calls() == 4 and git('rev-parse', 'refs/stash') == retained, 'Blocked rebase checkout prompted or mutated Git')
assert(kind() == 'home', 'Checkout failure navigated away from overview')
-- A refresh during the dirty prompt keeps the same review selected. Its new
-- PR metadata must drive the final status without changing the pinned diff.
vim.fn.delete(project .. '/.git/rebase-merge', 'd')
vim.fn.delete(fixture .. '/fail_checkout')
local pending
vim.ui.select = function(_, _, callback) pending = callback end
before = #notices
vim.cmd('Review checkout')
wait(function() return pending ~= nil end, 'Dirty checkout choice did not open')
local refreshed = state()
refreshed.review.read.pull_request.revision = { base = base, head = base }
refreshed.review.read.pull_request.source.branch = 'updated-feature'
write('state.json', refreshed)
vim.cmd('Review pull')
wait(function()
  local row = checkout_row()
  return row and row:find('updated-feature', 1, true)
end, 'PR refresh did not update checkout source while prompt open')
pending('Carry changes & switch')
wait(function()
  local row = checkout_row()
  return #gh_calls() == 5 and #notices > before and row and row:find('feature → updated-feature [ahead of PR]', 1, true)
end, 'Completed checkout kept stale source/head status after refresh')
assert(git('branch', '--show-current') == 'feature' and git('diff', '--cached') == index and git('diff') == worktree and git('rev-parse', 'refs/stash') == retained, 'Refreshed checkout lost changes or created stash')
pinned()

-- Navigating while the choice is open cancels before any Git mutation.
git('switch', 'main')
pending = nil
before = #notices
vim.cmd('Review checkout')
wait(function() return pending ~= nil end, 'Dirty checkout choice did not open')
vim.cmd('Review prs')
wait(function() return kind() == 'prs' end, 'Review navigation did not leave checkout overview')
pending('Stash & switch')
wait(function() return #notices > before end, 'Changed review navigation did not cancel checkout')
assert(notices[#notices]:find('review selection changed', 1, true), 'Checkout cancellation lost navigation reason')
assert(#gh_calls() == 5 and git('rev-parse', 'refs/stash') == retained and git('branch', '--show-current') == 'main', 'Cancelled checkout mutated Git')
assert(git('diff', '--cached') == index and git('diff') == worktree and vim.fn.filereadable(project .. '/carry.txt') == 1, 'Cancelled checkout changed local files')
for _, call in ipairs(gh_calls()) do assert(call.cwd == project, 'Checkout used wrong project root') end
pinned()
print('Neovim checkout UI integration passed')
vim.cmd.cd(repository)
vim.fn.delete(sandbox, 'rf')
vim.cmd('qa!')
