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
use serde::{Deserialize, Serialize};
use url::Url;

pub mod agent;
pub mod discovery;
pub mod x402;

pub use agent::{AgentMessage, Inbox};
pub use discovery::{ServiceAnnouncement, ServiceRegistry};

/// Bootstrap peers every node starts from, the way kademlia and libp2p ship a
/// seed list.
///
/// This exists because no amount of iroh gets two nodes into the same gossip
/// swarm on its own: relays and pkarr/DNS answer *"how do I reach node X?"*,
/// never *"who else is out there?"*. Off a LAN (where mDNS covers it, see
/// [`discovery`]) somebody has to already know somebody.
///
/// Entries are [`EndpointId`]s, never hostnames: an `EndpointId` *is* the
/// public key, and iroh needs it before any lookup — it's the TLS identity. So
/// the seed's DNS name belongs in the comment and its id in the string. Pinning
/// the key is also what stops someone else answering for that name.
///
/// Populate it by running long-lived seeds (`weft daemon`, see
/// `docs/self-hosting.md`) and pasting the id each one prints. Users override
/// per-node with `weft config set bootstrap …`, and an explicitly empty list
/// there opts out entirely.
pub const DEFAULT_BOOTSTRAP: &[&str] = &[
    // "…", // bootstrap1.droak.sh
    // "…", // bootstrap2.droak.sh
];

/// How a node reaches the network: which relays to use, which discovery
/// service to publish to, and who to bootstrap gossip through.
///
/// [`Config::default`] uses n0's public infrastructure. Set [`Config::relays`]
/// and/or [`Config::pkarr_relay`] to run entirely on your own — see
/// `docs/self-hosting.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            // A malformed constant is a build-time typo, not a runtime state.
            bootstrap: DEFAULT_BOOTSTRAP
                .iter()
                .map(|id| id.parse().expect("DEFAULT_BOOTSTRAP entry is not an EndpointId"))
                .collect(),
            relays: Vec::new(),
            pkarr_relay: None,
        }
    }
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

    /// Read a saved config, falling back to [`Config::default`].
    ///
    /// A missing file is the normal case. A *corrupt* file warns and falls back
    /// rather than failing: a hand-edited config shouldn't leave a node unable
    /// to start.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let Ok(text) = std::fs::read_to_string(path.as_ref()) else {
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(path = %path.as_ref().display(), %err, "ignoring unreadable config");
                Self::default()
            }
        }
    }

    /// Persist this config so later runs pick it up.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join("weft-config-test");
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);

        // Missing file → defaults.
        assert_eq!(Config::load(&path).bootstrap.len(), DEFAULT_BOOTSTRAP.len());

        let id: EndpointId = "ff87a0b0a3c7c0ce827e9cada5ff79e75a44a0633bfcb5b50f99307ddb26b337"
            .parse()
            .unwrap();
        Config::with_bootstrap(vec![id]).save(&path).unwrap();
        assert_eq!(Config::load(&path).bootstrap, vec![id]);

        // A partial file keeps serde defaults for the rest.
        std::fs::write(&path, r#"{"relays":[]}"#).unwrap();
        assert!(Config::load(&path).pkarr_relay.is_none());

        // A corrupt file must not brick the node.
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Config::load(&path).bootstrap.len(), DEFAULT_BOOTSTRAP.len());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
