-- Separate editor process: shutdown and final-window close must never submit.
local api = vim.api
vim.opt.runtimepath:prepend(vim.fn.getcwd())
require('buf_review').setup({ executable = vim.fn.getcwd() .. '/tests/nvim/fixture_pr.py' })
vim.notify = function() end
vim.cmd('runtime plugin/buf_review.lua')
vim.cmd('Review')
assert(ReviewTest.wait(5000, function() return vim.b.buf_review and vim.b.buf_review.kind == 'home' end, 10))
ReviewTest.wait(100)
vim.ui.select = function(_, _, callback) callback('approve') end
vim.cmd('Review conclude')
assert(ReviewTest.wait(5000, function() return vim.b.buf_review and vim.b.buf_review.kind == 'composer' end, 10))
api.nvim_buf_set_lines(0, 0, -1, false, { 'Saved before editor shutdown' })
api.nvim_feedkeys(':w\r', 'xt', false)
local operation = vim.env.BUF_REVIEW_SHUTDOWN
if operation == 'last' then
  local current = api.nvim_get_current_win()
  for _, win in ipairs(api.nvim_list_wins()) do if win ~= current then api.nvim_win_close(win, true) end end
  api.nvim_feedkeys(':q\r', 'xt', false)
else
  api.nvim_feedkeys(':' .. operation .. '!\r', 'xt', false)
end
error('Editor shutdown did not exit')
