<img src="docs/hermione-logo.png" alt="Hermione" height="96" />

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
- **Bounded cost as data grows.** Terminal chunks are buffered and written in
  batched multi-row INSERTs (flushed by size or a short timer) rather than one
  INSERT per read. Live queries (overview, activity) only scan a recent time
  window, backed by an index; session history is replayed in pages so long
  sessions never load entirely into memory.
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
| `--admin-token`   | `HERMIONE_ADMIN_TOKEN`   | none — set it to enable the provisioning API          |
| `--bootstrap-admin-username` | `HERMIONE_BOOTSTRAP_ADMIN_USERNAME` | `admin`                            |
| `--bootstrap-admin-password` | `HERMIONE_BOOTSTRAP_ADMIN_PASSWORD` | none — set it to seed an admin on first start |
| `--anthropic-api-key` | `HERMIONE_ANTHROPIC_API_KEY` | none — set it to enable the AI teaching assistant |
| `--assistant-model`   | `HERMIONE_ASSISTANT_MODEL`   | `claude-opus-4-8`                              |

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

| Flag              | Env var              | Default                     |
|-------------------|----------------------|-----------------------------|
| `--backend`       | `HERMIONE_BACKEND`   | `http://127.0.0.1:50051`    |
| `--student`       | `HERMIONE_STUDENT`   | `$USER`                     |
| `--token`         | `HERMIONE_TOKEN`     | none — the course enrollment token (required unless in open dev mode) |
| `--auth-url`      | `HERMIONE_AUTH_URL`  | none — HTTP base URL for student sign-in; set to use verified identity |
| `--auth-provider` | `HERMIONE_AUTH_PROVIDER` | `github` — which configured IdP to sign in with |
| `--capture-input` | —                    | off — keystrokes are not recorded (see Security & privacy) |
| `--offline`       | —                    | off (record locally only)   |

### 4. Watch live

Open **http://localhost:8080**. The board groups students by the exercise
they're on, with time-on-task; click an avatar to open their terminal (multiple
tile side by side). Students who look stuck — errors or failed runs in their
terminal, or a long time on one exercise — are flagged **needs help** and sorted
to the top, with a count in the header.

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
- `GET /api/courses` / `POST /api/courses` — list the courses the signed-in
  teacher may access / create one (optionally linking a git repo). The creator is
  automatically enrolled, so it appears in their switcher immediately. See below.
- `GET /api/exercises?course=…` / `POST /api/exercises` — list / define (upsert)
  a course's exercises (title + order). The dashboard shows all of them, even
  ones nobody has started, with per-exercise stats.
- `POST /api/messages` — teacher broadcasts a message to a course (persisted).
- `GET /ws` — live message stream (WebSocket). Authenticated by the enrollment
  token (`?token=`) or the session cookie (`?course=`); pass `?since=<id>` to
  replay missed messages, omit it for live-only. Used by the extension (real-time
  broadcasts) and the dashboard.
- `GET /api/inbox?since=<id>` — HTTP fallback for the message inbox.

The teacher routes require a session cookie (obtained via `POST /api/login`);
agent routes require a course enrollment token. See below.

---

## Multi-tenancy (courses)

Everything is scoped to a **course** (the tenant). Sessions and file activity
belong to a course; admins are granted access per course; the dashboard only
ever shows the selected course's data.

- **Admin accounts + membership.** Named admin accounts log in at `/login`;
  each may be granted access to one or more courses (the dashboard has a course
  switcher). Passwords are Argon2-hashed.
- **Teachers create their own courses.** A signed-in teacher can create a course
  straight from the dashboard (the **＋** next to the course switcher) — give it a
  name, and optionally link a git repository as the course. They're enrolled
  automatically. Slug and name are derived from the repo/name when omitted. The
  same is available over the API (`POST /api/courses`) and the CLI:

  ```bash
  # link the current git repo as a course, signing in as a teacher
  hermione course create --link --user prof --server http://localhost:8080

  # or name it explicitly (repo optional)
  hermione course create cs101 --name "CS 101" --repo https://github.com/org/cs101
  ```

  The command prints the new course's enrollment token. Auth is either a teacher
  sign-in (`--user`, password prompted or `HERMIONE_PASSWORD`) or the
  provisioning secret (`--admin-token` / `HERMIONE_ADMIN_TOKEN`).
- **A course repo's folders are its exercises.** Since courses are usually a repo
  of exercise folders, `hermione course create --link` (run inside the repo)
  seeds the course's exercises from it: the `.hermione.json` exercise list if
  present (same file the extension reads, order preserved), otherwise each
  top-level folder (tooling/build/VCS dirs skipped). Opt out with
  `--no-exercises`. Seeding uses the teacher session, so it applies to the
  `--user` flow (not `--admin-token`).
- **Provisioning API** (guarded by `HERMIONE_ADMIN_TOKEN`): create courses and
  admins and grant membership.

  ```bash
  # returns the course's enrollment token (repoUrl is optional)
  curl -X POST localhost:8080/api/admin/courses   -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" -d '{"slug":"cs101","name":"CS 101"}'
  curl -X POST localhost:8080/api/admin/admins    -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" -d '{"username":"prof","password":"…"}'
  curl -X POST localhost:8080/api/admin/memberships -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" -d '{"username":"prof","courseSlug":"cs101"}'
  ```
