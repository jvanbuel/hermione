import { execSync } from 'child_process';
import * as os from 'os';
import * as vscode from 'vscode';
import WebSocket from 'ws';
import { ExerciseMap } from './exercises';

interface CourseConfig {
    backend?: string;
    token?: string;
    identity?: string;
    authProvider?: string;
}

/** Reads the course config from the workspace's `.hermione.json`. */
async function readCourseConfig(): Promise<CourseConfig> {
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
        try {
            const uri = vscode.Uri.joinPath(folder.uri, '.hermione.json');
            const bytes = await vscode.workspace.fs.readFile(uri);
            const cfg = JSON.parse(Buffer.from(bytes).toString('utf8'));
            return {
                backend: cfg.backend,
                token: cfg.token,
                identity: cfg.identity,
                authProvider: cfg.authProvider,
            };
        } catch (_) {
            // try the next folder
        }
    }
    return {};
}

interface FileEvent {
    student: string;
    studentSource?: string;
    repo?: string;
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
    private studentSource = '';
    private repo = '';
    private identityToken = '';
    private authProvider = '';
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
        await this.loadConfig();
        await this.exercises.load();
        this.enabled = true;

        this.disposables.push(
            vscode.window.onDidChangeActiveTextEditor((editor) => this.onFocus(editor)),
            vscode.window.onDidChangeWindowState((s) => {
                this.windowFocused = s.focused;
            }),
            vscode.workspace.onDidCloseTextDocument((doc) => this.onClose(doc)),
            vscode.workspace.onDidChangeWorkspaceFolders(async () => {
                await this.loadConfig();
                await this.exercises.load();
            }),
            vscode.workspace.onDidChangeConfiguration(async (e) => {
                if (e.affectsConfiguration('hermione')) {
                    await this.loadConfig();
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
            prompt: 'Override the student identifier reported to Hermione',
            value: this.student,
        });
        if (value !== undefined) {
            await vscode.workspace
                .getConfiguration('hermione')
                .update('student', value, vscode.ConfigurationTarget.Global);
            await this.loadConfig();
            this.updateStatusBar();
        }
    }

    /**
     * Loads configuration. The committed course file (`.hermione.json`) is the
     * single source of truth a teacher controls — it can carry `backend`,
     * `token`, and an `identity` source — falling back to VSCode settings/env.
     */
    private async loadConfig(): Promise<void> {
        const cfg = vscode.workspace.getConfiguration('hermione');
        const course = await readCourseConfig();
        this.serverUrl = (course.backend || cfg.get<string>('serverUrl') || 'http://localhost:8080')
            .replace(/\/$/, '');
        this.heartbeatSeconds = Math.max(5, cfg.get<number>('heartbeatSeconds') ?? 15);
        this.token = course.token || cfg.get<string>('token') || process.env.HERMIONE_TOKEN || '';
        const id = this.resolveStudent(course.identity);
        this.student = id.value;
        this.studentSource = id.source;
        this.repo = this.resolveRepo();
        this.authProvider = course.authProvider || 'github';
        await this.ensureIdentity(false);
    }

    private identityKey(): string {
        return `hermione.identity:${this.serverUrl}:${this.authProvider}`;
    }

    /**
     * Obtains a Hermione identity token via VSCode's GitHub auth (silent in
     * Codespaces) exchanged at the backend. Cached until expiry; `interactive`
     * triggers a sign-in prompt when needed.
     */
    private async ensureIdentity(interactive: boolean): Promise<void> {
        const cached = this.context.globalState.get<{ token: string; exp: number }>(this.identityKey());
        if (cached && cached.exp > Date.now() / 1000 + 60) {
            this.identityToken = cached.token;
            return;
        }
        try {
            const session = await vscode.authentication.getSession(
                'github',
                ['read:user'],
                interactive ? { createIfNone: true } : { silent: true },
            );
            if (!session) {
                return;
            }
            const res = await fetch(`${this.serverUrl}/api/auth/exchange`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ provider: this.authProvider, token: session.accessToken }),
            });
            if (!res.ok) {
                return;
            }
            const data = (await res.json()) as { identityToken: string; expiresIn: number };
            this.identityToken = data.identityToken;
            await this.context.globalState.update(this.identityKey(), {
                token: data.identityToken,
                exp: Math.floor(Date.now() / 1000) + (data.expiresIn || 28800),
            });
        } catch (_) {
            // identity unavailable; ingest will 401 if the backend enforces it
        }
    }

    /**
     * Derives the student identity from the container environment — no login —
     * along with which signal produced it (provenance). The auto path does NOT
     * fall back to the OS username (it collides in shared devcontainers); an
     * unresolved identity is reported as "unknown" so it's visible.
     */
    private resolveStudent(pref?: string): { value: string; source: string } {
        const cfg = vscode.workspace.getConfiguration('hermione');
        const explicit = cfg.get<string>('student') || process.env.HERMIONE_STUDENT || '';
        const ghUser = process.env.GITHUB_USER || '';
        const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        const gitEmail = (): string => {
            try {
                return execSync('git config user.email', { cwd, encoding: 'utf8' }).trim();
            } catch (_) {
                return '';
            }
        };
        const pick = (val: string, src: string) => (val ? { value: val, source: src } : null);
        let r: { value: string; source: string } | null;
        switch (pref) {
            case 'github': r = pick(ghUser, 'github') || pick(explicit, 'config'); break;
            case 'git-email': r = pick(gitEmail(), 'git-email') || pick(explicit, 'config'); break;
            case 'env': r = pick(explicit, 'config'); break;
            case 'os': r = pick(os.userInfo().username, 'os'); break;
            default:
                r = pick(explicit, 'config') || pick(ghUser, 'github') || pick(gitEmail(), 'git-email');
        }
        return r || { value: 'unknown', source: 'unknown' };
    }

    /** The repo (owner/name) the activity comes from, for identity provenance. */
    private resolveRepo(): string {
        const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        try {
            const url = execSync('git config --get remote.origin.url', { cwd, encoding: 'utf8' }).trim();
            const m = url.match(/[:/]([^/]+\/[^/]+?)(?:\.git)?$/);
            return m ? m[1] : '';
        } catch (_) {
            return '';
        }
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
            studentSource: this.studentSource || undefined,
            repo: this.repo || undefined,
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
            if (this.identityToken) {
                headers['X-Hermione-Identity'] = this.identityToken;
            }
            const res = await fetch(`${this.serverUrl}/api/file-events`, {
                method: 'POST',
                headers,
                body: JSON.stringify(batch),
            });
            if (res.status === 401) {
                // Backend requires a verified identity — sign in, then retry.
                await this.ensureIdentity(true);
                throw new Error('identity required');
            }
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
