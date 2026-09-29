import * as os from 'os';
import * as vscode from 'vscode';
import WebSocket from 'ws';
import { registerAssistant, StreamHandlers } from './assistant';
import { backoffMs } from './backoff';
import { CourseFile, readCourseFiles } from './course';
import { ExerciseMap } from './exercises';
import { gitConfig } from './git-config';
import {
    mayReceiveCredentials,
    mayShareContents,
    parseRepo,
    resolveConnection,
    resolveStudent,
} from './settings';
import { buildOpenFile, Report, SnapshotTarget, snapshotKey } from './snapshot';
import { sseEvents } from './sse';

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

/** Longest a request may take before it is given up on, so a hung backend can't stall the editor. */
const REQUEST_TIMEOUT_MS = 10_000;
/** A chat turn streams for as long as the assistant works. */
const STREAM_TIMEOUT_MS = 180_000;
/** Events kept while the backend is unreachable; the oldest go first. */
const MAX_QUEUED_EVENTS = 1_000;
/** A batch the backend refused for what it *is* will never succeed, however often it is resent. */
const PERMANENT_REFUSALS = new Set([400, 403, 413]);
/** How often a stale identity may interrupt the student with a sign-in prompt. */
const SIGN_IN_PROMPT_COOLDOWN_MS = 10 * 60_000;
/** How often the message socket is checked for being silently dead (a laptop woke up, a NAT dropped it). */
const PING_INTERVAL_MS = 30_000;

class HttpError extends Error {
    constructor(readonly status: number) {
        super(`HTTP ${status}`);
    }
}

