import * as vscode from 'vscode';
import * as WebSocket from 'ws';

// ============================================================================
// Message Types (matching backend)
// ============================================================================

interface CursorPosition {
    line: number;
    column: number;
}

interface ClientMessage {
    type: 'register' | 'file_update' | 'heartbeat';
    client_type?: 'vscode' | 'shell' | 'webapp';
    student_name?: string;
    session_id?: string;
    file_path?: string;
    file_content?: string;
    cursor_position?: CursorPosition;
}

interface ServerMessage {
    type: 'session_created' | 'file_updated' | 'error' | 'session_list';
    session_id?: string;
    message?: string;
}

// ============================================================================
// Hermione Client
// ============================================================================

class HermioneClient {
    private ws: WebSocket | null = null;
    private sessionId: string | null = null;
    private config: vscode.WorkspaceConfiguration;
    private statusBarItem: vscode.StatusBarItem;
    private disposables: vscode.Disposable[] = [];
    private reconnectTimeout: NodeJS.Timeout | null = null;
    private heartbeatInterval: NodeJS.Timeout | null = null;
    private updateTimeout: NodeJS.Timeout | null = null;

    constructor(private context: vscode.ExtensionContext) {
        this.config = vscode.workspace.getConfiguration('hermione');

        // Create status bar item
        this.statusBarItem = vscode.window.createStatusBarItem(
            vscode.StatusBarAlignment.Right,
            100
        );
        this.statusBarItem.command = 'hermione.toggleConnection';
        this.updateStatusBar('disconnected');
        this.statusBarItem.show();

        this.setupEventListeners();

        // Auto-connect if configured
        if (this.config.get('autoConnect', true)) {
            this.connect();
        }
    }

    private setupEventListeners(): void {
        // Listen for active editor changes
        this.disposables.push(
            vscode.window.onDidChangeActiveTextEditor((editor) => {
                this.handleActiveEditorChange(editor);
            })
        );

        // Listen for text document changes (debounced)
        this.disposables.push(
            vscode.workspace.onDidChangeTextDocument((event) => {
                if (event.document === vscode.window.activeTextEditor?.document) {
                    this.handleDocumentChange(event.document);
                }
            })
        );

        // Listen for cursor position changes
        this.disposables.push(
            vscode.window.onDidChangeTextEditorSelection((event) => {
                if (event.textEditor === vscode.window.activeTextEditor) {
                    this.handleCursorChange(event.textEditor);
                }
            })
        );

        // Listen for configuration changes
        this.disposables.push(
            vscode.workspace.onDidChangeConfiguration((event) => {
                if (event.affectsConfiguration('hermione')) {
                    this.config = vscode.workspace.getConfiguration('hermione');
                }
            })
        );
    }

    private updateStatusBar(status: 'connected' | 'connecting' | 'disconnected' | 'error'): void {
        const icons: Record<string, string> = {
            connected: '$(check)',
            connecting: '$(sync~spin)',
            disconnected: '$(circle-outline)',
            error: '$(alert)'
        };

        const sessionInfo = this.sessionId ? ` (${this.sessionId.slice(0, 8)})` : '';

        switch (status) {
            case 'connected':
                this.statusBarItem.text = `${icons.connected} Hermione${sessionInfo}`;
                this.statusBarItem.backgroundColor = undefined;
                break;
            case 'connecting':
                this.statusBarItem.text = `${icons.connecting} Hermione: Connecting...`;
                this.statusBarItem.backgroundColor = undefined;
                break;
            case 'disconnected':
                this.statusBarItem.text = `${icons.disconnected} Hermione: Disconnected`;
                this.statusBarItem.backgroundColor = undefined;
                break;
            case 'error':
                this.statusBarItem.text = `${icons.error} Hermione: Error`;
                this.statusBarItem.backgroundColor = new vscode.ThemeColor(
                    'statusBarItem.errorBackground'
                );
                break;
        }
    }

    private handleActiveEditorChange(editor: vscode.TextEditor | undefined): void {
        if (!this.ws || !this.sessionId || !editor) {
            return;
        }

        this.sendFileUpdate(editor);
    }

    private handleDocumentChange(document: vscode.TextDocument): void {
        if (!this.ws || !this.sessionId) {
            return;
        }

        // Debounce updates to avoid too frequent messages
        if (this.updateTimeout) {
            clearTimeout(this.updateTimeout);
        }

        this.updateTimeout = setTimeout(() => {
            const editor = vscode.window.activeTextEditor;
            if (editor && editor.document === document) {
                this.sendFileUpdate(editor);
            }
        }, this.config.get('debounceMs', 500));
    }

    private handleCursorChange(editor: vscode.TextEditor): void {
        // Only send cursor updates if configured and not too frequent
        if (!this.config.get('sendCursorPosition', true)) {
            return;
        }

        // Debounce cursor updates
        if (this.updateTimeout) {
            clearTimeout(this.updateTimeout);
        }

        this.updateTimeout = setTimeout(() => {
            this.sendFileUpdate(editor);
        }, this.config.get('cursorDebounceMs', 200));
    }

