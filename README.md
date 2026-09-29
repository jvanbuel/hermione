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
  process, so it reports file-focus, heartbeat, and edit-burst events as plain JSON to the
  Axum server (`POST /api/file-events`). Far less machinery than gRPC in a
  TypeScript extension, and it still lands in the same Postgres.
- **File contents are pulled on demand, and never stored.** Activity is history
  worth keeping; the buffer someone has on screen is not. When a teacher opens a
  student's file, the backend sends a control frame down that student's existing
  message WebSocket, their extension answers with one snapshot, and it lives in
  memory for minutes. The diff is computed in the extension, against the
  baseline `git` itself reports for HEAD — so it covers edits the student hasn't
  saved yet, which `git diff` on its own would miss.
- **Syntax highlighting happens in the backend** (`crates/server/src/highlight.rs`,
  [syntect]). A snapshot is highlighted once when it arrives rather than once per
  teacher per poll, the dashboard gains no vendored language packs, and what
  crosses the wire is **spans, not colours** — each line becomes `[class, text]`
  pairs from a seven-class vocabulary that the page maps onto its own design
  tokens, so highlighting follows the chalkboard/whiteboard themes instead of
  importing a third palette. The page keeps control of escaping and of where the
  student's caret goes. The diff arrives as rows that each carry their own sign,
  line numbers and spans, so the page has nothing to look up or count.
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
| `--github-app-id` | `HERMIONE_GITHUB_APP_ID` | none — GitHub App id used to mint **repository-scoped** seeding tokens (preferred; see note below). Needs `--github-app-private-key-path` too |
| `--github-app-private-key-path` | `HERMIONE_GITHUB_APP_PRIVATE_KEY_PATH` | none — path to the GitHub App's PEM private key file |
| `--github-token`  | `HERMIONE_GITHUB_TOKEN`  | none — fallback PAT that reads a linked repo's folders to seed exercises. Only sent to owners in `--github-allowed-owners`; use a **read-only, minimally-scoped** token (see note below) |
| `--github-allowed-owners` | `HERMIONE_GITHUB_ALLOWED_OWNERS` | empty — comma-separated GitHub owners/orgs whose repos may be read with the PAT. Empty ⇒ PAT-backed seeding off (public repos still seed) |

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
terminal, a long time on one exercise, or a long stretch on it without typing
anything — are flagged **needs help** and sorted to the top, with a count in the
header. Students who are still actively editing show a **typing** marker, so a
big number on the clock isn't mistaken for being stuck.

### 5. (Optional) Report editor activity

Install the VSCode extension (`vscode-extension/`) on the student's machine to
report their active file and exercise. See
[`vscode-extension/README.md`](vscode-extension/README.md) for setup and the
`.hermione.json` exercise-mapping format.

```bash
cd vscode-extension && npm install && npm run compile
# then press F5 in VSCode to launch an Extension Development Host
```

For a tour of what teachers see, with screenshots, read the
[teacher's guide](docs/teacher-guide.md).

### 6. (Optional) Watch a file

With the extension installed, a student's pane on the board has a
**Terminal / File** switch. **File** shows the buffer they have on screen right
now — syntax-highlighted, their cursor included — and **Diff** toggles it to the
working changes against their last commit. Clicking someone in the tree view
opens the file directly, since that's what you were already looking at.

Highlighting covers the languages [syntect] ships (C/C++, Python, Rust, Go,
Java, JavaScript, Ruby, PHP, shell, SQL, HTML/CSS/JSON/YAML, Markdown and more);
TypeScript borrows the JavaScript syntax, and anything unrecognised — or over
4000 lines — renders as plain text rather than wrongly coloured.

The **Diff** view is highlighted too, the way GitHub's is: added-or-removed is
carried entirely by the row's background, and the foreground is left to the
syntax. (GitHub's own `diffBlob.additionLine.fgColor` is plain
`fgColor.default` — spending the foreground on the diff signal as well is what
would make the two compete.)

The extension sends each hunk as unified-diff `lines` and nothing derived from
them — no added/removed counts, no per-hunk line counts; the backend works those
out from the lines, so they cannot disagree with them. It answers with `rows`. Context and added lines are lines of the buffer, so their spans are read
out of the highlighted buffer by line number. Removed lines exist only in the
last commit, which the backend never sees, so the hunk's old side is parsed as a
fragment and only the removed rows are kept. A row's spans are used only if
they rebuild that row's text exactly — a buffer clamped at the size cap has
fewer lines than its diff refers to — and that check happens once, when the
snapshot arrives, rather than per row in the browser.

Within an edited line the words that changed are marked too. A run of removed
rows followed by a run of added ones is an edit, so the two are paired line for
line and compared word by word (`similar`, over words, whitespace runs and single
symbols); a marked span carries a third element, `[class, text, 1]`, and the page
underlines it in the row's colour. A pair that shares less than half of the
shorter line's text (spacing doesn't count), or would be marked from end to end,
is left to the row's wash. The mark is an underline rather than the stronger
background GitHub uses because the syntax colours already sit just above 4.5:1
on the row washes, and a second wash under the changed words would take them
below it.

