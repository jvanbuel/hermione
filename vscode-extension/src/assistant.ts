import * as vscode from 'vscode';

/** What the panel needs from the reporter to talk to the backend. */
export interface AssistantClient {
    assistantStatus(): Promise<boolean>;
    assistantHistory(): Promise<{ role: string; body: string }[]>;
    assistantChat(message: string): Promise<string>;
}

let panel: vscode.WebviewPanel | undefined;

/**
 * Registers the "Ask Assistant" command, which opens a chat webview wired to the
 * course's AI teaching assistant. The panel only opens when the course has the
 * assistant enabled, so courses without one are unaffected.
 */
export function registerAssistant(context: vscode.ExtensionContext, client: AssistantClient): void {
    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.askAssistant', async () => {
            if (!(await client.assistantStatus())) {
                vscode.window.showInformationMessage(
                    'The Hermione AI assistant is not enabled for this course.',
                );
                return;
            }
            if (panel) {
                panel.reveal(vscode.ViewColumn.Beside);
                return;
            }
            panel = vscode.window.createWebviewPanel(
                'hermioneAssistant',
                'Hermione Assistant',
                vscode.ViewColumn.Beside,
                { enableScripts: true, retainContextWhenHidden: true },
            );
            panel.onDidDispose(() => (panel = undefined), null, context.subscriptions);
            panel.webview.html = chatHtml(panel.webview);
            panel.webview.onDidReceiveMessage(
                async (msg) => {
                    if (msg?.type === 'ready') {
                        const messages = await client.assistantHistory();
                        panel?.webview.postMessage({ type: 'history', messages });
                    } else if (msg?.type === 'send') {
                        const text = String(msg.text || '').trim();
                        if (!text) {
                            return;
                        }
                        panel?.webview.postMessage({ type: 'thinking' });
                        try {
                            const reply = await client.assistantChat(text);
                            panel?.webview.postMessage({ type: 'reply', body: reply });
                        } catch (e) {
                            const message = e instanceof Error ? e.message : 'Request failed';
                            panel?.webview.postMessage({ type: 'error', body: message });
                        }
                    }
                },
                undefined,
                context.subscriptions,
            );
        }),
    );
}

function nonce(): string {
    let s = '';
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
    for (let i = 0; i < 32; i++) {
        s += chars.charAt(Math.floor(Math.random() * chars.length));
    }
    return s;
}

function chatHtml(webview: vscode.Webview): string {
    const n = nonce();
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta http-equiv="Content-Security-Policy"
    content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${n}';" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <style>
    body { margin: 0; display: flex; flex-direction: column; height: 100vh;
      font-family: var(--vscode-font-family); font-size: var(--vscode-font-size);
      color: var(--vscode-foreground); background: var(--vscode-editor-background); }
    #log { flex: 1 1 auto; overflow-y: auto; padding: 12px; display: flex; flex-direction: column; gap: 10px; }
    .msg { max-width: 92%; padding: 8px 11px; border-radius: 10px; white-space: pre-wrap; word-wrap: break-word; line-height: 1.4; }
    .msg.student { align-self: flex-end; background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
    .msg.assistant { align-self: flex-start; background: var(--vscode-input-background); border: 1px solid var(--vscode-input-border, transparent); }
    .msg.error { align-self: flex-start; background: var(--vscode-inputValidation-errorBackground, #5a1d1d);
      border: 1px solid var(--vscode-inputValidation-errorBorder, #be1100); }
    .empty { margin: auto; color: var(--vscode-descriptionForeground); text-align: center; padding: 24px; }
    .thinking { align-self: flex-start; color: var(--vscode-descriptionForeground); font-style: italic; }
    #bar { flex: 0 0 auto; display: flex; gap: 8px; padding: 10px; border-top: 1px solid var(--vscode-panel-border, transparent); }
    #input { flex: 1; resize: none; min-height: 38px; max-height: 160px; padding: 8px 10px; border-radius: 8px;
      font: inherit; color: var(--vscode-input-foreground); background: var(--vscode-input-background);
      border: 1px solid var(--vscode-input-border, var(--vscode-contrastBorder, #555)); }
    #send { padding: 0 14px; border: none; border-radius: 8px; cursor: pointer;
      background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
    #send:disabled { opacity: .5; cursor: not-allowed; }
  </style>
</head>
<body>
  <div id="log"><div class="empty">Ask the teaching assistant about your code or the current exercise.</div></div>
  <div id="bar">
    <textarea id="input" placeholder="Ask a question…  (Enter to send, Shift+Enter for a new line)" rows="1"></textarea>
    <button id="send" type="button">Send</button>
  </div>
  <script nonce="${n}">
    const vscode = acquireVsCodeApi();
    const log = document.getElementById('log');
    const input = document.getElementById('input');
    const send = document.getElementById('send');
    let thinkingEl = null;

    function clearEmpty() {
      const e = log.querySelector('.empty');
      if (e) e.remove();
    }
    function addMessage(role, body) {
      clearEmpty();
      const el = document.createElement('div');
      el.className = 'msg ' + role;
      el.textContent = body;
      log.appendChild(el);
      log.scrollTop = log.scrollHeight;
      return el;
    }
    function setThinking(on) {
      if (on) {
        clearEmpty();
        thinkingEl = document.createElement('div');
        thinkingEl.className = 'thinking';
        thinkingEl.textContent = 'Assistant is thinking…';
        log.appendChild(thinkingEl);
        log.scrollTop = log.scrollHeight;
      } else if (thinkingEl) {
        thinkingEl.remove();
        thinkingEl = null;
      }
    }
    function setBusy(busy) {
      send.disabled = busy;
      input.disabled = busy;
      if (!busy) input.focus();
    }

    function submit() {
      const text = input.value.trim();
      if (!text) return;
      addMessage('student', text);
      input.value = '';
      setBusy(true);
      vscode.postMessage({ type: 'send', text });
    }
    send.addEventListener('click', submit);
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit(); }
    });

    window.addEventListener('message', (event) => {
      const m = event.data;
      if (m.type === 'history') {
        for (const msg of m.messages || []) addMessage(msg.role === 'student' ? 'student' : 'assistant', msg.body);
      } else if (m.type === 'thinking') {
        setThinking(true);
      } else if (m.type === 'reply') {
        setThinking(false); addMessage('assistant', m.body); setBusy(false);
      } else if (m.type === 'error') {
        setThinking(false); addMessage('error', m.body); setBusy(false);
      }
    });

    vscode.postMessage({ type: 'ready' });
    input.focus();
  </script>
</body>
</html>`;
}
