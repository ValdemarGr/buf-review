local api = vim.api
local repository = vim.fn.getcwd()

if vim.env.BUF_REVIEW_DIFF_SWAP_CHILD ~= '1' then
  -- Capture the real UI's completed redraw frames through Neovim's native
  -- MessagePack codec; no Python RPC package or terminal emulator is needed.
  local input, output, errors = vim.uv.new_pipe(false), vim.uv.new_pipe(false), vim.uv.new_pipe(false)
  local unpacker, responses, stderr = vim.mpack.Unpacker(), {}, ''
  local cells, width, height, phase, failure, status = {}, 0, 0
  local old_frame, new_frame, swap_frames = false, false, 0
  local function clear()
    cells = {}
    for row = 1, height do cells[row] = {}; for col = 1, width do cells[row][col] = ' ' end end
  end
  local function screen()
    local result = {}
    for row = 1, height do result[row] = table.concat(cells[row]) end
    return table.concat(result, '\n')
  end
  local function message(value)
    if value[1] == 1 then responses[value[2]] = value; return end
    if value[1] ~= 2 then return end
    if value[2] == 'diff_swap_phase' then phase = value[3][1]; return end
    if value[2] ~= 'redraw' then return end
    for _, event in ipairs(value[3]) do
      for index = 2, #event do
        local data = event[index]
        if event[1] == 'grid_resize' and data[1] == 1 then
          width, height = data[2], data[3]; clear()
        elseif event[1] == 'grid_clear' and data[1] == 1 then clear()
        elseif event[1] == 'grid_line' and data[1] == 1 then
          local row, col = data[2] + 1, data[3] + 1
          for _, cell in ipairs(data[4]) do
            for _ = 1, cell[3] or 1 do cells[row][col], col = cell[1], col + 1 end
          end
        elseif event[1] == 'grid_scroll' and data[1] == 1 then
          local previous = vim.deepcopy(cells)
          for row = data[2] + 1, data[3] do
            for col = data[4] + 1, data[5] do
              local source_row, source_col = row + data[6], col + data[7]
              cells[row][col] = source_row > data[2] and source_row <= data[3]
                and source_col > data[4] and source_col <= data[5] and previous[source_row][source_col] or ' '
            end
          end
        elseif event[1] == 'flush' then
          local visible = screen()
          local old = visible:find('BASE ONE', 1, true) and visible:find('HEAD ONE', 1, true)
          local new = visible:find('BASE TWO', 1, true) and visible:find('HEAD TWO', 1, true)
          if phase == 'old' then old_frame = old_frame or old end
          if phase == 'swap' then
            swap_frames = swap_frames + 1
            new_frame = new_frame or new
            if not old and not new then failure = failure or ('incomplete comparison frame:\n' .. visible) end
          end
        end
      end
    end
  end
  local child = assert(vim.uv.spawn(vim.v.progpath, {
    args = { '--headless', '--embed', '-u', 'NONE' }, stdio = { input, output, errors },
  }, function(code) status = code end))
  output:read_start(function(err, data)
    assert(not err, err)
    if not data then return end
    local index = 1
    while index <= #data do
      local value
      value, index = unpacker(data, index)
      if value then message(value) end
    end
  end)
  errors:read_start(function(err, data) assert(not err, err); stderr = stderr .. (data or '') end)
  local function request(id, method, arguments)
    input:write(vim.mpack.encode({ 0, id, method, arguments }))
  end
  request(1, 'nvim_ui_attach', { 180, 45, { rgb = true, ext_linegrid = true } })
  assert(ReviewTest.wait(5000, function() return responses[1] end, 10), 'UI attachment timed out')
  assert(responses[1][3] == vim.NIL, vim.inspect(responses[1]))
  request(2, 'nvim_exec_lua', { "vim.env.BUF_REVIEW_DIFF_SWAP_CHILD = '1'; assert(loadfile('tests/nvim/run.lua'))('tests/nvim/diff_swap.lua')", {} })
  local exited = ReviewTest.wait(30000, function() return status ~= nil end, 10)
  if not exited then child:kill('sigterm') end
  output:read_stop(); errors:read_stop()
  for _, handle in ipairs({ input, output, errors, child }) do if not handle:is_closing() then handle:close() end end
  assert(exited and status == 0, 'embedded diff regression failed:\n' .. stderr)
  assert(not failure, failure)
  assert(old_frame and new_frame and swap_frames > 0, 'UI did not expose both complete comparisons')
  print('Neovim fresh diff swap and attached-UI frames passed')
  vim.cmd('qa!')
  return
end

