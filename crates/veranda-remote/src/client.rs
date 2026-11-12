//! SSH connections: host-key trust, authentication (key, password, agent, keyboard-interactive),
//! jump hosts, and one-shot command execution.

use crate::known_hosts;
use anyhow::{Context, Result, anyhow, bail};
use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse, Msg};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Disconnect};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    Unknown,
    Changed,
}

#[derive(Debug, Clone)]
pub struct HostKeyRequest {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub status: HostKeyStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    Passphrase,
    /// A keyboard-interactive prompt (one-time codes, PAM...).
    Interactive,
}

#[derive(Debug, Clone)]
pub struct SecretRequest {
    pub kind: SecretKind,
    pub title: String,
    pub prompt: String,
    pub echo: bool,
}

/// Questions a connection may ask the user. The desktop app forwards them to the UI.
pub trait Prompter: Send + Sync {
    fn host_key(&self, req: HostKeyRequest) -> BoxFut<'_, bool>;
    /// `None` = cancelled.
    fn secret(&self, req: SecretRequest) -> BoxFut<'_, Option<String>>;
    /// Informational text (server banner, progress).
    fn notice(&self, text: String) -> BoxFut<'_, ()>;
}

/// A prompter that accepts nothing and answers nothing (tests, batch jobs).
pub struct Silent;
impl Prompter for Silent {
    fn host_key(&self, _req: HostKeyRequest) -> BoxFut<'_, bool> {
        Box::pin(async { false })
    }
    fn secret(&self, _req: SecretRequest) -> BoxFut<'_, Option<String>> {
        Box::pin(async { None })
    }
    fn notice(&self, _text: String) -> BoxFut<'_, ()> {
        Box::pin(async {})
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub label: String,
}

pub enum AuthMethod {
    Key { key: Arc<PrivateKey>, label: String },
    Password(String),
    AskPassword,
    Agent,
    Interactive,
}

pub struct Auth {
    pub username: String,
    pub methods: Vec<AuthMethod>,
}

pub struct ConnectRequest {
    pub target: Target,
    pub auth: Auth,
    /// Hop before the target (ProxyJump); only one level here, the caller chains more.
    pub jump: Option<Box<ConnectRequest>>,
    pub keepalive_secs: u32,
    pub known_hosts: PathBuf,
    /// Seconds to wait for TCP + key exchange.
    pub timeout_secs: u64,
}

/// A channel the server opened towards us (remote port forwarding).
pub struct ForwardedChannel {
    pub channel: Channel<Msg>,
    pub connected_address: String,
    pub connected_port: u32,
    pub originator: String,
}

pub struct Connection {
    pub handle: Mutex<Handle<ClientHandler>>,
    pub forwarded: Mutex<mpsc::Receiver<ForwardedChannel>>,
    pub target: Target,
    pub username: String,
    /// Which method finally succeeded (`key:<label>`, `password`, `agent`, `interactive`).
    pub auth_used: String,
    /// Password typed by the user during this login (so the caller may offer to save it).
    pub typed_password: Option<String>,
    _jump: Option<Arc<Connection>>,
}

impl Connection {
    pub async fn open_session(&self) -> Result<Channel<Msg>> {
        Ok(self.handle.lock().await.channel_open_session().await?)
    }

    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        originator: &str,
        originator_port: u16,
    ) -> Result<Channel<Msg>> {
        Ok(self
            .handle
            .lock()
            .await
            .channel_open_direct_tcpip(host, port as u32, originator, originator_port as u32)
            .await?)
    }

    pub async fn is_closed(&self) -> bool {
        self.handle.lock().await.is_closed()
    }

    pub async fn disconnect(&self) {
        let h = self.handle.lock().await;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            h.disconnect(Disconnect::ByApplication, "bye", "en"),
        )
        .await;
    }
}

