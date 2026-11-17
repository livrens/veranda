//! Interactive terminals: an SSH shell channel or a local pseudo-terminal, both driven through
//! the same [`TermHandle`] / [`TermEvent`] pair.

use crate::client::Connection;
use anyhow::{Context, Result};
use russh::ChannelMsg;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

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
    /// A secret asked for by hand; the terminal's own loop decides whether it goes out.
    Secret {
        source: SecretSource,
        enter: bool,
        reply: oneshot::Sender<SecretReply>,
    },
}

/// Outcome of [`TermHandle::answer_prompt`]. Only `Sent` means that bytes went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretReply {
    Sent,
    /// No open password prompt, output still moving, or startup steps still running.
    NotAsked,
    /// A prompt is open but the source had no secret to give.
    NoSecret,
    /// The terminal is gone or the write failed.
    Failed,
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
    /// Type a secret (with Enter when `enter`), but only while the terminal shows an open
    /// password prompt and its output has been quiet for 400 ms. `source` is not called
    /// otherwise, and one prompt gets one answer.
    pub async fn answer_prompt(&self, source: SecretSource, enter: bool) -> SecretReply {
        let (reply, rx) = oneshot::channel();
        let input = Input::Secret {
            source,
            enter,
            reply,
        };
        if self.tx.send(input).await.is_err() {
            return SecretReply::Failed;
        }
        rx.await.unwrap_or(SecretReply::Failed)
    }
}

pub struct ShellOptions {
    pub cols: u16,
    pub rows: u16,
    pub agent_forwarding: bool,
    /// `NAME=value` pairs requested through `env` (servers usually allow only `LANG`/`LC_*`).
    pub env: Vec<String>,
    /// Steps sent one after another once the shell starts.
    pub startup: Option<StartupPlan>,
}

pub struct StartupPlan {
    pub steps: Vec<ShellStep>,
    /// Told when a step is skipped or the rest of the steps is cancelled.
    pub notify: Box<dyn Fn(StartupNotice) + Send + Sync>,
}

/// Hands out a secret at the moment it is needed; `None` when there is none.
pub type SecretSource = Box<dyn FnOnce() -> Option<String> + Send>;

pub enum ShellStep {
    /// Text typed into the shell. The first step goes out at once, later ones when the output
    /// has been quiet for 400 ms (or after 10 s).
    Text(String),
    /// Answer a password prompt, once. Nothing is sent unless a prompt is on screen.
    Password(SecretSource),
    /// Like `Password`, but only a prompt that carries the `[sudo]` mark is answered.
    Sudo(SecretSource),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupNotice {
    /// No password prompt showed up in time: step skipped, nothing sent, next step goes on.
    NoPrompt,
    /// A prompt showed up but there is no saved password: the remaining steps are cancelled.
    NoPassword,
    /// The prompt came back after the password was sent: no retry, remaining steps cancelled.
    Rejected,
}

const QUIET: Duration = Duration::from_millis(400);
const STEP_WAIT: Duration = Duration::from_secs(10);
const PROMPT_WAIT: Duration = Duration::from_secs(15);
/// Output kept for prompt detection.
const TAIL_MAX: usize = 2048;
const PROMPT_MAX: usize = 200;

enum Due {
    Send(String),
    Secret(SecretSource),
    Notice(StartupNotice),
}

/// The startup sequence of one shell. Holds no secret: only the sources that can fetch one.
struct Startup {
    steps: VecDeque<ShellStep>,
    /// Output since the last step or secret was sent, capped at [`TAIL_MAX`]. Kept after the
    /// sequence is over, for [`Startup::asked`].
    tail: Vec<u8>,
    since: Instant,
    last_out: Instant,
    /// A password was just sent: watching whether the prompt comes back.
    verify: bool,
    /// The current tail was looked at already: wait for more output or the time limit.
    checked: bool,
}

impl Startup {
    fn new(steps: Vec<ShellStep>) -> Self {
        let now = Instant::now();
        Self {
            steps: steps
                .into_iter()
                .filter(|s| !matches!(s, ShellStep::Text(t) if t.trim().is_empty()))
                .collect(),
            tail: Vec::new(),
            since: now,
            last_out: now,
            verify: false,
            checked: false,
        }
    }

