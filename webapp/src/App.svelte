<script>
  import { onMount, onDestroy } from 'svelte';
  import FileMonitor from './FileMonitor.svelte';
  import TerminalMonitor from './TerminalMonitor.svelte';

  let ws = null;
  let connected = false;
  let connectionStatus = 'Disconnected';
  let sessions = {};
  let terminalBuffers = {};
  let serverUrl = 'ws://localhost:8080/ws';

  // Message types from backend
  // session_created, file_updated, terminal_data, terminal_input_received,
  // session_list, session_disconnected, error

  function connectWebSocket() {
    try {
      ws = new WebSocket(serverUrl);

      ws.onopen = () => {
        console.log('Connected to Hermione server');
        connected = true;
        connectionStatus = 'Connected';

        // Register as webapp client
        ws.send(JSON.stringify({
          type: 'register',
          client_type: 'webapp'
        }));
      };

      ws.onmessage = (event) => {
        try {
          const message = JSON.parse(event.data);
          handleMessage(message);
        } catch (e) {
          console.error('Failed to parse message:', e);
        }
      };

      ws.onclose = () => {
        console.log('WebSocket connection closed');
        connected = false;
        connectionStatus = 'Disconnected';
        setTimeout(connectWebSocket, 3000);
      };

      ws.onerror = (error) => {
        console.error('WebSocket error:', error);
        connected = false;
        connectionStatus = 'Error';
      };
    } catch (e) {
      console.error('Failed to connect:', e);
      connectionStatus = 'Error';
      setTimeout(connectWebSocket, 3000);
    }
  }

  function handleMessage(message) {
    console.log('Received:', message);

    switch (message.type) {
      case 'session_list':
        // Initialize sessions from list
        message.sessions.forEach(session => {
          if (!sessions[session.session_id]) {
            sessions[session.session_id] = {
              id: session.session_id,
              student_name: session.student_name,
              client_type: session.client_type,
              current_file: session.current_file,
              file_content: null,
              cursor_position: null,
              connected_at: session.connected_at,
              last_activity: session.last_activity
            };
          }
        });
        sessions = sessions;
        break;

      case 'file_updated':
        if (!sessions[message.session_id]) {
          sessions[message.session_id] = {
            id: message.session_id,
            client_type: 'vscode'
          };
        }
        sessions[message.session_id] = {
          ...sessions[message.session_id],
          student_name: message.student_name,
          current_file: message.file_path,
          file_content: message.file_content,
          cursor_position: message.cursor_position,
          last_activity: message.timestamp
        };
        sessions = sessions;
        break;

      case 'terminal_data':
        if (!terminalBuffers[message.session_id]) {
          terminalBuffers[message.session_id] = [];
        }
        terminalBuffers[message.session_id].push({
          type: 'output',
          stream: message.stream,
          content: message.output,
          timestamp: message.timestamp
        });
        // Keep only last 1000 entries
        if (terminalBuffers[message.session_id].length > 1000) {
          terminalBuffers[message.session_id] = terminalBuffers[message.session_id].slice(-1000);
        }
        terminalBuffers = terminalBuffers;

        // Also update session info
        if (!sessions[message.session_id]) {
          sessions[message.session_id] = {
            id: message.session_id,
            client_type: 'shell',
            student_name: message.student_name
          };
        }
        sessions[message.session_id].last_activity = message.timestamp;
        sessions[message.session_id].student_name = message.student_name || sessions[message.session_id].student_name;
        sessions = sessions;
        break;

      case 'terminal_input_received':
        if (!terminalBuffers[message.session_id]) {
          terminalBuffers[message.session_id] = [];
        }
        terminalBuffers[message.session_id].push({
          type: 'input',
          content: message.input,
          timestamp: message.timestamp
        });
        terminalBuffers = terminalBuffers;
        break;

      case 'session_disconnected':
        if (sessions[message.session_id]) {
          sessions[message.session_id].disconnected = true;
          sessions = sessions;
        }
        break;

      case 'error':
        console.error('Server error:', message.message);
        break;
    }
  }

  function getSessionType(session) {
    return session.client_type || 'unknown';
  }

  function getVSCodeSessions() {
    return Object.values(sessions).filter(s => getSessionType(s) === 'vscode' && !s.disconnected);
  }

  function getShellSessions() {
    return Object.values(sessions).filter(s => getSessionType(s) === 'shell' && !s.disconnected);
  }

  function getDisconnectedSessions() {
    return Object.values(sessions).filter(s => s.disconnected);
  }

  function clearDisconnected() {
    Object.keys(sessions).forEach(id => {
      if (sessions[id].disconnected) {
        delete sessions[id];
        delete terminalBuffers[id];
      }
    });
    sessions = sessions;
    terminalBuffers = terminalBuffers;
  }

  onMount(() => {
    connectWebSocket();
  });

  onDestroy(() => {
    if (ws) {
      ws.close();
    }
  });
</script>

