import * as vscode from 'vscode';
import * as WebSocket from 'ws';

interface ClientMessage {
    type: 'register' | 'file_update';
    client_type?: 'vscode' | 'webapp';
    session_id?: string;
    active_file?: string;
    file_content?: string;
}

interface ServerMessage {
    type: 'session_created' | 'file_updated' | 'error';
    session_id?: string;
    active_file?: string;
    file_content?: string;
    message?: string;
}

class HermioneClient {
    private ws: WebSocket | null = null;
    private sessionId: string | null = null;
    private config: vscode.WorkspaceConfiguration;
    private statusBarItem: vscode.StatusBarItem;
    private disposables: vscode.Disposable[] = [];

    constructor(private context: vscode.ExtensionContext) {
        this.config = vscode.workspace.getConfiguration('hermione');
        this.statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
        this.statusBarItem.text = "$(circle-outline) Hermione: Disconnected";
        this.statusBarItem.show();

        this.setupEventListeners();

        if (this.config.get('autoConnect', true)) {
            this.connect();
        }
    }

    private setupEventListeners() {
        // Listen for active editor changes
        this.disposables.push(
            vscode.window.onDidChangeActiveTextEditor((editor) => {
                this.handleActiveEditorChange(editor);
            })
        );

        // Listen for text document changes
        this.disposables.push(
            vscode.workspace.onDidChangeTextDocument((event) => {
                if (event.document === vscode.window.activeTextEditor?.document) {
                    this.handleDocumentChange(event.document);
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

    private async handleActiveEditorChange(editor: vscode.TextEditor | undefined) {
        if (!this.ws || !this.sessionId || !editor) {
            return;
        }

        const document = editor.document;
        const filePath = document.uri.fsPath;
        const sendContent = this.config.get('sendFileContent', true);

        let fileContent: string | undefined;
        if (sendContent) {
            fileContent = document.getText();
        }

        this.sendFileUpdate(filePath, fileContent);
    }

    private async handleDocumentChange(document: vscode.TextDocument) {
        if (!this.ws || !this.sessionId) {
            return;
        }

        const filePath = document.uri.fsPath;
        const sendContent = this.config.get('sendFileContent', true);

        let fileContent: string | undefined;
        if (sendContent) {
            fileContent = document.getText();
        }

        // Debounce updates to avoid too frequent messages
        clearTimeout((this as any).updateTimeout);
        (this as any).updateTimeout = setTimeout(() => {
            this.sendFileUpdate(filePath, fileContent);
        }, 500);
    }

    private sendFileUpdate(filePath: string, fileContent?: string) {
        if (!this.ws || !this.sessionId) {
            return;
        }

        const message: ClientMessage = {
            type: 'file_update',
            session_id: this.sessionId,
            active_file: filePath,
            file_content: fileContent
        };

        try {
            this.ws.send(JSON.stringify(message));
        } catch (error) {
            console.error('Failed to send file update:', error);
        }
    }

    public connect() {
        const serverUrl = this.config.get('serverUrl', 'ws://localhost:8080');

        try {
            this.ws = new WebSocket(serverUrl);

            this.ws.on('open', () => {
                console.log('Connected to Hermione service');
                this.statusBarItem.text = "$(check) Hermione: Connected";

                // Register as VSCode client
                const registerMessage: ClientMessage = {
                    type: 'register',
                    client_type: 'vscode'
                };

                this.ws!.send(JSON.stringify(registerMessage));
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
                this.statusBarItem.text = "$(circle-outline) Hermione: Disconnected";
                this.sessionId = null;

                // Attempt to reconnect after 5 seconds
                setTimeout(() => {
                    if (this.config.get('autoConnect', true)) {
                        this.connect();
                    }
                }, 5000);
            });

            this.ws.on('error', (error) => {
                console.error('WebSocket error:', error);
                this.statusBarItem.text = "$(alert) Hermione: Error";
                vscode.window.showErrorMessage(`Hermione connection error: ${error.message}`);
            });

        } catch (error) {
            console.error('Failed to connect to Hermione service:', error);
            this.statusBarItem.text = "$(alert) Hermione: Error";
            vscode.window.showErrorMessage(`Failed to connect to Hermione service: ${error}`);
        }
    }

    private handleServerMessage(message: ServerMessage) {
        switch (message.type) {
            case 'session_created':
                this.sessionId = message.session_id!;
                console.log(`Session created: ${this.sessionId}`);
                this.statusBarItem.text = `$(check) Hermione: Session ${this.sessionId.slice(0, 8)}...`;

                // Send current active file if any
                const activeEditor = vscode.window.activeTextEditor;
                if (activeEditor) {
                    this.handleActiveEditorChange(activeEditor);
                }
                break;

            case 'error':
                console.error('Server error:', message.message);
                vscode.window.showErrorMessage(`Hermione server error: ${message.message}`);
                break;
        }
    }

    public disconnect() {
        if (this.ws) {
            this.ws.close();
            this.ws = null;
            this.sessionId = null;
            this.statusBarItem.text = "$(circle-outline) Hermione: Disconnected";
        }
    }

    public dispose() {
        this.disconnect();
        this.statusBarItem.dispose();
        this.disposables.forEach(d => d.dispose());
    }
}

export function activate(context: vscode.ExtensionContext) {
    console.log('Hermione extension is now active');

    const hermioneClient = new HermioneClient(context);

    // Register commands
    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.connect', () => {
            hermioneClient.connect();
            vscode.window.showInformationMessage('Connecting to Hermione service...');
        })
    );

    context.subscriptions.push(
        vscode.commands.registerCommand('hermione.disconnect', () => {
            hermioneClient.disconnect();
            vscode.window.showInformationMessage('Disconnected from Hermione service');
        })
    );

    context.subscriptions.push(hermioneClient);
}

export function deactivate() {
    console.log('Hermione extension is now deactivated');
}