# weft

A peer-to-peer communication fabric for agents, built on [iroh](https://github.com/n0-computer/iroh).

Agents (and devices, and services) get a stable cryptographic identity and can
talk to each other directly — across NATs, without a central server. iroh
handles the hard parts (identity, hole-punching, relay fallback, discovery);
weft adds the thin layer agents actually use: **a messaging protocol, service
discovery, and optional payments**.

```
┌──────────── weft (this crate) ────────────┐
│  agent messaging  │  service discovery     │
│  (request/reply)  │  (gossip)   │  x402    │
├────────────────────────────────────────────┤
│                   iroh                      │
│  identity · NAT traversal · relay · disco   │
└─────────────────────────────────────────────┘
```

## Why

Getting two programs on different machines to talk usually means a broker, a
public IP, TURN servers, or a cloud queue. weft gives each node a portable
`EndpointId` and lets any node reach any other by that id alone. Connections are
**direct when hole-punching works, relayed when it doesn't** — automatically,
and transparently to your code.

## What you get

- **Base connectivity** — bind a node with one call; it's immediately reachable
  by its `EndpointId` from anywhere. → [docs/connectivity.md](docs/connectivity.md)
- **Bootstrapping & service discovery** — nodes announce what they offer on a
  shared gossip topic; others collect a live registry. On a LAN, **mDNS
  auto-discovers peers** (works offline) and folds them into the swarm with no
  `--bootstrap`. → [docs/service-discovery.md](docs/service-discovery.md)
- **Agent messaging** — a tiny JSON request/reply protocol between nodes.
- **Payments (x402)** — charge per request for relaying, API access, or any
  capability, using the [x402](https://x402.org) 402-Payment-Required handshake.
  → [docs/use-cases/x402.md](docs/use-cases/x402.md)
- **Agent-to-agent sessions** — two agents (e.g. two Claude Code sessions) talk
  by `EndpointId` on localhost or LAN. → [docs/use-cases/agent-sessions.md](docs/use-cases/agent-sessions.md)
- **IoT** — the same node model runs on a device: stable identity, no inbound
  port, reachable as it roams networks. → [docs/use-cases/iot.md](docs/use-cases/iot.md)
- **Self-hostable** — run your own relays with the `weft-relay` binary (and your
  own discovery), so the fabric depends on no third party.
  → [docs/self-hosting.md](docs/self-hosting.md)

Architecture overview: [docs/architecture.md](docs/architecture.md).

## Quick start

weft runs as a **background daemon**; the CLI talks to it over a local socket.

```bash
# Install the prebuilt binary (Linux/macOS, no Rust needed)
curl -fsSL https://raw.githubusercontent.com/d-roak/weft/main/install.sh | sh

# Node A — start the daemon and announce a service
weft --key a.json start --announce echo:demo
#   weft daemon started
#     id:  86a931be48cc4b2b…       ← copy this

# Node B — start its daemon, then message A by id
weft --key b.json start
weft --key b.json send 86a931be48cc4b2b… "hello"
#   ← ack "received"

# A reads what arrived, then shuts down
weft --key a.json inbox            #   ← message from …: "hello"
weft --key a.json stop
```

No relay setup, no port forwarding — nodes find each other through iroh's
discovery (or mDNS on a LAN) and connect directly or via a relay as needed.

### Running on your own infrastructure

Nodes use n0's public relays by default. To depend on no third party, run your
own relay and point nodes at it:

```bash
weft-relay --http-bind '[::]:8080'                  # on your server
weft start --relay http://relay.example.com:8080    # on each node
```

Discovery can be self-hosted too (`--pkarr-relay`). See
[docs/self-hosting.md](docs/self-hosting.md).

## CLI

The daemon holds the live node; every other command is a thin client to it.

| Command | What it does |
|---|---|
| `weft id` | Print this node's `EndpointId` (no daemon needed). |
| `weft start [--announce name:kind]… [--bootstrap <id>]…` | Start the node as a background daemon. |
| `weft stop` | Stop the running daemon. |
| `weft status` | Show whether the daemon is running, its id, and counters. |
| `weft send <to> <text>` | Send a message to a peer; print the reply. |
| `weft announce <name:kind>` | Announce a service on the fabric. |
| `weft services` | List services the daemon has discovered. |
| `weft inbox` | Print and clear messages the daemon has received. |
| `weft daemon` | Run the node in the foreground (what `start` launches). |

`start` and `daemon` also take network options: `--bootstrap <id>` (gossip entry
point), `--relay <url>` (`WEFT_RELAY`), and `--pkarr-relay <url>`
(`WEFT_PKARR_RELAY`). Omit them to use n0's public infrastructure.

A second binary, **`weft-relay`**, runs a relay server —
see [docs/self-hosting.md](docs/self-hosting.md).

Identity is stored at `~/.weft/key.json` (override with `--key`, which also
selects *which* daemon the CLI talks to). Keep the key to keep your
`EndpointId` stable across restarts. The control socket lives at
`/tmp/weft-<hash>.sock`; the pid and log sit next to the key file.

> **Bootstrapping note:** gossip discovery needs at least one peer to join
> through. The first node stands alone; later nodes pass `--bootstrap <id>` of a
> node already in the swarm. Direct `weft send <id>` messaging needs no
> bootstrap — the id is enough.

## Library

```rust
use weft::{Weft, AgentMessage, Config, load_or_create_secret_key};

let secret = load_or_create_secret_key("~/.weft/key.json")?;
// Config::default() = n0's public infra; set `relays`/`pkarr_relay` to self-host.
let (weft, mut inbox) = Weft::spawn(secret, Config::default()).await?;

// offer a service
weft.registry().announce("weather", "oracle", serde_json::json!({})).await?;

// receive
while let Some((msg, reply)) = inbox.recv().await {
    reply.send(AgentMessage::new(weft.id(), "ack", serde_json::json!("ok")));
}

// send to a peer (EndpointId is all you need)
let reply = weft.send(peer_id, &AgentMessage::new(weft.id(), "ping", serde_json::Value::Null)).await?;
```

## Examples

```bash
# Two agents chatting (localhost or LAN) — persistent identity each side
cargo run --example agent_chat -- --key agent1.json            # prints its id
cargo run --example agent_chat -- --key agent2.json --peer <agent1-id>

# IoT: a sensor node you can read from anywhere
cargo run --example iot_sensor -- sensor
cargo run --example iot_sensor -- read <sensor-id>

# x402: a paid relay — first call gets 402, second call pays and succeeds
cargo run --example x402_relay -- server
cargo run --example x402_relay -- client <server-id>
```

## Status

Working core, verified with two nodes over the public relay network. The x402
settlement and payment verification are stubbed at a single seam
(`x402::verify_payment`) for you to wire to a real facilitator — everything else
(the handshake, discovery, messaging, connectivity) is real.
