<script>
  export let sessionId;
  export let session;

  function getFileIcon(filename) {
    if (!filename) return '📄';

    const ext = filename.split('.').pop()?.toLowerCase();
    const iconMap = {
      'js': '🟨',
      'ts': '🔷',
      'svelte': '🧡',
      'html': '🔶',
      'css': '🎨',
      'json': '📋',
      'md': '📝',
      'rs': '🦀',
      'py': '🐍',
      'java': '☕',
      'cpp': '⚙️',
      'c': '⚙️',
      'go': '🐹',
      'vue': '💚',
      'react': '⚛️',
      'php': '🐘',
      'rb': '💎',
      'swift': '🍎',
      'kt': '🎯',
      'dart': '🎯',
      'yaml': '📄',
      'yml': '📄',
      'xml': '📄',
      'txt': '📄'
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
    if (parts.length > 3) {
      return '.../' + parts.slice(-3).join('/');
    }
    return filepath;
  }

  function getLanguageFromFile(filename) {
    if (!filename) return '';

    const ext = filename.split('.').pop()?.toLowerCase();
    const langMap = {
      'js': 'javascript',
      'ts': 'typescript',
      'svelte': 'svelte',
      'html': 'html',
      'css': 'css',
      'json': 'json',
      'md': 'markdown',
      'rs': 'rust',
      'py': 'python',
      'java': 'java',
      'cpp': 'cpp',
      'c': 'c',
      'go': 'go',
      'vue': 'vue',
      'php': 'php',
      'rb': 'ruby',
      'swift': 'swift',
      'kt': 'kotlin',
      'dart': 'dart'
    };

    return langMap[ext] || '';
  }
</script>

<div class="session-card">
  <div class="session-header">
    <h3>
      <span class="session-icon">💻</span>
      Session: {sessionId.slice(0, 8)}...
    </h3>
    <div class="file-info">
      <span class="file-icon">{getFileIcon(session.active_file)}</span>
      <div class="file-details">
        <div class="file-name">{formatFileName(session.active_file)}</div>
        <div class="file-path">{formatFilePath(session.active_file)}</div>
      </div>
    </div>
  </div>

  {#if session.file_content}
    <div class="file-content">
      <div class="content-header">
        <span class="language-tag">{getLanguageFromFile(session.active_file)}</span>
        <span class="content-length">{session.file_content.length} characters</span>
      </div>
      <pre><code>{session.file_content}</code></pre>
    </div>
  {:else}
    <div class="no-content">
      <p>File content not available</p>
    </div>
  {/if}
</div>

<style>
  .session-card {
    background: #2a2a2a;
    border: 1px solid #404040;
    border-radius: 12px;
    margin-bottom: 20px;
    overflow: hidden;
    box-shadow: 0 4px 6px rgba(0, 0, 0, 0.3);
  }

  .session-header {
    padding: 20px;
    background: #333;
    border-bottom: 1px solid #404040;
  }

  .session-header h3 {
    margin: 0 0 15px 0;
    color: #fff;
    font-size: 1.2em;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .session-icon {
    font-size: 1.2em;
  }

  .file-info {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .file-icon {
    font-size: 2em;
  }

  .file-details {
    flex: 1;
  }

  .file-name {
    color: #fff;
    font-weight: 600;
    font-size: 1.1em;
    margin-bottom: 4px;
  }

  .file-path {
    color: #888;
    font-size: 0.9em;
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
  }

  .file-content {
    max-height: 400px;
    overflow: hidden;
  }

  .content-header {
    padding: 12px 20px;
    background: #1e1e1e;
    border-bottom: 1px solid #404040;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .language-tag {
    background: #007acc;
    color: white;
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 0.8em;
    font-weight: 500;
    text-transform: uppercase;
  }

  .content-length {
    color: #888;
    font-size: 0.8em;
  }

  pre {
    margin: 0;
    padding: 20px;
    background: #1e1e1e;
    color: #d4d4d4;
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
    font-size: 0.9em;
    line-height: 1.4;
    overflow-x: auto;
    white-space: pre-wrap;
    word-wrap: break-word;
  }

  code {
    color: #d4d4d4;
  }

  .no-content {
    padding: 40px 20px;
    text-align: center;
    color: #666;
  }

  .no-content p {
    margin: 0;
    font-style: italic;
  }
</style>