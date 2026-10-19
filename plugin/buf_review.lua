if vim.g.loaded_buf_review then
  return
end
vim.g.loaded_buf_review = true

if vim.fn.has('nvim-0.10') == 0 then
  vim.notify('buf-review requires Neovim 0.10 or newer', vim.log.levels.ERROR)
  return
end

require('buf_review').register()
