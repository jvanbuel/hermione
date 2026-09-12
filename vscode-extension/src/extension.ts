import { execSync } from 'child_process';
import * as os from 'os';
import * as vscode from 'vscode';
import WebSocket from 'ws';
import { registerAssistant } from './assistant';
import { ExerciseMap } from './exercises';
import { buildSnapshot, FileSnapshot, SnapshotTarget } from './snapshot';

interface CourseConfig {
    backend?: string;
    token?: string;
    identity?: string;
    authProvider?: string;
    shareFileContents?: boolean;
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
                shareFileContents: cfg.shareFileContents,
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
    kind: 'focus' | 'heartbeat' | 'close' | 'edit';
    /** Document changes coalesced into this event ('edit' only). */
    edits?: number;
    /** 1-based cursor line, when the file was the one on screen. */
    line?: number;
    atUnixMs: number;
}

/**
 * Individual keystrokes are far too noisy to report, so document changes are
 * counted and flushed as one 'edit' event per window.
 */
const EDIT_WINDOW_MS = 5000;

/**
 * How long one snapshot request keeps this editor in "a teacher is looking"
 * mode. The backend re-asks while the teacher's pane is open, so this only has
 * to outlast the gap between two polls; when the pane closes, the requests stop
 * and the window lapses on its own.
 */
const WATCH_WINDOW_MS = 10_000;

/**
 * While watched, a focus change or a typing burst pushes a fresh snapshot
 * rather than waiting for the next request, so the teacher's view tracks the
 * student. Coalesced over this window so a fast typist sends a few per second
 * at most.
 */
const SNAPSHOT_DEBOUNCE_MS = 400;

/**
 * Documents worth reporting. A notebook's cells are separate TextDocuments
 * under the `vscode-notebook-cell` scheme, so a course taught in notebooks
 * reports nothing at all if only `file` counts. Every cell URI carries the
 * notebook's own path (the cell id lives in the fragment), so `fsPath` and
 * `asRelativePath` already collapse the cells of one notebook onto one file,
 * and exercise matching keeps working untouched.
 */
function tracked(doc: vscode.TextDocument): boolean {
    return doc.uri.scheme === 'file' || doc.uri.scheme === 'vscode-notebook-cell';
}

/**
 * The `file:` URI a document belongs to.
 *
 * A cell's URI keeps the notebook's path but carries the
 * `vscode-notebook-cell` scheme, and both `getWorkspaceFolder` and
 * `asRelativePath` match on scheme: they see no workspace folder and hand back
 * an absolute path. That reported `/workspace/ex1/nb.ipynb` instead of
 * `ex1/nb.ipynb`, so nothing matched the exercise globs and every notebook
 * event landed with no exercise at all.
 */
function fileUri(uri: vscode.Uri): vscode.Uri {
    if (uri.scheme === 'file') {
        return uri;
    }
    // Prefer the notebook's own URI. Rebuilding one keeps the cell's authority,
    // and in a Codespace that authority is the machine name — `fsPath` then
    // renders it UNC-style as `//codespaces+name/workspace/...`, which matches
    // no workspace folder either. The real document carries whatever scheme and
    // authority this window actually uses.
    const notebook = vscode.workspace.notebookDocuments.find(nb => nb.uri.path === uri.path);
    return notebook ? notebook.uri : uri.with({ scheme: 'file', authority: '', fragment: '' });
}