- **Enrollment by token.** Each course has an enrollment token. The recorder
  (`--token` / `HERMIONE_TOKEN`) and extension (`hermione.token`) present it;
  the backend resolves which course the data belongs to. No global token.
- **Devcontainer flow.** A teacher commits the course token + backend URL into a
  devcontainer (see [`examples/course-template`](examples/course-template)); a
  student just opens it and is connected, scoped to that course.
- **Open dev mode.** Until the first admin exists, the dashboard is open and
  scoped to a seeded `default` course, and untokened agents land there — so
  local dev is frictionless. Creating an admin locks it down.
- **Bootstrap admin.** Setting `HERMIONE_BOOTSTRAP_ADMIN_PASSWORD` seeds an
  admin at startup (granted every existing course) so a deployment is never
  served unauthenticated. It only applies while the admin table is empty, so
  restarts and later password changes are left alone. See
  [`infra/`](infra/README.md) for the deployment that uses it.

## Verified student identity

By default `student` is derived from the environment (attribution, not auth).
Configure OIDC to make it **server-trusted** — students authenticate with an IdP
and the backend issues a short-lived Hermione identity token that agents present
with each ingest.

```bash
HERMIONE_IDENTITY_SECRET=$(openssl rand -hex 32) \
HERMIONE_OIDC_PROVIDERS='[
  {"name":"github","kind":"github","clientId":"<gh-oauth-app-client-id>"},
  {"name":"google","kind":"oidc","issuer":"https://accounts.google.com","clientId":"<google-client-id>"}
]' \
cargo run -p hermione-server
```

- **GitHub** is verified via the GitHub API; **Google and any OIDC provider** via
  discovery + JWKS. Add more by listing them (issuer + clientId).
- Once configured, identity is **enforced**: ingest without a valid identity token
  is rejected, and the verified student (e.g. `github:alice`) overrides any
  self-asserted name.
- **Agents:** the **extension** uses VSCode's GitHub sign-in (silent in
  Codespaces) and exchanges it at `POST /api/auth/exchange`. The **recorder**
  (`--auth-url`) uses the same exchange in Codespaces (the platform
  `GITHUB_TOKEN`) or the OAuth **device flow** (`/api/auth/device/*`) otherwise,
  caching the token between sessions.

> Endpoints: `POST /api/auth/exchange`, `POST /api/auth/device/{start,poll}`.
> The interactive browser/device logins require real IdP credentials and aren't
> exercised by the test suite (the token issuance/verification + enforcement are).

## Security & privacy

Hermione records keystrokes and exposes live student terminals, so treat it as
sensitive.

- **Access control** is the multi-tenant model above: admin login for the
  dashboard, per-course enrollment tokens for agents, `HERMIONE_ADMIN_TOKEN` for
  provisioning.
- **Keystrokes are not recorded by default.** The recorder streams terminal
  output but not stdin, so passwords and other typed secrets are never stored.
  `--capture-input` opts in to keystroke capture for richer analysis; even then,
  input during no-echo password prompts is redacted.
- **TLS:** terminate TLS at a reverse proxy in front of the HTTP and gRPC ports,
  and add the `Secure` attribute to the session cookie there.

---

## Deployment

The three components ship independently:

- **Backend** — a container image (built by the `Dockerfile`; the web viewer is
  embedded in the binary). Run the whole stack with `docker compose up` (Postgres
  + server), or pull `ghcr.io/jvanbuel/hermione`. Migrations run on startup.
- **Recorder** — a single static-ish binary. Install with:

  ```bash
  curl -fsSL https://raw.githubusercontent.com/jvanbuel/hermione/main/scripts/install-recorder.sh | sh
  ```

  (downloads a prebuilt binary for the host, or builds from source with cargo).
- **VSCode extension** — packaged as a `.vsix` (`cd vscode-extension && npm run
  package`), installable via `code --install-extension` or published to a
  marketplace.

CI (`.github/workflows/ci.yml`) runs fmt/clippy/test and compiles the extension.
Tagging `v*` triggers `release.yml`, which builds the recorder binaries (x86_64 +
aarch64 Linux), packages the `.vsix`, and builds/pushes the server image — the
artifacts the [`examples/course-template`](examples/course-template) devcontainer
consumes.

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

- [x] Multi-tenancy: courses, admin accounts + membership, per-course enrollment.
- [x] Struggle detection: flag students with errors/failed runs/time-stuck.
- [x] First-class exercise model: defined exercises (title + order) with stats.
- [x] Broadcast messages (teacher → students) over WebSocket.
- [x] Student identity: env-derived attribution (default) or verified OIDC/GitHub
      (GitHub, Google, any OIDC provider) — server-trusted, enforced when configured.
- [x] Authentication: admin login + per-course enrollment tokens.
- [x] AI teaching assistant: per-course, teacher-enabled, configured with a system
      prompt + agent skills/MCP (Anthropic Managed Agents). Students chat from a
      VSCode panel; courses without it are unaffected. Set
      `HERMIONE_ANTHROPIC_API_KEY` to enable.
- [ ] Two-way chat (student → teacher) on the existing WebSocket channel.
- [ ] Richer offline analytics: replay timeline.

[xterm.js]: https://xtermjs.org/
