local M = {}
local api = vim.api
local client = require('buf_review.client')
local editor = require('buf_review.editor')
local action_queue = require('buf_review.actions')
local checkout = require('buf_review.checkout')
local repositories, buffers = {}, {}
local namespace = api.nvim_create_namespace('buf-review')
local range_namespace = api.nvim_create_namespace('buf-review-comment-range')
local highlighted, range_matches, range_pending = {}, {}, false
local sequence = 0
local first_load = true
local commands = {
  'prs', 'pr', 'start', 'leave', 'conclude', 'publish', 'refresh', 'comment', 'reply',
  'edit', 'delete', 'restore', 'suggest', 'done', 'todo', 'viewed', 'unviewed',
  'next', 'previous', 'compare', 'context', 'relocate', 'resolve', 'log', 'sessions', 'checkout',
}
local mark_move_bindings = {
  { ']F', 'ViewedNext', 'file', 1 }, { '[F', 'ViewedPrevious', 'file', -1 },
  { ']R', 'DoneNext', 'comment', 1 }, { '[R', 'DonePrevious', 'comment', -1 },
}
local dispatch, home, open_file, open_comment, compose, refresh, show_conflict, thread, thread_list
local navigate, publication, navigate_items
local render_reader
local refresh_checkout, checkout_current
local highlight_comment
local pending_renders, render_scheduled = {}, false
local function render_pending(safe)
  render_scheduled = false
  if not safe and not action_queue.ready() then return end
  local pending = pending_renders
  pending_renders = {}
  for buf, cursor in pairs(pending) do
    if api.nvim_buf_is_valid(buf) and #vim.fn.win_findbuf(buf) > 0 then render_reader(buf, cursor, safe) end
  end
end
local function defer_render(buf, cursor)
  pending_renders[buf] = cursor or 'position'
  if not render_scheduled then
    render_scheduled = true
    vim.schedule(render_pending)
  end
end
local function settle(state)
  if state.writer then state.writer.wait() end
end
local delete_comments
local delete_visual, pending_delete_visual = false

local function notify(message, level)
  vim.notify('buf-review: ' .. tostring(message), level or vim.log.levels.INFO)
end

local function valid(win)
  return win and api.nvim_win_is_valid(win)
end

local function show_buffer(win, buf)
  api.nvim_win_call(win, function()
    api.nvim_cmd({ cmd = 'buffer', args = { tostring(buf) }, mods = { keepjumps = true } }, {})
  end)
end

local function content(row)
  return row.comment.comment
end

local function unpublished(row)
  return row.comment.origin == 'draft' and row.destination ~= nil
end

local function find(view, id)
  for _, row in ipairs(view.comments) do
    if content(row).id == id then return row end
  end
end

local function root_row(view, row)
  local seen, depth = {}, 0
  while row and content(row).target.kind == 'reply' do
    local parent = content(row).target.target
    if seen[parent] then return nil, depth end
    seen[parent] = true
    row = find(view, parent)
    depth = depth + 1
  end
  return row, depth
end

local function matches(view, location, current)
  if not location then return false end
  if vim.deep_equal(location.revision, view.comparison) then return true end
  local revision = location.revision
  if revision and not revision.base and revision.head == view.comparison.head then
    if current and vim.deep_equal(view.review.read.pull_request.revision, view.comparison) then return true end
    return location.range and location.range.start.side == 'head'
  end
  return false
end

local function anchor(view, row)
  local root = root_row(view, row)
  if not root or content(root).target.kind ~= 'diff' then return nil end
  local location = content(root).target.target
  if root.comment.origin == 'draft' then return location end
  local current = location.current
  if current and matches(view, current, true) then
    return vim.tbl_extend('force', {}, current, { revision = view.comparison })
  end
  return location
end

local function displayed_location(view, row, meta)
  if not meta or not meta.historical then
    if meta and not vim.deep_equal(meta.revision, view.comparison) then return end
    local location = anchor(view, row)
    return matches(view, location) and location or nil
  end
  local root = root_row(view, row)
  if not root or content(root).target.kind ~= 'diff' then return end
  if content(root).id == meta.root then return meta.location end
  if meta.side ~= 'base' then return end
  local location = content(root).target.target
  local range = location.range
  if not range or range.start.side ~= range['end'].side then return end
  local source = location.revision[range.start.side]
  if source ~= meta.revision.base or location.path ~= meta.source_path then return end
  location = vim.deepcopy(location)
  location.revision, location.path = meta.revision, meta.path
  location.range.start.side, location.range['end'].side = 'base', 'base'
  return location
end

