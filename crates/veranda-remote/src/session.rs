//! Interactive terminals: an SSH shell channel or a local pseudo-terminal, both driven through
//! the same [`TermHandle`] / [`TermEvent`] pair.

use crate::client::Connection;
use anyhow::{Context, Result};
use russh::ChannelMsg;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum TermEvent {
    Data(Vec<u8>),
    Exit(Option<u32>),
    Error(String),
}

enum Input {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

#[derive(Clone)]
pub struct TermHandle {
    tx: mpsc::Sender<Input>,
}

impl TermHandle {
    pub async fn write(&self, data: Vec<u8>) {
        let _ = self.tx.send(Input::Data(data)).await;
    }
    pub async fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(Input::Resize(cols, rows)).await;
    }
    pub async fn close(&self) {
        let _ = self.tx.send(Input::Close).await;
    }
}

pub struct ShellOptions {
    pub cols: u16,
    pub rows: u16,
    pub agent_forwarding: bool,
    /// `NAME=value` pairs requested through `env` (servers usually allow only `LANG`/`LC_*`).
    pub env: Vec<String>,
    /// Text sent right after the shell starts (startup snippet).
    pub startup: Option<String>,
}

/// Open a PTY + shell on the connection and pump bytes both ways until it ends.
pub async fn open_ssh_shell(
    conn: Arc<Connection>,
    opts: ShellOptions,
    sink: mpsc::Sender<TermEvent>,
) -> Result<TermHandle> {
    let ch = conn
        .open_session()
        .await
        .context("opening a session channel")?;
    if opts.agent_forwarding {
        let _ = ch.agent_forward(false).await;
    }
    for kv in &opts.env {
        if let Some((k, v)) = kv.split_once('=') {
            let _ = ch.set_env(false, k.trim(), v.trim()).await;
        }
    }
    ch.request_pty(
        true,
        "xterm-256color",
        opts.cols.max(2) as u32,
        opts.rows.max(1) as u32,
        0,
        0,
        &[],
    )
    .await
    .context("requesting a terminal")?;
    ch.request_shell(true).await.context("starting the shell")?;
    let (tx, mut rx) = mpsc::channel::<Input>(256);
    let (mut read, write) = ch.split();
    if let Some(text) = opts.startup.filter(|t| !t.trim().is_empty()) {
        let mut bytes = text.into_bytes();
        if bytes.last() != Some(&b'\n') {
            bytes.push(b'\n');
        }
        let _ = tx.send(Input::Data(bytes)).await;
    }
    tokio::spawn(async move {
        let _conn = conn;
        let mut exit = None;
        loop {
            tokio::select! {
                msg = read.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if sink.send(TermEvent::Data(data.to_vec())).await.is_err() { break; }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if sink.send(TermEvent::Data(data.to_vec())).await.is_err() { break; }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                    Some(ChannelMsg::Close) | None => break,
                    Some(_) => {}
                },
                input = rx.recv() => match input {
                    Some(Input::Data(d)) => {
                        if let Err(e) = write.data(&d[..]).await {
                            let _ = sink.send(TermEvent::Error(e.to_string())).await;
                            break;
                        }
                    }
                    Some(Input::Resize(c, r)) => { let _ = write.window_change(c.max(2) as u32, r.max(1) as u32, 0, 0).await; }
                    Some(Input::Close) | None => { let _ = write.eof().await; let _ = write.close().await; break; }
                },
            }
        }
        let _ = sink.send(TermEvent::Exit(exit)).await;
    });
    Ok(TermHandle { tx })
}

pub struct LocalOptions {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Spawn a local shell in a pseudo-terminal. Blocking PTY I/O runs on plain threads.
pub fn open_local(opts: LocalOptions, sink: mpsc::Sender<TermEvent>) -> Result<TermHandle> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: opts.rows.max(1),
            cols: opts.cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("opening a pseudo-terminal: {e}"))?;
    let mut cmd = CommandBuilder::new(&opts.program);
    cmd.args(&opts.args);
    if let Some(d) = &opts.cwd {
        cmd.cwd(d);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Veranda");
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("starting {}: {e}", opts.program))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("pty reader: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("pty writer: {e}"))?;
    let master = pair.master;
    let (tx, mut rx) = mpsc::channel::<Input>(256);

    // exit code: one thread waits for the child
    let (exit_tx, exit_rx) = std::sync::mpsc::channel::<Option<u32>>();
    let mut killer = child.clone_killer();
    std::thread::Builder::new()
        .name("veranda-pty-wait".into())
        .spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code());
            let _ = exit_tx.send(code);
        })
        .context("spawning the pty wait thread")?;

    // output: blocking reads forwarded to the async sink; EOF means the shell is gone
    let sink_out = sink;
    std::thread::Builder::new()
        .name("veranda-pty-read".into())
        .spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink_out
                            .blocking_send(TermEvent::Data(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let code = exit_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(None);
            let _ = sink_out.blocking_send(TermEvent::Exit(code));
        })
        .context("spawning the pty reader thread")?;

    // input + resize: this thread owns the master side
    std::thread::Builder::new()
        .name("veranda-pty-ctl".into())
        .spawn(move || {
            use std::io::Write;
            loop {
                match rx.blocking_recv() {
                    Some(Input::Data(d)) => {
                        if writer.write_all(&d).and_then(|_| writer.flush()).is_err() {
                            break;
                        }
                    }
                    Some(Input::Resize(cols, rows)) => {
                        let _ = master.resize(PtySize {
                            rows: rows.max(1),
                            cols: cols.max(2),
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    Some(Input::Close) | None => break,
                }
            }
            drop(writer);
            let _ = killer.kill();
            drop(master);
        })
        .context("spawning the pty control thread")?;
    Ok(TermHandle { tx })
}
