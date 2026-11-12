//! End-to-end checks against an in-process russh server: host-key trust, password and key
//! authentication, exec, an interactive shell, local and dynamic (SOCKS5) port forwarding.

use russh::server::{self, Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, Pty};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use veranda_remote::client::{
    self, Auth as ClientAuth, AuthMethod, BoxFut, ConnectRequest, HostKeyRequest, Prompter,
    SecretRequest, Target,
};
use veranda_remote::session::{ShellOptions, TermEvent, open_ssh_shell};
use veranda_remote::store::{ForwardKind, ForwardRule};
use veranda_remote::{forward, keys, known_hosts};

const USER: &str = "tester";
const PASSWORD: &str = "s3cret";

#[derive(Clone)]
struct TestServer;

struct TestHandler;

impl server::Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> TestHandler {
        TestHandler
    }
}

impl server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(if user == USER {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"welcome\r\n".to_vec())?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, format!("exec:{}\n", String::from_utf8_lossy(data)))?;
        session.exit_status_request(channel, 3)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, data.to_vec())?;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let target = (host_to_connect.to_string(), port_to_connect as u16);
        tokio::spawn(async move {
            if let Ok(mut tcp) = TcpStream::connect(target).await {
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
            }
        });
        Ok(())
    }
}

async fn start_server() -> u16 {
    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_millis(0),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        keys: vec![
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap(),
        ],
        ..Default::default()
    });
    let listener: &'static TcpListener =
        Box::leak(Box::new(TcpListener::bind("127.0.0.1:0").await.unwrap()));
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = TestServer;
        let _ = server.run_on_socket(config, listener).await;
    });
    port
}

/// Plain TCP echo service the tunnels point at.
async fn start_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

struct Accepting {
    host_keys: AtomicUsize,
    secrets: AtomicUsize,
    answer: Option<String>,
}

impl Prompter for Accepting {
    fn host_key(&self, _req: HostKeyRequest) -> BoxFut<'_, bool> {
        self.host_keys.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { true })
    }
    fn secret(&self, _req: SecretRequest) -> BoxFut<'_, Option<String>> {
        self.secrets.fetch_add(1, Ordering::Relaxed);
        let a = self.answer.clone();
        Box::pin(async move { a })
    }
    fn notice(&self, _text: String) -> BoxFut<'_, ()> {
        Box::pin(async {})
    }
}

