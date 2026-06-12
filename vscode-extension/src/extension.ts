import * as os from 'os';
import * as vscode from 'vscode';
import WebSocket from 'ws';
import { ExerciseMap } from './exercises';

interface FileEvent {
    student: string;
    workspace?: string;
    path: string;
    relativePath?: string;
    language?: string;
    exercise?: string;
    kind: 'focus' | 'heartbeat' | 'close';
    atUnixMs: number;
}

/**
 * Watches which file the student has active and reports it to the Hermione
 * backend — on focus changes and via periodic heartbeats (so we can measure
 * time-on-task). Events are queued and flushed with retry so transient backend
 * outages don't lose data or disrupt the editor.
 */
class Reporter {
    private enabled = false;
    private student = '';
    private serverUrl = '';
    private heartbeatSeconds = 15;
    private token = '';

    private exercises = new ExerciseMap();
    private queue: FileEvent[] = [];
    private heartbeatTimer?: NodeJS.Timeout;
    private flushTimer?: NodeJS.Timeout;
    private socket?: WebSocket;
    private reconnectTimer?: NodeJS.Timeout;
    private lastMessageId = 0;
    private windowFocused = true;
    private statusBar: vscode.StatusBarItem;
    private disposables: vscode.Disposable[] = [];

    constructor(private context: vscode.ExtensionContext) {
        this.statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
        this.statusBar.command = 'hermione.setStudent';
        context.subscriptions.push(this.statusBar);
    }

    async start(): Promise<void> {
        this.readConfig();
        await this.exercises.load();
        this.enabled = true;

        this.disposables.push(
            vscode.window.onDidChangeActiveTextEditor((editor) => this.onFocus(editor)),
            vscode.window.onDidChangeWindowState((s) => {
                this.windowFocused = s.focused;
            }),
            vscode.workspace.onDidCloseTextDocument((doc) => this.onClose(doc)),
            vscode.workspace.onDidChangeWorkspaceFolders(() => this.exercises.load()),
            vscode.workspace.onDidChangeConfiguration((e) => {
                if (e.affectsConfiguration('hermione')) {
                    this.readConfig();
                    this.restartHeartbeat();
                }
            }),
        );

        this.restartHeartbeat();
        this.onFocus(vscode.window.activeTextEditor); // report current file immediately

        // Surface teacher broadcasts for this course over a live WebSocket.
        this.lastMessageId = this.context.globalState.get(this.messageKey(), 0);
        this.connectMessages();

        this.updateStatusBar();
    }

