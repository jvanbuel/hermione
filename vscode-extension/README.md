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
  "relativePath": "ex1/main.py",
  "line": 42, "column": 17,     // where the caret actually is
  "dirty": true,                // unsaved changes in the buffer
  "content": "…",               // the buffer, as they see it this instant
  "base": "head",               // "head" | "untracked" | "none"
  "diff": { "added": 8, "removed": 1, "hunks": [ /* unified-diff hunks */ ] },
  "atUnixMs": 1718200000000
}
```

Notes on how it behaves:

- **Nothing is retried.** A snapshot describes one instant; a late one is worse
  than none, so a failed send is simply dropped and the next request re-asks.
- **The diff covers unsaved work.** The baseline is the committed file as `git`
  reports it (via the built-in git extension), and it is compared against the
  live buffer — not the file on disk, which is what `git diff` alone would show.
  A notebook cell reports the cell, with no baseline; a file with no committed
  version reports `base: "untracked"`.
- **The student can see it.** While a teacher is looking, the status bar reads
  *"teacher viewing"* and is highlighted; it clears on its own when they stop.
- **Either side can switch it off.** `"shareFileContents": false` in the
  course's `.hermione.json` (the teacher's policy) or the
  `hermione.shareFileContents` setting (the student's own veto). The more
  restrictive of the two wins, and the extension answers the request with a
  refusal so the dashboard says so rather than spinning forever.
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

## Develop

```bash
npm install
npm run compile      # type-check (tsc --noEmit)
npm run bundle       # build out/extension.js (esbuild, inlines `ws`)
npm run package      # produce hermione-vscode.vsix
```

Then press `F5` in VSCode to launch an Extension Development Host. Make sure the
backend is running (`cargo run -p hermione-server`) and open
**http://localhost:8080** to watch activity appear on the board.