<main>
  <header>
    <div class="header-left">
      <h1>Hermione</h1>
      <span class="subtitle">Student Observability Platform</span>
    </div>
    <div class="header-right">
      <div class="status" class:connected class:error={connectionStatus === 'Error'}>
        <span class="status-dot"></span>
        {connectionStatus}
      </div>
      <input
        type="text"
        bind:value={serverUrl}
        placeholder="ws://localhost:8080/ws"
        class="server-input"
      />
    </div>
  </header>

  <div class="container">
    {#if Object.keys(sessions).length === 0}
      <div class="empty-state">
        <div class="empty-icon">👀</div>
        <h2>No Active Sessions</h2>
        <p>Waiting for students to connect...</p>
        <div class="instructions">
          <h3>How to connect:</h3>
          <ul>
            <li><strong>VS Code:</strong> Install the Hermione extension and it will auto-connect</li>
            <li><strong>Terminal:</strong> Run <code>hermione-shell</code> to start a monitored shell session</li>
          </ul>
        </div>
      </div>
    {:else}
      <!-- VS Code Sessions -->
      {#if getVSCodeSessions().length > 0}
        <section class="session-section">
          <h2>
            <span class="section-icon">💻</span>
            Editor Sessions ({getVSCodeSessions().length})
          </h2>
          <div class="sessions-grid">
            {#each getVSCodeSessions() as session (session.id)}
              <FileMonitor sessionId={session.id} {session} />
            {/each}
          </div>
        </section>
      {/if}

      <!-- Shell Sessions -->
      {#if getShellSessions().length > 0}
        <section class="session-section">
          <h2>
            <span class="section-icon">🖥️</span>
            Terminal Sessions ({getShellSessions().length})
          </h2>
          <div class="sessions-grid">
            {#each getShellSessions() as session (session.id)}
              <TerminalMonitor
                sessionId={session.id}
                {session}
                buffer={terminalBuffers[session.id] || []}
              />
            {/each}
          </div>
        </section>
      {/if}

      <!-- Disconnected Sessions -->
      {#if getDisconnectedSessions().length > 0}
        <section class="session-section disconnected-section">
          <div class="section-header">
            <h2>
              <span class="section-icon">📴</span>
              Disconnected ({getDisconnectedSessions().length})
            </h2>
            <button class="clear-btn" on:click={clearDisconnected}>Clear All</button>
          </div>
          <div class="disconnected-list">
            {#each getDisconnectedSessions() as session (session.id)}
              <div class="disconnected-item">
                <span class="student-name">{session.student_name || 'Unknown'}</span>
                <span class="session-type">{getSessionType(session)}</span>
                <span class="session-id">{session.id.slice(0, 8)}...</span>
              </div>
            {/each}
          </div>
        </section>
      {/if}
    {/if}
  </div>
</main>

<style>
  :global(*) {
    box-sizing: border-box;
  }

  :global(body) {
    margin: 0;
    padding: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif;
    background: #0f0f0f;
    color: #e0e0e0;
  }

  main {
    min-height: 100vh;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 16px 24px;
    background: #1a1a1a;
    border-bottom: 1px solid #2a2a2a;
    position: sticky;
    top: 0;
    z-index: 100;
  }

  .header-left {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }

  h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    color: #fff;
  }

  .subtitle {
    color: #666;
    font-size: 0.9rem;
  }

  .header-right {
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border-radius: 20px;
    font-size: 0.85rem;
    font-weight: 500;
    background: rgba(220, 53, 69, 0.2);
    color: #dc3545;
  }

  .status.connected {
    background: rgba(40, 167, 69, 0.2);
    color: #28a745;
  }

  .status.error {
    background: rgba(220, 53, 69, 0.2);
    color: #dc3545;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
  }

  .server-input {
    padding: 6px 12px;
    border: 1px solid #333;
    border-radius: 6px;
    background: #1a1a1a;
    color: #e0e0e0;
    font-size: 0.85rem;
    width: 240px;
  }

  .server-input:focus {
    outline: none;
    border-color: #007acc;
  }

  .container {
    max-width: 1400px;
    margin: 0 auto;
    padding: 24px;
  }

  .empty-state {
    text-align: center;
    padding: 80px 20px;
  }

  .empty-icon {
    font-size: 4rem;
    margin-bottom: 16px;
  }

  .empty-state h2 {
    margin: 0 0 8px 0;
    color: #fff;
    font-size: 1.5rem;
  }

  .empty-state p {
    color: #666;
    margin: 0 0 32px 0;
  }

  .instructions {
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 12px;
    padding: 24px;
    max-width: 500px;
    margin: 0 auto;
    text-align: left;
  }

  .instructions h3 {
    margin: 0 0 16px 0;
    color: #fff;
    font-size: 1rem;
  }

  .instructions ul {
    margin: 0;
    padding-left: 20px;
  }

  .instructions li {
    margin-bottom: 12px;
    color: #999;
    line-height: 1.5;
  }

  .instructions code {
    background: #2a2a2a;
    padding: 2px 6px;
    border-radius: 4px;
    font-family: 'Monaco', 'Menlo', monospace;
    font-size: 0.9em;
    color: #e0e0e0;
  }

  .session-section {
    margin-bottom: 32px;
  }

  .session-section h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 16px 0;
    font-size: 1.1rem;
    font-weight: 600;
    color: #fff;
  }

  .section-icon {
    font-size: 1.2em;
  }

  .sessions-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(500px, 1fr));
    gap: 16px;
  }

  .disconnected-section {
    opacity: 0.7;
  }

  .section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
  }

  .section-header h2 {
    margin: 0;
  }

  .clear-btn {
    padding: 6px 12px;
    background: #2a2a2a;
    border: 1px solid #3a3a3a;
    border-radius: 6px;
    color: #999;
    font-size: 0.85rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .clear-btn:hover {
    background: #3a3a3a;
    color: #fff;
  }

  .disconnected-list {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .disconnected-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 8px;
    font-size: 0.85rem;
  }

  .student-name {
    color: #fff;
    font-weight: 500;
  }

  .session-type {
    color: #666;
    text-transform: capitalize;
  }

  .session-id {
    color: #444;
    font-family: monospace;
    font-size: 0.8em;
  }

  @media (max-width: 600px) {
    .sessions-grid {
      grid-template-columns: 1fr;
    }

    header {
      flex-direction: column;
      gap: 12px;
    }

    .header-right {
      width: 100%;
      justify-content: space-between;
    }

    .server-input {
      width: 160px;
    }
  }
</style>
