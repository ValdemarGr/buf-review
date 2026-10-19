vim.opt.runtimepath:append(vim.fn.getcwd())
local checkout = require('buf_review.checkout')
local api = vim.api
local root, bin = vim.fn.tempname(), vim.fn.tempname()
vim.fn.mkdir(root, 'p')
vim.fn.mkdir(bin, 'p')
local function git(...)
  local result = vim.system(vim.list_extend({ 'git' }, { ... }), { cwd = root, text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
local function write(path, lines) vim.fn.writefile(lines, root .. '/' .. path) end
local function receive(start)
  local done, value, failure
  start(function(status, err) value, failure, done = status, err, true end)
  assert(ReviewTest.wait(10000, function() return done end, 10), 'checkout callback timed out')
  return value, failure
end
git('init', '-b', 'main')
git('config', 'user.name', 'Review Fixture')
git('config', 'user.email', 'fixture@example.invalid')
write('file', { 'base' })
write('other', { 'other' })
write('.gitignore', { 'ignored' })
git('add', '.')
git('commit', '-m', 'base')
git('switch', '-c', 'feature')
write('file', { 'feature' })
git('commit', '-am', 'feature')
local pull = { url = 'https://github.com/acme/repo/pull/42', revision = { head = git('rev-parse', 'HEAD') },
  source = { repository = 'https://github.com/acme/repo', branch = 'feature' } }
git('remote', 'add', 'origin', 'git@github.com:acme/repo.git')
git('config', 'branch.feature.remote', 'origin')
git('config', 'branch.feature.merge', 'refs/heads/feature')
vim.fn.writefile({ '#!/bin/sh', 'if test -f fail-checkout; then echo "fixture checkout refused" >&2; exit 1; fi', 'exec git switch feature' }, bin .. '/gh')
vim.fn.setfperm(bin .. '/gh', 'rwxr-xr-x')
local previous_path = vim.env.PATH
vim.env.PATH = bin .. ':' .. previous_path
local function inspect() return receive(function(callback) checkout.inspect(root, pull, callback) end) end
local function run(options) return receive(function(callback) checkout.run(root, pull, options or {}, callback) end) end

local status, err = inspect()
assert(not err and status.branch == 'feature' and status.head == pull.revision.head and status.matches and status.source_match, vim.inspect({ status, err }))
write('file', { 'ahead' })
git('commit', '-am', 'local fix')
local ahead = git('rev-parse', 'HEAD')
status, err = inspect()
assert(not err and status.relationship == 'ahead' and status.ahead == 1 and status.matches, vim.inspect({ status, err }))
status, err = run()
assert(not err and git('rev-parse', 'HEAD') == ahead, 'checkout discarded ahead commit: ' .. tostring(err))

-- Reload saved source buffers through native checktime even when users keep
-- autoread disabled. Restore their option afterward.
git('switch', 'main')
local saved = vim.fn.bufadd(root .. '/file')
vim.fn.bufload(saved)
vim.bo[saved].autoread = false
assert(api.nvim_buf_get_lines(saved, 0, -1, false)[1] == 'base')
status, err = run()
assert(not err and api.nvim_buf_get_lines(saved, 0, -1, false)[1] == 'ahead', 'saved buffer did not reload: ' .. tostring(err))
assert(vim.bo[saved].autoread == false, 'checkout changed autoread option')
api.nvim_buf_delete(saved, { force = true })

-- gh may switch successfully then refuse its fast-forward; buffers must still
-- reflect the resulting worktree while the original Git error stays visible.
git('switch', 'main')
saved = vim.fn.bufadd(root .. '/file')
vim.fn.bufload(saved)
vim.fn.writefile({ '#!/bin/sh', 'git switch feature || exit', 'echo "fixture fast-forward refused" >&2', 'exit 1' }, bin .. '/gh')
status, err = run()
assert(err and err:find('fixture fast-forward refused', 1, true) and api.nvim_buf_get_lines(saved, 0, -1, false)[1] == 'ahead', tostring(err))
api.nvim_buf_delete(saved, { force = true })
vim.fn.writefile({ '#!/bin/sh', 'if test -f fail-checkout; then echo "fixture checkout refused" >&2; exit 1; fi', 'exec git switch feature' }, bin .. '/gh')

-- Status counts use NUL records: renamed paths and embedded newlines are not
-- interpreted as records. Reserved review state remains outside local changes.
git('mv', 'other', 'renamed\nfile')
write('file', { 'unstaged' })
write('untracked\nfile', { 'untracked' })
vim.fn.mkdir(root .. '/.review', 'p')
write('.review/state.sqlite', { 'review session' })
write('ignored', { 'ignored' })
status, err = inspect()
assert(not err and status.staged == 1 and status.modified == 1 and status.untracked == 1, vim.inspect({ status, err }))
local selections = 0
vim.ui.select = function(items, _, callback)
  selections = selections + 1
  assert(vim.deep_equal(items, { 'Cancel', 'Stash & switch', 'Carry changes & switch' }))
  callback('Cancel')
end
status, err = run()
assert(not err and status.cancelled and selections == 1 and git('branch', '--show-current') == 'feature')

-- Stashes remain ordinary Git objects, including their index/untracked state.
-- Reserved .review and ignored files must never be removed or captured.
vim.ui.select = function(_, _, callback) callback('Stash & switch') end
status, err = run()
assert(not err and status.stash and status.staged == 0 and status.modified == 0 and status.untracked == 0, vim.inspect({ status, err }))
assert(git('rev-parse', 'refs/stash') == status.stash.oid)
assert(git('stash', 'list'):find('buf%-review: feature before https://github.com/acme/repo/pull/42'))
assert(vim.fn.filereadable(root .. '/.review/state.sqlite') == 1 and vim.fn.filereadable(root .. '/ignored') == 1)
local captured = git('ls-tree', '-r', '--name-only', 'refs/stash^3')
assert(captured:find('untracked') and not captured:find('.review', 1, true) and not captured:find('ignored', 1, true), captured)

-- A failed checkout leaves parked work available in Git, never auto-applies it.
write('file', { 'second stash' })
write('fail-checkout', { 'fail' })
git('add', 'fail-checkout')
git('commit', '-m', 'fixture failure marker')
status, err = run()
assert(err and err:find('fixture checkout refused', 1, true) and status.stash and git('rev-parse', 'refs/stash') == status.stash.oid)
assert(vim.fn.readfile(root .. '/file')[1] == 'ahead', 'failed checkout reapplied stash')
git('rm', 'fail-checkout')
git('commit', '-m', 'remove failure marker')

-- A changed review selection can cancel after stashing but before checkout.
-- The parked stash stays visible and saved buffers reflect the stash rollback.
write('file', { 'cancelled after stash' })
saved = vim.fn.bufadd(root .. '/file')
vim.fn.bufload(saved)
local stash_before_cancel = git('rev-parse', 'refs/stash')
status, err = run({ validate = function()
  if git('rev-parse', 'refs/stash') ~= stash_before_cancel then return false, 'fixture review changed' end
  return true
end })
assert(err == 'fixture review changed' and status.stash, vim.inspect({ status, err }))
assert(api.nvim_buf_get_lines(saved, 0, -1, false)[1] == 'ahead', 'stash cancellation left saved buffer stale')
api.nvim_buf_delete(saved, { force = true })

-- Git does not stash dirty submodule worktrees. A successful no-op must never
-- claim an older stash as this checkout's newly parked work.
local child = vim.fn.tempname()
vim.fn.mkdir(child, 'p')
local function child_git(...)
  local result = vim.system(vim.list_extend({ 'git', '-C', child }, { ... }), { text = true }):wait()
  assert(result.code == 0, result.stderr)
end
child_git('init', '-b', 'main')
child_git('config', 'user.name', 'Review Fixture')
child_git('config', 'user.email', 'fixture@example.invalid')
vim.fn.writefile({ 'child' }, child .. '/file')
child_git('add', 'file')
child_git('commit', '-m', 'child')
git('-c', 'protocol.file.allow=always', 'submodule', 'add', child, 'module')
git('commit', '-am', 'add submodule')
write('module/file', { 'dirty child' })
local old_stash = git('rev-parse', 'refs/stash')
status, err = run()
assert(err == 'Local changes remain after stashing' and not status.stash and git('rev-parse', 'refs/stash') == old_stash,
  'old stash reported as new: ' .. vim.inspect({ status, err }))
git('-C', 'module', 'restore', 'file')
vim.fn.delete(child, 'rf')

-- Native Git operation markers block before opening a dirty-state prompt.
local gitdir = git('rev-parse', '--absolute-git-dir')
vim.fn.mkdir(gitdir .. '/rebase-merge', 'p')
status, err = run()
assert(err and err:find('rebase-merge', 1, true), tostring(err))
vim.fn.delete(gitdir .. '/rebase-merge', 'd')

-- Hidden unsaved source buffers block; unrelated repository buffers do not.
local buf = api.nvim_create_buf(true, false)
assert(vim.uv.fs_symlink(root, root .. '-alias'))
api.nvim_buf_set_name(buf, root .. '-alias/new-directory/hidden-new')
api.nvim_buf_set_lines(buf, 0, -1, false, { 'unsaved' })
status, err = run()
assert(err and err:find('hidden-new', 1, true), tostring(err))
api.nvim_buf_delete(buf, { force = true })
vim.fn.delete(root .. '-alias')

-- Recheck unsaved buffers after a choice. Keep concurrent requests from
-- racing while the first selection is still waiting for user input.
write('file', { 'dirty' })
local pending
vim.ui.select = function(_, _, callback) pending = callback end
local done, first_error
checkout.run(root, pull, {}, function(_, error) first_error, done = error, true end)
assert(ReviewTest.wait(5000, function() return pending ~= nil end, 10))
status, err = run()
assert(err == 'Checkout already in progress')
buf = api.nvim_create_buf(true, false)
api.nvim_buf_set_name(buf, root .. '/file')
api.nvim_buf_set_lines(buf, 0, -1, false, { 'changed during prompt' })
pending('Stash & switch')
assert(ReviewTest.wait(5000, function() return done end, 10))
assert(first_error and first_error:find('unsaved buffer', 1, true), tostring(first_error))
api.nvim_buf_delete(buf, { force = true })

vim.env.PATH = previous_path
vim.fn.delete(root, 'rf')
vim.fn.delete(bin, 'rf')
print('checkout Git tests passed')
vim.cmd('qa!')