local function roots_at(view, location, meta)
  local roots, seen = {}, {}
  local point = location.range.start
  for _, row in ipairs(view.comments) do
    local at = displayed_location(view, row, meta)
    if at and at.path == location.path and at.range
      and at.range.start.side == point.side and point.line >= at.range.start.line and point.line <= at.range['end'].line then
      local root = root_row(view, row)
      if root and not seen[content(root).id] then
        seen[content(root).id] = true
        roots[#roots + 1] = root
      end
    end
  end
  return roots
end

local function preview(text)
  return (text:gsub('%s+', ' ')):sub(1, 100)
end

local function location_text(location)
  if not location then return nil end
  local at = location.path
  if location.range then
    local range = location.range
    at = at .. ':' .. range.start.side .. ':' .. range.start.line
    if range['end'].line ~= range.start.line then at = at .. '-' .. range['end'].line end
  end
  return at
end

local function comment_metadata(view, row)
  local comment = content(row)
  local location = anchor(view, row)
  local at = location_text(location) or 'PR'
  local author = comment.author and (comment.author.name or comment.author.id) or 'unknown author'
  local flags = {}
  if row.fetched then flags[#flags + 1] = row.done and 'Done 👍' or 'Todo' end
  if row.conflict then flags[#flags + 1] = 'CONFLICT' end
  if row.pending_delete then flags[#flags + 1] = 'pending deletion' end
  if row.anchor_error then flags[#flags + 1] = 'REJECTED ANCHOR: ' .. row.anchor_error .. '; :Review relocate/delete' end
  if row.destination then flags[#flags + 1] = row.destination == 'review' and 'Review' or 'Standalone' end
  return at, author, flags, location
end

highlight_comment = function(safe)
  if not safe then
    if range_pending then return end
    range_pending = true
    vim.schedule(function()
      if action_queue.ready() then highlight_comment(true) end
    end)
    return
  end
  range_pending = false
  for _, buf in ipairs(highlighted) do
    if api.nvim_buf_is_valid(buf) then api.nvim_buf_clear_namespace(buf, range_namespace, 0, -1) end
  end
  for _, match in ipairs(range_matches) do
    if valid(match.window) then pcall(vim.fn.matchdelete, match.id, match.window) end
  end
  highlighted, range_matches = {}, {}
  local visible = {}
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    visible[buf] = visible[buf] or {}
    visible[buf][#visible[buf] + 1] = win
  end
  for _, repository in pairs(repositories) do
    local pane = repository.active_comment
    if pane and visible[pane.buffer] then
      local view = pane.state.view
      for buf, windows in pairs(visible) do
        local meta = buffers[buf]
        if meta and meta.kind == 'diff' and meta.state == pane.state then
          local location = displayed_location(view, find(view, pane.id), meta)
          local range = location and location.range
          if range and range.start.side == range['end'].side then
            local first, last = range.start.line, range['end'].line
            if meta.path == location.path and meta.side == range.start.side
              and first >= 1 and last >= first and last <= meta.source_lines and not meta.binary then
              api.nvim_buf_set_extmark(buf, range_namespace, first - 1, 0, {
                end_row = last, end_col = 0, hl_group = 'BufReviewCommentRange', hl_eol = true, priority = 200,
              })
              highlighted[#highlighted + 1] = buf
              local rows = vim.fn.range(first, last)
              for _, win in ipairs(windows) do
                local id = vim.fn.matchaddpos('BufReviewCommentRange', rows, 200, -1, { window = win })
                range_matches[#range_matches + 1] = { window = win, id = id }
              end
            end
          end
        end
      end
    end
  end
end

local function activate(buf)
  if api.nvim_get_current_buf() ~= buf then return end
  local meta, edit = buffers[buf], editor.get(buf)
  if not meta then return end
  if meta.kind == 'diff' then
    meta.state.repository.active_file = { id = meta.path, revision = meta.revision, state = meta.state }
    return
  end
  if not (meta.kind == 'thread' or meta.kind == 'conflict' or (edit and edit.kind == 'comment')) then return end
  local object = meta.rows[api.nvim_win_get_cursor(0)[1]]
  local id = (object and object.comment) or (edit and edit.comment) or meta.comment or meta.object
  meta.comment = id
  local previous = meta.state.repository.active_comment
  meta.state.repository.active_comment = { buffer = buf, id = id, state = meta.state }
  if not previous or previous.buffer ~= buf or previous.id ~= id then highlight_comment() end
end

local function get_state()
  local meta = buffers[api.nvim_get_current_buf()]
  if meta then
    if meta.dirty then render_reader(api.nvim_get_current_buf(), 'position') end
    activate(api.nvim_get_current_buf())
    return meta.state, meta
  end
  local name = api.nvim_buf_get_name(0)
  local path = name ~= '' and vim.fs.dirname(name) or vim.fn.getcwd()
  local marker = vim.fs.find('.git', { path = path, upward = true })[1]
  local root = marker and vim.fs.dirname(marker)
  if not root then error('No Git repository for current buffer') end
  if not repositories[root] then
    local repository = { sessions = {}, query = {} }
    repository.current = { root = root, repository = repository }
    repositories[root] = repository
  end
  return repositories[root].current, nil
end

local function ready(state)
  if state.loading then error('Review loading in progress; wait for it to finish', 0) end
end

local function protected(state)
  if not state.tab or not api.nvim_tabpage_is_valid(state.tab) then return false end
  for _, win in ipairs(api.nvim_tabpage_list_wins(state.tab)) do
    local buf = api.nvim_win_get_buf(win)
    local composer = editor.get(buf)
    if composer and (vim.bo[buf].modified or composer.saving or composer.submitting) then
      return true
    end
  end
  return false
end

local function guard(state)
  if protected(state) then error('E37: Review composer has unsaved text or an operation in progress; save or explicitly abandon it first') end
end

local function leave_diff(win)
  if valid(win) and vim.wo[win].diff then
    api.nvim_win_call(win, function() vim.cmd('diffoff') end)
  end
end

api.nvim_create_autocmd('WinClosed', { callback = function(event)
  local win = tonumber(event.match)
  local meta = valid(win) and buffers[api.nvim_win_get_buf(win)]
  if meta and meta.role then leave_diff(win) end
end })

local function tab(state)
  local repository = state.repository
  local previous = repository.current
  if previous ~= state then
    settle(previous)
    guard(previous)
    if previous.tab and api.nvim_tabpage_is_valid(previous.tab) then
      if valid(previous.discussion) then leave_diff(previous.discussion); api.nvim_win_close(previous.discussion, false) end
      if valid(previous.right) then leave_diff(previous.right); api.nvim_win_close(previous.right, false) end
      state.tab, state.top = previous.tab, valid(previous.left) and previous.left or previous.top
      previous.tab, previous.top, previous.left, previous.right, previous.discussion = nil, nil, nil, nil, nil
    end
    repository.current = state
  end
  if state.tab and api.nvim_tabpage_is_valid(state.tab) then
    api.nvim_set_current_tabpage(state.tab)
  else
    vim.cmd('keepjumps tabnew')
    state.tab = api.nvim_get_current_tabpage()
    state.top = api.nvim_get_current_win()
    state.left, state.right, state.discussion = nil, nil, nil
  end
end

local function delete_motion(visual)
  pending_delete_visual = visual == true
  vim.go.operatorfunc = "v:lua.require'buf_review'.delete_operator"
  return 'g@'
end

local reader_windows = {
  home = 'top', loading = 'top', prs = 'top', sessions = 'top', historical = 'left',
  ['diff:base'] = 'left', ['diff:head'] = 'right', context = 'context_left',
  ['context:base'] = 'context_left', ['context:head'] = 'context_right',
}

local function reader_slot(state, role)
  state.readers = state.readers or {}
  local slot = state.readers[role] or {}
  state.readers[role] = slot
  local owner = state[reader_windows[role] or 'discussion']
  local function available(buf)
    return buf and api.nvim_buf_is_valid(buf) and not editor.retains(buf)
      and not vim.tbl_contains(vim.fn.win_findbuf(buf), function(win) return win ~= owner end, { predicate = true })
  end
  if available(slot.current) then return slot, slot.current end
  for _, buf in ipairs(slot) do if available(buf) then return slot, buf end end
  return slot
end

local function scratch(state, kind, object, lines, writable, role)
  role = role or kind
  local slot, buf
  if not writable then slot, buf = reader_slot(state, role) end
  local fresh = not buf
  if fresh then
    sequence = sequence + 1
    buf = api.nvim_create_buf(false, true)
    local name = writable and ('composer/%d'):format(sequence) or ('review-%d'):format(sequence)
    api.nvim_buf_set_name(buf, ('review://%s/%s'):format(state.view and state.view.id or 'prs', name))
    if slot then slot[#slot + 1] = buf end
  else
    pending_renders[buf] = nil
    api.nvim_buf_clear_namespace(buf, -1, 0, -1)
    vim.bo[buf].filetype = ''
    vim.bo[buf].syntax = ''
  end
  if slot then slot.current = buf end
  local meta = buffers[buf] or {}
  for key in pairs(meta) do meta[key] = nil end
  meta.state, meta.kind, meta.object, meta.rows = state, kind, object, {}
  meta.workspace, meta.role = state.view and state.view.id, not writable and role or nil
  buffers[buf] = meta
  vim.b[buf].buf_review = { repository = state.root, workspace = meta.workspace, kind = kind, object = object }
  vim.bo[buf].buftype = 'nofile'
  vim.bo[buf].bufhidden = 'hide'
  vim.bo[buf].swapfile = false
  vim.bo[buf].filetype = 'review'
  vim.bo[buf].readonly = false
  vim.bo[buf].modifiable = true
  if not writable then vim.bo[buf].undolevels = -1 end
  api.nvim_buf_set_lines(buf, 0, -1, false, lines or {})
  vim.bo[buf].modified = false
  vim.bo[buf].modifiable = writable or false
  vim.bo[buf].readonly = not writable
  if not fresh then return buf, meta end
  api.nvim_create_autocmd('BufWipeout', { buffer = buf, once = true, callback = function() buffers[buf] = nil end })
  api.nvim_create_autocmd({ 'BufEnter', 'BufWinEnter', 'WinEnter', 'WinLeave', 'CursorMoved' }, { buffer = buf, callback = function()
    if meta.dirty then render_reader(buf, 'position') end
    activate(buf)
  end })
  if not writable then
    api.nvim_create_autocmd('BufWinLeave', { buffer = buf, callback = function()
      for _, win in ipairs(vim.fn.win_findbuf(buf)) do leave_diff(win) end
    end })
  end
  if kind == 'home' then
    api.nvim_create_autocmd({ 'BufEnter', 'WinEnter' }, { buffer = buf, callback = function() refresh_checkout(state) end })
  end
  local mappings = vim.list_extend(api.nvim_get_keymap('n'), api.nvim_buf_get_keymap(buf, 'n'))
  for _, binding in ipairs(vim.list_extend({
    { ']r', 'NextComment' }, { '[r', 'PreviousComment' },
    { ']f', 'NextFile' }, { '[f', 'PreviousFile' },
  }, mark_move_bindings)) do
    if not vim.tbl_contains(mappings, function(mapping) return mapping.lhsraw == binding[1] end, { predicate = true }) then
      vim.keymap.set('n', binding[1], '<Plug>(Review' .. binding[2] .. ')', { buffer = buf, silent = true })
    end
  end
  if not writable then
    vim.keymap.set('n', '<CR>', function() M.enter() end, { buffer = buf, silent = true })
    vim.keymap.set('n', 's', function() M.progress(true) end, { buffer = buf, silent = true })
    vim.keymap.set('n', 'u', function() M.progress(false) end, { buffer = buf, silent = true })
    if kind == 'diff' or kind == 'threads' or kind == 'thread' then
      vim.keymap.set('n', 'a', function() M.append() end, { buffer = buf, silent = true })
    end
    if kind ~= 'diff' and kind ~= 'context' then
      for _, pair in ipairs({ { 's', true }, { 'u', false } }) do
        vim.keymap.set('x', pair[1], function() M.progress(pair[2], true) end, { buffer = buf, silent = true })
      end
    end
    if kind == 'home' or kind == 'threads' or kind == 'thread' then
      api.nvim_create_autocmd('ModeChanged', { buffer = buf, callback = function()
        if vim.v.event.old_mode:sub(1, 2) == 'no' and vim.v.event.new_mode == 'n' then pending_delete_visual = nil end
      end })
      vim.keymap.set('n', 'd', delete_motion, { buffer = buf, silent = true, expr = true })
      vim.keymap.set('n', 'dd', function() return delete_motion() .. '_' end, { buffer = buf, silent = true, expr = true })
      vim.keymap.set('n', 'D', function() return delete_motion() .. '$' end, { buffer = buf, silent = true, expr = true })
      for _, key in ipairs({ 'd', 'x' }) do
        vim.keymap.set('x', key, function() return delete_motion(true) end, { buffer = buf, silent = true, expr = true })
      end
      vim.keymap.set('n', 'x', function()
        local line = api.nvim_win_get_cursor(0)[1]
        if api.nvim_get_current_line() ~= '' then delete_comments(line, line) end
      end, { buffer = buf, silent = true })
    end
  end
  return buf, meta
end

local function replace(buf, lines)
  if not api.nvim_buf_is_valid(buf) then return end
  vim.bo[buf].readonly = false
  vim.bo[buf].modifiable = true
  api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modified = false
  vim.bo[buf].modifiable = false
  vim.bo[buf].readonly = true
end

local function discussion(state, buf)
  guard(state)
  state.repository.opening = {}
  if api.nvim_get_current_win() ~= state.discussion then state.discussion_origin = api.nvim_get_current_win() end
  tab(state)
  if not valid(state.discussion) then
    local top = valid(state.left) and state.left or state.top
    if not valid(top) then top = api.nvim_get_current_win() end
    api.nvim_set_current_win(top)
    vim.cmd('keepjumps botright split')
    state.discussion = api.nvim_get_current_win()
    api.nvim_win_set_height(state.discussion, math.max(6, math.floor(vim.o.lines * 0.3)))
  end
  leave_diff(state.discussion)
  show_buffer(state.discussion, buf)
  api.nvim_set_current_win(state.discussion)
  vim.wo.wrap = true
  vim.wo.diff = false
end

local function selected(meta)
  return meta and (meta.rows[api.nvim_win_get_cursor(0)[1]] or (meta.kind == 'composer' and { comment = meta.object }) or nil)
end

local function at_continuation(state, win)
  local continuation = state.continuation
  local position = continuation and continuation.position
  return position and continuation.id == state.selected and position.window == win and valid(win)
    and api.nvim_win_get_buf(win) == position.buffer
    and vim.deep_equal(api.nvim_win_get_cursor(win), position.cursor)
end

local function require_view(state)
  if not state.view then error('No active review workspace; use :Review first') end
  return state.view
end

local function annotate(buf, meta, view)
  api.nvim_buf_clear_namespace(buf, namespace, 0, -1)
  local counts = {}
  for _, row in ipairs(view.comments) do
    local location = displayed_location(view, row, meta)
    if location and location.path == meta.path and location.range and location.range.start.side == meta.side then
      local line = location.range.start.line
      local count = counts[line] or { fetched = 0, done = 0, local_comments = 0 }
      if row.fetched then
        count.fetched = count.fetched + 1
        if row.done then count.done = count.done + 1 end
      else count.local_comments = count.local_comments + 1 end
      counts[line] = count
    end
  end
  for line, count in pairs(counts) do
    if line <= api.nvim_buf_line_count(buf) then
      local text = {}
      if count.fetched > 0 then
        text[#text + 1] = { ('  %d comment%s '):format(count.fetched, count.fetched == 1 and '' or 's'), 'Comment' }
        local status, group = '[ ]', 'BufReviewTodo'
        if count.done == count.fetched then status, group = '[x]', 'BufReviewDone'
        elseif count.done > 0 then status, group = '[-]', 'BufReviewPartial' end
        text[#text + 1] = { status, group }
      end
      if count.local_comments > 0 then
        text[#text + 1] = { ('  %d local comment%s'):format(count.local_comments, count.local_comments == 1 and '' or 's'), 'Comment' }
      end
      api.nvim_buf_set_extmark(buf, namespace, line - 1, 0, { virt_text = text })
    end
  end
end

local render_home
render_reader = function(buf, cursor, safe)
  local meta = buffers[buf]
  if not meta or not api.nvim_buf_is_valid(buf) then return end
  if not safe and not action_queue.ready() then defer_render(buf, cursor); return end
  pending_renders[buf] = nil
  local state = meta.state
  if meta.kind == 'home' and state.loading then return end
  meta.dirty = nil
  if meta.kind == 'home' then render_home(state, cursor)
  elseif meta.kind == 'thread' then thread(state, find(state.view, meta.object), buf, cursor)
  elseif meta.kind == 'threads' then
    local roots = {}
    if meta.location and vim.deep_equal(meta.location.revision, state.view.comparison) then roots = roots_at(state.view, meta.location)
    else
      for _, id in ipairs(meta.root_ids) do
        local row = find(state.view, id)
        if row then roots[#roots + 1] = row end
      end
    end
    thread_list(state, roots, buf, cursor)
  elseif meta.kind == 'diff' then annotate(buf, meta, state.view) end
end

local function apply(state, response, cursor)
  if response.workspace then
    if state.view == response.workspace then return state end
    if state.view and state.view.id == response.workspace.id and response.workspace.generation < state.view.generation then
      return state
    end
    if state.view and state.view.id ~= response.workspace.id then return state end
    state.view = response.workspace
    for buf, meta in pairs(buffers) do
      if meta.state == state and api.nvim_buf_is_valid(buf) and (meta.kind == 'home' or meta.kind == 'thread' or meta.kind == 'threads' or meta.kind == 'diff') then
        meta.dirty = true
        if #vim.fn.win_findbuf(buf) > 0 then render_reader(buf, cursor) end
      end
    end
    highlight_comment()
  end
  return state
end

local function request(state, operation, fields, callback, generation)
  local origin = callback and { window = api.nvim_get_current_win(), buffer = api.nvim_get_current_buf(), cursor = api.nvim_win_get_cursor(0) }
  local payload = vim.tbl_extend('force', { op = operation }, fields or {})
  if vim.tbl_contains({ 'start', 'leave', 'save', 'delete', 'restore', 'undo', 'viewed', 'reaction', 'summary', 'relocate', 'resolve', 'pr_details' }, operation) then
    payload = { op = 'actions', actions = { vim.tbl_extend('force', { action = operation }, fields or {}) } }
  end
  payload.workspace = require_view(state).id
  if generation ~= false and not vim.tbl_contains({ 'get', 'code', 'context', 'log', 'history' }, operation) then
    payload.generation = type(generation) == 'number' and generation or require_view(state).generation
  end
  local function receive(response, err, report)
    -- Check before rendering: applying a projection may clamp the cursor.
    local stationary = origin and api.nvim_get_current_win() == origin.window
      and api.nvim_get_current_buf() == origin.buffer and vim.deep_equal(api.nvim_win_get_cursor(0), origin.cursor)
    local position = state.continuation and state.continuation.position
    local continuing = position and at_continuation(state, position.window)
    local cursor = (operation == 'actions' or operation == 'reaction' or operation == 'viewed' or operation == 'delete' or operation == 'restore') and 'position' or ((operation == 'refresh' or operation == 'apply_refresh') and 'object' or nil)
    if continuing and not cursor then cursor = 'object' end
    local current = response and operation ~= 'refresh' and operation ~= 'apply_refresh' and apply(state, response, cursor) or state
    if callback then callback(response, err, current, stationary) elseif err and report ~= false then notify(err, vim.log.levels.ERROR) end
    if continuing and current.continuation and current.continuation.position == position and valid(position.window)
      and api.nvim_win_get_buf(position.window) == position.buffer then
      position.cursor = api.nvim_win_get_cursor(position.window)
    end
  end
  if payload.op == 'actions' or payload.op == 'apply_refresh' then
    if not state.writer then
      state.writer = action_queue.new(function(command, done)
        command = vim.tbl_extend('force', {}, command, { workspace = state.view.id, generation = state.view.generation })
        client.request(state.root, command, done)
      end)
    end
    if payload.op == 'actions' then state.writer.enqueue(payload.actions, receive)
    else state.writer.enqueue_request(payload, receive) end
  else
    settle(state)
    if payload.generation then payload.generation = type(generation) == 'number' and generation or state.view.generation end
    client.request(state.root, payload, receive)
  end
end

local function actions(state, queue, callback)
  if #queue == 0 then return end
  local entries = {}
  for _, item in ipairs(queue) do entries[#entries + 1] = vim.tbl_extend('force', { action = item[1] }, item[2]) end
  request(state, 'actions', { actions = entries }, callback)
end

local function refresh_snapshot(state, compare_latest, callback)
  request(state, 'fetch_refresh', {}, function(fetched, err)
    if err then callback(fetched, err); return end
    request(state, 'apply_refresh', { prepared_refresh = fetched.prepared_refresh, compare_latest = compare_latest }, callback)
  end)
end

local function prepare_objects(state, callback, recheck)
  local revision = vim.deepcopy(require_view(state).comparison)
  local job = state.objects
  if job and job.review == state.view.review.id and vim.deep_equal(job.revision, revision) then
    if not job.done then
      if callback then job.waiters[#job.waiters + 1] = callback end
      return
    elseif not job.error and not recheck then
      if callback then callback() end
      return
    end
  end
  job = { revision = revision, review = state.view.review.id, waiters = callback and { callback } or {} }
  state.objects = job
  local function redraw()
    if state.home and buffers[state.home] then
      buffers[state.home].dirty = true
      if #vim.fn.win_findbuf(state.home) > 0 then render_reader(state.home, 'position') end
    end
  end
  redraw()
  client.request(state.root, { op = 'fetch_objects', workspace = state.view.id }, function(response, err)
    if not err and not (response and vim.deep_equal(response.revision, revision)) then
      err = 'Comparison changed while preparing code; reopen the file'
    end
    job.done, job.error = true, err
    if state.objects == job and vim.deep_equal(state.view.comparison, revision) then
      redraw()
      if response and response.warnings then
        notify('Historical commits unavailable: ' .. table.concat(response.missing_commits or {}, ', '), vim.log.levels.WARN)
      end
      if err and state.repository.current == state then notify(err, vim.log.levels.ERROR) end
    end
    for _, waiter in ipairs(job.waiters) do waiter(err) end
    job.waiters = {}
  end)
end

local home_namespace = api.nvim_create_namespace('buf-review-home')
for group, link in pairs({
  BufReviewTitle = 'Title', BufReviewLabel = 'Label', BufReviewSection = 'PreProc', BufReviewCount = 'Number',
  BufReviewPath = 'Directory', BufReviewAuthor = 'Identifier', BufReviewPreview = 'Normal',
  BufReviewState = 'Special', BufReviewError = 'DiagnosticError', BufReviewPending = 'WarningMsg',
  BufReviewTodo = 'DiagnosticInfo', BufReviewPartial = 'DiagnosticWarn', BufReviewDone = 'DiagnosticOk',
  BufReviewAdded = 'Added', BufReviewDeleted = 'Removed', BufReviewModified = 'Changed',
  BufReviewCommentRange = 'Visual',
}) do api.nvim_set_hl(0, group, { default = true, link = link }) end

local function counts_text(counts)
  local parts = {}
  for _, field in ipairs({ { 'comments', 'comment' }, { 'edits', 'edit' }, { 'deletions', 'deletion' }, { 'reactions', 'reaction' }, { 'files', 'file' }, { 'conclusions', 'conclusion' } }) do
    local count = counts and counts[field[1]] or 0
    if count > 0 then parts[#parts + 1] = count .. ' ' .. field[2] .. (count == 1 and '' or 's') end
  end
  return #parts > 0 and table.concat(parts, ', ') or '0 changes'
end

local function publication_text(summary)
  local destination = summary.destination == 'review' and 'Review' or (summary.destination == 'standalone' and 'Standalone' or 'Publication')
  return destination .. ' · ' .. summary.status:gsub('_', ' ') .. ' · ' .. summary.id
end

function M.foldexpr()
  local meta = buffers[api.nvim_get_current_buf()]
  return meta and meta.folds and meta.folds[vim.v.lnum] or 0
end

local log_namespace = api.nvim_create_namespace('buf-review-log')
local function show_log(state, entries)
  local source = state.view.pr.source
  local label = source.kind == 'local_intent' and (source.branches.source.branch .. ' → ' .. source.branches.sink.branch .. ' [local]') or source.locator
  local lines = { 'Review log · session ' .. state.view.id, label, #entries .. ' events', '' }
  local folds, highlights = {}, {
    { line = 1, group = 'BufReviewTitle' }, { line = 2, group = 'BufReviewPath' },
    { line = 3, group = 'BufReviewCount' },
  }
  for _, entry in ipairs(entries) do
    local prefix = '#' .. entry.generation .. '  '
    local group = entry.level == 'error' and 'BufReviewError' or (entry.level == 'success' and 'BufReviewDone' or 'BufReviewSection')
    lines[#lines + 1] = prefix .. entry.summary:gsub('[\r\n]', ' ')
    folds[#lines] = '>1'
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewCount', finish = #prefix }
    highlights[#highlights + 1] = { line = #lines, group = group, start = #prefix }
    for _, text in ipairs(entry.details) do
      for _, line in ipairs(vim.split(text:gsub('\r\n', '\n'), '\n', { plain = true })) do
        lines[#lines + 1], folds[#lines + 1] = '  ' .. line, 1
        highlights[#highlights + 1] = { line = #lines, group = entry.level == 'error' and 'BufReviewError' or 'BufReviewPreview' }
      end
    end
    lines[#lines + 1], folds[#lines + 1] = '  Recorded payload', '>2'
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel' }
    for _, text in ipairs(entry.raw) do
      for _, line in ipairs(vim.split(text, '\n', { plain = true })) do
        lines[#lines + 1], folds[#lines + 1] = '    ' .. line, 2
      end
    end
    folds[#lines] = '<2'
    lines[#lines + 1], folds[#lines + 1] = '', '<1'
  end
  if #entries == 0 then lines[#lines + 1] = 'No recorded events' end
  local buf, meta = scratch(state, 'log', nil, lines)
  meta.folds = folds
  for _, highlight in ipairs(highlights) do
    api.nvim_buf_add_highlight(buf, log_namespace, highlight.group, highlight.line - 1, highlight.start or 0, highlight.finish or -1)
  end
  discussion(state, buf)
  vim.wo.foldmethod, vim.wo.foldexpr = 'expr', "v:lua.require'buf_review'.foldexpr()"
  vim.wo.foldtext, vim.wo.foldlevel, vim.wo.foldminlines, vim.wo.foldenable = '', 0, 0, true
  api.nvim_win_set_cursor(0, { #entries > 0 and 5 or 1, 0 })
end

local function capture_home_folds(state)
  if not state.home or not api.nvim_buf_is_valid(state.home) then return end
  state.home_fold_states = state.home_fold_states or {}
  local sections = buffers[state.home].sections or {}
  for _, win in ipairs(vim.fn.win_findbuf(state.home)) do
    local saved = { closed = {} }
    api.nvim_win_call(win, function()
      saved.view = vim.fn.winsaveview()
      saved.row = buffers[state.home].rows[saved.view.lnum]
      saved.preview = saved.row and buffers[state.home].rows[saved.view.lnum - 1] == saved.row
      for name, line in pairs(sections) do saved.closed[name] = vim.fn.foldclosed(line) == line end
    end)
    state.home_fold_states[win] = saved
  end
end

local function restore_home_folds(state, win, cursor)
  if not valid(win) or api.nvim_win_get_buf(win) ~= state.home then return end
  local saved = state.home_fold_states and state.home_fold_states[win]
  api.nvim_win_call(win, function()
    local previous = saved and saved.view or vim.fn.winsaveview()
    vim.cmd('silent! normal! zx')
    if saved then
      for name, line in pairs(buffers[state.home].sections or {}) do
        if saved.closed[name] ~= nil then
          api.nvim_win_set_cursor(win, { line, 0 })
          vim.cmd('silent! normal! ' .. (saved.closed[name] and 'zc' or 'zo'))
        end
      end
      if saved.row and cursor ~= 'position' then
        local selected_line
        local rows = buffers[state.home].rows
        for line = 1, api.nvim_buf_line_count(state.home) do
          local row = rows[line]
          if row and ((saved.row.comment and row.comment == saved.row.comment)
            or (saved.row.file and row.file == saved.row.file)
            or (saved.row.conclusion and row.conclusion)) then
            selected_line = selected_line or line
            if vim.deep_equal(row, saved.row) then selected_line = line; break end
          end
        end
        if selected_line then
          local line = selected_line + (saved.preview and 1 or 0)
          previous = vim.tbl_extend('force', {}, previous, {
            lnum = line, topline = math.max(1, previous.topline + line - previous.lnum),
          })
        end
      end
    end
    vim.fn.winrestview(previous)
  end)
end

refresh_checkout = function(state)
  if not state.view or state.checkout_check or state.checkout_running then return end
  if state.view.pr.source.kind == 'local_intent' then return end
  local pull = state.view.review.read.pull_request
  local token = {}
  state.checkout_check = token
  checkout.inspect(state.root, pull, function(status, err)
    if state.checkout_check ~= token then return end
    state.checkout_check = nil
    local current = state.view.review.read.pull_request
    if not vim.deep_equal(current.source, pull.source) or current.revision.head ~= pull.revision.head then
      refresh_checkout(state)
      return
    end
    state.checkout_status, state.checkout_error = status, err
    if state.home and buffers[state.home] then
      buffers[state.home].dirty = true
      if #vim.fn.win_findbuf(state.home) > 0 then render_reader(state.home, 'position') end
    end
  end)
end

checkout_current = function(state)
  if require_view(state).pr.source.kind == 'local_intent' then error('Local PR intent has no remote PR to check out') end
  guard(state)
  settle(state)
  local repository = state.repository
  local selection = repository.current
  local navigation = repository.navigation
  local pull = vim.tbl_extend('force', {}, require_view(state).review.read.pull_request, { url = state.view.locator })
  if state.checkout_running then error('Checkout already in progress') end
  state.checkout_check, state.checkout_running = nil, true
  state.checkout_error = nil
  if state.home and buffers[state.home] then
    buffers[state.home].dirty = true
    if #vim.fn.win_findbuf(state.home) > 0 then render_reader(state.home, 'position') end
  end
  checkout.run(state.root, pull, { validate = function()
    if repository.current ~= selection or repository.navigation ~= navigation then
      return false, 'Checkout cancelled: review selection changed'
    end
    guard(state)
  end }, function(status, err)
    state.checkout_running = nil
    state.checkout_status, state.checkout_error = status, err
    if state.home and buffers[state.home] then
      buffers[state.home].dirty = true
      if #vim.fn.win_findbuf(state.home) > 0 then render_reader(state.home, 'position') end
    end
    local message = err
    if status and status.stash then
      message = (message and (message .. '\n') or '') .. 'Stash retained: ' .. status.stash.oid .. '\n' .. status.stash.message
    end
    if message then notify(message, err and vim.log.levels.ERROR or vim.log.levels.INFO)
    elseif not (status and status.cancelled) then notify('Checked out PR branch') end
    local current = state.view.review.read.pull_request
    if not vim.deep_equal(current.source, pull.source) or current.revision.head ~= pull.revision.head then refresh_checkout(state) end
    if repository.current ~= state then refresh_checkout(repository.current) end
  end)
end

render_home = function(state, cursor)
  capture_home_folds(state)
  local view = require_view(state)
  local pull = view.review.read.pull_request
  local pr = view.pr
  local branches = pr.source.kind == 'local_intent' and pr.source.branches or nil
  local wip = view.wip and (view.wip.active and 'active' or 'left; resumable') or 'none'
  local freshness = state.error and (branches and 'cached; local refresh failed' or 'cached; remote fetch failed')
    or (state.refresh_deferred and 'cached; refresh deferred')
    or (state.refreshed_at and ('refreshed ' .. state.refreshed_at)) or 'cached snapshot'
  if branches and not state.error then freshness = 'local committed snapshot' end
  if view.new_commits then freshness = freshness .. ' | new commits: :Review compare latest' end
  local lines = {
    'Pull request: ' .. (branches and (branches.source.branch .. ' → ' .. branches.sink.branch .. ' [local]') or pr.source.locator),
    '  ' .. (pr.title ~= '' and pr.title or '(untitled)'),
    ('Reviewer: @%s  WIP: %s'):format(view.review.reviewer.name, wip),
    ('Comparison: %s → %s'):format(view.comparison.base:sub(1, 10), view.comparison.head:sub(1, 10)),
    'Freshness: ' .. freshness,
  }
  for line, text in ipairs(lines) do lines[line] = text:gsub('[\r\n]', ' ') end
  local highlights = {
    { line = 1, group = 'BufReviewLabel', finish = 13 },
    { line = 1, group = 'BufReviewTitle', start = 14 },
    { line = 2, group = 'BufReviewTitle' },
    { line = 3, group = 'BufReviewLabel', finish = 9 },
    { line = 3, group = 'BufReviewAuthor', start = 10 },
    { line = 4, group = 'BufReviewLabel', finish = 11 },
    { line = 4, group = 'BufReviewState', start = 12 },
    { line = 5, group = 'BufReviewLabel', finish = 10 },
    { line = 5, group = state.error and 'BufReviewError' or 'BufReviewState', start = 11 },
  }
  lines[#lines + 1] = 'Session: ' .. view.id
  highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel', finish = 8 }
  if view.stats then
    lines[#lines + 1] = 'Publication: ' .. view.stats.status:gsub('_', ' ')
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewState' }
    lines[#lines + 1] = 'Pending: ' .. counts_text(view.stats.pending)
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPending' }
    lines[#lines + 1] = 'Published: ' .. counts_text(view.stats.published)
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewState' }
  end
  local objects = state.objects
  local code_status = (not objects or not objects.done) and 'preparing pinned commits'
    or (objects.error and 'fetch failed; open a file or :Review refresh to retry') or 'ready'
  lines[#lines + 1] = 'Code: ' .. code_status
  highlights[#highlights + 1] = { line = #lines, group = objects and objects.error and 'BufReviewError' or 'BufReviewState' }
  if state.error or view.last_error then
    local first = #lines + 1
    vim.list_extend(lines, vim.split(('Error: ' .. (state.error or view.last_error)):gsub('\r\n', '\n'), '\n', { plain = true }))
    for line = first, #lines do highlights[#highlights + 1] = { line = line, group = 'BufReviewError' } end
  end
  if view.attempt then
    lines[#lines + 1] = 'Publication in progress; frozen saved changes: ' .. view.attempt.id
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPending' }
  end
  for _, attempt in ipairs(view.suspended_attempts or {}) do
    lines[#lines + 1] = 'Publication result uncertain; reconciliation required: ' .. attempt.id
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPending' }
  end
  local rows, sections, folds = {}, {}, {}
  if branches then
    rows[2] = { pr_details = true }
    lines[#lines + 1] = 'PR creation: pending'
    rows[#lines] = { pr_details = true }
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPending' }
  else
    local status = state.checkout_status
    local source = pull.source and pull.source.branch or 'PR source unavailable'
    local text = 'Checkout: ' .. (status and status.branch or (state.checkout_error and 'unavailable' or 'checking')) .. ' → ' .. source
    if state.checkout_running then text = text .. ' [switching]'
    elseif status then
      local relationship = ({ at_pr = 'at PR head', ahead = 'ahead of PR', behind = 'behind PR', diverged = 'diverged from PR', unknown = 'PR history unavailable' })[status.relationship]
      text = text .. ' [' .. (relationship or 'different branch') .. ']'
      if status.operation then text = text .. ' [' .. status.operation .. ']' end
      text = text .. (' | %d staged, %d modified, %d untracked'):format(status.staged or 0, status.modified or 0, status.untracked or 0)
      if (status.conflicted or 0) > 0 then text = text .. (' | %d conflicts'):format(status.conflicted) end
    end
    lines[#lines + 1] = text:gsub('[\r\n]', ' ')
    rows[#lines] = { checkout = true }
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel', finish = 9 }
    highlights[#highlights + 1] = { line = #lines, group = status and status.matches and 'BufReviewState' or 'BufReviewPending', start = 10 }
    if state.checkout_error then
      lines[#lines + 1] = 'Checkout error: ' .. state.checkout_error:gsub('[\r\n]', ' ')
      rows[#lines] = { checkout = true }
      highlights[#highlights + 1] = { line = #lines, group = 'BufReviewError' }
    end
  end
  local function comment_entry(row, object, suffix)
    local comment = content(row)
    local at, author, flags, location = comment_metadata(view, row)
    if not location then at = row.summary and 'Review summary' or 'PR discussion' end
    if suffix then flags[#flags + 1] = suffix end
    at, author = at:gsub('[\r\n]', ' '), author:gsub('[\r\n]', ' ')
    return {
      text = at .. '  @' .. author .. (#flags > 0 and ('  [' .. table.concat(flags, ', ') .. ']') or ''),
      preview = preview(comment.body), row = object, path_end = #at + 2,
      author_start = #at + 4, author_end = #at + #author + 5,
      flags_start = #at + #author + 7,
      flags_group = (row.conflict or row.anchor_error) and 'BufReviewError' or 'BufReviewState',
    }
  end
  local function section(name, entries)
    lines[#lines + 1] = ''
    lines[#lines + 1] = ('%s (%d)'):format(name, #entries)
    sections[name], folds[#lines] = #lines, '>1'
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewSection' }
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewCount', start = #name + 2, finish = #lines[#lines] - 1 }
    for _, entry in ipairs(entries) do
      lines[#lines + 1] = '  ' .. entry.text:gsub('[\r\n]', ' ')
      rows[#lines], folds[#lines] = entry.row, 1
      highlights[#highlights + 1] = { line = #lines, group = entry.path_end and 'BufReviewPath' or (entry.row.file and 'BufReviewPath' or 'BufReviewState'), start = entry.path_start or 2, finish = entry.path_end }
      for _, span in ipairs(entry.highlights or {}) do
        highlights[#highlights + 1] = { line = #lines, group = span.group, start = span.start, finish = span.finish }
      end
      if entry.author_start then
        highlights[#highlights + 1] = { line = #lines, group = 'BufReviewAuthor', start = entry.author_start, finish = entry.author_end }
        if entry.flags_start < #lines[#lines] then highlights[#highlights + 1] = { line = #lines, group = entry.flags_group, start = entry.flags_start } end
      end
      if entry.preview then
        lines[#lines + 1] = '    ' .. entry.preview
        rows[#lines], folds[#lines] = entry.row, 1
        highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPreview', start = 4 }
      end
    end
  end
  local function comments(ids)
    local entries = {}
    for _, id in ipairs(ids or {}) do
      local row = find(view, id)
      if row then entries[#entries + 1] = comment_entry(row, { comment = id, checklist = true }) end
    end
    return entries
  end
  section('Todo', comments(view.todo))
  local pending, included = {}, {}
  for _, row in ipairs(view.comments) do
    local id = content(row).id
    if unpublished(row) or row.pending_delete or row.conflict then
      included[id] = true
      pending[#pending + 1] = comment_entry(row, { comment = id, draft = unpublished(row), conflict = row.conflict })
    end
  end
  for _, op in ipairs(view.pending or {}) do
    local change = op.change or op
    local id = type(change.comment) == 'string' and change.comment or nil
    if change.kind == 'set_file_viewed' then
      pending[#pending + 1] = { text = ('%s → %s'):format(change.path, change.viewed and 'viewed' or 'unviewed'), row = { file = change.path } }
    elseif id and not included[id] then
      local row = find(view, id)
      if row then
        included[id] = true
        local action = ({ edit_comment = 'edit staged', delete_comment = 'delete staged', set_comment_reaction = change.present and 'thumbs-up staged' or 'thumbs-up removal staged', create_comment = 'comment staged' })[change.kind] or 'change staged'
        pending[#pending + 1] = comment_entry(row, { comment = id, conflict = row.conflict }, action)
      end
    end
  end
  if view.wip and view.wip.conclusion then
    local outcome = ({ comment = 'Comment', approve = 'Approve', request_changes = 'Request changes' })[view.wip.conclusion.outcome] or view.wip.conclusion.outcome
    pending[#pending + 1] = { text = 'Review conclusion: ' .. outcome, row = { conclusion = true } }
  end
  section('Unpublished changes', pending)
  for _, viewed in ipairs({ false, true }) do
    local entries = {}
    for _, file in ipairs(view.files) do
      if file.viewed == viewed then
        local status = file.change == 'added' and 'A' or (file.change == 'deleted' and 'D' or 'M')
        local added, removed = '+' .. (file.stats and file.stats.additions or '?'), '-' .. (file.stats and file.stats.deletions or '?')
        local prefix = status .. ' ' .. added .. ' ' .. removed .. '  '
        entries[#entries + 1] = {
          text = prefix .. file.path, path_start = 2 + #prefix,
          row = { file = file.path, file_row = true },
          highlights = {
            { group = status == 'A' and 'BufReviewAdded' or (status == 'D' and 'BufReviewDeleted' or 'BufReviewModified'), start = 2, finish = 3 },
            { group = 'BufReviewAdded', start = 4, finish = 4 + #added },
            { group = 'BufReviewDeleted', start = 5 + #added, finish = 5 + #added + #removed },
          },
        }
      end
    end
    section(viewed and 'Viewed files' or 'Unviewed files', entries)
  end
  section('Done', comments(view.done))
  local publications = {}
  for _, summary in ipairs(view.publications or {}) do
    publications[#publications + 1] = {
      text = publication_text(summary),
      preview = 'Planned: ' .. counts_text(summary.planned) .. ' | Published: ' .. counts_text(summary.published),
      row = { publication = summary.id },
    }
  end
  section('Publication history', publications)
  if not state.home or not api.nvim_buf_is_valid(state.home) then
    state.home = scratch(state, 'home', nil, lines)
    vim.keymap.set('n', 'cc', function() dispatch('conclude') end, { buffer = state.home, silent = true })
  else replace(state.home, lines) end
  buffers[state.home].rows = rows
  buffers[state.home].dirty = nil
  buffers[state.home].sections, buffers[state.home].folds = sections, folds
  api.nvim_buf_clear_namespace(state.home, home_namespace, 0, -1)
  for _, highlight in ipairs(highlights) do
    api.nvim_buf_add_highlight(state.home, home_namespace, highlight.group, highlight.line - 1, highlight.start or 0, highlight.finish or -1)
  end
  for _, win in ipairs(vim.fn.win_findbuf(state.home)) do
    if vim.wo[win].foldmethod ~= 'expr' then vim.wo[win].foldlevel = 1 end
    vim.wo[win].foldmethod = 'expr'
    vim.wo[win].foldexpr = "v:lua.require'buf_review'.foldexpr()"
    restore_home_folds(state, win, cursor)
    if not cursor and (state.selected or state.selected_file) then
      local fallback, preferred
      for line = 1, #lines do
        local row = rows[line]
        if row and ((state.selected and row.comment == state.selected) or (state.selected_file and row.file == state.selected_file)) then
          fallback = fallback or line
          if row.checklist or row.file_row then preferred = line; break end
        end
      end
      if preferred or fallback then api.nvim_win_set_cursor(win, { preferred or fallback, 0 }) end
    end
  end
end

local function display_home(state)
  if state.loading then capture_home_folds(state) end
  if state.loading then
    local local_intent = state.view and state.view.pr.source.kind == 'local_intent'
    local lines = { local_intent and 'Loading local review…' or 'Loading remote review…', '', state.loading }
    state.loading_buf = scratch(state, 'loading', nil, lines)
  else render_home(state, api.nvim_win_get_buf(state.top) == state.home and 'object' or nil) end
  local buf = state.loading and state.loading_buf or state.home
  local entering = api.nvim_win_get_buf(state.top) ~= buf
  leave_diff(state.top)
  show_buffer(state.top, buf)
  vim.wo[state.top].diff = false
  vim.wo[state.top].winbar = ''
  vim.wo[state.top].foldmethod = state.loading and 'manual' or 'expr'
  vim.wo[state.top].foldexpr = "v:lua.require'buf_review'.foldexpr()"
  vim.wo[state.top].foldtext = 'getline(v:foldstart)'
  if entering then vim.wo[state.top].foldlevel = 1 end
  if not state.loading then restore_home_folds(state, state.top) end
end

home = function(state)
  guard(state)
  state.repository.opening = {}
  settle(state)
  tab(state)
  if valid(state.discussion) then leave_diff(state.discussion); api.nvim_win_close(state.discussion, false) end
  state.discussion = nil
  if valid(state.right) then leave_diff(state.right); api.nvim_win_close(state.right, false) end
  state.right = nil
  if valid(state.left) then state.top = state.left end
  state.left = nil
  if not valid(state.top) then state.top = api.nvim_get_current_win() end
  display_home(state)
  api.nvim_set_current_win(state.top)
end

local function show_browser(state, buf)
  guard(state)
  state.repository.opening = {}
  tab(state)
  if valid(state.discussion) then leave_diff(state.discussion); api.nvim_win_close(state.discussion, false); state.discussion = nil end
  if valid(state.right) then leave_diff(state.right); api.nvim_win_close(state.right, false); state.right = nil end
  if valid(state.left) then state.top = state.left; state.left = nil end
  leave_diff(state.top)
  show_buffer(state.top, buf)
  api.nvim_set_current_win(state.top)
  vim.wo.diff, vim.wo.winbar, vim.wo.foldmethod = false, '', 'manual'
end

local function render_prs(state, pulls, err)
  local query = state.repository.query
  local chosen
  if state.list and api.nvim_buf_is_valid(state.list) and valid(state.top) and api.nvim_win_get_buf(state.top) == state.list then
    chosen = selected(buffers[state.list])
  end
  state.pull_requests = pulls or state.pull_requests or {}
  local lines = { ('Pull requests | state=%s assignee=%s'):format(query.state or 'open', query.assignee or 'all') }
  if err then vim.list_extend(lines, vim.split(('Error: ' .. err):gsub('\r\n', '\n'), '\n', { plain = true })) end
  lines[#lines + 1] = ''
  local rows, selected_line = {}, nil
  for _, pull in ipairs(state.pull_requests) do
    local author = type(pull.author) == 'table' and (pull.author.name or pull.author.login) or pull.author or 'unknown'
    local assignees = {}
    for _, user in ipairs(pull.assignees or {}) do assignees[#assignees + 1] = type(user) == 'table' and (user.name or user.login) or user end
    lines[#lines + 1] = ('#%s %s @%s [%s]'):format(pull.number, pull.title, author, table.concat(assignees, ', ')):gsub('[\r\n]', ' ')
    rows[#lines] = { pull = pull }
    if chosen and chosen.pull.url == pull.url then selected_line = #lines end
  end
  state.list = scratch(state, 'prs', nil, lines)
  buffers[state.list].rows = rows
  show_browser(state, state.list)
  if selected_line then api.nvim_win_set_cursor(0, { selected_line, 0 }) end
end

local function render_sessions(state, browser)
  local lines, rows, highlights = { browser.pr and 'Sessions · selected pull request' or 'Sessions' }, {}, {}
  local chosen
  if state.session_browser and api.nvim_buf_is_valid(state.session_browser) and api.nvim_get_current_buf() == state.session_browser then
    chosen = selected(buffers[state.session_browser])
  end
  local selected_line
  for _, group in ipairs(browser.groups) do
    lines[#lines + 1] = ''
    local pull = group.pr
    local branches = pull.source and pull.source.kind == 'local_intent' and pull.source.branches
    local label = branches and (branches.source.branch .. ' → ' .. branches.sink.branch .. ' [local]')
      or (pull.number and ('#' .. pull.number) or (pull.url or pull.id))
    lines[#lines + 1] = (label .. ' ' .. (pull.title ~= '' and pull.title or '(untitled)')):gsub('[\r\n]', ' ')
    rows[#lines] = { pr = pull.id }
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewSection' }
    for _, session in ipairs(group.sessions) do
      local stats = session.stats
      local object = { session = session.id, pr = pull.id }
      lines[#lines + 1] = ('  %s  @%s  [%s%s]'):format(session.id, session.reviewer.name or session.reviewer.id, stats.status:gsub('_', ' '), session.active and ', selected' or ''):gsub('[\r\n]', ' ')
      rows[#lines] = object
      highlights[#highlights + 1] = { line = #lines, group = 'BufReviewState', start = 2 }
      if chosen and chosen.session == session.id then selected_line = #lines end
      lines[#lines + 1] = ('    Pending: %s | Published: %s | Conflicts: %d'):format(counts_text(stats.pending), counts_text(stats.published), stats.conflicts)
      rows[#lines] = object
      highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPreview', start = 4 }
    end
  end
  state.session_browser = scratch(state, 'sessions', nil, lines)
  local meta = buffers[state.session_browser]
  meta.rows, meta.pr = rows, browser.pr
  api.nvim_buf_clear_namespace(state.session_browser, home_namespace, 0, -1)
  for _, span in ipairs(highlights) do api.nvim_buf_add_highlight(state.session_browser, home_namespace, span.group, span.line - 1, span.start or 0, -1) end
  show_browser(state, state.session_browser)
  if selected_line then api.nvim_win_set_cursor(0, { selected_line, 0 }) end
end

local function list(state, query)
  navigate(state, vim.tbl_extend('force', { op = 'list' }, query or state.repository.query))
end

refresh = function(state, callback)
  if state.refreshing then
    if callback then state.refreshing.callbacks[#state.refreshing.callbacks + 1] = callback end
    return
  end
  settle(state)
  local job = { callbacks = callback and { callback } or {}, report = not callback }
  state.refreshing = job
  local was_loading = state.loading
  local loading = first_load and 'Fetching comments, reactions and file progress.' or nil
  if loading then state.loading = loading end
  first_load = false
  if (state.loading or was_loading) and valid(state.top) and (api.nvim_win_get_buf(state.top) == state.home or api.nvim_win_get_buf(state.top) == state.loading_buf) then
    display_home(state)
  end
  refresh_snapshot(state, false, function(response, err)
    state.refreshing = nil
    if loading and state.loading == loading then state.loading = nil end
    local rendered, problem = pcall(function()
      state.refresh_deferred = response and response.refresh_deferred or false
      if not (response and response.refresh_deferred) then
        state.error = err
        if not err then state.refreshed_at = os.date('%H:%M:%S') end
      end
      if response then apply(state, response, 'object') end
      refresh_checkout(state)
      prepare_objects(state)
      if valid(state.top) and api.nvim_win_get_buf(state.top) == state.loading_buf then display_home(state) end
    end)
    if not rendered then
      err = err or ('Could not display refreshed review: ' .. tostring(problem))
      state.error = err
    end
    if err and job.report then notify(err, vim.log.levels.ERROR) end
    for _, receive in ipairs(job.callbacks) do receive(response, err) end
  end)
end

local function publish(state, operation, callback)
  local ok, err
  if state.publishing then ok, err = false, 'Publication is still running; wait before retrying'
  else ok, err = pcall(settle, state) end
  if not ok then
    if callback then callback(nil, err) else error(err) end
    return
  end
  state.publishing = true
  local finished = false
  local function published(response, publication_error)
    local function complete(latest, refresh_error)
      local rendered, problem = pcall(function()
        render_pending(true)
        highlight_comment(true)
      end)
      if not rendered then refresh_error = refresh_error or ('Could not redraw review: ' .. tostring(problem)) end
      state.publishing = nil
      finished = true
      if callback then callback(response, publication_error)
      elseif publication_error then notify(publication_error, vim.log.levels.ERROR) end
      if refresh_error then
        notify('Publication finished; refresh failed: ' .. refresh_error, vim.log.levels.WARN)
      elseif latest and latest.refresh_deferred then
        notify('Publication finished; refresh deferred. Run :Review refresh to reconcile remote state.', vim.log.levels.WARN)
      end
    end
    local function fetch()
      local started, problem = pcall(refresh, state, complete)
      if not started then complete(nil, tostring(problem)) end
    end
    -- A fetch already running when the write finishes cannot confirm that write.
    if state.refreshing then refresh(state, fetch) else fetch() end
  end
  local function send()
    local ok, err = pcall(request, state, operation, {}, published)
    if not ok then published(nil, err) end
  end
  if state.refreshing then refresh(state, send) else send() end
  while not finished do
    local _, reason = vim.wait(1000, function()
      if state.writer then state.writer.flush(true) end
      return finished
    end, 10)
    if reason == -2 then
      error('Publication continues in the background; wait for it to finish before retrying')
    end
  end
end

navigate = function(state, payload)
  local repository = state.repository
  if repository.navigating then return end
  guard(repository.current)
  guard(state)
  repository.opening = {}
  repository.navigation = (repository.navigation or 0) + 1
  settle(repository.current)
  if state ~= repository.current then settle(state) end
  if payload.op == 'list' then repository.query = { assignee = payload.assignee, state = payload.state } end
  repository.navigating = true
  if (first_load or not state.view) and payload.op ~= 'sessions' and payload.op ~= 'list' then
    state.loading = 'Finding review session.'
    home(state)
  end
  local function failure(err)
    repository.navigating = nil
    state.loading = nil
    if not protected(state) and not protected(repository.current) then
      if payload.op == 'list' then render_prs(state, nil, err)
      elseif state.view then home(state)
      elseif state.loading_buf and api.nvim_buf_is_valid(state.loading_buf) then
        replace(state.loading_buf, vim.split('Error: ' .. err, '\n', { plain = true }))
      end
    end
    notify(err, vim.log.levels.ERROR)
  end
  local receive
  receive = function(response, err)
    if err then failure(err); return end
    if protected(repository.current) or protected(state) then
      failure('Unsaved composer; session selection cancelled')
      return
    end
    if response.route == 'batch' then
      client.request(state.root, { op = 'select', batch = response.batch, fresh_review = response.fresh_review }, receive)
      return
    end
    local ok, problem = pcall(function()
      if response.workspace then
        local view = response.workspace
        local current = repository.sessions[view.id]
        if not current then
          current = { root = state.root, repository = repository }
          repository.sessions[view.id] = current
        end
        if state.loading then
          current.loading = state.loading
          state.loading = nil
        end
        if response.fresh == true then
          first_load = false
          current.loading, current.error, current.refresh_deferred = nil, nil, false
          current.refreshed_at = os.date('%H:%M:%S')
        end
        apply(current, response)
        current.loading = 'Preparing Git history.'
        home(current)
        prepare_objects(current, function()
          current.loading = nil
          if valid(current.top) and api.nvim_win_get_buf(current.top) == current.loading_buf then display_home(current) end
          if response.fresh ~= true then refresh(current) end
        end, true)
        repository.navigating = nil
      elseif response.sessions then
        state.loading = nil
        render_sessions(state, response.sessions)
      elseif response.pull_requests then
        state.loading = nil
        if response.route == 'prs' then repository.query = {} end
        render_prs(state, response.pull_requests)
      else error('Invalid review navigation response') end
    end)
    repository.navigating = nil
    if not ok then failure(problem) end
  end
  client.request(state.root, payload, receive)
end

local function immutable(state, path, side, revision, text, code)
  local lines = vim.split(text or '', '\n', { plain = true })
  if lines[#lines] == '' and #lines > 1 then table.remove(lines) end
  local buf, meta = scratch(state, 'diff', path, lines, false, 'diff:' .. side)
  meta.path, meta.side, meta.revision = path, side, revision
  meta.historical, meta.location, meta.root = code and code.historical, code and code.location, code and code.root
  meta.source_path = side == 'base' and code and code.base_path or path
  meta.source_lines = text == '' and 0 or #lines
  local marker = vim.b[buf].buf_review
  marker.side, marker.path, marker.revision = side, path, revision
  marker.historical, marker.provenance = meta.historical, code and code.provenance
  marker.source_path = meta.source_path
  vim.b[buf].buf_review = marker
  local ft = vim.filetype.match({ filename = meta.source_path })
  if ft then vim.bo[buf].filetype = ft end
  vim.keymap.set('x', 'a', function()
    local ok, err = pcall(M.new_comment, nil, true)
    if not ok then notify(err, vim.log.levels.ERROR) end
  end, { buffer = buf, silent = true })
  annotate(buf, meta, state.view)
  return buf
end

local function show_code(state, code)
  local path = code.path
  tab(state)
  if valid(state.discussion) then
    local buf = api.nvim_win_get_buf(state.discussion)
    local meta, edit = buffers[buf], editor.get(buf)
    local location = meta and meta.state == state and (meta.location or anchor(state.view, find(state.view, meta.object)))
    if edit and edit.target then
      location = edit.target.kind == 'diff' and edit.target.target
        or edit.target.kind == 'reply' and anchor(state.view, find(state.view, edit.target.target))
    end
    if location and location.path ~= path then
      leave_diff(state.discussion)
      api.nvim_win_close(state.discussion, false)
      state.discussion = nil
    end
  end
  if not valid(state.left) then
    state.left = valid(state.top) and state.top or api.nvim_get_current_win()
    api.nvim_set_current_win(state.left)
    vim.cmd('keepjumps rightbelow vsplit')
    state.right = api.nvim_get_current_win()
  end
  if not valid(state.right) then
    api.nvim_set_current_win(state.left)
    vim.cmd('keepjumps rightbelow vsplit')
    state.right = api.nvim_get_current_win()
  end
  leave_diff(state.left)
  leave_diff(state.right)
  local base = immutable(state, path, 'base', code.revision, code.base_content, code)
  local head = immutable(state, path, 'head', code.revision, code.head_content, code)
  buffers[base].binary, buffers[head].binary = code.binary, code.binary
  show_buffer(state.left, base)
  show_buffer(state.right, head)
  for _, win in ipairs({ state.left, state.right }) do
    api.nvim_win_call(win, function() vim.cmd('diffthis'); vim.wo.wrap = false end)
  end
  if code.historical then
    local label = 'Historical' .. (code.provenance == 'reconstructed' and ' (reconstructed)' or '')
    vim.wo[state.left].winbar = ('%s %s  %s'):format(label, code.revision.base, code.base_path or path):gsub('%%', '%%%%')
    vim.wo[state.right].winbar = ('PR head %s  %s%s'):format(code.revision.head, path, code.head_missing and ' [file absent]' or ''):gsub('%%', '%%%%')
  else
    vim.wo[state.left].winbar = ('Base %s  %s'):format(code.revision.base, code.base_path or path):gsub('%%', '%%%%')
    vim.wo[state.right].winbar = ('Head %s  %s'):format(code.revision.head, path):gsub('%%', '%%%%')
  end
  state.top = state.left
  state.path = path
  api.nvim_set_current_win(state.right)
  activate(head)
end

local function show_historical_hunk(state, response, id)
  tab(state)
  if valid(state.right) then leave_diff(state.right); api.nvim_win_close(state.right, false) end
  state.right = nil
  state.left = valid(state.left) and state.left or (valid(state.top) and state.top or api.nvim_get_current_win())
  leave_diff(state.left)
  local lines = { 'Historical context unavailable', response.message or '', '' }
  vim.list_extend(lines, vim.split(response.diff_hunk or '', '\n', { plain = true }))
  local buf, meta = scratch(state, 'historical', id, lines)
  meta.historical = true
  meta.path = response.anchor and response.anchor.path
  for line = 1, #lines do meta.rows[line] = { comment = id } end
  vim.bo[buf].filetype = 'diff'
  show_buffer(state.left, buf)
  vim.wo[state.left].winbar = 'Historical reference · recorded hunk'
  state.top, state.path = state.left, meta.path
  api.nvim_set_current_win(state.left)
end

local function reveal_location(left, right, location)
  local range = location and location.range
  if not range then return end
  local win = range.start.side == 'base' and left or right
  if not valid(win) then return end
  local line = range.start.line
  if line < 1 or line > api.nvim_buf_line_count(api.nvim_win_get_buf(win)) then return end
  -- Run after the discussion split has its final height. A native jump updates
  -- diff cursor/scroll binding and matching folds; setting one cursor does not.
  api.nvim_win_call(win, function()
    vim.cmd('keepjumps normal! ' .. line .. 'G0zv')
  end)
end

local function open_historical(state, id)
  local selection, navigation = state.repository.current, state.repository.navigation
  local revision, opening = vim.deepcopy(state.view.comparison), {}
  local head = state.view.review.read.pull_request.revision.head
  state.repository.opening = opening
  prepare_objects(state, function()
    if state.repository.opening ~= opening or state.repository.current ~= selection or state.repository.navigation ~= navigation then return end
    request(state, 'historical_code', { comment = id }, function(response, err)
      if state.repository.opening ~= opening or state.repository.current ~= selection or state.repository.navigation ~= navigation
        or not vim.deep_equal(state.view.comparison, revision) then return end
      if err then notify(err, vim.log.levels.ERROR); return end
      if protected(state) then notify('Unsaved composer; historical view preserved', vim.log.levels.ERROR); return end
      if head ~= state.view.review.read.pull_request.revision.head then open_historical(state, id); return end
      if response.code and response.code.revision.head ~= head then
        notify('PR head changed while loading; reopen this comment', vim.log.levels.WARN)
        return
      end
      local row = find(state.view, id)
      if not row then return end
      if response.code then
        response.code.root = content(root_row(state.view, row)).id
        show_code(state, response.code)
      else show_historical_hunk(state, response, id) end
      thread(state, row)
      if response.code then reveal_location(state.left, state.right, response.code.location) end
    end, false)
  end)
end

open_file = function(state, path, callback)
  guard(state)
  local opening = {}
  state.repository.opening = opening
  settle(state)
  local selection = state.repository.current
  local navigation = state.repository.navigation
  local revision = vim.deepcopy(require_view(state).comparison)
  prepare_objects(state, function(problem)
    if problem then return end
    if state.repository.opening ~= opening or state.repository.current ~= selection or state.repository.navigation ~= navigation
      or not vim.deep_equal(state.view.comparison, revision) then return end
    request(state, 'code', { path = path }, function(response, err)
      if state.repository.opening ~= opening or state.repository.current ~= selection or state.repository.navigation ~= navigation then return end
      if err then notify(err, vim.log.levels.ERROR); return end
      if protected(state) then notify('Unsaved composer; code view preserved', vim.log.levels.ERROR); return end
      local code = response.code
      if not vim.deep_equal(state.view.comparison, revision) or not vim.deep_equal(code.revision, revision) then return end
      show_code(state, code)
      if callback then callback() end
    end, false)
  end)
end

local discussion_namespace = api.nvim_create_namespace('buf-review-discussion')
local divider = '-----------------------------'

local function discussion_header(view, row, indent)
  local at, author, flags, location = comment_metadata(view, row)
  if not location then at = row.summary and 'Review summary' or 'PR discussion' end
  at, author = at:gsub('[\r\n]', ' '), author:gsub('[\r\n]', ' ')
  local text = string.rep(' ', indent) .. at .. '  @' .. author
  local highlights = {
    { group = 'BufReviewPath', start = indent, finish = indent + #at },
    { group = 'BufReviewAuthor', start = indent + #at + 2, finish = #text },
  }
  if #flags > 0 then
    local start = #text + 2
    text = text .. '  [' .. table.concat(flags, ', ') .. ']'
    highlights[#highlights + 1] = { group = (row.conflict or row.anchor_error) and 'BufReviewError' or 'BufReviewState', start = start, finish = #text }
  end
  return text:gsub('[\r\n]', ' '), highlights
end

local function highlight_discussion(buf, highlights)
  api.nvim_buf_clear_namespace(buf, discussion_namespace, 0, -1)
  for _, highlight in ipairs(highlights) do
    api.nvim_buf_add_highlight(buf, discussion_namespace, highlight.group, highlight.line - 1, highlight.start or 0, highlight.finish or -1)
  end
end

thread_list = function(state, roots, existing, cursor, location)
  local views = {}
  if existing then
    for _, win in ipairs(vim.fn.win_findbuf(existing)) do
      api.nvim_win_call(win, function()
        local view = vim.fn.winsaveview()
        local rows = buffers[existing].rows
        local object = rows[view.lnum]
        views[win] = { view = view, comment = object and object.comment, preview = object and rows[view.lnum - 1] == object }
      end)
    end
  end
  -- Keep comment objects away from Vim's clamped empty-motion range at BOF.
  local lines, objects, highlights = { '' }, {}, {}
  for _, row in ipairs(roots) do
    local heading, spans = discussion_header(state.view, row, 2)
    local object = { comment = content(row).id }
    lines[#lines + 1] = heading
    objects[#lines] = object
    for _, span in ipairs(spans) do span.line = #lines; highlights[#highlights + 1] = span end
    lines[#lines + 1] = '    ' .. preview(content(row).body)
    objects[#lines] = object
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewPreview', start = 4 }
  end
  lines[#lines + 1] = ''
  lines[#lines + 1] = divider
  highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel' }
  lines[#lines + 1] = ''
  local buf, meta
  if existing then buf, meta = existing, buffers[existing]; replace(buf, lines)
  else
    buf, meta = scratch(state, 'threads', nil, lines)
    meta.location = vim.deepcopy(location)
  end
  meta.root_ids = {}
  for _, row in ipairs(roots) do meta.root_ids[#meta.root_ids + 1] = content(row).id end
  meta.rows = objects
  highlight_discussion(buf, highlights)
  if not existing then
    discussion(state, buf)
    api.nvim_win_set_cursor(0, { #roots > 0 and 2 or #lines, 0 })
  end
  for win, saved in pairs(views) do
    local view = saved.view
    if cursor ~= 'position' and saved.comment then
      for line = 2, #lines, 2 do
        if objects[line] and objects[line].comment == saved.comment then
          local current = line + (saved.preview and 1 or 0)
          view = vim.tbl_extend('force', {}, view, { lnum = current, topline = math.max(1, view.topline + current - view.lnum) })
          break
        end
      end
    end
    if valid(win) then api.nvim_win_call(win, function() vim.fn.winrestview(view) end) end
  end
end

thread = function(state, row, existing, cursor)
  if not existing then guard(state) end
  local root = row and (root_row(state.view, row) or row)
  local root_id = root and content(root).id or buffers[existing].object
  local selected_id = row and content(row).id
  local views = {}
  if existing and cursor then
    for _, win in ipairs(vim.fn.win_findbuf(existing)) do
      api.nvim_win_call(win, function()
        local saved = { view = vim.fn.winsaveview() }
        local rows = buffers[existing].rows
        local object = rows[saved.view.lnum]
        if object and object.comment then
          saved.comment = object.comment
          for line = 1, saved.view.lnum do
            if rows[line] and rows[line].comment == object.comment then
              saved.offset = saved.view.lnum - line
              break
            end
          end
        end
        views[win] = saved
      end)
    end
  elseif existing then
    for _, win in ipairs(vim.fn.win_findbuf(existing)) do
      local old = buffers[existing].rows[api.nvim_win_get_cursor(win)[1]]
      if old and old.comment then selected_id = old.comment; break end
    end
  end
  local lines, objects, highlights = { '' }, {}, {}
  local location = row and anchor(state.view, row)
  if location and not matches(state.view, location) then
    lines = { '', 'Historical location', '' }
    highlights[#highlights + 1] = { line = 2, group = 'BufReviewPending' }
  elseif root and not location and content(root).target.kind == 'diff' then
    lines = { '', 'Location unavailable', '' }
    highlights[#highlights + 1] = { line = 2, group = 'BufReviewPending' }
  end
  local first_member = true
  for _, member in ipairs(state.view.comments) do
    local member_root = root_row(state.view, member)
    if root and member_root and content(member_root).id == root_id then
      local comment = content(member)
      if not first_member then
        lines[#lines + 1] = divider
        highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel' }
        lines[#lines + 1] = ''
      end
      first_member = false
      local heading, spans = discussion_header(state.view, member, 0)
      lines[#lines + 1] = heading
      local object = { comment = comment.id, conflict = member.conflict }
      objects[#lines] = object
      for _, span in ipairs(spans) do span.line = #lines; highlights[#highlights + 1] = span end
      lines[#lines + 1] = ''
      for _, line in ipairs(vim.split(comment.body, '\n', { plain = true })) do
        lines[#lines + 1] = line
        objects[#lines] = object
      end
      lines[#lines + 1] = ''
    end
  end
  if root and content(root).target.kind == 'diff' and not root.summary then
    lines[#lines + 1] = divider
    highlights[#highlights + 1] = { line = #lines, group = 'BufReviewLabel' }
    lines[#lines + 1] = ''
  end
  local buf, meta
  if existing then buf, meta = existing, buffers[existing]; replace(buf, lines)
  else buf, meta = scratch(state, 'thread', root_id, lines) end
  if not existing then meta.comment = selected_id end
  vim.bo[buf].filetype = 'markdown'
  meta.rows = objects
  highlight_discussion(buf, highlights)
  if not existing then discussion(state, buf) end
  if cursor then
    for win, saved in pairs(views) do
      local view = saved.view
      if cursor == 'object' and saved.comment then
        local first, last
        for line = 1, #lines do
          if objects[line] and objects[line].comment == saved.comment then
            first, last = first or line, line
          end
        end
        if first then
          local line = math.min(first + saved.offset, last)
          view = vim.tbl_extend('force', {}, view, {
            lnum = line, topline = math.max(1, view.topline + line - view.lnum),
          })
        end
      end
      if valid(win) then api.nvim_win_call(win, function() vim.fn.winrestview(view) end) end
    end
    return
  end
  local selected_line
  for line = 1, #lines do if objects[line] and objects[line].comment == selected_id then selected_line = line; break end end
  if selected_line then for _, win in ipairs(vim.fn.win_findbuf(buf)) do api.nvim_win_set_cursor(win, { selected_line, 0 }) end end
  if not existing then activate(buf) end
end

open_comment = function(state, id)
  guard(state)
  state.repository.opening = {}
  settle(state)
  local row = find(state.view, id)
  if not row then error('Comment no longer present in this workspace') end
  state.selected = id
  state.selected_file = nil
  local location = anchor(state.view, row)
  if location and not matches(state.view, location) then open_historical(state, id); return end
  local available = location and vim.tbl_contains(state.view.files, function(file)
    return file.path == location.path
  end, { predicate = true })
  if available then
    open_file(state, location.path, function()
      thread(state, row)
      if matches(state.view, location) then reveal_location(state.left, state.right, location) end
    end)
  else thread(state, row) end
end

local function composer_header(buf, message)
  if api.nvim_buf_is_valid(buf) then
    api.nvim_buf_clear_namespace(buf, namespace, 0, -1)
    local lines = {}
    for _, line in ipairs(vim.split(message, '\n', { plain = true })) do lines[#lines + 1] = { { line, 'Comment' } } end
    api.nvim_buf_set_extmark(buf, namespace, 0, 0, { virt_lines = lines, virt_lines_above = true })
  end
end

compose = function(state, options)
  state.repository.opening = {}
  settle(state)
  guard(state)
  local view = require_view(state)
  local row = options.row
  if row then
    row = find(view, content(row).id)
    if not row then error('Comment no longer present in this workspace') end
    if row.pending_delete then error('Comment has pending deletion; restore it before editing') end
  end
  local comment = row and content(row)
  local kind = options.kind or 'comment'
  local text = options.body or (comment and comment.body) or ''
  if kind == 'pr' then
    if view.pr.source.kind ~= 'local_intent' then error('Remote PR title and body are read-only') end
    text = view.pr.title .. '\n\n' .. view.pr.body
  end
  local target = options.target or (comment and comment.target)
  if not row and target and target.kind == 'diff' and not vim.deep_equal(target.target.revision, view.comparison) then
    error('Comparison changed; reopen this code location before composing')
  end
  local destination = options.destination or (row and row.destination) or (row and 'standalone') or (view.wip and view.wip.active and 'review' or 'standalone')
  local buf, meta = scratch(state, 'composer', comment and comment.id, vim.split(text, '\n', { plain = true }), true)
  local captured = view.generation
  local edit = { kind = kind, comment = comment and comment.id, target = target, destination = destination, state = state, relocation = options.relocation }
  edit.before_write = function() settle(state) end
  edit.return_buf, edit.return_view = options.return_buf, options.return_view
  local origin = api.nvim_get_current_buf()
  local origin_meta = buffers[origin]
  if kind ~= 'conclusion' and not edit.return_buf and api.nvim_get_current_win() == state.discussion
    and origin_meta and (origin_meta.kind == 'thread' or origin_meta.kind == 'threads') then
    edit.return_buf, edit.return_view = origin, vim.fn.winsaveview()
  end
  edit.return_win = api.nvim_get_current_win() == state.discussion and state.discussion_origin or api.nvim_get_current_win()
  local function saved_row(snapshot, id)
    local saved = id and find(snapshot, id)
    if not saved then return false end
    return {
      comment = saved.comment, destination = saved.destination,
      pending_delete = saved.pending_delete, conflict = saved.conflict,
      fetched = saved.fetched,
    }
  end
  local function signature(snapshot)
    snapshot = snapshot or state.view
    if kind == 'conclusion' then
      return { wip = snapshot.wip and snapshot.wip.id, conclusion = snapshot.wip and snapshot.wip.conclusion }
    elseif kind == 'pr' then
      return snapshot.pr
    end
    return {
      comment = saved_row(snapshot, edit.comment), comparison = snapshot.comparison,
      parent = edit.target and edit.target.kind == 'reply' and saved_row(snapshot, edit.target.target) or nil,
      wip = destination == 'review' and snapshot.wip and snapshot.wip.id or nil,
    }
  end
  local baseline = vim.deepcopy(signature())
  local message = options.message
  if not message then
    if kind == 'conclusion' then message = 'Conclusion: ' .. options.outcome
    elseif kind == 'pr' then
      local branches = view.pr.source.branches
      message = 'Local PR: ' .. branches.source.branch .. ' → ' .. branches.sink.branch
    else
      local location = target.kind == 'diff' and target.target or (target.kind == 'reply' and anchor(view, find(view, target.target)))
      message = (target.kind == 'reply' and 'Reply: ' or 'Comment: ') .. (location_text(location) or 'PR discussion') .. ' [' .. destination .. ']'
    end
  end
  if row and row.anchor_error then message = message .. '\nRejected anchor: ' .. row.anchor_error .. ' — :Review relocate/delete' end
  edit.status = function(status) composer_header(buf, message .. ' | ' .. status) end
  edit.save = function(body, callback)
    if edit.deletion_saved and vim.trim(body) == '' then callback(); return end
    if state.view.generation ~= captured then
      if not vim.deep_equal(baseline, signature()) then
        callback('Review target or saved content changed while composing; text retained. Inspect the updated workspace before reopening this composer.')
        return
      end
      captured = state.view.generation
    end
    local fields, operation
    if kind == 'conclusion' then
      operation, fields = 'summary', { conclusion = { outcome = options.outcome, body = body } }
    elseif kind == 'pr' then
      local lines = vim.split(body, '\n', { plain = true })
      local title = table.remove(lines, 1)
      if lines[1] == '' then table.remove(lines, 1) end
      operation, fields = 'pr_details', { revision = baseline.revision, title = title, body = table.concat(lines, '\n') }
    elseif options.resolve then
      operation, fields = 'resolve', { comment = edit.comment, resolution = 'edit', body = body }
    else
      operation, fields = 'save', { comment = edit.comment, target = edit.relocation and { kind = 'diff', target = edit.relocation } or (not edit.comment and target or nil), body = body, destination = destination }
    end
    local deleting = operation == 'save' and edit.comment and vim.trim(body) == ''
    if not edit.comment and kind == 'comment' and vim.trim(body) == '' then callback(); return end
    local function save_body(retried)
      request(state, operation, fields, function(response, err)
        local stale = err and (err:find('workspace changed in another session', 1, true) or err:find('Stale workspace generation', 1, true))
        if stale and not retried then
          request(state, 'get', {}, function(latest, read_error)
            if read_error then callback(read_error); return end
            if not latest.workspace or not vim.deep_equal(baseline, signature()) then
              callback('Review target or saved content changed while composing; text retained. Inspect the updated workspace before reopening this composer.')
              return
            end
            captured = state.view.generation
            save_body(true)
          end, false)
          return
        end
        if response and not err then
          captured = response.workspace.generation
          if kind == 'comment' then
            edit.comment = response.comment or response.workspace.saved_comment or edit.comment
            if not edit.comment then
              for _, candidate in ipairs(response.workspace.comments) do
                if unpublished(candidate) and content(candidate).body == body and vim.deep_equal(content(candidate).target, target) then edit.comment = content(candidate).id end
              end
            end
            meta.object = edit.comment
            local marker = vim.b[buf].buf_review
            marker.object = edit.comment
            vim.b[buf].buf_review = marker
            activate(buf)
          end
          options.resolve = false
          if deleting then
            edit.deletion_saved = true
            edit.relocation = nil
            edit.status('Deletion saved locally')
          elseif edit.deletion_saved then
            edit.deletion_saved = false
            composer_header(buf, message)
          elseif edit.relocation then
            edit.target = { kind = 'diff', target = edit.relocation }
            edit.relocation = nil
          end
          baseline = vim.deepcopy(signature(response.workspace))
        end
        if stale then
          err = err .. '\nWorkspace changed again during save; retry when the concurrent operation settles. Unsaved text remains.'
        end
        callback(err)
      end, captured)
      state.writer.flush(true)
    end
    save_body()
  end
  edit.submit = function(callback)
    if options.outcome ~= 'approve' and vim.trim(edit.saved_body or '') == '' then
      callback('Comment and Request changes require a nonblank summary')
      return
    end
    publish(state, 'submit', function(response, err)
      if err and response and response.workspace and not response.workspace.wip then
        notify('Review submitted; progress remains queued: ' .. err, vim.log.levels.WARN)
        callback()
      else callback(err) end
    end)
  end
  editor.attach(buf, edit)
  vim.bo[buf].modified = false
  discussion(state, buf)
  composer_header(buf, message)
  return buf
end

local function owned(state, row)
  local author = content(row).author
  return author and author.id == state.view.review.reviewer.id
end

local function picked(state, meta)
  local object = selected(meta)
  if not object or not object.comment then error('Select a comment first') end
  local row = find(require_view(state), object.comment)
  if not row then error('Selected comment is no longer available') end
  return row
end

local function deletion_actions(state, rows)
  local comments, seen = {}, {}
  guard(state)
  for _, row in ipairs(rows) do
    local id = content(row).id
    if not seen[id] then
      seen[id] = true
      comments[#comments + 1] = id
    end
  end
  return #comments > 0 and { { 'delete_comments', { comments = comments } } } or {}
end

delete_comments = function(first, last)
  local ok, err = pcall(function()
    local state, meta = get_state()
    if not meta or (meta.kind ~= 'home' and meta.kind ~= 'threads' and meta.kind ~= 'thread') then return end
    local rows = {}
    for line = first, last do
      local object = meta.rows[line]
      local row = object and object.comment and find(require_view(state), object.comment)
      if row then rows[#rows + 1] = row end
    end
    if #rows == 0 then return end
    ready(state)
    actions(state, deletion_actions(state, rows))
  end)
  if not ok then notify(err, vim.log.levels.ERROR) end
end

function M.delete_operator(kind)
  -- A canceled motion must not replace the visual context retained for dot.
  if pending_delete_visual ~= nil then delete_visual, pending_delete_visual = pending_delete_visual, nil end
  local first, last = vim.fn.getpos("'["), vim.fn.getpos("']")
  if first[2] > last[2] or (first[2] == last[2] and first[3] > last[3]) then return end
  if kind == 'char' and not delete_visual and first[2] == last[2] and vim.fn.getline(first[2]) == '' then return end
  delete_comments(first[2], last[2])
end

local function edit_comment(state, row)
  state.repository.opening = {}
  settle(state)
  row = find(require_view(state), content(row).id)
  if not row then error('Comment no longer present in this workspace') end
  if not owned(state, row) then error('Editing requires verified ownership of this comment') end
  if row.pending_delete then error('Comment has pending deletion; restore it before editing') end
  if row.conflict then show_conflict(state, row); return end
  compose(state, { row = row })
end

local function reply(state, row, destination, options)
  state.repository.opening = {}
  settle(state)
  row = find(require_view(state), content(row).id)
  if not row then error('Comment no longer present in this workspace') end
  if row.pending_delete then error('Cannot reply to a comment with pending deletion') end
  local root = root_row(state.view, row)
  if not root or content(root).target.kind ~= 'diff' or root.summary then
    error('This comment does not support diff-thread replies; create a standalone PR-wide comment from the home header')
  end
  if root.pending_delete then error('Cannot reply to a comment with pending deletion') end
  if unpublished(root) then
    if destination and destination ~= root.destination then error('Unpublished replies must share their parent destination') end
    destination = root.destination
  end
  return compose(state, vim.tbl_extend('force', options or {}, { target = { kind = 'reply', target = content(root).id }, destination = destination }))
end

local function range(meta, first, last)
  return { revision = meta.revision, path = meta.path, range = { start = { side = meta.side, line = first }, ['end'] = { side = meta.side, line = last } } }
end

function M.append()
  local ok, err = pcall(function()
    local state, meta = get_state()
    ready(state)
    local view = require_view(state)
    local options = { return_buf = api.nvim_get_current_buf(), return_view = vim.fn.winsaveview() }
    if meta and meta.kind == 'diff' then
      if not vim.deep_equal(meta.revision, view.comparison) then error('Comparison changed; reopen this code location before appending') end
      M.new_comment()
    elseif meta and meta.kind == 'threads' and meta.location then
      if not vim.deep_equal(meta.location.revision, view.comparison) then error('Comparison changed; reopen this code location before appending') end
      options.target = { kind = 'diff', target = vim.deepcopy(meta.location) }
      compose(state, options)
    elseif meta and meta.kind == 'thread' then
      local root = find(view, meta.object)
      if not root then error('Thread is no longer available') end
      reply(state, root, nil, options)
    else error('Append requires review code, a code-location list or conversation') end
    api.nvim_feedkeys('i', 'in', false)
  end)
  if not ok then notify(err, vim.log.levels.ERROR) end
end

function M.new_comment(destination, visual, args)
  local state, meta = get_state()
  ready(state)
  require_view(state)
  if not meta then error('New inline comments require a review diff; use home header for PR-wide comments') end
  local target
  if meta.kind == 'diff' then
    if meta.historical then error('Historical code is reference only; reply in its thread or open the current file') end
    if meta.source_lines == 0 then error('This comparison side has no source lines; create a file comment from its home row instead') end
    if meta.binary then error('Binary files support file comments from the home file row; line anchors are unavailable') end
    local first, last = api.nvim_win_get_cursor(0)[1], api.nvim_win_get_cursor(0)[1]
    if visual then
      if vim.fn.mode():find('\022', 1, true) then error('Blockwise selections cannot anchor comments; use characterwise or linewise selection') end
      local start = vim.fn.getpos('v')[2]
      first, last = math.min(first, start), math.max(last, start)
      local escape = api.nvim_replace_termcodes('<Esc>', true, false, true)
      api.nvim_cmd({ cmd = 'normal', bang = true, args = { escape } }, {})
    elseif args and args.range > 0 then
      if vim.fn.visualmode() == '\022' then error('Blockwise selections cannot anchor comments') end
      first, last = args.line1, args.line2
    end
    target = { kind = 'diff', target = range(meta, first, last) }
  elseif meta.kind == 'home' then
    local object = selected(meta)
    if object and object.file then target = { kind = 'diff', target = { path = object.file, revision = state.view.comparison } }
    elseif object then error('Select a file or use the home header to create a comment')
    else
      if destination == 'review' then error('Review-wide prose belongs in :Review conclude; PR-wide comments are standalone') end
      target, destination = { kind = 'pull_request' }, 'standalone'
    end
  else error('New comments require a review diff, home file row, or home header') end
  return compose(state, { target = target, destination = destination })
end

function M.enter()
  local ok, err = pcall(function()
    local state, meta = get_state()
    settle(state)
    ready(state)
    if not meta then return end
    if meta.kind == 'log' then
      if vim.fn.foldlevel('.') > 0 then vim.cmd('normal! za') end
      return
    end
    if meta.kind == 'diff' then
      if meta.source_lines == 0 then error('This comparison side has no source lines; open its comments or create a file comment from home') end
      if meta.binary then error('Binary files support file comments from their home row; line anchors are unavailable') end
      local line = api.nvim_win_get_cursor(0)[1]
      local location = range(meta, line, line)
      local roots = roots_at(state.view, location, meta)
      if #roots > 0 then thread_list(state, roots, nil, nil, location) end
      return
    end
    local object = selected(meta)
    if not object then return end
    if object.pull then navigate(state, { op = 'route', url = object.pull.url })
    elseif object.session then navigate(state, { op = 'select', batch = object.session })
    elseif object.checkout then checkout_current(state)
    elseif object.pr_details then compose(state, { kind = 'pr' })
    elseif object.publication then publication(state, object.publication)
    elseif object.file then open_file(state, object.file)
    elseif object.conclusion then dispatch('conclude')
    elseif object.comment then
      local row = find(state.view, object.comment)
      if not row then return end
      if object.conflict or row.conflict then show_conflict(state, row)
      elseif object.draft then edit_comment(state, row)
      elseif meta.kind == 'thread' then
        if owned(state, row) then edit_comment(state, row) end
      elseif meta.kind == 'threads' then thread(state, row)
      else open_comment(state, object.comment) end
    end
  end)
  if not ok then notify(err, vim.log.levels.ERROR) end
end

publication = function(state, id)
  guard(state)
  local opening = {}
  state.repository.opening = opening
  local selection = state.repository.current
  local navigation = state.repository.navigation
  request(state, 'history', { publication = id }, function(response, err)
    if state.repository.opening ~= opening then return end
    if err then notify(err, vim.log.levels.ERROR); return end
    if state.repository.current ~= selection or state.repository.navigation ~= navigation or protected(state.repository.current) then return end
    local history = response.publication
    local lines = {
      publication_text(history.summary),
      'Planned: ' .. counts_text(history.summary.planned),
      'Published: ' .. counts_text(history.summary.published),
    }
    if history.error or history.summary.error then
        vim.list_extend(lines, vim.split(('Error: ' .. (history.error or history.summary.error)):gsub('\r\n', '\n'), '\n', { plain = true }))
    end
    for _, item in ipairs(history.operations) do
      local change = item.operation.change
      lines[#lines + 1] = ''
      lines[#lines + 1] = '---'
      lines[#lines + 1] = ''
      local title = ({ create_comment = 'New comment', edit_comment = 'Edit comment', delete_comment = 'Delete comment', set_file_viewed = 'File progress', set_review_conclusion = 'Review conclusion', set_comment_reaction = 'Comment reaction', dismiss_review = 'Dismiss review' })[change.kind] or change.kind:gsub('_', ' ')
      lines[#lines + 1] = title .. (item.published and ' · published' or ' · not published')
      local comment = type(change.comment) == 'table' and change.comment or nil
      if comment then
        local target = comment.target
        if target.kind == 'diff' then lines[#lines + 1] = 'Path: ' .. target.target.path:gsub('[\r\n]', ' ')
        elseif target.kind == 'reply' then lines[#lines + 1] = 'Reply to: ' .. target.target end
      elseif change.comment then lines[#lines + 1] = 'Comment: ' .. change.comment end
      if change.path then lines[#lines + 1] = change.path .. (change.viewed and ' · viewed' or ' · unviewed') end
      if change.reaction then lines[#lines + 1] = change.reaction .. (change.present and ' · added' or ' · removed') end
      if change.conclusion then lines[#lines + 1] = 'Outcome: ' .. change.conclusion.outcome:gsub('_', ' ') end
      local body = comment and comment.body or change.body or (change.conclusion and change.conclusion.body) or change.message
      if body then lines[#lines + 1] = ''; vim.list_extend(lines, vim.split(body, '\n', { plain = true })) end
      if item.response then
        lines[#lines + 1] = ''
        lines[#lines + 1] = 'Provider response:'
        lines[#lines + 1] = '```json'
        lines[#lines + 1] = vim.json.encode(item.response)
        lines[#lines + 1] = '```'
      end
    end
    local buf = scratch(state, 'history', id, lines)
    vim.bo[buf].filetype = 'markdown'
    discussion(state, buf)
  end, false)
end

function M.progress(present, visual, mode, selection, callback)
  local ok, err = pcall(function()
    local state, meta = get_state()
    if selection and selection.state then state = selection.state end
    ready(state)
    local view = require_view(state)
    local objects, seen = {}, {}
    if selection and selection.comment then objects = { { comment = selection.comment } }
    elseif selection and selection.file then objects = { { file = selection.file } }
    elseif meta and meta.kind == 'diff' then
      if meta.historical then error('Historical code cannot mark the current file viewed; mark its comment in the thread') end
      objects = { { file = meta.path } }
    elseif visual or (selection and selection.range > 0) then
      local first = visual and math.min(vim.fn.getpos('v')[2], api.nvim_win_get_cursor(0)[1]) or selection.line1
      local last = visual and math.max(vim.fn.getpos('v')[2], api.nvim_win_get_cursor(0)[1]) or selection.line2
      for line = first, last do
        local object = meta and meta.rows[line]
        if object then objects[#objects + 1] = object end
      end
      if visual then api.nvim_cmd({ cmd = 'normal', bang = true, args = { api.nvim_replace_termcodes('<Esc>', true, false, true) } }, {}) end
    else
      local object = selected(meta)
      if object then objects = { object } end
    end
    objects = vim.tbl_filter(function(object) return object.comment or object.file end, objects)
    if #objects == 0 then
      if mode then error('Select a fetched comment or compared file') end
      return
    end
    local queue, undo = {}, {}
    for _, object in ipairs(objects) do
      if seen[object.comment or object.file] then
        -- Several selected body lines can belong to the same comment.
      elseif object.comment and mode ~= 'file' then
        -- Contextual u reverses local work; explicit Todo remains progress-only.
        if not present and not mode then
          local row = find(view, object.comment)
          if row and (row.pending_delete or unpublished(row)) then guard(state) end
          undo[#undo + 1] = object.comment
        else
          queue[#queue + 1] = { 'reaction', { comment = object.comment, present = present } }
          for index, id in ipairs(view.todo) do
            if id == object.comment then
              state.continuation = {
                id = id, index = index, order = vim.deepcopy(view.todo),
                position = { window = api.nvim_get_current_win(), buffer = api.nvim_get_current_buf(), cursor = api.nvim_win_get_cursor(0) },
              }
            end
          end
        end
        seen[object.comment] = true
        state.selected = object.comment
        state.selected_file = nil
      elseif object.file and mode ~= 'comment' then
        seen[object.file] = true
        state.selected_file = object.file
        state.selected = nil
        queue[#queue + 1] = { 'viewed', { path = object.file, viewed = present } }
      else error(mode == 'file' and 'Select a file or review diff' or 'Select a fetched comment') end
    end
    if #undo > 0 then table.insert(queue, 1, { 'undo', { comments = undo } }) end
    actions(state, queue, callback)
  end)
  if not ok then notify(err, vim.log.levels.ERROR) end
end

local function visible_target(state, kind)
  local pane = state.repository['active_' .. kind]
  if not pane then return end
  if kind == 'file' and not vim.deep_equal(pane.revision, pane.state.view.comparison) then return end
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    local buf = api.nvim_win_get_buf(win)
    if kind == 'comment' then
      if buf == pane.buffer then return pane end
    else
      local meta = buffers[buf]
      if meta and meta.kind == 'diff' and meta.state == pane.state and meta.path == pane.id
        and vim.deep_equal(meta.revision, pane.revision) then return pane end
    end
  end
end

function M.mark_move(kind, direction)
  local ok, err = pcall(function()
    local state, meta = get_state()
    guard(state)
    local pane = visible_target(state, kind)
    if pane then state = pane.state end
    ready(state)
    guard(state)
    local view = require_view(state)
    if kind == 'comment' and not pane then
      state.repository.opening = {}
      settle(state)
      view = require_view(state)
      local roots = {}
      if meta and meta.kind == 'diff' then
        local line = api.nvim_win_get_cursor(0)[1]
        for _, root in ipairs(roots_at(view, range(meta, line, line), meta)) do
          roots[content(root).id] = true
        end
      elseif meta and meta.kind == 'threads' then
        for _, id in ipairs(meta.root_ids) do roots[id] = true end
      end
      if next(roots) then
        for at = direction > 0 and 1 or #view.todo, direction > 0 and #view.todo or 1, direction do
          local id = view.todo[at]
          local root = root_row(view, find(view, id))
          if root and roots[content(root).id] then
            open_comment(state, id)
            return
          end
        end
      end
      navigate_items(state, meta, direction, 'todo')
      return
    end
    local object = selected(meta)
    local id = pane and pane.id or (kind == 'file' and ((object and object.file) or (meta and meta.kind == 'diff' and meta.path) or state.path))
    if kind == 'file' and not pane
      and not vim.tbl_contains(view.files, function(file) return file.path == id end, { predicate = true }) then id = nil end
    if not id and kind == 'comment' then
      navigate_items(state, nil, direction, 'todo')
      return
    end
    local repository, navigation = state.repository.current, state.repository.navigation
    local opening = {}
    state.repository.opening = opening
    local comparison = vim.deepcopy(view.comparison)
    local function advance(_, problem, current, stationary)
      if problem then notify(problem, vim.log.levels.ERROR) end
      if not stationary or state.repository.opening ~= opening or state.repository.current ~= repository or state.repository.navigation ~= navigation
        or not vim.deep_equal(current.view.comparison, comparison) or protected(state) then return end
      if pane then
        local present = visible_target(state, kind)
        if not present or present.state ~= pane.state or present.buffer ~= pane.buffer or present.id ~= id then return end
      end
      local items = kind == 'file' and current.view.files or current.view.comments
      local index
      for at, item in ipairs(items) do
        if (kind == 'file' and item.path or content(item).id) == id then index = at; break end
      end
      if not id and kind == 'file' then index = direction > 0 and 0 or #items + 1 end
      if index then
        for at = index + direction, direction > 0 and #items or 1, direction do
          local item = items[at]
          if kind == 'file' then
            if not item.viewed then open_file(state, item.path); return end
          elseif vim.tbl_contains(current.view.todo, content(item).id) then
            open_comment(state, content(item).id)
            return
          end
        end
      end
      notify('End of ' .. (kind == 'file' and 'unviewed file' or 'todo') .. ' list')
    end
    local row = kind == 'comment' and find(view, id)
    if kind == 'file' then
      for _, file in ipairs(view.files) do if file.path == id then row = file; break end end
    end
    if not pane or (kind == 'file' and (not row or row.viewed))
      or (kind == 'comment' and (not row or not row.fetched or row.done or row.pending_delete)) then
      advance(nil, nil, state, true)
    else
      M.progress(true, false, kind, { [kind] = id, state = pane.state }, advance)
    end
  end)
  if not ok then notify(err, vim.log.levels.ERROR) end
end

show_conflict = function(state, row)
  guard(state)
  local id = content(row).id
  local conflict
  for _, candidate in ipairs(state.view.conflicts or {}) do if candidate.comment == id then conflict = candidate; break end end
  if not conflict then error('No conflict for selected comment') end
  local lines = { 'Conflict: ' .. id, ':Review resolve local|remote|edit', '', '## Local', content(row).body, '', '## Remote', conflict.remote and conflict.remote.body or '(deleted remotely)', '', '## Previously observed', conflict.previous.body }
  local buf, meta = scratch(state, 'conflict', id, vim.split(table.concat(lines, '\n'), '\n', { plain = true }))
  vim.bo[buf].filetype = 'markdown'
  for line = 1, api.nvim_buf_line_count(buf) do meta.rows[line] = { comment = id, conflict = true } end
  discussion(state, buf)
end

navigate_items = function(state, meta, direction, kind)
  state.repository.opening = {}
  settle(state)
  local view = require_view(state)
  local ids = {}
  if kind == 'todo' then ids = view.todo
  elseif kind == 'comment' then for _, row in ipairs(view.comments) do ids[#ids + 1] = content(row).id end
  elseif kind == 'file' then
    for _, viewed in ipairs({ false, true }) do for _, file in ipairs(view.files) do if file.viewed == viewed then ids[#ids + 1] = file.path end end end
  else error('Navigation expects todo, comment, or file') end
  if #ids == 0 then notify('No ' .. kind .. ' items'); return end
  local object = selected(meta)
  local current = kind == 'file' and ((object and object.file) or (meta and meta.path)) or ((object and object.comment) or state.selected)
  if kind == 'todo' and at_continuation(state, api.nvim_get_current_win()) then current = state.continuation.id end
  local index
  for at, id in ipairs(ids) do if id == current then index = at + direction; break end end
  if not index and kind == 'todo' and state.continuation then
    local ranks, selected_rank = {}, nil
    for at, row in ipairs(view.comments) do
      ranks[content(row).id] = at
      if content(row).id == state.continuation.id then selected_rank = at end
    end
    if selected_rank then
      if direction > 0 then
        for at, id in ipairs(ids) do if ranks[id] > selected_rank then index = at; break end end
        index = index or #ids + 1
      else
        for at = #ids, 1, -1 do if ranks[ids[at]] < selected_rank then index = at; break end end
        index = index or 0
      end
    end
  end
  if not index and kind == 'todo' and state.continuation then
    local old = state.continuation.order
    local old_index = state.continuation.index
    local candidates = {}
    for at, id in ipairs(old) do candidates[id] = at end
    if direction > 0 then
      for at, id in ipairs(ids) do if (candidates[id] or math.huge) > old_index then index = at; break end end
      index = index or #ids + 1
    else
      for at = #ids, 1, -1 do if (candidates[ids[at]] or math.huge) < old_index then index = at; break end end
      index = index or 0
    end
  end
  index = index or (direction > 0 and 1 or #ids)
  if index < 1 or index > #ids then notify('End of ' .. kind .. ' list'); return end
  if kind == 'file' then open_file(state, ids[index]) else open_comment(state, ids[index]) end
end

local function conclude(state)
  state.repository.opening = {}
  settle(state)
  guard(state)
  local selection = state.repository.current
  local view = require_view(state)
  local outcome = view.wip and view.wip.conclusion and view.wip.conclusion.outcome
  local choices = { 'comment', 'approve', 'request_changes' }
  if outcome then
    choices = vim.tbl_filter(function(choice) return choice ~= outcome end, choices)
    table.insert(choices, 1, outcome)
  end
  vim.ui.select(choices, { prompt = 'Review outcome', format_item = function(value) return ({ comment = 'Comment', approve = 'Approve', request_changes = 'Request changes' })[value] end }, function(choice)
    if not choice then return end
    if state.repository.current ~= selection then return end
    if protected(state) then notify('Unsaved composer; conclusion cancelled', vim.log.levels.ERROR); return end
    request(state, 'start', {}, function(_, err)
      if err then notify(err, vim.log.levels.ERROR); return end
      if state.repository.current ~= selection then return end
      local wip = state.view.wip
      local count = 0
      for _, row in ipairs(state.view.comments) do if row.destination == 'review' and row.comment.origin == 'draft' then count = count + 1 end end
      compose(state, { kind = 'conclusion', outcome = choice, body = wip and wip.conclusion and wip.conclusion.body or '', message = ('Conclusion: %s | %d saved comments'):format(choice, count) })
    end)
  end)
end

local function suggestion()
  local buf = api.nvim_get_current_buf()
  local edit = editor.get(buf)
  if not edit or edit.kind ~= 'comment' or not edit.target or edit.target.kind ~= 'diff' then error('Suggestions require a line/range-anchored root comment composer') end
  local location = edit.relocation or edit.target.target
  if not location.range then error('Suggestions require a line/range-anchored root comment composer') end
  local state = edit.state
  local source
  for _, win in ipairs({ state.left, state.right }) do
    if valid(win) then
      local source_buf = api.nvim_win_get_buf(win)
      local meta = buffers[source_buf]
      if meta and meta.path == location.path and meta.side == location.range.start.side and vim.deep_equal(meta.revision, location.revision) then source = source_buf; break end
    end
  end
  if not source then error('Suggestion source unavailable for this exact anchor; open its immutable code first') end
  local last = location.range['end'].line
  if last > api.nvim_buf_line_count(source) then error('Suggestion source lines unavailable') end
  local lines = api.nvim_buf_get_lines(source, location.range.start.line - 1, last, false)
  table.insert(lines, 1, '```suggestion')
  lines[#lines + 1] = '```'
  local cursor = api.nvim_win_get_cursor(0)
  api.nvim_buf_set_lines(buf, cursor[1] - 1, cursor[1] - 1, false, lines)
end

local function context(state, row)
  guard(state)
  local opening = {}
  state.repository.opening = opening
  local selection = state.repository.current
  local navigation = state.repository.navigation
  request(state, 'context', { comment = content(row).id }, function(response, err)
    if state.repository.opening ~= opening then return end
    if err then notify(err, vim.log.levels.ERROR); return end
    if state.repository.current ~= selection or state.repository.navigation ~= navigation or protected(state.repository.current) then return end
    local ctx = response.context
    if state.context_tab and api.nvim_tabpage_is_valid(state.context_tab) then
      api.nvim_set_current_tabpage(state.context_tab)
      if not valid(state.context_left) then
        vim.cmd('keepjumps leftabove split')
        state.context_left = api.nvim_get_current_win()
      end
    else
      vim.cmd('keepjumps tabnew')
      state.context_tab, state.context_left = api.nvim_get_current_tabpage(), api.nvim_get_current_win()
      state.context_right = nil
    end
    leave_diff(state.context_left)
    if valid(state.context_right) then leave_diff(state.context_right) end
    if not (ctx.base_content ~= nil and ctx.head_content ~= nil) and valid(state.context_right) then
      api.nvim_win_close(state.context_right, false)
      state.context_right = nil
    end
    api.nvim_set_current_win(state.context_left)
    local title = 'Original context: ' .. content(row).id
    local lines = { title, 'Location unavailable', ctx.message or '', '', ctx.diff_hunk or ctx.hunk or '', '', content(row).body }
    local function source(buf, side, commit)
      local meta = buffers[buf]
      for at = 1, api.nvim_buf_line_count(buf) do meta.rows[at] = { comment = content(row).id } end
      local location = ctx.anchor
      local path = location and location.path or ''
      local ft = vim.filetype.match({ filename = path })
      if ft then vim.bo[buf].filetype = ft end
      vim.wo.winbar = ('Original %s %s  %s'):format(side, commit or '', path):gsub('%%', '%%%%')
    end
    if ctx.base_content ~= nil and ctx.head_content ~= nil then
      local base = scratch(state, 'context', content(row).id, vim.split(ctx.base_content, '\n', { plain = true }), false, 'context:base')
      local head = scratch(state, 'context', content(row).id, vim.split(ctx.head_content, '\n', { plain = true }), false, 'context:head')
      show_buffer(0, base)
      local base_win = api.nvim_get_current_win()
      vim.cmd('diffthis')
      source(base, 'base', ctx.anchor and ctx.anchor.revision.base)
      if not valid(state.context_right) then
        vim.cmd('keepjumps rightbelow vsplit')
        state.context_right = api.nvim_get_current_win()
        leave_diff(state.context_right)
      else api.nvim_set_current_win(state.context_right) end
      show_buffer(0, head)
      vim.cmd('diffthis')
      source(head, 'head', ctx.anchor and ctx.anchor.revision.head)
      reveal_location(base_win, api.nvim_get_current_win(), ctx.anchor)
    elseif ctx.content ~= nil then
      lines = vim.split(ctx.content, '\n', { plain = true })
      local buf = scratch(state, 'context', content(row).id, lines)
      show_buffer(0, buf)
      local side = ctx.anchor and ctx.anchor.range and ctx.anchor.range.start.side or 'head'
      source(buf, side, ctx.anchor and ctx.anchor.revision[side])
      reveal_location(api.nvim_get_current_win(), api.nvim_get_current_win(), ctx.anchor)
    else
      local buf, meta = scratch(state, 'context', content(row).id, vim.split(table.concat(lines, '\n'), '\n', { plain = true }))
      for at = 1, api.nvim_buf_line_count(buf) do meta.rows[at] = { comment = content(row).id } end
      show_buffer(0, buf)
      vim.wo.winbar = 'Original context unavailable; recorded discussion'
    end
  end, false)
end

dispatch = function(command, arguments, opts)
  local state, meta = get_state()
  local selection = state.repository.current
  if state.repository.navigating and (not command or command == 'prs' or command == 'sessions') then return end
  ready(state)
  arguments = arguments or {}
  if not command then
    guard(state)
    if meta and meta.kind ~= 'prs' and meta.kind ~= 'sessions' and state.view then
      home(state)
      refresh(state)
    else navigate(state, { op = 'entry' }) end
    return
  end
  if command == 'prs' then
    local query = {}
    for _, argument in ipairs(arguments) do
      local key, value = argument:match('^(%w+)=(.+)$')
      if key ~= 'assignee' and key ~= 'state' then error('Expected assignee=LOGIN|@me|all or state=open|closed|merged|all') end
      if key == 'state' and not vim.tbl_contains({ 'open', 'closed', 'merged', 'all' }, value) then error('Invalid PR state: ' .. value) end
      query[key] = value
    end
    list(state, query)
    return
  end
  if command == 'pr' then
    if #arguments ~= 1 or arguments[1] == '' then error('Use :Review pr BASE') end
    navigate(state, { op = 'pr', base = arguments[1] })
    return
  end
  if command == 'sessions' then
    if #arguments > 1 or (arguments[1] and arguments[1] ~= 'new') then error('Use :Review sessions [new]') end
    local payload = { op = arguments[1] == 'new' and 'new' or 'sessions' }
    local object = selected(meta)
    if payload.op == 'new' and object and object.pull then payload.url = object.pull.url
    elseif payload.op == 'new' and object and object.pr then payload.pr = object.pr
    elseif payload.op == 'new' and meta and meta.kind == 'sessions' and meta.pr then payload.pr = meta.pr
    elseif state.view then payload.workspace = state.view.id end
    navigate(state, payload)
    return
  end
  if command == 'refresh' and meta and meta.kind == 'prs' then list(state); return end
  if command == 'refresh' and meta and meta.kind == 'sessions' then
    navigate(state, { op = 'sessions', workspace = state.view and state.view.id, pr = meta.pr })
    return
  end
  require_view(state)
  if command == 'start' or command == 'leave' then request(state, command)
  elseif command == 'publish' then publish(state, command)
  elseif command == 'checkout' then
    if #arguments > 0 then error('Use :Review checkout') end
    checkout_current(state)
  elseif command == 'refresh' then refresh(state)
  elseif command == 'conclude' then conclude(state)
  elseif command == 'comment' then M.new_comment(arguments[1], false, opts)
  elseif command == 'reply' then reply(state, picked(state, meta), arguments[1])
  elseif command == 'edit' then edit_comment(state, picked(state, meta))
  elseif command == 'delete' or command == 'restore' then
    guard(state)
    local row = picked(state, meta)
    if not owned(state, row) then error('Deletion/restoration requires verified ownership') end
    if row.summary and command == 'delete' then error('Submitted review summaries cannot be deleted') end
    state.selected = content(row).id
    state.selected_file = nil
    request(state, command, { comment = content(row).id })
  elseif command == 'done' or command == 'todo' then M.progress(command == 'done', false, 'comment', opts)
  elseif command == 'viewed' or command == 'unviewed' then M.progress(command == 'viewed', false, 'file', opts)
  elseif command == 'next' or command == 'previous' then navigate_items(state, meta, command == 'next' and 1 or -1, arguments[1] or 'todo')
  elseif command == 'suggest' then suggestion()
  elseif command == 'log' then
    guard(state)
    local opening = {}
    state.repository.opening = opening
    request(state, 'log', {}, function(response, err)
      if state.repository.opening ~= opening then return end
      if err then notify(err, vim.log.levels.ERROR); return end
      if state.repository.current ~= selection or protected(state.repository.current) then return end
      show_log(state, response.log)
    end, false)
  elseif command == 'compare' then
    if arguments[1] ~= 'latest' then error('Use :Review compare latest') end
    guard(state)
    local opening = {}
    state.repository.opening = opening
    local previous = vim.deepcopy(state.view.comparison)
    refresh_snapshot(state, true, function(response, err)
      if response then apply(state, response, 'object') end
      if err then notify(err, vim.log.levels.ERROR); return end
      if response and response.refresh_deferred then
        notify('Comparison update deferred while another operation settles; retry :Review compare latest')
        return
      end
      if state.repository.opening ~= opening or state.repository.current ~= selection or protected(state) then return end
      prepare_objects(state, nil, true)
      notify(('Comparison %s..%s → %s..%s'):format(previous.base, previous.head, state.view.comparison.base, state.view.comparison.head))
      if state.path and valid(state.left) then
        if vim.tbl_contains(state.view.files, function(file) return file.path == state.path end, { predicate = true }) then
          open_file(state, state.path)
        else home(state) end
      end
    end)
  elseif command == 'context' then context(state, picked(state, meta))
  elseif command == 'resolve' then
    local resolution = arguments[1]
    if not vim.tbl_contains({ 'local', 'remote', 'edit' }, resolution) then error('Use :Review resolve local|remote|edit') end
    local row = picked(state, meta)
    if resolution == 'edit' then
      if row.pending_delete then error('Deletion conflicts cannot resolve through body editing') end
      local conflict
      for _, candidate in ipairs(state.view.conflicts) do if candidate.comment == content(row).id then conflict = candidate end end
      if not conflict or not conflict.remote then error('Missing remote target; use remote to abandon stale intent') end
      compose(state, { row = row, resolve = true })
    else
      request(state, 'resolve', { comment = content(row).id, resolution = resolution }, function(_, err)
        if err then notify(err, vim.log.levels.ERROR); return end
        if state.repository.current ~= selection then return end
        local resolved = find(state.view, content(row).id)
        if resolved then thread(state, resolved)
        else
          local buf = scratch(state, 'thread', nil, { 'Conflict resolved; remote comment no longer exists.' })
          discussion(state, buf)
        end
      end)
    end
  elseif command == 'relocate' then
    if not meta or meta.kind ~= 'diff' then error('Select a line or range in the current review diff') end
    if meta.source_lines == 0 or meta.binary then error('Relocation requires source lines on this immutable comparison side') end
    guard(state)
    local first, last = api.nvim_win_get_cursor(0)[1], api.nvim_win_get_cursor(0)[1]
    if opts and opts.range > 0 then
      if vim.fn.visualmode() == '\022' then error('Blockwise selections cannot anchor relocation; use characterwise or linewise selection') end
      first, last = opts.line1, opts.line2
    end
    local location = range(meta, first, last)
    local candidates = {}
    for _, row in ipairs(state.view.comments) do if unpublished(row) and content(row).target.kind == 'diff' then candidates[#candidates + 1] = row end end
    local function choose(row)
      if not row then return end
      compose(state, { row = row, relocation = location, message = 'Relocate: ' .. location_text(content(row).target.target) .. ' → ' .. location_text(location) .. ' (:w saves)' })
    end
    if arguments[1] then
      local row = find(state.view, arguments[1])
      if not row or not unpublished(row) or content(row).target.kind ~= 'diff' then error('Relocation requires an unpublished inline/file root ID') end
      choose(row)
    else vim.ui.select(candidates, { prompt = 'Relocate draft', format_item = function(row) return content(row).id .. ' ' .. preview(content(row).body) end }, choose) end
  else error('Unknown Review command: ' .. command) end
end

local function complete(lead, line)
  local parts = vim.split(line, '%s+', { trimempty = true })
  local candidates = commands
  if #parts > 2 or (#parts == 2 and lead == '') then
    local command = parts[2]
    candidates = ({ prs = { 'assignee=', 'assignee=@me', 'assignee=all', 'state=open', 'state=closed', 'state=merged', 'state=all' }, sessions = { 'new' }, comment = { 'review', 'standalone' }, reply = { 'review', 'standalone' }, next = { 'todo', 'comment', 'file' }, previous = { 'todo', 'comment', 'file' }, compare = { 'latest' }, resolve = { 'local', 'remote', 'edit' } })[command] or {}
    if command == 'relocate' then
      local ok, state = pcall(get_state)
      if ok and state.view then for _, row in ipairs(state.view.comments) do if unpublished(row) and content(row).target.kind == 'diff' then candidates[#candidates + 1] = content(row).id end end end
    end
  end
  return vim.tbl_filter(function(value) return value:sub(1, #lead) == lead end, candidates)
end

function M.register()
  api.nvim_create_autocmd('FocusGained', { callback = function()
    local meta = buffers[api.nvim_get_current_buf()]
    if meta and meta.kind == 'home' then refresh_checkout(meta.state) end
  end })
  api.nvim_create_autocmd('SafeState', { callback = function()
    for _, repository in pairs(repositories) do
      for _, state in pairs(repository.sessions) do if state.writer then state.writer.flush(true) end end
    end
    render_pending(true)
    if range_pending then highlight_comment(true) end
  end })
  api.nvim_create_autocmd('VimLeavePre', { callback = function()
    for _, repository in pairs(repositories) do
      for _, state in pairs(repository.sessions) do
        local ok, err = pcall(settle, state)
        if not ok then notify(err, vim.log.levels.ERROR) end
      end
    end
  end })
  api.nvim_create_autocmd({ 'BufWinEnter', 'BufWinLeave', 'WinNew', 'WinClosed', 'TabEnter' }, { callback = function() highlight_comment() end })
  api.nvim_create_user_command('Review', function(opts)
    local arguments = vim.deepcopy(opts.fargs)
    local command = table.remove(arguments, 1)
    local ok, err = pcall(dispatch, command, arguments, opts)
    if not ok then notify(err, vim.log.levels.ERROR) end
  end, { nargs = '*', range = true, complete = complete, desc = 'Review and address PR feedback' })
  local mappings = api.nvim_get_keymap('n')
  for _, binding in ipairs({
    { '<leader>rs', 'Review' }, { '<leader>rr', 'Review refresh' },
    { '<leader>rp', 'Review publish' }, { '<leader>rcc', 'Review conclude' },
    { '<leader>rcs', 'Review suggest' },
  }) do
    local lhs = api.nvim_replace_termcodes(binding[1], true, false, true)
    if not vim.tbl_contains(mappings, function(mapping) return mapping.lhsraw == lhs end, { predicate = true }) then
      vim.keymap.set('n', binding[1], '<Cmd>' .. binding[2] .. '<CR>', { silent = true })
    end
  end
  for _, item in ipairs({
    { 'Done', 'done' }, { 'Todo', 'todo' }, { 'Viewed', 'viewed' }, { 'Unviewed', 'unviewed' },
    { 'NextTodo', 'next todo' }, { 'PreviousTodo', 'previous todo' },
    { 'NextComment', 'next comment' }, { 'PreviousComment', 'previous comment' },
    { 'NextFile', 'next file' }, { 'PreviousFile', 'previous file' },
  }) do
    vim.keymap.set('n', '<Plug>(Review' .. item[1] .. ')', '<Cmd>Review ' .. item[2] .. '<CR>', { silent = true })
  end
  for _, binding in ipairs(mark_move_bindings) do
    vim.keymap.set('n', '<Plug>(Review' .. binding[2] .. ')', function() M.mark_move(binding[3], binding[4]) end, { silent = true })
  end
end

return M
