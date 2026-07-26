# Connectivity: direct and relayed

weft nodes address each other by `EndpointId` — a 32-byte public key. How the
bytes actually travel is decided by iroh, per connection, and upgrades live.

## The two paths

| Path | When | Latency |
|---|---|---|
| **Direct** | Hole-punching succeeds (most home/office NATs). | Lowest — packets go peer-to-peer. |
| **Relayed** | While hole-punching is in progress, or when NAT/firewall blocks direct (symmetric NAT, strict firewalls). | Higher — packets bounce through a relay. |

A connection **starts relayed** (so it works immediately) and **upgrades to
direct** the moment hole-punching completes. Your code never chooses; it just
sees a `Connection`. This is entirely iroh's mechanism — weft adds nothing here.

```
peer A ──────── relay ──────── peer B     ① connect: works right away, relayed
peer A ─────────────────────── peer B     ② upgraded: direct once hole-punched
```

## How a peer is found

With the `presets::N0` preset (what `Weft::spawn` uses), a node:

1. Publishes its address record (relay URL + direct addrs) to n0's DNS/pkarr
   discovery, keyed by its `EndpointId`.
2. On `connect(endpoint_id, alpn)`, resolves that record from the id, reaches
   the peer via its home relay, then hole-punches for a direct path.

So on the happy path, **the `EndpointId` alone is enough to connect** — no IP,
no ticket, no manual address exchange.

### Local network (mDNS)

weft also registers an **mDNS** address lookup, so on a LAN nodes advertise and
resolve each other over multicast — no internet, no relay, no DNS required. This
runs *alongside* n0 DNS discovery: mDNS handles same-LAN peers (including fully
offline networks), DNS handles everyone else. mDNS-discovered nodes are also fed
into gossip's known peers, so a LAN fabric self-assembles without `--bootstrap`
(see [service-discovery.md](service-discovery.md)).

## NAT traversal, concretely

- **Full-cone / restricted / port-restricted NAT** → direct connection after
  hole-punching.
- **Symmetric NAT / strict firewall** → stays relayed; still fully functional,
  just with a relay in the path.
- **Both peers behind the same LAN** → local direct connection.

No port forwarding or public IP is required on either side.

## Relays

A relay is a lightweight rendezvous + fallback data path. `presets::N0` uses
n0's public relays by default. You can point at your own by configuring the
endpoint builder's relay mode (`RelayMode`) if you need to keep traffic on your
own infrastructure — see the iroh docs. Relaying itself can be monetized; see
[use-cases/x402.md](use-cases/x402.md).

## Identity

Identity is a `SecretKey`; the public half is the `EndpointId`. Persist the key
(weft stores it at `~/.weft/key.json`) and your address is stable across
restarts and network changes — a device can move between Wi-Fi and cellular and
remain reachable at the same id.
