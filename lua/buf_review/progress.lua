local M = {}
local api = vim.api
local active

function M.start(title)
  local tab = api.nvim_get_current_tabpage()
  local job = {}
  active = job
  local function show(message)
    if active ~= job or job.hidden or api.nvim_get_current_tabpage() ~= tab or vim.fn.getcmdtype() ~= '' then return end
    api.nvim_echo({ { 'buf-review: ' .. title .. ' · ' .. message } }, false, {})
    vim.cmd('redraw')
  end
  job.update = function(event)
    local message = event.message:gsub('[\r\n]+', ' ')
    if event.completed ~= nil and event.total ~= nil then
      message = ('%s %d/%d'):format(message, event.completed, event.total)
    end
    show(message)
  end
  job.finish = function(err)
    show(err and 'Finished with errors' or 'Complete')
    if active == job then active = nil end
  end
  return job
end

return M
