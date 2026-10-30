local M = {}

-- Rust owns provider operations and business rules. Lua sends typed requests;
-- paths and comment bodies are never interpolated into a shell command.
function M.request(root, request, callback, on_progress)
  local executable = vim.g.buf_review_executable or 'pr'
  if vim.fn.executable(executable) == 0 then
    callback(nil, 'Missing executable: ' .. executable .. ' (install the matching buf-review CLI)')
    return
  end
  local pending, diagnostics = '', {}
  local function line(value)
    local decoded, event = pcall(vim.json.decode, value)
    if decoded and type(event) == 'table' and event.event == 'progress'
      and type(event.stage) == 'string' and type(event.message) == 'string'
      and (event.completed == nil or type(event.completed) == 'number' and event.completed >= 0 and event.completed % 1 == 0)
      and (event.total == nil or type(event.total) == 'number' and event.total >= 0 and event.total % 1 == 0)
      and (event.completed == nil or event.total == nil or event.completed <= event.total) then
      if on_progress then vim.schedule(function() on_progress(event) end) end
    else diagnostics[#diagnostics + 1] = value end
  end
  local function stderr(err, data)
    if err then diagnostics[#diagnostics + 1] = tostring(err) end
    pending = pending .. (data or '')
    while true do
      local at = pending:find('\n', 1, true)
      if not at then break end
      line(pending:sub(1, at - 1))
      pending = pending:sub(at + 1)
    end
  end
  local ok, process = pcall(vim.system, { executable, 'workspace' }, {
    cwd = root,
    stdin = vim.json.encode(request),
    text = true,
    stderr = stderr,
  }, function(result)
    if pending ~= '' then line(pending); pending = '' end
    vim.schedule(function()
      if result.code ~= 0 then
        local diagnostic = vim.trim(table.concat(diagnostics, '\n'))
        callback(nil, diagnostic ~= '' and diagnostic or ('pr exited ' .. result.code))
        return
      end
      local decoded, response = pcall(vim.json.decode, result.stdout, { luanil = { object = true, array = true } })
      if not decoded or type(response) ~= 'table' then
        callback(nil, 'Invalid JSON response from pr: ' .. tostring(response))
      elseif response.error then
        callback(response, type(response.error) == 'string' and response.error or vim.inspect(response.error))
      else
        callback(response)
      end
    end)
  end)
  if not ok then
    callback(nil, 'Cannot run ' .. executable .. ': ' .. tostring(process))
  end
  return ok and process or nil
end

return M
