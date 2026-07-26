//! Service discovery and bootstrapping over a shared gossip topic.
//!
//! Every weft node subscribes to one well-known [`TopicId`]. To offer a
//! capability a node broadcasts a [`ServiceAnnouncement`]; every subscriber
//! collects those into a local registry. This doubles as bootstrapping: pass a
//! peer's [`EndpointId`] as a bootstrap and gossip stitches you into the swarm,
//! after which announcements flow without any central directory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::EndpointId;
use iroh_gossip::TopicId;
use iroh_gossip::api::Event;
use iroh_gossip::net::Gossip;
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};

/// The one topic every weft node joins for service discovery.
///
/// ponytail: a fixed constant, not derived from a network name — swap in a
/// per-network hash when you want isolated fabrics.
pub const DISCOVERY_TOPIC: TopicId = TopicId::from_bytes(*b"weft.service.discovery.topic.v0!");

/// Re-announce interval so late joiners learn about long-lived services and
/// stale entries fade if a node goes away without withdrawing.
const REANNOUNCE: Duration = Duration::from_secs(30);

/// A service a node offers to the fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAnnouncement {
    /// The node offering the service — connect here with the agent protocol.
    pub endpoint_id: EndpointId,
    /// Unique-per-node service name, e.g. `"weather-oracle"`.
    pub name: String,
    /// Coarse category, e.g. `"relay"`, `"sensor"`, `"inference"`.
    pub kind: String,
    /// Free-form metadata (endpoints, schema, capabilities).
    #[serde(default)]
    pub meta: serde_json::Value,
    /// Set when the service charges via x402. Carries the price so a consumer
    /// can decide before it even connects. See [`crate::x402`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<crate::x402::PriceTag>,
}

/// A live view of services announced on the fabric, plus a handle to announce
/// your own. Cloneable and cheap — all clones share the same state.
#[derive(Clone)]
pub struct ServiceRegistry {
    me: EndpointId,
    services: Arc<Mutex<HashMap<String, ServiceAnnouncement>>>,
    outgoing: Arc<tokio::sync::mpsc::Sender<ServiceAnnouncement>>,
}

impl ServiceRegistry {
    /// Subscribe to the discovery topic and start collecting announcements.
    pub(crate) async fn spawn(
        gossip: Gossip,
        me: EndpointId,
        bootstrap: Vec<EndpointId>,
    ) -> Result<Self> {
        let topic = gossip
            .subscribe(DISCOVERY_TOPIC, bootstrap)
            .await
            .context("subscribing to discovery topic")?;
        let (sender, mut receiver) = topic.split();

        // Fold mDNS-discovered LAN nodes into gossip's known peers: a listen-only
        // mDNS instance surfaces every endpoint it sees on the local network, and
        // we `join_peers` each one so the swarm self-assembles on a LAN with no
        // manual `--bootstrap`. The endpoint's own mDNS lookup (see lib.rs)
        // resolves their addresses when gossip dials them.
        let mdns_sender = sender.clone();
        tokio::spawn(async move {
            let mdns = match MdnsAddressLookup::builder().advertise(false).build(me) {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(%err, "mDNS discovery unavailable; LAN auto-peering off");
                    return;
                }
            };
            let mut events = mdns.subscribe().await;
            let mut joined = std::collections::HashSet::new();
            while let Some(event) = events.next().await {
                if let DiscoveryEvent::Discovered { endpoint_info, .. } = event {
                    let id = endpoint_info.endpoint_id;
                    // mDNS re-emits on every tick; only join each peer once.
                    if id != me && joined.insert(id) {
                        tracing::debug!(peer = %id, "mDNS: adding LAN peer to gossip");
                        let _ = mdns_sender.join_peers(vec![id]).await;
                    }
                }
            }
        });

        let services: Arc<Mutex<HashMap<String, ServiceAnnouncement>>> = Default::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ServiceAnnouncement>(16);

        // Inbound: fold received announcements into the registry.
        let store = services.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.next().await {
                let Ok(Event::Received(msg)) = event else { continue };
                if let Ok(ann) = serde_json::from_slice::<ServiceAnnouncement>(&msg.content) {
                    store
                        .lock()
                        .unwrap()
                        .insert(format!("{}/{}", ann.endpoint_id, ann.name), ann);
                }
            }
        });

        // Outbound: broadcast on request and periodically re-broadcast the last
        // announcement so the swarm keeps our services fresh.
        tokio::spawn(async move {
            let mut last: Option<ServiceAnnouncement> = None;
            let mut tick = tokio::time::interval(REANNOUNCE);
            loop {
                tokio::select! {
                    Some(ann) = rx.recv() => {
                        if let Ok(bytes) = serde_json::to_vec(&ann) {
                            let _ = sender.broadcast(bytes.into()).await;
                        }
                        last = Some(ann);
                    }
                    _ = tick.tick() => {
                        if let Some(ann) = &last
                            && let Ok(bytes) = serde_json::to_vec(ann) {
                            let _ = sender.broadcast(bytes.into()).await;
                        }
                    }
                    else => break,
                }
            }
        });

        Ok(Self { me, services, outgoing: Arc::new(tx) })
    }

    /// Announce a service you offer. Re-announced periodically until the node
    /// exits. Calling again with the same details refreshes; ponytail: no
    /// explicit withdraw — entries simply stop being refreshed and age out on
    /// consumers that track freshness.
    pub async fn announce(&self, name: &str, kind: &str, meta: serde_json::Value) -> Result<()> {
        self.announce_full(ServiceAnnouncement {
            endpoint_id: self.me,
            name: name.into(),
            kind: kind.into(),
            meta,
            price: None,
        })
        .await
    }

    /// Announce a service, including an optional x402 price.
    pub async fn announce_full(&self, ann: ServiceAnnouncement) -> Result<()> {
        // Reflect our own service locally too, so `list()` includes it.
        self.services
            .lock()
            .unwrap()
            .insert(format!("{}/{}", ann.endpoint_id, ann.name), ann.clone());
        self.outgoing.send(ann).await.context("registry task stopped")?;
        Ok(())
    }

    /// Snapshot of all services currently known on the fabric.
    pub fn list(&self) -> Vec<ServiceAnnouncement> {
        self.services.lock().unwrap().values().cloned().collect()
    }

    /// Services matching a `kind`, e.g. all `"relay"` nodes.
    pub fn find(&self, kind: &str) -> Vec<ServiceAnnouncement> {
        self.services
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.kind == kind)
            .cloned()
            .collect()
    }
}
