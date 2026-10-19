local M = {}

-- Rust owns provider operations and business rules. Lua sends typed requests;
-- paths and comment bodies are never interpolated into a shell command.
function M.request(root, request, callback)
  local executable = vim.g.buf_review_executable or 'pr'
  if vim.fn.executable(executable) == 0 then
    callback(nil, 'Missing executable: ' .. executable .. ' (install the matching buf-review CLI)')
    return
  end
  local ok, process = pcall(vim.system, { executable, 'workspace' }, {
    cwd = root,
    stdin = vim.json.encode(request),
    text = true,
  }, function(result)
    vim.schedule(function()
      if result.code ~= 0 then
        callback(nil, vim.trim(result.stderr or '') ~= '' and vim.trim(result.stderr) or ('pr exited ' .. result.code))
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
