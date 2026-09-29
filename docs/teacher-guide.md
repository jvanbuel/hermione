# Teacher's guide

What you see on the dashboard, and what your students' editors share to make it
possible. For setting Hermione up, see the [README](../README.md); the pictures
below are the chalkboard (dark) theme, and every page also has a whiteboard
(light) theme, switched from the **⋯** menu.

## The board

![The board: one column per exercise, students sorted by how stuck they look](images/teacher-board.png)

Students are grouped by the exercise they are working on, one column each. A card
shows the file they have open, how long they have been on the exercise, and how
long ago they last did anything.

- **Needs help** (red) and **watch** (amber) are how the board says "look here
  first". They are worked out from what the terminal shows (errors, failed runs),
  how long a student has been on the exercise compared with the rest of the class,
  and how long they have gone without typing. Those thresholds are set per
  exercise from how the class is actually doing, so a hard exercise doesn't flag
  everyone.
- **Typing** means the student has made edits in the last few minutes (hover it for
  the count). A big clock on someone who is still typing is usually work, not being
  stuck, so typing softens the flag.
- The **1 need help** count in the top bar is the whole course at a glance.
- Click a student to open their pane. Several panes tile side by side.

## The tree view

![The tree view: repository folders, with students on the files they have open](images/teacher-tree.png)

The switch beside the course name, its right-hand button, changes from the board
to the tree. It shows the course repository as folders and files, with each
student on the file they have open right now. It answers "who is in `list.c`?"
and "is anyone touching `ex04` yet?" at a glance.

- A folder shows how many students are inside it, in red or amber when any of them
  needs help or is worth watching, so a collapsed folder still tells you.
- Folders nobody is in are dimmed. Click a folder (or press Enter or Space on it)
  to collapse it; your choice is remembered per course.
- Click a student to open their pane, on their file.

## A student's pane

Each pane has a **Terminal / File** switch, and **Diff** when a file is showing.

### Terminal

Their terminal session, replayed and then live.

### File

![A student's file, with their cursor](images/teacher-file.png)

The file as the student sees it this instant, unsaved edits included, with syntax
highlighting and a marker on the line their cursor is on. The line under the path
tells you whether the buffer is **unsaved**, whether it was **truncated** (files
over 256 KB are cut), and how old the snapshot is.

### Diff

![The same file as a diff against their last commit](images/teacher-file-diff.png)

**Diff** shows the working changes against the student's last commit, in the style
of GitHub's diff: added and removed lines are washed green and red, and the
syntax colours are left alone. Inside an edited line, the words that actually
changed are underlined, so `malloc(8)` → `malloc(sizeof(node_t))` shows what moved
rather than only which lines. It includes changes they haven't saved.

It says when there is nothing to compare with:

- *hasn't committed this file yet* — a new file with no earlier version.
- *No git baseline for this file* — a notebook cell, or a folder that isn't a git
  repository.
- *No changes since their last commit.*
- *The diff is too large to show.* Very large changes are cut rather than sent
  whole.

### When there is no file to show

The pane says why instead of staying blank:

| It says | Because |
|---|---|
| *Asking …'s editor…* | the request has just gone out; the answer usually arrives within a second |
| *…'s editor isn't connected* | the VSCode extension isn't running on their machine |
| *…has file sharing turned off* | they have switched sharing off (see below) |
| *…has no file open* | sharing is on, but no editor tab is open |
| *Lost the connection to the server — retrying…* | your browser can't reach the server; the last file stays on screen |

## The top bar, and the status bar

The bar holds what you use all class: the course, the board/tree switch, Analytics,
Transcripts and Broadcast. What you use once a term — **New course**, **Assistant
settings**, the theme and **Sign out** — is behind the **⋯** menu. On a narrow
window the labels drop away before any control does, so nothing is ever cut off.

The strip along the bottom says how fresh the board is (*Updated 2s ago*) and shows
the one-time hint for new users.

## Analytics, Transcripts and Broadcast

![Time on task for one student](images/teacher-analytics.png)

- **Analytics** — time on task for one student, per exercise and per file. It counts
  the gaps between file events, capped per gap, so a lunch break isn't counted as
  work.
- **Transcripts** — the conversations students have had with the teaching
  assistant, by student, so you can see where the assistant is (or isn't) helping.
- **Broadcast** — a message to every student in the course, delivered into their
  editor.

## What students share

Seeing a file is the most sensitive thing on this dashboard, so it is deliberately
narrow:

- **Only while you are looking.** A student's editor is asked for the file only
  while you have their **File** pane open. Close the pane and nothing more is sent.
- **Nothing is stored.** The latest snapshot is held in memory on the server for a
  few minutes and never written to the database.
- **They can see it, and stop it.** Their status bar says *teacher viewing*
  whenever it is happening. Either side can switch file sharing off: the student
  with the `hermione.shareFileContents` setting, or you with
  `"shareFileContents": false` in the course's `.hermione.json`. When it is off, the
  pane says so rather than showing anything.
- **Only their own name is trusted.** Where student sign-in is enforced, which
  student a request is about comes from their verified identity, not from what the
  editor claims, so one student's editor can't be made to answer for another's.

Which file a student has open, which exercise it belongs to and how long they have
been on it are reported separately, as they always have been, and are not affected
by the file-sharing switch.
