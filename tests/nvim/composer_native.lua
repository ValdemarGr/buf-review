-- Native command composition must finish each local composer before consuming
-- the remaining command stream. No remote provider is involved in this fixture.
local api = vim.api
vim.opt.runtimepath:prepend(vim.fn.getcwd())
local editor = require('buf_review.editor')
local origin = api.nvim_get_current_buf()
api.nvim_buf_set_lines(origin, 0, -1, false, { 'first', 'second', 'third' })
vim.bo[origin].modified = false
local saves, barriers, errors = {}, 0, {}
vim.notify = function(message) errors[#errors + 1] = tostring(message) end
local fail, submitting, opened = false, 0, {}

local function composer(kind)
  local source = api.nvim_get_current_buf()
  local view = vim.fn.winsaveview()
  local line = api.nvim_win_get_cursor(0)[1]
  local buf = api.nvim_create_buf(false, true)
  opened[#opened + 1] = buf
  api.nvim_buf_set_name(buf, 'review-native://' .. buf)
  editor.attach(buf, {
    kind = kind or 'comment', return_buf = source, return_view = view,
    before_write = function() barriers = barriers + 1 end,
    save = function(text, callback)
      vim.defer_fn(function()
        if fail then callback('Fixture local save failed'); return end
        saves[#saves + 1] = { text = text, line = line }
        callback()
      end, 10)
    end,
    status = function() end,
    submit = function(callback) submitting = submitting + 1; callback() end,
  })
  api.nvim_set_current_buf(buf)
  vim.bo[buf].modified = false
  return buf
end

vim.keymap.set('n', 'a', function()
  composer()
  -- Native typeahead insertion precedes the remaining :normal/macro keys.
  api.nvim_feedkeys('i', 'in', false)
end, { buffer = origin })

local function normal(keys)
  vim.cmd.normal({ api.nvim_replace_termcodes(keys, true, false, true), bang = false })
end
local function input(keys)
  api.nvim_feedkeys(api.nvim_replace_termcodes(keys, true, false, true), 'xt', false)
end

-- One :normal sequence opens, types, writes, returns, then moves in origin.
normal('aFirst comment<Esc>:wq<CR>j')
assert(#saves == 1 and saves[1].text == 'First comment', 'normal append did not finish its local write')
assert(api.nvim_get_current_buf() == origin and api.nvim_win_get_cursor(0)[1] == 2, 'normal continued before restoring origin')
assert(not api.nvim_buf_is_valid(opened[1]) and not editor.get(opened[1]), 'native close suppressed buffer lifecycle cleanup')

-- Native global iteration retains its source marks across composer switches.
local before = #saves
vim.cmd('global/./normal ' .. api.nvim_replace_termcodes('aGlobal comment<Esc>:wq<CR>', true, false, true))
assert(#saves == before + 3, 'global lost matched source lines')
for offset = 1, 3 do
  assert(saves[before + offset].text == 'Global comment' and saves[before + offset].line == offset, 'global changed source order or composer text')
end
assert(api.nvim_get_current_buf() == origin, 'global did not return to source')

-- Ranged :normal and repeated register execution use the same close boundary.
api.nvim_win_set_cursor(0, { 1, 0 })
before = #saves
vim.cmd('1,2normal ' .. api.nvim_replace_termcodes('aRange comment<Esc>:x<CR>', true, false, true))
assert(#saves == before + 2 and saves[before + 1].line == 1 and saves[before + 2].line == 2, 'ranged normal lost source selection')
vim.fn.setreg('q', api.nvim_replace_termcodes('aMacro comment<Esc>:wq<CR>j', true, false, true))
api.nvim_win_set_cursor(0, { 1, 0 })
before = #saves
normal('2@q')
assert(#saves == before + 2 and saves[before + 1].line == 1 and saves[before + 2].line == 2, 'macro continued before composer close')
assert(api.nvim_get_current_buf() == origin and api.nvim_win_get_cursor(0)[1] == 3, 'macro navigation escaped origin')

-- Native write only waits for local persistence and keeps composer open.
local buf = composer()
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Saved locally' })
vim.cmd.write()
assert(not vim.bo[buf].modified and api.nvim_get_current_buf() == buf, 'write did not finish locally')
input(':q<CR>')
assert(api.nvim_get_current_buf() == origin and submitting == 0, 'plain quit submitted or lost return buffer')

-- Failed saves preserve unsaved text; force quit still discards without submit.
buf = composer()
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Keep failed text' })
fail = true
input(':wq<CR>')
assert(api.nvim_get_current_buf() == buf and vim.bo[buf].modified and api.nvim_get_current_line() == 'Keep failed text', 'failed close lost composer')
assert(errors[#errors]:find('Fixture local save failed', 1, true), 'save error not reported')
fail = false
input(':q!<CR>')
assert(api.nvim_get_current_buf() == origin and submitting == 0, 'discard submitted or failed to restore origin')

-- A failed write barrier must not start another save or strand its saving flag.
buf = composer()
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Blocked by earlier write' })
local descriptor = editor.get(buf)
local before_write = descriptor.before_write
descriptor.before_write = function() error('Earlier write failed') end
before = #saves
local ok, failure = pcall(vim.cmd.write)
assert(not ok and failure:find('Earlier write failed', 1, true), 'barrier failure did not abort write')
assert(#saves == before and not descriptor.saving and vim.bo[buf].modified, 'barrier failure changed persistence or retained text')
descriptor.before_write = before_write
input(':q!<CR>')

-- Native ZZ/ZQ share local save and unsaved-return semantics.
buf = composer()
api.nvim_buf_set_lines(buf, 0, -1, false, { 'ZZ local' })
normal('ZZj')
assert(api.nvim_get_current_buf() == origin and saves[#saves].text == 'ZZ local', 'ZZ failed to save and return')
buf = composer()
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Discard ZQ' })
before = #saves
normal('ZQ')
assert(api.nvim_get_current_buf() == origin and #saves == before, 'ZQ wrote discarded text')

-- Every conclusion close remains local, including explicit write-and-close.
buf = composer('conclusion')
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Conclusion' })
vim.cmd.write()
input(':q<CR>')
assert(submitting == 0 and api.nvim_get_current_buf() == origin, 'saved conclusion quit submitted')
buf = composer('conclusion')
api.nvim_buf_set_lines(buf, 0, -1, false, { 'Ready' })
input(':wq<CR>')
assert(submitting == 0 and api.nvim_get_current_buf() == origin and saves[#saves].text == 'Ready', 'conclusion write-and-close submitted or lost local save')
assert(barriers == #saves + 1, 'save barrier was bypassed')
assert(vim.deep_equal(api.nvim_buf_get_lines(origin, 0, -1, false), { 'first', 'second', 'third' }), 'composer commands mutated source')
for _, closed in ipairs(opened) do
  assert(not api.nvim_buf_is_valid(closed) and not editor.get(closed), 'closed composer retained buffer or editor state')
end
print('Native composer command composition passed')
vim.cmd('qa!')
