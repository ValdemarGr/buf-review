-- Queue contracts independent of rendering, Rust and provider fixtures.
vim.opt.runtimepath:append(vim.fn.getcwd())
local actions = require('buf_review.actions')
local notices = {}
vim.notify = function(message) notices[#notices + 1] = tostring(message) end
local function entry(id) return { action = 'reaction', comment = id, present = true } end
local function fake()
  local sent = {}
  local queue = actions.new(function(command, done)
    sent[#sent + 1] = { command = command, done = done }
  end)
  return queue, sent
end
local function noop() end

-- Adjacent writes share one process, preserving group and operation order.
do
  local queue, sent = fake()
  local received = {}
  queue.enqueue({ entry('one'), entry('two') }, function(_, err) received[#received + 1] = { id = 'first', err = err } end)
  queue.enqueue({ entry('three') }, function(_, err) received[#received + 1] = { id = 'second', err = err } end)
  queue.flush(true)
  assert(#sent == 1 and sent[1].command.op == 'actions', 'contiguous writes did not form one batch')
  local entries = sent[1].command.actions
  assert(#entries == 3 and entries[1].comment == 'one' and entries[2].comment == 'two' and entries[3].comment == 'three', 'batch changed insertion order')
  sent[1].done({ workspace = { generation = 3 } })
  assert(#received == 2 and received[1].id == 'first' and received[2].id == 'second' and not received[1].err and not received[2].err, 'batch changed callback order')
  queue.wait()
end

-- A metadata/provider command is an ordered boundary, never merged with writes
-- on either side. Writes added during an inflight process stay behind it.
do
  local queue, sent = fake()
  queue.enqueue({ entry('before') }, noop)
  queue.enqueue_request({ op = 'log' }, noop)
  queue.enqueue({ entry('after') }, noop)
  queue.flush(true)
  assert(#sent == 1 and #sent[1].command.actions == 1 and sent[1].command.actions[1].comment == 'before', 'command boundary merged following writes')
  queue.enqueue({ entry('inflight') }, noop)
  queue.flush(true)
  assert(#sent == 1, 'inflight process allowed concurrent dispatch')
  sent[1].done({ workspace = { generation = 1 } })
  queue.flush(true)
  assert(#sent == 2 and sent[2].command.op == 'log', 'metadata command moved across writes')
  sent[2].done({ entries = {} })
  queue.flush(true)
  assert(#sent == 3 and #sent[3].command.actions == 2 and sent[3].command.actions[1].comment == 'after' and sent[3].command.actions[2].comment == 'inflight', 'writes after boundary lost order')
  sent[3].done({ workspace = { generation = 3 } })
  queue.wait()
end

-- The sender obtains the next generation after the previous callback applies
-- its projection, including a write enqueued reentrantly from that callback.
do
  local generation, sent, queue = 7, {}, nil
  queue = actions.new(function(command, done)
    sent[#sent + 1] = { command = command, done = done, generation = generation }
  end)
  queue.enqueue({ entry('first') }, function(response)
    generation = response.workspace.generation
    queue.enqueue({ entry('reentrant') }, function(latest) generation = latest.workspace.generation end)
  end)
  queue.flush(true)
  queue.enqueue_request({ op = 'get' }, function(response) generation = response.workspace.generation end)
  assert(#sent == 1 and sent[1].generation == 7, 'first dispatch used wrong generation')
  sent[1].done({ workspace = { generation = 8 } })
  queue.flush(true)
  assert(#sent == 2 and sent[2].command.op == 'get' and sent[2].generation == 8, 'next process preceded projection callback')
  sent[2].done({ workspace = { generation = 9 } })
  queue.flush(true)
  assert(#sent == 3 and sent[3].command.actions[1].comment == 'reentrant' and sent[3].generation == 9, 'callback write ignored earlier command projection')
  sent[3].done({ workspace = { generation = 10 } })
  queue.wait()
  assert(generation == 10, 'last projection was not applied')
end

-- Failure detaches the queued suffix before delivery. A failed callback may
-- enter a completion barrier without sending actions already canceled by it.
do
  local queue, sent = fake()
  local events = {}
  queue.enqueue({ entry('failing') }, function(_, err)
    assert(err == 'Fixture action failed', 'failed action lost error')
    queue.wait()
    events[#events + 1] = 'failed callback completed'
  end)
  queue.enqueue_request({ op = 'publish' }, function(_, err, report)
    assert(err == 'Earlier review action failed; remaining actions cancelled' and report == false, 'canceled suffix lost error/report contract')
    events[#events + 1] = 'suffix canceled'
  end)
  queue.enqueue({ entry('must-not-send') }, function(_, err)
    assert(err == 'Earlier review action failed; remaining actions cancelled', 'queued write survived earlier failure')
    events[#events + 1] = 'write canceled'
  end)
  vim.defer_fn(function()
    assert(#sent == 1, 'failed waiter dispatched suffix before callback')
    sent[1].done({ workspace = { generation = 0 }, failed_index = 0 }, 'Fixture action failed')
  end, 10)
  local ok, failure = pcall(queue.wait)
  assert(not ok and failure:find('Fixture action failed', 1, true), 'completion barrier swallowed earlier write failure')
  queue.flush(true)
  assert(#sent == 1 and vim.deep_equal(events, { 'failed callback completed', 'suffix canceled', 'write canceled' }), 'failure did not detach suffix before reentrant barrier')
end

-- A callback exception is isolated: remaining groups and the outer completion
-- barrier must finish, rather than leaving the writer permanently running.
do
  local queue, sent = fake()
  local finished, notice_before = false, #notices
  queue.enqueue({ entry('throws') }, function() error('Fixture receive exploded') end)
  queue.enqueue({ entry('still-delivered') }, function(_, err) assert(not err); finished = true end)
  vim.defer_fn(function() sent[1].done({ workspace = { generation = 2 } }) end, 10)
  queue.wait()
  assert(finished and #notices == notice_before + 1 and notices[#notices]:find('Fixture receive exploded', 1, true), 'callback exception stranded following group or waiter')
end

-- A failed metadata refresh with an updated workspace is not a failed local
-- mutation. Keep its error visible while allowing later writes to proceed.
do
  local queue, sent = fake()
  local command_error, action_done = nil, false
  queue.enqueue_request({ op = 'refresh' }, function(_, err) command_error = err end)
  queue.enqueue({ entry('after-refresh') }, function(_, err) assert(not err); action_done = true end)
  queue.flush(true)
  sent[1].done({ workspace = { generation = 4 } }, 'Fixture provider unavailable')
  queue.flush(true)
  assert(command_error == 'Fixture provider unavailable' and #sent == 2 and sent[2].command.actions[1].comment == 'after-refresh', 'metadata error canceled local intent')
  sent[2].done({ workspace = { generation = 5 } })
  queue.wait()
  assert(action_done, 'write after metadata error was not delivered')
end

-- Partial application assigns errors per group, using the provider's zero-based
-- failed_index: completed groups succeed; first failed group alone reports.
do
  local queue, sent = fake()
  local results = {}
  local function receive(id)
    return function(_, err, report) results[#results + 1] = { id = id, err = err, report = report } end
  end
  queue.enqueue({ entry('completed-one'), entry('completed-two') }, receive('completed'))
  queue.enqueue({ entry('partial-completed'), entry('failed') }, receive('partial'))
  queue.enqueue({ entry('later-group') }, receive('later'))
  queue.flush(true)
  sent[1].done({ workspace = { generation = 3 }, failed_index = 3 }, 'Fixture partial failure')
  assert(#results == 3 and results[1].id == 'completed' and not results[1].err and results[1].report == false, 'completed prefix inherited failure')
  assert(results[2].id == 'partial' and results[2].err == 'Fixture partial failure' and results[2].report == true, 'first failed group lost diagnostic')
  assert(results[3].id == 'later' and results[3].err == 'Fixture partial failure' and results[3].report == false, 'later failed group duplicated diagnostic')
  queue.wait()
end

assert(#notices == 1, 'unexpected queue callback exception: ' .. vim.inspect(notices))
print('Ordered action queue contracts passed')
vim.cmd('qa!')
