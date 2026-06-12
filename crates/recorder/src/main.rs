//! `hermione` — a transparent terminal recorder.
//!
//! Works like the classic `script` command: it spawns the user's shell inside
//! a pseudo-terminal and faithfully forwards I/O in both directions, so the
//! session feels completely native. In parallel it tees every byte to the
//! Hermione backend over gRPC, both for live observation and long-term storage.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use hermione_proto::v1::{
    ingest_client::IngestClient,
    ingest_event::Event,
    IngestEvent, Resize, SessionEnd, SessionStart, StreamKind, TerminalChunk,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Transparent terminal recorder that streams to the Hermione backend.
#[derive(Parser, Debug)]
#[command(name = "hermione", version, about)]
struct Args {
    /// Backend gRPC endpoint.
    #[arg(long, env = "HERMIONE_BACKEND", default_value = "http://127.0.0.1:50051")]
    backend: String,

    /// Student identifier (defaults to $USER).
    #[arg(long, env = "HERMIONE_STUDENT")]
    student: Option<String>,

    /// Record locally without sending anything to a backend.
    #[arg(long)]
    offline: bool,

    /// Command to record (defaults to $SHELL, or /bin/bash). Pass after `--`.
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

/// Restores the terminal's cooked mode on drop, even on panic.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let student = args
        .student
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());

    let (prog, prog_args) = resolve_command(&args.command);
    let command_line = std::iter::once(prog.clone())
        .chain(prog_args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    let (cols, rows) = match crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => (c, r),
        _ => (80, 24),
    };

    // ---- Spawn the recorded process inside a PTY ----------------------------
    let pty = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new(&prog);
    for arg in &prog_args {
        cmd.arg(arg);
    }
    // Inherit the full environment so the shell behaves normally.
    for (key, value) in std::env::vars() {
        cmd.env(key, value);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let mut child = pty.slave.spawn_command(cmd)?;
    // Drop the slave so the master sees EOF once the child exits.
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader()?;
    let mut writer = pty.master.take_writer()?;
    let master = pty.master;

    // ---- Wire up the backend ingest stream ----------------------------------
    let (tx, rx) = mpsc::channel::<IngestEvent>(2048);
    let stream = ReceiverStream::new(rx);

    let backend_task = if args.offline {
        drop(stream);
        None
    } else {
        match IngestClient::connect(args.backend.clone()).await {
            Ok(mut client) => Some(tokio::spawn(async move {
                if let Err(e) = client.stream_session(stream).await {
                    eprintln!("\r\nhermione: ingest stream ended with error: {e}");
                }
            })),
            Err(e) => {
                eprintln!(
                    "hermione: could not reach backend {} ({e}); recording locally only.",
                    args.backend
                );
                drop(stream);
                None
            }
        }
    };

    let start = Instant::now();

    // First event: session metadata.
    let _ = tx
        .send(IngestEvent {
            event: Some(Event::Start(SessionStart {
                student: student.clone(),
                command: command_line.clone(),
                cols: cols as u32,
                rows: rows as u32,
                hostname,
            })),
        })
        .await;

    eprintln!(
        "hermione: recording '{}' as '{}'{}",
        command_line,
        student,
        if backend_task.is_some() {
            format!(" -> {}", args.backend)
        } else {
            " (offline)".to_string()
        }
    );

    // ---- Enter raw mode and start shuttling bytes ---------------------------
    crossterm::terminal::enable_raw_mode()?;
    let _raw_guard = RawModeGuard;

    // PTY output -> real stdout (+ tee to backend).
    let tx_out = tx.clone();
    let out_thread = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                    let _ = tx_out.try_send(chunk(StreamKind::Stdout, &buf[..n], &start));
                }
            }
        }
    });

    // Real stdin -> PTY input (+ tee to backend).
    let tx_in = tx.clone();
    let _in_thread = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                    let _ = tx_in.try_send(chunk(StreamKind::Stdin, &buf[..n], &start));
                }
            }
        }
    });

    // Forward terminal resizes to the PTY and the backend.
    #[cfg(unix)]
    {
        let tx_resize = tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let Ok(mut winch) = signal(SignalKind::window_change()) else {
                return;
            };
            while winch.recv().await.is_some() {
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = master.resize(PtySize {
                        rows: r,
                        cols: c,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                    let _ = tx_resize
                        .send(IngestEvent {
                            event: Some(Event::Resize(Resize {
                                cols: c as u32,
                                rows: r as u32,
                            })),
                        })
                        .await;
                }
            }
        });
    }

    // ---- Wait for the recorded process to exit ------------------------------
    let status = tokio::task::spawn_blocking(move || child.wait()).await??;
    let exit_code = status.exit_code() as i32;

    // The PTY read side EOFs on its own; make sure the stdout pump is done.
    let _ = out_thread.join();

    let _ = tx
        .send(IngestEvent {
            event: Some(Event::End(SessionEnd { exit_code })),
        })
        .await;
    drop(tx);

    // Give the backend a brief moment to flush the final events. The stdin
    // pump thread may still be blocked on read(), so we don't join it.
    if let Some(task) = backend_task {
        let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
    }

    crossterm::terminal::disable_raw_mode().ok();
    eprintln!("hermione: session ended (exit code {exit_code}).");
    std::process::exit(exit_code);
}

/// Builds a `TerminalChunk` ingest event for a slice of bytes.
fn chunk(stream: StreamKind, data: &[u8], start: &Instant) -> IngestEvent {
    IngestEvent {
        event: Some(Event::Chunk(TerminalChunk {
            stream: stream as i32,
            data: data.to_vec(),
            offset_ms: start.elapsed().as_millis() as i64,
        })),
    }
}

/// Resolves the program and arguments to record, defaulting to the user's shell.
fn resolve_command(command: &[String]) -> (String, Vec<String>) {
    if let Some((prog, rest)) = command.split_first() {
        (prog.clone(), rest.to_vec())
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        (shell, Vec::new())
    }
}
