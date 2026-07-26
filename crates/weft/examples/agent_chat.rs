//! Two agents talking over weft — the primitive behind "two Claude sessions
//! communicating" on localhost or LAN.
//!
//! Each side runs a node with a **persistent** identity (so its EndpointId is
//! stable across restarts), prints incoming messages, and sends whatever you
//! type to the current peer. Whoever messages you first becomes your peer, so
//! only one side needs to know the other's id to start.
//!
//!     # terminal A (also works as two Claude sessions on one machine)
//!     cargo run --example agent_chat -- --key a.json
//!     #   you are  AAAA…            ← copy this id
//!
//!     # terminal B — start pointed at A
//!     cargo run --example agent_chat -- --key b.json --peer AAAA…
//!
//! Type in either terminal; the line appears in the other. On localhost the two
//! connect over loopback; on a LAN they hole-punch a direct path. See
//! docs/use-cases/agent-sessions.md.

use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use iroh::EndpointId;
use tokio::io::{AsyncBufReadExt, BufReader};
use weft::{AgentMessage, Weft, load_or_create_secret_key};

#[tokio::main]
async fn main() -> Result<()> {
    // Minimal flag parsing: --key <path> [--peer <endpoint-id>].
    let mut key_path = "weft-chat-key.json".to_string();
    let mut peer: Option<EndpointId> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key" => key_path = args.next().unwrap_or(key_path),
            "--peer" => {
                let Some(v) = args.next() else { bail!("--peer needs an endpoint id") };
                peer = Some(v.parse()?);
            }
            other => bail!("unknown arg: {other} (use --key <path> [--peer <id>])"),
        }
    }

    // Persistent identity — stable EndpointId across restarts.
    let secret = load_or_create_secret_key(&key_path)?;
    let (weft, mut inbox) = Weft::spawn(secret, vec![]).await?;
    let me = weft.id();
    println!("you are  {me}");
    match &peer {
        Some(p) => println!("peer is  {p}\ntype a message and press enter:"),
        None => println!("waiting for someone to message you (share your id above)…"),
    }

    // Current peer: seeded from --peer, updated to whoever messages us. Shared
    // so the stdin loop can send to the latest sender.
    let peer = Arc::new(Mutex::new(peer));

    // Print incoming messages; adopt the sender as our peer so we can reply.
    let recv_peer = peer.clone();
    tokio::spawn(async move {
        while let Some((msg, reply)) = inbox.recv().await {
            if let Some(text) = msg.body.as_str() {
                println!("\r{}: {text}", short(&msg.from));
            }
            *recv_peer.lock().unwrap() = Some(msg.from);
            reply.send(AgentMessage::new(me, "ack", serde_json::Value::Null));
        }
    });

    // Read stdin lines and send each to the current peer.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }
        let to = *peer.lock().unwrap();
        let Some(to) = to else {
            println!("(no peer yet — wait for a message, or restart with --peer <id>)");
            continue;
        };
        let msg = AgentMessage::new(me, "chat", serde_json::json!(line));
        if let Err(e) = weft.send(to, &msg).await {
            println!("(send failed: {e})");
        }
    }

    weft.endpoint().close().await;
    Ok(())
}

fn short(id: &EndpointId) -> String {
    let s = id.to_string();
    format!("{}…", &s[..s.len().min(8)])
}
