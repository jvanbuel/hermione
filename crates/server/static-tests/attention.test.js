// Run with: node --test crates/server/static-tests
const test = require('node:test');
const assert = require('node:assert/strict');
const Attention = require('../static/assets/attention.js');

const s = (student, struggle, secondsOnExercise = 0) => ({ student, struggle, secondsOnExercise });
const help = (n, secs) => s(n, 'help', secs);
const watch = (n, secs) => s(n, 'watch', secs);
const fine = (n) => s(n, undefined);

test('only flagged students get an entry, and it goes when they recover', () => {
  let { entries } = Attention.update({}, [help('ada'), watch('bo'), fine('cy')], 100);
  assert.deepEqual(Object.keys(entries).sort(), ['ada', 'bo']);
  ({ entries } = Attention.update(entries, [fine('ada'), watch('bo')], 200));
  assert.deepEqual(Object.keys(entries), ['bo']);
});

test('the next student is the worst one not yet looked at', () => {
  const students = [watch('bo', 900), help('ada', 100), help('cy', 500), fine('di')];
  let { entries } = Attention.update({}, students, 0);
  assert.equal(Attention.next(entries, students).student, 'cy'); // help, longest at it
  entries = Attention.markSeen(entries, 'cy', 1);
  assert.equal(Attention.next(entries, students).student, 'ada');
  entries = Attention.markSeen(entries, 'ada', 2);
  assert.equal(Attention.next(entries, students).student, 'bo'); // watch after every help
  entries = Attention.markSeen(entries, 'bo', 3);
  assert.equal(Attention.next(entries, students), undefined);
});

test('ties in rank and time fall back to the name, so the order never flickers', () => {
  const students = [help('zed', 60), help('amy', 60)];
  const { entries } = Attention.update({}, students, 0);
  assert.deepEqual(Attention.unseen(entries, students).map((x) => x.student), ['amy', 'zed']);
});

test('a student who gets worse after being seen is unseen again, and announced', () => {
  let { entries } = Attention.update({}, [watch('bo')], 10);
  entries = Attention.markSeen(entries, 'bo', 20);
  assert.equal(Attention.isSeen(entries, 'bo'), true);
  const r = Attention.update(entries, [help('bo')], 30);
  assert.equal(Attention.isSeen(r.entries, 'bo'), false);
  assert.deepEqual(r.escalated, ['bo']);
});

test('getting better and worse again starts from not looked at', () => {
  let { entries } = Attention.update({}, [help('ada')], 10);
  entries = Attention.markSeen(entries, 'ada', 20);
  ({ entries } = Attention.update(entries, [fine('ada')], 30));
  const r = Attention.update(entries, [help('ada')], 40);
  assert.equal(Attention.isSeen(r.entries, 'ada'), false);
  assert.deepEqual(r.escalated, ['ada']);
});

test('staying flagged neither re-announces nor forgets that they were seen', () => {
  let { entries } = Attention.update({}, [help('ada')], 10);
  entries = Attention.markSeen(entries, 'ada', 20);
  const r = Attention.update(entries, [help('ada')], 30);
  assert.deepEqual(r.escalated, []);
  assert.equal(Attention.isSeen(r.entries, 'ada'), true);
});

test('a watch student who was already at help is not announced when they ease off', () => {
  let { entries } = Attention.update({}, [help('ada')], 10);
  const r = Attention.update(entries, [watch('ada')], 20);
  assert.deepEqual(r.escalated, []);
  // ...and is still flagged at the same standing, so a return to help is not news.
  assert.deepEqual(Attention.update(r.entries, [help('ada')], 30).escalated, []);
});

test('opening someone who is not flagged records nothing', () => {
  const entries = Attention.markSeen({}, 'di', 5);
  assert.deepEqual(entries, {});
});

test('a saved list from long ago is not believed', () => {
  const saved = { old: { rank: 2, flaggedAt: 0 }, recent: { rank: 2, flaggedAt: 9_000 }, junk: null };
  assert.deepEqual(Object.keys(Attention.prune(saved, 10_000, 5_000)), ['recent']);
  assert.deepEqual(Attention.prune(undefined, 1, 1), {});
});
