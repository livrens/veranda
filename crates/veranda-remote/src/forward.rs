//! Port forwarding: local (`-L`), remote (`-R`) and dynamic SOCKS5 (`-D`) over one connection.

use crate::client::Connection;
use crate::store::{ForwardKind, ForwardRule};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

pub struct ActiveForward {
    pub rule_id: String,
    /// Open client connections right now.
    pub connections: Arc<AtomicUsize>,
    /// Total connections since the rule started.
    pub total: Arc<AtomicUsize>,
    /// Port actually bound (remote rules with port 0 get one from the server).
    pub bound_port: u16,
    stop: Arc<Notify>,
    stopped: Arc<AtomicBool>,
    conn: Arc<Connection>,
}

impl ActiveForward {
    pub async fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.stop.notify_waiters();
        self.stop.notify_one();
        self.conn.disconnect().await;
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
}

struct Counters {
    connections: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
}

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn bump(c: &Counters) -> Guard {
    c.connections.fetch_add(1, Ordering::Relaxed);
    c.total.fetch_add(1, Ordering::Relaxed);
    Guard(c.connections.clone())
}

pub async fn start(conn: Arc<Connection>, rule: &ForwardRule) -> Result<ActiveForward> {
    let stop = Arc::new(Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(Counters {
        connections: Arc::new(AtomicUsize::new(0)),
        total: Arc::new(AtomicUsize::new(0)),
    });
    let bound_port = match rule.kind {
        ForwardKind::Local | ForwardKind::Dynamic => {
            let listener = TcpListener::bind((rule.bind_address.as_str(), rule.bind_port))
                .await
                .with_context(|| format!("binding {}:{}", rule.bind_address, rule.bind_port))?;
            let port = listener
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(rule.bind_port);
            let dynamic = rule.kind == ForwardKind::Dynamic;
            let (dest_host, dest_port) = (rule.dest_host.clone(), rule.dest_port);
            let (conn2, stop2, stopped2, c2) = (
                conn.clone(),
                stop.clone(),
                stopped.clone(),
                counters.clone(),
            );
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        r = listener.accept() => r,
                        _ = stop2.notified() => break,
                    };
                    if stopped2.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok((socket, peer)) = accepted else {
                        continue;
                    };
                    let (conn3, c3) = (conn2.clone(), c2.clone());
                    let (dh, dp) = (dest_host.clone(), dest_port);
                    tokio::spawn(async move {
                        let _g = bump(&c3);
                        let r = if dynamic {
                            socks5(conn3, socket).await
                        } else {
                            pipe_local(conn3, socket, &dh, dp, peer).await
                        };
                        if let Err(e) = r {
                            tracing::debug!("forward connection ended: {e}");
                        }
                    });
                }
            });
            port
        }
        ForwardKind::Remote => {
            let port = conn
                .handle
                .lock()
                .await
                .tcpip_forward(rule.bind_address.clone(), rule.bind_port as u32)
                .await
                .with_context(|| {
                    format!(
                        "the server refused to listen on {}:{}",
                        rule.bind_address, rule.bind_port
                    )
                })?;
            let (dest_host, dest_port) = (rule.dest_host.clone(), rule.dest_port);
            let (conn2, stop2, stopped2, c2) = (
                conn.clone(),
                stop.clone(),
                stopped.clone(),
                counters.clone(),
            );
            tokio::spawn(async move {
                loop {
                    let next = {
                        let mut rx = conn2.forwarded.lock().await;
                        tokio::select! {
                            n = rx.recv() => n,
                            _ = stop2.notified() => break,
                        }
                    };
                    if stopped2.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(fwd) = next else { break };
                    let (dh, dp, c3) = (dest_host.clone(), dest_port, c2.clone());
                    tokio::spawn(async move {
                        let _g = bump(&c3);
                        let mut stream = fwd.channel.into_stream();
                        match TcpStream::connect((dh.as_str(), dp)).await {
                            Ok(mut local) => {
                                let _ =
                                    tokio::io::copy_bidirectional(&mut local, &mut stream).await;
                            }
                            Err(e) => {
                                tracing::debug!("remote forward: cannot reach {dh}:{dp}: {e}")
                            }
                        }
                    });
                }
            });
            port as u16
        }
    };
    Ok(ActiveForward {
        rule_id: rule.id.clone(),
        connections: counters.connections.clone(),
        total: counters.total.clone(),
        bound_port,
        stop,
        stopped,
        conn,
    })
}

async fn pipe_local(
    conn: Arc<Connection>,
    mut socket: TcpStream,
    dest_host: &str,
    dest_port: u16,
    peer: std::net::SocketAddr,
) -> Result<()> {
    let ch = conn
        .open_direct_tcpip(dest_host, dest_port, &peer.ip().to_string(), peer.port())
        .await?;
    let mut stream = ch.into_stream();
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}

/// Minimal SOCKS5 (RFC 1928): no authentication, CONNECT only, IPv4/IPv6/domain targets.
async fn socks5(conn: Arc<Connection>, mut socket: TcpStream) -> Result<()> {
    let mut head = [0u8; 2];
    socket.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("not SOCKS5");
    }
    let mut methods = vec![0u8; head[1] as usize];
    socket.read_exact(&mut methods).await?;
    socket.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    socket.read_exact(&mut req).await?;
    if req[1] != 1 {
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("only CONNECT is supported");
    }
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            socket.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            socket.read_exact(&mut n).await?;
            let mut name = vec![0u8; n[0] as usize];
            socket.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            socket.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => {
            socket.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("unsupported address type");
        }
    };
    let mut p = [0u8; 2];
    socket.read_exact(&mut p).await?;
    let port = u16::from_be_bytes(p);
    let peer = socket.peer_addr().ok();
    let ch = match conn
        .open_direct_tcpip(
            &host,
            port,
            &peer
                .map(|a| a.ip().to_string())
                .unwrap_or_else(|| "127.0.0.1".into()),
            peer.map(|a| a.port()).unwrap_or(0),
        )
        .await
    {
        Ok(ch) => ch,
        Err(e) => {
            socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Err(e);
        }
    };
    socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    let mut stream = ch.into_stream();
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}
