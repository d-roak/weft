//! x402 use case: a paid service on the weft fabric.
//!
//! Models a relay/API node that charges per request using the x402 handshake
//! (see `src/x402.rs`). The exact same pattern gates a relay hop, an API call,
//! or any agent capability behind a payment.
//!
//! Run the paid service:
//!     cargo run --example x402_relay -- server
//! Then invoke it (prints the server's EndpointId on start):
//!     cargo run --example x402_relay -- client <server-endpoint-id>
//!
//! The client's first call arrives unpaid and gets a `402 payment-required`
//! back with the price + a nonce; it "pays" and retries with proof, and the
//! second call succeeds. Settlement is stubbed in `x402::verify_payment` — that
//! is the one seam to wire to a real x402 facilitator.

use anyhow::{Result, bail};
use iroh::{EndpointId, SecretKey};
use weft::x402::{KIND_PAYMENT_REQUIRED, PaymentPayload, PaymentRequired, PriceTag, verify_payment};
use weft::{AgentMessage, Weft};

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "server" => run_server().await,
        "client" => {
            let Some(id) = std::env::args().nth(2) else { bail!("usage: client <endpoint-id>") };
            run_client(id.parse()?).await
        }
        _ => bail!("usage: x402_relay <server|client <endpoint-id>>"),
    }
}

fn price() -> PriceTag {
    // 0.001 USDC on Base Sepolia. pay_to would be the operator's wallet.
    PriceTag::new(1_000, "USDC", "base-sepolia", "0xF00Dcafe0000000000000000000000000000BEEF")
}

async fn run_server() -> Result<()> {
    let (weft, mut inbox) = Weft::spawn(SecretKey::generate(), vec![]).await?;
    println!("paid relay online: {}", weft.id());
    println!("invoke it with: cargo run --example x402_relay -- client {}", weft.id());

    // Advertise the service *with its price* so consumers can decide up front.
    weft.registry()
        .announce_full(weft::ServiceAnnouncement {
            endpoint_id: weft.id(),
            name: "premium-relay".into(),
            kind: "relay".into(),
            meta: serde_json::json!({ "desc": "forwards one message per paid request" }),
            price: Some(price()),
        })
        .await?;

    let me = weft.id();
    while let Some((msg, reply)) = inbox.recv().await {
        if msg.kind != "invoke" {
            reply.send(AgentMessage::new(me, "error", serde_json::json!("send kind=invoke")));
            continue;
        }

        // Is a payment attached?
        match msg.body.get("payment") {
            None => {
                // Unpaid: reply 402 with price + a per-request nonce.
                // ponytail: fixed nonce keyed off the caller; a real server
                // stores issued nonces to prevent replay across requests.
                let required = PaymentRequired {
                    price: price(),
                    nonce: format!("nonce-for-{}", msg.from),
                    resource: "premium-relay".into(),
                };
                println!("← unpaid invoke from {msg_from}; replying 402", msg_from = msg.from);
                reply.send(AgentMessage::new(
                    me,
                    KIND_PAYMENT_REQUIRED,
                    serde_json::to_value(&required)?,
                ));
            }
            Some(payment) => {
                let payload: PaymentPayload = serde_json::from_value(payment.clone())?;
                let required = PaymentRequired {
                    price: price(),
                    nonce: format!("nonce-for-{}", msg.from),
                    resource: "premium-relay".into(),
                };
                if verify_payment(&required, &payload) {
                    println!("← paid invoke from {}; serving", msg.from);
                    reply.send(AgentMessage::new(
                        me,
                        "result",
                        serde_json::json!({ "ok": true, "relayed": msg.body.get("data") }),
                    ));
                } else {
                    reply.send(AgentMessage::new(
                        me,
                        "error",
                        serde_json::json!("payment verification failed"),
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn run_client(server: EndpointId) -> Result<()> {
    let (weft, _inbox) = Weft::spawn(SecretKey::generate(), vec![]).await?;
    let me = weft.id();

    // 1) Unpaid invoke.
    let first = AgentMessage::new(me, "invoke", serde_json::json!({ "data": "ship it" }));
    let resp = weft.send(server, &first).await?;
    if resp.kind != KIND_PAYMENT_REQUIRED {
        println!("unexpected (no charge?): {} {}", resp.kind, resp.body);
        weft.endpoint().close().await;
        return Ok(());
    }
    let required: PaymentRequired = serde_json::from_value(resp.body)?;
    println!(
        "402: pay {} {} on {} to {}",
        required.price.amount, required.price.asset, required.price.network, required.price.pay_to
    );

    // 2) "Pay" and retry with proof.
    // ponytail: proof is a placeholder string; a real client signs/settles an
    // x402 payment here and passes the resulting authorization as `proof`.
    let payment = PaymentPayload {
        scheme: required.price.scheme.clone(),
        nonce: required.nonce.clone(),
        proof: "0xsettled-payment-authorization".into(),
    };
    let paid = AgentMessage::new(
        me,
        "invoke",
        serde_json::json!({ "data": "ship it", "payment": payment }),
    );
    let result = weft.send(server, &paid).await?;
    println!("→ {} {}", result.kind, result.body);

    weft.endpoint().close().await;
    Ok(())
}
