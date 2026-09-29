// "Since I last looked": what changed in a student's file since the teacher
// last had it open.
//
// The dashboard keeps the file as it was when the teacher closed the pane, in
// this browser's localStorage, and compares it with what is on screen now. That
// answers "did my hint work?" without the server storing anything, and without
// the student's code leaving the teacher's own machine any further than it
// already has. Plain functions, loaded as a page global (`Since`) and, for the
// tests, as a CommonJS module.
const Since = (() => {
  const CONTEXT = 3;                       // unchanged lines shown around a change
  const MAX_CELLS = 4_000_000;             // beyond this the comparison is too costly to do here
  const MAX_STORED_CHARS = 100_000;        // a bigger file is not kept
  const KEEP_PATHS = 8;                    // files remembered per student
  const MAX_AGE_MS = 24 * 3600 * 1000;     // yesterday's class is not "last time"

  // The lines of `a` and `b` that differ, as an edit script of
  // { sign: ' ' | '-' | '+', old, new, text } with 1-based line numbers.
  // Matching lines at both ends are peeled off first: a student's edit is nearly
  // always local, so the table below is usually tiny.
  function script(a, b) {
    let head = 0;
    while (head < a.length && head < b.length && a[head] === b[head]) head++;
    let tail = 0;
    while (tail < a.length - head && tail < b.length - head
      && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
    const ma = a.slice(head, a.length - tail), mb = b.slice(head, b.length - tail);
    if ((ma.length + 1) * (mb.length + 1) > MAX_CELLS) return null;

    // Longest common subsequence of the middles.
    const w = mb.length + 1;
    const t = new Uint16Array((ma.length + 1) * w);
    for (let i = ma.length - 1; i >= 0; i--) {
      for (let j = mb.length - 1; j >= 0; j--) {
        t[i * w + j] = ma[i] === mb[j]
          ? t[(i + 1) * w + j + 1] + 1
          : Math.max(t[(i + 1) * w + j], t[i * w + j + 1]);
      }
    }
    const ops = [];
    for (let i = 0; i < head; i++) ops.push({ sign: ' ', old: i + 1, new: i + 1, text: a[i] });
    let i = 0, j = 0;
    while (i < ma.length || j < mb.length) {
      if (i < ma.length && j < mb.length && ma[i] === mb[j]) {
        ops.push({ sign: ' ', old: head + i + 1, new: head + j + 1, text: ma[i] }); i++; j++;
      } else if (i < ma.length && (j === mb.length || t[(i + 1) * w + j] >= t[i * w + j + 1])) {
        // Where either would do, the removal comes first, as in any diff.
        ops.push({ sign: '-', old: head + i + 1, new: null, text: ma[i] }); i++;
      } else {
        ops.push({ sign: '+', old: null, new: head + j + 1, text: mb[j] }); j++;
      }
    }
    for (let k = 0; k < tail; k++) {
      ops.push({ sign: ' ', old: a.length - tail + k + 1, new: b.length - tail + k + 1, text: a[a.length - tail + k] });
    }
    return ops;
  }

  // Changes with `CONTEXT` lines around each, nearby ones merged into one hunk —
  // the same shape the server's diffs use, minus the highlighting.
  function diff(oldText, newText) {
    if (oldText === newText) return { state: 'same' };
    const ops = script(oldText.split('\n'), newText.split('\n'));
    if (!ops) return { state: 'uncomparable' };
    let added = 0, removed = 0;
    const changed = [];
    ops.forEach((o, i) => {
      if (o.sign === '+') added++;
      if (o.sign === '-') removed++;
      if (o.sign !== ' ') changed.push(i);
    });
    if (!changed.length) return { state: 'same' };   // only a trailing-newline difference, say

    const hunks = [];
    let from = null, to = null;
    const close = () => {
      if (from === null) return;
      const rows = ops.slice(from, to + 1);
      const olds = rows.filter(r => r.old !== null), news = rows.filter(r => r.new !== null);
      hunks.push({
        oldStart: olds.length ? olds[0].old : 0, oldLines: olds.length,
        newStart: news.length ? news[0].new : 0, newLines: news.length,
        rows,
      });
    };
    for (const idx of changed) {
      const lo = Math.max(0, idx - CONTEXT), hi = Math.min(ops.length - 1, idx + CONTEXT);
      if (from !== null && lo <= to + 1) { to = hi; continue; }
      close();
      from = lo; to = hi;
    }
    close();
    return { state: 'differs', added, removed, hunks };
  }

  // ---- what was on screen when the teacher last looked ----

  const readAll = (store, key, now) => {
    let all = {};
    try { all = JSON.parse(store.getItem(key) || '{}') || {}; } catch (_) { all = {}; }
    const fresh = {};
    for (const [path, e] of Object.entries(all)) {
      if (e && typeof e.content === 'string' && typeof e.at === 'number' && now - e.at <= MAX_AGE_MS) fresh[path] = e;
    }
    return fresh;
  };

  // Keep `content` as the last look at `path`. A file too big to compare later
  // is not kept, and only the most recent few files per student are.
  function remember(store, key, path, content, now) {
    if (content.length > MAX_STORED_CHARS) return;
    const all = readAll(store, key, now);
    all[path] = { content, at: now };
    const newest = Object.entries(all).sort((x, y) => y[1].at - x[1].at).slice(0, KEEP_PATHS);
    try { store.setItem(key, JSON.stringify(Object.fromEntries(newest))); } catch (_) { /* full or private */ }
  }

  const recall = (store, key, path, now) => readAll(store, key, now)[path] || null;
  const anything = (store, key, now) => Object.keys(readAll(store, key, now)).length > 0;

  return { diff, remember, recall, anything, KEEP_PATHS };
})();
if (typeof module !== 'undefined') module.exports = Since;
