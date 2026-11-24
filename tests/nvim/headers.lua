local api = vim.api
local repository, scenario = vim.fn.getcwd(), vim.env.BUF_REVIEW_HEADER_CASE
if not scenario then
  for _, name in ipairs({ 'default', 'global', 'local' }) do
    local result = vim.system({ vim.v.progpath, '--headless', '-u', 'NONE', '-c',
      ('lua assert(loadfile(%q))(%q)'):format(repository .. '/tests/nvim/run.lua', repository .. '/tests/nvim/headers.lua') },
      { env = { BUF_REVIEW_HEADER_CASE = name }, text = true }):wait()
    assert(result.code == 0, name .. ': ' .. result.stderr)
  end
  print('Neovim Review header ownership passed')
  vim.cmd('qa!')
  return
end

local project = vim.fn.tempname()
vim.fn.mkdir(project, 'p')
vim.env.BUF_REVIEW_FIXTURE = project
vim.opt.runtimepath:prepend(repository)
local review = require('buf_review')
assert(debug.getinfo(review.setup, 'S').source == '@' .. repository .. '/lua/buf_review/init.lua',
  'header regression loaded installed plugin instead of workspace')
review.setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
local function git(...)
  local result = vim.system(vim.list_extend({ 'git' }, { ... }), { cwd = project, text = true }):wait()
  assert(result.code == 0, result.stderr)
  return vim.trim(result.stdout)
end
git('init', '--quiet')
git('config', 'user.name', 'Review fixture')
git('config', 'user.email', 'review@example.invalid')
local filename, path = 'file%name.txt', project .. '/file%name.txt'
local fresh_name, fresh_path = 'first%local.txt', project .. '/first%local.txt'
vim.fn.writefile({ 'original' }, path)
vim.fn.writefile({ 'first local file' }, fresh_path)
git('add', filename, fresh_name); git('commit', '--quiet', '-m', 'Base')
local base = git('rev-parse', 'HEAD')
vim.fn.writefile({ 'committed' }, path)
git('commit', '--quiet', '-am', 'Head')
local head = git('rev-parse', 'HEAD')
local revision = { base = base, head = head }
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, project .. '/' .. name) end
local function state() return vim.json.decode(table.concat(vim.fn.readfile(project .. '/state.json'), '\n')) end
local target = { path = filename, revision = revision,
  range = { start = { side = 'head', line = 1 }, ['end'] = { side = 'head', line = 1 } } }
local view = {
  id = 'headers', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'headers-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Review headers', revision = revision }, files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = { { comment = { origin = 'read', comment = { id = 'root', body = 'Existing header comment',
    author = { id = 'alice', name = 'alice' }, target = { kind = 'diff', target = target } } },
    fetched = true, done = false, conflict = false, summary = false } },
  files = { { path = filename, viewed = false }, { path = fresh_name, viewed = false } },
  pending = {}, todo = { 'root' }, done = {}, conflicts = {}, publications = {}, new_commits = false,
}
write('state.json', view)
write('routes.json', { entry = { route = 'batch', batch = view.id, fresh_review = view.review.id } })
write('code_contexts.json', { [filename] = { path = filename, revision = revision, binary = false,
  base_content = 'original\n', head_content = 'committed\n', local_path = path, local_dirty = false },
  [fresh_name] = { path = fresh_name, revision = revision, binary = false,
    base_content = 'first original\n', head_content = 'first local file\n', local_path = fresh_path, local_dirty = false } })
write('historical_contexts.json', { root = { anchor = target, code = {
  path = filename, base_path = filename, revision = { base = head, head = head },
  base_content = 'committed\n', head_content = 'committed\n', binary = false, historical = true, provenance = 'recorded',
  original_base = { commit = base, path = filename, content = 'original\n', missing = false, provenance = 'recorded' },
  location = { path = filename, revision = { base = head, head = head },
    range = { start = { side = 'base', line = 1 }, ['end'] = { side = 'base', line = 1 } } },
  local_path = path, local_dirty = false,
} } })
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd.cd(project)
api.nvim_cmd({ cmd = 'edit', args = { path }, magic = { file = false } }, {})
local native = api.nvim_get_current_buf()
vim.bo[native].filetype, vim.bo[native].shiftwidth, vim.bo[native].undolevels = 'text', 7, 80
vim.go.winbar = scenario == 'default' and '' or 'Global 100%% %f'
vim.wo.winbar = scenario == 'local' and 'Personal 100%% %f' or ''
local original = api.nvim_get_option_value('winbar', { win = api.nvim_get_current_win(), scope = 'local' })
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices))
  ReviewTest.wait(30)
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
  ReviewTest.wait(30)
