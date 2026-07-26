//! Agent-to-agent messaging: a tiny request/reply protocol over iroh streams.
//!
//! Each message is a JSON [`AgentMessage`] sent on a fresh bidirectional
//! stream; the receiver replies with another [`AgentMessage`] on the same
//! stream. That's the whole wire format — length is delimited by the QUIC
//! stream's end-of-stream, so there's no framing to get wrong.
//!
//! Whether the bytes travel directly (hole-punched) or via a relay is decided
//! by iroh underneath; agents just see [`Weft::send`](crate::Weft::send).

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// ALPN for the weft agent protocol. Bump the suffix on wire-breaking changes.
pub const ALPN: &[u8] = b"weft/agent/0";

/// Max bytes accepted for a single message. Agents exchange control messages,
/// not bulk data — stream large payloads over a dedicated ALPN instead.
const MAX_MSG: usize = 1 << 20; // 1 MiB

/// A message from one agent to another. `kind` lets a receiver route by intent
/// (e.g. `"ping"`, `"invoke"`, `"x402/payment-required"`); `body` is free-form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub from: EndpointId,
    pub kind: String,
    #[serde(default)]
    pub body: serde_json::Value,
}

impl AgentMessage {
    pub fn new(from: EndpointId, kind: impl Into<String>, body: serde_json::Value) -> Self {
        Self { from, kind: kind.into(), body }
    }
}

/// Receiving end for messages addressed to this node. Each entry carries the
/// message and a [`Reply`] the handler uses to answer the sender.
pub type Inbox = mpsc::Receiver<(AgentMessage, Reply)>;

/// A one-shot channel back to the sender of an [`AgentMessage`].
#[derive(Debug)]
pub struct Reply(tokio::sync::oneshot::Sender<AgentMessage>);

impl Reply {
    /// Answer the sender. Dropping a `Reply` without calling this sends a
    /// default `"ack"` back so the sender's `send()` never hangs.
    pub fn send(self, msg: AgentMessage) {
        let _ = self.0.send(msg);
    }
}

/// Protocol handler installed on the router; forwards each request to [`Inbox`].
#[derive(Debug, Clone)]
pub struct AgentHandler {
    me: EndpointId,
    tx: mpsc::Sender<(AgentMessage, Reply)>,
}

impl AgentHandler {
    pub fn new(me: EndpointId) -> (Self, Inbox) {
        let (tx, rx) = mpsc::channel(64);
        (Self { me, tx }, rx)
    }
}

impl ProtocolHandler for AgentHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // One request/reply per accepted bi stream; loop so a peer can reuse
        // the connection for several messages.
        loop {
            let (mut send, mut recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break, // peer closed the connection
            };
            let bytes = recv.read_to_end(MAX_MSG).await.map_err(AcceptError::from_err)?;
            let msg: AgentMessage =
                serde_json::from_slice(&bytes).map_err(AcceptError::from_err)?;

            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            if self.tx.send((msg, Reply(reply_tx))).await.is_err() {
                break; // node is shutting down
            }
            let reply = reply_rx
                .await
                .unwrap_or_else(|_| AgentMessage::new(self.me, "ack", serde_json::Value::Null));

            let out = serde_json::to_vec(&reply).map_err(AcceptError::from_err)?;
            send.write_all(&out).await.map_err(AcceptError::from_err)?;
            send.finish().map_err(AcceptError::from_err)?;
        }
        Ok(())
    }
}

/// Open a connection to `to`, send one message, and await the reply.
///
/// With n0 discovery enabled, `to` (an [`EndpointId`]) is all you need — iroh
/// resolves the address and picks a direct or relayed path automatically.
pub async fn send(endpoint: &Endpoint, to: EndpointId, msg: &AgentMessage) -> Result<AgentMessage> {
    let conn = endpoint.connect(to, ALPN).await.context("connecting to peer")?;
    let (mut send, mut recv) = conn.open_bi().await.context("opening stream")?;

    let out = serde_json::to_vec(msg)?;
    send.write_all(&out).await.context("writing request")?;
    send.finish().context("finishing request")?;

    let bytes = recv.read_to_end(MAX_MSG).await.context("reading reply")?;
    let reply = serde_json::from_slice(&bytes).context("decoding reply")?;
    conn.close(0u32.into(), b"done");
    Ok(reply)
}
