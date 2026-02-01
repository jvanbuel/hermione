<script>
  export let sessionId;
  export let session;

  let expanded = true;

  function getFileIcon(filename) {
    if (!filename) return '📄';

    const ext = filename.split('.').pop()?.toLowerCase();
    const iconMap = {
      'js': '🟨',
      'jsx': '⚛️',
      'ts': '🔷',
      'tsx': '⚛️',
      'svelte': '🧡',
      'vue': '💚',
      'html': '🔶',
      'css': '🎨',
      'scss': '🎨',
      'less': '🎨',
      'json': '📋',
      'md': '📝',
      'rs': '🦀',
      'py': '🐍',
      'java': '☕',
      'cpp': '⚙️',
      'c': '⚙️',
      'h': '⚙️',
      'go': '🐹',
      'php': '🐘',
      'rb': '💎',
      'swift': '🍎',
      'kt': '🎯',
      'dart': '🎯',
      'yaml': '📄',
      'yml': '📄',
      'xml': '📄',
      'sql': '🗄️',
      'sh': '🐚',
      'bash': '🐚',
      'zsh': '🐚'
    };

    return iconMap[ext] || '📄';
  }

  function formatFileName(filepath) {
    if (!filepath) return 'No file selected';
    return filepath.split('/').pop() || filepath;
  }

  function formatFilePath(filepath) {
    if (!filepath) return '';
    const parts = filepath.split('/');
    if (parts.length > 4) {
      return '.../' + parts.slice(-4).join('/');
    }
    return filepath;
  }

  function getLanguage(filename) {
    if (!filename) return 'plaintext';

    const ext = filename.split('.').pop()?.toLowerCase();
    const langMap = {
      'js': 'javascript',
      'jsx': 'javascript',
      'ts': 'typescript',
      'tsx': 'typescript',
      'svelte': 'svelte',
      'vue': 'vue',
      'html': 'html',
      'css': 'css',
      'scss': 'scss',
      'less': 'less',
      'json': 'json',
      'md': 'markdown',
      'rs': 'rust',
      'py': 'python',
      'java': 'java',
      'cpp': 'cpp',
      'c': 'c',
      'h': 'c',
      'go': 'go',
      'php': 'php',
      'rb': 'ruby',
      'swift': 'swift',
      'kt': 'kotlin',
      'dart': 'dart',
      'yaml': 'yaml',
      'yml': 'yaml',
      'xml': 'xml',
      'sql': 'sql',
      'sh': 'bash',
      'bash': 'bash',
      'zsh': 'bash'
    };

    return langMap[ext] || 'plaintext';
  }

  function formatTimestamp(ts) {
    if (!ts) return '';
    const date = new Date(ts);
    return date.toLocaleTimeString();
  }

  function getLineCount(content) {
    if (!content) return 0;
    return content.split('\n').length;
  }

  function highlightCursorLine(content, cursor) {
    if (!content || !cursor) return content;
    const lines = content.split('\n');
    if (cursor.line < lines.length) {
      lines[cursor.line] = `<span class="cursor-line">${lines[cursor.line]}</span>`;
    }
    return lines.join('\n');
  }
</script>

<div class="card">
  <div class="card-header" on:click={() => expanded = !expanded}>
    <div class="student-info">
      <span class="student-avatar">👤</span>
      <div class="student-details">
        <span class="student-name">{session.student_name || 'Anonymous'}</span>
        <span class="session-id">{sessionId.slice(0, 8)}</span>
      </div>
    </div>

    <div class="file-info">
      <span class="file-icon">{getFileIcon(session.current_file)}</span>
      <div class="file-details">
        <span class="file-name">{formatFileName(session.current_file)}</span>
        <span class="file-path">{formatFilePath(session.current_file)}</span>
      </div>
    </div>

    <div class="card-meta">
      {#if session.cursor_position}
        <span class="cursor-pos">Ln {session.cursor_position.line + 1}, Col {session.cursor_position.column + 1}</span>
      {/if}
      <span class="timestamp">{formatTimestamp(session.last_activity)}</span>
      <span class="expand-icon">{expanded ? '▼' : '▶'}</span>
    </div>
  </div>

  {#if expanded && session.file_content}
    <div class="card-content">
      <div class="content-header">
        <span class="language-badge">{getLanguage(session.current_file)}</span>
        <span class="line-count">{getLineCount(session.file_content)} lines</span>
      </div>
      <pre class="code-block"><code>{session.file_content}</code></pre>
    </div>
  {:else if expanded}
    <div class="card-content empty">
      <p>No file content available</p>
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

  .file-info {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
    padding: 0 16px;
  }

  .file-icon {
    font-size: 1.5rem;
    flex-shrink: 0;
  }

  .file-details {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .file-name {
    color: #fff;
    font-weight: 500;
    font-size: 0.9rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .file-path {
    color: #666;
    font-size: 0.75rem;
    font-family: monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-shrink: 0;
  }

  .cursor-pos {
    color: #888;
    font-size: 0.8rem;
    font-family: monospace;
    background: #2a2a2a;
    padding: 4px 8px;
    border-radius: 4px;
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

  .card-content.empty {
    padding: 32px;
    text-align: center;
    color: #666;
  }

  .card-content.empty p {
    margin: 0;
    font-style: italic;
  }

  .content-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 16px;
    background: #1e1e1e;
    border-bottom: 1px solid #2a2a2a;
  }

  .language-badge {
    background: #007acc;
    color: #fff;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 500;
    text-transform: uppercase;
  }

  .line-count {
    color: #666;
    font-size: 0.8rem;
  }

  .code-block {
    margin: 0;
    padding: 16px;
    background: #1e1e1e;
    color: #d4d4d4;
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', 'Consolas', monospace;
    font-size: 0.85rem;
    line-height: 1.5;
    overflow-x: auto;
    max-height: 400px;
    overflow-y: auto;
  }

  .code-block code {
    white-space: pre;
  }

  :global(.cursor-line) {
    background: rgba(255, 255, 0, 0.1);
    display: block;
  }
</style>