end
local function label(win)
  win = win or api.nvim_get_current_win()
  local format = vim.wo[win].winbar
  if format == '' then format = vim.go.winbar end
  return api.nvim_eval_statusline(format, { winid = win, use_winbar = true, maxwidth = 10000 }).str
end
local function native_header(reason, exact)
  assert(vim.b.buf_review == nil, reason .. ': ordinary file gained Review metadata')
  local rendered = label()
  for _, prefix in ipairs({ 'Working tree', 'Base ', 'Head ', 'Original ', 'Historical reference', 'Comment:', 'Reply:' }) do
    assert(not rendered:find(prefix, 1, true), reason .. ': leaked ' .. rendered)
  end
  if exact then
    local current = api.nvim_get_option_value('winbar', { win = api.nvim_get_current_win(), scope = 'local' })
    assert(current == original, reason .. ': prior user winbar was not restored: ' .. vim.inspect(current))
  end
end
local function native_state()
  assert(api.nvim_get_current_buf() == native and vim.bo[native].buftype == ''
    and vim.bo[native].modifiable and not vim.bo[native].readonly, 'native file editing options changed')
  assert(vim.bo[native].filetype == 'text' and vim.bo[native].shiftwidth == 7 and vim.bo[native].undolevels == 80,
    'native file configuration changed')
  for _, mapping in ipairs(api.nvim_buf_get_keymap(native, 'n')) do
    assert(not vim.tbl_contains({ 'a', 's', 'u', 'o' }, mapping.lhs), 'native file gained Review mapping')
  end
