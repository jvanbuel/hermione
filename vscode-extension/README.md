# Hermione VSCode Extension

Reports which file a student has active to the Hermione backend, so a teacher
can see live what each student is working on (for timely intervention) and so
time-on-task per file and per exercise can be analyzed offline.

It is **passive and transparent**: it never changes the student's editor. It
sends a small JSON event when the active file changes, a periodic heartbeat
while a file stays focused (paused when the window is unfocused), an `edit`
event summarizing each burst of typing, and a `close` event when a file is
closed.

Edits are reported as a **count of changes, never their content** — enough to
tell a student who is writing code from one who is stuck on the same screen,
without shipping their work off the machine.

File contents are the one exception, and they are **only ever sent on request**:
see [Watching a file](#watching-a-file).

## What it sends

`POST {serverUrl}/api/file-events` with a batch of:

```jsonc
{
  "student": "alice",
  "workspace": "course",
  "path": "/home/alice/course/ex1/main.py",
  "relativePath": "ex1/main.py",
  "language": "python",
  "exercise": "ex1",          // resolved from .hermione.json, if any
  "kind": "focus",            // "focus" | "heartbeat" | "edit" | "close"
  "edits": 12,                // "edit" only: changes coalesced into this event
  "line": 42,                 // 1-based cursor line, when the file is on screen
  "atUnixMs": 1718200000000
}
```

Events are queued and retried, so a brief backend outage neither loses data nor
disrupts the editor.

## Watching a file

A teacher can open the file a student has on screen — the buffer, their cursor,
and the diff against their last commit — from the dashboard.

This is a **pull, not a push**. The extension sends nothing until the backend
asks, over the message WebSocket it already holds, and the backend only asks
while a teacher has that student's file pane open. Each answer is one
`POST {serverUrl}/api/file-snapshots`:

```jsonc
{
  "student": "alice",
  "state": "file",              // "file" | "empty" (nothing open) | "declined"
  "path": "/home/alice/course/ex1/main.py",
  "relativePath": "ex1/main.py",
  "language": "python",         // VSCode's language id
  "cursor": { "line": 42, "column": 17 },   // where the caret actually is
  "dirty": true,                // unsaved changes in the buffer
  "content": "…",               // the buffer, as they see it this instant
  "baseline": {                 // what the buffer is compared with:
    "kind": "head",             //   "head" | "untracked" | "none"
    "hunks": [ { "oldStart": 5, "newStart": 5, "lines": [" ctx", "-old", "+new"] } ]
  }
}
```

The report is a union on `state`, not a bag of optional fields: a `declined`
report has no `content` to send, so it cannot carry one, and a file that is
`untracked` has no diff against its last commit. The extension also leaves out
anything the server can work out for itself — how many lines a diff added or
removed, how long a hunk is — so a count that disagrees with its lines cannot be
sent. The server refuses reports that describe an impossible state (a blank
student, a cursor on line 0, a diff line with no `' '`, `'+'` or `'-'`).

Notes on how it behaves:

- **Nothing is retried.** A snapshot describes one instant; a late one is worse
  than none, so a failed send is simply dropped and the next request re-asks.
- **The diff covers unsaved work.** The baseline is the committed file as `git`
  reports it (via the built-in git extension), and it is compared against the
  live buffer — not the file on disk, which is what `git diff` alone would show.
  A notebook cell reports the cell, with no baseline (`"none"`, as does a file
  outside any git repository); a file with no committed version reports
  `"untracked"`.
- **Git is asked once per commit, not once per snapshot.** While watched, a
  snapshot is built on every keystroke and cursor move, and each question to git
  is a child process on the student's machine. The committed text is cached per
  file and keyed on HEAD's commit — exact, since nothing but a new commit changes
  what HEAD holds — and the diff is reused while the buffer is the same. "No such
  file" is only believed for ten seconds, so a transient git failure can't leave
  a tracked file looking untracked until the next commit.
- **The student can see it.** While a teacher is looking, the status bar reads
  *"teacher viewing"* and is highlighted; it clears on its own when they stop.
- **Either side can switch it off.** `"shareFileContents": false` in the
  course's `.hermione.json` (the teacher's policy) or the
  `hermione.shareFileContents` setting (the student's own veto). The more
  restrictive of the two wins, and the extension answers the request with a
  refusal so the dashboard says so rather than spinning forever.
- **Only course files are ever shown.** A file outside every workspace folder,
  or one that holds credentials (`.env*`, `*.pem`, `*.key`, `id_rsa*`, anything
  under `.ssh`/`.aws`/`.gnupg`, and similar), is answered with the same refusal
  as an opt-out, whatever the teacher opens.
- **Syntax highlighting is the backend's job**, not the extension's. The server
  classifies the buffer on arrival and sends the dashboard spans rather than
  colours; the extension sends `language` (VSCode's language id) and the path,
  which is all that choice needs.
- Nothing here is written to the database. Snapshots are cached in the server's
  memory and expire in minutes.

It also opens a WebSocket (`/ws`) to receive **teacher broadcasts** for the
course in real time, shown as notifications. On (re)connect it replays anything
missed via a `since` cursor, so messages aren't lost across disconnects.

## Settings

| Setting                      | Default                  | Description                                      |
|------------------------------|--------------------------|--------------------------------------------------|
| `hermione.serverUrl`         | `http://localhost:8080`  | Backend URL (overridden by `backend` in `.hermione.json` / `$HERMIONE_*`) |
| `hermione.student`           | `""`                     | Manual identity override (identity is otherwise derived — see below) |
| `hermione.token`             | `""`                     | Course enrollment token (overridden by `token` in `.hermione.json` / `$HERMIONE_TOKEN`) |
| `hermione.heartbeatSeconds`  | `15`                     | How often to confirm the current file is active  |
| `hermione.shareFileContents` | `true`                   | Answer a teacher's request to see the file you have open (see above) |
| `hermione.enabled`           | `true`                   | Start reporting automatically                    |

Commands: **Hermione: Start / Stop Reporting**, **Hermione: Set Student Identifier**.

## Course config & identity (`.hermione.json`)

The committed `.hermione.json` is the single course config — it carries the
backend, enrollment token, identity source, and the file→exercise mapping. The
**student identity is derived from the container environment** (no login); the
`identity` field picks the source (`github` / `git-email` / `env` / `os`, or
auto). See [`examples/course-template`](../examples/course-template) for the
trust model.

```json
{
  "backend": "https://hermione.example.edu:50051",
  "token": "<course enrollment token>",
  "identity": "github",
  "shareFileContents": true,
  "exercises": [
    { "name": "ex1", "match": "ex1/**" },
    { "name": "ex2", "match": ["ex2/**", "solutions/ex2/*"] }
  ]
}
```

Files that match no rule simply have no exercise.

**Trust.** A repository you have just cloned can carry any `.hermione.json`, so
until you trust the workspace the `backend` and `token` in it are ignored (your
own settings apply) and a notice says so. The GitHub token used to sign in is
only ever sent over `https`, or to this machine. A `.hermione.json` that is
present but not valid JSON is reported rather than silently ignored.

Diagnostics go to the **Hermione** output channel.

## Develop

```bash
npm install
npm run compile      # type-check (tsc --noEmit)
npm test             # unit tests for the git baseline cache (node:test, no VSCode needed)
npm run bundle       # build out/extension.js (esbuild, inlines `ws`)
npm run package      # produce hermione-vscode.vsix
```

Then press `F5` in VSCode to launch an Extension Development Host. Make sure the
backend is running (`cargo run -p hermione-server`) and open
**http://localhost:8080** to watch activity appear on the board.
