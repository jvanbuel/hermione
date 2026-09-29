import { randomBytes } from 'crypto';
import * as vscode from 'vscode';

/** What a streamed turn reports as it goes. */
export interface StreamHandlers {
    status: (text: string) => void;
    message: (text: string) => void;
    error: (text: string) => void;
}

/** What the panel needs from the reporter to talk to the backend. */
export interface AssistantClient {
    assistantStatus(): Promise<boolean>;
    assistantHistory(): Promise<{ role: string; body: string }[]>;
    /** Resolves when the turn has ended; reports failures through `on.error`. */
    assistantChatStream(message: string, on: StreamHandlers): Promise<void>;
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
            // Owned by the panel: they go when it does, instead of piling up in
            // the extension's subscriptions once per time the panel is opened.
            const own: vscode.Disposable[] = [];
            panel.onDidDispose(
                () => {
                    panel = undefined;
                    own.forEach((d) => d.dispose());
                },
                null,
                own,
            );
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
                        const post = (m: object) => panel?.webview.postMessage(m);
                        post({ type: 'status', body: 'thinking…' });
                        try {
                            await client.assistantChatStream(text, {
                                status: (t) => post({ type: 'status', body: t }),
                                message: (t) => post({ type: 'message', body: t }),
                                error: (t) => post({ type: 'error', body: t }),
                            });
                        } finally {
                            // The panel disables its input while a turn runs; if
                            // this were skipped it would stay disabled for good.
                            post({ type: 'done' });
                        }
                    }
                },
                undefined,
                own,
            );
        }),
    );
}

function nonce(): string {
    return randomBytes(16).toString('hex');
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
    let statusEl = null;

    function clearEmpty() {
      const e = log.querySelector('.empty');
      if (e) e.remove();
    }
    function addMessage(role, body) {
      clearEmpty();
      const el = document.createElement('div');
      el.className = 'msg ' + role;
      el.textContent = body;
      // Keep the transient status line pinned to the bottom.
      log.insertBefore(el, statusEl);
      log.scrollTop = log.scrollHeight;
      return el;
    }
    function setStatus(text) {
      clearEmpty();
      if (!statusEl) {
        statusEl = document.createElement('div');
        statusEl.className = 'thinking';
        log.appendChild(statusEl);
      }
      statusEl.textContent = text || 'working…';
      log.scrollTop = log.scrollHeight;
    }
    function clearStatus() {
      if (statusEl) { statusEl.remove(); statusEl = null; }
    }
    function setBusy(busy) {
      send.disabled = busy;
      input.disabled = busy;
      if (!busy) input.focus();
    }

    function submit() {
      const text = input.value.trim();
      if (!text || send.disabled) return;
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
      } else if (m.type === 'status') {
        setStatus(m.body);
      } else if (m.type === 'message') {
        addMessage('assistant', m.body);
      } else if (m.type === 'error') {
        addMessage('error', m.body);
      } else if (m.type === 'done') {
        clearStatus(); setBusy(false);
      }
    });

    vscode.postMessage({ type: 'ready' });
    input.focus();
  </script>
</body>
</html>`;
}
