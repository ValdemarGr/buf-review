local M = {}
local api = vim.api
local editors = {}

local function body(buf)
  return table.concat(api.nvim_buf_get_lines(buf, 0, -1, false), '\n')
end

function M.get(buf)
  return editors[buf or api.nvim_get_current_buf()]
end

function M.retains(buf)
  for _, editor in pairs(editors) do
    if editor.return_buf == buf then return true end
  end
  return false
end

function M.save(buf)
  local editor = assert(editors[buf], 'Not a review composer')
  if editor.completed then error('Review already submitted; close this composer before starting another review') end
  if editor.submitting or editor.saving then
    error('Review save/submission in progress; text retained')
  end
  if editor.before_write then editor.before_write() end
  local text = body(buf)
  local finished, failure = false, nil
  editor.saving = true
  editor.save(text, function(err)
    editor.saving = false
    failure = err
    finished = true
    if not err and api.nvim_buf_is_valid(buf) then
      editor.saved_in_open = true
      editor.saved_body = text
      if body(buf) == text then
        vim.bo[buf].modified = false
      end
    end
  end)
  -- BufWriteCmd must finish the local write before native :w can succeed.
  -- Provider operations run only on explicit publication or conclusion close.
  if not vim.wait(10000, function() return finished end, 10) then
    error('Local save still running; text retained. Wait before retrying.')
  end
  if failure then
    error(failure)
  end
end

local function close_window(buf, force, win, editor)
  local focused = api.nvim_get_current_win() == win
  if api.nvim_win_is_valid(win) and api.nvim_win_get_buf(win) == buf then
    if editor.return_buf and api.nvim_buf_is_valid(editor.return_buf) then
      api.nvim_win_call(win, function()
        api.nvim_cmd({ cmd = 'buffer', args = { tostring(editor.return_buf) }, bang = force or false, mods = { keepjumps = true } }, {})
        if editor.return_view then vim.fn.winrestview(editor.return_view) end
      end)
      return
    end
    api.nvim_win_close(win, force)
    local origin = editor.return_win
    if focused and #vim.fn.win_findbuf(buf) == 0 and origin and api.nvim_win_is_valid(origin) then
      api.nvim_set_current_win(origin)
    end
  end
end

function M.close(buf, command, force, win)
  local editor = editors[buf]
  if not editor then return end
  win = win or api.nvim_get_current_win()
  if editor.submitting or editor.saving then
    error('Review save/submission in progress; keep composer open until result is known')
  end
  local windows = 0
  for _, win in ipairs(api.nvim_list_wins()) do
    if api.nvim_win_get_config(win).relative == '' then windows = windows + 1 end
  end
  if windows == 1 and not (editor.return_buf and api.nvim_buf_is_valid(editor.return_buf)) then
    -- Completing the whole editor never submits, even after a prior :w.
    api.nvim_cmd({ cmd = command == 'x' and 'xit' or command, bang = force or false }, {})
    return
  end
  if command == 'wq' or command == 'wquit' then
    M.save(buf)
  elseif (command == 'x' or command == 'xit' or command == 'exit') and vim.bo[buf].modified then
    M.save(buf)
  end
  if vim.bo[buf].modified and not force then
    error('E37: No write since last change (add ! to override)')
  end
  local complete = command == 'wq' or command == 'wquit' or command == 'x' or command == 'xit' or command == 'exit'
  if editor.kind ~= 'conclusion' or not complete or not editor.saved_in_open then
    close_window(buf, force, win, editor)
    return
  end
  editor.submitting = true
  editor.status('Submitting review…')
  editor.submit(function(err)
    editor.submitting = false
    if not api.nvim_buf_is_valid(buf) then return end
    if err then
      editor.status('Submission failed: ' .. err)
      vim.notify('buf-review: ' .. err, vim.log.levels.ERROR)
    else
      editor.completed = true
      editor.saved_in_open = false
      editor.status('Review submitted')
      close_window(buf, false, win, editor)
    end
  end)
end

function M.attach(buf, editor)
  editors[buf] = editor
  editor.saved_body = body(buf)
  editor.saved_in_open = false
  vim.bo[buf].buftype = 'acwrite'
  vim.bo[buf].bufhidden = 'wipe'
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = 'markdown'
  vim.bo[buf].modifiable = true
  vim.bo[buf].readonly = false
  api.nvim_create_autocmd('BufWriteCmd', {
    buffer = buf,
    callback = function() M.save(buf) end,
    desc = 'Save review text locally',
  })
  api.nvim_create_autocmd('BufWipeout', {
    buffer = buf,
    once = true,
    callback = function() editors[buf] = nil end,
  })
  api.nvim_buf_create_user_command(buf, 'ReviewClose', function(args)
    local command = args.fargs[1] or 'q'
    local force = args.bang or command:sub(-1) == '!'
    command = command:gsub('!$', '')
    M.close(buf, command, force)
  end, { nargs = 1, bang = true })
  -- Explicit interactive close is distinct from buffer teardown, :qall, or
  -- editor shutdown. No unload/close autocommand initiates provider writes.
  vim.keymap.set('n', 'ZZ', function() M.close(buf, 'x', false) end, { buffer = buf, silent = true })
  vim.keymap.set('n', 'ZQ', function() M.close(buf, 'q', true) end, { buffer = buf, silent = true })
end

api.nvim_create_autocmd('CmdlineLeave', {
  pattern = ':',
  -- Synchronous closes must still trigger buffer/window lifecycle cleanup.
  nested = true,
  callback = function()
    local buf = api.nvim_get_current_buf()
    local win = api.nvim_get_current_win()
    if not editors[buf] or vim.v.event.abort then return end
    local ok, command = pcall(api.nvim_parse_cmd, vim.fn.getcmdline(), {})
    if not ok or command.nextcmd ~= '' or #command.args > 0 then return end
    if command.cmd ~= 'quit' and command.cmd ~= 'wq' and command.cmd ~= 'xit' and command.cmd ~= 'exit' then return end
    vim.cmd('let v:event.abort = v:true')
    -- Finish the close before Vim consumes the next macro/:normal command.
    -- Scheduling this loses the originating buffer when queued keys continue.
    local success, err = pcall(M.close, buf, command.cmd, command.bang, win)
    if not success then vim.notify('buf-review: ' .. err, vim.log.levels.ERROR) end
  end,
  desc = 'Complete review composer only through explicit user close',
})

return M
