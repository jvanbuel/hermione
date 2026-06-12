# Course template

A starting point for a Hermione-observed course. Commit this layout to the
repository your students clone/open as a devcontainer.

```
your-course/
├── .devcontainer/devcontainer.json   # connects students to the backend
├── .hermione.json                    # maps files → exercises
├── lab1/ lab2/ project/ ...          # your exercises
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

## What students do

Nothing special: they open the repo in its devcontainer. The extension begins
reporting their active file/exercise, and every terminal is recorded — all
scoped to this course.

## What the admin sees

Log in at `https://hermione.example.edu/` as `prof` and pick **CS 101** from the
course switcher to watch the class live.
