local path = ...
local native_wait, task = vim.wait
-- A test waiting from its Lua driver must return control to the actual main
-- loop. Native mappings and production callbacks keep Neovim's blocking wait;
-- they must still expose redraws that happen inside commands and macros.
_G.ReviewTest = { wait = function(timeout, predicate, interval)
  if coroutine.running() ~= task then return native_wait(timeout, predicate, interval) end
  local done, code, failure = coroutine.yield({ timeout = timeout, predicate = predicate, interval = interval or 10 })
  if failure then error(failure) end
  return done, code
end }
local function resume(...)
  local ok, request = coroutine.resume(task, ...)
  if not ok then
    io.stderr:write(debug.traceback(task, request) .. '\n')
    vim.cmd('cquit')
    return
  end
  if coroutine.status(task) == 'dead' then return end
  local started = vim.uv.hrtime()
  local function poll()
    if request.predicate then
      local success, done = pcall(request.predicate)
      if not success then resume(false, nil, done); return end
      if done then resume(true); return end
    end
    if (vim.uv.hrtime() - started) / 1e6 >= request.timeout then resume(false, -1); return end
    vim.defer_fn(poll, request.interval)
  end
  vim.defer_fn(poll, 0)
end
task = coroutine.create(assert(loadfile(path)))
vim.schedule(resume)
