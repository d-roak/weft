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
use iroh::EndpointId;
use weft::{AgentMessage, Weft, load_or_create_secret_key};

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

#[derive(Subcommand)]
enum Cmd {
    /// Print this node's EndpointId (works without a running daemon).
    Id,
    /// Start the node as a background daemon.
    Start {
        #[arg(long)]
        bootstrap: Vec<EndpointId>,
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
    /// Run the node in the foreground (this is what `start` launches).
    Daemon {
        #[arg(long)]
        bootstrap: Vec<EndpointId>,
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

        Cmd::Start { bootstrap, announce } => start(&key_path, &sock, bootstrap, announce).await?,

        Cmd::Stop => match control::call(&sock, &Request::Stop).await {
            Ok(_) => println!("stopped"),
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

        Cmd::Daemon { bootstrap, announce } => {
            run_daemon(&key_path, &sock, bootstrap, announce).await?
        }
    }
    Ok(())
}

/// Spawn the daemon as a detached background process, then wait for it to answer.
async fn start(
    key_path: &Path,
    sock: &Path,
    bootstrap: Vec<EndpointId>,
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
    for b in &bootstrap {
        cmd.arg("--bootstrap").arg(b.to_string());
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
    bootstrap: Vec<EndpointId>,
    announce: Vec<String>,
) -> Result<()> {
    let secret = load_or_create_secret_key(key_path)?;
    let (weft, mut rx) = Weft::spawn(secret, bootstrap).await?;
    let me = weft.id();

    // Buffer inbound messages for the CLI to drain; auto-ack senders.
    let inbox: control::Inbox = Arc::new(Mutex::new(Vec::new()));
    {
        let inbox = inbox.clone();
        tokio::spawn(async move {
            while let Some((msg, reply)) = rx.recv().await {
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
        r = control::serve(weft.clone(), inbox, listener) => r?,
        _ = tokio::signal::ctrl_c() => {}
    }

    weft.endpoint().close().await;
    let _ = std::fs::remove_file(sock);
    let _ = std::fs::remove_file(&pid);
    Ok(())
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
