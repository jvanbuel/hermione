// Deterministic fixture data for the Hermione teacher web UI.
//
// The real pages (crates/server/static/*.html) talk to the Rust + Postgres
// backend over /api/*. To screenshot them without standing up the whole stack,
// server.js serves the static files and answers those endpoints from the data
// below. Everything a page fetches on load is covered here.
//
// Times are relative to a `now` injected by the harness (see FIXED_NOW in
// server.js / shoot.js) so "27m", "6s ago" and the freshness clock render
// identically on every run — that keeps screenshot diffs meaningful.
//
// Edit these fixtures to stage whatever situation you want to capture: a calm
// class, a room full of struggling students, an empty course, a long file
// list, unicode names, etc. The shapes here mirror what the handlers in
// crates/server/src return; if you change an API shape in the backend, mirror
// it here so the screenshots stay honest.
function buildFixtures(now) {
  const mins = m => now - m * 60_000;

  const courses = [
    { slug: 'cs101', name: 'CS101 · Intro to Systems', repoUrl: 'https://github.com/acme/cs101', archived: false },
    { slug: 'algo', name: 'Algorithms (Fall)', repoUrl: null, archived: false },
  ];

  // A course's defined exercises (GET /api/exercises?course=…).
  const courseExercises = [
    { slug: 'ex1-pointers', title: 'Exercise 1 — Pointers & memory', position: 0 },
    { slug: 'ex2-strings', title: 'Exercise 2 — Strings', position: 1 },
    { slug: 'ex3-trees', title: 'Exercise 3 — Binary trees', position: 2 },
  ];

  // Full detail for the course-settings panel (GET /api/courses/{slug}).
  const courseDetail = {
    slug: 'cs101',
    name: 'CS101 · Intro to Systems',
    repoUrl: 'https://github.com/acme/cs101',
    enrollmentToken: 'enroll-9904ff7c7aec4f9c',
    archived: false,
    members: ['ada', 'grace', 'linus'],
    description: 'Systems programming in C: memory, pointers, and data structures.',
    term: 'Fall 2026',
    institution: 'Acme University',
    level: 'Intermediate',
  };

  const overview = {
    exercises: [
      {
        exercise: 'ex01-pointers',
        title: 'Exercise 1 — Pointers & memory',
        stats: { total: 6, active: 5, needHelp: 1, medianSeconds: 8 * 60 },
        students: [
          // Ada and Grace are the contrast the edit signal exists for: both have
          // a big number on the clock, but only one of them is still writing code.
          { student: 'Ada Lovelace', file: 'src/list.c', exercise: 'ex01-pointers',
            status: 'active', secondsOnExercise: 27 * 60, lastSeenUnixMs: mins(0.1),
            struggle: 'help', struggleReasons: ['27 min on one file', 'many failing compiles', 'no edits for 18 min'],
            editsRecent: 0, lastEditUnixMs: mins(18),
            terminalSessionId: 's1', studentSource: 'github', repo: 'ada/systems-hw' },
          { student: 'Grace Hopper', file: 'src/list.c', exercise: 'ex01-pointers',
            status: 'active', secondsOnExercise: 13 * 60, lastSeenUnixMs: mins(0.3),
            struggle: 'watch', struggleReasons: ['13 min, no test run'],
            editsRecent: 62, lastEditUnixMs: mins(0.2),
            terminalSessionId: 's2', studentSource: 'github', repo: 'grace/hw' },
          { student: 'Alan Turing', file: 'src/main.c', exercise: 'ex01-pointers',
            status: 'active', secondsOnExercise: 6 * 60, lastSeenUnixMs: mins(0.2),
            struggle: null, struggleReasons: [], editsRecent: 34, lastEditUnixMs: mins(0.4),
            terminalSessionId: 's3', studentSource: 'github' },
          { student: 'Katherine Johnson', file: 'src/main.c', exercise: 'ex01-pointers',
            status: 'active', secondsOnExercise: 4 * 60, lastSeenUnixMs: mins(0.4),
            struggle: null, struggleReasons: [], editsRecent: 21, lastEditUnixMs: mins(0.6),
            terminalSessionId: 's4', studentSource: 'github' },
          { student: 'Linus Torvalds', file: 'Makefile', exercise: 'ex01-pointers',
            status: 'active', secondsOnExercise: 2 * 60, lastSeenUnixMs: mins(0.5),
            struggle: null, struggleReasons: [], editsRecent: 9, lastEditUnixMs: mins(1),
            terminalSessionId: 's5', studentSource: 'github' },
          { student: 'Margaret Hamilton', file: 'src/list.c', exercise: 'ex01-pointers',
            status: 'idle', secondsOnExercise: 9 * 60, lastSeenUnixMs: mins(3),
            struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: mins(4),
            terminalSessionId: 's6', studentSource: 'github' },
        ],
      },
      {
        exercise: 'ex02-strings',
        title: 'Exercise 2 — Strings',
        stats: { total: 4, active: 2, needHelp: 0, medianSeconds: 5 * 60 },
        students: [
          { student: 'Dennis Ritchie', file: 'strings.c', exercise: 'ex02-strings',
            status: 'active', secondsOnExercise: 12 * 60, lastSeenUnixMs: mins(0.2),
            struggle: 'watch', struggleReasons: ['12 min on strings.c'],
            editsRecent: 5, lastEditUnixMs: mins(2),
            terminalSessionId: 's7', studentSource: 'github' },
          { student: 'Ken Thompson', file: 'strings.c', exercise: 'ex02-strings',
            status: 'active', secondsOnExercise: 3 * 60, lastSeenUnixMs: mins(0.3),
            struggle: null, struggleReasons: [], editsRecent: 18, lastEditUnixMs: mins(0.5),
            terminalSessionId: 's8', studentSource: 'github' },
          { student: 'Barbara Liskov', file: 'strings.c', exercise: 'ex02-strings',
            status: 'idle', secondsOnExercise: 6 * 60, lastSeenUnixMs: mins(6),
            struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: mins(7),
            terminalSessionId: 's9', studentSource: 'github' },
          { student: 'Donald Knuth', file: 'strings.c', exercise: 'ex02-strings',
            status: 'idle', secondsOnExercise: 1 * 60, lastSeenUnixMs: mins(11),
            struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: null,
            terminalSessionId: 's10', studentSource: 'github' },
        ],
      },
      {
        exercise: 'ex03-trees',
        title: 'Exercise 3 — Binary trees',
        stats: { total: 3, active: 1, needHelp: 0, medianSeconds: 2 * 60 },
        students: [
          { student: 'Edsger Dijkstra', file: 'tree.c', exercise: 'ex03-trees',
            status: 'active', secondsOnExercise: 2 * 60, lastSeenUnixMs: mins(0.6),
            struggle: null, struggleReasons: [], editsRecent: 12, lastEditUnixMs: mins(1),
            terminalSessionId: 's11', studentSource: 'github' },
          { student: 'John von Neumann', file: 'tree.c', exercise: 'ex03-trees',
            status: 'idle', secondsOnExercise: 4 * 60, lastSeenUnixMs: mins(8),
            struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: mins(9),
            terminalSessionId: 's12', studentSource: 'github' },
          { student: 'Claude Shannon', file: 'tree.c', exercise: 'ex03-trees',
            status: 'idle', secondsOnExercise: 30, lastSeenUnixMs: mins(14),
            struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: null,
            terminalSessionId: 's13', studentSource: 'github' },
        ],
      },
    ],
    noExercise: [
      { student: 'Tim Berners-Lee', file: '~/.bashrc', exercise: null,
        status: 'active', secondsOnExercise: 3 * 60, lastSeenUnixMs: mins(0.5),
        struggle: null, struggleReasons: [], editsRecent: 7, lastEditUnixMs: mins(1),
        terminalSessionId: 's14', studentSource: 'github' },
      // No edit history at all — what an older extension looks like.
      { student: 'Radia Perlman', file: null, exercise: null,
        status: 'idle', secondsOnExercise: 60, lastSeenUnixMs: mins(9),
        struggle: null, struggleReasons: [], editsRecent: 0, lastEditUnixMs: null,
        terminalSessionId: null, studentSource: 'github' },
    ],
  };

  const analytics = {
    student: 'Ada Lovelace',
    totalSeconds: 96 * 60,
    perExercise: [
      { exercise: 'ex01-pointers', seconds: 54 * 60 },
      { exercise: 'ex02-strings', seconds: 27 * 60 },
      { exercise: 'ex03-trees', seconds: 15 * 60 },
    ],
    perFile: [
      { path: '/home/ada/systems-hw/src/list.c', relativePath: 'src/list.c', exercise: 'ex01-pointers', seconds: 41 * 60 },
      { path: '/home/ada/systems-hw/src/main.c', relativePath: 'src/main.c', exercise: 'ex01-pointers', seconds: 13 * 60 },
      { path: '/home/ada/systems-hw/strings.c', relativePath: 'strings.c', exercise: 'ex02-strings', seconds: 27 * 60 },
      { path: '/home/ada/systems-hw/tree.c', relativePath: 'tree.c', exercise: 'ex03-trees', seconds: 12 * 60 },
      { path: '/home/ada/systems-hw/Makefile', relativePath: 'Makefile', exercise: null, seconds: 3 * 60 },
    ],
  };

  const conversations = [
    { id: 'c1', student: 'Ada Lovelace', messageCount: 6, lastRole: 'assistant',
      preview: "Try printing the pointer value right before the loop — what do you see?",
      lastActivityUnixMs: mins(2) },
    { id: 'c2', student: 'Grace Hopper', messageCount: 4, lastRole: 'student',
      preview: "why does my linked list segfault when the list is empty?",
      lastActivityUnixMs: mins(7) },
    { id: 'c3', student: 'Dennis Ritchie', messageCount: 2, lastRole: 'assistant',
      preview: "Good question — strlen doesn't count the null terminator.",
      lastActivityUnixMs: mins(24) },
    { id: 'c4', student: 'Edsger Dijkstra', messageCount: 8, lastRole: 'student',
      preview: "thanks, that makes sense now!",
      lastActivityUnixMs: mins(52) },
  ];

  const messages = {
    c1: {
      student: 'Ada Lovelace',
      messages: [
        { role: 'student', body: "I'm getting a segfault in my list_append function but I can't figure out where.", createdAtUnixMs: mins(9) },
        { role: 'assistant', body: "Let's narrow it down together. Can you show me what `head` is set to when you call `list_append` the first time?", createdAtUnixMs: mins(8) },
        { role: 'student', body: "It's NULL because the list starts empty.", createdAtUnixMs: mins(6) },
        { role: 'assistant', body: "Right — so when you write `head->next`, you're dereferencing a NULL pointer. What check could you add before touching `head->next`?", createdAtUnixMs: mins(5) },
        { role: 'student', body: "Oh! I should check if head == NULL first and handle that case separately.", createdAtUnixMs: mins(3) },
        { role: 'assistant', body: "Exactly. Try printing the pointer value right before the loop — what do you see?", createdAtUnixMs: mins(2) },
      ],
    },
  };

  const sessions = overview.exercises.flatMap(g => g.students)
    .concat(overview.noExercise)
    .filter(s => s.terminalSessionId)
    .map(s => ({ student: s.student, id: s.terminalSessionId }));

  const activity = sessions.map(s => ({ student: s.student }));

  const assistant = {
    available: true, enabled: true, configured: true,
    model: 'claude-sonnet-5',
    systemPrompt: "You are a patient teaching assistant for an intro systems course. Guide students toward the answer with questions and small hints. Never paste a full solution.",
    skills: [{ type: 'anthropic', skillId: 'pdf' }],
    mcpServers: [{ name: 'docs', url: 'https://mcp.example.com/sse' }],
  };

  return { courses, courseDetail, courseExercises, overview, analytics, conversations, messages, sessions, activity, assistant };
}
module.exports = { buildFixtures };
