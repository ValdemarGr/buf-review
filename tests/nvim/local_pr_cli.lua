-- Exercise Neovim, the actual Rust protocol, SQLite and Git in one disposable repo.
local api = vim.api
local repository = vim.env.BUF_REVIEW_LOCAL_PR_RUNTIME or vim.fn.getcwd()
local resume = vim.env.BUF_REVIEW_LOCAL_PR_PROJECT
local sandbox = resume and vim.fs.dirname(resume) or vim.fn.tempname()
local project, bin = resume or sandbox .. '/project', sandbox .. '/bin'
local executable = vim.env.BUF_REVIEW_LOCAL_PR_BINARY or repository .. '/target/debug/pr'
assert(vim.fn.executable(executable) == 1, 'Build pr before running real CLI Neovim workflows')
vim.env.GIT_CONFIG_GLOBAL, vim.env.GIT_CONFIG_SYSTEM = '/dev/null', '/dev/null'
vim.env.GIT_ALLOW_PROTOCOL = 'file'
local function git(...)
  local result = vim.system(vim.list_extend({ 'git' }, { ... }), { cwd = project, text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
if not resume then
  vim.fn.mkdir(project, 'p')
  vim.fn.mkdir(bin, 'p')
  git('init', '-b', 'main')
  git('config', 'user.name', 'Review fixture')
  git('config', 'user.email', 'review@example.invalid')
  git('config', 'push.default', 'simple')
  vim.fn.writefile({ '.review/' }, project .. '/.git/info/exclude')
  vim.fn.writefile({ 'one', 'two', 'three' }, project .. '/app.txt')
  vim.fn.writefile({ 'ordinary notes' }, project .. '/notes.txt')
  git('add', 'app.txt', 'notes.txt')
  git('commit', '-m', 'Base')
  git('switch', '-c', 'feature')
  vim.fn.writefile({ 'one', 'committed change', 'three' }, project .. '/app.txt')
  git('add', 'app.txt')
  git('commit', '-m', 'Feature')
  git('remote', 'add', 'origin', 'https://github.com/acme/repo.git')
  git('remote', 'set-url', '--push', 'origin', 'https://github.com/acme/repo.git')
  git('remote', 'set-url', 'origin', project)
  vim.fn.writefile({ 'dirty file must stay outside review' }, project .. '/app.txt')
  vim.fn.writefile({ 'unsaved to Git' }, project .. '/notes.txt')
  vim.fn.writefile({ 'untracked contents' }, project .. '/untracked.txt')
  vim.fn.writefile({
    '#!/usr/bin/env python3',
    'import json, os, pathlib, sys, urllib.parse',
    'args = sys.argv[1:]',
    'method = args[args.index("--method") + 1] if "--method" in args else "GET"',
    'with pathlib.Path(os.environ["BUF_REVIEW_LOCAL_PR_LOG"]).open("a") as stream:',
    '    stream.write(json.dumps({"args":args,"method":method}) + "\\n")',
    'if method != "GET":',
    '    print("Fixture rejects all provider writes", file=sys.stderr); sys.exit(1)',
    'if args == ["repo", "view", "--json", "nameWithOwner"]:',
    '    print(json.dumps({"nameWithOwner":"acme/repo"}))',
    'elif args[:2] == ["api", "user"]:',
    '    print(json.dumps({"id":7,"login":"reviewer"}))',
    'elif args[0] == "api" and args[1].startswith("repos/acme/repo/pulls?"):',
    '    query = urllib.parse.parse_qs(urllib.parse.urlsplit(args[1]).query)',
    '    assert query == {"state":["open"],"head":["acme:feature"],"base":["main"],"per_page":["100"]}, query',
    '    print("[[]]")',
    'elif args[:2] == ["api", "repos/acme/repo/branches/feature"]:',
    '    print("gh: Not Found (HTTP 404)", file=sys.stderr); sys.exit(1)',
    'else:',
    '    print("Unexpected gh request: " + repr(args), file=sys.stderr); sys.exit(1)',
  }, bin .. '/gh')
  assert(vim.fn.setfperm(bin .. '/gh', 'rwxr-xr-x') == 1)
end
vim.env.BUF_REVIEW_LOCAL_PR_LOG = sandbox .. '/gh.jsonl'
vim.env.PATH = bin .. ':' .. vim.env.PATH
vim.opt.runtimepath:prepend(repository)
require('buf_review').setup({ executable = executable })
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd.cd(project)
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function text() return table.concat(api.nvim_buf_get_lines(0, 0, -1, false), '\n') end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(10000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(40)
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row: ' .. needle .. '\n' .. text())
end
local active_id
local function active()
  local request = active_id and { op = 'get', workspace = active_id } or { op = 'active' }
  local result = vim.system({ executable, 'workspace' }, { cwd = project, stdin = vim.json.encode(request), text = true }):wait()
  assert(result.code == 0, result.stderr)
  local response = vim.json.decode(result.stdout, { luanil = { object = true, array = true } })
  assert(not response.error, response.error)
  local view = assert(response.workspace, result.stdout)
  if not active_id then active_id = view.id; return active() end
  return view
end
local function home()
  local expected_head = git('rev-parse', 'HEAD'):sub(1, 10)
  vim.cmd('Review')
  wait(function() return kind() == 'home' and text():find('HEAD ' .. expected_head, 1, true)
    and text():find('Code: ready', 1, true) end, 'Current local overview missing')
end
local function draft(view)
  for _, value in ipairs(view.comments) do
    if value.comment.comment.body == 'Actual CLI draft' then return value.comment.comment end
  end
end
if resume then
  vim.cmd('Review sessions')
  wait(function() return kind() == 'sessions' end, 'Restart lost session browser')
  assert(text():find('feature → main [local]', 1, true), 'Session browser omitted local branch pair')
  row(assert(vim.env.BUF_REVIEW_LOCAL_PR_BATCH)); input('<CR>')
  wait(function() return kind() == 'home' end, 'Restart could not select local session')
  local view = active()
  assert(view.pr.id == vim.env.BUF_REVIEW_LOCAL_PR_ID and view.id == vim.env.BUF_REVIEW_LOCAL_PR_BATCH)
  assert(view.pr.title == 'Actual CLI title' and view.pr.body == 'Body saved through native Neovim')
  assert(draft(view).id == vim.env.BUF_REVIEW_LOCAL_PR_COMMENT, 'Restart replaced saved comment identity')
  assert(view.comparison.head == vim.env.BUF_REVIEW_LOCAL_PR_HEAD)
  print('Neovim actual CLI local PR restart passed')
  vim.cmd('qa!')
  return
end

local head, status = git('rev-parse', 'HEAD'), git('status', '--porcelain')
vim.cmd('Review pr main')
wait(function() return kind() == 'home' end, 'Actual CLI local PR did not open')
local initial = active()
assert(initial.pr.source.kind == 'local_intent' and initial.pr.source.branches.source.branch == 'feature')
assert(initial.pr.source.branches.sink.branch == 'main' and initial.pr.title == '' and initial.pr.body == '')
assert(initial.comparison.head == head and text():find('local committed snapshot', 1, true))
assert(#initial.files == 1 and initial.files[1].path == 'app.txt', 'Review included dirty/untracked-only files')
assert(vim.fn.filereadable(project .. '/.review/state.sqlite3') == 1, 'Actual SQLite state missing')
api.nvim_win_set_cursor(0, { 2, 0 }); input('<CR>')
wait(function() return kind() == 'composer' end, 'Actual metadata composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Actual CLI title', '', 'Body saved through native Neovim' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'Actual metadata :wq failed')
assert(active().pr.title == 'Actual CLI title' and active().pr.body == 'Body saved through native Neovim')
home()
row('M +1 -1  app.txt'); input('<CR>')
wait(function() return kind() == 'diff' and vim.b.buf_review.side == 'base' end, 'Dirty file did not focus left Review pane')
input('<C-w>l')
wait(function() return api.nvim_buf_get_name(0) == project .. '/app.txt' end, 'Dirty file did not open as native right pane')
assert(kind() == nil and vim.bo.modifiable and not vim.bo.readonly, 'Dirty right pane became a Review buffer')
assert(text() == 'dirty file must stay outside review' and vim.fn.maparg('a', 'n') == '', 'Dirty file lost native contents or editing')
assert(git('status', '--porcelain') == status and git('rev-parse', 'HEAD') == head, 'Opening local pane changed Git work')
api.nvim_buf_set_lines(0, 0, -1, false, { 'one', 'new committed change', 'three' })
vim.cmd.write()
git('add', 'app.txt'); git('commit', '-m', 'Commit edited review file')
head, status = git('rev-parse', 'HEAD'), git('status', '--porcelain')
assert(kind() == nil, 'Commit silently converted native pane to Review buffer')
home()
row('M +1 -1  app.txt'); input('<CR>')
wait(function() return kind() == 'diff' end, 'Actual committed diff missing')
assert(text() == 'one\nnew committed change\nthree' and vim.bo.readonly and not vim.bo.modifiable)
api.nvim_win_set_cursor(0, { 2, 0 }); input('a')
wait(function() return kind() == 'composer' end, 'Actual inline comment composer missing')
api.nvim_buf_set_lines(0, 0, -1, false, { 'Actual CLI draft' })
input(':wq<CR>')
wait(function() return kind() ~= 'composer' end, 'Actual inline comment save failed')
local saved = active()
local comment = assert(draft(saved), 'Actual CLI dropped local draft')
assert(comment.target.target.revision.head == head and comment.target.target.range.start.line == 2)
assert(git('status', '--porcelain') == status and git('rev-parse', 'HEAD') == head, 'Local review changed Git work')

-- A fresh Neovim process must recover the same persisted objects and bodies.
local command = ('lua assert(loadfile(%q))(%q)'):format(repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/local_pr_cli.lua')
local restarted = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c', command }, { cwd = project, text = true, env = {
  BUF_REVIEW_LOCAL_PR_RUNTIME = repository, BUF_REVIEW_LOCAL_PR_PROJECT = project,
  BUF_REVIEW_LOCAL_PR_BINARY = executable, BUF_REVIEW_LOCAL_PR_BATCH = saved.id,
  BUF_REVIEW_LOCAL_PR_ID = saved.pr.id, BUF_REVIEW_LOCAL_PR_COMMENT = comment.id, BUF_REVIEW_LOCAL_PR_HEAD = head,
} }):wait()
assert(restarted.code == 0, restarted.stderr .. restarted.stdout)

-- Refresh retains saved PR evidence while the visible diff follows Git checkout.
vim.fn.writefile({ 'one', 'new committed change', 'three' }, project .. '/app.txt')
git('switch', '-c', 'unrelated', 'main')
vim.fn.writefile({ 'another branch' }, project .. '/unrelated.txt')
git('add', 'unrelated.txt'); git('commit', '-m', 'Unrelated work')
local unrelated = git('rev-parse', 'HEAD')
assert(unrelated ~= head)
local generation = active().generation
home()
vim.cmd('Review pull')
wait(function() return active().generation > generation end, 'Actual local refresh did not complete')
assert(active().comparison.head == unrelated and active().review.read.pull_request.revision.head == head, 'Refresh mixed checked-out HEAD with saved PR source')
assert(draft(active()).target.target.revision.head == head, 'Checkout rewrote existing draft anchor')
row('A +1 -0  unrelated.txt'); input('<CR>')
wait(function() return kind() == 'diff' end, 'Checked-out branch diff did not open')
assert(text() == 'another branch', 'Diff ignored checked-out branch')
home()

-- Unpushed publication gives Git guidance without sending any provider writes.
local before = active()
local notice_count = #notices
vim.cmd('Review publish')
wait(function()
  for index = notice_count + 1, #notices do if notices[index]:find('push', 1, true) then return true end end
  return false
end, 'Unpushed publication lacked actionable push error')
local after = active()
assert(after.id == before.id and after.pr.id == before.pr.id and after.pr.source.kind == 'local_intent')
assert(after.pr.title == before.pr.title and after.pr.body == before.pr.body and draft(after).id == comment.id)
local before_changes, after_changes = {}, {}
for _, operation in ipairs(before.pending) do before_changes[#before_changes + 1] = operation.change end
for _, operation in ipairs(after.pending) do after_changes[#after_changes + 1] = operation.change end
assert(vim.deep_equal(after_changes, before_changes) and after.comparison.head == unrelated, 'Unpushed publication consumed local changes or changed checkout projection')
assert(git('branch', '--show-current') == 'unrelated' and git('rev-parse', 'HEAD') == unrelated)
assert(table.concat(vim.fn.readfile(project .. '/notes.txt'), '\n') == 'unsaved to Git')
local branch_checked = false
for _, line in ipairs(vim.fn.readfile(vim.env.BUF_REVIEW_LOCAL_PR_LOG)) do
  local request = vim.json.decode(line)
  assert(request.method == 'GET', 'Unpushed publication attempted provider write')
  if request.args[2] == 'repos/acme/repo/branches/feature' then branch_checked = true end
end
assert(branch_checked, 'Publication did not verify source branch exists remotely')
print('Neovim actual CLI local PR workflows passed')
vim.cmd('qa!')
