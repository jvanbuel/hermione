# 🔮 Hermione

**An observability platform for teachers.**

Hermione lets an instructor watch students' terminal sessions live — to give
feedback the moment someone gets stuck — and stores every session for offline
analysis of where students spend their time and where they struggle.

The terminal recorder is **transparent**: like the classic `script` command, it
spawns the student's normal shell inside a pseudo-terminal and faithfully
forwards I/O in both directions, while teeing every byte to the backend.

---

## Architecture

```
   ┌────────────────────┐        gRPC (stream)        ┌──────────────────────────┐
   │  hermione (recorder)│ ──────────────────────────▶ │     hermione-server      │
   │  transparent PTY,   │   IngestEvent: start /      │  ┌────────────────────┐  │
   │  script-like        │   chunk / resize / end      │  │ Ingest  gRPC svc   │  │
   └────────────────────┘                              │  │ Viewer  gRPC svc   │  │
            ▲  student's real terminal                 │  └─────────┬──────────┘  │
            │                                          │       persist │ fan-out  │
            ▼                                          │         ┌─────▼───────┐   │
   student types / sees output                        │         │  Postgres   │   │
                                                       │         │ (SeaORM)    │   │
   ┌────────────────────┐         SSE (live)          │         └─────────────┘   │
   │  teacher's browser  │ ◀─────────────────────────  │   Axum HTTP + web viewer  │
   │  xterm.js viewer    │    base64 terminal chunks   │                           │
   └────────────────────┘                              └──────────────────────────┘
```

A Rust workspace with five crates:

| Crate                 | What it is                                                               |
|-----------------------|--------------------------------------------------------------------------|
| `crates/proto`        | The gRPC/protobuf contract (`proto/hermione.proto`), compiled with tonic |
| `crates/recorder`     | The `hermione` CLI — a transparent, `script`-like terminal recorder      |
| `crates/server`       | `hermione-server` — tonic gRPC ingest/viewer + Axum HTTP/SSE web viewer  |
| `crates/entity`       | SeaORM entities for the Postgres schema                                  |
| `crates/migration`    | SeaORM migrations (runs automatically on server start)                   |
| `vscode-extension`    | VSCode extension reporting the student's active file + exercise          |

### Key design decisions

- **Transport: gRPC bidirectional streaming (tonic).** The recorder client-streams
  a sequence of `IngestEvent`s (`SessionStart`, `TerminalChunk`, `Resize`,
  `SessionEnd`) to the backend. A typed contract with built-in backpressure.
- **Storage: everything in Postgres (SeaORM).** Sessions live in `sessions`;
  raw terminal activity is append-only in `terminal_events`. Each event keeps
  **both** forms of the bytes: the verbatim raw bytes (ANSI escapes and all,
  which may not be valid UTF-8) base64-encoded in `data` for faithful replay,
  and an ANSI-stripped, lossy-UTF8 plain-text version in `text` that is
  readable and searchable for offline analysis.
- **Editor activity: HTTP/JSON, not gRPC.** The VSCode extension is a Node
  process, so it reports file-focus and heartbeat events as plain JSON to the
  Axum server (`POST /api/file-events`). Far less machinery than gRPC in a
  TypeScript extension, and it still lands in the same Postgres.
- **Live web view: Server-Sent Events.** Browsers can't speak raw gRPC, so the
  Axum server exposes an SSE endpoint that replays history then tails live. The
  bundled viewer renders it with [xterm.js]. Native/programmatic observers can
  use the gRPC `Viewer` service instead.

---

## Quick start

### 1. Start Postgres

```bash
docker compose up -d
```

### 2. Run the backend

```bash
cargo run -p hermione-server
# gRPC on 0.0.0.0:50051, HTTP/web viewer on http://localhost:8080
```

Migrations run automatically on startup. Configuration is via flags or env vars:

