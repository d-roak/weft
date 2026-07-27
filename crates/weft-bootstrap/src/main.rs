//! `weft-bootstrap` — a long-lived peer that new nodes join the fabric through.
//!
//! Nothing in iroh puts two nodes in the same gossip swarm on its own: relays
//! and pkarr/DNS answer *"how do I reach node X?"*, never *"who else is out
//! there?"*. On a LAN mDNS closes that gap; across the internet somebody has to
//! already know somebody. This binary is that somebody — the seed node whose
//! [`EndpointId`] ships in `weft::DEFAULT_BOOTSTRAP` or lands in a user's
//! `weft config set bootstrap …`.
//!
//! It is an ordinary weft node that does nothing but stay up and stay reachable.
//! Its one real requirement is a **stable identity**: keep `--key` on a
//! persisted volume, because a bootstrap peer whose id changes on restart is
//! not a bootstrap peer.
//!
//! Run several and point them at each other with `--bootstrap` so the seed list
//! is one mesh rather than N islands.
//!
//! ponytail: no control socket, no daemonising, no config file — systemd and
//! Docker already own restarts and logging. It's `weft daemon` minus the local
//! CLI surface; if that ever reads as duplication, collapse it into a
//! `weft daemon` flag and delete this crate.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use iroh::{EndpointId, RelayUrl};
use url::Url;
use weft::{Config, Weft, load_or_create_secret_key};

#[derive(Parser, Debug)]
#[command(name = "weft-bootstrap", version, about = "Long-lived bootstrap peer for weft")]
struct Cli {
    /// Node identity file. Must be on persistent storage — the whole point of
    /// this process is an id that outlives restarts.
    #[arg(long, env = "WEFT_BOOTSTRAP_KEY", default_value = "/var/lib/weft/bootstrap.json")]
    key: PathBuf,

    /// Other bootstrap peers to join (repeatable). Use this to mesh several
    /// bootstrap servers together.
    #[arg(long, env = "WEFT_BOOTSTRAP_PEERS", value_delimiter = ',')]
    bootstrap: Vec<EndpointId>,

    /// Relay server URL (repeatable). Default: n0's public relays.
    #[arg(long, env = "WEFT_RELAY", value_delimiter = ',')]
    relay: Vec<RelayUrl>,

    /// pkarr relay used for discovery. Default: n0's public DNS.
    #[arg(long, env = "WEFT_PKARR_RELAY")]
    pkarr_relay: Option<Url>,

    /// Service name announced on the fabric, so `weft services` lists this node.
    #[arg(long, env = "WEFT_BOOTSTRAP_NAME", default_value = "bootstrap")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let secret = load_or_create_secret_key(&cli.key)?;
    let config = Config {
        bootstrap: cli.bootstrap,
        relays: cli.relay,
        pkarr_relay: cli.pkarr_relay,
    };
    let (weft, mut inbox) = Weft::spawn(secret, config).await?;

    // Announcing itself makes the seed mesh self-describing: once you reach one
    // bootstrap node, `weft services` shows you the rest.
    weft.registry().announce(&cli.name, "bootstrap", serde_json::Value::Null).await?;

    // A bootstrap node has nothing to say, but it still drains the inbox (a full
    // channel would stall the handler) and acks so senders aren't left waiting.
    let me = weft.id();
    tokio::spawn(async move {
        while let Some((_msg, reply)) = inbox.recv().await {
            reply.send(weft::AgentMessage::new(me, "ack", serde_json::json!("bootstrap")));
        }
    });

    println!("weft-bootstrap up");
    println!("  id:  {}", weft.id());
    println!("  key: {}", cli.key.display());
    println!();
    println!("Add this node to a peer with:");
    println!("  weft config set bootstrap {}", weft.id());

    tokio::signal::ctrl_c().await?;
    weft.endpoint().close().await;
    Ok(())
}
