# Use case: IoT

An IoT device is just a weft node. That single fact solves most of what makes
IoT connectivity painful.

## What the device model gives you

- **Stable identity** — the device's `EndpointId` is derived from a key it holds.
  It's the device's permanent address, independent of IP, carrier, or network.
- **No inbound port, no port forwarding** — the device dials out; peers reach it
  through iroh's discovery + relay. It works behind cellular CGNAT, home routers,
  and corporate firewalls with nothing opened.
- **Roaming** — move from Wi-Fi to LTE and the `EndpointId` doesn't change.
  In-flight connections migrate; new ones resolve the fresh address from the id.
- **End-to-end encryption** — every connection is authenticated QUIC/TLS keyed to
  the device identity. No broker sees the plaintext.
- **Direct when possible** — a phone on the same LAN as the device talks to it
  directly; a phone across the world falls back to a relay. Same code.

## Shape of a device node

```rust
let (weft, mut inbox) = Weft::spawn(persisted_secret_key, vec![]).await?;
weft.registry().announce("thermostat-1", "sensor",
    serde_json::json!({ "unit": "celsius" })).await?;

while let Some((msg, reply)) = inbox.recv().await {
    if msg.kind == "read" {
        let celsius = read_gpio_sensor();          // ← your hardware here
        reply.send(AgentMessage::new(weft.id(), "reading",
            serde_json::json!({ "celsius": celsius })));
    }
}
```

A consumer reads it from anywhere with just the id:

```rust
let reply = weft.send(device_id, &AgentMessage::new(me, "read", Value::Null)).await?;
```

Runnable version: [`examples/iot_sensor.rs`](../../examples/iot_sensor.rs).

```bash
cargo run --example iot_sensor -- sensor       # prints the device id
cargo run --example iot_sensor -- read <id>    # reads it from another machine
```

## Patterns this enables

- **Command & control** — send `kind: "actuate"` messages to a device by id;
  it replies with status. Request/reply is built in.
- **Telemetry pull** — consumers `read` on demand instead of the device pushing
  to a cloud endpoint it has to be configured for.
- **Fleet discovery** — devices announce `kind: "sensor"`; a controller calls
  `registry().find("sensor")` to enumerate the fleet. See
  [service-discovery.md](../service-discovery.md).
- **Paid telemetry** — gate readings behind an x402 price so third parties pay
  the device (or its operator) per query. See [x402.md](x402.md).

## Fit and limits

weft targets devices that can run a Rust async runtime and hold a QUIC
connection — Linux SBCs (Raspberry Pi and up), OpenWrt routers, edge gateways.
It is **not** aimed at bare-metal microcontrollers with kilobytes of RAM; put a
gateway node in front of those and let it bridge to constrained sensors.

## Provisioning tips

- Generate and persist the device key at manufacture/first-boot; print the
  `EndpointId` on a label or QR so it can be paired without discovery.
- Ship a couple of seed-node ids for bootstrapping if devices should discover
  each other or a controller automatically.