The contents of a file are **pulled, never pushed**: the backend asks that one
student's editor for a snapshot only while a teacher has their file pane open,
nothing is written to Postgres, and the cached snapshot expires in minutes.
File *activity* (which file, which exercise, how long) works as before and is
unaffected. The student's status bar says "teacher viewing" whenever this is
happening, and either side can switch it off — `"shareFileContents": false` in
the course's `.hermione.json`, or the `hermione.shareFileContents` setting.

### Reference solutions

A course can name where its answers live in the linked GitHub repo — a branch,
tag or commit (`solutionsRef`), a folder (`solutionsDir`), or both — in *Course
settings*. Teachers then get a **Solution** view beside **Diff**: the student's
file against the solution **at the same path** (`ex01/list.c` is looked up as
`<solutionsDir>/ex01/list.c` at `<solutionsRef>`). It is drawn with the same rows
as the commit diff, with the solution as the old side: a `-` row is a line the
solution has that the student's file lacks, a `+` row one that is only theirs.
Line endings and a final newline don't count as differences.

The design follows from one requirement: **a solution must never reach a
student's machine.** So the comparison is made on the server, from the two texts
(the editor has no solution to diff against), and only answered to a course's
teachers; a student's editor gets nothing back from posting a snapshot. It also
means the repository must keep the branch or folder hidden from students — the
repository is what they clone — which Hermione can't enforce for you.

- **Every empty case is a state of its own** — not configured, no repository
  linked, not a GitHub repo, no solution for this file, GitHub refusing, a buffer
  too long to compare — so the pane says which, instead of showing nothing.
- **Nothing is read unless a teacher asks**, and what is read is cached: 5 minutes
  when found, 1 minute when missing, 20 seconds when GitHub failed, and one
  request in flight per file however many polls arrive. A GitHub App token costs
  two extra requests to mint, so without the cache a pane would spend the rate
  limit in minutes.
- **The path is untrusted.** The branch and folder are parsed into types that
  cannot hold a `..`, a space or a query character, and the student's file path —
  which comes from an editor — is checked the same way; the request URL is built
  from percent-encoded segments, never a format string, so a `?`, `#` or `%` in a
  file name stays inside its segment. Redirects are refused, so a token can't be
  followed to another owner's repository. Credentials follow the same ladder as
  seeding: a repo-scoped App token, else the shared token for allow-listed owners,
  else none (public repos).
- A GitHub 404 is ambiguous (missing file, missing branch, or a repo GitHub won't
  show us), so one is probed to tell them apart: a mistyped branch reads as *not
  found*, not as *no solution for this file*.

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
- `GET /api/students/file?student=alice` — what that student has on screen:
  `{connected, latest}`, where `latest` is `null` until their editor answers and
  otherwise `{ageMs, rev, snapshot}`. The snapshot is one of `declined`,
  `empty` or `file`; a file carries its text, the caret (`cursor`), one list of
  `[class, text]` spans per line (`highlight`), and a `baseline` — `head` with
  the diff as rows, `untracked`, or `none`. Each call also asks their editor for
  a fresh one, so nothing is captured unless a teacher is looking (see
  **Watching a file** below). `ageMs` is by the server's clock, and `rev`
  changes with every snapshot received. A snapshot whose text is unchanged from
  the last (a cursor move, which is most of them) reuses that one's highlight
  rather than parsing the buffer again — about 13 µs against 300 ms for a
  2,000-line file. `&compare=solution` adds a `solution` beside the file — how it
  stands against the course's reference solution (see **Reference solutions**
  below); nothing is read from GitHub unless it is asked for.
- `POST /api/file-snapshots` — one such snapshot, posted by the extension in
  answer to that request. A report that describes an impossible state — a blank
  student, a cursor on line 0, a diff line with no sign — is refused with a 4xx
  rather than stored.
- `GET /api/analytics/time-per-file?student=alice` — estimated time-on-task per
  file and per exercise for one student.
- `GET /api/courses` / `POST /api/courses` — list the courses the signed-in
  teacher may access / create one (optionally linking a git repo). The creator is
  automatically enrolled, so it appears in their switcher immediately. `?archived=1`
  lists archived courses. See below.
- `GET /api/courses/{slug}` — course detail (repo, enrollment token, teachers,
  and the profile). `PATCH /api/courses/{slug}` — rename, relink the repo
  (`repoUrl: null` unlinks), set where **reference solutions** live
  (`solutionsRef`, `solutionsDir`; validated, `null` or empty clears), edit the
  **profile** (`description`, `term`, `institution`, `level`; `null` clears a
  field), or `archived: true/false`.
  `POST /api/courses/{slug}/rotate-token` — issue a
  fresh enrollment token. `POST /api/courses/{slug}/members` adds a co-teacher
  (body `{"username":"…"}`); `DELETE /api/courses/{slug}/members/{username}`
  removes one (the last member cannot be removed). A co-teacher is any existing
  admin. All scoped to a member.
