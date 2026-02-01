<script>
  import { afterUpdate } from 'svelte';

  export let sessionId;
  export let session;
  export let buffer = [];

  let expanded = true;
  let autoScroll = true;
  let terminalEl;

  afterUpdate(() => {
    if (autoScroll && terminalEl) {
      terminalEl.scrollTop = terminalEl.scrollHeight;
    }
  });

  function formatTimestamp(ts) {
    if (!ts) return '';
    const date = new Date(ts);
    return date.toLocaleTimeString();
  }

  function getDisplayContent() {
    // Combine buffer entries into displayable content
    return buffer.map(entry => entry.content).join('');
  }

  function clearBuffer() {
    buffer = [];
  }

  function toggleAutoScroll() {
    autoScroll = !autoScroll;
  }

  function handleScroll() {
    if (!terminalEl) return;
    const isAtBottom = terminalEl.scrollHeight - terminalEl.scrollTop <= terminalEl.clientHeight + 50;
    if (!isAtBottom) {
      autoScroll = false;
    }
  }
</script>

<div class="card">
  <div class="card-header" on:click={() => expanded = !expanded}>
    <div class="student-info">
      <span class="student-avatar">🖥️</span>
      <div class="student-details">
        <span class="student-name">{session.student_name || 'Anonymous'}</span>
        <span class="session-id">{sessionId.slice(0, 8)}</span>
      </div>
    </div>

    <div class="terminal-stats">
      <span class="stat">
        <span class="stat-label">Lines:</span>
        <span class="stat-value">{buffer.length}</span>
      </span>
    </div>

    <div class="card-meta">
      <span class="timestamp">{formatTimestamp(session.last_activity)}</span>
      <span class="expand-icon">{expanded ? '▼' : '▶'}</span>
    </div>
  </div>

  {#if expanded}
    <div class="card-content">
      <div class="terminal-toolbar">
        <div class="toolbar-left">
          <span class="terminal-badge">Terminal</span>
        </div>
        <div class="toolbar-right">
          <button
            class="toolbar-btn"
            class:active={autoScroll}
            on:click|stopPropagation={toggleAutoScroll}
            title={autoScroll ? 'Auto-scroll enabled' : 'Auto-scroll disabled'}
          >
            {autoScroll ? '📜 Auto' : '📜 Manual'}
          </button>
          <button
            class="toolbar-btn"
            on:click|stopPropagation={clearBuffer}
            title="Clear terminal history"
          >
            🗑️ Clear
          </button>
        </div>
      </div>

      <div
        class="terminal"
        bind:this={terminalEl}
        on:scroll={handleScroll}
      >
        {#if buffer.length === 0}
          <div class="terminal-empty">
            <p>Waiting for terminal output...</p>
          </div>
        {:else}
          <pre class="terminal-content">{getDisplayContent()}</pre>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .card {
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 12px;
    overflow: hidden;
    transition: border-color 0.2s;
  }

  .card:hover {
    border-color: #3a3a3a;
  }

  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px;
    background: #222;
    cursor: pointer;
    user-select: none;
  }

  .card-header:hover {
    background: #282828;
  }

  .student-info {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 150px;
  }

  .student-avatar {
    font-size: 1.5rem;
  }

  .student-details {
    display: flex;
    flex-direction: column;
  }

  .student-name {
    color: #fff;
    font-weight: 600;
    font-size: 0.95rem;
  }

  .session-id {
    color: #666;
    font-size: 0.75rem;
    font-family: monospace;
  }

  .terminal-stats {
    display: flex;
    gap: 16px;
    flex: 1;
    justify-content: center;
  }

  .stat {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .stat-label {
    color: #666;
    font-size: 0.8rem;
  }

  .stat-value {
    color: #fff;
    font-size: 0.9rem;
    font-weight: 500;
    font-family: monospace;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-shrink: 0;
  }

  .timestamp {
    color: #666;
    font-size: 0.8rem;
  }

  .expand-icon {
    color: #666;
    font-size: 0.7rem;
  }

  .card-content {
    border-top: 1px solid #2a2a2a;
  }

  .terminal-toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 16px;
    background: #1e1e1e;
    border-bottom: 1px solid #2a2a2a;
  }

  .toolbar-left {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .toolbar-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .terminal-badge {
    background: #28a745;
    color: #fff;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 500;
    text-transform: uppercase;
  }

  .toolbar-btn {
    padding: 4px 8px;
    background: #2a2a2a;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    color: #999;
    font-size: 0.75rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .toolbar-btn:hover {
    background: #3a3a3a;
    color: #fff;
  }

  .toolbar-btn.active {
    background: #007acc;
    border-color: #007acc;
    color: #fff;
  }

  .terminal {
    background: #0d0d0d;
    height: 300px;
    overflow-y: auto;
    overflow-x: auto;
  }

  .terminal-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: #666;
  }

  .terminal-empty p {
    margin: 0;
    font-style: italic;
  }

  .terminal-content {
    margin: 0;
    padding: 12px 16px;
    color: #33ff33;
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', 'Consolas', monospace;
    font-size: 0.85rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-all;
  }

  /* Scrollbar styling */
  .terminal::-webkit-scrollbar {
    width: 8px;
    height: 8px;
  }

  .terminal::-webkit-scrollbar-track {
    background: #1a1a1a;
  }

  .terminal::-webkit-scrollbar-thumb {
    background: #3a3a3a;
    border-radius: 4px;
  }

  .terminal::-webkit-scrollbar-thumb:hover {
    background: #4a4a4a;
  }
</style>
