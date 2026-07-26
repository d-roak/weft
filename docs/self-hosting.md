# Self-hosting weft infrastructure

By default a weft node uses [n0]'s public infrastructure: their relay servers
and their DNS/pkarr discovery. That's convenient, and it's a third party in your
path. This page shows how to run the whole fabric on your own machines.

There are two independent pieces:

| Piece | What it does | How to self-host |
|---|---|---|
| **Relay** | Rendezvous + fallback data path between peers. | Run `weft-relay`, point nodes at it with `--relay`. |
| **Discovery** | Maps an `EndpointId` → its current addresses. | Run a [pkarr] relay, point nodes at it with `--pkarr-relay`. |

You can self-host either one alone. Relays are the usual starting point.

> On a LAN you may need neither: weft's built-in mDNS discovers peers over
> multicast and connects directly, with no relay and no internet. See
> [service-discovery.md](service-discovery.md).

## Running a relay

The `weft-relay` binary is a relay server. Plain HTTP by default — no
certificates needed:

```bash
weft-relay --http-bind '[::]:8080'
#   weft-relay: http  [::]:8080
```

Then point nodes at it:

```bash
weft start --relay http://relay.example.com:8080
```

Every node that shares a relay can reach every other node through it. Nodes
still hole-punch a **direct** path when they can — the relay carries traffic
only until that succeeds, and as a fallback when it can't.

### With TLS

Pass a certificate chain and key to serve HTTPS and enable QUIC address
discovery (which improves NAT traversal):

```bash
weft-relay \
  --tls-cert /etc/weft/fullchain.pem \
  --tls-key  /etc/weft/privkey.pem \
  --http-bind '[::]:80' \
  --https-bind '[::]:443'
```

Nodes then use `--relay https://relay.example.com`.

Certificates come from your existing issuer (certbot, cert-manager, an internal
CA). `weft-relay` deliberately has no ACME mode; if you want the relay itself to
obtain certificates, upstream's `iroh-relay` binary supports Let's Encrypt.

### Behind a proxy or load balancer

Run `weft-relay` in plain-HTTP mode and terminate TLS at your proxy. The relay
speaks HTTP + WebSocket, so any proxy that forwards WebSocket upgrades works.
Point nodes at the proxy's public URL.

### Ports

| Port | Purpose | Needed |
|---|---|---|
| 8080 (or 80) | HTTP relay + WebSocket | always |
| 443 | HTTPS relay | with `--tls-cert` |
| 7842 | QUIC address discovery (UDP) | with TLS, recommended |
| — | metrics, via `--metrics-bind` | optional |

### Options

All flags have environment-variable equivalents, which is handy in systemd or
containers:

| Flag | Env | Default |
|---|---|---|
| `--http-bind` | `WEFT_RELAY_HTTP_BIND` | `[::]:8080` |
| `--tls-cert` | `WEFT_RELAY_TLS_CERT` | — |
| `--tls-key` | `WEFT_RELAY_TLS_KEY` | — |
| `--https-bind` | `WEFT_RELAY_HTTPS_BIND` | `[::]:443` |
| `--quic-bind` | `WEFT_RELAY_QUIC_BIND` | `[::]:7842` |
| `--metrics-bind` | `WEFT_RELAY_METRICS_BIND` | off |

### Running it as a service

```ini
# /etc/systemd/system/weft-relay.service
[Unit]
Description=weft relay
After=network-online.target

[Service]
ExecStart=/usr/local/bin/weft-relay
Environment=WEFT_RELAY_HTTP_BIND=[::]:8080
Restart=always
DynamicUser=yes

[Install]
WantedBy=multi-user.target
```

The relay holds no identity or persistent state, so it needs no data directory
and is safe to restart or run several of.

## Self-hosting discovery

A relay gets bytes between peers, but a node still has to *find* a peer's
address from its `EndpointId` (see
[service-discovery.md](service-discovery.md#why-a-bootstrap-needs-only-the-id--no-ip-or-port)).
By default that lookup goes to n0's DNS. To run your own, point nodes at your
own [pkarr] relay:

```bash
weft start --pkarr-relay https://pkarr.example.com
```

This **replaces** n0 discovery entirely — nodes publish their address records to
your pkarr relay and resolve other nodes through it, so no n0 service is
involved. Combine with `--relay` for a fully independent fabric:

```bash
weft start \
  --relay       http://relay.example.com:8080 \
  --pkarr-relay https://pkarr.example.com
```

weft doesn't ship a pkarr relay binary — the [pkarr] project provides one.

## Configuring nodes

Both options are repeatable/settable via flags or environment variables, and
work on `weft start` and `weft daemon`:

| Flag | Env | Meaning |
|---|---|---|
| `--relay <url>` | `WEFT_RELAY` | Relay to use (repeatable). Default: n0's relays. |
| `--pkarr-relay <url>` | `WEFT_PKARR_RELAY` | Discovery service. Default: n0's DNS. |

From Rust, the same thing via [`Config`](../crates/weft/src/lib.rs):

```rust
use weft::{Config, Weft};

let config = Config {
    relays: vec!["http://relay.example.com:8080".parse()?],
    pkarr_relay: Some("https://pkarr.example.com".parse()?),
    ..Default::default()
};
let (weft, inbox) = Weft::spawn(secret_key, config).await?;
```

## Verifying it works

Start a relay and a node against it, and check the node adopts it as its home
relay:

```bash
weft-relay --http-bind 127.0.0.1:8080 &
RUST_LOG=iroh::socket=info weft --key a.json daemon --relay http://127.0.0.1:8080
#   ... home is now relay http://127.0.0.1:8080/, was None
```

That log line is the confirmation: the node is routing through your relay, not
n0's.

[n0]: https://n0.computer
[pkarr]: https://pkarr.org