pub struct ClientHandler {
    prompter: Arc<dyn Prompter>,
    target: Target,
    known_hosts: PathBuf,
    forwarded_tx: mpsc::Sender<ForwardedChannel>,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn auth_banner(&mut self, banner: &str, _session: &mut client::Session) -> Result<()> {
        let text = banner.trim();
        if !text.is_empty() {
            self.prompter.notice(text.to_string()).await;
        }
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool> {
        let key = server_public_key.public_key();
        let (host, port) = (self.target.host.as_str(), self.target.port);
        let (status, replace) = match known_hosts::check(&self.known_hosts, host, port, &key) {
            known_hosts::Status::Known => return Ok(true),
            known_hosts::Status::Unknown => (HostKeyStatus::Unknown, None),
            known_hosts::Status::Changed { line } => (HostKeyStatus::Changed, Some(line)),
        };
        let accepted = self
            .prompter
            .host_key(HostKeyRequest {
                host: host.to_string(),
                port,
                algorithm: key.algorithm().to_string(),
                fingerprint: crate::keys::fingerprint(&key),
                status,
            })
            .await;
        if !accepted {
            return Ok(false);
        }
        known_hosts::learn(&self.known_hosts, host, port, &key, replace)?;
        Ok(true)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<()> {
        reply.accept().await;
        let _ = self
            .forwarded_tx
            .send(ForwardedChannel {
                channel,
                connected_address: connected_address.to_string(),
                connected_port,
                originator: format!("{originator_address}:{originator_port}"),
            })
            .await;
        Ok(())
    }
}

fn config(keepalive_secs: u32) -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: (keepalive_secs > 0)
            .then(|| Duration::from_secs(keepalive_secs as u64)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

/// Connect and authenticate. Jump hosts are connected first; the target then runs over a
/// `direct-tcpip` channel of the hop.
pub fn connect(
    req: ConnectRequest,
    prompter: Arc<dyn Prompter>,
) -> BoxFut<'static, Result<Arc<Connection>>> {
    Box::pin(async move {
        let jump = match req.jump {
            Some(j) => Some(connect(*j, prompter.clone()).await?),
            None => None,
        };
        let (forwarded_tx, forwarded_rx) = mpsc::channel(32);
        let handler = ClientHandler {
            prompter: prompter.clone(),
            target: req.target.clone(),
            known_hosts: req.known_hosts.clone(),
            forwarded_tx,
        };
        let cfg = config(req.keepalive_secs);
        let timeout = Duration::from_secs(req.timeout_secs.max(5));
        let mut handle = match &jump {
            Some(j) => {
                let ch = j
                    .open_direct_tcpip(&req.target.host, req.target.port, "127.0.0.1", 0)
                    .await
                    .with_context(|| format!("opening a tunnel through {}", j.target.label))?;
                tokio::time::timeout(
                    timeout,
                    client::connect_stream(cfg, ch.into_stream(), handler),
                )
                .await
                .map_err(|_| {
                    anyhow!(
                        "timed out connecting to {} via {}",
                        req.target.host,
                        j.target.label
                    )
                })?
                .with_context(|| format!("connecting to {}:{}", req.target.host, req.target.port))?
            }
            None => tokio::time::timeout(
                timeout,
                client::connect(cfg, (req.target.host.as_str(), req.target.port), handler),
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "timed out connecting to {}:{}",
                    req.target.host,
                    req.target.port
                )
            })?
            .with_context(|| format!("connecting to {}:{}", req.target.host, req.target.port))?,
        };
        let (auth_used, typed_password) =
            authenticate(&mut handle, &req.auth, &req.target, prompter.as_ref()).await?;
        Ok(Arc::new(Connection {
            handle: Mutex::new(handle),
            forwarded: Mutex::new(forwarded_rx),
            target: req.target,
            username: req.auth.username.clone(),
            auth_used,
            typed_password,
            _jump: jump,
        }))
    })
}

