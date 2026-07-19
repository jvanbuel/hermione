---
name: hermione-ui-screenshots
description: >-
  Render the real Hermione teacher web UI (crates/server/static/*.html — the
  live board, analytics, transcripts, login, terminal panes, and modals) with
  realistic fixture data and screenshot every page in both the dark
  "chalkboard" and light "whiteboard" themes — no Rust backend or Postgres
  required. Use this skill WHENEVER you touch the web frontend and want to see
  or show the result: editing tokens.css or any static HTML/CSS/JS, reviewing a
  UI change, producing before/after screenshots, verifying a restyle across
  themes, or when the user says "show me screenshots", "how does the board
  look", "did that break light mode", or similar. Reach for it before hand-
  rolling a Playwright harness — the fixtures, fake API server, and shooter are
  already built here.
---

# Hermione UI screenshots

Hermione's web UI is plain static HTML/CSS/JS in `crates/server/static/` that
talks to a Rust + Postgres backend over `/api/*` and SSE. Standing that stack up
just to look at a CSS change is slow and often impossible in a sandbox. This
skill serves the **real, unmodified** pages and answers every `/api/*` call they
make from deterministic fixtures, then drives them with Playwright — so what you
screenshot is exactly what ships, only the data is faked.

## When to use it

Any time you change or review the frontend. The whole point of the product is
at-a-glance triage (who needs help, how long they've been stuck), so a UI change
is only "done" when you've *looked* at it — in both themes. Use this to:

- produce before/after screenshots for a restyle,
- confirm a change survives light **and** dark mode,
- sanity-check that the JS↔CSS↔API contract still renders (class names and IDs
  in the static files are load-bearing; a rename can silently blank a page).

## Quickest path

From the repo root (`OUT_DIR` is the only required input):

```bash
OUT_DIR=/tmp/hermione-shots LABEL=after \
  .claude/skills/hermione-ui-screenshots/scripts/capture.sh
```

That starts the fixtures server, captures every page in both themes, and stops
the server. You'll get these PNGs in `$OUT_DIR` (× `dark` and `light`):

```
<LABEL>-board-<theme>.png        # live class overview (the hero screen)
<LABEL>-terminal-<theme>.png     # board with a student's xterm pane open
<LABEL>-analytics-<theme>.png    # per-student time-on-task
<LABEL>-transcripts-<theme>.png  # student ↔ assistant conversation
<LABEL>-login-<theme>.png        # sign-in
<LABEL>-broadcast-<theme>.png    # broadcast composer modal over the board
```

Then look at them with the Read tool, and send the ones that matter with
SendUserFile. For a **before/after**, run `capture.sh` twice against the same
checkout — once with `LABEL=before` (stash or `git stash` your change first),
once with `LABEL=after` — into the same `OUT_DIR`.

## Requirements & gotchas

- **Node + Playwright + Chromium.** In this environment Playwright is installed
  *globally*, so `require('playwright')` fails from the repo. `capture.sh`
  handles this by exporting `NODE_PATH=$(npm root -g)`; if you invoke `shoot.js`
  directly, set it yourself. Chromium is at `/opt/pw-browsers/chromium` (override
  with `PW_CHROMIUM`). Do **not** run `playwright install`.
- **Deterministic clock.** The server and shooter share `FIXED_NOW` (an epoch
  ms) and Playwright freezes `Date`, so "6s ago" and the freshness counter are
  identical every run — that keeps screenshot diffs meaningful. `capture.sh`
  sets it; keep both sides equal if you run the pieces by hand.
- **Themes** come from `localStorage['hermione.theme']` read before first paint;
  the shooter pins it per pass, so there's no flash of the wrong theme.

## Running the pieces by hand

Useful when you want a custom viewport, a single page, or to poke at one screen:

```bash
export NODE_PATH=$(npm root -g)
FIXED_NOW=$(node -e 'process.stdout.write(String(Date.parse("2025-07-19T13:30:00Z")))')
PORT=8799 FIXED_NOW=$FIXED_NOW node .claude/skills/hermione-ui-screenshots/scripts/server.js &
PORT=8799 FIXED_NOW=$FIXED_NOW OUT_DIR=/tmp/shots LABEL=after \
  node .claude/skills/hermione-ui-screenshots/scripts/shoot.js
```

## Staging different situations

The screenshots are only as good as the data. `scripts/fixtures.js` builds every
API response from one `buildFixtures(now)` function; edit it to stage what you
need to see — a calm class vs. a room full of `struggle: 'help'` students, an
empty course (return empty `exercises`/`noExercise` to check empty states), long
file lists, unicode or very long names (to test truncation), a disabled
assistant (`available: false`), and so on. The shapes mirror the handlers in
`crates/server/src`; **if you change an API shape in the backend, mirror it here**
so the screenshots stay honest. To capture a new interaction (e.g. the assistant
settings modal), add a `shoot(...)` call in `scripts/shoot.js` with an `action`
that clicks into it.

## What's in `scripts/`

- `capture.sh` — orchestrates server + shooter; the one command you usually want.
- `server.js` — static file server + fake `/api/*` (including an SSE terminal
  replay for open panes). Defaults `STATIC_DIR` to `crates/server/static`.
- `shoot.js` — Playwright driver; one `shoot()` call per page, both themes.
- `fixtures.js` — all the fake data, in one editable function.
