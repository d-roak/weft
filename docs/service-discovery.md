# Service discovery & bootstrapping

Two nodes can talk if they know each other's `EndpointId`. Discovery is how a
node *finds* ids and capabilities it didn't already know — without a central
directory.

## The mechanism: one gossip topic

Every weft node subscribes to a single well-known topic,
`DISCOVERY_TOPIC` ([src/discovery.rs](../src/discovery.rs)). To offer a
capability, a node broadcasts a `ServiceAnnouncement`:

```jsonc
{
  "endpoint_id": "86a931be…",   // where to connect (agent protocol)
  "name": "weather-oracle",      // unique per node
  "kind": "oracle",              // category to filter on
  "meta": { "schema": "…" },     // free-form
  "price": { "amount": 1000, "asset": "USDC", … }  // optional x402 price
}
```

Every subscriber folds announcements into a local `ServiceRegistry`. Query it:

```rust
weft.registry().list();            // everything known
weft.registry().find("relay");     // just relay nodes
```

Announcements are **re-broadcast every 30s** so late joiners learn about
long-lived services, and entries you stop refreshing naturally age out on
consumers that track freshness. There's no central registry to fail — the live
view is assembled independently by each node.

## Bootstrapping into the swarm

Gossip needs an entry point. A brand-new node knows no peers, so it must join
*through* one:

```bash
# node 1 — the first node, stands alone
weft up --announce oracle:weather
#   id: AAAA…

# node 2 — joins through node 1
weft services --bootstrap AAAA…
#   • weather (oracle) @ AAAA…
```

Once joined, gossip stitches the node into the mesh and announcements propagate
peer-to-peer; you don't need every node's id, just one reachable bootstrap.

Common bootstrap sources:

- **mDNS on a LAN — automatic.** Every node weft discovers over local multicast
  is added to gossip's known peers for you, so nodes on the same network join the
  swarm with no `--bootstrap` at all. This is handled in
  [`src/discovery.rs`](../src/discovery.rs) and works offline.
- A few long-lived "seed" nodes whose ids you ship in config.
- Any peer id a user already has.
- Out-of-band exchange (QR, paste, another channel).

> **Direct messaging needs no bootstrap.** If you already have a peer's
> `EndpointId`, `weft send <id>` connects straight to it. Bootstrapping is only
> for *discovering* ids you don't have yet.

## Scoping the fabric

`DISCOVERY_TOPIC` is a fixed constant, so all weft nodes share one discovery
namespace. To run an isolated fabric (e.g. per-tenant or per-deployment),
derive a topic from a network name and have nodes subscribe to that instead —
the `ponytail:` note in `src/discovery.rs` marks the spot.