/**
 * Watches which file the student has active and reports it to the Hermione
 * backend — on focus changes, via periodic heartbeats (so we can measure
 * time-on-task), and on edit bursts (so time-on-task can be told apart from
 * time-stuck). Events are queued and flushed with retry so transient backend
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

    /** Path last reported as focused, to suppress same-file focus churn. */
    private focusedPath = '';

    private exercises = new ExerciseMap();
    private queue: FileEvent[] = [];
    private pendingEdits = new Map<
        string,
        { doc: vscode.TextDocument; count: number; at: number }
    >();
    private heartbeatTimer?: NodeJS.Timeout;
    private flushTimer?: NodeJS.Timeout;
    private editTimer?: NodeJS.Timeout;
    private socket?: WebSocket;
    private reconnectTimer?: NodeJS.Timeout;
    private lastMessageId = 0;
    private windowFocused = true;

    /** Whether this student's config permits sharing file contents at all. */
    private shareFileContents = true;
    /** Set while a teacher is known to be looking; its existence *is* `watched`. */
    private watchTimer?: NodeJS.Timeout;
    private snapshotTimer?: NodeJS.Timeout;
    private snapshotSending = false;

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
            vscode.window.onDidChangeActiveTextEditor(() => this.onFocus()),
            // A notebook can gain focus without any cell entering edit mode,
            // which fires no text-editor event.
            vscode.window.onDidChangeActiveNotebookEditor(() => this.onFocus()),
            vscode.window.onDidChangeWindowState((s) => {
                this.windowFocused = s.focused;
            }),
            vscode.workspace.onDidChangeTextDocument((e) => this.onEdit(e)),
            // Moving the cursor changes nothing worth reporting as activity,
            // but it is most of what a teacher watching the file wants to see.
            vscode.window.onDidChangeTextEditorSelection(() => this.pushSnapshot()),
            vscode.workspace.onDidCloseTextDocument((doc) => this.onClose(doc)),
            vscode.workspace.onDidCloseNotebookDocument((nb) => this.onCloseNotebook(nb)),
            vscode.workspace.onDidChangeWorkspaceFolders(async () => {
                await this.loadConfig();
                await this.exercises.load();
            }),
            vscode.workspace.onDidChangeConfiguration(async (e) => {
                if (e.affectsConfiguration('hermione')) {
                    const before = this.student;
                    await this.loadConfig();
                    this.restartHeartbeat();
                    // Snapshot requests are routed by student name, so a
                    // renamed identity needs a fresh socket — the open one is
                    // still subscribed under the name we connected with.
                    if (this.student !== before) {
                        this.reconnectMessages();
                    }
                }
            }),
        );

        this.restartHeartbeat();
        this.onFocus(); // report current file immediately

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
        if (this.editTimer) {
            clearTimeout(this.editTimer);
            this.editTimer = undefined;
        }
        this.pendingEdits.clear();
        if (this.snapshotTimer) {
            clearTimeout(this.snapshotTimer);
            this.snapshotTimer = undefined;
        }
        if (this.watchTimer) {
            clearTimeout(this.watchTimer);
            this.watchTimer = undefined;
        }
        // start() reports the current file immediately; leaving this set would
        // suppress exactly that.
        this.focusedPath = '';
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
        // Lets the backend route snapshot requests to this editor alone rather
        // than to the whole course. Routing only: the snapshot that goes back
        // is attributed by the identity the backend verifies on ingest.
        if (this.student) {
            params.set('student', this.student);
        }

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
                const m = JSON.parse(data.toString()) as { id: number; body: string; kind?: string };
                // Control frames carry a `kind` and no body: the backend asking
                // for what's on screen because a teacher opened this student's
                // file. Frames are addressed per student, so anything arriving
                // here is for us.
                if (m && m.kind === 'snapshot-request') {
                    this.onSnapshotRequest();
                    return;
                }
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
            // A socket we already replaced deliberately must not schedule a
            // second connection on its way out.
            if (this.socket !== ws) {
                return;
            }
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

    /** Replaces the message socket immediately, without the reconnect delay. */
    private reconnectMessages(): void {
        const old = this.socket;
        this.socket = undefined;
        if (old) {
            try {
                old.close();
            } catch (_) {
                // already closing
            }
        }
        this.connectMessages();
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

    // ---- AI assistant (chat panel) ----

    private authHeaders(): Record<string, string> {
        const h: Record<string, string> = {};
        if (this.token) {
            h['Authorization'] = `Bearer ${this.token}`;
        }
        if (this.identityToken) {
            h['X-Hermione-Identity'] = this.identityToken;
        }
        return h;
    }

    /** Whether the course this workspace is enrolled in has the assistant on. */
    async assistantStatus(): Promise<boolean> {
        if (!this.serverUrl) {
            await this.loadConfig();
        }
        await this.ensureIdentity(false);
        try {
            const res = await fetch(`${this.serverUrl}/api/assistant/status`, {
                headers: this.authHeaders(),
            });
            if (!res.ok) {
                return false;
            }
            const data = (await res.json()) as { enabled?: boolean };
            return !!data.enabled;
        } catch (_) {
            return false;
        }
    }

    /** This student's prior turns with the assistant. */
    async assistantHistory(): Promise<{ role: string; body: string }[]> {
        try {
            const url = `${this.serverUrl}/api/assistant/history?student=${encodeURIComponent(this.student)}`;
            const res = await fetch(url, { headers: this.authHeaders() });
            return res.ok ? ((await res.json()) as { role: string; body: string }[]) : [];
        } catch (_) {
            return [];
        }
    }

    private chatBody(message: string): string {
        const editor = vscode.window.activeTextEditor;
        const onFile = editor && tracked(editor.document);
        const file = onFile ? vscode.workspace.asRelativePath(fileUri(editor!.document.uri), false) : undefined;
        const language = onFile ? editor!.document.languageId : undefined;
        return JSON.stringify({ message, student: this.student, file, language });
    }

    /** Sends one question, grounded with the current file, and returns the reply. */
    async assistantChat(message: string): Promise<string> {
        await this.ensureIdentity(false);
        const res = await fetch(`${this.serverUrl}/api/assistant/chat`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...this.authHeaders() },
            body: this.chatBody(message),
        });
        if (!res.ok) {
            throw new Error((await res.text()) || `HTTP ${res.status}`);
        }
        const data = (await res.json()) as { reply: string };
        return data.reply;
    }

    /**
     * Streams one turn. Managed Agents streams at message granularity, so
     * `onMessage` fires once per complete agent message; `onStatus` reports
     * progress between tool calls. Resolves when the turn ends.
     */
    async assistantChatStream(
        message: string,
        on: { status: (t: string) => void; message: (t: string) => void; error: (t: string) => void },
    ): Promise<void> {
        await this.ensureIdentity(false);
        let res: Awaited<ReturnType<typeof fetch>>;
        try {
            res = await fetch(`${this.serverUrl}/api/assistant/chat/stream`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json', ...this.authHeaders() },
                body: this.chatBody(message),
            });
        } catch (e) {
            on.error(e instanceof Error ? e.message : 'Request failed');
            return;
        }
        if (!res.ok || !res.body) {
            on.error((await res.text().catch(() => '')) || `HTTP ${res.status}`);
            return;
        }

        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buf = '';
        let event = 'message';
        for (;;) {
            const { done, value } = await reader.read();
            if (done) {
                break;
            }
            buf += decoder.decode(value, { stream: true });
            let nl: number;
            while ((nl = buf.indexOf('\n')) >= 0) {
                const line = buf.slice(0, nl).replace(/\r$/, '');
                buf = buf.slice(nl + 1);
                if (line === '') {
                    event = 'message';
                } else if (line.startsWith(':')) {
                    // keep-alive comment
                } else if (line.startsWith('event:')) {
                    event = line.slice(6).trim();
                } else if (line.startsWith('data:')) {
                    let payload: { text?: string } = {};
                    try {
                        payload = JSON.parse(line.slice(5).trim());
                    } catch (_) {
                        // ignore malformed frame
                    }
                    const text = payload.text || '';
                    if (event === 'status') {
                        on.status(text);
                    } else if (event === 'error') {
                        on.error(text);
                    } else if (event === 'message') {
                        on.message(text);
                    }
                    // 'done' ends the turn; the stream closes right after.
                }
            }
        }
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
        // Either side may withhold file contents: the course sets the policy,
        // and the student keeps a veto over their own buffer. Both must allow
        // it, so the more restrictive of the two wins.
        this.shareFileContents =
            course.shareFileContents !== false &&
            (cfg.get<boolean>('shareFileContents') ?? true);
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
                this.report('heartbeat');
            }
        }, this.heartbeatSeconds * 1000);
    }

    private onFocus(): void {
        if (this.enabled) {
            this.report('focus');
            this.pushSnapshot();
            this.updateStatusBar();
        }
    }

    /**
     * Counts a typing burst. Reporting every change would flood the backend and
     * say little, so changes are accumulated per document and flushed once per
     * window. What the teacher needs is whether the student is writing code at
     * all — time-on-task alone can't distinguish working from stuck.
     */
    private onEdit(e: vscode.TextDocumentChangeEvent): void {
        if (!this.enabled || !tracked(e.document) || e.contentChanges.length === 0) {
            return;
        }
        const entry = this.pendingEdits.get(e.document.uri.fsPath);
        if (entry) {
            entry.count += e.contentChanges.length;
            entry.at = Date.now();
        } else {
            this.pendingEdits.set(e.document.uri.fsPath, {
                doc: e.document,
                count: e.contentChanges.length,
                at: Date.now(),
            });
        }
        if (!this.editTimer) {
            this.editTimer = setTimeout(() => this.flushEdits(), EDIT_WINDOW_MS);
        }
        // Edit *events* are deliberately coalesced over five seconds; a teacher
        // watching the file wants the keystroke, not the summary.
        this.pushSnapshot();
    }

    private flushEdits(): void {
        if (this.editTimer) {
            clearTimeout(this.editTimer);
            this.editTimer = undefined;
        }
        for (const { doc, count, at } of this.pendingEdits.values()) {
            // Stamped when the typing happened, not when the timer fired —
            // otherwise an edit lands up to a window later than it occurred and
            // can sort after a focus change the student made in between, making
            // the board show the file they already left.
            const event = this.buildEvent('edit', doc.uri, doc.languageId, this.cursorLine(doc), at);
            event.edits = count;
            this.enqueue(event);
        }
        this.pendingEdits.clear();
    }

    /** 1-based cursor line, but only when the document is the one on screen. */
    private cursorLine(doc: vscode.TextDocument): number | undefined {
        const editor = vscode.window.activeTextEditor;
        return editor && editor.document === doc ? editor.selection.active.line + 1 : undefined;
    }

    private onClose(doc: vscode.TextDocument): void {
        // Deleting or collapsing a cell closes that cell's document while the
        // notebook stays open, so cells are left to onCloseNotebook.
        if (this.enabled && doc.uri.scheme === 'file') {
            // Flush first so a final typing burst isn't lost, and so the close
            // stays last in the stream.
            this.flushEdits();
            this.forgetFocus(doc.uri);
            this.enqueue(this.buildEvent('close', doc.uri, undefined));
        }
    }

    private onCloseNotebook(nb: vscode.NotebookDocument): void {
        if (!this.enabled) {
            return;
        }
        this.flushEdits();
        this.forgetFocus(nb.uri);
        this.enqueue(this.buildEvent('close', nb.uri, undefined));
    }

    /**
     * Closing a file has to clear it, or reopening the same one is mistaken for
     * focus that never moved and is never reported.
     */
    private forgetFocus(uri: vscode.Uri): void {
        if (uri.fsPath === this.focusedPath) {
            this.focusedPath = '';
        }
    }

    /**
     * The file the student is looking at.
     *
     * `activeTextEditor` only points at a notebook cell while that cell is in
     * edit mode, so a student reading or clicking through cells has no active
     * text editor at all. Falling back to `activeNotebookEditor` is what makes
     * a notebook count as being worked on, rather than only counting the
     * moments someone is mid-keystroke.
     */
    private activeTarget(): SnapshotTarget | undefined {
        const editor = vscode.window.activeTextEditor;
        if (editor && tracked(editor.document)) {
            return {
                doc: editor.document,
                editor,
                uri: fileUri(editor.document.uri),
                cell: editor.document.uri.scheme === 'vscode-notebook-cell',
            };
        }
        const nb = vscode.window.activeNotebookEditor;
        if (nb) {
            // A notebook with no cells still counts as the file being worked
            // on; there is simply no buffer to read.
            const cell = nb.notebook.cellCount > 0 ? nb.notebook.cellAt(nb.selection.start) : undefined;
            return { doc: cell?.document, uri: nb.notebook.uri, cell: true };
        }
        return undefined;
    }

    /** A target's path relative to the workspace, and the exercise it maps to. */
    private locate(uri: vscode.Uri): { relativePath: string; exercise?: string } {
        const relativePath = vscode.workspace.asRelativePath(uri, false);
        return { relativePath, exercise: this.exercises.resolve(relativePath) };
    }

    // ---- file snapshots (what the teacher sees when they open your file) ----

    /** True while a teacher is known to have this student's file open. */
    private get watched(): boolean {
        return this.watchTimer !== undefined;
    }

    /**
     * The backend asks for a snapshot whenever a teacher has this student's
     * file open. Answering also puts the editor in "watched" mode, so further
     * edits are pushed without waiting to be asked again.
     */
    private onSnapshotRequest(): void {
        if (!this.enabled) {
            return;
        }
        const wasWatched = this.watched;
        // The timer is the whole state: it lapses when the requests stop, which
        // drops the "being viewed" badge rather than leaving a stale one.
        clearTimeout(this.watchTimer);
        this.watchTimer = setTimeout(() => {
            this.watchTimer = undefined;
            this.updateStatusBar();
        }, WATCH_WINDOW_MS);
        if (!wasWatched) {
            // Make it visible in the student's own editor the moment it starts.
            this.updateStatusBar();
        }
        this.sendSnapshot();
    }

    /** Pushes a fresh snapshot, but only while someone is actually looking. */
    private pushSnapshot(): void {
        if (!this.watched || this.snapshotTimer) {
            return;
        }
        this.snapshotTimer = setTimeout(() => {
            this.snapshotTimer = undefined;
            if (this.watched) {
                this.sendSnapshot();
            }
        }, SNAPSHOT_DEBOUNCE_MS);
    }

    /**
     * Builds and sends one snapshot. Unlike file events these are never queued
     * or retried: a snapshot describes one instant, and a stale one is worse
     * than none at all.
     */
    private async sendSnapshot(): Promise<void> {
        // Two triggers drive this — the backend's request and the student's own
        // typing — and they must not stack into overlapping uploads.
        if (this.snapshotSending) {
            return;
        }
        const target = this.activeTarget();
        const doc = target?.doc;
        let snapshot: FileSnapshot;
        if (!this.shareFileContents) {
            // Answer anyway. Silence is indistinguishable from a disconnected
            // editor, and the teacher deserves to be told which one it is.
            snapshot = { student: this.student, declined: true, atUnixMs: Date.now() };
        } else if (!target || !doc) {
            snapshot = { student: this.student, atUnixMs: Date.now() };
        } else {
            snapshot = await buildSnapshot(
                { ...target, doc },
                { student: this.student, ...this.locate(target.uri) },
            );
        }

        this.snapshotSending = true;
        try {
            // Never queued or retried: a snapshot describes one instant, and a
            // stale one is worse than none at all.
            await this.post('/api/file-snapshots', JSON.stringify(snapshot));
        } catch (_) {
            // The teacher's next poll re-asks; nothing to recover here.
        } finally {
            this.snapshotSending = false;
        }
    }

    private report(kind: 'focus' | 'heartbeat'): void {
        const target = this.activeTarget();
        if (!target) {
            return;
        }
        // Clicking from cell to cell inside one notebook is a focus change per
        // cell, all of them the same file. Report the file the student moved
        // to, not every step they took inside it.
        if (kind === 'focus') {
            if (target.uri.fsPath === this.focusedPath) {
                return;
            }
            this.focusedPath = target.uri.fsPath;
        }
        this.enqueue(this.buildEvent(kind, target.uri, target.doc?.languageId,
            target.doc && this.cursorLine(target.doc)));
    }

    private buildEvent(
        kind: FileEvent['kind'],
        uri: vscode.Uri,
        language: string | undefined,
        line?: number,
        at?: number,
    ): FileEvent {
        const file = fileUri(uri);
        const folder = vscode.workspace.getWorkspaceFolder(file);
        const relativePath = vscode.workspace.asRelativePath(file, false);
        return {
            student: this.student,
            studentSource: this.studentSource || undefined,
            repo: this.repo || undefined,
            workspace: folder?.name,
            path: file.fsPath,
            relativePath,
            language,
            exercise: this.exercises.resolve(relativePath),
            kind,
            line,
            atUnixMs: at ?? Date.now(),
        };
    }

    private enqueue(event: FileEvent): void {
        this.queue.push(event);
        // Debounce: coalesce bursts (e.g. rapid focus changes) into one POST.
        if (!this.flushTimer) {
            this.flushTimer = setTimeout(() => this.flush(), 250);
        }
    }

    /**
     * One authenticated POST. Every caller shares the `401` handling, because a
     * stale identity token is a property of the connection rather than of the
     * payload — a snapshot that silently 401s forever would leave the teacher
     * staring at "Asking…" with no way to find out why.
     */
    private async post(path: string, body: string): Promise<void> {
        const res = await fetch(`${this.serverUrl}${path}`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...this.authHeaders() },
            body,
        });
        if (res.status === 401) {
            // Backend requires a verified identity — sign in, then let the
            // caller decide whether this one is worth resending.
            await this.ensureIdentity(true);
            throw new Error('identity required');
        }
        if (!res.ok) {
            throw new Error(`HTTP ${res.status}`);
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
            await this.post('/api/file-events', JSON.stringify(batch));
        } catch {
            // Re-queue and retry on the next tick so we don't lose events.
            this.queue = batch.concat(this.queue);
            if (!this.flushTimer) {
                this.flushTimer = setTimeout(() => this.flush(), 5000);
            }
        }
    }

    private updateStatusBar(): void {
        this.statusBar.backgroundColor = undefined;
        if (!this.enabled) {
            this.statusBar.text = '$(circle-slash) Hermione: off';
            this.statusBar.tooltip = 'Hermione reporting is stopped';
            this.statusBar.show();
            return;
        }
        const target = this.activeTarget();
        const exercise = target && this.locate(target.uri).exercise;
        const suffix = exercise ? ` · ${exercise}` : '';

        // Being watched is never silent. A teacher reading your buffer is a
        // different thing from time-on-task being counted, and the student can
        // see which one is happening.
        if (this.watched) {
            this.statusBar.text = `$(book) Hermione: ${this.student}${suffix} · teacher viewing`;
            this.statusBar.tooltip =
                'A teacher is looking at the file you have open. ' +
                'Turn this off with the hermione.shareFileContents setting.';
            this.statusBar.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
            this.statusBar.show();
            return;
        }

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

    // The AI assistant chat panel (only opens when the course has it enabled).
    registerAssistant(context, reporter);

    if (vscode.workspace.getConfiguration('hermione').get<boolean>('enabled', true)) {
        reporter.start();
    }
}

export function deactivate(): void {
    // Reporter is disposed via context.subscriptions.
}
