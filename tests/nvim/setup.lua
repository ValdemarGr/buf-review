local api = vim.api
local repository, directory = vim.fn.getcwd(), vim.fn.tempname() .. ' executable paths'
vim.fn.mkdir(directory, 'p')
vim.opt.runtimepath:prepend(repository)
local function executable(name, selected)
  local path = directory .. '/' .. name
  vim.fn.writefile({ '#!/bin/sh', "printf '%s\\n' '" .. vim.json.encode({ selected = selected }) .. "'" }, path)
  assert(vim.fn.setfperm(path, 'rwxr-xr-x') == 1)
  return path
end
executable('pr', 'default')
local early, late = executable('early CLI', 'early'), executable('late CLI', 'late')
vim.env.PATH = directory .. ':' .. vim.env.PATH
local review, client = require('buf_review'), require('buf_review.client')
local function selected()
  local finished, result, failure = false
  client.request(repository, { op = 'fixture' }, function(response, err)
    result, failure, finished = response, err, true
  end)
  assert(ReviewTest.wait(5000, function() return finished end, 10), 'configured CLI request did not finish')
  assert(not failure, failure)
  return result.selected
end

assert(selected() == 'default', 'default CLI on PATH required setup')
assert(vim.fn.exists(':Review') == 0, 'requiring module registered command before plugin load')
review.setup({ executable = early })
vim.cmd('runtime plugin/buf_review.lua')
assert(vim.fn.exists(':Review') == 2 and selected() == 'early', 'setup before plugin load lost configured executable')
local autocmds, mappings = #api.nvim_get_autocmds({}), #api.nvim_get_keymap('n')
review.setup()
review.setup({})
assert(selected() == 'early', 'empty setup reset configured/package default')
review.setup({ executable = late })
vim.cmd('runtime plugin/buf_review.lua')
assert(selected() == 'late', 'setup after plugin load did not change subsequent CLI requests')
assert(#api.nvim_get_autocmds({}) == autocmds and #api.nvim_get_keymap('n') == mappings,
  'setup duplicated commands, mappings or autocmds')
for _, opts in ipairs({ false, 'invalid', { executable = 42 }, { executable = false } }) do
  assert(not pcall(review.setup, opts), 'invalid executable configuration was accepted')
end
assert(selected() == 'late', 'invalid setup changed valid configuration')
vim.fn.delete(directory, 'rf')
print('Neovim optional Lua setup passed')
vim.cmd('qa!')
