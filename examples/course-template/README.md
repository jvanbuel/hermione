# Course template

A starting point for a Hermione-observed course. Commit this layout to the
repository your students clone/open as a devcontainer.

```
your-course/
├── .devcontainer/devcontainer.json   # connects students to the backend
├── .hermione.json                    # identity source + maps files → exercises
├── lab1/ lab2/ project/ ...          # your exercises
```

The `.hermione.json` is the course config a student's extension reads:

```jsonc
{
  "identity": "github",          // how to identify the student (see below)
  "shareFileContents": true,     // may a teacher open the file a student
                                  // has on screen? (they are told when one does)
  "exercises": [                  // map files → exercise slugs
    { "name": "lab1", "match": "lab1/**" },
    { "name": "lab2", "match": "lab2/**" }
  ]
}
```

## One-time setup (course administrator)

With the backend running and `HERMIONE_ADMIN_TOKEN` set:

```bash
# 1. Create the course — returns its enrollment token.
curl -X POST https://hermione.example.edu/api/admin/courses \
  -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"slug":"cs101","name":"CS 101"}'

# 2. Create an admin account and grant it the course.
curl -X POST https://hermione.example.edu/api/admin/admins \
  -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"username":"prof","password":"<pick-one>"}'

curl -X POST https://hermione.example.edu/api/admin/memberships \
  -H "Authorization: Bearer $HERMIONE_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"username":"prof","courseSlug":"cs101"}'
```

Put the enrollment token from step 1 into `.devcontainer/devcontainer.json`
(`HERMIONE_TOKEN`) and set `HERMIONE_BACKEND` to your backend.

Optionally define the course's **exercises** (title + order) so the dashboard
shows them all — even ones nobody has started:

```bash
curl -X POST https://hermione.example.edu/api/exercises \
  -H "Cookie: hermione_session=<from logging in>" \
  -H 'Content-Type: application/json' \
  -d '{"course":"cs101","exercises":[
        {"slug":"lab1","title":"Lab 1 — Sorting","position":0},
        {"slug":"lab2","title":"Lab 2 — Trees","position":1}
      ]}'
```

## Student identity (no login)

Identity is derived from the container environment — students never log in. The
`identity` field in `.hermione.json` picks the source:

| `identity`  | Source                              | Trust |
|-------------|-------------------------------------|-------|
| `github`    | `$GITHUB_USER`                      | **Trustworthy in GitHub Codespaces** (platform-set); weak elsewhere |
| `git-email` | `git config user.email`             | Stable, but student-editable |
| `env`       | `HERMIONE_STUDENT`                  | Trustworthy only if the *teacher* sets it |
| `os`        | OS username                         | Collides in shared devcontainers — avoid |
| *(omitted)* | auto: github → git-email → os       | — |

This is classroom **attribution**, not authentication: a determined student can
spoof it. For graded/high-stakes use, run on **GitHub Codespaces** (so
`$GITHUB_USER` is platform-authenticated), or issue per-student enrollment
tokens via GitHub Classroom.

## What students do

Nothing special: they open the repo in its devcontainer. The extension begins
reporting their active file/exercise, and every terminal is recorded — all
scoped to this course.

## What the admin sees

Log in at `https://hermione.example.edu/` as `prof` and pick **CS 101** from the
course switcher to watch the class live.
