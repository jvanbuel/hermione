const test = require('node:test');
const assert = require('node:assert/strict');
const Since = require('../static/assets/since.js');

const lines = (...l) => l.join('\n');

test('identical text has no changes', () => {
  assert.deepEqual(Since.diff('a\nb', 'a\nb'), { state: 'same' });
});

test('an added line and a removed line are counted and numbered on both sides', () => {
  const r = Since.diff(lines('a', 'b', 'c'), lines('a', 'x', 'b'));
  assert.equal(r.state, 'differs');
  assert.equal(r.added, 1);
  assert.equal(r.removed, 1);
  const rows = r.hunks.flatMap(h => h.rows);
  assert.deepEqual(rows.map(x => x.sign + x.text), [' a', '+x', ' b', '-c']);
  assert.deepEqual(rows.map(x => [x.old, x.new]), [[1, 1], [null, 2], [2, 3], [3, null]]);
});

test('a changed line is a removal and an addition, the removal first', () => {
  const r = Since.diff(lines('one', 'two', 'three'), lines('one', 'TWO', 'three'));
  assert.equal(r.added, 1);
  assert.equal(r.removed, 1);
  assert.deepEqual(r.hunks[0].rows.map(x => x.sign + x.text), [' one', '-two', '+TWO', ' three']);
});

test('unchanged lines far from a change are left out, and hunks report their ranges', () => {
  const old = Array.from({ length: 40 }, (_, i) => `line ${i + 1}`);
  const next = old.slice();
  next[19] = 'line 20 edited';
  const r = Since.diff(old.join('\n'), next.join('\n'));
  assert.equal(r.hunks.length, 1);
  const h = r.hunks[0];
  assert.equal(h.rows.length, 3 + 2 + 3); // context, the pair, context
  assert.deepEqual([h.oldStart, h.oldLines, h.newStart, h.newLines], [17, 7, 17, 7]);
});

test('changes close together share a hunk; distant ones do not', () => {
  const old = Array.from({ length: 60 }, (_, i) => `l${i}`);
  const next = old.slice();
  next[10] = 'A'; next[13] = 'B';   // within context of each other
  next[50] = 'C';
  const r = Since.diff(old.join('\n'), next.join('\n'));
  assert.equal(r.hunks.length, 2);
});

test('a change at the very start or end works', () => {
  assert.equal(Since.diff('a\nb', 'z\na\nb').added, 1);
  assert.equal(Since.diff('a\nb', 'a\nb\nz').added, 1);
  const gone = Since.diff('a\nb\nc', 'b\nc');
  assert.deepEqual([gone.added, gone.removed], [0, 1]);
});

test('the empty file and the whole file', () => {
  const r = Since.diff('', 'a\nb');
  assert.equal(r.state, 'differs');
  assert.equal(r.added, 2); // '' is one empty line, replaced
  assert.equal(r.removed, 1);
});

test('a comparison too big to be worth doing here says so', () => {
  const big = (p) => Array.from({ length: 2500 }, (_, i) => `${p}${i}`).join('\n');
  assert.equal(Since.diff(big('a'), big('b')).state, 'uncomparable');
});

test('a large file with one local edit is fast because the ends are peeled off first', () => {
  const old = Array.from({ length: 30_000 }, (_, i) => `line ${i}`);
  const next = old.slice();
  next[15_000] = 'edited';
  const started = Date.now();
  const r = Since.diff(old.join('\n'), next.join('\n'));
  assert.equal(r.state, 'differs');
  assert.deepEqual([r.added, r.removed], [1, 1]);
  assert.ok(Date.now() - started < 1000);
});

const memory = () => {
  const m = new Map();
  return { getItem: k => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, v) };
};

test('what was on screen is recalled for the same file, and only that file', () => {
  const store = memory();
  Since.remember(store, 'k', 'src/a.c', 'old a', 1000);
  Since.remember(store, 'k', 'src/b.c', 'old b', 2000);
  assert.equal(Since.recall(store, 'k', 'src/a.c', 3000).content, 'old a');
  assert.equal(Since.recall(store, 'k', 'src/c.c', 3000), null);
  assert.equal(Since.anything(store, 'k', 3000), true);
  assert.equal(Since.anything(store, 'other', 3000), false);
});

test('yesterday is not last time', () => {
  const store = memory();
  Since.remember(store, 'k', 'a.c', 'x', 0);
  assert.equal(Since.recall(store, 'k', 'a.c', 25 * 3600 * 1000), null);
});

test('only the most recent few files are kept, and a huge one is not kept at all', () => {
  const store = memory();
  for (let i = 0; i < 12; i++) Since.remember(store, 'k', `f${i}.c`, 'x', 1000 + i);
  assert.equal(Since.recall(store, 'k', 'f0.c', 2000), null);
  assert.ok(Since.recall(store, 'k', 'f11.c', 2000));
  Since.remember(store, 'k', 'huge.c', 'x'.repeat(200_000), 3000);
  assert.equal(Since.recall(store, 'k', 'huge.c', 3000), null);
});

test('a corrupt or full store is tolerated', () => {
  const broken = { getItem: () => '{not json', setItem: () => { throw new Error('quota'); } };
  assert.equal(Since.recall(broken, 'k', 'a.c', 1), null);
  assert.doesNotThrow(() => Since.remember(broken, 'k', 'a.c', 'x', 1));
});
