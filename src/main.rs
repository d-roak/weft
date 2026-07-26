//! `weft` — run a node, message agents, announce and discover services.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iroh::EndpointId;
use weft::{AgentMessage, Weft, load_or_create_secret_key};

#[derive(Parser)]
#[command(name = "weft", version, about = "P2P communication fabric for agents (iroh)")]
struct Cli {
    /// Where the node identity is stored.
    #[arg(long, global = true, default_value = "~/.weft/key.json")]
    key: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print this node's EndpointId (its shareable address).
    Id,
    /// Run the node: base connectivity + service discovery + agent inbox.
    Up {
        /// Bootstrap peers to join the gossip swarm (repeatable).
        #[arg(long)]
        bootstrap: Vec<EndpointId>,
        /// Announce a service as `name:kind` (repeatable).
        #[arg(long)]
        announce: Vec<String>,
    },
    /// Send a message to a peer and print its reply.
    Send {
        /// Recipient EndpointId.
        to: EndpointId,
        /// Message text (sent as kind `"message"`).
        text: String,
    },
    /// Join the fabric, wait briefly, and list discovered services.
    Services {
        #[arg(long)]
        bootstrap: Vec<EndpointId>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let key_path = expand_tilde(&cli.key);
    let secret = load_or_create_secret_key(&key_path)?;

    match cli.cmd {
        Cmd::Id => {
            println!("{}", secret.public());
        }

        Cmd::Up { bootstrap, announce } => {
            let (weft, mut inbox) = Weft::spawn(secret, bootstrap).await?;
            println!("weft node up");
            println!("  id: {}", weft.id());
            println!("  share this id so peers can reach you\n");

            for spec in &announce {
                let (name, kind) = spec.split_once(':').unwrap_or((spec.as_str(), "generic"));
                weft.registry().announce(name, kind, serde_json::Value::Null).await?;
                println!("announced service {name} ({kind})");
            }

            // Handle inbound agent messages until Ctrl-C.
            let me = weft.id();
            tokio::spawn(async move {
                while let Some((msg, reply)) = inbox.recv().await {
                    println!("← {} from {}: {}", msg.kind, short(&msg.from), msg.body);
                    reply.send(AgentMessage::new(me, "ack", serde_json::json!("received")));
                }
            });

            tokio::signal::ctrl_c().await?;
            println!("\nshutting down");
        }

        Cmd::Send { to, text } => {
            let (weft, _inbox) = Weft::spawn(secret, vec![]).await?;
            let msg = AgentMessage::new(weft.id(), "message", serde_json::json!(text));
            let reply = weft.send(to, &msg).await.context("send failed")?;
            println!("→ sent to {}", short(&to));
            println!("← reply: {} {}", reply.kind, reply.body);
            weft.endpoint().close().await;
        }

        Cmd::Services { bootstrap } => {
            let (weft, _inbox) = Weft::spawn(secret, bootstrap).await?;
            println!("joining fabric as {}…", short(&weft.id()));
            tokio::time::sleep(Duration::from_secs(3)).await;
            let services = weft.registry().list();
            if services.is_empty() {
                println!("no services discovered (need a --bootstrap peer that has some)");
            }
            for s in services {
                let price =
                    s.price.map(|p| format!(" [{} {}]", p.amount, p.asset)).unwrap_or_default();
                println!("• {} ({}) @ {}{}", s.name, s.kind, short(&s.endpoint_id), price);
            }
            weft.endpoint().close().await;
        }
    }
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
