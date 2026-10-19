local M = {}
local api, uv = vim.api, vim.uv or vim.loop
local running = {}
local paths = { '.', ':(top,exclude).review' }

local function command(root, arguments, callback)
  local ok, process = pcall(vim.system, arguments, { cwd = root, text = false }, function(result)
    vim.schedule(function()
      local err = result.code ~= 0 and (vim.trim(result.stderr or '') ~= '' and vim.trim(result.stderr)
        or vim.trim(result.stdout or '') ~= '' and vim.trim(result.stdout)
        or (arguments[1] .. ' exited ' .. result.code)) or nil
      callback(result, err)
    end)
  end)
  if not ok then callback(nil, tostring(process)) end
end

local function git(root, arguments, callback)
  command(root, vim.list_extend({ 'git' }, arguments), callback)
end

local function repository_url(value)
  if not value then return nil end
  return value:gsub('^%w+://', ''):gsub('^[^/@]+@', ''):gsub(':', '/'):gsub('/+$', ''):gsub('%.git$', ''):lower()
end

local function normal_buffers(root)
  local result = {}
  for _, buf in ipairs(api.nvim_list_bufs()) do
    if api.nvim_buf_is_loaded(buf) and vim.bo[buf].buftype == '' then
      local name = api.nvim_buf_get_name(buf)
      name = vim.fn.resolve(vim.fs.normalize(name))
      if name:sub(1, #root + 1) == root .. '/' then result[#result + 1] = buf end
    end
  end
  return result
end

local function validate(root, options)
  if options.validate then
    local called, ok, err = pcall(options.validate)
    if not called then return tostring(ok) end
    if ok == false then return err or 'Checkout cancelled: review context changed' end
  end
  for _, buf in ipairs(normal_buffers(root)) do
    if vim.bo[buf].modified then
      return 'Save or discard unsaved buffer before checkout: ' .. api.nvim_buf_get_name(buf)
    end
  end
end

function M.inspect(root, pull, callback)
  root = uv.fs_realpath(root) or vim.fs.normalize(root)
  git(root, vim.list_extend({ '--no-optional-locks', 'status', '--porcelain=v2', '--branch', '-z', '--untracked-files=all', '--' }, paths), function(result, err)
    if err then callback(nil, err); return end
    local status = { staged = 0, modified = 0, untracked = 0, conflicted = 0, relationship = 'unknown', matches = false }
    local entries = vim.split(result.stdout, '\0', { plain = true, trimempty = true })
    local skip = false
    for _, entry in ipairs(entries) do
      if skip then
        skip = false
      elseif entry:sub(1, 13) == '# branch.oid ' then
        status.head = entry:sub(14)
      elseif entry:sub(1, 14) == '# branch.head ' then
        status.branch = entry:sub(15)
      elseif entry:sub(1, 2) == '? ' then
        status.untracked = status.untracked + 1
      elseif entry:sub(1, 2) == 'u ' then
        status.conflicted = status.conflicted + 1
      elseif entry:sub(1, 2) == '1 ' or entry:sub(1, 2) == '2 ' then
        if entry:sub(3, 3) ~= '.' then status.staged = status.staged + 1 end
        if entry:sub(4, 4) ~= '.' then status.modified = status.modified + 1 end
        skip = entry:sub(1, 1) == '2'
      end
    end
    git(root, { 'rev-parse', '--absolute-git-dir' }, function(directory, directory_err)
      if directory_err then callback(nil, directory_err); return end
      local gitdir = vim.trim(directory.stdout)
      for _, operation in ipairs({ 'MERGE_HEAD', 'rebase-merge', 'rebase-apply', 'CHERRY_PICK_HEAD', 'REVERT_HEAD', 'sequencer' }) do
        if uv.fs_stat(gitdir .. '/' .. operation) then status.operation = operation; break end
      end
      local head = pull.revision and pull.revision.head
      local function relationship()
        if status.head == head then
          status.relationship, status.matches = 'at_pr', true
          callback(status)
        elseif not head or not head:match('^%x+$') or status.head == '(initial)' then
          callback(status)
        else
          git(root, { 'rev-list', '--left-right', '--count', head .. '...HEAD' }, function(counts)
            local behind, ahead
            if counts then behind, ahead = counts.stdout:match('^(%d+)%s+(%d+)') end
            if behind then
              status.behind, status.ahead = tonumber(behind), tonumber(ahead)
              status.relationship = status.behind == 0 and 'ahead' or status.ahead == 0 and 'behind' or 'diverged'
              status.matches = status.source_match == true and status.relationship == 'ahead'
            end
            callback(status)
          end)
        end
      end
      local source = pull.source
      if not source or status.branch ~= source.branch then relationship(); return end
      git(root, { 'config', '--get', 'branch.' .. status.branch .. '.remote' }, function(remote, remote_err)
        if remote_err then relationship(); return end
        git(root, { 'config', '--get', 'branch.' .. status.branch .. '.merge' }, function(merge, merge_err)
          if merge_err or vim.trim(merge.stdout) ~= 'refs/heads/' .. source.branch then relationship(); return end
          local name = vim.trim(remote.stdout)
          git(root, { 'remote', 'get-url', name }, function(url, url_err)
            status.source_match = repository_url(not url_err and vim.trim(url.stdout) or name) == repository_url(source.repository)
            relationship()
          end)
        end)
      end)
    end)
  end)
end

function M.run(root, pull, options, callback)
  root, options = uv.fs_realpath(root) or vim.fs.normalize(root), options or {}
  if type(pull.url) ~= 'string' or not pull.url:match('^https?://') then
    callback(nil, 'Checkout requires PR URL'); return
  end
  if running[root] then callback(nil, 'Checkout already in progress'); return end
  running[root] = true
  local stash, mutated
  local function finish(status, err)
    -- Git commands can change files before a later step fails or is cancelled.
    if mutated then
      for _, buf in ipairs(normal_buffers(root)) do
        if not vim.bo[buf].modified then
          local autoread = vim.bo[buf].autoread
          vim.bo[buf].autoread = true
          local ok, reload_err = pcall(api.nvim_buf_call, buf, function() vim.cmd('silent checktime') end)
          if api.nvim_buf_is_valid(buf) then vim.bo[buf].autoread = autoread end
          if not ok then err = err or tostring(reload_err); break end
        end
      end
    end
    running[root] = nil
    if stash then
      status = status or {}
      status.stash = stash
    end
    callback(status, err)
  end
  local function blocked(status)
    return validate(root, options) or (status.operation and 'Finish Git operation before checkout: ' .. status.operation)
      or (status.conflicted > 0 and 'Resolve Git conflicts before checkout') or nil
  end
  local function checkout(status)
    local err = blocked(status)
    if err then finish(status, err); return end
    mutated = true
    command(root, { 'gh', 'pr', 'checkout', pull.url }, function(_, checkout_err)
      M.inspect(root, pull, function(current, inspect_err) finish(current or status, checkout_err or inspect_err) end)
    end)
  end
  local choose
  local function prepare(original, choice)
    M.inspect(root, pull, function(status, err)
      if err then finish(nil, err); return end
      err = blocked(status)
      if err then finish(status, err); return end
      if status.branch ~= original.branch or status.head ~= original.head then
        finish(status, 'Checkout cancelled: local branch changed'); return
      end
      local dirty = status.staged + status.modified + status.untracked > 0
      if dirty and not choice then choose(status); return end
      if choice ~= 'Stash & switch' or not dirty then checkout(status); return end
      local message = 'buf-review: ' .. (status.branch or 'detached HEAD') .. ' before ' .. pull.url
      git(root, { 'rev-parse', '--verify', '--quiet', 'refs/stash' }, function(previous, previous_err)
        if previous_err and (not previous or previous.code ~= 1) then finish(status, previous_err); return end
        err = blocked(status)
        if err then finish(status, err); return end
        mutated = true
        git(root, vim.list_extend({ 'stash', 'push', '--include-untracked', '--message', message, '--' }, paths), function(_, stash_err)
          if stash_err then finish(status, stash_err); return end
          git(root, { 'rev-parse', '--verify', '--quiet', 'refs/stash' }, function(saved, saved_err)
            if saved_err and (not saved or saved.code ~= 1) then finish(status, saved_err); return end
            if not saved_err and (previous_err or saved.stdout ~= previous.stdout) then
              stash = { oid = vim.trim(saved.stdout), message = message }
            end
            M.inspect(root, pull, function(current, current_err)
              if current_err then finish(status, current_err); return end
              if current.staged + current.modified + current.untracked > 0 then
                finish(current, 'Local changes remain after stashing' .. (stash and '; stash retained' or '')); return
              end
              checkout(current)
            end)
          end)
        end)
      end)
    end)
  end
  choose = function(status)
    vim.ui.select({ 'Cancel', 'Stash & switch', 'Carry changes & switch' }, {
      prompt = 'Checkout PR: local changes',
    }, function(choice)
      if not choice or choice == 'Cancel' then status.cancelled = true; finish(status); return end
      prepare(status, choice)
    end)
  end
  M.inspect(root, pull, function(status, err)
    if err then finish(nil, err); return end
    err = blocked(status)
    if err then finish(status, err); return end
    if status.staged + status.modified + status.untracked > 0 then choose(status) else prepare(status) end
  end)
end

return M