fn request(port: u16, known_hosts: &std::path::Path, methods: Vec<AuthMethod>) -> ConnectRequest {
    ConnectRequest {
        target: Target {
            host: "127.0.0.1".into(),
            port,
            label: "test".into(),
        },
        auth: ClientAuth {
            username: USER.into(),
            methods,
        },
        jump: None,
        keepalive_secs: 0,
        known_hosts: known_hosts.to_path_buf(),
        timeout_secs: 10,
    }
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut)
        .await
        .expect("timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_login_exec_shell_and_tunnels() {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let port = start_server().await;
    let echo = start_echo().await;

    // first connection: unknown host key is accepted and recorded
    let prompter = Arc::new(Accepting {
        host_keys: AtomicUsize::new(0),
        secrets: AtomicUsize::new(0),
        answer: Some(PASSWORD.into()),
    });
    let conn = within(client::connect(
        request(port, &kh, vec![AuthMethod::Password(PASSWORD.into())]),
        prompter.clone(),
    ))
    .await
    .expect("connect");
    assert_eq!(conn.auth_used, "password");
    assert_eq!(prompter.host_keys.load(Ordering::Relaxed), 1);
    assert_eq!(known_hosts::list(&kh).unwrap().len(), 1);

    // exec collects output and the exit status
    let r = within(client::exec(&conn, "uptime", 10)).await.unwrap();
    assert_eq!(r.stdout, "exec:uptime\n");
    assert_eq!(r.exit, Some(3));

    // interactive shell: banner, echo, clean exit
    let (tx, mut rx) = mpsc::channel(64);
    let shell = within(open_ssh_shell(
        conn.clone(),
        ShellOptions {
            cols: 80,
            rows: 24,
            agent_forwarding: false,
            env: vec![],
            startup: None,
        },
        tx,
    ))
    .await
    .unwrap();
    let mut got = Vec::new();
    while !String::from_utf8_lossy(&got).contains("welcome") {
        match within(rx.recv()).await {
            Some(TermEvent::Data(d)) => got.extend(d),
            other => panic!("unexpected {other:?}"),
        }
    }
    shell.write(b"abc\n".to_vec()).await;
    got.clear();
    while !String::from_utf8_lossy(&got).contains("abc\n") {
        match within(rx.recv()).await {
            Some(TermEvent::Data(d)) => got.extend(d),
            other => panic!("unexpected {other:?}"),
        }
    }
    shell.resize(100, 30).await;
    shell.close().await;
    loop {
        match within(rx.recv()).await {
            Some(TermEvent::Exit(_)) | None => break,
            Some(TermEvent::Data(_)) => {}
            Some(TermEvent::Error(e)) => panic!("shell error {e}"),
        }
    }

    // local forward: connect to the bound port, bytes travel through the server to the echo
    let rule = ForwardRule {
        id: "r1".into(),
        kind: ForwardKind::Local,
        host_id: "h".into(),
        bind_address: "127.0.0.1".into(),
        bind_port: 0,
        dest_host: "127.0.0.1".into(),
        dest_port: echo,
        ..Default::default()
    };
    let active = within(forward::start(conn.clone(), &rule)).await.unwrap();
    assert!(active.bound_port > 0);
    let mut s = TcpStream::connect(("127.0.0.1", active.bound_port))
        .await
        .unwrap();
    s.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    within(s.read_exact(&mut buf)).await.unwrap();
    assert_eq!(&buf, b"ping");
    drop(s);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active.total.load(Ordering::Relaxed), 1);
    assert_eq!(active.connections.load(Ordering::Relaxed), 0);
    active.stop().await;

    // the forward stopped: the port must be free again (the listener task ended)
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        TcpListener::bind(("127.0.0.1", active.bound_port))
            .await
            .is_ok()
    );

    // dynamic forward: SOCKS5 CONNECT to the echo service
    let conn2 = within(client::connect(
        request(port, &kh, vec![AuthMethod::Password(PASSWORD.into())]),
        prompter.clone(),
    ))
    .await
    .unwrap();
    assert_eq!(
        prompter.host_keys.load(Ordering::Relaxed),
        1,
        "known key must not prompt again"
    );
    let socks = within(forward::start(
        conn2.clone(),
        &ForwardRule {
            id: "r2".into(),
            kind: ForwardKind::Dynamic,
            host_id: "h".into(),
            bind_address: "127.0.0.1".into(),
            bind_port: 0,
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    let mut s = TcpStream::connect(("127.0.0.1", socks.bound_port))
        .await
        .unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut hello = [0u8; 2];
    within(s.read_exact(&mut hello)).await.unwrap();
    assert_eq!(hello, [5, 0]);
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend_from_slice(&echo.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut reply = [0u8; 10];
    within(s.read_exact(&mut reply)).await.unwrap();
    assert_eq!(reply[1], 0, "SOCKS reply must be success");
    s.write_all(b"pong").await.unwrap();
    within(s.read_exact(&mut buf)).await.unwrap();
    assert_eq!(&buf, b"pong");
    drop(s);
    socks.stop().await;
    conn.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_login_and_rejections() {
    let dir = tempfile::tempdir().unwrap();
    let kh = dir.path().join("known_hosts");
    let keys_dir = dir.path().join("keys");
    let port = start_server().await;
    let meta = keys::generate(&keys_dir, "test", "ed25519", Some("pp")).unwrap();
    let key = Arc::new(keys::load(&keys_dir, &meta.id, Some("pp")).unwrap());
    let prompter = Arc::new(Accepting {
        host_keys: AtomicUsize::new(0),
        secrets: AtomicUsize::new(0),
        answer: None,
    });
    let conn = within(client::connect(
        request(
            port,
            &kh,
            vec![AuthMethod::Key {
                key,
                label: "test".into(),
            }],
        ),
        prompter.clone(),
    ))
    .await
    .expect("key login");
    assert_eq!(conn.auth_used, "key:test");
    conn.disconnect().await;

    // wrong saved password, prompt cancelled -> a clear error, no panic
    let err = within(client::connect(
        request(
            port,
            &kh,
            vec![
                AuthMethod::Password("wrong".into()),
                AuthMethod::AskPassword,
            ],
        ),
        prompter.clone(),
    ))
    .await
    .err()
    .expect("must fail");
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(prompter.secrets.load(Ordering::Relaxed), 1);

    // a refused host key never reaches authentication
    struct Refusing;
    impl Prompter for Refusing {
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
    let other_kh = dir.path().join("kh2");
    let err = within(client::connect(
        request(port, &other_kh, vec![AuthMethod::Password(PASSWORD.into())]),
        Arc::new(Refusing),
    ))
    .await
    .err()
    .expect("must fail");
    assert!(
        !other_kh.exists(),
        "a refused key must not be recorded: {err}"
    );
}
