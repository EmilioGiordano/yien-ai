// A local stand-in for an OpenAI-compatible chat completions API, so
// OpenCode can run in a YIEN test without an account or quota.
//
// Usage: node mock-openai-api.js --port <n> [--log <file>]
//   --port  loopback port to listen on (0 picks a free one)
//   --log   append one line per request (method, path, slow or not); never
//           the prompt itself
// Prints "listening <port>" on stdout once ready.
//
// A turn whose last user message contains "slow" streams for about 40 s;
// anything else answers at once. A turn whose last user message contains
// "mockerror" answers with an API error after a few seconds. A turn whose
// last user message contains "mocktool" answers with a bash tool call
// instead, so the permission flow can run; the turn after the tool result
// answers with plain text. Point an OpenCode provider at
// http://127.0.0.1:<port>/v1 with any bearer token; auth is not checked.

'use strict';

const http = require('http');
const fs = require('fs');

const SLOW_CHUNKS = 80;
const SLOW_GAP_MS = 500;
const FAST_CHUNKS = 2;
const FAST_GAP_MS = 10;

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
        .filter((part) => part && typeof part.text === 'string')
        .map((part) => part.text)
        .join(' ');
    }
  }
  return '';
}

// A tool result comes back as a role=tool message; a turn after one is the
// wrap-up, never a new tool call or failure.
function hasToolResult(body) {
  const messages = Array.isArray(body.messages) ? body.messages : [];
  return messages.some((message) => message && message.role === 'tool');
}

const ERROR_DELAY_MS = 8000;

function sendJson(res, status, value) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(value));
}

function chunk(id, model, delta, finishReason) {
  return {
    id,
    object: 'chat.completion.chunk',
    created: Math.floor(Date.now() / 1000),
    model,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
  };
}

function answer(req, res, raw) {
  const path = req.url || '';
  if (req.method !== 'POST' || path !== '/v1/chat/completions') {
    note(`${req.method} ${path} 404`);
    sendJson(res, 404, { error: { message: 'mock', type: 'invalid_request_error' } });
    return;
  }
  let body = {};
  try {
    body = JSON.parse(raw);
  } catch (_) {
    body = {};
  }
  const slow = /slow/.test(lastUserText(body)) && !hasToolResult(body);
  const fail = /mockerror/.test(lastUserText(body)) && !hasToolResult(body);
  const tool = /mocktool/.test(lastUserText(body)) && !hasToolResult(body);
  note(`chat stream=${Boolean(body.stream)} slow=${slow} fail=${fail} tool=${tool}`);
  const model = body.model || 'mock';
  const id = `chatcmpl-mock-${Date.now()}`;
  if (fail) {
    // A non-retryable error, like an invalid key: OpenCode gives up at
    // once and fails the turn. The wait lets a scenario watch it land.
    let closed = false;
    res.on('close', () => {
      closed = true;
    });
    res.on('error', () => {});
    setTimeout(() => {
      if (closed || res.writableEnded) return;
      sendJson(res, 400, {
        error: { message: 'mock failure', type: 'invalid_request_error' },
      });
    }, ERROR_DELAY_MS);
    return;
  }
  // Not on the safe-command list, so the CLI asks first. Port 9 discards;
  // the call fails fast and nothing reaches the network.
  const toolArguments = JSON.stringify({ command: 'curl -s http://127.0.0.1:9/yien-permission-probe' });
  if (tool) {
    // A bash tool call, so OpenCode wants to run it and asks for approval.
    if (!body.stream) {
      sendJson(res, 200, {
        id,
        object: 'chat.completion',
        created: Math.floor(Date.now() / 1000),
        model,
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              tool_calls: [
                {
                  id: 'call_mock',
                  type: 'function',
                  function: { name: 'bash', arguments: toolArguments },
                },
              ],
            },
            finish_reason: 'tool_calls',
          },
        ],
        usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 },
      });
      return;
    }
    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
      connection: 'keep-alive',
    });
    const send = (value) => res.write(`data: ${JSON.stringify(value)}\n\n`);
    send(
      chunk(
        id,
        model,
        {
          role: 'assistant',
          tool_calls: [
            {
              index: 0,
              id: 'call_mock',
              type: 'function',
              function: { name: 'bash', arguments: '' },
            },
          ],
        },
        null
      )
    );
    send(
      chunk(
        id,
        model,
        { tool_calls: [{ index: 0, function: { arguments: toolArguments } }] },
        null
      )
    );
    send(chunk(id, model, {}, 'tool_calls'));
    res.write('data: [DONE]\n\n');
    res.end();
    return;
  }
  if (!body.stream) {
    sendJson(res, 200, {
      id,
      object: 'chat.completion',
      created: Math.floor(Date.now() / 1000),
      model,
      choices: [
        {
          index: 0,
          message: { role: 'assistant', content: 'ok' },
          finish_reason: 'stop',
        },
      ],
      usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 },
    });
    return;
  }
  res.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'keep-alive',
  });
  const send = (value) => res.write(`data: ${JSON.stringify(value)}\n\n`);
  send(chunk(id, model, { role: 'assistant', content: '' }, null));
  let left = slow ? SLOW_CHUNKS : FAST_CHUNKS;
  let closed = false;
  res.on('close', () => {
    if (!res.writableFinished) note('client closed the stream');
    closed = true;
  });
  const tick = () => {
    if (closed) return;
    if (left-- > 0) {
      send(chunk(id, model, { content: 'word ' }, null));
      setTimeout(tick, slow ? SLOW_GAP_MS : FAST_GAP_MS);
      return;
    }
    send(chunk(id, model, {}, 'stop'));
    res.write('data: [DONE]\n\n');
    res.end();
  };
  tick();
}

const server = http.createServer((req, res) => {
  let raw = '';
  req.setEncoding('utf8');
  req.on('data', (chunkOfBytes) => {
    raw += chunkOfBytes;
  });
  req.on('end', () => answer(req, res, raw));
});

server.listen(port, '127.0.0.1', () => {
  const bound = server.address().port;
  note(`listening ${bound}`);
  process.stdout.write(`listening ${bound}\n`);
});
