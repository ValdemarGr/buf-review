local M = {}

function M.ready()
  return vim.fn.state('moS') == ''
end

-- A session has one writer. Native commands collect intent without changing the
-- reader they are traversing; each process receives the latest known generation.
function M.new(send)
  local pending, running, scheduled = {}, false, false
  local failures, last_error = 0, nil
  local drain
  local function schedule()
    if scheduled then return end
    scheduled = true
    vim.schedule(function() scheduled = false; drain() end)
  end
  drain = function(force)
    if running or #pending == 0 then return end
    if not force and not M.ready() then return end
    local groups, entries = {}, {}
    local command = pending[1].command
    if command then
      groups[1] = table.remove(pending, 1)
      groups[1].first = 0
    else
      while pending[1] and not pending[1].command do
        local group = table.remove(pending, 1)
        group.first = #entries
        vim.list_extend(entries, group.entries)
        groups[#groups + 1] = group
      end
    end
    running = true
    send(command or { op = 'actions', actions = entries }, function(response, err)
      running = false
      local stopped = err and not (command and response and response.workspace)
      if stopped then failures, last_error = failures + 1, err end
      local canceled = stopped and pending or {}
      if stopped then pending = {} end
      local failed = response and response.failed_index or 0
      local reported = false
      local function deliver(group, problem, report)
        local ok, failure = pcall(group.receive, response, problem, report)
        if not ok then vim.notify('buf-review: ' .. tostring(failure), vim.log.levels.ERROR) end
      end
      for _, group in ipairs(groups) do
        local failure = err and group.first + (group.entries and #group.entries or 1) > failed
        local report = failure and not reported
        reported = reported or failure
        deliver(group, failure and err or nil, report)
      end
      for _, group in ipairs(canceled) do
        deliver(group, 'Earlier review action failed; remaining actions cancelled', false)
      end
      if #pending > 0 then schedule() end
    end)
  end
  return {
    flush = drain,
    enqueue = function(entries, receive)
      pending[#pending + 1] = { entries = entries, receive = receive }
      schedule()
    end,
    enqueue_request = function(command, receive)
      pending[#pending + 1] = { command = command, receive = receive }
      schedule()
    end,
    wait = function()
      if not running and #pending == 0 then return end
      local before = failures
      drain(true)
      if not vim.wait(10000, function()
        if not running then drain(true) end
        return not running and #pending == 0
      end, 10) then
        error('Local review actions still running; wait before continuing')
      end
      if failures > before then error(last_error) end
    end,
  }
end

return M
