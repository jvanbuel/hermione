// Which flagged students the teacher has already looked at, and who to go to next.
//
// The board flags students who need help ("help") or deserve a glance ("watch").
// Walking round a class, a teacher wants the next one they have NOT been to, not
// the same red card again. This is that bookkeeping, as plain functions over a
// plain object so it can be tested without a browser and saved as JSON.
//
//   entries = { [student]: { rank, flaggedAt, seenAt? } }
//
// An entry exists only while a student is flagged. Once they recover it is
// dropped, so being flagged again later starts from "not looked at". Loaded as a
// page global (`Attention`) and, for the tests, as a CommonJS module.
const Attention = (() => {
  const RANK = { help: 2, watch: 1 };
  const rankOf = (s) => RANK[s.struggle] || 0;

  // Entries for students no longer flagged are dropped; a newly flagged student
  // gets one; a student whose flag got worse (watch -> help) starts over, since
  // what the teacher saw is no longer what is happening.
  //
  // `escalated` lists who newly reached "help" in this update, for alerting.
  function update(entries, students, now) {
    const next = {};
    const escalated = [];
    for (const s of students) {
      const rank = rankOf(s);
      if (!rank) continue;
      const before = entries[s.student];
      if (before && before.rank >= rank) {
        // Still flagged as badly as before, or a little better: the same entry,
        // so a student flickering between "help" and "watch" is neither
        // announced again nor made to look unvisited.
        next[s.student] = before;
      } else {
        next[s.student] = { rank, flaggedAt: now };
        if (rank === 2) escalated.push(s.student);
      }
    }
    return { entries: next, escalated };
  }

  // The teacher opened this student. Only a flagged student has anything to mark.
  function markSeen(entries, student, now) {
    const e = entries[student];
    return e ? { ...entries, [student]: { ...e, seenAt: now } } : entries;
  }

  const isSeen = (entries, student) => {
    const e = entries[student];
    return !!e && e.seenAt !== undefined && e.seenAt >= e.flaggedAt;
  };

  // "Not now": the teacher knows about this one and will come back. A snoozed
  // student stays flagged on the board but drops out of what asks for attention
  // — the next-up key, the count in the tab title — until the time is up, or
  // until they get worse, which starts their entry afresh and so ends the snooze.
  function snooze(entries, student, until) {
    const e = entries[student];
    return e ? { ...entries, [student]: { ...e, snoozedUntil: until } } : entries;
  }
  const isSnoozed = (entries, student, now) => {
    const e = entries[student];
    return !!e && typeof e.snoozedUntil === 'number' && e.snoozedUntil > now;
  };

  // Worst first, then whoever has been at the exercise longest, then by name so
  // the order never flickers between polls.
  const byUrgency = (a, b) =>
    rankOf(b) - rankOf(a)
    || (b.secondsOnExercise || 0) - (a.secondsOnExercise || 0)
    || a.student.localeCompare(b.student);

  // Flagged students not yet looked at and not snoozed, most urgent first.
  function unseen(entries, students, now = Date.now()) {
    return students
      .filter((s) => rankOf(s) && !isSeen(entries, s.student) && !isSnoozed(entries, s.student, now))
      .sort(byUrgency);
  }

  const next = (entries, students, now) => unseen(entries, students, now)[0];

  // Everyone flagged, most urgent first: the ring N and P walk round. From
  // `student` (who need not be flagged) one step forwards or backwards, wrapping;
  // from nobody, forwards starts at the top and backwards at the bottom.
  function step(students, student, direction) {
    const ring = students.filter(rankOf).sort(byUrgency);
    if (!ring.length) return undefined;
    const at = ring.findIndex((s) => s.student === student);
    if (at < 0) return direction > 0 ? ring[0] : ring[ring.length - 1];
    return ring[(at + direction + ring.length) % ring.length];
  }

  // Pinned students are the teacher's own priority ("keep an eye on this one
  // all lesson"): they go first wherever a list is drawn, in their existing order.
  const pinnedFirst = (students, pins) =>
    students.slice().sort((a, b) => (pins[b.student] ? 1 : 0) - (pins[a.student] ? 1 : 0));
  function togglePin(pins, student) {
    const next = { ...pins };
    if (next[student]) delete next[student]; else next[student] = true;
    return next;
  }

  // Drop entries older than `maxAgeMs`, so a saved list from last week's class
  // can't mark this week's students as already looked at.
  function prune(entries, now, maxAgeMs) {
    const out = {};
    for (const [k, e] of Object.entries(entries || {})) {
      if (e && typeof e.flaggedAt === 'number' && now - e.flaggedAt <= maxAgeMs) out[k] = e;
    }
    return out;
  }

  return { update, markSeen, isSeen, snooze, isSnoozed, unseen, next, step, pinnedFirst, togglePin, prune };
})();
if (typeof module !== 'undefined') module.exports = Attention;
