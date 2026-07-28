//! `weft` — run a background node daemon and interact with it from the CLI.
//!
//! `weft start` launches a detached daemon that holds the live node; the other
//! commands (`send`, `services`, `inbox`, …) are thin clients that talk to it
//! over a Unix socket. See [`control`] for the protocol.

mod control;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use iroh::{EndpointId, RelayUrl};
use url::Url;
use weft::{AgentMessage, Config, Weft, load_or_create_secret_key};

use control::{Request, Response};

#[derive(Parser)]
#[command(name = "weft", version, about = "P2P communication fabric for agents (iroh)")]
struct Cli {
    /// Node identity file. Also selects which daemon the CLI talks to.
    #[arg(long, global = true, default_value = "~/.weft/key.json")]
    key: String,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Which network a node joins: relays, discovery, and gossip bootstrap.
///
/// Defaults to n0's public infrastructure; point these at your own to run a
/// fully self-hosted fabric (see `docs/self-hosting.md`).
#[derive(clap::Args, Clone, Debug, Default)]
struct NetOpts {
    /// Peer to bootstrap gossip through (repeatable).
    #[arg(long)]
    bootstrap: Vec<EndpointId>,
    /// Relay server URL (repeatable). Default: n0's public relays.
    #[arg(long, env = "WEFT_RELAY")]
    relay: Vec<RelayUrl>,
    /// pkarr relay used for discovery. Default: n0's public DNS.
    #[arg(long, env = "WEFT_PKARR_RELAY")]
    pkarr_relay: Option<Url>,
}

impl NetOpts {
    /// Layer explicitly-given flags over a base config (the saved file, or the
    /// built-in defaults). A flag that wasn't passed leaves the base alone; a
    /// repeatable flag that *was* passed replaces the base list wholesale —
    /// append-vs-replace semantics are the kind of thing nobody remembers at 3am.
    fn merge_over(self, base: Config) -> Config {
        Config {
            bootstrap: if self.bootstrap.is_empty() { base.bootstrap } else { self.bootstrap },
            relays: if self.relay.is_empty() { base.relays } else { self.relay },
            pkarr_relay: self.pkarr_relay.or(base.pkarr_relay),
        }
    }
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the saved network config and where it lives.
    Show,
    /// Set `bootstrap`, `relay`, or `pkarr-relay`. No values clears the setting.
    ///
    /// `setting`, not `key`: clap matches global args by id, so a positional
    /// named `key` silently overwrites the global `--key` identity path.
    Set {
        setting: String,
        values: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Print this node's EndpointId (works without a running daemon).
    Id,
    /// Start the node as a background daemon.
    Start {
        #[command(flatten)]
        net: NetOpts,
        /// Announce a service as `name:kind` (repeatable).
        #[arg(long)]
        announce: Vec<String>,
    },
    /// Stop the running daemon.
    Stop,
    /// Show whether the daemon is running and its id / counters.
    Status,
    /// Send a message to a peer via the daemon and print the reply.
    Send {
        to: EndpointId,
        text: String,
    },
    /// Announce a service (`name:kind`) via the daemon.
    Announce {
        spec: String,
    },
    /// List services the daemon has discovered on the fabric.
    Services,
    /// Print and clear messages the daemon has received.
    Inbox,
    /// List peers seen on the fabric and how recently each was heard.
    Peers,
    /// Serve a live dashboard of this node's peers and counters.
    Dash {
        /// Port to serve the dashboard on (localhost only).
        #[arg(long, default_value_t = 4040)]
        port: u16,
    },
    /// Show or edit the saved network config (bootstrap peers, relays).
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Run the node in the foreground (this is what `start` launches).
    Daemon {
        #[command(flatten)]
        net: NetOpts,
        #[arg(long)]
        announce: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let key_path = expand_tilde(&cli.key);
    let sock = control::socket_path(&key_path);

    match cli.cmd {
        // Local, no daemon needed — id is just the public key.
        Cmd::Id => {
            let secret = load_or_create_secret_key(&key_path)?;
            println!("{}", secret.public());
        }

        Cmd::Start { net, announce } => start(&key_path, &sock, net, announce).await?,

        Cmd::Stop => match control::call(&sock, &Request::Stop).await {
            Ok(_) => {
                // The daemon answers `Stop` before it closes its endpoint and
                // unlinks the socket, so returning here would let an immediate
                // `start` bind a socket that the still-exiting process then
                // deletes — leaving a live daemon no client can reach. Wait for
                // the socket to actually go away.
                if !wait_gone(&sock).await {
                    println!("warning: daemon still shutting down ({})", sock.display());
                }
                println!("stopped");
            }
            Err(_) => println!("not running"),
        },

        Cmd::Status => match control::call(&sock, &Request::Status).await {
            Ok(Response::Status { id, services, inbox }) => {
                println!("running");
                println!("  id: {id}");
                println!("  services known: {services}");
                println!("  inbox waiting:  {inbox}");
            }
            _ => println!("not running"),
        },

        Cmd::Send { to, text } => match control::call(&sock, &Request::Send { to, text }).await? {
            Response::Reply { message } => println!("← {} {}", message.kind, message.body),
            Response::Error { message } => bail!("send failed: {message}"),
            other => bail!("unexpected response: {other:?}"),
        },

        Cmd::Announce { spec } => {
            let (name, kind) = spec.split_once(':').unwrap_or((spec.as_str(), "generic"));
            let req = Request::Announce { name: name.into(), kind: kind.into() };
            match control::call(&sock, &req).await? {
                Response::Ok => println!("announced {name} ({kind})"),
                Response::Error { message } => bail!("{message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        }

        Cmd::Services => match control::call(&sock, &Request::Services).await? {
            Response::Services { services } => {
                if services.is_empty() {
                    println!("no services discovered yet");
                }
                for s in services {
                    let price = s
                        .price
                        .map(|p| format!(" [{} {}]", p.amount, p.asset))
                        .unwrap_or_default();
                    println!("• {} ({}) @ {}{}", s.name, s.kind, short(&s.endpoint_id), price);
                }
            }
            other => bail!("unexpected response: {other:?}"),
        },

        Cmd::Inbox => match control::call(&sock, &Request::Inbox).await? {
            Response::Inbox { messages } => {
                if messages.is_empty() {
                    println!("(empty)");
                }
                for m in messages {
                    println!("← {} from {}: {}", m.kind, short(&m.from), m.body);
                }
            }
            other => bail!("unexpected response: {other:?}"),
        },

        Cmd::Peers => match control::call(&sock, &Request::Telemetry).await? {
            Response::Telemetry { telemetry } => {
                if telemetry.peers.is_empty() {
                    println!("no peers seen yet");
                }
                let mut peers = telemetry.peers;
                peers.sort_by_key(|p| p.last_seen_secs);
                for p in peers {
                    println!(
                        "• {}  seen {}s ago  {}",
                        short(&p.id),
                        p.last_seen_secs,
                        p.services.join(", ")
                    );
                }
            }
            other => bail!("unexpected response: {other:?}"),
        },

        Cmd::Dash { port } => dash(&sock, port).await?,

        Cmd::Config { cmd } => config_cmd(&control::config_path(&key_path), cmd)?,

        Cmd::Daemon { net, announce } => run_daemon(&key_path, &sock, net, announce).await?,
    }
    Ok(())
}

/// Wait (up to ~5s) for a path to disappear. False if it's still there.
async fn wait_gone(path: &Path) -> bool {
    for _ in 0..50 {
        if !path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Show or edit the saved network config.
///
/// Values are parsed before anything is written, so a typo fails here rather
/// than at daemon start where it surfaces as a node that quietly talks to nobody.
fn config_cmd(path: &Path, cmd: ConfigCmd) -> Result<()> {
    let mut config = Config::load(path);
    match cmd {
        ConfigCmd::Show => {
            println!("config: {}", path.display());
            if !path.exists() {
                println!("  (not saved yet — showing built-in defaults)");
            }
            println!("  bootstrap:   {}", join(&config.bootstrap));
            println!("  relay:       {}", join(&config.relays));
            println!(
                "  pkarr-relay: {}",
                config.pkarr_relay.map(|u| u.to_string()).unwrap_or_else(|| "(n0 default)".into())
            );
        }
        ConfigCmd::Set { setting, values } => {
            match setting.as_str() {
                "bootstrap" => config.bootstrap = parse_all(&values)?,
                "relay" => config.relays = parse_all(&values)?,
                "pkarr-relay" => {
                    config.pkarr_relay = match values.as_slice() {
                        [] => None,
                        [one] => Some(one.parse().context("parsing pkarr-relay")?),
                        _ => bail!("pkarr-relay takes at most one value"),
                    }
                }
                other => bail!("unknown key `{other}` (bootstrap, relay, pkarr-relay)"),
            }
            config.save(path)?;
            println!("set {setting} in {}", path.display());
        }
    }
    Ok(())
}

fn parse_all<T: std::str::FromStr>(values: &[String]) -> Result<Vec<T>>
where
    T::Err: std::fmt::Display,
{
    values
        .iter()
        .map(|v| v.parse::<T>().map_err(|e| anyhow::anyhow!("`{v}`: {e}")))
        .collect()
}

fn join<T: std::fmt::Display>(items: &[T]) -> String {
    match items {
        [] => "(none)".into(),
        _ => items.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "),
    }
}

/// Spawn the daemon as a detached background process, then wait for it to answer.
async fn start(
    key_path: &Path,
    sock: &Path,
    net: NetOpts,
    announce: Vec<String>,
) -> Result<()> {
    if control::call(sock, &Request::Status).await.is_ok() {
        println!("already running");
        return Ok(());
    }

    let exe = std::env::current_exe().context("finding weft binary")?;
    let log = control::log_path(key_path);
    if let Some(dir) = key_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let out = std::fs::File::create(&log).context("opening daemon log")?;
    let err = out.try_clone()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--key")
        .arg(key_path)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        // New process group so closing the terminal / Ctrl-C doesn't kill it.
        .process_group(0);
    for b in &net.bootstrap {
        cmd.arg("--bootstrap").arg(b.to_string());
    }
    for r in &net.relay {
        cmd.arg("--relay").arg(r.to_string());
    }
    if let Some(pkarr) = &net.pkarr_relay {
        cmd.arg("--pkarr-relay").arg(pkarr.to_string());
    }
    for a in &announce {
        cmd.arg("--announce").arg(a);
    }
    cmd.spawn().context("spawning daemon")?;

    // Poll the socket until the daemon is answering (up to ~5s).
    for _ in 0..50 {
        if let Ok(Response::Status { id, .. }) = control::call(sock, &Request::Status).await {
            println!("weft daemon started");
            println!("  id:  {id}");
            println!("  log: {}", log.display());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("daemon did not come up — check {}", log.display());
}

/// The foreground daemon: own the node, buffer its inbox, serve the control socket.
async fn run_daemon(
    key_path: &Path,
    sock: &Path,
    net: NetOpts,
    announce: Vec<String>,
) -> Result<()> {
    let secret = load_or_create_secret_key(key_path)?;
    // Saved config is the base; flags passed to `start`/`daemon` layer over it.
    let config = net.merge_over(Config::load(control::config_path(key_path)));
    tracing::info!(bootstrap = ?config.bootstrap, "network config");
    let (weft, mut rx) = Weft::spawn(secret, config).await?;
    let me = weft.id();

    // Buffer inbound messages for the CLI to drain; auto-ack senders.
    let inbox: control::Inbox = Arc::new(Mutex::new(Vec::new()));
    let stats = control::Stats::new();
    {
        let (inbox, stats) = (inbox.clone(), stats.clone());
        tokio::spawn(async move {
            while let Some((msg, reply)) = rx.recv().await {
                stats.received.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                inbox.lock().unwrap().push(msg);
                reply.send(AgentMessage::new(me, "ack", serde_json::json!("received")));
            }
        });
    }

    for spec in &announce {
        let (name, kind) = spec.split_once(':').unwrap_or((spec.as_str(), "generic"));
        weft.registry().announce(name, kind, serde_json::Value::Null).await?;
    }

    // Bind the control socket (clear any stale one) and record the pid.
    let _ = std::fs::remove_file(sock);
    let listener = tokio::net::UnixListener::bind(sock)
        .with_context(|| format!("binding control socket {}", sock.display()))?;
    let pid = control::pid_path(key_path);
    std::fs::write(&pid, std::process::id().to_string()).ok();
    println!("weft daemon up: {me}");

    // Serve until `stop`, or shut down cleanly on Ctrl-C.
    tokio::select! {
        r = control::serve(weft.clone(), inbox, stats, listener) => r?,
        _ = tokio::signal::ctrl_c() => {}
    }

    // Unlink before the (slow) endpoint close, so the socket file's lifetime
    // tracks "reachable" rather than "process alive" — a `stop` waiting on it
    // then hands over to a fresh `start` cleanly.
    let _ = std::fs::remove_file(sock);
    let _ = std::fs::remove_file(&pid);
    weft.endpoint().close().await;
    Ok(())
}

/// The dashboard page, embedded so `weft dash` is a single binary.
const DASH_HTML: &str = include_str!("dash.html");

/// Serve the dashboard on localhost: `/` is the page, `/telemetry` proxies the
/// daemon's Unix socket as JSON. ponytail: hand-rolled HTTP over TcpListener,
/// one request per connection — reach for axum if this ever grows a third route.
async fn dash(sock: &Path, port: u16) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Fail early if there's no daemon to visualize.
    control::call(sock, &Request::Status).await?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    println!("dashboard: http://127.0.0.1:{port}  (Ctrl-C to stop)");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let sock = sock.to_path_buf();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let Ok(n) = stream.read(&mut buf).await else { return };
            let req = String::from_utf8_lossy(&buf[..n]);

            let (status, ctype, body): (&str, &str, String) = if req.starts_with("GET /telemetry")
            {
                match control::call(&sock, &Request::Telemetry).await {
                    Ok(Response::Telemetry { telemetry }) => (
                        "200 OK",
                        "application/json",
                        serde_json::to_string(&telemetry).unwrap_or_else(|_| "{}".into()),
                    ),
                    _ => ("502 Bad Gateway", "application/json", r#"{"error":"daemon not running"}"#.into()),
                }
            } else {
                ("200 OK", "text/html; charset=utf-8", DASH_HTML.into())
            };

            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        });
    }
}

fn short(id: &EndpointId) -> String {
    let s = id.to_string();
    format!("{}…", &s[..s.len().min(12)])
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}
