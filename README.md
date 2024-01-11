# ![1696401093521](image/README/1696401093521.png)

Hermione is an ambitious, yet overly confident teaching assistant (or, if you will, a teacher's pet).

### Commands

- `configure`: configures the paths (folder structure) for a named `exercise` and the common struggles that students face as context for feedback. The config is stored as a yaml file and is used as context for (AI-generated) feedback.
- `listen`: records the stdin and stdout of the student, and syncs it to a file (or remote storage) for in promptu feedback, and for later analysis
- `summarize`: based on the recorded data, summarizes the students' progress and common struggles, as feedback to the teachers. Provide an easy-to-use interface to add this summary as additional exercise context.

### Implementation ideas

- Use ChatGPT (teacher context) in combination with GitHub Copilot to provide feedback or suggestions to students
- Use Bubble Tea for a TUI for teachers to provide exercise context to Hermione. Cobra as CLI framework.

### Further random thoughts

- Analyze student performance (e.g. time spent on exercises, number of attempts, etc.) to provide feedback to teachers on how to improve their exercises
- Analyze student performance with and without


### TODO

- [] Write stdin, stdout and sterr to file for later analysis
- Close goroutine after shell child process is closed
- Write buffered output to limit disk IO
- [ ] Create a simple CLI with Cobra
    - [ ] add  
- [ ] Create a simple TUI with Bubble Tea
- [ ]