# Hermione - Student Observability Platform

A real-time observability platform for monitoring student learning activities. Teachers can see what files students are editing in VS Code and what commands they're running in their terminal.

## Architecture

```
┌──────────────────┐     ┌─────────────────┐     ┌──────────────────┐
│   VS Code        │     │                 │     │   Svelte         │
│   Extension      │────▶│   Axum Backend  │◀────│   Dashboard      │
│   (File Monitor) │ WS  │   (WebSocket)   │ WS  │   (Teacher View) │
└──────────────────┘     └─────────────────┘     └──────────────────┘
                                 ▲
                                 │ WS
                         ┌───────┴────────┐
                         │  Shell Wrapper │
                         │ (Terminal I/O) │
                         └────────────────┘
```

## Components

### 1. Backend (`backend/`)

Rust/Axum WebSocket server that:
- Manages student sessions and connections
- Routes file updates from VS Code to the dashboard
- Streams terminal I/O from shell wrapper to the dashboard
- Handles session lifecycle and heartbeats

**Endpoints:**
- `GET /ws` - WebSocket endpoint for all clients
- `GET /health` - Health check endpoint

### 2. VS Code Extension (`vscode-extension/`)

TypeScript extension that:
- Monitors active file changes in the editor
- Sends file content and cursor position to the backend
- Auto-connects on startup with reconnection support
- Shows connection status in the status bar

### 3. Shell Wrapper (`shell-wrapper/`)

Rust binary similar to the `script` command that:
- Creates a PTY and spawns a shell
- Captures all stdin/stdout in real-time
- Streams terminal I/O to the backend via WebSocket
- Maintains terminal functionality (colors, line editing, etc.)

### 4. Web Dashboard (`webapp/`)

Svelte application that:
- Displays all active student sessions
- Shows real-time file content from VS Code sessions
- Shows terminal output from shell sessions
- Supports multiple concurrent students

## Quick Start

### 1. Start the Backend

```bash
cd backend
cargo run
```

The server starts on `http://localhost:8080`.

### 2. Start the Web Dashboard

```bash
cd webapp
npm install
npm run dev
```

Dashboard available at `http://localhost:5173`.

### 3. Connect as a Student

**Option A: VS Code Extension**

```bash
cd vscode-extension
npm install
npm run compile
```

Then in VS Code:
- Press `F5` to launch Extension Development Host, or
- Package with `npx vsce package` and install the `.vsix`

**Option B: Terminal Monitoring**

```bash
cd shell-wrapper
cargo run -- --name "Student Name"
```

This starts a monitored shell session.

## Configuration

### VS Code Extension Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `hermione.serverUrl` | `ws://localhost:8080/ws` | Backend WebSocket URL |
| `hermione.autoConnect` | `true` | Auto-connect on startup |
| `hermione.sendFileContent` | `true` | Include file content in updates |
| `hermione.sendCursorPosition` | `true` | Send cursor position |
| `hermione.studentName` | `""` | Student name for identification |
| `hermione.debounceMs` | `500` | Debounce delay for file updates |
| `hermione.maxFileSize` | `100000` | Max file size to send (chars) |

### Shell Wrapper Options

```bash
hermione-shell [OPTIONS]

Options:
  -s, --server <URL>   WebSocket server URL [default: ws://localhost:8080/ws]
  -n, --name <NAME>    Student name for identification
  -c, --shell <SHELL>  Shell to run [default: $SHELL or /bin/bash]
      --offline        Run without server connection
  -h, --help           Print help
```

## Protocol

### Client Types

- `vscode` - VS Code extension (creates session, sends file updates)
- `shell` - Shell wrapper (creates session, sends terminal I/O)
- `webapp` - Dashboard (receives all updates)

### Message Types

**Client → Server:**
```typescript
// Registration
{ type: "register", client_type: "vscode" | "shell" | "webapp", student_name?: string }

// File update (VS Code)
{ type: "file_update", session_id: string, file_path: string, file_content?: string, cursor_position?: { line: number, column: number } }

// Terminal output (Shell)
{ type: "terminal_output", session_id: string, output: string, stream: "stdout" | "stderr" }

// Terminal input (Shell)
{ type: "terminal_input", session_id: string, input: string }

// Heartbeat
{ type: "heartbeat", session_id: string }
```

**Server → Client:**
```typescript
// Session created
{ type: "session_created", session_id: string }

// File updated (to webapp)
{ type: "file_updated", session_id: string, student_name?: string, file_path: string, file_content?: string, cursor_position?: object, timestamp: string }

// Terminal data (to webapp)
{ type: "terminal_data", session_id: string, student_name?: string, output: string, stream: string, timestamp: string }

// Session list (to webapp on connect)
{ type: "session_list", sessions: SessionInfo[] }

// Session disconnected
{ type: "session_disconnected", session_id: string }

// Error
{ type: "error", message: string }
```

## Development

### Build All Components

```bash
make build
```

### Run in Development

```bash
# Terminal 1: Backend
make run-backend

# Terminal 2: Dashboard
cd webapp && npm run dev

# Terminal 3: Shell wrapper (as student)
make run-shell
```

### Clean Build

```bash
make clean
```

## Project Structure

```
hermione/
├── backend/                 # Axum WebSocket server
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── shell-wrapper/           # PTY shell wrapper
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── vscode-extension/        # VS Code extension
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       └── extension.ts
├── webapp/                  # Svelte dashboard
│   ├── package.json
│   ├── vite.config.js
│   └── src/
│       ├── App.svelte
│       ├── FileMonitor.svelte
│       └── TerminalMonitor.svelte
├── Cargo.toml              # Workspace config
├── Makefile
└── README.md
```

## Security Notes

- The server runs on localhost by default
- No authentication is implemented (suitable for local/classroom use)
- File content sharing is optional and configurable
- Terminal I/O is streamed in real-time

## License

GPL-3.0
