//! weft — a peer-to-peer communication fabric for agents, built on [iroh].
//!
//! iroh already gives us the hard parts of P2P: stable cryptographic node
//! identity, NAT traversal (direct hole-punching with relay fallback), and
//! discovery. weft is the thin layer on top that agents actually talk to:
//!
//! - **Base connectivity** — [`Weft::spawn`] binds an iroh endpoint with the
//!   n0 preset (relay + DNS/pkarr discovery). See [`agent`] for messaging.
//! - **Node bootstrapping & service discovery** — a shared gossip topic where
//!   nodes announce the services they offer. See [`discovery`].
//! - **Direct or relayed** — handled entirely by iroh. A connection starts
//!   relayed and upgrades to direct once hole-punching succeeds; nothing in
//!   weft has to care which path a packet took.
//! - **Payments (x402)** — an optional payment envelope so a node can charge
//!   for relaying, API access, or a discovered service. See [`x402`].
//!
//! [iroh]: https://docs.rs/iroh

use std::path::Path;

use anyhow::{Context, Result};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::presets};
use iroh::protocol::Router;
use iroh_gossip::net::Gossip;
use iroh_mdns_address_lookup::MdnsAddressLookup;

pub mod agent;
pub mod discovery;
pub mod x402;

pub use agent::{AgentMessage, Inbox};
pub use discovery::{ServiceAnnouncement, ServiceRegistry};

/// A running weft node: an iroh endpoint wired up with gossip-based service
/// discovery and the agent messaging protocol.
#[derive(Clone)]
pub struct Weft {
    endpoint: Endpoint,
    gossip: Gossip,
    registry: ServiceRegistry,
    _router: Router,
}

impl Weft {
    /// Spawn a node with the given identity, joining the gossip swarm through
    /// `bootstrap` peers (empty = you are the first node / rely on discovery).
    ///
    /// Returns the node and an [`Inbox`] receiving agent messages addressed to
    /// this node.
    pub async fn spawn(secret_key: SecretKey, bootstrap: Vec<EndpointId>) -> Result<(Self, Inbox)> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![agent::ALPN.to_vec(), iroh_gossip::ALPN.to_vec()])
            // mDNS on top of n0's DNS discovery: advertise + resolve peers on
            // the local network, so LAN nodes find each other with no relay,
            // no bootstrap, and even with no internet.
            .address_lookup(MdnsAddressLookup::builder())
            .bind()
            .await
            .context("binding iroh endpoint")?;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let (handler, inbox) = agent::AgentHandler::new(endpoint.id());

        let router = Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(agent::ALPN, handler)
            .spawn();

        let registry = ServiceRegistry::spawn(gossip.clone(), endpoint.id(), bootstrap).await?;

        Ok((
            Self { endpoint, gossip, registry, _router: router },
            inbox,
        ))
    }

    /// This node's stable public identity. Share it so peers can reach you.
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// The underlying iroh endpoint, for direct use of the connectivity layer.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The gossip handle, for joining additional topics.
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }

    /// The service registry: announce what you offer, list what others offer.
    pub fn registry(&self) -> &ServiceRegistry {
        &self.registry
    }

    /// Send an agent message to `to` and await the reply.
    pub async fn send(&self, to: EndpointId, msg: &AgentMessage) -> Result<AgentMessage> {
        agent::send(&self.endpoint, to, msg).await
    }
}

/// Load a persisted node identity from `path`, creating one if absent.
///
/// Keeping the secret key stable means your [`EndpointId`] is stable — peers
/// can save it and reconnect across restarts.
pub fn load_or_create_secret_key(path: impl AsRef<Path>) -> Result<SecretKey> {
    let path = path.as_ref();
    if let Ok(text) = std::fs::read_to_string(path) {
        return serde_json::from_str(text.trim()).context("parsing stored secret key");
    }
    let key = SecretKey::generate();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, serde_json::to_string(&key)?).context("writing secret key")?;
    Ok(key)
}
