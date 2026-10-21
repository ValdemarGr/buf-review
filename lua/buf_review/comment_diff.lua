local M = {}
local api = vim.api
local namespace = api.nvim_create_namespace('buf-review-comment-diff')
local window_highlights = {}
api.nvim_set_hl(0, 'BufReviewCommentAdded', { default = true, link = 'DiffAdd' })
api.nvim_set_hl(0, 'BufReviewCommentRemoved', { default = true, link = 'DiffDelete' })
api.nvim_set_hl(0, 'BufReviewCommentUnchanged', {})

function M.window(win, active)
  if not api.nvim_win_is_valid(win) then window_highlights[win] = nil; return end
  local previous = window_highlights[win] or vim.wo[win].winhighlight
  previous = table.concat(vim.tbl_filter(function(mapping)
    return not mapping:match(':BufReviewCommentUnchanged$')
  end, vim.split(previous, ',', { trimempty = true })), ',')
  if active then
    window_highlights[win] = previous
    vim.wo[win].winhighlight = (previous ~= '' and previous .. ',' or '')
      .. 'DiffChange:BufReviewCommentUnchanged,DiffText:BufReviewCommentUnchanged'
  else
    vim.wo[win].winhighlight = previous
    window_highlights[win] = nil
  end
end

function M.sides(code)
  local original = code.original_base
  local same_original = original and original.content == code.base_content
    and (original.missing == true) == (code.base_missing == true)
  local same_head = code.base_content == code.head_content
    and (code.base_missing == true) == (code.head_missing == true)
  return original and not same_original, not same_head
end

function M.highlight(buf, code, working_text)
  api.nvim_buf_clear_namespace(buf, namespace, 0, -1)
  if not code.historical or code.binary then return end
  local function mark(hunks, offset, group, priority)
    for _, hunk in ipairs(hunks) do
      for line = hunk[offset], hunk[offset] + hunk[offset + 1] - 1 do
        if line > 0 and line <= api.nvim_buf_line_count(buf) then
          api.nvim_buf_set_extmark(buf, namespace, line - 1, 0, {
            end_row = line, end_col = 0, hl_group = group, hl_eol = true, priority = priority,
          })
        end
      end
    end
  end
  if code.original_base then
    mark(vim.diff(code.original_base.content, code.base_content, { result_type = 'indices' }), 3, 'BufReviewCommentAdded', 210)
  end
  mark(vim.diff(code.base_content, working_text or code.head_content, { result_type = 'indices' }), 1, 'BufReviewCommentRemoved', 220)
end

return M
