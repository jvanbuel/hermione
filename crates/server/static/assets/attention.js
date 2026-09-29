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

  // Flagged students not yet looked at: the worst first, then whoever has been at
  // the exercise longest, then by name so the order never flickers between polls.
  function unseen(entries, students) {
    return students
      .filter((s) => rankOf(s) && !isSeen(entries, s.student))
      .sort((a, b) =>
        rankOf(b) - rankOf(a)
        || (b.secondsOnExercise || 0) - (a.secondsOnExercise || 0)
        || a.student.localeCompare(b.student));
  }

  const next = (entries, students) => unseen(entries, students)[0];

  // Drop entries older than `maxAgeMs`, so a saved list from last week's class
  // can't mark this week's students as already looked at.
  function prune(entries, now, maxAgeMs) {
    const out = {};
    for (const [k, e] of Object.entries(entries || {})) {
      if (e && typeof e.flaggedAt === 'number' && now - e.flaggedAt <= maxAgeMs) out[k] = e;
    }
    return out;
  }

  return { update, markSeen, isSeen, unseen, next, prune };
})();
if (typeof module !== 'undefined') module.exports = Attention;