async fn rsa_hash(handle: &Handle<ClientHandler>, key: &PrivateKey) -> Option<HashAlg> {
    if !key.algorithm().is_rsa() {
        return None;
    }
    match handle.best_supported_rsa_hash().await {
        Ok(Some(h)) => h,
        // no server-sig-algs extension: modern servers still accept rsa-sha2-256
        _ => Some(HashAlg::Sha256),
    }
}

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    auth: &Auth,
    target: &Target,
    prompter: &dyn Prompter,
) -> Result<(String, Option<String>)> {
    let user = auth.username.as_str();
    if user.is_empty() {
        bail!("no username for {}", target.label);
    }
    let mut errors: Vec<String> = vec![];
    let mut typed: Option<String> = None;
    let mut tried_interactive = false;
    for method in &auth.methods {
        match method {
            AuthMethod::Key { key, label } => {
                let hash = rsa_hash(handle, key).await;
                let pk = PrivateKeyWithHashAlg::new(key.clone(), hash);
                match handle.authenticate_publickey(user, pk).await? {
                    AuthResult::Success => return Ok((format!("key:{label}"), typed)),
                    AuthResult::Failure {
                        partial_success, ..
                    } => {
                        errors.push(format!(
                            "key {label} rejected{}",
                            if partial_success {
                                " (more needed)"
                            } else {
                                ""
                            }
                        ));
                    }
                }
            }
            AuthMethod::Password(p) => match handle.authenticate_password(user, p.clone()).await? {
                AuthResult::Success => return Ok(("password".into(), typed)),
                AuthResult::Failure {
                    remaining_methods, ..
                } => {
                    errors.push("saved password rejected".into());
                    if remaining_methods.contains(&russh::MethodKind::KeyboardInteractive)
                        && !tried_interactive
                    {
                        tried_interactive = true;
                        if interactive(handle, user, target, prompter).await? {
                            return Ok(("interactive".into(), typed));
                        }
                    }
                }
            },
            AuthMethod::AskPassword => {
                for attempt in 0..3 {
                    let Some(p) = prompter
                        .secret(SecretRequest {
                            kind: SecretKind::Password,
                            title: target.label.clone(),
                            prompt: if attempt == 0 {
                                format!("Password for {user}@{}", target.host)
                            } else {
                                format!("Password rejected. Password for {user}@{}", target.host)
                            },
                            echo: false,
                        })
                        .await
                    else {
                        bail!("cancelled");
                    };
                    match handle.authenticate_password(user, p.clone()).await? {
                        AuthResult::Success => return Ok(("password".into(), Some(p))),
                        AuthResult::Failure {
                            remaining_methods, ..
                        } => {
                            typed = None;
                            if remaining_methods.contains(&russh::MethodKind::KeyboardInteractive)
                                && !remaining_methods.contains(&russh::MethodKind::Password)
                            {
                                // the server only speaks keyboard-interactive: use that instead
                                if interactive(handle, user, target, prompter).await? {
                                    return Ok(("interactive".into(), None));
                                }
                                break;
                            }
                        }
                    }
                }
                errors.push("password rejected".into());
            }
            AuthMethod::Agent => match agent_auth(handle, user).await {
                Ok(Some(label)) => return Ok((format!("agent:{label}"), typed)),
                Ok(None) => errors.push("no agent key accepted".into()),
                Err(e) => errors.push(format!("agent: {e}")),
            },
            AuthMethod::Interactive => {
                if tried_interactive {
                    continue;
                }
                tried_interactive = true;
                if interactive(handle, user, target, prompter).await? {
                    return Ok(("interactive".into(), typed));
                }
                errors.push("interactive login failed".into());
            }
        }
    }
    bail!(
        "authentication failed for {user}@{}: {}",
        target.host,
        errors.join("; ")
    )
}