    stop(): void {
        this.enabled = false;
        if (this.heartbeatTimer) {
            clearInterval(this.heartbeatTimer);
        }
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = undefined;
        }
        if (this.socket) {
            try {
                this.socket.close();
            } catch (_) {
                // already closing
            }
            this.socket = undefined;
        }
        this.disposables.forEach((d) => d.dispose());
        this.disposables = [];
        this.updateStatusBar();
    }

    private messageKey(): string {
        return `hermione.lastMessageId:${this.serverUrl}`;
    }

    /**
     * Opens the message WebSocket. On connect the server replays anything
     * missed (via `since`), then pushes new broadcasts live; we reconnect with a
     * fixed backoff if the connection drops.
     */
    private connectMessages(): void {
        if (!this.enabled) {
            return;
        }
        const base = this.serverUrl.replace(/^http/, 'ws');
        const params = new URLSearchParams();
        if (this.token) {
            params.set('token', this.token);
        }
        params.set('since', String(this.lastMessageId));

        let ws: WebSocket;
        try {
            ws = new WebSocket(`${base}/ws?${params.toString()}`);
        } catch (_) {
            this.scheduleReconnect();
            return;
        }
        this.socket = ws;

        ws.on('message', (data: WebSocket.RawData) => {
            try {
                const m = JSON.parse(data.toString()) as { id: number; body: string };
                if (m && m.body) {
                    vscode.window.showInformationMessage(`📣 ${m.body}`);
                    if (m.id > this.lastMessageId) {
                        this.lastMessageId = m.id;
                        this.context.globalState.update(this.messageKey(), this.lastMessageId);
                    }
                }
            } catch (_) {
                // ignore malformed frames
            }
        });
        ws.on('close', () => {
            this.socket = undefined;
            this.scheduleReconnect();
        });
        ws.on('error', () => {
            try {
                ws.close();
            } catch (_) {
                // already closing
            }
        });
    }

    private scheduleReconnect(): void {
        if (this.reconnectTimer || !this.enabled) {
            return;
        }
        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = undefined;
            this.connectMessages();
        }, 5000);
    }

    async setStudent(): Promise<void> {
        const value = await vscode.window.showInputBox({
            prompt: 'Student identifier reported to Hermione',
            value: this.student,
        });
        if (value !== undefined) {
            await vscode.workspace
                .getConfiguration('hermione')
                .update('student', value, vscode.ConfigurationTarget.Global);
            this.readConfig();
            this.updateStatusBar();
        }
    }

    private readConfig(): void {
        const cfg = vscode.workspace.getConfiguration('hermione');
        this.serverUrl = (cfg.get<string>('serverUrl') || 'http://localhost:8080').replace(/\/$/, '');
        this.heartbeatSeconds = Math.max(5, cfg.get<number>('heartbeatSeconds') ?? 15);
        this.student =
            cfg.get<string>('student') ||
            process.env.HERMIONE_STUDENT ||
            os.userInfo().username ||
            'unknown';
        this.token = cfg.get<string>('token') || process.env.HERMIONE_TOKEN || '';
    }

    private restartHeartbeat(): void {
        if (this.heartbeatTimer) {
            clearInterval(this.heartbeatTimer);
        }
        this.heartbeatTimer = setInterval(() => {
            if (this.enabled && this.windowFocused) {
                this.report('heartbeat', vscode.window.activeTextEditor);
            }
        }, this.heartbeatSeconds * 1000);
    }

    private onFocus(editor: vscode.TextEditor | undefined): void {
        if (this.enabled) {
            this.report('focus', editor);
            this.updateStatusBar();
        }
    }

    private onClose(doc: vscode.TextDocument): void {
        if (this.enabled && doc.uri.scheme === 'file') {
            this.enqueue(this.buildEvent('close', doc, undefined));
        }
    }

    private report(kind: 'focus' | 'heartbeat', editor: vscode.TextEditor | undefined): void {
        if (!editor || editor.document.uri.scheme !== 'file') {
            return;
        }
        this.enqueue(this.buildEvent(kind, editor.document, editor.document.languageId));
    }

    private buildEvent(
        kind: FileEvent['kind'],
        doc: vscode.TextDocument,
        language: string | undefined,
    ): FileEvent {
        const uri = doc.uri;
        const folder = vscode.workspace.getWorkspaceFolder(uri);
        const relativePath = vscode.workspace.asRelativePath(uri, false);
        return {
            student: this.student,
            workspace: folder?.name,
            path: uri.fsPath,
            relativePath,
            language,
            exercise: this.exercises.resolve(relativePath),
            kind,
            atUnixMs: Date.now(),
        };
    }

    private enqueue(event: FileEvent): void {
        this.queue.push(event);
        // Debounce: coalesce bursts (e.g. rapid focus changes) into one POST.
        if (!this.flushTimer) {
            this.flushTimer = setTimeout(() => this.flush(), 250);
        }
    }

    private async flush(): Promise<void> {
        this.flushTimer = undefined;
        if (this.queue.length === 0) {
            return;
        }
        const batch = this.queue;
        this.queue = [];
        try {
            const headers: Record<string, string> = { 'Content-Type': 'application/json' };
            if (this.token) {
                headers['Authorization'] = `Bearer ${this.token}`;
            }
            const res = await fetch(`${this.serverUrl}/api/file-events`, {
                method: 'POST',
                headers,
                body: JSON.stringify(batch),
            });
            if (!res.ok) {
                throw new Error(`HTTP ${res.status}`);
            }
        } catch {
            // Re-queue and retry on the next tick so we don't lose events.
            this.queue = batch.concat(this.queue);
            if (!this.flushTimer) {
                this.flushTimer = setTimeout(() => this.flush(), 5000);
            }
        }
    }

    private updateStatusBar(): void {
        if (!this.enabled) {
            this.statusBar.text = '$(circle-slash) Hermione: off';
            this.statusBar.tooltip = 'Hermione reporting is stopped';
            this.statusBar.show();
            return;
        }
        const editor = vscode.window.activeTextEditor;
        const rel = editor ? vscode.workspace.asRelativePath(editor.document.uri, false) : undefined;
        const exercise = rel ? this.exercises.resolve(rel) : undefined;
        const suffix = exercise ? ` · ${exercise}` : '';
        this.statusBar.text = `$(eye) Hermione: ${this.student}${suffix}`;
        this.statusBar.tooltip = `Reporting file activity as "${this.student}" to ${this.serverUrl}`;
        this.statusBar.show();
    }
}

export function activate(context: vscode.ExtensionContext): void {
    const reporter = new Reporter(context);

    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.start', () => reporter.start()),
        vscode.commands.registerCommand('hermione.stop', () => reporter.stop()),
        vscode.commands.registerCommand('hermione.setStudent', () => reporter.setStudent()),
        { dispose: () => reporter.stop() },
    );

    if (vscode.workspace.getConfiguration('hermione').get<boolean>('enabled', true)) {
        reporter.start();
    }
}

export function deactivate(): void {
    // Reporter is disposed via context.subscriptions.
}