    private sendFileUpdate(editor: vscode.TextEditor): void {
        if (!this.ws || !this.sessionId || this.ws.readyState !== WebSocket.OPEN) {
            return;
        }

        const document = editor.document;
        const filePath = document.uri.fsPath;
        const sendContent = this.config.get('sendFileContent', true);

        let fileContent: string | undefined;
        if (sendContent) {
            // Limit file content size to avoid overwhelming the server
            const maxSize = this.config.get('maxFileSize', 100000);
            const content = document.getText();
            fileContent = content.length > maxSize ? content.slice(0, maxSize) : content;
        }

        const cursorPosition: CursorPosition = {
            line: editor.selection.active.line,
            column: editor.selection.active.character
        };

        const message: ClientMessage = {
            type: 'file_update',
            session_id: this.sessionId,
            file_path: filePath,
            file_content: fileContent,
            cursor_position: cursorPosition
        };

        try {
            this.ws.send(JSON.stringify(message));
        } catch (error) {
            console.error('Failed to send file update:', error);
        }
    }

    public connect(): void {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
            return;
        }

        const serverUrl = this.config.get('serverUrl', 'ws://localhost:8080/ws');
        this.updateStatusBar('connecting');

        try {
            this.ws = new WebSocket(serverUrl);

            this.ws.on('open', () => {
                console.log('Connected to Hermione service');

                // Register as VSCode client
                const studentName = this.config.get<string>('studentName');
                const registerMessage: ClientMessage = {
                    type: 'register',
                    client_type: 'vscode',
                    student_name: studentName
                };

                this.ws!.send(JSON.stringify(registerMessage));

                // Start heartbeat
                this.startHeartbeat();
            });

            this.ws.on('message', (data: WebSocket.Data) => {
                try {
                    const message: ServerMessage = JSON.parse(data.toString());
                    this.handleServerMessage(message);
                } catch (error) {
                    console.error('Failed to parse server message:', error);
                }
            });

            this.ws.on('close', () => {
                console.log('Disconnected from Hermione service');
                this.sessionId = null;
                this.stopHeartbeat();
                this.updateStatusBar('disconnected');

                // Attempt to reconnect
                this.scheduleReconnect();
            });

            this.ws.on('error', (error) => {
                console.error('WebSocket error:', error);
                this.updateStatusBar('error');
            });

        } catch (error) {
            console.error('Failed to connect to Hermione service:', error);
            this.updateStatusBar('error');
            this.scheduleReconnect();
        }
    }

    private handleServerMessage(message: ServerMessage): void {
        switch (message.type) {
            case 'session_created':
                this.sessionId = message.session_id!;
                console.log(`Session created: ${this.sessionId}`);
                this.updateStatusBar('connected');

                // Send current active file if any
                const activeEditor = vscode.window.activeTextEditor;
                if (activeEditor) {
                    this.sendFileUpdate(activeEditor);
                }

                vscode.window.showInformationMessage(
                    `Hermione: Connected (Session: ${this.sessionId.slice(0, 8)}...)`
                );
                break;

            case 'error':
                console.error('Server error:', message.message);
                vscode.window.showErrorMessage(
                    `Hermione server error: ${message.message}`
                );
                break;
        }
    }

    private startHeartbeat(): void {
        this.stopHeartbeat();

        const interval = this.config.get('heartbeatInterval', 30000);
        this.heartbeatInterval = setInterval(() => {
            if (this.ws && this.sessionId && this.ws.readyState === WebSocket.OPEN) {
                const message: ClientMessage = {
                    type: 'heartbeat',
                    session_id: this.sessionId
                };
                this.ws.send(JSON.stringify(message));
            }
        }, interval);
    }

    private stopHeartbeat(): void {
        if (this.heartbeatInterval) {
            clearInterval(this.heartbeatInterval);
            this.heartbeatInterval = null;
        }
    }

    private scheduleReconnect(): void {
        if (this.reconnectTimeout) {
            return;
        }

        const reconnectDelay = this.config.get('reconnectDelay', 5000);
        if (this.config.get('autoConnect', true)) {
            this.reconnectTimeout = setTimeout(() => {
                this.reconnectTimeout = null;
                this.connect();
            }, reconnectDelay);
        }
    }

    public disconnect(): void {
        if (this.reconnectTimeout) {
            clearTimeout(this.reconnectTimeout);
            this.reconnectTimeout = null;
        }

        this.stopHeartbeat();

        if (this.ws) {
            this.ws.close();
            this.ws = null;
            this.sessionId = null;
            this.updateStatusBar('disconnected');
        }
    }

    public toggleConnection(): void {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
            this.disconnect();
            vscode.window.showInformationMessage('Hermione: Disconnected');
        } else {
            this.connect();
        }
    }

    public dispose(): void {
        this.disconnect();
        this.statusBarItem.dispose();
        this.disposables.forEach(d => d.dispose());
    }
}

// ============================================================================
// Extension Activation
// ============================================================================

export function activate(context: vscode.ExtensionContext): void {
    console.log('Hermione extension is now active');

    const hermioneClient = new HermioneClient(context);

    // Register commands
    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.connect', () => {
            hermioneClient.connect();
        })
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.disconnect', () => {
            hermioneClient.disconnect();
            vscode.window.showInformationMessage('Hermione: Disconnected');
        })
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.toggleConnection', () => {
            hermioneClient.toggleConnection();
        })
    );

    context.subscriptions.push(hermioneClient);
}

export function deactivate(): void {
    console.log('Hermione extension is now deactivated');
}
