// Static + fake-API server for the Hermione teacher web UI.
//
// Serves crates/server/static/*.html verbatim and answers every /api/* call
// the pages make on load from fixtures.js — so the REAL shipped pages render
// with realistic data, no Rust backend or Postgres required.
//
// Env:
//   STATIC_DIR  path to crates/server/static (default: inferred from repo root)
//   FIXED_NOW   epoch ms used as "now" for the fixtures (default: a fixed date)
//   PORT        listen port (default 8799)
const http = require('http');
const fs = require('fs');
const path = require('path');
const { buildFixtures } = require('./fixtures');

// Default to the repo's static dir: this file lives at
// <repo>/.claude/skills/hermione-ui-screenshots/scripts/server.js
const REPO_ROOT = path.resolve(__dirname, '..', '..', '..', '..');
const STATIC_DIR = process.env.STATIC_DIR || path.join(REPO_ROOT, 'crates', 'server', 'static');
// A stable default date so screenshots are byte-comparable across runs.
const NOW = Number(process.env.FIXED_NOW || Date.parse('2025-07-19T13:30:00Z'));
const PORT = Number(process.env.PORT || 8799);
const F = buildFixtures(NOW);

const MIME = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png' };

// A short, believable terminal replay: a failing gcc compile (matches Ada's
// "many failing compiles" struggle reason), delivered as SSE data frames so the
// xterm.js viewer in an open pane paints something real.
function terminalReplay() {
  const B = s => Buffer.from(s).toString('base64');
  const lines = [
    "\x1b[32mada@systems-hw\x1b[0m:~/src$ make\r\n",
    "gcc -Wall -g -c list.c -o list.o\r\n",
    "\x1b[1mlist.c:\x1b[m In function \x1b[1m'list_append'\x1b[m:\r\n",
    "\x1b[1mlist.c:24:9:\x1b[m \x1b[31m\x1b[1merror:\x1b[m dereferencing NULL pointer 'head'\r\n",
    "   24 |     head->next = node;\r\n",
    "      |     \x1b[31m\x1b[1m~~~~^~~~~~\x1b[m\r\n",
    "make: *** [Makefile:8: list.o] Error 1\r\n",
    "\x1b[32mada@systems-hw\x1b[0m:~/src$ \x1b[5m█\x1b[m",
  ];
  return lines.map(l => `data: ${JSON.stringify({ stream: 'stdout', data: B(l) })}\n\n`).join('');
}

function sendJson(res, obj) {
  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(obj));
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;

  // ---- fake API (every endpoint the pages fetch) ----
  if (p === '/api/courses') {
    // ?archived=1 lists archived courses (none in the fixtures).
    const archived = /[?&]archived=(1|true|yes)/.test(url.search);
    return sendJson(res, archived ? [] : F.courses);
  }
  // GET /api/courses/{slug} → detail for the settings panel; writes 204 otherwise.
  if (/^\/api\/courses\/[^/]+$/.test(p)) {
    if (req.method === 'GET') return sendJson(res, F.courseDetail);
    res.writeHead(204); return res.end();
  }
  // Rotate-token mirrors production: 200 + a fresh token (the UI reads it back).
  if (/^\/api\/courses\/[^/]+\/rotate-token$/.test(p)) {
    return sendJson(res, { enrollmentToken: 'enroll-rotated-3f21b8d0c95e4a17' });
  }
  if (p.startsWith('/api/courses/')) { res.writeHead(204); return res.end(); }
  if (p === '/api/overview') return sendJson(res, F.overview);
  if (p === '/api/analytics/time-per-file') return sendJson(res, F.analytics);
  if (p === '/api/sessions') return sendJson(res, F.sessions);
  if (p === '/api/students/activity') return sendJson(res, F.activity);
  if (p === '/api/assistant/conversations') return sendJson(res, F.conversations);
  if (p.startsWith('/api/assistant/conversations/')) {
    const id = p.split('/')[4];
    return sendJson(res, F.messages[id] || { student: '?', messages: [] });
  }
  if (p === '/api/assistant') return sendJson(res, F.assistant);
  if (p.startsWith('/api/sessions/') && p.endsWith('/stream')) {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache',
      'Connection': 'keep-alive' });
    res.write(terminalReplay());
    return; // keep the SSE connection open
  }
  if (p.startsWith('/api/')) return sendJson(res, {}); // anything else: harmless empty JSON

  // ---- static files (with the server's clean URL → file mapping) ----
  const rel = p === '/' ? '/index.html'
    : p === '/analytics' ? '/analytics.html'
    : p === '/transcripts' ? '/transcripts.html'
    : p === '/login' ? '/login.html' : p;
  const file = path.join(STATIC_DIR, rel);
  if (!file.startsWith(STATIC_DIR)) { res.writeHead(403); return res.end(); }
  fs.readFile(file, (err, buf) => {
    if (err) { res.writeHead(404); return res.end('not found'); }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
    res.end(buf);
  });
});

server.listen(PORT, () => console.log(`hermione-ui fixtures server on http://localhost:${PORT} (static: ${STATIC_DIR})`));