| Flag              | Env var                  | Default                                               |
|-------------------|--------------------------|-------------------------------------------------------|
| `--database-url`  | `HERMIONE_DATABASE_URL`  | `postgres://hermione:hermione@localhost:5432/hermione`|
| `--grpc-addr`     | `HERMIONE_GRPC_ADDR`     | `0.0.0.0:50051`                                       |
| `--http-addr`     | `HERMIONE_HTTP_ADDR`     | `0.0.0.0:8080`                                        |

### 3. Record a session (on the student's machine)

```bash
cargo run -p hermione-recorder
# or the built binary, named `hermione`:
./target/debug/hermione --student alice
```

This drops you into your normal shell. Everything you do is recorded and
streamed. Type `exit` (or Ctrl-D) to end the session. To record a specific
command instead of a shell:

```bash
hermione --student alice -- python3 exercise.py
```

Recorder options:

| Flag           | Env var              | Default                     |
|----------------|----------------------|-----------------------------|
| `--backend`    | `HERMIONE_BACKEND`   | `http://127.0.0.1:50051`    |
| `--student`    | `HERMIONE_STUDENT`   | `$USER`                     |
| `--offline`    | —                    | off (record locally only)   |

### 4. Watch live

Open **http://localhost:8080** in a browser. Pick a session from the sidebar to
watch it live; ended sessions replay their full history. The **Students · open
files** panel shows what each student currently has open in their editor.

### 5. (Optional) Report editor activity

Install the VSCode extension (`vscode-extension/`) on the student's machine to
report their active file and exercise. See
[`vscode-extension/README.md`](vscode-extension/README.md) for setup and the
`.hermione.json` exercise-mapping format.

```bash
cd vscode-extension && npm install && npm run compile
# then press F5 in VSCode to launch an Extension Development Host
```

---

## API reference

### gRPC (`proto/hermione.proto`)

- `Ingest.StreamSession(stream IngestEvent) → IngestSummary` — recorders push here.
- `Viewer.ListSessions(...) → ListSessionsResponse` — list all sessions.
- `Viewer.WatchSession(WatchRequest) → stream TerminalChunk` — replay + live tail.

### HTTP

- `GET /` — the bundled xterm.js web viewer.
- `GET /api/sessions` — JSON list of sessions.
- `GET /api/sessions/{id}/stream?history=true` — SSE stream of terminal chunks
  (`{ stream, offset_ms, data, text }`, where `data` is verbatim base64 bytes
  and `text` is the ANSI-stripped plain text).
- `GET /api/sessions/{id}/transcript?stream=stdout` — the ANSI-stripped plain
  text transcript of a session as `text/plain` (`stream` = `stdout` (default),
  `stdin`, or `all`).
- `POST /api/file-events` — batch of editor file-activity events (used by the
  VSCode extension).
- `GET /api/students/activity` — the latest file activity per student (what each
  student has open right now).
- `GET /api/analytics/time-per-file?student=alice` — estimated time-on-task per
  file and per exercise for one student.

---

## Development

```bash
cargo build              # build everything
cargo clippy --workspace # lint
cargo run -p hermione-server
```

The protobuf compiler is needed to build `crates/proto`. A vendored `protoc` is
used automatically if one isn't on `PATH`.

---

## Roadmap

Milestone 1 delivers the terminal pipeline end-to-end: transparent recorder →
gRPC ingest → Postgres → live web view.

Milestone 2 (in progress) adds editor observability: the **VSCode extension**
reports the student's active file and resolved exercise; the backend exposes
live per-student activity and time-on-task analytics, surfaced in the viewer.

Planned next:

- [ ] First-class exercise model (assignments table; teacher-side mapping UI).
- [ ] Correlate terminal sessions with editor activity per student/exercise.
- [ ] Authentication and per-class access control for teachers.
- [ ] Richer offline analytics: replay timeline, struggle detection.
- [ ] Render stdin keystrokes distinctly in the viewer (e.g. input highlighting).

[xterm.js]: https://xtermjs.org/
