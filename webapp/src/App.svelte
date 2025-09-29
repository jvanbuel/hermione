<script>
  import { onMount, onDestroy } from 'svelte';
  import FileMonitor from './FileMonitor.svelte';

  let ws = null;
  let connected = false;
  let sessions = {};
  let connectionStatus = 'Disconnected';

  function connectWebSocket() {
    try {
      ws = new WebSocket('ws://localhost:8080');

      ws.onopen = () => {
        console.log('Connected to WebSocket server');
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
          console.log('Received message:', message);

          switch (message.type) {
            case 'file_updated':
              sessions[message.session_id] = {
                active_file: message.active_file,
                file_content: message.file_content
              };
              sessions = sessions; // Trigger reactivity
              break;
            case 'error':
              console.error('Server error:', message.message);
              break;
          }
        } catch (e) {
          console.error('Failed to parse message:', e);
        }
      };

      ws.onclose = () => {
        console.log('WebSocket connection closed');
        connected = false;
        connectionStatus = 'Disconnected';

        // Attempt to reconnect after 3 seconds
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
    <h1>🔮 Hermione - File Monitor</h1>
    <div class="status" class:connected class:error={connectionStatus === 'Error'}>
      Status: {connectionStatus}
    </div>
  </header>

  <div class="container">
    {#if Object.keys(sessions).length === 0}
      <div class="no-sessions">
        <p>No active sessions. Start VSCode with the Hermione extension to see file activity.</p>
      </div>
    {:else}
      <div class="sessions">
        <h2>Active Sessions</h2>
        {#each Object.entries(sessions) as [sessionId, session]}
          <FileMonitor {sessionId} {session} />
        {/each}
      </div>
    {/if}
  </div>
</main>

<style>
  :global(body) {
    margin: 0;
    padding: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    background: #1a1a1a;
    color: #ffffff;
  }

  main {
    min-height: 100vh;
    padding: 20px;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 20px 0;
    border-bottom: 1px solid #333;
    margin-bottom: 30px;
  }

  h1 {
    margin: 0;
    color: #fff;
    font-size: 2em;
  }

  .status {
    padding: 8px 16px;
    border-radius: 20px;
    background: #dc3545;
    color: white;
    font-weight: 500;
    font-size: 0.9em;
  }

  .status.connected {
    background: #28a745;
  }

  .status.error {
    background: #dc3545;
  }

  .container {
    max-width: 1200px;
    margin: 0 auto;
  }

  .no-sessions {
    text-align: center;
    padding: 60px 20px;
    color: #888;
  }

  .no-sessions p {
    font-size: 1.1em;
    margin: 0;
  }

  .sessions h2 {
    color: #fff;
    margin-bottom: 20px;
    font-size: 1.5em;
  }
</style>