- `GET /api/exercises?course=…` / `POST /api/exercises` — list / define (upsert)
  a course's exercises (title + order); `"replace": true` sets the list exactly
  (removing omitted ones), which backs the dashboard's exercises editor. The
  dashboard shows all of them, even ones nobody has started, with per-exercise
  stats.
- `GET /api/recap?course=<slug>&lesson=<n>` — a lesson summed up (per exercise, who needed help,
  quiet students, activity timeline, messages sent); `lesson=0` is the latest, larger numbers
  earlier ones, and the reply lists the lessons on offer.
- `POST /api/messages` — teacher sends a message (persisted): `{course, text}` to
  the whole course, or `{course, text, students: ["ada", …]}` to just those
  students (one to 500 names; a private message is stored once per student and
  reaches no other editor and not the dashboard).
- `GET /ws` — live message stream (WebSocket). Authenticated by the enrollment
  token (`Authorization: Bearer …`; `?token=` is still read for older editors, but
  a URL ends up in proxy logs, so don't use it in new clients) or the session
  cookie (`?course=`); pass `?since=<id>` to
  replay missed messages, omit it for live-only. Used by the extension (real-time
  broadcasts) and the dashboard.
- `GET /api/inbox?since=<id>&student=<name>` — HTTP fallback for the message inbox:
  the course's messages and those addressed to the asking student (a verified
  identity, where enforced, decides who that is; `student` is only believed where
  nothing verifies students).

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
  of exercise folders, both create paths seed the course's exercises from the
  repo: prefer the `.hermione.json` exercise list (the same file the extension
  reads, order preserved), otherwise each top-level folder (tooling/build/VCS
  dirs skipped).
    - **Dashboard "New course"** fetches the linked GitHub repo's folders over
      the API; untick *Seed exercises* to skip. Best-effort — seeding never blocks
      course creation. Only folder **names** are read (not contents). Because a
      teacher can link *any* repo URL, private-repo seeding must be authorized so
      one course can't disclose another repo's folder names. Two ways, in order of
      preference:
        - **GitHub App (recommended).** Configure `HERMIONE_GITHUB_APP_ID` +
          `HERMIONE_GITHUB_APP_PRIVATE_KEY_PATH`. For each seeding request the
          server mints a short-lived installation token scoped to *only* that
          repo (read-only `contents`/`metadata`), so the authorization boundary
          is enforced by GitHub: the token can read nothing else. Install the App
          on just the org/repos teachers may seed from. Repos the App isn't
          installed on fall back to the PAT (below) or public seeding.
        - **Fallback PAT.** `HERMIONE_GITHUB_TOKEN` is a shared token sent **only
          to owners listed in `HERMIONE_GITHUB_ALLOWED_OWNERS`** — a coarser
          allow-list guard for when a full App isn't set up. Keep it read-only and
          minimally scoped.

      Public repos seed with or without either; a private repo needs the App
      installed on it, or its owner allow-listed with the PAT.
    - **`hermione course create --link`** (run inside the repo) seeds from the
      local checkout, so it needs no token and works for private repos; opt out
      with `--no-exercises`. Seeding uses the teacher session, so it applies to
      the `--user` flow (not `--admin-token`).
    - **`hermione course init`** scaffolds a `.hermione.json` from the repo's
      top-level folders (one exercise per folder) so the mapping lives in the
      repo and the extension can read it. Commit it, then
      `hermione course create --link`. (`--force` overwrites an existing file.)
- **Manage a course from the dashboard.** The gear next to the course switcher
  opens *Course settings*: rename it, link/unlink its repo, edit its **profile**
  (description, term, institution, level), copy or **rotate** the enrollment token
  (if it leaks), add/remove **co-teachers** (any existing admin, no super-admin
  secret needed), **archive** it (keeps all data, drops it from the switcher;
  restore from *New course*), and **edit its exercises** (rename, reorder, add,
  remove). The header also links straight to the linked repo, and the switcher
  shows a course's description on hover. A teacher with no courses yet gets a
  *Create your first course* prompt. Profile fields can also be set at creation
  (`POST /api/courses` and `hermione course create --description/--term/…`).
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
  self-asserted name. That includes the message socket: a snapshot request is
  addressed to one student, and the address says who a teacher is looking at, so
  a socket is subscribed as the student it *proves* to be — never as the one it
  claims in `?student=`, which only routes where nothing verifies students. A
  teacher's own socket receives no student's requests at all.
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
On request it also serves the buffer itself, with the student's cursor and a
toggleable diff against their last commit.

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
[syntect]: https://github.com/trishume/syntect
