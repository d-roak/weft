# Architecture

weft is a thin fabric over [iroh]. iroh provides the transport; weft provides
the protocols agents speak on top of it.

```
        your agent / device / service
                    │
        ┌───────────┴────────────┐
        │          Weft          │   crates/weft/src/lib.rs
        │  ┌──────────────────┐  │
        │  │ agent messaging  │  │   crates/weft/src/agent.rs   ALPN weft/agent/0
        │  ├──────────────────┤  │
        │  │ service registry │  │   crates/weft/src/discovery.rs  (gossip topic)
        │  ├──────────────────┤  │
        │  │ x402 payments    │  │   crates/weft/src/x402.rs    (over agent messages)
        │  └──────────────────┘  │
        └───────────┬────────────┘
                    │
          iroh Endpoint + Router
   identity · NAT traversal · relay · discovery · QUIC
```

## The node

A [`Weft`](../crates/weft/src/lib.rs) node owns three iroh pieces:

- **Endpoint** — the iroh object. Bound with the `presets::N0` preset, which
  turns on relay + n0 DNS/pkarr discovery. Its identity is a `SecretKey`; the
  public half is the `EndpointId` everyone addresses.
- **Router** — dispatches incoming connections by ALPN to a protocol handler.
  weft registers two: `iroh_gossip::ALPN` (discovery) and `weft/agent/0`
  (messaging).
- **Gossip** — an [`iroh-gossip`] instance used as the discovery bus.

`Weft::spawn(secret, config)` wires all three together and returns the node
plus an `Inbox` of incoming agent messages.

## Protocols

### Agent messaging (`crates/weft/src/agent.rs`)

Request/reply over a QUIC bidirectional stream. One JSON [`AgentMessage`]
(`{ from, kind, body }`) per stream; the receiver replies with another. Message
length is delimited by end-of-stream, so there's no framing to maintain. The
handler forwards each request to the node's `Inbox`, whose consumer produces the
reply via a one-shot `Reply` channel.

### Service discovery (`crates/weft/src/discovery.rs`)

Every node subscribes to one well-known gossip `TopicId`. Announcing a service
broadcasts a `ServiceAnnouncement`; every subscriber folds those into a local
`HashMap`. Announcements are re-broadcast periodically so late joiners catch up
and stale entries fade. A listen-only **mDNS** subscription feeds every
LAN-discovered `EndpointId` into gossip's known peers (`join_peers`), so a local
fabric self-assembles with no bootstrap. See
[service-discovery.md](service-discovery.md).

### x402 payments (`crates/weft/src/x402.rs`)

Not a separate transport — a convention *on top of* agent messages. An unpaid
`invoke` gets a `x402/payment-required` reply carrying the price and a nonce;
the caller retries with a `payment` in the body. See
[use-cases/x402.md](use-cases/x402.md).

## Process model

The `weft` binary runs as a **background daemon** that owns the live node; CLI
commands are thin clients. `weft start` spawns the daemon in its own process
group (survives terminal close), and `send`/`services`/`inbox`/… connect to it
over a Unix socket at `/tmp/weft-<hash>.sock` (keyed by the identity file, so one
`--key` = one daemon). The protocol is one newline-delimited JSON request/
response per connection — see [`crates/weft/src/control.rs`](../crates/weft/src/control.rs). Inbound agent
messages are buffered in the daemon and drained by `weft inbox`.

## Design choices

- **iroh does connectivity; weft does not.** No custom NAT logic, no relay code.
  Direct vs relayed is iroh's call, made per connection.
- **EndpointId is the only address you need** — with n0 discovery, iroh resolves
  the current relay + direct addresses from the id. No ticket/address passing
  for the common case.
- **Gossip, not a directory server.** Discovery is decentralized; the only
  centralized dependency is the default relay/DNS, which is replaceable.
- **JSON on the wire.** Agents exchange small control messages; legibility beats
  micro-optimization. Stream bulk data over a dedicated ALPN instead.

[iroh]: https://docs.rs/iroh
[iroh-gossip]: https://docs.rs/iroh-gossip
[`AgentMessage`]: ../crates/weft/src/agent.rs
[`Weft`]: ../crates/weft/src/lib.rs
