use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use nix::pty::{openpty, OpenptyResult, Winsize};
use nix::sys::termios::{self, SetArg};
use nix::unistd::{dup2, execvp, fork, setsid, ForkResult};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

// ============================================================================
// Message Types (matching backend)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        client_type: String,
        student_name: Option<String>,
    },
    TerminalOutput {
        session_id: String,
        output: String,
        stream: String,
    },
    TerminalInput {
        session_id: String,
        input: String,
    },
    Heartbeat {
        session_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    SessionCreated { session_id: String },
    Error { message: String },
}

// ============================================================================
// CLI Arguments
// ============================================================================

#[derive(Parser, Debug)]
#[command(
    name = "hermione-shell",
    about = "A shell wrapper that monitors stdin/stdout for the Hermione observability platform"
)]
struct Args {
    /// WebSocket server URL
    #[arg(short, long, default_value = "ws://localhost:8080/ws")]
    server: String,

    /// Student name for identification
    #[arg(short, long)]
    name: Option<String>,

    /// Shell to run (defaults to $SHELL or /bin/bash)
    #[arg(short = 'c', long)]
    shell: Option<String>,

    /// Run in offline mode (no server connection)
    #[arg(long)]
    offline: bool,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermione_shell=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    // Determine shell to use
    let shell = args
        .shell
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/bash".to_string());

    info!("Starting hermione-shell with shell: {}", shell);

    // Channel for sending data to WebSocket
    let (ws_tx, ws_rx) = mpsc::channel::<ClientMessage>(1000);

    // Atomic flag for clean shutdown
    let running = Arc::new(AtomicBool::new(true));

    // Connect to server if not offline
    let session_id = if !args.offline {
        match connect_to_server(&args.server, args.name.clone(), ws_rx).await {
            Ok(session_id) => {
                info!("Connected to server, session: {}", session_id);
                eprintln!("\x1b[32m[Hermione] Connected - Session: {}\x1b[0m", &session_id[..8]);
                Some(session_id)
            }
            Err(e) => {
                warn!("Failed to connect to server: {}. Running in offline mode.", e);
                eprintln!("\x1b[33m[Hermione] Running offline - could not connect to server\x1b[0m");
                None
            }
        }
    } else {
        eprintln!("\x1b[33m[Hermione] Running in offline mode\x1b[0m");
        None
    };

    // Run the PTY
    run_pty(&shell, session_id, ws_tx, running).await?;

    Ok(())
}

async fn connect_to_server(
    server_url: &str,
    student_name: Option<String>,
    mut ws_rx: mpsc::Receiver<ClientMessage>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let url = url::Url::parse(server_url)?;
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Register with server
    let register_msg = ClientMessage::Register {
        client_type: "shell".to_string(),
        student_name,
    };
    write
        .send(Message::Text(serde_json::to_string(&register_msg)?))
        .await?;

    // Wait for session ID
    let session_id = loop {
        if let Some(msg) = read.next().await {
            match msg? {
                Message::Text(text) => {
                    let server_msg: ServerMessage = serde_json::from_str(&text)?;
                    match server_msg {
                        ServerMessage::SessionCreated { session_id } => {
                            break session_id;
                        }
                        ServerMessage::Error { message } => {
                            return Err(format!("Server error: {}", message).into());
                        }
                    }
                }
                Message::Close(_) => {
                    return Err("Connection closed".into());
                }
                _ => {}
            }
        }
    };

    // Spawn task to handle outgoing messages
    tokio::spawn(async move {
        while let Some(msg) = ws_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if write.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }

        // Send close when done
        let _ = write.close().await;
    });

    Ok(session_id)
}

async fn run_pty(
    shell: &str,
    session_id: Option<String>,
    ws_tx: mpsc::Sender<ClientMessage>,
    running: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Get terminal size
    let winsize = get_terminal_size();

    // Open PTY
    let OpenptyResult { master, slave } = openpty(&winsize, None)?;

    // Save original terminal settings using stdin
    let stdin = io::stdin();
    let original_termios = termios::tcgetattr(&stdin).ok();

    // Fork process
    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            // Close slave in parent
            drop(slave);

            // Set terminal to raw mode
            if let Some(ref orig) = original_termios {
                let mut raw = orig.clone();
                termios::cfmakeraw(&mut raw);
                termios::tcsetattr(&stdin, SetArg::TCSANOW, &raw)?;
            }

            // Handle I/O
            let master_fd = master.as_raw_fd();
            let result = handle_io(master_fd, session_id, ws_tx, running.clone()).await;

            // Restore terminal settings
            if let Some(orig) = original_termios {
                let _ = termios::tcsetattr(&stdin, SetArg::TCSANOW, &orig);
            }

            // Wait for child
            let _ = nix::sys::wait::waitpid(child, None);

            result?;
        }
        ForkResult::Child => {
            // Close master in child
            drop(master);

            // Create new session
            setsid()?;

            // Set slave as controlling terminal
            let slave_fd = slave.as_raw_fd();
            unsafe {
                libc::ioctl(slave_fd, libc::TIOCSCTTY, 0);
            }

            // Redirect stdin/stdout/stderr to slave
            dup2(slave_fd, 0)?;
            dup2(slave_fd, 1)?;
            dup2(slave_fd, 2)?;

            if slave_fd > 2 {
                drop(slave);
            }

            // Execute shell
            let shell_cstr = CString::new(shell)?;
            let args = [shell_cstr.clone()];
            execvp(&shell_cstr, &args)?;
        }
    }

    Ok(())
}

async fn handle_io(
    master_fd: i32,
    session_id: Option<String>,
    ws_tx: mpsc::Sender<ClientMessage>,
    running: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Convert to async file descriptors
    let master_read = tokio::fs::File::from_std(unsafe {
        std::fs::File::from_raw_fd(libc::dup(master_fd))
    });
    let master_write = tokio::fs::File::from_std(unsafe {
        std::fs::File::from_raw_fd(libc::dup(master_fd))
    });

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut master_read = tokio::io::BufReader::new(master_read);
    let mut master_write = master_write;
    let mut stdin = tokio::io::BufReader::new(stdin);

    let mut pty_buf = vec![0u8; 4096];
    let mut stdin_buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            // Read from PTY and write to stdout + WebSocket
            result = master_read.read(&mut pty_buf) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = &pty_buf[..n];

                        // Write to stdout
                        stdout.write_all(data).await?;
                        stdout.flush().await?;

                        // Send to WebSocket if connected
                        if let Some(ref sid) = session_id {
                            let output = String::from_utf8_lossy(data).to_string();
                            let msg = ClientMessage::TerminalOutput {
                                session_id: sid.clone(),
                                output,
                                stream: "stdout".to_string(),
                            };
                            let _ = ws_tx.send(msg).await;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(_) => break,
                }
            }

            // Read from stdin and write to PTY + WebSocket
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = &stdin_buf[..n];

                        // Write to PTY
                        master_write.write_all(data).await?;
                        master_write.flush().await?;

                        // Send to WebSocket if connected
                        if let Some(ref sid) = session_id {
                            let input = String::from_utf8_lossy(data).to_string();
                            let msg = ClientMessage::TerminalInput {
                                session_id: sid.clone(),
                                input,
                            };
                            let _ = ws_tx.send(msg).await;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    running.store(false, Ordering::SeqCst);
    Ok(())
}

fn get_terminal_size() -> Winsize {
    let mut winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    unsafe {
        libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut winsize);
    }

    winsize
}
