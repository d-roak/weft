# Service discovery & bootstrapping

Two nodes can talk if they know each other's `EndpointId`. Discovery is how a
node *finds* ids and capabilities it didn't already know — without a central
directory.

## The mechanism: one gossip topic

Every weft node subscribes to a single well-known topic,
`DISCOVERY_TOPIC` ([src/discovery.rs](../crates/weft/src/discovery.rs)). To offer a
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
weft --key n1.json start --announce oracle:weather
#   id:  AAAA…

# node 2 — joins through node 1
weft --key n2.json start --bootstrap AAAA…
weft --key n2.json services
#   • weather (oracle) @ AAAA…
```

Once joined, gossip stitches the node into the mesh and announcements propagate
peer-to-peer; you don't need every node's id, just one reachable bootstrap.

### Why a bootstrap needs only the id — no IP or port

`--bootstrap AAAA…` takes just an `EndpointId`, and that is genuinely all it
needs. Here's why.

**An `EndpointId` is a public key, not an address.** It's the node's permanent
identity (the public half of its `SecretKey`). It says *who* to reach, never
*where* — the "where" (IP addresses, UDP ports, relay) changes as a node moves
between networks, but the id never does.

**The "where" is looked up from the "who" at connect time.** Every node
continuously *publishes* its own reachability record — its home **relay URL** and
whatever **direct socket addresses** (IP:port) it currently has — into discovery,
signed by its secret key and keyed by its `EndpointId`. Two discovery backends
run at once:

- **n0 DNS / pkarr** (the default `presets::N0`): the record is published to a
  DNS-based key-value system and resolved with an HTTPS/DNS lookup on the id.
  Works across the internet.
- **mDNS** on the local network: the record is announced over multicast and
  resolved by any node on the same LAN — no internet, no DNS.

So when you `--bootstrap AAAA…`, iroh does roughly:

```
EndpointId AAAA…  ──resolve──▶  { relay: https://relay.example,
                                  direct: [192.168.1.5:51820, …] }
```

then dials those addresses (and, in parallel, the relay). You never typed an IP
or port because the node *told the network* its current ones, and the id is the
key you look them up under.

**Why this is safe.** The address record is signed by the node's secret key and
the QUIC/TLS handshake authenticates the peer against the `EndpointId`. A wrong
or spoofed address simply fails the handshake — you always end up talking to the
holder of that id or to no one. That's also why the id can't be shortened to an
IP: the id *is* the cryptographic identity the connection is verified against.

**What if the address isn't known yet?** The connection still succeeds: iroh
reaches the peer through its **relay** first (also resolved from the id), so
bytes flow immediately, then hole-punches a **direct** path in the background and
switches over once it's up. See [connectivity.md](connectivity.md).

The upshot: an `EndpointId` is a stable, self-authenticating handle; addresses
are ephemeral details the fabric resolves for you. Pin a peer's id once and it
stays reachable across reboots, Wi-Fi↔cellular changes, and IP reassignments.

### Public infrastructure is a phone book, not a party line

A common expectation is that because every node shares `DISCOVERY_TOPIC` and
every node talks to n0's public relays, the swarm should assemble itself. It
doesn't, and the reason is worth being blunt about:

- **Relays** forward packets and coordinate hole-punching for a node you are
  *already* trying to reach.
- **pkarr / DNS discovery** publishes *your own* record, keyed by your public key.

Both answer *"how do I reach node X?"*. Neither answers *"who else is out
there?"* — there is no global peer list, no DHT to crawl, and topic membership
is registered nowhere. Two nodes on the same topic with no path between them are
two separate meshes that happen to share a name. Somebody has to already know
somebody.

Common bootstrap sources:

- **mDNS on a LAN — automatic.** Every node weft discovers over local multicast
  is added to gossip's known peers for you, so nodes on the same network join the
  swarm with no `--bootstrap` at all. This is handled in
  [`crates/weft/src/discovery.rs`](../crates/weft/src/discovery.rs) and works offline.
- **A seed list — automatic across the internet.** `weft::DEFAULT_BOOTSTRAP`
  holds the ids of long-lived seed nodes, exactly as kademlia and libp2p ship a
  bootstrap list. Run one as a
  [long-lived `weft daemon`](self-hosting.md#running-a-bootstrap-node).
- Any peer id a user already has, or out-of-band exchange (QR, paste, another
  channel).

### Saving bootstrap peers

Retyping `--bootstrap` on every `start` gets old. `weft config` persists the
network options next to the key file, and the daemon reads them at startup:

```bash
weft config set bootstrap AAAA… BBBB…   # ids are validated before writing
weft config show
weft start                              # no flags needed
```

Precedence is **flag → saved config → `DEFAULT_BOOTSTRAP`**. A flag that is
passed replaces the saved list rather than appending to it, and
`weft config set bootstrap` with no ids writes an empty list, which opts out of
the built-in seeds entirely.

> **Direct messaging needs no bootstrap.** If you already have a peer's
> `EndpointId`, `weft send <id>` connects straight to it. Bootstrapping is only
> for *discovering* ids you don't have yet.

## Scoping the fabric

`DISCOVERY_TOPIC` is a fixed constant, so all weft nodes share one discovery
namespace. To run an isolated fabric (e.g. per-tenant or per-deployment),
derive a topic from a network name and have nodes subscribe to that instead —
the `ponytail:` note in `crates/weft/src/discovery.rs` marks the spot.
