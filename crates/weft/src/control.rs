//! Control channel between the `weft` CLI and the background daemon.
//!
//! The daemon owns the live node (endpoint, gossip, discovery, inbox). CLI
//! commands are thin clients that connect to a Unix socket next to the key file
//! and exchange one newline-delimited JSON [`Request`]/[`Response`] per
//! connection. ponytail: Unix socket + JSON lines — no RPC framework for a
//! local control plane.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use weft::{AgentMessage, ServiceAnnouncement, Weft};

/// Received agent messages the daemon holds until the CLI drains them.
pub type Inbox = Arc<Mutex<Vec<AgentMessage>>>;

/// Counters the daemon accumulates for telemetry. ponytail: two atomics and a
/// start time — reach for iroh's EndpointMetrics when someone needs bytes/paths.
pub struct Stats {
    pub started: Instant,
    pub sent: AtomicU64,
    pub received: AtomicU64,
}

impl Stats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { started: Instant::now(), sent: 0.into(), received: 0.into() })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Id,
    Status,
    Send { to: EndpointId, text: String },
    Announce { name: String, kind: String },
    Services,
    /// Drain and return buffered inbound messages.
    Inbox,
    /// Snapshot of the node's counters, relays, and known peers.
    Telemetry,
    /// Ask the daemon to shut down.
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Id { id: EndpointId },
    Status { id: EndpointId, services: usize, inbox: usize },
    Reply { message: AgentMessage },
    Services { services: Vec<ServiceAnnouncement> },
    Inbox { messages: Vec<AgentMessage> },
    Telemetry { telemetry: Telemetry },
    Ok,
    Error { message: String },
}

/// Everything `weft peers` and `weft dash` need, in one snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub struct Telemetry {
    pub id: EndpointId,
    pub uptime_secs: u64,
    pub sent: u64,
    pub received: u64,
    pub inbox: usize,
    pub relays: Vec<RelayInfo>,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RelayInfo {
    pub url: String,
    pub connected: bool,
}

/// A peer as seen through gossip: what it announces and how fresh it is.
/// Peers re-announce every 30s, so `last_seen_secs` well past that means gone.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: EndpointId,
    pub services: Vec<String>,
    pub last_seen_secs: u64,
}

/// Control socket path: `/tmp/weft-<hash>.sock`, keyed by the key file so one
/// `--key` = one daemon. ponytail: sockets go in /tmp, not next to the key —
/// Unix socket paths are capped (~104 bytes) and key files can be deep.
pub fn socket_path(key_path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key_path.hash(&mut h);
    PathBuf::from(format!("/tmp/weft-{:016x}.sock", h.finish()))
}

/// PID file path (written by the daemon, for `stop`/diagnostics).
pub fn pid_path(key_path: &Path) -> PathBuf {
    key_dir(key_path).join("weft.pid")
}

/// Daemon log path (where `start` redirects the detached process output).
pub fn log_path(key_path: &Path) -> PathBuf {
    key_dir(key_path).join("weft.log")
}

/// Saved network config. Lives beside the key, so one key = one daemon = one
/// config, matching how the socket and pid are already scoped.
pub fn config_path(key_path: &Path) -> PathBuf {
    key_dir(key_path).join("config.json")
}

fn key_dir(key_path: &Path) -> &Path {
    key_path.parent().unwrap_or_else(|| Path::new("."))
}

/// Send one request to the daemon and await its response.
pub async fn call(sock: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(sock).await.with_context(|| {
        format!("no daemon at {} — start one with `weft start`", sock.display())
    })?;
    let (r, mut w) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    w.write_all(line.as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    serde_json::from_str(&resp).context("decoding daemon response")
}

/// Run the control server until a [`Request::Stop`] arrives. The caller owns the
/// node and the inbox buffer; this just dispatches requests against them.
pub async fn serve(
    weft: Weft,
    inbox: Inbox,
    stats: Arc<Stats>,
    listener: UnixListener,
) -> Result<()> {
    let stop = Arc::new(Notify::new());
    loop {
        tokio::select! {
            _ = stop.notified() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accepting control connection")?;
                let (weft, inbox, stats, stop) =
                    (weft.clone(), inbox.clone(), stats.clone(), stop.clone());
                tokio::spawn(handle(stream, weft, inbox, stats, stop));
            }
        }
    }
    Ok(())
}

async fn handle(
    stream: UnixStream,
    weft: Weft,
    inbox: Inbox,
    stats: Arc<Stats>,
    stop: Arc<Notify>,
) {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }

    let (resp, should_stop) = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Stop) => (Response::Ok, true),
        Ok(req) => (dispatch(req, &weft, &inbox, &stats).await, false),
        Err(e) => (Response::Error { message: format!("bad request: {e}") }, false),
    };

    if let Ok(mut out) = serde_json::to_string(&resp) {
        out.push('\n');
        let _ = w.write_all(out.as_bytes()).await;
    }
    // Signal shutdown only after the reply is on the wire.
    if should_stop {
        stop.notify_one();
    }
}

async fn dispatch(req: Request, weft: &Weft, inbox: &Inbox, stats: &Stats) -> Response {
    match req {
        Request::Id => Response::Id { id: weft.id() },
        Request::Status => Response::Status {
            id: weft.id(),
            services: weft.registry().list().len(),
            inbox: inbox.lock().unwrap().len(),
        },
        Request::Send { to, text } => {
            let msg = AgentMessage::new(weft.id(), "message", serde_json::json!(text));
            match weft.send(to, &msg).await {
                Ok(reply) => {
                    stats.sent.fetch_add(1, Ordering::Relaxed);
                    Response::Reply { message: reply }
                }
                Err(e) => Response::Error { message: e.to_string() },
            }
        }
        Request::Announce { name, kind } => {
            match weft.registry().announce(&name, &kind, serde_json::Value::Null).await {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error { message: e.to_string() },
            }
        }
        Request::Services => Response::Services { services: weft.registry().list() },
        Request::Inbox => {
            let messages = std::mem::take(&mut *inbox.lock().unwrap());
            Response::Inbox { messages }
        }
        Request::Telemetry => {
            use iroh::Watcher as _;
            let relays = weft
                .endpoint()
                .home_relay_status()
                .get()
                .iter()
                .map(|r| RelayInfo { url: r.url().to_string(), connected: r.is_connected() })
                .collect();
            // Group per-service announcements into one entry per peer; a peer is
            // as fresh as its most recent announcement.
            let mut peers = std::collections::HashMap::<EndpointId, PeerInfo>::new();
            for (ann, age) in weft.registry().list_seen() {
                if ann.endpoint_id == weft.id() {
                    continue;
                }
                let p = peers.entry(ann.endpoint_id).or_insert_with(|| PeerInfo {
                    id: ann.endpoint_id,
                    services: Vec::new(),
                    last_seen_secs: age,
                });
                p.last_seen_secs = p.last_seen_secs.min(age);
                p.services.push(format!("{} ({})", ann.name, ann.kind));
            }
            Response::Telemetry {
                telemetry: Telemetry {
                    id: weft.id(),
                    uptime_secs: stats.started.elapsed().as_secs(),
                    sent: stats.sent.load(Ordering::Relaxed),
                    received: stats.received.load(Ordering::Relaxed),
                    inbox: inbox.lock().unwrap().len(),
                    relays,
                    peers: peers.into_values().collect(),
                },
            }
        }
        Request::Stop => Response::Ok, // handled in `handle`
    }
}