/** Cancels a timer (or interval) and returns `undefined`, so it can be assigned back. */
function cancel(timer: NodeJS.Timeout | undefined): undefined {
    if (timer) {
        clearTimeout(timer);
    }
    return undefined;
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
    private starting = false;
    private student = '';
    private studentSource = '';
    private repo = '';
    private authProvider = '';
    private serverUrl = '';
    private heartbeatSeconds = 15;
    private token = '';

    /** Path last reported as focused, to suppress same-file focus churn. */
    private focusedPath = '';

    private exercises = new ExerciseMap();
    private queue: FileEvent[] = [];
    /** Consecutive failed flushes, for backing off. */
    private flushFailures = 0;
    private pendingEdits = new Map<
        string,
        { doc: vscode.TextDocument; count: number; at: number }
    >();
    private heartbeatTimer?: NodeJS.Timeout;
    private flushTimer?: NodeJS.Timeout;
    private editTimer?: NodeJS.Timeout;
    private socket?: WebSocket;
    private reconnectTimer?: NodeJS.Timeout;
    private pingTimer?: NodeJS.Timeout;
    private reconnectAttempts = 0;
    /** Bumped whenever the socket is replaced, so a slow connect can tell it was overtaken. */
    private socketGeneration = 0;
    private lastMessageId = 0;
    private windowFocused = true;

    /** Whether this student's config permits sharing file contents at all. */
    private shareFileContents = true;
    /** Set while a teacher is known to be looking; its existence *is* `watched`. */
    private watchTimer?: NodeJS.Timeout;
    private snapshotTimer?: NodeJS.Timeout;
    private snapshotSending = false;
    /**
     * The last whole file the server accepted: what it was built from (see
     * `snapshotKey`) and the number the server gave it. While the key still
     * matches, only the caret can have changed, and the caret alone is sent.
     */
    private lastFile?: { key: string; rev: number };

    private identityPending?: Promise<string | undefined>;
    private lastSignInPrompt = 0;
    private warned = new Set<string>();

    private statusBar: vscode.StatusBarItem;
    private output: vscode.OutputChannel;
    private disposables: vscode.Disposable[] = [];

    constructor(private context: vscode.ExtensionContext) {
        this.statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
        this.statusBar.command = 'hermione.setStudent';
        this.output = vscode.window.createOutputChannel('Hermione');
        context.subscriptions.push(this.statusBar, this.output);
    }

    /** Diagnostics for whoever is debugging the extension; never shown unprompted. */
    private log(message: string): void {
        this.output.appendLine(`${new Date().toISOString()} ${message}`);
    }

    /** Tells the student once per distinct problem, however often config is reloaded. */
    private warnOnce(message: string): void {
        this.log(message);
        if (!this.warned.has(message)) {
            this.warned.add(message);
            void vscode.window.showWarningMessage(`Hermione: ${message}`);
        }
    }

    async start(): Promise<void> {
        // Starting twice would stack a second set of listeners on the first.
        if (this.enabled || this.starting) {
            return;
        }
        this.starting = true;
        try {
            await this.reload();
        } finally {
            this.starting = false;
        }
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
            vscode.workspace.onDidCloseTextDocument((doc) => {
                // Deleting or collapsing a cell closes that cell's document
                // while the notebook stays open, so cells are left to the
                // notebook's own close.
                if (doc.uri.scheme === 'file') {
                    this.closeFile(doc.uri);
                }
            }),
            vscode.workspace.onDidCloseNotebookDocument((nb) => this.closeFile(nb.uri)),
            vscode.workspace.onDidChangeWorkspaceFolders(() => this.reload()),
            vscode.workspace.onDidChangeConfiguration((e) => {
                if (e.affectsConfiguration('hermione')) {
                    void this.reload();
                }
            }),
            vscode.workspace.onDidGrantWorkspaceTrust(() => this.reload()),
        );

        this.restartHeartbeat();
        this.onFocus(); // report current file immediately

        // Surface teacher broadcasts for this course over a live WebSocket.
        this.reconnectMessages();

        this.updateStatusBar();
    }

    stop(): void {
        this.enabled = false;
        this.heartbeatTimer = cancel(this.heartbeatTimer);
        this.editTimer = cancel(this.editTimer);
        this.snapshotTimer = cancel(this.snapshotTimer);
        this.watchTimer = cancel(this.watchTimer);
        // Reporting is off: what is still queued must not be sent, and a retry
        // must not keep the backend busy after the student turned it off.
        this.flushTimer = cancel(this.flushTimer);
        this.queue = [];
        this.lastFile = undefined;
        this.flushFailures = 0;
        this.pendingEdits.clear();
        // start() reports the current file immediately; leaving this set would
        // suppress exactly that.
        this.focusedPath = '';
        this.reconnectTimer = cancel(this.reconnectTimer);
        this.closeSocket();
        this.disposables.forEach((d) => d.dispose());
        this.disposables = [];
        this.updateStatusBar();
    }

    /**
     * Re-reads configuration and applies it: the course file and settings can
     * change under a running editor (a folder is added, a setting edited, a
     * workspace is trusted), and anything already connected must follow.
     */
    private async reload(): Promise<void> {
        const before = this.connectionKey();
        const course = await readCourseFiles();
        for (const problem of course.problems) {
            this.warnOnce(problem);
        }
        this.exercises.load(course.files);
        this.lastFile = undefined; // what a report says (exercise, sharing) may have changed
        await this.loadConfig(course.files[0] ?? {});
        if (!this.enabled) {
            return;
        }
        this.restartHeartbeat();
        // The socket is subscribed under the server, token and student name it
        // connected with, and snapshot requests are routed by that name.
        if (this.connectionKey() !== before) {
            this.reconnectMessages();
        }
        this.updateStatusBar();
    }

    private connectionKey(): string {
        return [this.serverUrl, this.token, this.authProvider, this.student].join('\n');
    }

    private messageKey(): string {
        return `hermione.lastMessageId:${this.serverUrl}`;
    }

    // ---- the message socket ----

    /** Replaces the message socket immediately, without the reconnect delay. */
    private reconnectMessages(): void {
        this.closeSocket();
        this.reconnectTimer = cancel(this.reconnectTimer);
        this.reconnectAttempts = 0;
        this.lastMessageId = this.context.globalState.get(this.messageKey(), 0);
        void this.connectMessages();
    }

    private closeSocket(): void {
        this.socketGeneration++;
        this.pingTimer = cancel(this.pingTimer);
        const old = this.socket;
        this.socket = undefined;
        try {
            old?.terminate();
        } catch (_) {
            // already closed
        }
    }

    /**
     * Opens the message WebSocket. On connect the server replays anything
     * missed (via `since`), then pushes new broadcasts live; when the connection
     * drops we reconnect with a growing, jittered delay.
     */
    private async connectMessages(): Promise<void> {
        if (!this.enabled) {
            return;
        }
        const generation = ++this.socketGeneration;
        // The enrollment token goes in the Authorization header with the rest of
        // `authHeaders()`, not in the URL, where proxies and access logs keep it.
        // Credentials go with the socket as they do with every other request:
        // where students are verified, the backend routes this editor's frames
        // by who it proves to be, not by the `student` below.
        const headers = await this.authHeaders();
        if (!this.enabled || generation !== this.socketGeneration) {
            return; // stopped, or replaced while signing in
        }

        const params = new URLSearchParams();
        params.set('since', String(this.lastMessageId));
        // Lets the backend route snapshot requests to this editor alone rather
        // than to the whole course. Routing only: the snapshot that goes back
        // is attributed by the identity the backend verifies on ingest.
        if (this.student) {
            params.set('student', this.student);
        }

        let ws: WebSocket;
        try {
            ws = new WebSocket(`${this.serverUrl.replace(/^http/, 'ws')}/ws?${params}`, { headers });
        } catch (e) {
            this.log(`could not open the message socket: ${String(e)}`);
            this.scheduleReconnect();
            return;
        }
        this.socket = ws;

        let alive = true;
        ws.on('open', () => {
            this.reconnectAttempts = 0;
            // A half-open socket never fires `close`, and snapshot requests
            // would be lost silently until the window was reloaded.
            this.pingTimer = setInterval(() => {
                if (!alive) {
                    this.log('message socket stopped answering; reconnecting');
                    ws.terminate();
                    return;
                }
                alive = false;
                ws.ping();
            }, PING_INTERVAL_MS);
        });
        ws.on('pong', () => {
            alive = true;
        });
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
                    void vscode.window.showInformationMessage(`📣 ${m.body}`);
                    if (m.id > this.lastMessageId) {
                        this.lastMessageId = m.id;
                        void this.context.globalState.update(this.messageKey(), this.lastMessageId);
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
            this.pingTimer = cancel(this.pingTimer);
            this.scheduleReconnect();
        });
        ws.on('error', (e) => {
            this.log(`message socket: ${e.message}`);
            try {
                ws.terminate();
            } catch (_) {
                // already closed
            }
        });
    }

    private scheduleReconnect(): void {
        if (this.reconnectTimer || !this.enabled) {
            return;
        }
        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = undefined;
            void this.connectMessages();
        }, backoffMs(this.reconnectAttempts++));
    }

    // ---- talking to the backend ----

    /** The credentials every request carries; signs in silently if the identity has lapsed. */
    private async authHeaders(): Promise<Record<string, string>> {
        const headers: Record<string, string> = {};
        if (this.token) {
            headers['Authorization'] = `Bearer ${this.token}`;
        }
        const identity = await this.ensureIdentity();
        if (identity) {
            headers['X-Hermione-Identity'] = identity;
        }
        return headers;
    }

    /** One authenticated request. The caller decides what each status means. */
    private async api(
        path: string,
        init: { body?: string; stream?: boolean } = {},
    ): Promise<Response> {
        const headers = await this.authHeaders();
        if (init.body !== undefined) {
            headers['Content-Type'] = 'application/json';
        }
        return fetch(`${this.serverUrl}${path}`, {
            method: init.body === undefined ? 'GET' : 'POST',
            headers,
            body: init.body,
            signal: AbortSignal.timeout(init.stream ? STREAM_TIMEOUT_MS : REQUEST_TIMEOUT_MS),
        });
    }

    /**
     * One authenticated POST. Every caller shares the `401` handling, because a
     * stale identity token is a property of the connection rather than of the
     * payload — a snapshot that silently 401s forever would leave the teacher
     * staring at "Asking…" with no way to find out why.
     */
    private async post(path: string, body: string): Promise<Response> {
        const res = await this.api(path, { body });
        if (res.status === 401) {
            // Backend requires a verified identity — renew it, then let the
            // caller decide whether this one is worth resending.
            await this.renewIdentity();
        }
        if (!res.ok) {
            throw new HttpError(res.status);
        }
        return res;
    }

    // ---- identity ----

    private identityKey(): string {
        return `hermione.identity:${this.serverUrl}:${this.authProvider}`;
    }

    /** The Hermione identity token: the cached one while it is good, else a silent sign-in. */
    private ensureIdentity(): Promise<string | undefined> {
        const cached = this.context.globalState.get<{ token: string; exp: number }>(this.identityKey());
        if (cached && cached.exp > Date.now() / 1000 + 60) {
            return Promise.resolve(cached.token);
        }
        // Concurrent requests share one sign-in rather than each starting their own.
        return (this.identityPending ??= this.signIn(false).finally(() => {
            this.identityPending = undefined;
        }));
    }

    /**
     * The backend rejected our identity, so the cached one is no good whatever
     * its expiry says. Renew silently if VSCode still has a session; only
     * prompt when it does not, and no more than once in a while.
     */
    private async renewIdentity(): Promise<void> {
        if ((await this.signIn(false)) !== undefined) {
            return;
        }
        if (Date.now() - this.lastSignInPrompt > SIGN_IN_PROMPT_COOLDOWN_MS) {
            this.lastSignInPrompt = Date.now();
            await this.signIn(true);
        }
    }

    /**
     * Exchanges VSCode's GitHub session (silent in Codespaces) for a Hermione
     * identity token at the backend. `interactive` may prompt to sign in.
     */
    private async signIn(interactive: boolean): Promise<string | undefined> {
        if (!mayReceiveCredentials(this.serverUrl)) {
            this.warnOnce(
                `not signing in to ${this.serverUrl}: a GitHub token is only sent over https or to this machine`,
            );
            return undefined;
        }
        try {
            const session = await vscode.authentication.getSession(
                'github',
                ['read:user'],
                interactive ? { createIfNone: true } : { silent: true },
            );
            if (!session) {
                return undefined;
            }
            const res = await fetch(`${this.serverUrl}/api/auth/exchange`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ provider: this.authProvider, token: session.accessToken }),
                signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
            });
            if (!res.ok) {
                this.log(`identity exchange refused: HTTP ${res.status}`);
                return undefined;
            }
            const data = (await res.json()) as { identityToken: string; expiresIn: number };
            await this.context.globalState.update(this.identityKey(), {
                token: data.identityToken,
                exp: Math.floor(Date.now() / 1000) + (data.expiresIn || 28800),
            });
            return data.identityToken;
        } catch (e) {
            // Identity unavailable; ingest will 401 if the backend enforces it.
            this.log(`identity exchange failed: ${String(e)}`);
            return undefined;
        }
    }

    // ---- AI assistant (chat panel) ----

    /** Whether the course this workspace is enrolled in has the assistant on. */
    async assistantStatus(): Promise<boolean> {
        try {
            const res = await this.api('/api/assistant/status');
            return res.ok && !!((await res.json()) as { enabled?: boolean }).enabled;
        } catch (_) {
            return false;
        }
    }

    /** This student's prior turns with the assistant. */
    async assistantHistory(): Promise<{ role: string; body: string }[]> {
        try {
            const res = await this.api(
                `/api/assistant/history?student=${encodeURIComponent(this.student)}`,
            );
            return res.ok ? ((await res.json()) as { role: string; body: string }[]) : [];
        } catch (_) {
            return [];
        }
    }

    /** The question, grounded with the file the student is looking at. */
    private chatBody(message: string): string {
        const target = this.activeTarget();
        return JSON.stringify({
            message,
            student: this.student,
            file: target && this.locate(target.uri).relativePath,
            language: target?.doc?.languageId,
        });
    }

    /**
     * Streams one turn. Managed Agents streams at message granularity, so
     * `message` fires once per complete agent message; `status` reports progress
     * between tool calls. Resolves when the turn ends, however it ends: every
     * failure is reported through `error`, never thrown, because the panel is
     * waiting on this to re-enable its input.
     */
    async assistantChatStream(message: string, on: StreamHandlers): Promise<void> {
        try {
            const res = await this.api('/api/assistant/chat/stream', {
                body: this.chatBody(message),
                stream: true,
            });
            if (!res.ok || !res.body) {
                on.error((await res.text().catch(() => '')) || `HTTP ${res.status}`);
                return;
            }
            for await (const { event, data } of sseEvents(res.body)) {
                let text = '';
                try {
                    text = (JSON.parse(data) as { text?: string }).text || '';
                } catch (_) {
                    continue; // ignore a malformed frame
                }
                // 'done' ends the turn; the stream closes right after.
                if (event === 'status' || event === 'error' || event === 'message') {
                    on[event](text);
                }
            }
        } catch (e) {
            this.log(`assistant stream failed: ${String(e)}`);
            on.error('The connection to the assistant was lost.');
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
            await this.reload();
        }
    }

    /**
     * Applies configuration. The committed course file (`.hermione.json`) is the
     * single source of truth a teacher controls — it can carry `backend`,
     * `token`, and an `identity` source — falling back to VSCode settings/env.
     * The rules themselves are in `settings.ts`.
     */
    private async loadConfig(course: CourseFile): Promise<void> {
        const settings = vscode.workspace.getConfiguration('hermione');
        // A folder opened from anywhere can carry a `.hermione.json`. Until the
        // student trusts it, it may not choose where their activity, file
        // contents and sign-in go.
        const trusted = vscode.workspace.isTrusted;
        if (!trusted && (course.backend || course.token)) {
            this.warnOnce(
                "this workspace isn't trusted, so the backend and token in its .hermione.json are ignored",
            );
        }
        const connection = resolveConnection(
            trusted ? course : { ...course, backend: undefined, token: undefined },
            {
                serverUrl: settings.get<string>('serverUrl'),
                token: settings.get<string>('token'),
                shareFileContents: settings.get<boolean>('shareFileContents'),
            },
            process.env.HERMIONE_TOKEN,
        );
        this.serverUrl = connection.serverUrl;
        this.token = connection.token;
        this.authProvider = connection.authProvider;
        this.shareFileContents = connection.shareFileContents;
        this.heartbeatSeconds = Math.max(5, settings.get<number>('heartbeatSeconds') ?? 15);

        const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
        const id = await resolveStudent({
            preference: course.identity,
            explicit: settings.get<string>('student') || process.env.HERMIONE_STUDENT || '',
            githubUser: process.env.GITHUB_USER || '',
            gitEmail: () => gitConfig(['user.email'], cwd),
            osUser: () => os.userInfo().username,
        });
        this.student = id.value;
        this.studentSource = id.source;
        // The repo (owner/name) the activity comes from, for identity provenance.
        this.repo = parseRepo(await gitConfig(['--get', 'remote.origin.url'], cwd));
    }

    private restartHeartbeat(): void {
        this.heartbeatTimer = cancel(this.heartbeatTimer);
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
        this.editTimer = cancel(this.editTimer);
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

    /** A file (or notebook) was closed. */
    private closeFile(uri: vscode.Uri): void {
        if (!this.enabled) {
            return;
        }
        // Flush first so a final typing burst isn't lost, and so the close
        // stays last in the stream.
        this.flushEdits();
        // Closing a file has to clear it, or reopening the same one is mistaken
        // for focus that never moved and is never reported.
        if (uri.fsPath === this.focusedPath) {
            this.focusedPath = '';
        }
        this.enqueue(this.buildEvent('close', uri, undefined));
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
        cancel(this.watchTimer);
        this.watchTimer = setTimeout(() => {
            this.watchTimer = undefined;
            this.updateStatusBar();
        }, WATCH_WINDOW_MS);
        if (!wasWatched) {
            // Make it visible in the student's own editor the moment it starts.
            this.updateStatusBar();
        }
        void this.sendSnapshot();
    }

    /** Pushes a fresh snapshot, but only while someone is actually looking. */
    private pushSnapshot(): void {
        if (!this.watched || this.snapshotTimer) {
            return;
        }
        this.snapshotTimer = setTimeout(() => {
            this.snapshotTimer = undefined;
            if (this.watched) {
                void this.sendSnapshot();
            }
        }, SNAPSHOT_DEBOUNCE_MS);
    }

    /**
     * What to answer a snapshot request with, given what is on screen — and, for
     * a whole file, the key it was built from.
     */
    private async buildReport(): Promise<{ report: Report; key?: string }> {
        const { student } = this;
        if (!this.shareFileContents) {
            // Answer anyway. Silence is indistinguishable from a disconnected
            // editor, and the teacher deserves to be told which one it is.
            return { report: { student, state: 'declined' } };
        }
        const target = this.activeTarget();
        if (!target?.doc) {
            return { report: { student, state: 'empty' } };
        }
        const where = this.locate(target.uri);
        // The buffer is only read for a file that belongs to the course: one
        // elsewhere on the machine, or one that holds credentials, is declined
        // exactly as if the student had opted out.
        const inside = vscode.workspace.getWorkspaceFolder(target.uri) !== undefined;
        if (!mayShareContents(where.relativePath, inside)) {
            return { report: { student, state: 'declined' } };
        }
        const withDoc = { ...target, doc: target.doc };
        // Taken before the buffer is read: an edit in between makes the report
        // newer than its key, which only costs one redundant whole-file send.
        const key = await snapshotKey(withDoc);
        return { report: { student, ...(await buildOpenFile(withDoc, where)) }, key };
    }

    /**
     * When nothing but the caret has moved since the last whole file the server
     * took, say just that: a few dozen bytes instead of the whole buffer and its
     * diff, which is most of what a teacher watching someone think receives.
     * Returns whether it did; `false` means send the whole file.
     */
    private async sendCursorOnly(): Promise<boolean> {
        const last = this.lastFile;
        const target = this.activeTarget();
        const at = target?.editor?.selection.active;
        if (!last || !this.shareFileContents || !target?.doc || !at) {
            return false;
        }
        try {
            if ((await snapshotKey({ ...target, doc: target.doc })) !== last.key) {
                return false;
            }
            const res = await this.post(
                '/api/file-snapshots',
                JSON.stringify({
                    student: this.student,
                    state: 'cursor',
                    basis: last.rev,
                    cursor: { line: at.line + 1, column: at.character + 1 },
                }),
            );
            last.rev = ((await res.json()) as { rev: number }).rev;
            return true;
        } catch (e) {
            // A 409 is the server saying its copy is not the one we mean (it
            // expired, or restarted): the whole file it is, next.
            this.lastFile = undefined;
            if (!(e instanceof HttpError && e.status === 409)) {
                this.log(`cursor-only snapshot not sent: ${String(e)}`);
            }
            return false;
        }
    }

    /**
     * Builds and sends one snapshot. Unlike file events these are never queued
     * or retried: a snapshot describes one instant, and a stale one is worse
     * than none at all.
     */
    private async sendSnapshot(): Promise<void> {
        // Two triggers drive this — the backend's request and the student's own
        // typing — and they must not stack into overlapping uploads. Claimed
        // before anything is awaited, or two callers could both pass the check.
        if (this.snapshotSending) {
            return;
        }
        this.snapshotSending = true;
        try {
            if (await this.sendCursorOnly()) {
                return;
            }
            let built: { report: Report; key?: string };
            try {
                built = await this.buildReport();
            } catch (e) {
                // Whatever went wrong reading the buffer or git, the teacher's
                // pane must not be left asking forever: say nothing is open.
                this.log(`could not build a snapshot: ${String(e)}`);
                built = { report: { student: this.student, state: 'empty' } };
            }
            this.lastFile = undefined;
            const res = await this.post('/api/file-snapshots', JSON.stringify(built.report));
            if (built.key !== undefined) {
                // An older backend answers with no body: no number, so no
                // cursor-only reports, exactly as before.
                const { rev } = (await res.json().catch(() => ({}))) as { rev?: number };
                this.lastFile = typeof rev === 'number' ? { key: built.key, rev } : undefined;
            }
        } catch (e) {
            // The teacher's next poll re-asks; nothing to recover here.
            this.log(`snapshot not sent: ${String(e)}`);
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
        const { relativePath, exercise } = this.locate(file);
        return {
            student: this.student,
            studentSource: this.studentSource || undefined,
            repo: this.repo || undefined,
            workspace: vscode.workspace.getWorkspaceFolder(file)?.name,
            path: file.fsPath,
            relativePath,
            language,
            exercise,
            kind,
            line,
            atUnixMs: at ?? Date.now(),
        };
    }

    private enqueue(event: FileEvent): void {
        this.queue.push(event);
        // Debounce: coalesce bursts (e.g. rapid focus changes) into one POST.
        this.flushTimer ??= setTimeout(() => void this.flush(), 250);
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
            this.flushFailures = 0;
        } catch (e) {
            if (e instanceof HttpError && PERMANENT_REFUSALS.has(e.status)) {
                // Resending the same batch would be refused forever and block
                // everything behind it.
                this.log(`dropped ${batch.length} events the backend refused (${e.message})`);
                this.flushFailures = 0;
            } else {
                // Re-queue and retry with a growing delay, so an outage costs
                // the backend a trickle of requests rather than one per five
                // seconds from every student, and never grows without bound.
                this.queue = batch.concat(this.queue).slice(-MAX_QUEUED_EVENTS);
                this.flushTimer ??= setTimeout(
                    () => void this.flush(),
                    backoffMs(this.flushFailures++),
                );
                return;
            }
        }
        if (this.queue.length > 0) {
            this.flushTimer ??= setTimeout(() => void this.flush(), 250);
        }
    }

    private updateStatusBar(): void {
        let text: string;
        let tooltip: string;
        let warn = false;
        if (!this.enabled) {
            text = '$(circle-slash) Hermione: off';
            tooltip = 'Hermione reporting is stopped';
        } else {
            const target = this.activeTarget();
            const exercise = target && this.locate(target.uri).exercise;
            const suffix = exercise ? ` · ${exercise}` : '';
            if (this.watched) {
                // Being watched is never silent. A teacher reading your buffer
                // is a different thing from time-on-task being counted, and the
                // student can see which one is happening.
                text = `$(book) Hermione: ${this.student}${suffix} · teacher viewing`;
                tooltip =
                    'A teacher is looking at the file you have open. ' +
                    'Turn this off with the hermione.shareFileContents setting.';
                warn = true;
            } else {
                text = `$(eye) Hermione: ${this.student}${suffix}`;
                tooltip = `Reporting file activity as "${this.student}" to ${this.serverUrl}`;
            }
        }
        this.statusBar.text = text;
        this.statusBar.tooltip = tooltip;
        this.statusBar.backgroundColor = warn
            ? new vscode.ThemeColor('statusBarItem.warningBackground')
            : undefined;
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
        reporter.start().catch((e) => console.error('Hermione failed to start', e));
    }
}

export function deactivate(): void {
    // Reporter is disposed via context.subscriptions.
}
