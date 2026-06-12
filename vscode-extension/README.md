# Hermione VSCode Extension

Reports which file a student has active to the Hermione backend, so a teacher
can see live what each student is working on (for timely intervention) and so
time-on-task per file and per exercise can be analyzed offline.

It is **passive and transparent**: it never changes the student's editor. It
sends a small JSON event when the active file changes, a periodic heartbeat
while a file stays focused (paused when the window is unfocused), and a `close`
event when a file is closed.

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
  "kind": "focus",            // "focus" | "heartbeat" | "close"
  "atUnixMs": 1718200000000
}
```

Events are queued and retried, so a brief backend outage neither loses data nor
disrupts the editor.

It also opens a WebSocket (`/ws`) to receive **teacher broadcasts** for the
course in real time, shown as notifications. On (re)connect it replays anything
missed via a `since` cursor, so messages aren't lost across disconnects.

## Settings

| Setting                      | Default                  | Description                                      |
|------------------------------|--------------------------|--------------------------------------------------|
| `hermione.serverUrl`         | `http://localhost:8080`  | Hermione backend HTTP base URL                   |
| `hermione.student`           | `""`                     | Student id (falls back to `$HERMIONE_STUDENT` / OS username) |
| `hermione.token`             | `""`                     | Course enrollment token (falls back to `$HERMIONE_TOKEN`); selects the course the data belongs to |
| `hermione.heartbeatSeconds`  | `15`                     | How often to confirm the current file is active  |
| `hermione.enabled`           | `true`                   | Start reporting automatically                    |

Commands: **Hermione: Start / Stop Reporting**, **Hermione: Set Student Identifier**.

## Mapping files to exercises

Add a `.hermione.json` at the workspace root to label files with an exercise.
Patterns are matched against the workspace-relative path (`**`, `*`, `?`):

```json
{
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