    fn active(&self) -> bool {
        self.verify || !self.steps.is_empty()
    }

    /// The first step when it is text: it goes out as soon as the shell is open.
    fn first_text(&mut self) -> Option<String> {
        match self.steps.pop_front()? {
            ShellStep::Text(t) => Some(t),
            step => {
                self.steps.push_front(step);
                None
            }
        }
    }

    fn feed(&mut self, data: &[u8]) {
        keep_tail(&mut self.tail, data);
        self.last_out = Instant::now();
        self.checked = false;
    }

    /// A secret asked for by hand may go out: the steps are over and an open password prompt
    /// has been on screen for [`QUIET`].
    fn asked(&self, now: Instant) -> bool {
        !self.active() && prompt_open(&self.tail, self.last_out, now)
    }

    fn limit(&self) -> Instant {
        let wait = match self.steps.front() {
            Some(ShellStep::Password(_) | ShellStep::Sudo(_)) if !self.verify => PROMPT_WAIT,
            _ => STEP_WAIT,
        };
        self.since + wait
    }

    /// When to look again; `None` once the sequence is over, so no timer keeps running.
    fn wake(&self) -> Option<Instant> {
        if !self.active() {
            return None;
        }
        let limit = self.limit();
        Some(if self.checked {
            limit
        } else {
            limit.min(self.last_out + QUIET)
        })
    }

    fn sent(&mut self, now: Instant) {
        self.tail.clear();
        self.since = now;
        self.last_out = now;
        self.checked = false;
    }

    fn cancel(&mut self) {
        self.steps.clear();
        self.verify = false;
    }

    fn due(&mut self, now: Instant) -> Option<Due> {
        let quiet = now >= self.last_out + QUIET;
        if self.verify {
            // sudo answers a wrong password only after a pause, so quiet alone is not enough:
            // something must have been printed since the password went out
            if quiet && has_text(&self.tail) {
                self.verify = false;
                if is_password_prompt(&self.tail) {
                    self.cancel();
                    return Some(Due::Notice(StartupNotice::Rejected));
                }
            } else if now >= self.limit() {
                self.verify = false;
            } else {
                self.checked = quiet;
                return None;
            }
        }
        let timed_out = now >= self.limit();
        let ready = match self.steps.front()? {
            ShellStep::Text(_) => quiet || timed_out,
            ShellStep::Password(_) => quiet && is_password_prompt(&self.tail),
            ShellStep::Sudo(_) => quiet && is_sudo_prompt(&self.tail),
        };
        if ready {
            self.sent(now);
            return match self.steps.pop_front()? {
                ShellStep::Text(text) => Some(Due::Send(text)),
                ShellStep::Password(source) | ShellStep::Sudo(source) => {
                    self.verify = true;
                    Some(Due::Secret(source))
                }
            };
        }
        if timed_out {
            // only a password step gets here: its prompt never came
            self.steps.pop_front();
            return Some(Due::Notice(StartupNotice::NoPrompt));
        }
        self.checked = quiet;
        None
    }
}

/// Terminal output as plain text: escape sequences and control characters removed, line ends
/// kept.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            if c == '\n' || c == '\r' || !c.is_control() {
                out.push(c);
            }
            continue;
        }
        match it.next() {
            // CSI: parameters, then one final character
            Some('[') => {
                for x in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&x) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \
            Some(']') => {
                while let Some(x) = it.next() {
                    if x == '\x07' {
                        break;
                    }
                    if x == '\x1b' {
                        it.next();
                        break;
                    }
                }
            }
            // character set selection carries one more character
            Some('(' | ')') => {
                it.next();
            }
            _ => {}
        }
    }
    out
}

/// Append output to a tail capped at [`TAIL_MAX`].
fn keep_tail(tail: &mut Vec<u8>, data: &[u8]) {
    if data.len() >= TAIL_MAX {
        tail.clear();
        tail.extend_from_slice(&data[data.len() - TAIL_MAX..]);
        return;
    }
    tail.extend_from_slice(data);
    if tail.len() > TAIL_MAX {
        let cut = tail.len() - TAIL_MAX;
        tail.drain(..cut);
    }
}

