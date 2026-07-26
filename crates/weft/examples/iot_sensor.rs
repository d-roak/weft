//! IoT use case: a constrained sensor node on the weft fabric.
//!
//! An IoT device is just a weft node with a stable identity. It announces a
//! `sensor` service and answers `read` requests with its latest reading — no
//! open inbound port, no cloud broker, NAT-traversed automatically, and reached
//! by a single EndpointId that survives IP changes (great for devices that roam
//! between networks).
//!
//! Run the sensor:
//!     cargo run --example iot_sensor -- sensor
//! Then, from any machine, read it (prints the sensor's EndpointId on start):
//!     cargo run --example iot_sensor -- read <sensor-endpoint-id>
//!
//! A real device would swap the fake temperature for a GPIO/I2C read. Everything
//! else — identity, connectivity, discovery — is unchanged.

use std::time::Duration;

use anyhow::{Result, bail};
use iroh::{EndpointId, SecretKey};
use weft::{AgentMessage, Config, Weft};

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "sensor" => run_sensor().await,
        "read" => {
            let Some(id) = std::env::args().nth(2) else { bail!("usage: read <endpoint-id>") };
            read_sensor(id.parse()?).await
        }
        _ => bail!("usage: iot_sensor <sensor|read <endpoint-id>>"),
    }
}

async fn run_sensor() -> Result<()> {
    // Stable identity so the device keeps the same address across reboots.
    // ponytail: ephemeral key for the demo; persist it on a real device.
    let (weft, mut inbox) = Weft::spawn(SecretKey::generate(), Config::default()).await?;
    println!("sensor online: {}", weft.id());
    println!("read it with:  cargo run --example iot_sensor -- read {}", weft.id());

    weft.registry()
        .announce("thermostat-1", "sensor", serde_json::json!({ "unit": "celsius" }))
        .await?;

    let me = weft.id();
    let mut ticks: u32 = 0;
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = timer.tick() => ticks += 1,
            Some((msg, reply)) = inbox.recv() => {
                if msg.kind == "read" {
                    // Fake reading; a real device reads a GPIO/I2C sensor here.
                    let celsius = 20.0 + (ticks % 10) as f64 * 0.5;
                    reply.send(AgentMessage::new(
                        me, "reading",
                        serde_json::json!({ "celsius": celsius, "uptime_s": ticks }),
                    ));
                } else {
                    reply.send(AgentMessage::new(me, "error", serde_json::json!("unknown kind")));
                }
            }
        }
    }
}

async fn read_sensor(sensor: EndpointId) -> Result<()> {
    let (weft, _inbox) = Weft::spawn(SecretKey::generate(), Config::default()).await?;
    let req = AgentMessage::new(weft.id(), "read", serde_json::Value::Null);
    let reply = weft.send(sensor, &req).await?;
    println!("{}: {}", reply.kind, reply.body);
    weft.endpoint().close().await;
    Ok(())
}