local fixture = vim.fn.tempname()
vim.fn.mkdir(fixture, 'p')
vim.env.BUF_REVIEW_FIXTURE = fixture
vim.opt.runtimepath:prepend(repository)
local review = require('buf_review')
assert(debug.getinfo(review.setup, 'S').source == '@' .. repository .. '/lua/buf_review/init.lua',
  'regression loaded installed plugin instead of workspace source')
review.setup({ executable = repository .. '/tests/nvim/fixture_pr.py' })
vim.cmd('runtime plugin/buf_review.lua')
vim.o.hidden = true
vim.opt.diffopt = { 'internal', 'filler', 'closeoff', 'context:9999' }
local diffopt = vim.o.diffopt
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function write(name, value) vim.fn.writefile({ vim.json.encode(value) }, fixture .. '/' .. name) end
local function kind() return vim.b.buf_review and vim.b.buf_review.kind end
local function text(buf) return table.concat(api.nvim_buf_get_lines(buf or 0, 0, -1, false), '\n') end
local function wait(predicate, reason)
  assert(ReviewTest.wait(5000, predicate, 10), reason .. '\n' .. vim.inspect(notices) .. '\n' .. text())
  ReviewTest.wait(30)
end
local function row(path)
  for line, value in ipairs(api.nvim_buf_get_lines(0, 0, -1, false)) do
    if value:sub(-#path - 2) == '  ' .. path then api.nvim_win_set_cursor(0, { line, 0 }); return end
  end
  error('Missing file row ' .. path)
end
local revision = { base = string.rep('a', 40), head = string.rep('b', 40) }
local initial = {
  id = 'fresh-diff-swap', generation = 0, locator = 'https://github.com/acme/repo/pull/1', comparison = revision,
  review = { id = 'fresh-diff-snapshot', reviewer = { id = 'me', name = 'me' }, read = {
    pull_request = { id = 'PR-1', title = 'Fresh diff swap', revision = revision },
    files = {}, comments = {}, reactions = {}, reviews = {},
  } },
  comments = {}, files = {}, todo = {}, done = {}, pending = {}, conflicts = {}, publications = {}, new_commits = false,
}
local names = { 'a.scala', 'b.scala', 'one.rs', 'two.rs' }
local sources = {}
for index, path in ipairs(names) do
  initial.files[index] = { path = path, viewed = false }
  sources[path] = { path = path, revision = revision, binary = false,
    base_content = 'shared\nBASE ' .. (index == 4 and 'TWO' or 'ONE') .. '\nshared tail\n',
    head_content = 'shared\nHEAD ' .. (index == 4 and 'TWO' or 'ONE') .. '\nshared tail\n' }
  if index < 3 then
    sources[path].local_path = fixture .. '/' .. path
    local lines = {}
    for line = 1, 35 + index do lines[line] = ('Local %d only line %d'):format(index, line) end
    vim.fn.writefile(lines, sources[path].local_path)
  end
end
write('state.json', initial)
write('routes.json', { entry = { route = 'batch', batch = initial.id, fresh_review = initial.review.id } })
write('code_contexts.json', sources)
local function open(path)
  vim.cmd('Review'); wait(function() return kind() == 'home' end, 'home missing')
  row(path); require('buf_review').enter()
  wait(function() return kind() == 'diff' and vim.b.buf_review.path == path end, 'diff missing ' .. path)
end
local function pair()
  local result = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    local meta = vim.b[buf].buf_review
    if meta and meta.kind == 'diff' then
      assert(vim.wo[win].diff, 'comparison pane lost native diff mode')
      result[meta.side] = { window = win, buffer = buf }
    end
  end
  assert(result.base and result.head, 'complete Base/Head pair missing')
  return result
end
local hidden = {}
for index = 1, 2 do
  open(names[index])
  vim.cmd('normal o')
  vim.cmd('wincmd l')
  local win, buf = api.nvim_get_current_win(), api.nvim_get_current_buf()
  assert(api.nvim_buf_get_name(buf) == sources[names[index]].local_path and vim.wo.diff, 'Local pane missing')
  api.nvim_buf_set_lines(buf, 0, 1, false, { 'Unsaved Local ' .. index })
  hidden[index] = { buffer = buf, content = text(buf) }
  vim.cmd('enew')
  assert(api.nvim_win_get_buf(win) ~= buf and #vim.fn.win_findbuf(buf) == 0, 'Local buffer was not hidden')
  assert(api.nvim_buf_is_loaded(buf) and vim.bo[buf].modified and text(buf) == hidden[index].content,
    'leaving Local discarded native edits')
end
open('one.rs')
for _, pane in pairs(pair()) do
  api.nvim_win_call(pane.window, function()
    for line = 1, api.nvim_buf_line_count(pane.buffer) + 1 do
      assert(vim.fn.diff_filler(line) == 0, 'hidden Local buffer polluted clean comparison with filler')
    end
  end)
end
for _, old in ipairs(hidden) do
  assert(api.nvim_buf_is_loaded(old.buffer) and vim.bo[old.buffer].modified and text(old.buffer) == old.content,
    'new comparison discarded hidden Local edits')
end
assert(vim.o.diffopt == diffopt and not vim.tbl_contains(vim.opt.diffopt:get(), 'hiddenoff'), 'cleanup changed global diff options')

-- A native diff of the same Local buffer in another tab is independently owned.
local page = api.nvim_get_current_tabpage()
vim.cmd('tabnew')
local extra_page, extra_win = api.nvim_get_current_tabpage(), api.nvim_get_current_win()
api.nvim_win_set_buf(extra_win, hidden[2].buffer)
vim.cmd('diffthis')
api.nvim_set_current_tabpage(page)
open('b.scala'); vim.cmd('normal o'); vim.cmd('wincmd l'); vim.cmd('enew')
ReviewTest.wait(40)
assert(vim.wo[extra_win].diff and api.nvim_win_get_buf(extra_win) == hidden[2].buffer,
  'Local cleanup disabled unrelated diff of same file')
api.nvim_set_current_tabpage(extra_page); vim.cmd('tabclose'); api.nvim_set_current_tabpage(page)

open('one.rs')
local previous = pair()
vim.rpcnotify(1, 'diff_swap_phase', 'old')
vim.cmd('redraw!')
ReviewTest.wait(80)
vim.fn.writefile({ '0.15' }, fixture .. '/delay_code')
vim.rpcnotify(1, 'diff_swap_phase', 'swap')
vim.cmd('Review next file')
assert(vim.b.buf_review.path == 'one.rs' and api.nvim_buf_is_valid(previous.base.buffer)
  and api.nvim_buf_is_valid(previous.head.buffer), 'old comparison destroyed before new source was fetched')
wait(function() return kind() == 'diff' and vim.b.buf_review.path == 'two.rs' end, 'replacement diff missing')
local current = pair()
assert(current.base.window == previous.base.window and current.head.window == previous.head.window, 'swap recreated comparison windows')
assert(current.base.buffer ~= previous.base.buffer and current.head.buffer ~= previous.head.buffer, 'new comparison reused source buffers')
assert(not api.nvim_buf_is_valid(previous.base.buffer) and not api.nvim_buf_is_valid(previous.head.buffer), 'old comparison source survived swap')
vim.cmd('redraw!')
ReviewTest.wait(80)
vim.rpcnotify(1, 'diff_swap_phase', 'done')

-- Same-tab duplicates prevent BufWinLeave for the outgoing file. Clear only
-- the managed window, preserving a user diff and the replacement's wipe policy.
open('a.scala'); vim.cmd('normal o'); vim.cmd('wincmd l')
local managed, local_buffer = api.nvim_get_current_win(), api.nvim_get_current_buf()
vim.cmd('split')
local duplicate = api.nvim_get_current_win()
api.nvim_set_current_win(managed)
local replacement = api.nvim_create_buf(false, true)
vim.bo[replacement].bufhidden = 'wipe'
api.nvim_buf_set_lines(replacement, 0, -1, false, { 'Replacement buffer must survive cleanup' })
api.nvim_win_set_buf(managed, replacement)
assert(api.nvim_buf_is_valid(replacement) and api.nvim_win_get_buf(managed) == replacement
  and vim.bo[replacement].bufhidden == 'wipe' and text(replacement) == 'Replacement buffer must survive cleanup',
  'Local cleanup wiped or modified newly entered buffer')
assert(api.nvim_win_get_buf(duplicate) == local_buffer and vim.wo[duplicate].diff,
  'Local cleanup disabled same-tab user diff')

-- Once a user disabled diff, leaving that window must preserve subsequent
-- native option edits rather than issuing diffoff for the same buffer again.
open('a.scala'); vim.cmd('normal o'); vim.cmd('wincmd l')
managed, local_buffer = api.nvim_get_current_win(), api.nvim_get_current_buf()
vim.cmd('diffoff')
vim.wo.foldmethod, vim.wo.wrap = 'indent', false
vim.cmd('enew')
api.nvim_win_set_buf(managed, local_buffer)
assert(vim.wo.foldmethod == 'indent' and vim.wo.wrap == false,
  'Leaving an already-disabled Local pane reset native option changes')
assert(api.nvim_win_get_buf(duplicate) == local_buffer and vim.wo[duplicate].diff,
  'Already-disabled Local cleanup altered duplicate user diff')
api.nvim_win_close(duplicate, true)
vim.fn.delete(fixture, 'rf')
vim.cmd('qa!')
