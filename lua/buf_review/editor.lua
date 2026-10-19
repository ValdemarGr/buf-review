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
  if editor.saving then
    error('Review save in progress; text retained')
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
      editor.saved_body = text
      if body(buf) == text then
        vim.bo[buf].modified = false
      end
    end
  end)
  -- BufWriteCmd must finish the local write before native :w can succeed.
  -- Provider operations run only through explicit publication.
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
      for saved_win, saved in pairs(editor.return_views or {}) do
        if api.nvim_win_is_valid(saved_win) and api.nvim_win_get_buf(saved_win) == saved.buffer then
          api.nvim_win_call(saved_win, function() vim.fn.winrestview(saved.view) end)
        end
      end
      api.nvim_set_current_win(origin)
    end
  end
end

function M.close(buf, command, force, win)
  local editor = editors[buf]
  if not editor then return end
  win = win or api.nvim_get_current_win()
  if editor.saving then
    error('Review save in progress; keep composer open until result is known')
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
  close_window(buf, force, win, editor)
end

function M.attach(buf, editor)
  editors[buf] = editor
  editor.saved_body = body(buf)
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