end
local function row(needle)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    local file = needle == filename or needle == fresh_name
    if file and value:sub(-#needle - 2) == '  ' .. needle
      or not file and value:find(needle, 1, true) then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing row ' .. needle)
end
local function home()
  vim.cmd('Review')
  wait(function() return kind() == 'home' end, 'Review home missing')
end
local function file(name)
  home(); row(name or filename); review.enter()
  wait(function() return kind() == 'diff' end, 'Review code missing')
end

-- This ordinary window exists before Review has decorated any panes. Its
-- incoming file below has never been opened outside a Review Local pane.
local review_tab = api.nvim_get_current_tabpage()
local other_path = project .. '/ordinary.txt'
vim.fn.writefile({ 'ordinary source' }, other_path)
api.nvim_cmd({ cmd = 'tabnew', args = { other_path }, magic = { file = false } }, {})
local ordinary_window = api.nvim_get_current_win()
vim.wo.winbar = ''
api.nvim_set_current_tabpage(review_tab)
local function local_pane()
  file()
  if #vim.fn.win_findbuf(native) == 0 then input('o') end
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    if api.nvim_win_get_buf(win) == native then api.nvim_set_current_win(win); return win end
  end
  error('Local pane missing')
end
local function normal_file(exact)
  api.nvim_cmd({ cmd = 'edit', args = { path }, mods = { hide = true }, magic = { file = false } }, {})
  native_header('normal reopen', exact)
  native_state()
end

-- Main regression: a real Local file remains loaded but leaves its Review
-- window; reusing that buffer through ordinary :edit must not restore a header.
local local_window = local_pane()
assert(label():find('Working tree  ' .. filename, 1, true), 'Local header missing or percent escaped twice')
api.nvim_buf_set_lines(native, 0, -1, false, { 'unsaved local content' })
if scenario == 'global' then vim.go.winbar = 'Updated global 100%% %f' end
home()
assert(api.nvim_get_current_win() == local_window, 'home did not preserve invoking Local window')
normal_file(true)
if scenario == 'global' then assert(label():find('Updated global 100%', 1, true), 'cleanup pinned old global winbar value') end
assert(vim.bo[native].modified and api.nvim_get_current_line() == 'unsaved local content', 'header cleanup lost native edits')
api.nvim_cmd({ cmd = 'split', args = { path }, mods = { hide = true }, magic = { file = false } }, {})
native_header('normal split reopen')
api.nvim_win_close(0, false)
vim.cmd('tabnew')
normal_file(false)
vim.cmd('tabclose')
api.nvim_set_current_tabpage(review_tab)
assert(api.nvim_get_current_tabpage() == api.nvim_win_get_tabpage(local_window), 'native tab roundtrip changed Review tab')

-- Reentry and local progress refresh must still render Review headers, then
-- restore the same native header again when returning to ordinary editing.
local_window = local_pane()
local active_label = label(local_window)
local source
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  if vim.b[api.nvim_win_get_buf(win)].buf_review then source = win; break end
end
api.nvim_set_current_win(source)
assert(label():find(filename, 1, true), 'source header lost literal-percent path')
vim.cmd('Review viewed')
wait(function() return state().files[1].viewed and label(source):find('[viewed]', 1, true) end, 'source header lost progress updates')
assert(label(local_window) == active_label, 'progress update changed Local header')
api.nvim_set_current_win(local_window)
vim.cmd('split')
local duplicate = api.nvim_get_current_win()
vim.cmd('enew')
native_header('duplicate Local window replaced')
api.nvim_win_close(duplicate, true)
home(); normal_file(true)

-- A first display through Local also stores window options for later native
-- windows. Reusing an already-existing ordinary window must not inherit that
-- active Review owner's header, and must not clear the owner's own header.
assert(vim.fn.bufnr(fresh_path) == -1, 'first-Local regression file was already loaded')
file(fresh_name); input('o')
local first_local_window, first_local_buffer
for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
  local buf = api.nvim_win_get_buf(win)
  if api.nvim_buf_get_name(buf) == fresh_path then first_local_window, first_local_buffer = win, buf; break end
end
assert(first_local_window, 'first Local pane missing')
local first_local_header = label(first_local_window)
assert(first_local_header:find('Working tree  ' .. fresh_name, 1, true), 'first Local header missing')
api.nvim_set_current_win(ordinary_window)
api.nvim_cmd({ cmd = 'edit', args = { fresh_path }, mods = { hide = true }, magic = { file = false } }, {})
native_header('first-Local file entered existing ordinary window')
assert(api.nvim_get_current_buf() == first_local_buffer and vim.bo.buftype == '' and vim.bo.modifiable and not vim.bo.readonly,
  'first-Local file gained Review buffer state')
assert(vim.b.buf_review_context == nil, 'first-Local file gained Review header metadata')
assert(label(first_local_window) == first_local_header, 'ordinary file entry cleared active Review owner header')
api.nvim_set_current_win(first_local_window)
home(); normal_file(true)

if scenario == 'default' then
  -- Other source variants use the same lifecycle. Test real context buffers,
  -- recorded-hunk fallback and composer headers without depending on their
  -- internal winbar format (only their native rendered text).
  local function comment()
    home(); row('Existing header comment'); review.enter()
    wait(function() return kind() == 'thread' end, 'comment discussion missing')
  end
  local main_tab
  for _, context in ipairs({
    { available = true, anchor = target, base_content = 'original\n', head_content = 'committed\n' },
    { available = true, anchor = target, content = 'committed\n' },
    { available = false, message = 'Recorded only', diff_hunk = '@@ -1 +1 @@\n-original\n+committed' },
  }) do
    comment(); main_tab = api.nvim_get_current_tabpage()
    write('contexts.json', { root = context })
    vim.cmd('Review context')
    wait(function() return kind() == 'context' end, 'original context missing')
    assert(label():find('Original', 1, true), 'original context header missing')
    normal_file(false)
    vim.cmd('tabclose'); api.nvim_set_current_tabpage(main_tab)
  end

  write('historical_contexts.json', { root = { available = false, anchor = target,
    message = 'Missing Git object', diff_hunk = '@@ -1 +1 @@\n-original\n+committed' } })
  comment()
  local hunk
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local marker = vim.b[api.nvim_win_get_buf(win)].buf_review
    if marker and marker.kind == 'historical' then hunk = win end
  end
  assert(hunk, 'historical fallback source missing')
  api.nvim_set_current_win(hunk)
  assert(label():find('Historical reference', 1, true), 'historical fallback header missing')
  normal_file(false)

  local_pane()
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    if vim.b[api.nvim_win_get_buf(win)].buf_review then api.nvim_set_current_win(win); break end
  end
  input('a<Esc>')
  assert(kind() == 'composer' and label():find('Comment:', 1, true) and label():find(filename, 1, true), 'composer header missing')
  local composing, composing_window = api.nvim_get_current_buf(), api.nvim_get_current_win()
  vim.cmd('split')
  duplicate = api.nvim_get_current_win()
  api.nvim_win_set_buf(duplicate, native)
  native_header('composer window reused for ordinary file')
  api.nvim_win_set_buf(duplicate, composing)
  assert(label():find('Comment:', 1, true), 'composer reentry lost header')
  api.nvim_win_close(duplicate, false)
  api.nvim_set_current_win(composing_window)
  vim.cmd('quit')
end

assert(vim.bo[native].modified and table.concat(api.nvim_buf_get_lines(native, 0, -1, false), '\n') == 'unsaved local content',
  'Review header lifecycle changed native file contents')
print('Neovim Review header scenario ' .. scenario .. ' passed')
vim.fn.delete(project, 'rf')
vim.cmd('qa!')
