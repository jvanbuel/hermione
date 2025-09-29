# 🔮 Hermione - Real-time File Monitor

A WebSocket-based system that allows VSCode extensions to share active file information with web applications in real-time.

## Architecture

- **WebSocket Server** (Rust + tokio-tungstenite): Central server managing sessions and message routing
- **VSCode Extension** (TypeScript): Monitors active files and sends updates to the server
- **Svelte Web App** (JavaScript + Svelte): Displays active file information from all connected VSCode sessions

## Components

### 1. WebSocket Server (`src/main.rs`)

A Rust-based WebSocket server that:
- Manages client connections and sessions
- Routes messages between VSCode extensions and web clients
- Handles session creation and file update broadcasting
- Runs on `ws://localhost:8080`

**Message Types:**
- `register`: Client registration (vscode/webapp)
- `file_update`: File change notifications from VSCode
- `session_created`: New session confirmation
- `file_updated`: Broadcast to web clients
- `error`: Error messages

### 2. VSCode Extension (`vscode-extension/`)

A TypeScript extension that:
- Connects to the WebSocket server on startup
- Monitors active editor changes
- Sends file path and content updates
- Provides connect/disconnect commands
- Shows connection status in the status bar

**Features:**
- Auto-reconnection on disconnect
- Configurable server URL
- Optional file content sharing
- Debounced updates to prevent spam

### 3. Svelte Web App (`webapp/`)

A Svelte application that:
- Connects to the WebSocket server
- Displays active sessions and files
- Shows file content with syntax highlighting
- Auto-reconnects on connection loss
- Responsive design with dark theme

**Features:**
- Real-time session monitoring
- File type icons and language detection
- Clean, modern UI
- Connection status indicator

## Quick Start

### 1. Start the WebSocket Server

```bash
cargo run
```

The server will start on `ws://localhost:8080`.

### 2. Set up the Svelte Web App

```bash
cd webapp
npm install
npm run dev
```

The web app will be available at `http://localhost:5173`.

### 3. Install the VSCode Extension

```bash
cd vscode-extension
npm install
npm run compile
```

Then install the extension in VSCode:
- Open the Command Palette (`Ctrl+Shift+P`)
- Run "Extensions: Install from VSIX..."
- Select the compiled extension

Or for development:
- Open the `vscode-extension` folder in VSCode
- Press `F5` to launch a new Extension Development Host window

## Configuration

### VSCode Extension Settings

- `hermione.serverUrl`: WebSocket server URL (default: `ws://localhost:8080`)
- `hermione.autoConnect`: Auto-connect on startup (default: `true`)
- `hermione.sendFileContent`: Include file content in updates (default: `true`)

### Commands

- `Hermione: Connect to Hermione Service`: Manual connection
- `Hermione: Disconnect from Hermione Service`: Disconnect from service

## Development

### WebSocket Server

```bash
# Run the server
cargo run

# Check for compilation errors
cargo check

# Run with debug logging
RUST_LOG=debug cargo run
```

### Svelte Web App

```bash
cd webapp
npm run dev    # Development server
npm run build  # Production build
```

### VSCode Extension

```bash
cd vscode-extension
npm run compile  # Compile TypeScript
npm run watch    # Watch mode for development
```

## Protocol

### Client Registration

**VSCode Extension:**
```json
{
  "type": "register",
  "client_type": "vscode"
}
```

**Web App:**
```json
{
  "type": "register",
  "client_type": "webapp"
}
```

### File Updates

**From VSCode:**
```json
{
  "type": "file_update",
  "session_id": "uuid",
  "active_file": "/path/to/file.js",
  "file_content": "optional file content..."
}
```

**To Web App:**
```json
{
  "type": "file_updated",
  "session_id": "uuid",
  "active_file": "/path/to/file.js",
  "file_content": "optional file content..."
}
```

## Security Notes

- The server runs on localhost only
- File content sharing is optional and configurable
- No authentication is currently implemented (suitable for local development)

## Future Enhancements

- [ ] Authentication and authorization
- [ ] Multiple workspace support
- [ ] File diff visualization
- [ ] Custom themes for web app
- [ ] Plugin system for custom file processors
- [ ] Remote server deployment options