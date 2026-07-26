//! weft — a peer-to-peer communication fabric for agents, built on [iroh].
//!
//! iroh already gives us the hard parts of P2P: stable cryptographic node
//! identity, NAT traversal (direct hole-punching with relay fallback), and
//! discovery. weft is the thin layer on top that agents actually talk to:
//!
//! - **Base connectivity** — [`Weft::spawn`] binds an iroh endpoint using the
//!   relays and discovery named in [`Config`] (n0's public infrastructure by
//!   default, your own if configured). See [`agent`] for messaging.
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
use iroh::address_lookup::{PkarrPublisher, PkarrResolver};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointId, RelayMode, RelayUrl, SecretKey, endpoint::presets};
use iroh_gossip::net::Gossip;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use url::Url;

pub mod agent;
pub mod discovery;
pub mod x402;

pub use agent::{AgentMessage, Inbox};
pub use discovery::{ServiceAnnouncement, ServiceRegistry};

/// How a node reaches the network: which relays to use, which discovery
/// service to publish to, and who to bootstrap gossip through.
///
/// [`Config::default`] uses n0's public infrastructure. Set [`Config::relays`]
/// and/or [`Config::pkarr_relay`] to run entirely on your own — see
/// `docs/self-hosting.md`.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Peers to join the gossip swarm through. Empty = first node / rely on
    /// mDNS on the LAN.
    pub bootstrap: Vec<EndpointId>,
    /// Relay servers to use. Empty = n0's public relays.
    ///
    /// Run your own with the `weft-relay` binary.
    pub relays: Vec<RelayUrl>,
    /// pkarr relay used to publish and resolve endpoint addresses.
    /// `None` = n0's public DNS/pkarr discovery.
    ///
    /// Setting this *replaces* n0 discovery, so a self-hosted deployment does
    /// not depend on n0 infrastructure at all.
    pub pkarr_relay: Option<Url>,
}

impl Config {
    /// Bootstrap through the given peers, keeping default (n0) infrastructure.
    pub fn with_bootstrap(bootstrap: Vec<EndpointId>) -> Self {
        Self { bootstrap, ..Default::default() }
    }

    /// True when this config points at self-hosted infrastructure.
    pub fn is_self_hosted(&self) -> bool {
        !self.relays.is_empty() || self.pkarr_relay.is_some()
    }
}

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
    /// Spawn a node with the given identity and [`Config`].
    ///
    /// Returns the node and an [`Inbox`] receiving agent messages addressed to
    /// this node.
    pub async fn spawn(secret_key: SecretKey, config: Config) -> Result<(Self, Inbox)> {
        // Start from n0's defaults (public relays + DNS/pkarr discovery), then
        // override whichever pieces the config self-hosts.
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![agent::ALPN.to_vec(), iroh_gossip::ALPN.to_vec()]);

        if let Some(pkarr) = &config.pkarr_relay {
            // Replace n0 discovery entirely — publish and resolve through our
            // own pkarr relay so no n0 service is involved.
            builder = builder
                .clear_address_lookup()
                .address_lookup(PkarrPublisher::builder(pkarr.clone()))
                .address_lookup(PkarrResolver::builder(pkarr.clone()));
        }

        if !config.relays.is_empty() {
            builder = builder.relay_mode(RelayMode::Custom(
                config.relays.iter().cloned().collect(),
            ));
        }

        // mDNS on top of whichever discovery is configured: advertise + resolve
        // peers on the local network, so LAN nodes find each other with no
        // relay, no bootstrap, and even with no internet.
        let endpoint = builder
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

        let registry =
            ServiceRegistry::spawn(gossip.clone(), endpoint.id(), config.bootstrap).await?;

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