fn prompt_open(tail: &[u8], last_out: Instant, now: Instant) -> bool {
    now >= last_out + QUIET && is_password_prompt(tail)
}

fn has_text(tail: &[u8]) -> bool {
    plain(tail).chars().any(|c| !c.is_whitespace())
}

/// `true` when the output ends in an open password prompt: the last line has no line end yet,
/// ends with a colon (spaces may follow), is at most [`PROMPT_MAX`] characters long and has
/// the whole word "password" (any case) or "sandi".
fn is_password_prompt(tail: &[u8]) -> bool {
    prompt_line(tail).is_some()
}

/// An open password prompt printed by sudo: its line carries the `[sudo]` mark. Prompts of
/// su, ssh or a custom `sudo -p` do not count.
fn is_sudo_prompt(tail: &[u8]) -> bool {
    prompt_line(tail).is_some_and(|l| l.contains("[sudo]"))
}

/// The last line of `tail` when it is an open password prompt.
fn prompt_line(tail: &[u8]) -> Option<String> {
    let text = plain(tail);
    let line = text
        .rsplit(['\n', '\r'])
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let open = is_prompt_text(line);
    open.then(|| line.to_string())
}

fn is_prompt_text(line: &str) -> bool {
    if line.chars().count() > PROMPT_MAX {
        return false;
    }
    let Some(body) = line.strip_suffix(':') else {
        return false;
    };
    body.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "password" || w == "sandi")
}

fn with_newline(text: String) -> Vec<u8> {
    let mut bytes = text.into_bytes();
    if bytes.last() != Some(&b'\n') {
        bytes.push(b'\n');
    }
    bytes
}