async fn interactive(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    target: &Target,
    prompter: &dyn Prompter,
) -> Result<bool> {
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await?;
    for _ in 0..10 {
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let title = if name.trim().is_empty() {
                    target.label.clone()
                } else {
                    name
                };
                if !instructions.trim().is_empty() {
                    prompter.notice(instructions.trim().to_string()).await;
                }
                let mut answers = Vec::with_capacity(prompts.len());
                for p in prompts {
                    let Some(a) = prompter
                        .secret(SecretRequest {
                            kind: SecretKind::Interactive,
                            title: title.clone(),
                            prompt: p.prompt.trim().to_string(),
                            echo: p.echo,
                        })
                        .await
                    else {
                        bail!("cancelled");
                    };
                    answers.push(a);
                }
                resp = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

/// Try every identity the SSH agent offers. Returns the comment of the key that worked.
async fn agent_auth(handle: &mut Handle<ClientHandler>, user: &str) -> Result<Option<String>> {
    let mut agent = connect_agent().await?;
    let identities = agent
        .request_identities()
        .await
        .context("listing agent keys")?;
    if identities.is_empty() {
        bail!("the agent holds no keys");
    }
    for id in identities {
        let (pk, comment) = match &id {
            AgentIdentity::PublicKey { key, comment } => (key.clone(), comment.clone()),
            AgentIdentity::Certificate {
                certificate,
                comment,
            } => (
                russh::keys::PublicKey::new(certificate.public_key().clone(), ""),
                comment.clone(),
            ),
        };
        let hash = if pk.algorithm().is_rsa() {
            match handle.best_supported_rsa_hash().await {
                Ok(Some(h)) => h,
                _ => Some(HashAlg::Sha256),
            }
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(user, pk, hash, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(Some(comment)),
            Ok(AuthResult::Failure { .. }) => {}
            Err(e) => tracing::debug!("agent key {comment}: {e}"),
        }
    }
    Ok(None)
}

type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

#[cfg(unix)]
async fn connect_agent() -> Result<DynAgent> {
    Ok(AgentClient::connect_env()
        .await
        .map_err(|e| anyhow!("no SSH agent (SSH_AUTH_SOCK): {e}"))?
        .dynamic())
}

#[cfg(windows)]
async fn connect_agent() -> Result<DynAgent> {
    if let Ok(c) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        return Ok(c.dynamic());
    }
    Ok(AgentClient::connect_pageant()
        .await
        .map_err(|e| anyhow!("no SSH agent (OpenSSH agent service or Pageant): {e}"))?
        .dynamic())
}

/// Whether an SSH agent can be reached right now.
pub async fn agent_available() -> bool {
    connect_agent().await.is_ok()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecResult {
    pub exit: Option<u32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run one command without a PTY and collect its output.
pub async fn exec(conn: &Connection, command: &str, timeout_secs: u64) -> Result<ExecResult> {
    let mut ch = conn.open_session().await?;
    ch.exec(true, command).await?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut exit = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
    loop {
        let msg = match tokio::time::timeout_at(deadline, ch.wait()).await {
            Ok(m) => m,
            Err(_) => {
                let _ = ch.close().await;
                bail!("command timed out after {timeout_secs}s");
            }
        };
        match msg {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => err.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    Ok(ExecResult {
        exit,
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
    })
}

/// `uname -sr` (or Windows' `ver`) for the host list; empty when unknown.
pub async fn detect_os(conn: &Connection) -> String {
    match exec(conn, "uname -sr 2>/dev/null || ver", 10).await {
        Ok(r) => r
            .stdout
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string(),
        Err(_) => String::new(),
    }
}

/// Append a public key to `~/.ssh/authorized_keys` on the server (like `ssh-copy-id`).
pub async fn deploy_public_key(conn: &Connection, public_key: &str) -> Result<()> {
    let line = public_key.trim();
    if line.is_empty() || line.contains('\n') || line.contains('\'') {
        bail!("invalid public key line");
    }
    let script = format!(
        "umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && \
         (grep -qxF '{line}' ~/.ssh/authorized_keys || printf '%s\\n' '{line}' >> ~/.ssh/authorized_keys) \
         && chmod 600 ~/.ssh/authorized_keys && echo VERANDA_OK"
    );
    let r = exec(conn, &script, 30).await?;
    if !r.stdout.contains("VERANDA_OK") {
        bail!("could not install the key: {}", r.stderr.trim());
    }
    Ok(())
}
