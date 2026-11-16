// A local stand-in for the Anthropic Messages API, so Claude Code can run in
// a YIEN test without an account or quota.
//
// Usage: node mock-messages-api.js --port <n> [--log <file>]
//   --port  loopback port to listen on (0 picks a free one)
//   --log   append one line per request (method, path, slow or not); never
//           the prompt itself
// Prints "listening <port>" on stdout once ready.
//
// A turn whose last user message contains "slow" streams for about 40 s;
// anything else answers at once. A turn whose last user message contains
// "mockerror" answers with an API error after a few seconds, so a scenario
// can select the other tab first and watch the pane fail while unseen. A
// turn whose last user message contains "mocktool" answers with a Bash tool
// call instead, so the permission flow can run; the turn after the tool
// result answers with plain text. Only the main turn is slowed, failed or
// given a tool: Claude Code's side requests (titles, topic checks) carry no
// tools and always answer at once. Ctrl+C in Claude closes the stream; the
// mock stops writing then.

'use strict';

const http = require('http');
const fs = require('fs');

const SLOW_CHUNKS = 80;
const SLOW_GAP_MS = 500;
const FAST_CHUNKS = 2;
const FAST_GAP_MS = 10;
// Long enough for a scenario to select another tab before the error lands.
const ERROR_DELAY_MS = 8000;

function option(name, fallback) {
  const at = process.argv.indexOf(name);
  return at >= 0 && at + 1 < process.argv.length ? process.argv[at + 1] : fallback;
}

const port = Number(option('--port', '0'));
const logFile = option('--log', null);

function note(text) {
  if (logFile) fs.appendFileSync(logFile, `${new Date().toISOString()} ${text}\n`);
}

function lastUserText(body) {
  const messages = Array.isArray(body.messages) ? body.messages : [];
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (!message || message.role !== 'user') continue;
    if (typeof message.content === 'string') return message.content;
    if (Array.isArray(message.content)) {
      return message.content
        .filter((block) => block && block.type === 'text')
        .map((block) => block.text)
        .join(' ');
    }
  }
  return '';
}

// A tool result comes back as a user message with a tool_result block; a
// turn after one is the wrap-up, never a new tool call.
function hasToolResult(body) {
  const messages = Array.isArray(body.messages) ? body.messages : [];
  return messages.some(
    (message) =>
      message &&
      message.role === 'user' &&
      Array.isArray(message.content) &&
      message.content.some((block) => block && block.type === 'tool_result')
  );
}

function sendJson(res, status, value) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
}

function answer(req, res, raw) {
  const path = req.url || '';
  if (req.method !== 'POST' || !path.startsWith('/v1/messages')) {
    note(`${req.method} ${path} 404`);
    sendJson(res, 404, { type: 'error', error: { type: 'not_found_error', message: 'mock' } });
    return;
  }
  let body = {};
  try {
    body = JSON.parse(raw);
  } catch (_) {
    body = {};
  }
  if (path.startsWith('/v1/messages/count_tokens')) {
    note('count_tokens');
    sendJson(res, 200, { input_tokens: 10 });
    return;
  }
  const tools = Array.isArray(body.tools) ? body.tools.length : 0;
  const main = tools > 0 && !hasToolResult(body);
  const slow = main && /slow/.test(lastUserText(body));
  const fail = main && /mockerror/.test(lastUserText(body));
  const tool = main && /mocktool/.test(lastUserText(body));
  note(`messages stream=${Boolean(body.stream)} tools=${tools} slow=${slow} fail=${fail} tool=${tool}`);
  if (fail) {
    // A non-retryable error, like an invalid key: Claude Code gives up at
    // once and ends the turn with StopFailure. The wait lets a scenario
    // select another tab first, so the failure lands on an unseen pane.
    let closed = false;
    res.on('close', () => {
      closed = true;
    });
    res.on('error', () => {});
    setTimeout(() => {
      if (closed || res.writableEnded) return;
      sendJson(res, 400, {
        type: 'error',
        error: { type: 'invalid_request_error', message: 'mock failure' },
      });
    }, ERROR_DELAY_MS);
    return;
  }
  const message = {
    id: 'msg_mock',
    type: 'message',
    role: 'assistant',
    model: body.model || 'mock',
    content: [],
    stop_reason: null,
    stop_sequence: null,
    usage: { input_tokens: 10, output_tokens: 1 },
  };
  // Not on the safe-command list, so the CLI asks first. Port 9 discards;
  // the call fails fast and nothing reaches the network.
  const toolInput = { command: 'curl -s http://127.0.0.1:9/yien-permission-probe' };
  if (tool) {
    // A Bash tool call, so Claude wants to run it and asks for approval.
    if (!body.stream) {
      message.content = [
        { type: 'tool_use', id: 'toolu_mock', name: 'Bash', input: toolInput },
      ];
      message.stop_reason = 'tool_use';
      sendJson(res, 200, message);
      return;
    }
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
    const event = (name, data) => res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
    event('message_start', { type: 'message_start', message: { ...message, stop_reason: null } });
    event('content_block_start', {
      type: 'content_block_start',
      index: 0,
      content_block: { type: 'tool_use', id: 'toolu_mock', name: 'Bash', input: {} },
    });
    event('content_block_delta', {
      type: 'content_block_delta',
      index: 0,
      delta: { type: 'input_json_delta', partial_json: JSON.stringify(toolInput) },
    });
    event('content_block_stop', { type: 'content_block_stop', index: 0 });
    event('message_delta', {
      type: 'message_delta',
      delta: { stop_reason: 'tool_use', stop_sequence: null },
      usage: { output_tokens: 20 },
    });
    event('message_stop', { type: 'message_stop' });
    res.end();
    return;
  }
  if (!body.stream) {
    message.content = [{ type: 'text', text: 'ok' }];
    message.stop_reason = 'end_turn';
    sendJson(res, 200, message);
    return;
  }
  res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
  const event = (name, data) => res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
  event('message_start', { type: 'message_start', message });
  event('content_block_start', {
    type: 'content_block_start',
    index: 0,
    content_block: { type: 'text', text: '' },
  });
  let left = slow ? SLOW_CHUNKS : FAST_CHUNKS;
  let closed = false;
  res.on('close', () => {
    if (!res.writableFinished) note('client closed the stream');
    closed = true;
  });
  const tick = () => {
    if (closed) return;
    if (left-- > 0) {
      event('content_block_delta', {
        type: 'content_block_delta',
        index: 0,
        delta: { type: 'text_delta', text: 'word ' },
      });
      setTimeout(tick, slow ? SLOW_GAP_MS : FAST_GAP_MS);
      return;
    }
    event('content_block_stop', { type: 'content_block_stop', index: 0 });
    event('message_delta', {
      type: 'message_delta',
      delta: { stop_reason: 'end_turn', stop_sequence: null },
      usage: { output_tokens: 20 },
    });
    event('message_stop', { type: 'message_stop' });
    res.end();
  };
  tick();
}

const server = http.createServer((req, res) => {
  let raw = '';
  req.setEncoding('utf8');
  req.on('data', (chunk) => {
    raw += chunk;
  });
  req.on('end', () => answer(req, res, raw));
});

server.listen(port, '127.0.0.1', () => {
  const bound = server.address().port;
  note(`listening ${bound}`);
  process.stdout.write(`listening ${bound}\n`);
});