/// Overwrite a secret before its buffer is freed.
fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    std::hint::black_box(buf);
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
    let (steps, notify) = match opts.startup {
        Some(p) => (p.steps, Some(p.notify)),
        None => (vec![], None),
    };
    let mut seq = Startup::new(steps);
    if let Some(text) = seq.first_text() {
        let _ = tx.send(Input::Data(with_newline(text))).await;
    }
    tokio::spawn(async move {
        let _conn = conn;
        let mut exit = None;
        let notice = |n| {
            if let Some(f) = &notify {
                f(n)
            }
        };
        loop {
            let wake = seq.wake();
            tokio::select! {
                msg = read.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => {
                        seq.feed(&data);
                        if sink.send(TermEvent::Data(data.to_vec())).await.is_err() { break; }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        seq.feed(&data);
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
                    Some(Input::Secret { source, enter, reply }) => {
                        let now = Instant::now();
                        let asked = seq.asked(now);
                        let outcome = match asked.then(source).flatten() {
                            Some(secret) => {
                                seq.sent(now);
                                // written straight to the channel: never queued, logged or echoed here
                                let mut bytes = secret.into_bytes();
                                let mut sent = write.data(&bytes[..]).await.is_ok();
                                wipe(&mut bytes);
                                drop(bytes);
                                if enter {
                                    sent = sent && write.data(&b"\n"[..]).await.is_ok();
                                }
                                if sent { SecretReply::Sent } else { SecretReply::Failed }
                            }
                            None if asked => SecretReply::NoSecret,
                            None => SecretReply::NotAsked,
                        };
                        let _ = reply.send(outcome);
                        if outcome == SecretReply::Failed {
                            // fixed text: the library's error is not passed on for this write
                            let _ = sink.send(TermEvent::Error("sending to the terminal failed".into())).await;
                            break;
                        }
                    }
                    Some(Input::Resize(c, r)) => { let _ = write.window_change(c.max(2) as u32, r.max(1) as u32, 0, 0).await; }
                    Some(Input::Close) | None => { let _ = write.eof().await; let _ = write.close().await; break; }
                },
                _ = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                    let sent = match seq.due(Instant::now()) {
                        Some(Due::Send(text)) => write
                            .data(&with_newline(text)[..])
                            .await
                            .map_err(|e| e.to_string()),
                        Some(Due::Secret(source)) => match source() {
                            Some(secret) => {
                                // written straight to the channel: never queued, logged or echoed here
                                let mut bytes = secret.into_bytes();
                                let mut sent = write.data(&bytes[..]).await.is_ok();
                                wipe(&mut bytes);
                                drop(bytes);
                                sent = sent && write.data(&b"\n"[..]).await.is_ok();
                                // fixed text: the library's error is not passed on for this write
                                sent.then_some(()).ok_or_else(|| "sending a startup step failed".to_string())
                            }
                            None => {
                                seq.cancel();
                                notice(StartupNotice::NoPassword);
                                Ok(())
                            }
                        },
                        Some(Due::Notice(n)) => {
                            notice(n);
                            Ok(())
                        }
                        None => Ok(()),
                    };
                    if let Err(e) = sent {
                        let _ = sink.send(TermEvent::Error(e)).await;
                        break;
                    }
                }
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

    // last output and when it came, shared by the two threads below for `Input::Secret`
    let seen = Arc::new(Mutex::new((Vec::new(), Instant::now())));
    let seen_out = seen.clone();

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
                        {
                            let mut seen = seen_out.lock().unwrap_or_else(|e| e.into_inner());
                            keep_tail(&mut seen.0, &buf[..n]);
                            seen.1 = Instant::now();
                        }
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
                    Some(Input::Secret {
                        source,
                        enter,
                        reply,
                    }) => {
                        let asked = {
                            let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
                            let asked = prompt_open(&seen.0, seen.1, Instant::now());
                            if asked {
                                seen.0.clear();
                            }
                            asked
                        };
                        let outcome = match asked.then(source).flatten() {
                            Some(secret) => {
                                let mut bytes = secret.into_bytes();
                                let mut sent = writer.write_all(&bytes).is_ok();
                                wipe(&mut bytes);
                                drop(bytes);
                                if enter {
                                    sent = sent && writer.write_all(b"\n").is_ok();
                                }
                                if sent && writer.flush().is_ok() {
                                    SecretReply::Sent
                                } else {
                                    SecretReply::Failed
                                }
                            }
                            None if asked => SecretReply::NoSecret,
                            None => SecretReply::NotAsked,
                        };
                        let _ = reply.send(outcome);
                        if outcome == SecretReply::Failed {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn password_prompt_detection() {
        for yes in [
            "[sudo] password for deploy: ",
            "\x1b[?2004l\rPassword:",
            "Last login\r\n[sudo] kata sandi untuk deploy: ",
            "\x1b]0;title\x07\x1b[1mPASSWORD:\x1b[0m ",
        ] {
            assert!(is_password_prompt(yes.as_bytes()), "{yes:?}");
        }
        for no in [
            "",
            "[sudo] password for deploy: \r\n",
            concat!("Password:", " \r\nSorry, try again.\r\n"),
            "deploy@web:~$ ",
            "sandiaga@web:",
            "passwords:",
            "Enter the password",
            "Password: yes",
        ] {
            assert!(!is_password_prompt(no.as_bytes()), "{no:?}");
        }
        let long = format!("{} password:", "x".repeat(PROMPT_MAX));
        assert!(!is_password_prompt(long.as_bytes()));
    }

    fn sudo_sequence(asked: &Arc<AtomicUsize>) -> Startup {
        let n = asked.clone();
        let mut s = Startup::new(vec![
            ShellStep::Text("sudo -i".into()),
            ShellStep::Password(Box::new(move || {
                n.fetch_add(1, Ordering::Relaxed);
                Some("x".into())
            })),
            ShellStep::Text("whoami".into()),
        ]);
        assert_eq!(s.first_text().as_deref(), Some("sudo -i"));
        s
    }

    #[test]
    fn password_goes_out_only_at_a_prompt() {
        let asked = Arc::new(AtomicUsize::new(0));
        let mut s = sudo_sequence(&asked);
        s.feed(b"deploy@web:~$ sudo -i\r\n");
        assert!(s.due(s.last_out + QUIET).is_none());
        s.feed(b"[sudo] password for deploy: ");
        assert!(s.due(s.last_out).is_none());
        assert!(matches!(s.due(s.last_out + QUIET), Some(Due::Secret(_))));
        // the line end sudo prints is not an answer yet
        s.feed(b"\r\n");
        assert!(s.due(s.last_out + QUIET).is_none());
        s.feed(b"root@web:~# ");
        assert!(matches!(s.due(s.last_out + QUIET), Some(Due::Send(t)) if t == "whoami"));
        assert!(!s.active() && s.wake().is_none());
        assert_eq!(asked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rejected_password_cancels_the_rest() {
        let asked = Arc::new(AtomicUsize::new(0));
        let mut s = sudo_sequence(&asked);
        s.feed(b"[sudo] password for deploy: ");
        assert!(matches!(s.due(s.last_out + QUIET), Some(Due::Secret(_))));
        s.feed(b"\r\nSorry, try again.\r\n[sudo] password for deploy: ");
        assert!(matches!(
            s.due(s.last_out + QUIET),
            Some(Due::Notice(StartupNotice::Rejected))
        ));
        assert!(!s.active() && s.wake().is_none());
    }

    #[test]
    fn missing_prompt_skips_the_password_step() {
        let asked = Arc::new(AtomicUsize::new(0));
        let mut s = sudo_sequence(&asked);
        s.feed(b"root@web:~# ");
        assert!(s.due(s.last_out + QUIET).is_none());
        let late = s.since + PROMPT_WAIT;
        assert!(matches!(
            s.due(late),
            Some(Due::Notice(StartupNotice::NoPrompt))
        ));
        assert!(matches!(s.due(late), Some(Due::Send(t)) if t == "whoami"));
        assert!(!s.active());
        assert_eq!(asked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn secret_by_hand_needs_an_open_quiet_prompt() {
        let mut s = Startup::new(vec![]);
        assert!(!s.asked(s.last_out + QUIET));
        s.feed(b"deploy@web:~$ ");
        assert!(!s.asked(s.last_out + QUIET));
        s.feed(b"cat notes\r\nthe password: is not here\r\ndeploy@web:~$ ");
        assert!(!s.asked(s.last_out + QUIET));
        s.feed(b"sudo -i\r\n[sudo] password for deploy: ");
        // output still moving
        assert!(!s.asked(s.last_out));
        assert!(s.asked(s.last_out + QUIET));
        // the kept output starts no timer
        assert!(s.wake().is_none());
        // one prompt, one answer
        s.sent(s.last_out + QUIET);
        assert!(!s.asked(s.last_out + QUIET));
        s.feed(b"\r\nroot@web:~# ");
        assert!(!s.asked(s.last_out + QUIET));
    }

    #[test]
    fn secret_by_hand_waits_for_the_startup_steps() {
        let asked = Arc::new(AtomicUsize::new(0));
        let mut s = sudo_sequence(&asked);
        s.feed(b"[sudo] password for deploy: ");
        assert!(!s.asked(s.last_out + QUIET));
    }

    #[test]
    fn sudo_step_answers_only_a_sudo_prompt() {
        assert!(is_sudo_prompt(b"[sudo] password for deploy: "));
        assert!(is_sudo_prompt(
            "[sudo] kata sandi untuk deploy: ".as_bytes()
        ));
        for no in [
            "Password: ",
            "deploy@web's password: ",
            "Kata sandi: ",
            "[sudo] password for deploy: \r\n",
            "[sudo] ok\r\nPassword: ",
        ] {
            assert!(!is_sudo_prompt(no.as_bytes()), "{no:?}");
        }
        let mut s = Startup::new(vec![
            ShellStep::Text("su -".into()),
            ShellStep::Sudo(Box::new(|| Some("x".into()))),
        ]);
        assert_eq!(s.first_text().as_deref(), Some("su -"));
        s.feed(b"Password: ");
        assert!(s.due(s.last_out + QUIET).is_none());
        assert!(matches!(
            s.due(s.since + PROMPT_WAIT),
            Some(Due::Notice(StartupNotice::NoPrompt))
        ));
        assert!(!s.active());
    }

    #[test]
    fn kept_output_is_capped() {
        let mut s = Startup::new(vec![]);
        s.feed(&vec![b'x'; TAIL_MAX - 1]);
        s.feed(b"\r\n");
        s.feed(&vec![b'y'; 3 * TAIL_MAX]);
        assert_eq!(s.tail.len(), TAIL_MAX);
        s.feed(b"\r\nPassword: ");
        assert_eq!(s.tail.len(), TAIL_MAX);
        assert!(s.asked(s.last_out + QUIET));
    }
}
